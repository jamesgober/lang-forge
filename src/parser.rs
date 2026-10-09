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

use diag_lang::{Code, Diagnostic, Label, Severity};
use syntax_lang::{Node, Span, Token};

use crate::{
    codes,
    grammar::{CAT_CLASS, CAT_NODE, Expr, Grammar, Level, Program, RuleBody},
    kind::Kind,
    schematic::Fixity,
    set::{NO_SET, SetId},
    tree::{self, Event, Step},
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

/// No contextual keyword at this token.
const NOT_KEYWORD: u16 = u16::MAX;

/// Moves an event's forward link from one base index to another.
#[inline]
fn relocate(event: Event, from: u32, to: u32) -> Event {
    match event.step() {
        Step::Start { kind, forward } if forward != 0 => Event::start(kind, forward - from + to),
        _ => event,
    }
}

/// Parses `src`, always producing a tree, and the problems found on the way.
pub(crate) fn parse(grammar: &Grammar, src: &str) -> (Node<Kind>, Vec<Diagnostic>) {
    let program = &grammar.program;
    if u32::try_from(src.len()).is_err() {
        let root = program.rules[program.start as usize]
            .node
            .unwrap_or(program.error);
        let diag = Diagnostic::new(
            Severity::Error,
            "the source is larger than 4 GiB",
            Label::unlabelled(Span::empty(0)),
        )
        .with_code(codes::SOURCE_TOO_LARGE);
        return (Node::new(root, Vec::new()), Vec::from([diag]));
    }
    let mut tokens = Vec::new();
    let mut diags = Vec::new();
    grammar.lexer.run(src, &mut tokens, &mut diags);
    parse_tokens(grammar, src, &tokens, program.start, diags)
}

/// Parses already-lexed `tokens` of `src` with rule `start` as the root.
pub(crate) fn parse_tokens(
    grammar: &Grammar,
    src: &str,
    tokens: &[Token<Kind>],
    start: u32,
    mut diags: Vec<Diagnostic>,
) -> (Node<Kind>, Vec<Diagnostic>) {
    let program = &grammar.program;
    let root = program.rules[start as usize].node.unwrap_or(program.error);
    let mut parser = Parser::new(grammar, src, tokens);
    parser.start = start;
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
    (tree::build(tokens, &mut events, root, program.error), diags)
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
    Some(tree::build(
        &tokens,
        &mut events,
        root,
        grammar.program.error,
    ))
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
    (
        tree::build(&tokens, &mut events, root, grammar.program.error),
        diags,
    )
}

/// A position to rewind to after a failed speculative attempt.
#[derive(Clone, Copy)]
struct Checkpoint {
    events: usize,
    pos: usize,
    marks: usize,
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
    /// How many grammar levels below its own the attempt went: replaying it
    /// is the same as parsing it again only where that many levels are left
    /// before the depth limit (ISSUES P09).
    deep: u32,
    /// Where the attempt's events are. While they are still in the parser's
    /// event list (`saved` unset), they are `from..from + len` there. Once a
    /// rewind has rescued them, they are items `from..from + len` of
    /// `Memo::saved`, whose forward links count from `origin`.
    from: u32,
    len: u32,
    origin: u32,
    saved: bool,
}

const FAILED: u32 = u32::MAX;

/// One item of a rescued span of events.
#[derive(Clone, Copy)]
enum Saved {
    /// An event. A forward link counts from the start of the span.
    Event(Event),
    /// The events of a remembered attempt, replayed at this point.
    Entry(u32),
}

/// A remembered attempt replayed into the event list: `len` events from
/// `start` are a copy of entry `entry`'s.
#[derive(Clone, Copy)]
struct Replay {
    start: u32,
    len: u32,
    entry: u32,
}

/// Results of strict rule attempts, so speculation never parses the same rule
/// at the same position twice: without it, alternatives that share a prefix
/// through different rules re-parse that prefix once per alternative, at
/// every level of nesting — exponential time on modest input.
///
/// Strict results depend only on the rule and the position, so they stay
/// valid for as long as they are kept. They are dropped when the outermost
/// speculation ends, which bounds the memory to the work of one speculation;
/// a generation stamp makes that drop O(1).
///
/// A successful attempt's events are not copied when it is remembered: the
/// entry is a range of the parser's event list, where they already are. Only
/// when a failed alternative rewinds that list are the events about to be cut
/// off rescued into `saved`, once for every entry inside the rewound span,
/// and any part of the span that is itself a replayed attempt is kept as a
/// reference to that attempt rather than copied again. So each event the
/// parser produces is stored at most once, however deeply the rules that
/// produced it nest: copying the events of every success, at every level,
/// cost memory proportional to the input times its nesting depth.
#[derive(Default)]
struct Memo {
    /// By token position: the generation and newest entry (plus one).
    heads: Vec<(u32, u32)>,
    generation: u32,
    entries: Vec<MemoEntry>,
    /// Rescued spans: events and references to replayed entries.
    saved: Vec<Saved>,
    /// The entries whose events are still in the parser's event list, in the
    /// order they were made. That is also the order their events end in: an
    /// entry is made when its rule finishes, at the end of the list, and a
    /// rewind rescues every entry it would cut into.
    live: Vec<u32>,
    /// Replays still in the event list, in order. Replays never overlap: a
    /// replay records only the entry it expands, not the entries inside.
    replays: Vec<Replay>,
    /// Scratch for `rescue`: the entries it moves, and boundary positions.
    doomed: Vec<u32>,
    bounds: Vec<(u32, u32)>,
    /// Scratch for `expand`: the rescued spans being replayed, innermost
    /// last, as (next item, end item, origin, base).
    frames: Vec<(u32, u32, u32, u32)>,
}

impl Memo {
    /// The entry for `rule` at `pos`, and its index.
    fn get(&self, rule: u32, pos: usize) -> Option<(u32, MemoEntry)> {
        let &(generation, head) = self.heads.get(pos)?;
        if generation != self.generation {
            return None;
        }
        let mut at = head;
        while at != 0 {
            let entry = self.entries[at as usize - 1];
            if entry.rule == rule {
                return Some((at - 1, entry));
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
        if !entry.saved {
            debug_assert!(self.live.last().is_none_or(|&i| {
                let last = &self.entries[i as usize];
                last.from + last.len <= entry.from + entry.len
            }));
            self.live.push(self.entries.len() as u32);
        }
        self.entries.push(entry);
        *head = (self.generation, self.entries.len() as u32);
    }

    /// Appends entry `index`'s events to `events`, expanding the references
    /// in rescued spans with an explicit stack (they nest as deeply as the
    /// rules did), and relocating forward links to where the events land.
    fn expand(&mut self, index: u32, events: &mut Vec<Event>) {
        self.frames.clear();
        let mut next = Some(index);
        loop {
            if let Some(index) = next.take() {
                let entry = self.entries[index as usize];
                let base = events.len() as u32;
                if entry.saved {
                    self.frames
                        .push((entry.from, entry.from + entry.len, entry.origin, base));
                } else {
                    let range = entry.from as usize..(entry.from + entry.len) as usize;
                    events.extend_from_within(range);
                    for event in &mut events[base as usize..] {
                        *event = relocate(*event, entry.from, base);
                    }
                }
            }
            let Some(frame) = self.frames.last_mut() else {
                return;
            };
            if frame.0 == frame.1 {
                let _ = self.frames.pop();
                continue;
            }
            let (origin, base) = (frame.2, frame.3);
            let item = self.saved[frame.0 as usize];
            frame.0 += 1;
            match item {
                Saved::Event(event) => events.push(relocate(event, origin, base)),
                Saved::Entry(inner) => next = Some(inner),
            }
        }
    }

    /// Records that `len` events from `start` replay entry `entry`.
    fn replayed(&mut self, start: usize, len: usize, entry: u32) {
        if len > 0 {
            self.replays.push(Replay {
                start: start as u32,
                len: len as u32,
                entry,
            });
        }
    }

    /// Rescues the events of every entry that truncating `events` to `keep`
    /// would cut off, and forgets the replays it cuts off.
    ///
    /// Such entries lie wholly past `keep`: a rewind returns to the start of
    /// an attempt, and every entry made since began inside that attempt.
    /// Entries nest like the rules that made them, so the outermost ones are
    /// rescued as spans and the ones inside become ranges of those spans.
    fn rescue(&mut self, events: &[Event], keep: usize) {
        let entries = &self.entries;
        let split = self.live.partition_point(|&i| {
            let entry = &entries[i as usize];
            (entry.from + entry.len) as usize <= keep
        });
        let mut doomed = core::mem::take(&mut self.doomed);
        doomed.clear();
        doomed.extend(self.live.drain(split..));
        // Outermost first: by start, and the longer of two that start together.
        doomed.sort_unstable_by_key(|&i| {
            let entry = &self.entries[i as usize];
            (entry.from, u32::MAX - entry.len)
        });
        // Tags a boundary as an entry's end rather than its start.
        const END: u32 = 1 << 31;
        let mut bounds = core::mem::take(&mut self.bounds);
        let mut at = 0;
        while at < doomed.len() {
            let outer = self.entries[doomed[at] as usize];
            let (from, end) = (outer.from, outer.from + outer.len);
            debug_assert!(from as usize >= keep);
            let mut inner = at + 1;
            while inner < doomed.len() && self.entries[doomed[inner] as usize].from < end {
                inner += 1;
            }
            // The positions where the span's entries begin and end, to be
            // translated into item indexes as the span is walked.
            bounds.clear();
            for &i in &doomed[at..inner] {
                let entry = &self.entries[i as usize];
                bounds.push((entry.from, i));
                bounds.push((entry.from + entry.len, i | END));
            }
            bounds.sort_unstable_by_key(|&(position, _)| position);
            let mut bound = 0;
            let mut replay = self.replays.partition_point(|r| r.start < from);
            let mut position = from;
            loop {
                while let Some(&(at_position, tagged)) = bounds.get(bound) {
                    if at_position > position {
                        break;
                    }
                    // Entries never begin or end inside a replay.
                    debug_assert_eq!(at_position, position);
                    let item = self.saved.len() as u32;
                    let entry = &mut self.entries[(tagged & !END) as usize];
                    if tagged & END == 0 {
                        entry.origin = entry.from - from;
                        entry.from = item;
                    } else {
                        entry.len = item - entry.from;
                        entry.saved = true;
                    }
                    bound += 1;
                }
                if position >= end {
                    debug_assert_eq!(position, end, "a replay crosses an entry's end");
                    break;
                }
                match self.replays.get(replay) {
                    Some(r) if r.start == position => {
                        self.saved.push(Saved::Entry(r.entry));
                        position += r.len;
                        replay += 1;
                    }
                    _ => {
                        let event = relocate(events[position as usize], from, 0);
                        self.saved.push(Saved::Event(event));
                        position += 1;
                    }
                }
            }
            at = inner;
        }
        self.doomed = doomed;
        self.bounds = bounds;
        let kept = self.replays.partition_point(|r| (r.start as usize) < keep);
        self.replays.truncate(kept);
    }

    fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Stamps wrapped: no old stamp may match the new generation.
            self.heads.iter_mut().for_each(|h| *h = (0, 0));
            self.generation = 1;
        }
        self.entries.clear();
        self.saved.clear();
        self.live.clear();
        self.replays.clear();
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
    /// Set once an attempt, strict or not, has hit the depth limit during the
    /// current outermost speculation. Cleared when an outermost speculation
    /// begins and when it ends, so a construct nested too deeply limits only
    /// the speculation it is part of, not every one after it.
    hit_limit: bool,
    /// Counts events that make a strict result depend on more than the rule
    /// and position (the depth limit, the progress guard); such results are
    /// not memoized.
    taint: u32,
    memo: Memo,
    /// Whether strict attempts are memoized. Always on; the tests turn it off
    /// to check that memoization never changes a result.
    memoize: bool,
    /// The deepest grammar level the current attempt has reached.
    max_depth: u32,
    /// The rule the parse starts with.
    start: u32,
    /// By significant token: whether a line break precedes it (only when the
    /// grammar uses `LINE_START` or `NL_BEFORE`).
    nl_before: Vec<bool>,
    /// By significant token: the contextual keyword an `IDENT` spells, or
    /// `NOT_KEYWORD` (empty when the grammar has no contextual keywords).
    contextual: Vec<u16>,
    /// Tokens labelled in the current rule invocations, for text
    /// back-references: (label, token position).
    marks: Vec<(u16, u32)>,
    /// Where the innermost rule invocation's marks begin.
    mark_base: usize,
}

impl<'a> Parser<'a> {
    fn new(grammar: &'a Grammar, src: &'a str, tokens: &[Token<Kind>]) -> Self {
        let significant = tokens.iter().filter(|t| !t.is_trivia()).count();
        let mut kinds = Vec::with_capacity(significant + 1);
        let mut spans = Vec::with_capacity(significant + 1);
        let lines = grammar.program.v2.as_ref().is_some_and(|v| v.lines);
        let mut nl_before = Vec::new();
        let mut seen_break = true;
        for t in tokens {
            if t.is_trivia() {
                if lines {
                    let text = &src[t.span().start().to_usize()..t.span().end().to_usize()];
                    seen_break |= text.contains('\n');
                }
                continue;
            }
            kinds.push(t.kind().index());
            spans.push(t.span());
            if lines {
                nl_before.push(seen_break);
                // A line-break token itself ends a line.
                seen_break = t.kind().index() == grammar.program.newline;
            }
        }
        kinds.push(grammar.program.eof);
        let end = spans
            .last()
            .map_or(0, |s: &Span| s.end().to_u32())
            .max(tokens.last().map_or(0, |t| t.span().end().to_u32()));
        spans.push(Span::empty(end));
        if lines {
            nl_before.push(seen_break);
        }
        let mut contextual = Vec::new();
        if let Some(v2) = grammar
            .program
            .v2
            .as_ref()
            .filter(|v| !v.contextual.is_empty())
        {
            let ident = grammar.program.ident;
            contextual.reserve(kinds.len());
            let mut buf = String::new();
            for (k, span) in kinds.iter().zip(&spans) {
                let mut keyword = NOT_KEYWORD;
                if *k == ident {
                    let text = &src[span.start().to_usize()..span.end().to_usize()];
                    let text = if v2.case_insensitive {
                        buf.clear();
                        buf.extend(text.chars().map(|c| c.to_ascii_lowercase()));
                        buf.as_str()
                    } else {
                        text
                    };
                    if let Ok(at) = v2.contextual.binary_search_by(|(t, _)| (**t).cmp(text)) {
                        keyword = v2.contextual[at].1;
                    }
                }
                contextual.push(keyword);
            }
        }
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
            max_depth: 0,
            start: grammar.program.start,
            nl_before,
            contextual,
            marks: Vec::new(),
            mark_base: 0,
        }
    }

    /// The start rule, then any leftover input. Returns `false` only in strict
    /// mode, when the input does not match.
    fn top(&mut self) -> bool {
        let program = self.program;
        let start = self.start;
        let rule = &program.rules[start as usize];
        let root = rule.node.unwrap_or(self.program.error);
        self.events.push(Event::start(root, 0));
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
        self.events.push(Event::FINISH);
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
        self.max_depth = self.max_depth.max(self.depth);
        let ok = match expr {
            Expr::Rule(r) => self.rule(r),
            Expr::Seq { start, len } => self.seq(start, len),
            Expr::Choice { start, len } => self.choice(program.children(start, len)),
            Expr::Repeat {
                body,
                min_one,
                stop,
            } => self.repeat(body, min_one, stop),
            Expr::Optional(body) => {
                !self.at_set(program.first[body as usize]) || !self.guard(body) || self.expr(body)
            }
            Expr::Token(_) => true,
            other => self.expr_v2(e, other),
        };
        self.depth -= 1;
        ok
    }

    /// The format-2 expressions.
    #[inline(never)]
    fn expr_v2(&mut self, e: u32, expr: Expr) -> bool {
        match expr {
            Expr::Keyword(kind) => {
                if self.current() == kind {
                    self.bump();
                    return true;
                }
                if self.current() == self.program.ident && self.is_keyword_text(kind, self.pos) {
                    self.bump_as(kind);
                    return true;
                }
                self.missing_token(kind)
            }
            Expr::Word => {
                if self.at_set(self.program.first[e as usize]) && !self.at_eof() {
                    self.bump();
                    return true;
                }
                self.expected_word()
            }
            Expr::Label { label, body } => {
                self.events.push(Event::label(label));
                let before = self.pos;
                let ok = self.expr(body);
                self.events.push(Event::UNLABEL);
                if ok && self.pos > before && self.program.v2.as_ref().is_some_and(|v| v.backrefs) {
                    self.marks.push((label, self.pos as u32 - 1));
                }
                ok
            }
            Expr::And(body) => {
                if self.lookahead(body) {
                    return true;
                }
                self.failed_predicate(body, true)
            }
            Expr::Not(body) => {
                if !self.lookahead(body) {
                    return true;
                }
                self.failed_predicate(body, false)
            }
            Expr::BackRef { label, body } => self.backref(label, body),
            Expr::Eof => self.at_eof() || self.expected_eof(),
            Expr::LineStart | Expr::NlBefore => {
                let at = self.pos;
                let ok = self.nl_before.get(at).copied().unwrap_or(false)
                    || (matches!(expr, Expr::LineStart) && at == 0);
                ok || self.failed_assertion(matches!(expr, Expr::LineStart))
            }
            _ => true,
        }
    }

    /// Whether significant token `pos` is an `IDENT` spelling contextual
    /// keyword `kind`, under the keyword case policy.
    fn is_keyword_text(&self, kind: u16, pos: usize) -> bool {
        self.contextual.get(pos) == Some(&kind)
    }

    /// Whether a repetition or optional whose body begins with a predicate
    /// may commit: the predicate is asked first (format 2).
    #[inline]
    fn guard(&mut self, body: u32) -> bool {
        if !self.program.v2.as_ref().is_some_and(|v| v.predicates) {
            return true;
        }
        let mut e = body;
        loop {
            match self.program.exprs[e as usize] {
                Expr::And(inner) => return self.lookahead(inner),
                Expr::Not(inner) => return !self.lookahead(inner),
                Expr::Seq { start, len } if len > 0 => e = self.program.items[start as usize],
                Expr::Label { body, .. } => e = body,
                _ => return true,
            }
        }
    }

    /// Whether `e` would match here, without consuming input or recording
    /// anything: a strict attempt, rewound.
    fn lookahead(&mut self, e: u32) -> bool {
        let checkpoint = self.checkpoint();
        let outermost = self.speculating == 0;
        if outermost {
            self.hit_limit = false;
        }
        self.speculating += 1;
        self.strict += 1;
        let furthest = self.furthest;
        let ok = self.expr(e);
        self.furthest = furthest;
        self.strict -= 1;
        self.speculating -= 1;
        self.restore(checkpoint);
        if outermost {
            self.memo.clear();
            self.hit_limit = false;
        }
        ok
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            events: self.events.len(),
            pos: self.pos,
            marks: self.marks.len(),
        }
    }

    fn restore(&mut self, checkpoint: Checkpoint) {
        self.rewind(checkpoint.events);
        self.pos = checkpoint.pos;
        self.marks.truncate(checkpoint.marks);
    }

    /// `body=label`: the token's text must equal the text of the token last
    /// labelled `label` in this rule invocation.
    fn backref(&mut self, label: u16, body: u32) -> bool {
        let want = self.marks[self.mark_base..]
            .iter()
            .rev()
            .find(|(l, _)| *l == label)
            .map(|(_, at)| *at as usize);
        let same = want.is_some_and(|at| {
            let (a, b) = (self.text(at), self.text(self.pos));
            a == b
                || (self.program.v2.as_ref().is_some_and(|v| v.case_insensitive)
                    && self.program.exprs[body as usize] != Expr::Token(self.program.ident)
                    && a.eq_ignore_ascii_case(b))
        });
        if same {
            return self.expr(body);
        }
        if self.strict > 0 {
            return false;
        }
        self.mismatch(want);
        self.expr(body)
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
        if let Some((index, hit)) = self.memo.get(r, self.pos) {
            // P09: a result remembered with more levels to spare than are
            // left here could differ from parsing it again; parse it again.
            if self.depth + hit.deep <= MAX_DEPTH {
                return self.replay(index, hit);
            }
        }
        let first_event = self.events.len();
        let taint = self.taint;
        let outer = self.furthest;
        self.furthest = self.pos;
        let outer_depth = self.max_depth;
        self.max_depth = self.depth;
        let ok = self.rule_body(r, pos);
        let deep = self.max_depth - self.depth;
        self.max_depth = outer_depth.max(self.max_depth);
        let reach = self.furthest;
        self.furthest = outer.max(reach);
        if self.taint == taint {
            self.remember(r, pos, first_event, ok, reach, deep);
        }
        ok
    }

    /// Invokes rule `r` at `pos`, marking it active there so that a cycle of
    /// rules that consumes nothing is caught instead of followed.
    fn rule_body(&mut self, r: u32, pos: u32) -> bool {
        let rule = &self.program.rules[r as usize];
        let outer = core::mem::replace(&mut self.active[r as usize], pos);
        if let Some(kind) = rule.node {
            self.events.push(Event::start(kind, 0));
        }
        let base = core::mem::replace(&mut self.mark_base, self.marks.len());
        let sync = rule.sync != NO_SET && self.strict == 0;
        if sync {
            self.stops.push(rule.sync);
        }
        let ok = match rule.body {
            RuleBody::Expr(e) => self.expr(e),
            RuleBody::Pratt(p) => self.pratt(p, r, 0),
        };
        if sync {
            let _ = self.stops.pop();
        }
        self.marks.truncate(self.mark_base);
        self.mark_base = base;
        if ok && rule.node.is_some() {
            self.events.push(Event::FINISH);
        }
        self.active[r as usize] = outer;
        ok
    }

    /// Records a strict attempt's outcome. A success's events stay where
    /// they are, at the end of the event list, and the entry points at them.
    fn remember(
        &mut self,
        rule: u32,
        pos: u32,
        first_event: usize,
        ok: bool,
        reach: usize,
        deep: u32,
    ) {
        let mut entry = MemoEntry {
            rule,
            next: 0,
            end: FAILED,
            reach: reach as u32,
            deep,
            from: 0,
            len: 0,
            origin: 0,
            saved: true,
        };
        if ok {
            entry.end = self.pos as u32;
            let len = self.events.len() - first_event;
            if len > 0 {
                entry.from = first_event as u32;
                entry.len = len as u32;
                entry.saved = false;
            }
        }
        self.memo.put(entry, pos as usize, self.kinds.len());
    }

    /// Repeats a remembered attempt without parsing it again.
    fn replay(&mut self, index: u32, hit: MemoEntry) -> bool {
        self.furthest = self.furthest.max(hit.reach as usize);
        if hit.end == FAILED {
            return false;
        }
        let base = self.events.len();
        self.memo.expand(index, &mut self.events);
        self.memo.replayed(base, self.events.len() - base, index);
        self.pos = hit.end as usize;
        true
    }

    /// Truncates the event list to `keep`, first rescuing the events of any
    /// remembered attempt that would be cut off.
    fn rewind(&mut self, keep: usize) {
        self.memo.rescue(&self.events, keep);
        self.events.truncate(keep);
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
        if self.speculating == 0 {
            // A depth limit hit before this speculation began says nothing
            // about it: let it try every alternative again.
            self.hit_limit = false;
        }
        self.speculating += 1;
        let ok = self.try_alternatives(alternatives);
        self.speculating -= 1;
        if self.speculating == 0 {
            self.memo.clear();
            self.hit_limit = false;
        }
        ok
    }

    fn try_alternatives(&mut self, alternatives: &[u32]) -> bool {
        let program = self.program;
        let checkpoint = self.checkpoint();
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
            self.restore(checkpoint);
            // Results that hit the depth limit cannot be memoized, so trying
            // every alternative at every level of such input would take
            // exponential time. The input is reported as too deep regardless;
            // one attempt per decision keeps the rest of this speculation
            // linear. `speculate` clears the flag when the outermost
            // speculation ends, so the code after the deep construct is
            // parsed in full.
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
            if self.at_set(first) && self.guard(body) {
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
            if !recovering || self.at_eof() || self.in_stop(stop) || self.at_outer_stop() {
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
        self.max_depth = self.max_depth.max(self.depth);
        let ok = self.pratt_inner(p, rule, min_bp);
        self.depth -= 1;
        ok
    }

    /// The operator level `pratt` gives the current token, if any: from the
    /// `prefix` (or infix/postfix) table, or, for an `IDENT`, a contextual
    /// keyword operator with that text. The second value is the keyword kind
    /// to record the token as.
    fn operator(&self, p: u32, prefix: bool) -> (u8, Option<u16>) {
        let pratt = &self.program.pratts[p as usize];
        let current = self.current();
        let table = if prefix { &pratt.prefix } else { &pratt.after };
        let level = table.get(current as usize).copied().unwrap_or(0);
        if level != 0 || pratt.contextual.is_empty() || current != self.program.ident {
            return (level, None);
        }
        for &(kind, pre, after) in pratt.contextual.iter() {
            let level = if prefix { pre } else { after };
            if level != 0 && self.is_keyword_text(kind, self.pos) {
                // A keyword operator before something that cannot follow an
                // operator is an ordinary identifier (`await;`).
                let next = self
                    .kinds
                    .get(self.pos + 1)
                    .copied()
                    .unwrap_or(self.program.eof);
                let rule_first = self.program.rules.iter().find_map(|r| match r.body {
                    RuleBody::Pratt(q) if q == p => Some(r.first),
                    _ => None,
                });
                let fits = !prefix
                    || rule_first.is_some_and(|f| self.program.sets.contains(f, next as usize));
                if fits {
                    return (level, Some(kind));
                }
            }
        }
        (0, None)
    }

    fn pratt_inner(&mut self, p: u32, rule: u32, min_bp: u16) -> bool {
        let program = self.program;
        let pratt = &program.pratts[p as usize];
        // Format 2 labels the children of operator nodes.
        let labels = program.v2.as_ref().map(|v| v.op_labels);
        // A placeholder for the node that will wrap this operand, if any (and,
        // with labels, one for the `lhs` scope around the operand).
        let slot = self.events.len();
        self.events.push(Event::TOMBSTONE);
        if labels.is_some() {
            self.events.push(Event::TOMBSTONE);
        }
        let mut outer = slot;

        let (prefix, keyword) = self.operator(p, true);
        if prefix != 0 {
            let level = pratt.levels[prefix as usize - 1];
            self.events[slot] = Event::start(level.node, 0);
            self.operator_token(labels, keyword);
            if !self.operator_tail(level) {
                return false;
            }
            if let Some([.., operand]) = labels {
                self.events.push(Event::label(operand));
            }
            if !self.pratt(p, rule, level.rbp) {
                return false;
            }
            if labels.is_some() {
                self.events.push(Event::UNLABEL);
            }
            self.events.push(Event::FINISH);
        } else if self.at_set(program.first[pratt.operand as usize]) {
            if !self.expr(pratt.operand) {
                return false;
            }
        } else {
            return self.expected_rule(rule);
        }

        let mut chained: Option<u8> = None;
        loop {
            let (index, keyword) = self.operator(p, false);
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
            self.wrap(slot, &mut outer, level.node, labels);
            self.operator_token(labels, keyword);
            if !self.operator_tail(level) {
                return false;
            }
            if level.fixity != Fixity::Postfix {
                if let Some([_, _, rhs, _]) = labels {
                    self.events.push(Event::label(rhs));
                }
                if !self.pratt(p, rule, level.rbp) {
                    return false;
                }
                if labels.is_some() {
                    self.events.push(Event::UNLABEL);
                }
            }
            self.events.push(Event::FINISH);
            chained = (level.fixity == Fixity::NonAssoc).then_some(index);
        }
        true
    }

    /// Consumes an operator token, labelled `op` in format 2, recorded as
    /// `keyword` when it is a contextual keyword.
    fn operator_token(&mut self, labels: Option<[u16; 4]>, keyword: Option<u16>) {
        if let Some([_, op, ..]) = labels {
            self.events.push(Event::label(op));
        }
        match keyword {
            Some(kind) => self.bump_as(kind),
            None => self.bump(),
        }
        if labels.is_some() {
            self.events.push(Event::UNLABEL);
        }
    }

    /// The `then` part of an operator level, if it has one.
    fn operator_tail(&mut self, level: Level) -> bool {
        level.then.is_none_or(|then| self.expr(then))
    }

    /// Opens a node of `kind` around everything since `slot`. With labels,
    /// what it wraps becomes its `lhs` (or `operand`, for postfix).
    fn wrap(&mut self, slot: usize, outer: &mut usize, kind: Kind, labels: Option<[u16; 4]>) {
        if self.events[slot] == Event::TOMBSTONE {
            self.events[slot] = Event::start(kind, 0);
            if let Some([lhs, ..]) = labels {
                self.events[slot + 1] = Event::label(lhs);
                self.events.push(Event::UNLABEL);
            }
            return;
        }
        let new = self.events.len();
        self.events.push(Event::start(kind, 0));
        if let Step::Start { kind: wrapped, .. } = self.events[*outer].step() {
            let wrapped = match labels {
                Some([lhs, ..]) => wrapped.with_label(Some(lhs)),
                None => wrapped,
            };
            self.events[*outer] = Event::start(wrapped, new as u32);
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
            || (current == self.program.eof && sets.contains(set, self.program.newline as usize))
            || self.keyword_in(set)
    }

    /// Whether the current token is an `IDENT` spelling a contextual keyword
    /// that is in `set` (format 2).
    #[inline]
    fn keyword_in(&self, set: SetId) -> bool {
        !self.contextual.is_empty()
            && self
                .contextual
                .get(self.pos)
                .is_some_and(|&k| k != NOT_KEYWORD && self.program.sets.contains(set, k as usize))
    }

    /// Whether the current token is in stop set `set`.
    #[inline]
    fn in_stop(&self, set: SetId) -> bool {
        self.program.sets.contains(set, self.current() as usize) || self.keyword_in(set)
    }

    fn at_outer_stop(&self) -> bool {
        self.stops.iter().any(|&s| self.in_stop(s))
    }

    #[inline]
    fn bump(&mut self) {
        self.events.push(Event::TOKEN);
        self.pos += 1;
        self.furthest = self.furthest.max(self.pos);
    }

    /// Consumes the current token, recording it as keyword `kind`.
    fn bump_as(&mut self, kind: u16) {
        let kind = self.grammar.kinds.at(kind as usize);
        self.events.push(Event::token_as(kind));
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
            self.events.push(Event::start(self.program.error, 0));
            self.bump();
        }
        self.events.push(Event::FINISH);
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
        if kind == self.program.newline && self.at_eof() {
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

    /// `WORD` found something else.
    #[cold]
    #[inline(never)]
    fn expected_word(&mut self) -> bool {
        if self.strict > 0 {
            return false;
        }
        let message = format!("expected an identifier or keyword, found {}", self.found());
        self.report(message);
        true
    }

    /// A predicate that failed outside speculation: reported, and the parse
    /// carries on as if it had held.
    #[cold]
    #[inline(never)]
    fn failed_predicate(&mut self, body: u32, positive: bool) -> bool {
        if self.strict > 0 {
            return false;
        }
        let message = if positive {
            format!(
                "expected {}, found {}",
                self.describe_expr(body),
                self.found()
            )
        } else {
            format!("{} is not allowed here", capitalize(&self.found()))
        };
        self.report(message);
        true
    }

    /// `EOF` found more input.
    #[cold]
    #[inline(never)]
    fn expected_eof(&mut self) -> bool {
        if self.strict > 0 {
            return false;
        }
        let message = format!("expected the end of the input, found {}", self.found());
        self.report(message);
        true
    }

    /// `LINE_START` or `NL_BEFORE` did not hold.
    #[cold]
    #[inline(never)]
    fn failed_assertion(&mut self, line_start: bool) -> bool {
        if self.strict > 0 {
            return false;
        }
        let message = if line_start {
            format!("expected {} to begin a line", self.found())
        } else {
            format!("expected a line break before {}", self.found())
        };
        self.report(message);
        true
    }

    /// A text back-reference whose text differs from the labelled token's.
    #[cold]
    #[inline(never)]
    fn mismatch(&mut self, want: Option<usize>) {
        let found = self.text(self.pos);
        let message = match want {
            Some(at) => format!("`{found}` does not match `{}`", self.text(at)),
            None => format!("`{found}` has nothing earlier to match"),
        };
        self.report_code(codes::PARSE_MISMATCH, message);
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
        self.report_code(codes::PARSE_CHAINED, message);
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
        self.report_code(codes::PARSE_LEFTOVER, message);
        self.events.push(Event::start(self.program.error, 0));
        while !self.at_eof() {
            self.bump();
        }
        self.events.push(Event::FINISH);
    }

    fn report(&mut self, message: String) {
        self.report_code(codes::PARSE_EXPECTED, message);
    }

    fn report_code(&mut self, code: Code, message: String) {
        if self.last_error == self.pos {
            return;
        }
        self.last_error = self.pos;
        let span = self.spans[self.pos];
        self.diags.push(
            Diagnostic::new(Severity::Error, message, Label::unlabelled(span)).with_code(code),
        );
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
            ).with_code(codes::PARSE_TOO_DEEP));
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
        let program = self.program;
        match kind {
            k if k == program.ident => format!("identifier `{short}{ellipsis}`"),
            k if k == program.number => format!("number `{short}{ellipsis}`"),
            k if program.strings.contains(&k) => String::from("a string"),
            k if k == program.newline => String::from("a line break"),
            k if self.grammar.kinds.cats.get(k as usize) == Some(&CAT_CLASS) && text.is_empty() => {
                String::from(self.grammar.kinds.name_at(k as usize))
            }
            _ => format!("`{short}{ellipsis}`"),
        }
    }

    /// A token kind, for "expected ...".
    fn describe(&self, kind: u16) -> String {
        let program = self.program;
        match kind {
            k if k == program.ident => String::from("an identifier"),
            k if k == program.number => String::from("a number"),
            k if program.strings.contains(&k) => String::from("a string"),
            k if k == program.newline => String::from("a line break"),
            k if k == program.eof => String::from("the end of the input"),
            k if matches!(
                self.grammar.kinds.cats.get(k as usize),
                Some(&CAT_CLASS | &CAT_NODE)
            ) =>
            {
                String::from(self.grammar.kinds.name_at(k as usize))
            }
            k => format!("`{}`", self.grammar.kinds.name_at(k as usize)),
        }
    }

    fn describe_expr(&self, e: u32) -> String {
        match self.program.exprs[e as usize] {
            Expr::Rule(r) => String::from(&*self.program.rules[r as usize].name),
            Expr::Token(kind) | Expr::Keyword(kind) => self.describe(kind),
            Expr::Repeat { body, .. }
            | Expr::Optional(body)
            | Expr::Label { body, .. }
            | Expr::And(body)
            | Expr::BackRef { body, .. } => self.describe_expr(body),
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

/// `text` with its first letter in upper case.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
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

    /// Nested speculation that rewinds at every level, so the memo rescues
    /// spans that hold replays of spans it rescued before: memoized parsing
    /// must still agree with unmemoized parsing exactly, with and without
    /// errors. (Past the depth limit the two may differ: a rule remembered
    /// at a shallow depth is replayed where parsing it would hit the limit.
    /// Such input is reported as nested too deeply either way.)
    #[test]
    fn test_memo_rescues_agree_with_unmemoized_parsing() {
        let lang = speculative_language();
        let nested = |depth: usize, width: usize, inner: &str, tail: &str| {
            let mut src = String::new();
            for _ in 0..depth {
                src.push_str("{ ");
                for i in 0..width {
                    src.push_str(&format!("f(x{i}, {{ y; }}) + -1; z = {{ {i}; }}; "));
                }
            }
            src.push_str(inner);
            for _ in 0..depth {
                src.push_str(tail);
            }
            src
        };
        for (depth, width, inner, tail) in [
            (1, 1, "x;", " };"),
            (4, 2, "x = 1;", " };"),
            (6, 3, "x", " }"),
            (6, 3, "x = ;", " } = 1;"),
            (8, 1, "f(", " };"),
            (10, 1, "x;", " };"),
        ] {
            let src = nested(depth, width, inner, tail);
            let handle = std::thread::Builder::new()
                .stack_size(8 << 20)
                .spawn({
                    let lang = lang.clone();
                    move || {
                        let memoized = parse(lang.grammar(), &src);
                        let plain = parse_unmemoized(lang.grammar(), &src);
                        assert_eq!(memoized.0, plain.0, "trees differ for {src:?}");
                        assert_eq!(memoized.1, plain.1, "diagnostics differ for {src:?}");
                        assert_eq!(memoized.0.text(&src), Some(src.as_str()));
                    }
                })
                .unwrap();
            handle.join().unwrap();
        }
    }

    // ----- format 2 -----

    /// A format-2 grammar using every parser feature format 2 adds: labels
    /// (on tokens, nodes, groups), predicates, a text back-reference,
    /// contextual keywords (in rules and as Pratt operators), case-folded
    /// keywords, interpolated strings with a rule inside, and ordered
    /// choice that speculates across rules.
    fn v2_language() -> Language {
        let sketch = "[sketch]\nformat = 2\n[language]\nname = \"v\"\nversion = \"1.0.0\"\n\
             [lexer]\nline_comments = [\"#\"]\n\
             [lexer.keywords]\ncontextual = [\"async\", \"await\", \"is\"]\ncase = \"ascii-insensitive\"\n\
             [lexer.strings.DQ]\nopen = '\"'\nescape = \"\\\\\"\ninterpolate = [{ open = \"{\", close = \"}\", rule = \"expr\" }]\n\
             [rules]\nfile = \"items:stmt*\"\n\
             stmt = \"assign | fn_def | block_stmt | value:expr ';' | 'let' name:IDENT ';'\"\n\
             assign = \"target:expr '=' value:expr ';'\"\n\
             fn_def = \"'async'? 'fn' name:IDENT '(' (params:IDENT (',' params:IDENT)*)? ')' body:block\"\n\
             block_stmt = \"'begin' tag:IDENT body:stmt* 'end' IDENT=tag ';'\"\n\
             block = \"'{' stmts:stmt* '}'\"\n\
             call = \"callee:IDENT !'=' '(' (args:expr (',' args:expr)*)? ')'\"\n\
             [rules.expr]\noperand = \"call | IDENT | NUMBER | DQ | block\"\n\
             levels = [{ left = [\"is\"] }, { left = [\"+\"] }, { prefix = [\"-\", \"await\"] }]\n";
        Language::from_lsf(sketch).unwrap_or_else(|e| panic!("{e}\n{:?}", e.diagnostics()))
    }

    const V2_WORDS: [&str; 26] = [
        "x",
        "f",
        "1",
        "=",
        ";",
        "{",
        "}",
        "(",
        ")",
        ",",
        "+",
        "-",
        "let",
        "async",
        "await",
        "is",
        "fn",
        "FN",
        "begin",
        "end",
        "\"a {x} b\"",
        "\"",
        "{x",
        "# c\n",
        "\\",
        "IS",
    ];

    /// The tree as text, with every kind and label: trees that differ only
    /// in a label differ here (kinds compare equal regardless of labels).
    fn labelled_dump(lang: &Language, node: &Node<Kind>) -> String {
        fn walk(lang: &Language, node: &Node<Kind>, depth: usize, out: &mut String) {
            out.push_str(&format!(
                "{depth}:{:?}:{}@{:?}\n",
                node.kind().label(),
                lang.kind_name(*node.kind()),
                node.span()
            ));
            for child in node.children() {
                match child {
                    syntax_lang::Element::Node(n) => walk(lang, n, depth + 1, out),
                    syntax_lang::Element::Token(t) => {
                        out.push_str(&format!(
                            "{}:{:?}:{}@{:?}\n",
                            depth + 1,
                            t.kind().label(),
                            lang.kind_name(*t.kind()),
                            t.span()
                        ));
                    }
                }
            }
        }
        // The test inputs nest a few dozen levels at most.
        let mut out = String::new();
        walk(lang, node, 0, &mut out);
        out
    }

    #[test]
    fn test_v2_samples_agree_with_strict() {
        let lang = v2_language();
        for src in [
            "let x; x = f(1, -2) + 3; async fn g(a, b) { await g(1); }",
            "begin a x; begin b end b; end a; \"s {x + 1} t\";",
            "async = 1; await; x is f(1) is 2; FN h() { }",
        ] {
            let parse = lang.parse(src);
            assert!(!parse.has_errors(), "{src:?}: {:?}", messages(&lang, src));
            let strict = parse_strict(lang.grammar(), src).expect("strict parse succeeds");
            assert_eq!(
                labelled_dump(&lang, &strict),
                labelled_dump(&lang, parse.tree())
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// Format 2: lossless, labels included in the strict agreement.
        #[test]
        fn prop_v2_parse_is_lossless_and_agrees_with_strict(words in proptest::collection::vec(0..V2_WORDS.len(), 0..40)) {
            let lang = v2_language();
            let src: String = words.iter().map(|&w| V2_WORDS[w]).collect::<Vec<_>>().join(" ");
            let parse = lang.parse(&src);
            prop_assert_eq!(parse.tree().text(&src), Some(src.as_str()));
            prop_assert_eq!(parse.tree().span(), Span::new(0, src.len() as u32));
            let strict = parse_strict(lang.grammar(), &src);
            // Lexical errors (an unknown character, an unterminated string)
            // are the lexer's; strict mode judges the grammar only.
            let parse_errors = parse
                .diagnostics()
                .iter()
                .any(|d| d.code().is_some_and(|c| c.to_string().starts_with("LF1")));
            if parse_errors {
                prop_assert!(strict.is_none(), "strict accepted input with errors: {:?}", src);
            } else {
                let strict = strict.expect("strict agrees");
                prop_assert_eq!(labelled_dump(&lang, &strict), labelled_dump(&lang, parse.tree()), "trees differ for {:?}", src);
            }
        }

        /// Format 2: memoization is invisible, labels included.
        #[test]
        fn prop_v2_memoization_never_changes_a_result(words in proptest::collection::vec(0..V2_WORDS.len(), 0..40)) {
            let lang = v2_language();
            let src: String = words.iter().map(|&w| V2_WORDS[w]).collect::<Vec<_>>().join(" ");
            let memoized = parse(lang.grammar(), &src);
            let plain = parse_unmemoized(lang.grammar(), &src);
            prop_assert_eq!(labelled_dump(&lang, &memoized.0), labelled_dump(&lang, &plain.0), "trees differ for {:?}", src);
            prop_assert_eq!(memoized.1, plain.1, "diagnostics differ for {:?}", src);
        }

        #[test]
        fn prop_v2_arbitrary_text_never_panics(src in "\\PC{0,80}") {
            let lang = v2_language();
            let parse = lang.parse(&src);
            prop_assert_eq!(parse.tree().text(&src), Some(src.as_str()));
        }
    }

    /// ISSUES P09: a rule remembered at a shallow depth must not be replayed
    /// where parsing it would pass the depth limit. Here `x` is parsed
    /// under `a` (depth d) and memoized, then needed again under `b`, two
    /// levels deeper; near the limit, replaying it would accept input that
    /// the unmemoized parser reports as nested too deeply. Memoized and
    /// unmemoized parsing agree at every nesting around the limit.
    #[test]
    fn test_memo_replay_respects_the_depth_limit() {
        let lang = forge(
            "[rules]\ns = \"a | b\"\na = \"x 'q'\"\nb = \"w\"\nw = \"v\"\nv = \"x 'r'\"\nx = \"'(' x ')' | 'z'\"\n",
        );
        // One parenthesis costs several grammar levels, so the nestings tried
        // start well below the limit; without the P09 check they differ
        // from nesting 254 on.
        let limit = MAX_DEPTH as usize;
        let handle = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || {
                let mut deep = 0;
                for n in (limit / 4)..(limit + 4) {
                    let src = format!("{}z{} r", "(".repeat(n), ")".repeat(n));
                    let memoized = parse(lang.grammar(), &src);
                    let plain = parse_unmemoized(lang.grammar(), &src);
                    assert_eq!(memoized.0, plain.0, "trees differ at nesting {n}");
                    assert_eq!(memoized.1, plain.1, "diagnostics differ at nesting {n}");
                    assert_eq!(memoized.0.text(&src), Some(src.as_str()));
                    if memoized
                        .1
                        .iter()
                        .any(|d| d.message().contains("nested too deeply"))
                    {
                        deep += 1;
                    }
                }
                // The range crosses the limit: some inputs are too deep.
                assert!(deep > 0);
            })
            .unwrap();
        handle.join().unwrap();
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
