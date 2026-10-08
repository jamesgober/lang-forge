//! The forged parser: an interpreter over the compiled grammar tables.
//!
//! # Semantics
//!
//! Alternatives are tried in order and the first that matches wins (parsing
//! expression grammar semantics). The FIRST sets prune that search before it
//! starts: an alternative that cannot begin with the current token is never
//! tried, so for the great majority of decisions exactly one alternative
//! remains and the parser commits to it at once, like a predictive LL(1)
//! parser. Only when several alternatives can begin with the same token does
//! it *speculate*: it tries each in turn in a strict mode that fails fast and
//! records nothing, rewinding between attempts. Repetitions and optionals are
//! greedy: once their body can begin, they commit to it.
//!
//! # Recovery
//!
//! Outside speculation the parser never fails. A missing token is reported and
//! treated as present; a token no rule wants is reported and wrapped in an
//! `ERROR` node, after which the enclosing repetition carries on — unless the
//! token is one an enclosing construct is waiting for (its *stop set*), in
//! which case the repetition ends and lets that construct take it. When all
//! speculative attempts fail, the alternative that got furthest is re-run in
//! recovering mode, so the reported error is the one deepest into the input.
//! At most one syntax error is reported per token position.
//!
//! On input without errors, recovering and strict parsing take the same path
//! and build the same tree.
//!
//! # Events
//!
//! The parser does not build the tree directly. It records a flat list of
//! events — start node, token, finish node — that `tree::build` replays.
//! Speculation rewinds by truncating the list, and an operator node that must
//! wrap an operand already parsed (the `a` in `a + b`) is spliced in by a
//! forward link from the operand's start event instead of an insertion into
//! the middle of the list.

use alloc::{format, string::String, vec::Vec};

use diag_lang::{Diagnostic, Label, Severity};
use syntax_lang::{Node, Span, Token};

use crate::{
    grammar::{Expr, Grammar, Level, Program, RuleBody},
    kind::Kind,
    lexer,
    schematic::Fixity,
    set::{NO_SET, SetId},
    tree::{self, Event},
};

/// How deeply the parser may recurse into the grammar before it reports an
/// error instead of recursing further. Every recursive step counts — a rule, a
/// sequence, an alternative, a repetition, an optional, an operator operand —
/// so the bound holds for any grammar, however its rules nest. At this limit
/// the parser needs at most about 256 KiB of stack in a release build and
/// 768 KiB in a debug build (measured on the deepest-nesting grammars known),
/// which fits every default thread, including the 1 MiB Windows main thread.
/// Typical grammars reach it only past a hundred and fifty levels of nested
/// parentheses.
pub(crate) const MAX_DEPTH: u32 = 768;

/// Moves an event's forward link from one base index to another.
#[inline]
fn relocate(event: Event, from: u32, to: u32) -> Event {
    match event {
        Event::Start { kind, forward } if forward != 0 => Event::Start {
            kind,
            forward: forward - from + to,
        },
        other => other,
    }
}

/// Parses `src`, always producing a tree, and the problems found on the way.
pub(crate) fn parse(grammar: &Grammar, src: &str) -> (Node<Kind>, Vec<Diagnostic>) {
    let root = grammar.program.rules[grammar.program.start as usize]
        .node
        .unwrap_or(grammar.program.error);
    if u32::try_from(src.len()).is_err() {
        let diag = Diagnostic::new(
            Severity::Error,
            "the source is larger than 4 GiB",
            Label::unlabelled(Span::empty(0)),
        );
        return (Node::new(root, Vec::new()), Vec::from([diag]));
    }
    let mut tokens = Vec::new();
    let mut diags = Vec::new();
    grammar.lexer.run(src, &mut tokens, &mut diags);
    let mut parser = Parser::new(grammar, src, &tokens);
    // Outside strict mode the parser recovers from everything; `top` only
    // reports failure to a strict caller.
    let recovered = parser.top();
    debug_assert!(recovered);
    let Parser {
        mut events,
        diags: parse_diags,
        ..
    } = parser;
    if !parse_diags.is_empty() {
        diags.extend(parse_diags);
        diags.sort_by_key(|d| d.primary().span().start().to_u32());
    }
    (tree::build(&tokens, &mut events, root), diags)
}

/// Parses `src` in strict mode only: `None` if it does not match the grammar
/// exactly. The reference the recovering parser is tested against.
#[cfg(test)]
pub(crate) fn parse_strict(grammar: &Grammar, src: &str) -> Option<Node<Kind>> {
    let mut tokens = Vec::new();
    let mut diags = Vec::new();
    grammar.lexer.run(src, &mut tokens, &mut diags);
    let mut parser = Parser::new(grammar, src, &tokens);
    parser.strict = 1;
    if !parser.top() {
        return None;
    }
    let root = grammar.program.rules[grammar.program.start as usize].node?;
    let mut events = parser.events;
    Some(tree::build(&tokens, &mut events, root))
}

/// Parses `src` with memoization off: the reference memoized parsing must
/// agree with, tree and diagnostics alike.
#[cfg(test)]
pub(crate) fn parse_unmemoized(grammar: &Grammar, src: &str) -> (Node<Kind>, Vec<Diagnostic>) {
    let root = grammar.program.rules[grammar.program.start as usize]
        .node
        .unwrap_or(grammar.program.error);
    let mut tokens = Vec::new();
    let mut diags = Vec::new();
    grammar.lexer.run(src, &mut tokens, &mut diags);
    let mut parser = Parser::new(grammar, src, &tokens);
    parser.memoize = false;
    let recovered = parser.top();
    debug_assert!(recovered);
    let mut events = parser.events;
    diags.extend(parser.diags);
    diags.sort_by_key(|d| d.primary().span().start().to_u32());
    (tree::build(&tokens, &mut events, root), diags)
}

/// A position to rewind to after a failed speculative attempt.
#[derive(Clone, Copy)]
struct Checkpoint {
    events: usize,
    pos: usize,
}

/// What a strict attempt at a rule produced, remembered by rule and position.
#[derive(Clone, Copy)]
struct MemoEntry {
    rule: u32,
    /// The previous entry for the same position, plus one; zero ends the chain.
    next: u32,
    /// The position after the rule, or `FAILED`.
    end: u32,
    /// The furthest position the attempt looked at.
    reach: u32,
    /// The attempt's events in `Memo::events`.
    from: u32,
    len: u32,
}

const FAILED: u32 = u32::MAX;

/// Results of strict rule attempts, so speculation never parses the same rule
/// at the same position twice: without it, alternatives that share a prefix
/// through different rules re-parse that prefix once per alternative, at
/// every level of nesting — exponential time on modest input.
///
/// Strict results depend only on the rule and the position, so they stay
/// valid for as long as they are kept. They are dropped when the outermost
/// speculation ends, which bounds the memory to the work of one speculation;
/// a generation stamp makes that drop O(1).
#[derive(Default)]
struct Memo {
    /// By token position: the generation and newest entry (plus one).
    heads: Vec<(u32, u32)>,
    generation: u32,
    entries: Vec<MemoEntry>,
    /// Events of successful attempts; forward links are relative to the
    /// attempt's first event.
    events: Vec<Event>,
}

impl Memo {
    fn get(&self, rule: u32, pos: usize) -> Option<MemoEntry> {
        let &(generation, head) = self.heads.get(pos)?;
        if generation != self.generation {
            return None;
        }
        let mut at = head;
        while at != 0 {
            let entry = self.entries[at as usize - 1];
            if entry.rule == rule {
                return Some(entry);
            }
            at = entry.next;
        }
        None
    }

    fn put(&mut self, mut entry: MemoEntry, pos: usize, positions: usize) {
        if self.heads.is_empty() {
            self.heads = alloc::vec![(0, 0); positions];
            self.generation = 1;
        }
        let head = &mut self.heads[pos];
        entry.next = if head.0 == self.generation { head.1 } else { 0 };
        self.entries.push(entry);
        *head = (self.generation, self.entries.len() as u32);
    }

    fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Stamps wrapped: no old stamp may match the new generation.
            self.heads.iter_mut().for_each(|h| *h = (0, 0));
            self.generation = 1;
        }
        self.entries.clear();
        self.events.clear();
    }
}

struct Parser<'a> {
    grammar: &'a Grammar,
    program: &'a Program,
    src: &'a str,
    /// Kind indexes of the significant tokens, then the end-of-input bit.
    kinds: Vec<u16>,
    spans: Vec<Span>,
    pos: usize,
    events: Vec<Event>,
    diags: Vec<Diagnostic>,
    /// Non-zero while speculating.
    strict: u32,
    /// Speculations in progress, strict or not.
    speculating: u32,
    /// The furthest token position the current attempt reached.
    furthest: usize,
    /// The token position of the last reported error, to report one per token.
    last_error: usize,
    /// The event count right after the last `ERROR` node closed, so adjacent
    /// skipped tokens share one node.
    error_end: usize,
    /// Stop sets of the constructs being parsed, innermost last.
    stops: Vec<SetId>,
    /// By rule: the position of its innermost active invocation, or `u32::MAX`.
    active: Vec<u32>,
    depth: u32,
    too_deep: bool,
    /// Set once any attempt, strict or not, has hit the depth limit.
    hit_limit: bool,
    /// Counts events that make a strict result depend on more than the rule
    /// and position (the depth limit, the progress guard); such results are
    /// not memoized.
    taint: u32,
    memo: Memo,
    /// Whether strict attempts are memoized. Always on; the tests turn it off
    /// to check that memoization never changes a result.
    memoize: bool,
}

impl<'a> Parser<'a> {
    fn new(grammar: &'a Grammar, src: &'a str, tokens: &[Token<Kind>]) -> Self {
        let significant = tokens.iter().filter(|t| !t.is_trivia()).count();
        let mut kinds = Vec::with_capacity(significant + 1);
        let mut spans = Vec::with_capacity(significant + 1);
        for t in tokens.iter().filter(|t| !t.is_trivia()) {
            kinds.push(t.kind().index() as u16);
            spans.push(t.span());
        }
        kinds.push(grammar.program.eof);
        spans.push(Span::empty(src.len() as u32));
        Self {
            grammar,
            program: &grammar.program,
            src,
            kinds,
            spans,
            pos: 0,
            events: Vec::with_capacity(significant * 2 + 8),
            diags: Vec::new(),
            strict: 0,
            speculating: 0,
            furthest: 0,
            last_error: usize::MAX,
            error_end: usize::MAX,
            stops: Vec::new(),
            active: alloc::vec![u32::MAX; grammar.program.rules.len()],
            depth: 0,
            too_deep: false,
            hit_limit: false,
            taint: 0,
            memo: Memo::default(),
            memoize: true,
        }
    }

    /// The start rule, then any leftover input. Returns `false` only in strict
    /// mode, when the input does not match.
    fn top(&mut self) -> bool {
        let program = self.program;
        let start = program.start;
        let rule = &program.rules[start as usize];
        let root = rule.node.unwrap_or(self.program.error);
        self.events.push(Event::Start {
            kind: root,
            forward: 0,
        });
        let ok = if self.at_set(rule.first) || rule.nullable {
            self.active[start as usize] = 0;
            match rule.body {
                RuleBody::Expr(e) => self.expr(e),
                RuleBody::Pratt(p) => self.pratt(p, start, 0),
            }
        } else {
            self.expected_rule(start)
        };
        if !ok {
            return false;
        }
        if !self.at_eof() {
            if self.strict > 0 {
                return false;
            }
            self.leftover();
        }
        self.events.push(Event::Finish);
        true
    }

    /// Parses one grammar expression. Every recursive step of the parser
    /// passes through here or through `pratt`, and both count against
    /// `MAX_DEPTH`, which is what bounds the parser's stack.
    fn expr(&mut self, e: u32) -> bool {
        let program = self.program;
        let expr = program.exprs[e as usize];
        if let Expr::Token(kind) = expr {
            return self.token(kind);
        }
        if self.depth >= MAX_DEPTH {
            return self.too_deep();
        }
        self.depth += 1;
        let ok = match expr {
            Expr::Rule(r) => self.rule(r),
            Expr::Seq { start, len } => self.seq(start, len),
            Expr::Choice { start, len } => self.choice(program.children(start, len)),
            Expr::Repeat {
                body,
                min_one,
                stop,
            } => self.repeat(body, min_one, stop),
            Expr::Optional(body) => !self.at_set(program.first[body as usize]) || self.expr(body),
            Expr::Token(_) => true,
        };
        self.depth -= 1;
        ok
    }

    fn seq(&mut self, start: u32, len: u32) -> bool {
        let program = self.program;
        let items = program.children(start, len);
        if self.strict > 0 {
            return items.iter().all(|&item| self.expr(item));
        }
        let sync = &program.sync[start as usize..(start + len) as usize];
        for (&item, &sync) in items.iter().zip(sync) {
            // While an item recovers, the tokens the rest of this sequence
            // begins with are off limits to skipping: they are why the item
            // should end. Recovering parses never fail.
            if sync == NO_SET {
                let parsed = self.expr(item);
                debug_assert!(parsed);
            } else {
                self.stops.push(sync);
                let parsed = self.expr(item);
                debug_assert!(parsed);
                let _ = self.stops.pop();
            }
        }
        true
    }

    #[inline]
    fn token(&mut self, kind: u16) -> bool {
        if self.current() == kind {
            self.bump();
            return true;
        }
        self.missing_token(kind)
    }

    fn rule(&mut self, r: u32) -> bool {
        let program = self.program;
        let rule = &program.rules[r as usize];
        if !rule.nullable && !self.at_set(rule.first) {
            return self.expected_rule(r);
        }
        let pos = self.pos as u32;
        if self.active[r as usize] == pos {
            return self.no_progress();
        }
        if self.strict == 0 || !self.memoize {
            return self.rule_body(r, pos);
        }
        if let Some(hit) = self.memo.get(r, self.pos) {
            return self.replay(hit);
        }
        let first_event = self.events.len();
        let taint = self.taint;
        let outer = self.furthest;
        self.furthest = self.pos;
        let ok = self.rule_body(r, pos);
        let reach = self.furthest;
        self.furthest = outer.max(reach);
        if self.taint == taint {
            self.remember(r, pos, first_event, ok, reach);
        }
        ok
    }

    /// Invokes rule `r` at `pos`, marking it active there so that a cycle of
    /// rules that consumes nothing is caught instead of followed.
    fn rule_body(&mut self, r: u32, pos: u32) -> bool {
        let rule = &self.program.rules[r as usize];
        let outer = core::mem::replace(&mut self.active[r as usize], pos);
        if let Some(kind) = rule.node {
            self.events.push(Event::Start { kind, forward: 0 });
        }
        let ok = match rule.body {
            RuleBody::Expr(e) => self.expr(e),
            RuleBody::Pratt(p) => self.pratt(p, r, 0),
        };
        if ok && rule.node.is_some() {
            self.events.push(Event::Finish);
        }
        self.active[r as usize] = outer;
        ok
    }

    /// Records a strict attempt's outcome.
    fn remember(&mut self, rule: u32, pos: u32, first_event: usize, ok: bool, reach: usize) {
        let mut entry = MemoEntry {
            rule,
            next: 0,
            end: FAILED,
            reach: reach as u32,
            from: 0,
            len: 0,
        };
        if ok {
            entry.end = self.pos as u32;
            entry.from = self.memo.events.len() as u32;
            entry.len = (self.events.len() - first_event) as u32;
            let base = first_event as u32;
            self.memo.events.extend(
                self.events[first_event..]
                    .iter()
                    .map(|&event| relocate(event, base, 0)),
            );
        }
        self.memo.put(entry, pos as usize, self.kinds.len());
    }

    /// Repeats a remembered attempt without parsing it again.
    fn replay(&mut self, hit: MemoEntry) -> bool {
        self.furthest = self.furthest.max(hit.reach as usize);
        if hit.end == FAILED {
            return false;
        }
        let base = self.events.len() as u32;
        let range = hit.from as usize..(hit.from + hit.len) as usize;
        self.events.extend(
            self.memo.events[range]
                .iter()
                .map(|&event| relocate(event, 0, base)),
        );
        self.pos = hit.end as usize;
        true
    }

    fn choice(&mut self, alternatives: &[u32]) -> bool {
        let program = self.program;
        let mut viable = alternatives
            .iter()
            .filter(|&&a| program.nullable[a as usize] || self.at_set(program.first[a as usize]));
        match (viable.next(), viable.next()) {
            (Some(&only), None) => self.expr(only),
            (Some(_), Some(_)) => self.speculate(alternatives),
            (None, _) => self.expected_alternatives(alternatives),
        }
    }

    /// Tries each viable alternative strictly, in order; the first to match
    /// is kept.
    #[inline(never)]
    fn speculate(&mut self, alternatives: &[u32]) -> bool {
        self.speculating += 1;
        let ok = self.try_alternatives(alternatives);
        self.speculating -= 1;
        if self.speculating == 0 {
            self.memo.clear();
        }
        ok
    }

    fn try_alternatives(&mut self, alternatives: &[u32]) -> bool {
        let program = self.program;
        let checkpoint = Checkpoint {
            events: self.events.len(),
            pos: self.pos,
        };
        let mut best: Option<(u32, usize)> = None;
        for &alt in alternatives {
            if !program.nullable[alt as usize] && !self.at_set(program.first[alt as usize]) {
                continue;
            }
            let outer = self.furthest;
            self.furthest = self.pos;
            self.strict += 1;
            let ok = self.expr(alt);
            self.strict -= 1;
            let reach = self.furthest;
            self.furthest = outer.max(reach);
            if ok {
                return true;
            }
            if best.is_none_or(|(_, r)| reach > r) {
                best = Some((alt, reach));
            }
            self.events.truncate(checkpoint.events);
            self.pos = checkpoint.pos;
            // Results that hit the depth limit cannot be memoized, so trying
            // every alternative at every level of such input would take
            // exponential time. The input is reported as too deep regardless;
            // one attempt per decision keeps the rest of the parse linear.
            if self.hit_limit {
                break;
            }
        }
        if self.strict > 0 {
            return false;
        }
        match best {
            Some((alt, _)) => self.expr(alt),
            None => true,
        }
    }

    fn repeat(&mut self, body: u32, min_one: bool, stop: SetId) -> bool {
        let program = self.program;
        let first = program.first[body as usize];
        // The first item of `+` is mandatory: parse it unconditionally, so a
        // missing one is reported by the element that is missing.
        if min_one && !self.expr(body) {
            return false;
        }
        let recovering = self.strict == 0;
        if recovering {
            self.stops.push(stop);
        }
        loop {
            if self.at_set(first) {
                let before = self.pos;
                if !self.expr(body) {
                    return false;
                }
                if self.pos == before {
                    // An item that consumed nothing: the end of the input
                    // standing in for a line break, or recovery assuming the
                    // item. Stop, or skip a token, so the loop always ends.
                    if !recovering || self.at_eof() {
                        break;
                    }
                    self.skip();
                }
                continue;
            }
            if !recovering
                || self.at_eof()
                || program.sets.contains(stop, self.current() as usize)
                || self.at_outer_stop()
            {
                break;
            }
            self.unexpected_item(body);
        }
        if recovering {
            let _ = self.stops.pop();
        }
        true
    }

    fn pratt(&mut self, p: u32, rule: u32, min_bp: u16) -> bool {
        if self.depth >= MAX_DEPTH {
            return self.too_deep();
        }
        self.depth += 1;
        let ok = self.pratt_inner(p, rule, min_bp);
        self.depth -= 1;
        ok
    }

    fn pratt_inner(&mut self, p: u32, rule: u32, min_bp: u16) -> bool {
        let program = self.program;
        let pratt = &program.pratts[p as usize];
        // A placeholder for the node that will wrap this operand, if any.
        let slot = self.events.len();
        self.events.push(Event::Tombstone);
        let mut outer = slot;

        let current = self.current() as usize;
        let prefix = pratt.prefix.get(current).copied().unwrap_or(0);
        if prefix != 0 {
            let level = pratt.levels[prefix as usize - 1];
            self.events[slot] = Event::Start {
                kind: level.node,
                forward: 0,
            };
            self.bump();
            if !self.operator_tail(level) || !self.pratt(p, rule, level.rbp) {
                return false;
            }
            self.events.push(Event::Finish);
        } else if self.at_set(program.first[pratt.operand as usize]) {
            if !self.expr(pratt.operand) {
                return false;
            }
        } else {
            return self.expected_rule(rule);
        }

        let mut chained: Option<u8> = None;
        loop {
            let index = pratt
                .after
                .get(self.current() as usize)
                .copied()
                .unwrap_or(0);
            if index == 0 {
                break;
            }
            let level = pratt.levels[index as usize - 1];
            if level.lbp < min_bp {
                break;
            }
            if level.fixity == Fixity::NonAssoc && chained == Some(index) && !self.chained() {
                return false;
            }
            self.wrap(slot, &mut outer, level.node);
            self.bump();
            if !self.operator_tail(level) {
                return false;
            }
            if level.fixity != Fixity::Postfix && !self.pratt(p, rule, level.rbp) {
                return false;
            }
            self.events.push(Event::Finish);
            chained = (level.fixity == Fixity::NonAssoc).then_some(index);
        }
        true
    }

    /// The `then` part of an operator level, if it has one.
    fn operator_tail(&mut self, level: Level) -> bool {
        level.then.is_none_or(|then| self.expr(then))
    }

    /// Opens a node of `kind` around everything since `slot`.
    fn wrap(&mut self, slot: usize, outer: &mut usize, kind: Kind) {
        if matches!(self.events[slot], Event::Tombstone) {
            self.events[slot] = Event::Start { kind, forward: 0 };
            return;
        }
        let new = self.events.len();
        self.events.push(Event::Start { kind, forward: 0 });
        if let Event::Start { forward, .. } = &mut self.events[*outer] {
            *forward = new as u32;
        }
        *outer = new;
    }

    // ----- tokens -----

    #[inline]
    fn current(&self) -> u16 {
        self.kinds[self.pos]
    }

    #[inline]
    fn at_eof(&self) -> bool {
        self.pos + 1 >= self.kinds.len()
    }

    /// Whether the current token can begin something whose FIRST set is
    /// `set`. The end of the input also stands for a line break, so a
    /// `NEWLINE` anywhere a construct may begin accepts it.
    #[inline]
    fn at_set(&self, set: SetId) -> bool {
        let sets = &self.program.sets;
        let current = self.current();
        sets.contains(set, current as usize)
            || (current == self.program.eof && sets.contains(set, lexer::NEWLINE as usize))
    }

    fn at_outer_stop(&self) -> bool {
        let current = self.current() as usize;
        self.stops
            .iter()
            .any(|&s| self.program.sets.contains(s, current))
    }

    #[inline]
    fn bump(&mut self) {
        self.events.push(Event::Token);
        self.pos += 1;
        self.furthest = self.furthest.max(self.pos);
    }

    /// Wraps the current token in an `ERROR` node, extending the previous one
    /// when it ends right here.
    fn skip(&mut self) {
        if self.error_end == self.events.len() {
            let _ = self.events.pop();
            self.bump();
        } else {
            self.events.push(Event::Start {
                kind: self.program.error,
                forward: 0,
            });
            self.bump();
        }
        self.events.push(Event::Finish);
        self.error_end = self.events.len();
    }

    // ----- errors -----
    //
    // Everything below runs only on malformed input. Each method is kept out of
    // line and marked cold, so the recursive functions above stay small: their
    // stack frames bound how deeply the parser can nest, and their code stays
    // hot in the instruction cache.

    /// A required token is missing. In recovering mode it is reported and
    /// assumed — or, when the token after the current one is the expected one,
    /// the current token is skipped as a stray.
    #[cold]
    #[inline(never)]
    fn missing_token(&mut self, kind: u16) -> bool {
        // The end of the input ends the last line too, so a file need not
        // finish with a line break.
        if kind == lexer::NEWLINE && self.at_eof() {
            return true;
        }
        if self.strict > 0 {
            return false;
        }
        let message = format!("expected {}, found {}", self.describe(kind), self.found());
        let stray = self.kinds.get(self.pos + 1) == Some(&kind) && !self.at_outer_stop();
        self.report(message);
        if stray {
            self.skip();
            self.bump();
        }
        true
    }

    /// The current token cannot begin rule `r`.
    #[cold]
    #[inline(never)]
    fn expected_rule(&mut self, r: u32) -> bool {
        if self.strict > 0 {
            return false;
        }
        let message = format!(
            "expected {}, found {}",
            self.program.rules[r as usize].name,
            self.found()
        );
        self.report(message);
        true
    }

    /// The current token can begin none of `alternatives`.
    #[cold]
    #[inline(never)]
    fn expected_alternatives(&mut self, alternatives: &[u32]) -> bool {
        if self.strict > 0 {
            return false;
        }
        let sets: Vec<SetId> = alternatives
            .iter()
            .map(|&a| self.program.first[a as usize])
            .collect();
        let message = format!(
            "expected {}, found {}",
            self.describe_sets(&sets),
            self.found()
        );
        self.report(message);
        true
    }

    /// A token no item of the repetition can begin, and that nothing around
    /// it is waiting for: reported and skipped.
    #[cold]
    #[inline(never)]
    fn unexpected_item(&mut self, body: u32) {
        // A run of such tokens is one problem: report its first token only.
        if self.error_end != self.events.len() {
            let message = format!(
                "expected {}, found {}",
                self.describe_expr(body),
                self.found()
            );
            self.report(message);
        }
        self.skip();
    }

    /// A second non-associative operator at the same level, as in `a < b < c`.
    #[cold]
    #[inline(never)]
    fn chained(&mut self) -> bool {
        if self.strict > 0 {
            return false;
        }
        let message = format!(
            "`{}` cannot be chained; add parentheses",
            self.text(self.pos)
        );
        self.report(message);
        true
    }

    /// A rule entered again where it is already active, with nothing consumed
    /// in between. A grammar that forges cannot do this on valid input — that
    /// would be left recursion — but recovery that assumes a missing token, or
    /// the end of the input standing in for a line break, can. The inner
    /// invocation matches nothing instead of recursing.
    #[cold]
    #[inline(never)]
    fn no_progress(&mut self) -> bool {
        self.taint += 1;
        true
    }

    /// Input after the start rule is complete: one error, one `ERROR` node.
    #[cold]
    #[inline(never)]
    fn leftover(&mut self) {
        let message = format!("expected the end of the input, found {}", self.found());
        self.report(message);
        self.events.push(Event::Start {
            kind: self.program.error,
            forward: 0,
        });
        while !self.at_eof() {
            self.bump();
        }
        self.events.push(Event::Finish);
    }

    fn report(&mut self, message: String) {
        if self.last_error == self.pos {
            return;
        }
        self.last_error = self.pos;
        let span = self.spans[self.pos];
        self.diags.push(Diagnostic::new(
            Severity::Error,
            message,
            Label::unlabelled(span),
        ));
    }

    #[cold]
    #[inline(never)]
    fn too_deep(&mut self) -> bool {
        self.hit_limit = true;
        if self.strict > 0 {
            self.taint += 1;
            return false;
        }
        if !self.too_deep {
            self.too_deep = true;
            let span = self.spans[self.pos];
            self.diags.push(Diagnostic::new(
                Severity::Error,
                format!(
                    "the input is nested too deeply to parse (more than {MAX_DEPTH} grammar levels)"
                ),
                Label::unlabelled(span),
            ));
        }
        true
    }

    /// The source text of significant token `pos`.
    fn text(&self, pos: usize) -> &'a str {
        let span = self.spans[pos];
        &self.src[span.start().to_usize()..span.end().to_usize()]
    }

    /// The current token, for "found ...".
    fn found(&self) -> String {
        let kind = self.current();
        if self.at_eof() {
            return String::from("the end of the input");
        }
        let text = self.text(self.pos);
        let short: String = text.chars().take(32).collect();
        let ellipsis = if short.len() < text.len() { "…" } else { "" };
        match kind {
            lexer::IDENT => format!("identifier `{short}{ellipsis}`"),
            lexer::NUMBER => format!("number `{short}{ellipsis}`"),
            lexer::STRING => String::from("a string"),
            lexer::NEWLINE => String::from("a line break"),
            _ => format!("`{short}{ellipsis}`"),
        }
    }

    /// A token kind, for "expected ...".
    fn describe(&self, kind: u16) -> String {
        match kind {
            lexer::IDENT => String::from("an identifier"),
            lexer::NUMBER => String::from("a number"),
            lexer::STRING => String::from("a string"),
            lexer::NEWLINE => String::from("a line break"),
            k if k == self.program.eof => String::from("the end of the input"),
            k => format!("`{}`", self.grammar.kinds.name_at(k as usize)),
        }
    }

    fn describe_expr(&self, e: u32) -> String {
        match self.program.exprs[e as usize] {
            Expr::Rule(r) => String::from(&*self.program.rules[r as usize].name),
            Expr::Token(kind) => self.describe(kind),
            Expr::Repeat { body, .. } | Expr::Optional(body) => self.describe_expr(body),
            _ => self.describe_sets(&[self.program.first[e as usize]]),
        }
    }

    /// The members of the union of `sets`: "a", "a or b", "a, b, or c".
    fn describe_sets(&self, sets: &[SetId]) -> String {
        let mut members: Vec<usize> = sets
            .iter()
            .flat_map(|&s| self.program.sets.members(s))
            .collect();
        members.sort_unstable();
        members.dedup();
        let mut names: Vec<String> = members
            .iter()
            .take(6)
            .map(|&k| self.describe(k as u16))
            .collect();
        if members.len() > 6 {
            names.truncate(5);
            names.push(String::from("…"));
        }
        match names.len() {
            0 => String::from("nothing"),
            1 => names.remove(0),
            2 => format!("{} or {}", names[0], names[1]),
            n => format!("{}, or {}", names[..n - 1].join(", "), names[n - 1]),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use alloc::string::ToString;

    use proptest::prelude::*;

    use super::*;
    use crate::Language;

    fn forge(rules: &str) -> Language {
        let schematic = format!(
            "[language]\nname = \"t\"\n[lexer]\nline_comments = [\"#\"]\nstrings = ['\"']\n{rules}"
        );
        Language::from_lsf(&schematic).unwrap_or_else(|e| panic!("{e}\n{:?}", e.diagnostics()))
    }

    /// The tree as an S-expression of node names, tokens shown by text.
    fn sexp(lang: &Language, src: &str) -> String {
        fn walk(lang: &Language, src: &str, node: &Node<Kind>, out: &mut String) {
            out.push('(');
            out.push_str(lang.kind_name(*node.kind()));
            for child in node.children() {
                match child {
                    syntax_lang::Element::Node(n) => {
                        out.push(' ');
                        walk(lang, src, n, out);
                    }
                    syntax_lang::Element::Token(t) => {
                        if !syntax_lang::TokenKind::is_trivia(t.kind()) {
                            out.push(' ');
                            out.push_str(
                                &src[t.span().start().to_usize()..t.span().end().to_usize()],
                            );
                        }
                    }
                }
            }
            out.push(')');
        }
        let parse = lang.parse(src);
        let mut out = String::new();
        walk(lang, src, parse.tree(), &mut out);
        out
    }

    fn messages(lang: &Language, src: &str) -> Vec<String> {
        lang.parse(src)
            .diagnostics()
            .iter()
            .map(|d| d.message().to_string())
            .collect()
    }

    fn assert_strict_agrees(lang: &Language, src: &str) {
        let parse = lang.parse(src);
        assert!(!parse.has_errors(), "{src:?}: {:?}", messages(lang, src));
        let strict = parse_strict(lang.grammar(), src).expect("strict parse succeeds");
        assert_eq!(&strict, parse.tree());
    }

    #[test]
    fn test_parse_ordered_choice_speculates_across_rules() {
        let lang = forge(
            "[rules]\nfile = \"stmt*\"\nstmt = \"assign | call\"\n\
             assign = \"IDENT '=' NUMBER ';'\"\ncall = \"IDENT '(' ')' ';'\"\n",
        );
        assert_eq!(
            sexp(&lang, "f(); x = 1;"),
            "(file (stmt (call f ( ) ;)) (stmt (assign x = 1 ;)))"
        );
        assert_strict_agrees(&lang, "f(); x = 1;");
    }

    #[test]
    fn test_parse_left_factored_alternatives() {
        let lang = forge("[rules]\nitem = \"IDENT '=' NUMBER | IDENT '(' ')' | IDENT\"\n");
        assert_eq!(sexp(&lang, "a = 1"), "(item a = 1)");
        assert_eq!(sexp(&lang, "a ( )"), "(item a ( ))");
        assert_eq!(sexp(&lang, "a"), "(item a)");
    }

    #[test]
    fn test_parse_hidden_rules_splice_children() {
        let lang =
            forge("[rules]\nlist = \"_item*\"\n_item = \"NUMBER | word\"\nword = \"IDENT\"\n");
        assert_eq!(sexp(&lang, "1 a 2"), "(list 1 (word a) 2)");
    }

    #[test]
    fn test_parse_pratt_precedence_and_associativity() {
        let lang = forge(
            "[rules.expr]\noperand = \"NUMBER | IDENT | '(' expr ')'\"\nlevels = [\n\
             { left = [\"+\", \"-\"] },\n{ left = [\"*\"] },\n{ prefix = [\"-\"] },\n\
             { right = [\"^\"] },\n{ postfix = [\"!\"] },\n]\n",
        );
        assert_eq!(
            sexp(&lang, "1 + 2 * 3"),
            "(expr (binary 1 + (binary 2 * 3)))"
        );
        assert_eq!(
            sexp(&lang, "1 - 2 - 3"),
            "(expr (binary (binary 1 - 2) - 3))"
        );
        assert_eq!(
            sexp(&lang, "2 ^ 3 ^ 4"),
            "(expr (binary 2 ^ (binary 3 ^ 4)))"
        );
        assert_eq!(sexp(&lang, "-a * b"), "(expr (binary (prefix - a) * b))");
        assert_eq!(sexp(&lang, "-a ^ b"), "(expr (prefix - (binary a ^ b)))");
        assert_eq!(sexp(&lang, "-a!"), "(expr (prefix - (postfix a !)))");
        assert_eq!(
            sexp(&lang, "(1 + 2) * 3"),
            "(expr (binary ( (expr (binary 1 + 2)) ) * 3))"
        );
        for src in ["1 + 2 * 3", "-a ^ b ^ c!", "((1))", "1 - -2"] {
            assert_strict_agrees(&lang, src);
        }
    }

    #[test]
    fn test_parse_pratt_then_tails_and_node_names() {
        let lang = forge(
            "[rules]\nfile = \"expr\"\nargs = \"expr (',' expr)*\"\n[rules.expr]\noperand = \"IDENT | NUMBER\"\nlevels = [\n\
             { right = [\"?\"], then = \"expr ':'\", node = \"ternary\" },\n\
             { left = [\"+\"] },\n\
             { postfix = [\"(\"], then = \"args? ')'\", node = \"call\" },\n\
             { postfix = [\"[\"], then = \"expr ']'\", node = \"index\" },\n]\n",
        );
        assert_eq!(
            sexp(&lang, "f(a, 1)[0] + b"),
            "(file (expr (binary (index (call f ( (args (expr a) , (expr 1)) )) [ (expr 0) ]) + b)))"
        );
        assert_eq!(
            sexp(&lang, "a ? b : c ? d : e"),
            "(file (expr (ternary a ? (expr b) : (ternary c ? (expr d) : e))))"
        );
        assert_eq!(sexp(&lang, "g()"), "(file (expr (call g ( ))))");
        assert_strict_agrees(&lang, "f(a, 1)[0] + b ? x : y");
    }

    #[test]
    fn test_parse_nonassoc_chain_is_reported() {
        let lang = forge(
            "[rules.expr]\noperand = \"NUMBER\"\nlevels = [{ none = [\"<\", \"==\"] }, { left = [\"+\"] }]\n",
        );
        assert!(messages(&lang, "1 < 2 + 3").is_empty());
        assert_eq!(
            messages(&lang, "1 < 2 == 3"),
            ["`==` cannot be chained; add parentheses"]
        );
        assert!(parse_strict(lang.grammar(), "1 < 2 < 3").is_none());
    }

    #[test]
    fn test_parse_missing_token_is_assumed() {
        let lang = forge("[rules]\nfile = \"stmt*\"\nstmt = \"'let' IDENT '=' NUMBER ';'\"\n");
        assert_eq!(
            messages(&lang, "let x = 1 let y = 2;"),
            ["expected `;`, found `let`"]
        );
        assert_eq!(
            sexp(&lang, "let x = 1 let y = 2;"),
            "(file (stmt let x = 1) (stmt let y = 2 ;))"
        );
    }

    #[test]
    fn test_parse_one_stray_token_is_skipped() {
        let lang = forge("[rules]\nfile = \"stmt*\"\nstmt = \"'let' IDENT '=' NUMBER ';'\"\n");
        assert_eq!(
            messages(&lang, "let x = 1 = ;"),
            ["expected `;`, found `=`"]
        );
        assert_eq!(
            sexp(&lang, "let x = 1 = ;"),
            "(file (stmt let x = 1 (ERROR =) ;))"
        );
    }

    #[test]
    fn test_parse_garbage_in_repetition_is_wrapped() {
        let lang = forge("[rules]\nblock = \"'{' stmt* '}'\"\nstmt = \"IDENT ';'\"\n");
        assert_eq!(
            messages(&lang, "{ a; ; ; b; }"),
            ["expected stmt, found `;`"]
        );
        assert_eq!(
            sexp(&lang, "{ a; ; ; b; }"),
            "(block { (stmt a ;) (ERROR ; ;) (stmt b ;) })"
        );
    }

    #[test]
    fn test_parse_outer_stop_set_ends_inner_repetition() {
        let lang = forge(
            "[rules]\nblock = \"'{' stmt* '}'\"\nstmt = \"IDENT '(' (IDENT (',' IDENT)*)? ')' ';'\"\n",
        );
        // The `}` closes the block even though the call is unfinished.
        assert_eq!(sexp(&lang, "{ f(a, b }"), "(block { (stmt f ( a , b) })");
        assert_eq!(messages(&lang, "{ f(a, b }"), ["expected `)`, found `}`"]);
    }

    #[test]
    fn test_parse_leftover_input_is_one_error() {
        let lang = forge("[rules]\npair = \"NUMBER NUMBER\"\n");
        assert_eq!(
            messages(&lang, "1 2 3 4"),
            ["expected the end of the input, found number `3`"]
        );
        assert_eq!(sexp(&lang, "1 2 3 4"), "(pair 1 2 (ERROR 3 4))");
    }

    #[test]
    fn test_parse_empty_input() {
        let lang = forge("[rules]\nfile = \"NUMBER*\"\n");
        let parse = lang.parse("");
        assert!(!parse.has_errors());
        assert_eq!(parse.tree().span(), Span::empty(0));
        let lang = forge("[rules]\nfile = \"NUMBER+\"\n");
        assert_eq!(
            messages(&lang, "  "),
            ["expected file, found the end of the input"]
        );
    }

    #[test]
    fn test_parse_reports_lexer_errors_in_order() {
        let lang = forge("[rules]\nfile = \"(NUMBER | STRING)*\"\n");
        assert_eq!(
            messages(&lang, "1 @ \"open"),
            ["unexpected character `@`", "unterminated string"]
        );
    }

    #[test]
    fn test_parse_deep_nesting_is_bounded() {
        let lang = forge(
            "[rules.expr]\noperand = \"NUMBER | '(' expr ')'\"\nlevels = [{ left = [\"+\"] }]\n",
        );
        let depth = 100_000;
        let src = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        // A small stack proves the bound: the parser stops descending long
        // before it could exhaust it.
        let handle = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                let parse = lang.parse(&src);
                let count = parse
                    .diagnostics()
                    .iter()
                    .filter(|d| d.message().contains("nested too deeply"))
                    .count();
                (count, parse.tree().text(&src).map(str::len))
            })
            .unwrap();
        let (count, covered) = handle.join().unwrap();
        assert_eq!(count, 1);
        assert_eq!(covered, Some(2 * depth + 1));
    }

    #[test]
    fn test_parse_significant_newlines() {
        let schematic = "[language]\nname = \"lines\"\n[lexer]\nnewlines = true\n[rules]\n\
                         file = \"(entry | NEWLINE)*\"\nentry = \"IDENT '=' NUMBER NEWLINE\"\n";
        let lang = Language::from_lsf(schematic).unwrap();
        assert_eq!(
            sexp(&lang, "a = 1\n\nb = 2\n"),
            "(file (entry a = 1 \n) \n (entry b = 2 \n))"
        );
        // The end of the input ends the last line.
        assert_eq!(
            sexp(&lang, "a = 1\nb = 2"),
            "(file (entry a = 1 \n) (entry b = 2))"
        );
        assert_strict_agrees(&lang, "a = 1\nb = 2");
        assert_eq!(
            messages(&lang, "a = 1 b = 2\n"),
            ["expected a line break, found identifier `b`"]
        );

        // A mandatory `NEWLINE+` is satisfied by the end of the input too.
        let schematic = "[language]\nname = \"lines\"\n[lexer]\nnewlines = true\n[rules]\n\
                         file = \"entry*\"\nentry = \"IDENT NEWLINE+\"\n";
        let lang = Language::from_lsf(schematic).unwrap();
        assert!(messages(&lang, "a\n\nb").is_empty());
        assert_strict_agrees(&lang, "a\n\nb");
        assert_eq!(
            messages(&lang, "a b"),
            ["expected a line break, found identifier `b`"]
        );
    }

    #[test]
    fn test_parse_plus_reports_the_missing_element() {
        let lang = forge("[rules]\nlist = \"'[' NUMBER+ ']'\"\n");
        assert_eq!(messages(&lang, "[ ]"), ["expected a number, found `]`"]);
        let lang = forge("[rules]\nlist = \"'[' item+ ']'\"\nitem = \"NUMBER ';'\"\n");
        assert_eq!(messages(&lang, "[ ]"), ["expected item, found `]`"]);
    }

    /// A small statement language for token-soup properties.
    fn soup_language() -> Language {
        forge(
            "[rules]\nfile = \"stmt*\"\n\
             stmt = \"'let' IDENT '=' expr ';' | 'if' expr block ('else' block)? | block | expr ';'\"\n\
             block = \"'{' stmt* '}'\"\nargs = \"expr (',' expr)*\"\n\
             [rules.expr]\noperand = \"NUMBER | IDENT | STRING | '(' expr ')'\"\nlevels = [\n\
             { none = [\"==\", \"<\"] },\n{ left = [\"+\", \"-\"] },\n{ left = [\"*\"] },\n\
             { prefix = [\"-\", \"!\"] },\n{ postfix = [\"(\"], then = \"args? ')'\", node = \"call\" },\n]\n",
        )
    }

    /// Statements that are assignments or expressions, both beginning with an
    /// expression that can hold blocks of statements: every statement
    /// speculates, and shares its prefix across alternatives.
    fn speculative_language() -> Language {
        forge(
            "[rules]\nfile = \"stmt*\"\nstmt = \"assign | expr ';' | 'let' IDENT ';'\"\n\
             assign = \"expr '=' expr ';'\"\nblock = \"'{' stmt* '}'\"\ncall = \"IDENT '(' (expr (',' expr)*)? ')'\"\n\
             [rules.expr]\noperand = \"call | IDENT | NUMBER | block\"\nlevels = [{ left = [\"+\"] }, { prefix = [\"-\"] }]\n",
        )
    }

    const SPECULATIVE_WORDS: [&str; 14] = [
        "x", "f", "1", "=", ";", "{", "}", "(", ")", ",", "+", "-", "let", "y",
    ];

    const VOCABULARY: [&str; 22] = [
        "let", "if", "else", "x", "y", "1", "2", "\"s\"", "=", ";", "{", "}", "(", ")", ",", "+",
        "-", "*", "!", "==", "<", "# c\n",
    ];

    /// Cases per property: 2000, or `PROPTEST_CASES` for a longer soak.
    fn cases() -> u32 {
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// Whatever the input, the tree is lossless, and when no error is
        /// reported the strict parser builds exactly the same tree.
        #[test]
        fn prop_parse_is_lossless_and_agrees_with_strict(words in proptest::collection::vec(0..VOCABULARY.len(), 0..40)) {
            let lang = soup_language();
            let src: String = words.iter().map(|&w| VOCABULARY[w]).collect::<Vec<_>>().join(" ");
            let parse = lang.parse(&src);
            prop_assert_eq!(parse.tree().text(&src), Some(src.as_str()));
            prop_assert_eq!(parse.tree().span(), Span::new(0, src.len() as u32));
            let strict = parse_strict(lang.grammar(), &src);
            if parse.has_errors() {
                prop_assert!(strict.is_none(), "strict accepted input with errors: {:?}", src);
            } else {
                prop_assert_eq!(strict.as_ref(), Some(parse.tree()), "trees differ for {:?}", src);
            }
            for d in parse.diagnostics() {
                prop_assert!(d.primary().span().end().to_usize() <= src.len());
            }
        }

        /// Memoization is invisible: with it on or off, the same tree and the
        /// same diagnostics, on a grammar that speculates at every level.
        #[test]
        fn prop_memoization_never_changes_a_result(words in proptest::collection::vec(0..SPECULATIVE_WORDS.len(), 0..40)) {
            let lang = speculative_language();
            let src: String = words.iter().map(|&w| SPECULATIVE_WORDS[w]).collect::<Vec<_>>().join(" ");
            let memoized = parse(lang.grammar(), &src);
            let plain = parse_unmemoized(lang.grammar(), &src);
            prop_assert_eq!(&memoized.0, &plain.0, "trees differ for {:?}", src);
            prop_assert_eq!(memoized.1, plain.1, "diagnostics differ for {:?}", src);
        }

        #[test]
        fn prop_parse_arbitrary_text_never_panics(src in "\\PC{0,80}") {
            let lang = soup_language();
            let parse = lang.parse(&src);
            prop_assert_eq!(parse.tree().text(&src), Some(src.as_str()));
        }
    }

    #[test]
    fn test_parse_soup_samples_agree() {
        let lang = soup_language();
        for src in [
            "let x = 1 + 2 * -y; if x == 1 { x(1, 2); } else { }",
            "{ { } }",
            "x((1)) ;",
        ] {
            assert_strict_agrees(&lang, src);
        }
    }
}
