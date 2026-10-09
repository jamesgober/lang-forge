//! The repetition-overlap check (LSF2 §11.8, ISSUES M02).
//!
//! The parser's repetitions and optionals are greedy: `x*` and `x?` commit to
//! `x` as soon as the current token can begin it. When a token `t` can both
//! begin the body and follow the repetition, committing is a guess, and the
//! guess is wrong exactly when some token `u` can follow `t` on the
//! follower's side but cannot come next once the body has taken `t`: the
//! input `t u`, which the grammar appears to allow, is rejected. That is the
//! check, stated as LL(2): for each such `t`, the tokens that can follow `t`
//! *after* the repetition must be among those that can follow it once the
//! body took it (inside the body, or after it when the body can be exactly
//! `t`). A violation is reported with the witness input `t u`.
//!
//! A repetition or optional whose body begins with a predicate is exempt —
//! the predicate decides whether to commit (`(&(',' !')') ',' x)*`) — and so
//! is every repetition of a rule that says `allow = ["overlap"]`. A
//! predicate elsewhere constrains what may come next: `&CLOSE_TAG` lets only
//! `CLOSE_TAG` through, which keeps the check from inventing followers. A
//! contextual keyword is its own token here, as it is to the parser.
//! Divergences deeper than two tokens are not detected; the check is LL(2)
//! as specified.
//!
//! Everything is computed with bitsets over the token kinds by monotone
//! fixpoints: FOLLOW once for the grammar, and for each distinct candidate
//! token `t` what follows `t` inside each expression and what follows `t`
//! after it. The work is bounded: a grammar for which it would exceed the
//! budget gets a warning that the check was not run, never a silent pass.

use alloc::{format, string::String, vec, vec::Vec};

use crate::{
    codes,
    grammar::{Analysis, Expr, Kinds, Pratt, Rule, RuleBody},
    schematic::Fixity,
    set::SetId,
};

/// The most bitset words the check may touch in total.
const WORK_BUDGET: u64 = 1 << 30;

/// The most memory the check's sets may take.
const MEMORY_BUDGET: usize = 64 << 20;

/// A row-per-expression bitset table.
struct Table {
    width: usize,
    words: Vec<u64>,
}

impl Table {
    fn new(rows: usize, width: usize) -> Self {
        Self {
            width,
            words: vec![0; rows * width],
        }
    }

    fn row(&self, r: usize) -> &[u64] {
        &self.words[r * self.width..(r + 1) * self.width]
    }

    fn clear(&mut self) {
        self.words.iter_mut().for_each(|w| *w = 0);
    }

    fn insert(&mut self, r: usize, bit: usize) {
        self.words[r * self.width + (bit >> 6)] |= 1u64 << (bit & 63);
    }

    fn contains(&self, r: usize, bit: usize) -> bool {
        (self.words[r * self.width + (bit >> 6)] >> (bit & 63)) & 1 != 0
    }

    /// `row[dst] |= src`; whether it changed.
    fn or(&mut self, dst: usize, src: &[u64]) -> bool {
        let row = &mut self.words[dst * self.width..(dst + 1) * self.width];
        let mut changed = false;
        for (d, s) in row.iter_mut().zip(src) {
            let merged = *d | *s;
            changed |= merged != *d;
            *d = merged;
        }
        changed
    }
}

fn or_into(dst: &mut [u64], src: &[u64]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d |= *s;
    }
}

fn has(row: &[u64], bit: usize) -> bool {
    (row[bit >> 6] >> (bit & 63)) & 1 != 0
}

/// The grammar as the check sees it.
struct View<'a, 'r> {
    a: &'a Analysis<'r>,
    rules: &'a [Rule],
    pratts: &'a [Pratt],
    width: usize,
    /// Rows: one per expression, then one per rule.
    n: usize,
}

impl View<'_, '_> {
    fn rule_row(&self, r: usize) -> usize {
        self.n + r
    }

    /// The viability FIRST set of `e` (the parser's: a positive predicate
    /// contributes its body's FIRST set).
    fn vfirst(&self, e: usize, out: &mut [u64]) {
        self.set_into(self.a.first[e], out);
    }

    fn set_into(&self, set: SetId, out: &mut [u64]) {
        for m in self.a.sets.members(set) {
            out[m >> 6] |= 1 << (m & 63);
        }
    }

    fn vnull(&self, e: usize) -> bool {
        self.a.nullable[e]
    }
}

/// Runs the check, reporting each divergence as `LSF4301` (an error when
/// `deny`, else a warning).
#[allow(clippy::too_many_lines)]
pub(crate) fn check(
    a: &mut Analysis<'_>,
    rules: &[Rule],
    pratts: &[Pratt],
    allowed: &[bool],
    kinds: &Kinds,
    deny: bool,
    eof: u16,
) {
    let n = a.exprs.len();
    let bits = usize::from(eof) + 1;
    let width = bits.div_ceil(64).max(1);
    let rows = n + rules.len();
    if rows
        .saturating_mul(width)
        .saturating_mul(8)
        .saturating_mul(6)
        > MEMORY_BUDGET
    {
        skipped(
            a,
            "the grammar is too large for the overlap check's memory budget",
        );
        return;
    }
    let owned = core::mem::take(&mut a.owned);
    let order = a.rule_order(&owned);
    let reports = run(
        &View {
            a,
            rules,
            pratts,
            width,
            n,
        },
        &owned,
        &order,
        allowed,
        kinds,
        deny,
        eof,
        bits,
    );
    a.owned = owned;
    match reports {
        Ok(reports) => {
            for d in reports {
                a.report.diagnostic(d);
            }
        }
        Err(why) => skipped(a, why),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run(
    v: &View<'_, '_>,
    owned: &[Vec<u32>],
    order: &[u32],
    allowed: &[bool],
    kinds: &Kinds,
    deny: bool,
    eof: u16,
    bits: usize,
) -> Result<Vec<diag_lang::Diagnostic>, &'static str> {
    let a = v.a;
    let width = v.width;
    let rows = v.n + v.rules.len();
    let mut budget = WORK_BUDGET;
    let ops_after: Vec<Vec<u64>> = v
        .pratts
        .iter()
        .map(|p| {
            let mut row = vec![0u64; width];
            for (k, l) in p.after.iter().enumerate() {
                if *l != 0 {
                    row[k >> 6] |= 1 << (k & 63);
                }
            }
            for &(k, _, after) in p.contextual.iter() {
                if after != 0 {
                    row[usize::from(k) >> 6] |= 1 << (k & 63);
                }
            }
            row
        })
        .collect();
    let rule_first = |r: usize| -> Vec<u64> {
        let mut row = vec![0u64; width];
        v.set_into(v.rules[r].first, &mut row);
        row
    };

    // ----- consuming FIRST: what the body can take as its first token -----
    let mut cfirst = Table::new(rows, width);
    let mut cnull = vec![false; rows];
    let mut scratch = vec![0u64; width];
    loop {
        let mut changed = false;
        for &r in order {
            let r = r as usize;
            for &e in &owned[r] {
                let e = e as usize;
                scratch.iter_mut().for_each(|w| *w = 0);
                let null = match a.exprs[e] {
                    Expr::Token(_) | Expr::Keyword(_) | Expr::Word => {
                        v.vfirst(e, &mut scratch);
                        false
                    }
                    Expr::Rule(callee) => {
                        or_into(&mut scratch, cfirst.row(v.rule_row(callee as usize)));
                        cnull[v.rule_row(callee as usize)]
                    }
                    Expr::Seq { start, len } => {
                        let mut all = true;
                        for &item in a.children(start, len) {
                            or_into(&mut scratch, cfirst.row(item as usize));
                            if !cnull[item as usize] {
                                all = false;
                                break;
                            }
                        }
                        all
                    }
                    Expr::Choice { start, len } => {
                        let mut any = false;
                        for &item in a.children(start, len) {
                            or_into(&mut scratch, cfirst.row(item as usize));
                            any |= cnull[item as usize];
                        }
                        any
                    }
                    Expr::Repeat { body, min_one, .. } => {
                        or_into(&mut scratch, cfirst.row(body as usize));
                        !min_one || cnull[body as usize]
                    }
                    Expr::Optional(body) => {
                        or_into(&mut scratch, cfirst.row(body as usize));
                        true
                    }
                    Expr::Label { body, .. } | Expr::BackRef { body, .. } => {
                        or_into(&mut scratch, cfirst.row(body as usize));
                        cnull[body as usize]
                    }
                    Expr::And(_) | Expr::Not(_) | Expr::Eof | Expr::LineStart | Expr::NlBefore => {
                        true
                    }
                };
                changed |= cfirst.or(e, &scratch);
                if null && !cnull[e] {
                    cnull[e] = true;
                    changed = true;
                }
            }
            scratch.iter_mut().for_each(|w| *w = 0);
            let null = match v.rules[r].body {
                RuleBody::Expr(e) => {
                    or_into(&mut scratch, cfirst.row(e as usize));
                    cnull[e as usize]
                }
                RuleBody::Pratt(p) => {
                    let pratt = &v.pratts[p as usize];
                    for (k, l) in pratt.prefix.iter().enumerate() {
                        if *l != 0 {
                            scratch[k >> 6] |= 1 << (k & 63);
                        }
                    }
                    for &(k, prefix, _) in pratt.contextual.iter() {
                        if prefix != 0 {
                            scratch[usize::from(k) >> 6] |= 1 << (k & 63);
                        }
                    }
                    or_into(&mut scratch, cfirst.row(pratt.operand as usize));
                    cnull[pratt.operand as usize]
                }
            };
            changed |= cfirst.or(v.rule_row(r), &scratch);
            if null && !cnull[v.rule_row(r)] {
                cnull[v.rule_row(r)] = true;
                changed = true;
            }
        }
        budget = budget.saturating_sub((rows * width) as u64);
        if budget == 0 {
            return Err("the grammar is too large for the overlap check's work budget");
        }
        if !changed {
            break;
        }
    }

    // ----- FOLLOW, with the parser's (viability) FIRST sets -----
    let mut follow = Table::new(rows, width);
    let mut called = vec![false; v.rules.len()];
    for &e in owned.iter().flatten() {
        if let Expr::Rule(c) = a.exprs[e as usize] {
            called[c as usize] = true;
        }
    }
    for (r, &called) in called.iter().enumerate() {
        // Entry points (the start rule, interpolation and injection rules)
        // can be followed by the end of the input.
        if !called {
            follow.insert(v.rule_row(r), usize::from(eof));
        }
    }
    let mut rest = vec![0u64; width];
    loop {
        let mut changed = false;
        for &r in order.iter().rev() {
            let r = r as usize;
            let own_rule = follow.row(v.rule_row(r)).to_vec();
            match v.rules[r].body {
                RuleBody::Expr(e) => changed |= follow.or(e as usize, &own_rule),
                RuleBody::Pratt(p) => {
                    let pratt = &v.pratts[p as usize];
                    let operand = pratt.operand as usize;
                    changed |= follow.or(operand, &ops_after[p as usize]);
                    changed |= follow.or(operand, &own_rule);
                    let first = rule_first(r);
                    for level in pratt.levels.iter() {
                        let Some(then) = level.then else { continue };
                        if level.fixity == Fixity::Postfix {
                            changed |= follow.or(then as usize, &ops_after[p as usize]);
                            changed |= follow.or(then as usize, &own_rule);
                        } else {
                            changed |= follow.or(then as usize, &first);
                        }
                    }
                }
            }
            for &e in owned[r].iter().rev() {
                let e = e as usize;
                let own = follow.row(e).to_vec();
                match a.exprs[e] {
                    Expr::Rule(callee) => changed |= follow.or(v.rule_row(callee as usize), &own),
                    Expr::Seq { start, len } => {
                        rest.iter_mut().for_each(|w| *w = 0);
                        let mut nullable = true;
                        for &item in a.children(start, len).iter().rev() {
                            let item = item as usize;
                            changed |= follow.or(item, &rest);
                            if nullable {
                                changed |= follow.or(item, &own);
                            }
                            if !v.vnull(item) {
                                rest.iter_mut().for_each(|w| *w = 0);
                                nullable = false;
                            }
                            v.vfirst(item, &mut rest);
                        }
                    }
                    Expr::Choice { start, len } => {
                        for &item in a.children(start, len) {
                            changed |= follow.or(item as usize, &own);
                        }
                    }
                    Expr::Repeat { body, .. } => {
                        scratch.iter_mut().for_each(|w| *w = 0);
                        v.vfirst(body as usize, &mut scratch);
                        changed |= follow.or(body as usize, &scratch);
                        changed |= follow.or(body as usize, &own);
                    }
                    Expr::Optional(body)
                    | Expr::Label { body, .. }
                    | Expr::BackRef { body, .. } => {
                        changed |= follow.or(body as usize, &own);
                    }
                    _ => {}
                }
            }
        }
        budget = budget.saturating_sub((rows * width) as u64);
        if budget == 0 {
            return Err("the grammar is too large for the overlap check's work budget");
        }
        if !changed {
            break;
        }
    }

    // ----- candidates -----
    struct Candidate {
        expr: usize,
        body: usize,
        repeat: bool,
        tokens: Vec<usize>,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for (r, list) in owned.iter().enumerate() {
        if allowed.get(r).copied().unwrap_or(false) {
            continue;
        }
        for &e in list {
            let e = e as usize;
            let (body, repeat) = match a.exprs[e] {
                Expr::Repeat { body, .. } => (body as usize, true),
                Expr::Optional(body) => (body as usize, false),
                _ => continue,
            };
            if begins_with_predicate(a, body) {
                continue;
            }
            let tokens: Vec<usize> = (0..bits)
                .filter(|&t| cfirst.contains(body, t) && follow.contains(e, t))
                .collect();
            if !tokens.is_empty() {
                candidates.push(Candidate {
                    expr: e,
                    body,
                    repeat,
                    tokens,
                });
            }
        }
    }
    let mut out = Vec::new();
    if candidates.is_empty() {
        return Ok(out);
    }
    let mut ts: Vec<usize> = candidates
        .iter()
        .flat_map(|c| c.tokens.iter().copied())
        .collect();
    ts.sort_unstable();
    ts.dedup();

    // ----- per candidate token -----
    let mut fa = Table::new(rows, width);
    let mut j = vec![false; rows];
    let mut tr = vec![false; rows];
    let mut ft = Table::new(rows, width);
    let mut reported = vec![false; v.n];
    for &t in &ts {
        // Whether each expression lets `t` through without consuming it
        // (TR), what can follow `t` inside it when it begins with `t` (FA),
        // and whether it can be exactly `t` (J): bottom-up.
        fa.clear();
        j.iter_mut().for_each(|x| *x = false);
        tr.iter_mut().for_each(|x| *x = false);
        loop {
            let mut changed = false;
            for &r in order {
                let r = r as usize;
                for &e in &owned[r] {
                    let e = e as usize;
                    scratch.iter_mut().for_each(|w| *w = 0);
                    let (exact, through) = match a.exprs[e] {
                        Expr::Token(_) | Expr::Keyword(_) | Expr::Word => {
                            (cfirst.contains(e, t), false)
                        }
                        Expr::Rule(callee) => {
                            let row = v.rule_row(callee as usize);
                            or_into(&mut scratch, fa.row(row));
                            (j[row], tr[row])
                        }
                        Expr::Seq { start, len } => {
                            let items = a.children(start, len);
                            let mut exact = false;
                            let mut through = true;
                            for (k, &item) in items.iter().enumerate() {
                                let item = item as usize;
                                or_into(&mut scratch, fa.row(item));
                                if j[item] {
                                    // `t`, then the rest of the sequence.
                                    let mut rest_null = true;
                                    for &next in &items[k + 1..] {
                                        v.vfirst(next as usize, &mut scratch);
                                        if !v.vnull(next as usize) {
                                            rest_null = false;
                                            break;
                                        }
                                    }
                                    exact |= rest_null;
                                }
                                if !tr[item] {
                                    through = false;
                                    break;
                                }
                            }
                            (exact, through)
                        }
                        Expr::Choice { start, len } => {
                            let (mut exact, mut through) = (false, false);
                            for &item in a.children(start, len) {
                                or_into(&mut scratch, fa.row(item as usize));
                                exact |= j[item as usize];
                                through |= tr[item as usize];
                            }
                            (exact, through)
                        }
                        Expr::Repeat { body, min_one, .. } => {
                            let body = body as usize;
                            or_into(&mut scratch, fa.row(body));
                            if j[body] {
                                v.vfirst(body, &mut scratch);
                            }
                            (j[body], !min_one || tr[body])
                        }
                        Expr::Optional(body) => {
                            or_into(&mut scratch, fa.row(body as usize));
                            (j[body as usize], true)
                        }
                        Expr::Label { body, .. } | Expr::BackRef { body, .. } => {
                            or_into(&mut scratch, fa.row(body as usize));
                            (j[body as usize], tr[body as usize])
                        }
                        // A positive predicate lets `t` through only if `t`
                        // can begin its body.
                        Expr::And(body) => {
                            let body = body as usize;
                            let mut first = vec![0u64; width];
                            v.vfirst(body, &mut first);
                            (false, v.vnull(body) || has(&first, t))
                        }
                        Expr::Eof => (false, t == usize::from(eof)),
                        Expr::Not(_) | Expr::LineStart | Expr::NlBefore => (false, true),
                    };
                    changed |= fa.or(e, &scratch);
                    if exact && !j[e] {
                        j[e] = true;
                        changed = true;
                    }
                    if through && !tr[e] {
                        tr[e] = true;
                        changed = true;
                    }
                }
                // The rule.
                let row = v.rule_row(r);
                scratch.iter_mut().for_each(|w| *w = 0);
                let (exact, through) = match v.rules[r].body {
                    RuleBody::Expr(e) => {
                        or_into(&mut scratch, fa.row(e as usize));
                        (j[e as usize], tr[e as usize])
                    }
                    RuleBody::Pratt(p) => {
                        let pratt = &v.pratts[p as usize];
                        let operand = pratt.operand as usize;
                        // `t` as a prefix operator: what may come after it.
                        let mut prefix_level = pratt.prefix.get(t).copied().unwrap_or(0);
                        for &(k, prefix, _) in pratt.contextual.iter() {
                            if usize::from(k) == t && prefix != 0 {
                                prefix_level = prefix;
                            }
                        }
                        if prefix_level != 0 {
                            let level = pratt.levels[usize::from(prefix_level) - 1];
                            match level.then {
                                Some(then) => {
                                    v.vfirst(then as usize, &mut scratch);
                                    if v.vnull(then as usize) {
                                        or_into(&mut scratch, &rule_first(r));
                                    }
                                }
                                None => or_into(&mut scratch, &rule_first(r)),
                            }
                        }
                        or_into(&mut scratch, fa.row(operand));
                        if j[operand] {
                            or_into(&mut scratch, &ops_after[p as usize]);
                        }
                        (j[operand], tr[operand])
                    }
                };
                changed |= fa.or(row, &scratch);
                if exact && !j[row] {
                    j[row] = true;
                    changed = true;
                }
                if through && !tr[row] {
                    tr[row] = true;
                    changed = true;
                }
            }
            budget = budget.saturating_sub((rows * width) as u64);
            if budget == 0 {
                return Err("the grammar is too large for the overlap check's work budget");
            }
            if !changed {
                break;
            }
        }

        // What can come right after `t` when `t` comes right after each
        // expression (FT): top-down.
        ft.clear();
        loop {
            let mut changed = false;
            for &r in order.iter().rev() {
                let r = r as usize;
                let own_rule = ft.row(v.rule_row(r)).to_vec();
                match v.rules[r].body {
                    RuleBody::Expr(e) => changed |= ft.or(e as usize, &own_rule),
                    RuleBody::Pratt(p) => {
                        let pratt = &v.pratts[p as usize];
                        let operand = pratt.operand as usize;
                        // After an operand: operator `t` and what it takes.
                        let mut after_level = pratt.after.get(t).copied().unwrap_or(0);
                        for &(k, _, after) in pratt.contextual.iter() {
                            if usize::from(k) == t && after != 0 {
                                after_level = after;
                            }
                        }
                        scratch.iter_mut().for_each(|w| *w = 0);
                        if after_level != 0 {
                            let level = pratt.levels[usize::from(after_level) - 1];
                            let tail: Vec<u64> = if level.fixity == Fixity::Postfix {
                                let mut row = ops_after[p as usize].clone();
                                or_into(&mut row, follow.row(v.rule_row(r)));
                                row
                            } else {
                                rule_first(r)
                            };
                            match level.then {
                                Some(then) => {
                                    v.vfirst(then as usize, &mut scratch);
                                    if v.vnull(then as usize) {
                                        or_into(&mut scratch, &tail);
                                    }
                                }
                                None => or_into(&mut scratch, &tail),
                            }
                        }
                        or_into(&mut scratch, &own_rule);
                        changed |= ft.or(operand, &scratch);
                        for level in pratt.levels.iter() {
                            let Some(then) = level.then else { continue };
                            if level.fixity == Fixity::Postfix {
                                changed |= ft.or(then as usize, &scratch);
                            } else {
                                let rule_fa = fa.row(v.rule_row(r)).to_vec();
                                changed |= ft.or(then as usize, &rule_fa);
                            }
                        }
                    }
                }
                for &e in owned[r].iter().rev() {
                    let e = e as usize;
                    let own = ft.row(e).to_vec();
                    match a.exprs[e] {
                        Expr::Rule(callee) => changed |= ft.or(v.rule_row(callee as usize), &own),
                        Expr::Seq { start, len } => {
                            let items = a.children(start, len);
                            let seq_follow = follow.row(e).to_vec();
                            // Right to left: FA, J, TR, viability FIRST, and
                            // nullability of the suffix after each item.
                            let mut suffix_fa = vec![0u64; width];
                            let mut suffix_first = vec![0u64; width];
                            let (mut suffix_j, mut suffix_tr, mut suffix_null) =
                                (false, true, true);
                            for &item in items.iter().rev() {
                                let item = item as usize;
                                scratch.copy_from_slice(&suffix_fa);
                                if suffix_j {
                                    or_into(&mut scratch, &seq_follow);
                                }
                                if suffix_tr {
                                    or_into(&mut scratch, &own);
                                }
                                changed |= ft.or(item, &scratch);
                                // Extend the suffix with `item`.
                                let mut new_fa = fa.row(item).to_vec();
                                if j[item] {
                                    or_into(&mut new_fa, &suffix_first);
                                }
                                if tr[item] {
                                    or_into(&mut new_fa, &suffix_fa);
                                }
                                let new_j = (j[item] && suffix_null) || (tr[item] && suffix_j);
                                let mut new_first = vec![0u64; width];
                                v.vfirst(item, &mut new_first);
                                if v.vnull(item) {
                                    or_into(&mut new_first, &suffix_first);
                                }
                                suffix_fa = new_fa;
                                suffix_first = new_first;
                                suffix_j = new_j;
                                suffix_tr &= tr[item];
                                suffix_null &= v.vnull(item);
                            }
                        }
                        Expr::Choice { start, len } => {
                            for &item in a.children(start, len) {
                                changed |= ft.or(item as usize, &own);
                            }
                        }
                        Expr::Repeat { body, .. } => {
                            let body = body as usize;
                            let mut row = fa.row(body).to_vec();
                            if j[body] {
                                v.vfirst(body, &mut row);
                                or_into(&mut row, follow.row(e));
                            }
                            or_into(&mut row, &own);
                            changed |= ft.or(body, &row);
                        }
                        Expr::Optional(body)
                        | Expr::Label { body, .. }
                        | Expr::BackRef { body, .. } => {
                            changed |= ft.or(body as usize, &own);
                        }
                        _ => {}
                    }
                }
            }
            budget = budget.saturating_sub((rows * width) as u64);
            if budget == 0 {
                return Err("the grammar is too large for the overlap check's work budget");
            }
            if !changed {
                break;
            }
        }

        // ----- verdicts for the repetitions that can commit on `t` -----
        for c in &candidates {
            if reported[c.expr] || !c.tokens.contains(&t) {
                continue;
            }
            let mut inside = fa.row(c.body).to_vec();
            if j[c.body] {
                if c.repeat {
                    v.vfirst(c.body, &mut inside);
                }
                or_into(&mut inside, follow.row(c.expr));
            }
            let after = ft.row(c.expr);
            let witness = (0..bits).find(|&u| has(after, u) && !has(&inside, u));
            let Some(u) = witness else { continue };
            reported[c.expr] = true;
            let needs: Vec<String> = (0..bits)
                .filter(|&k| has(&inside, k))
                .take(4)
                .map(|k| format!("`{}`", show(kinds, k, eof)))
                .collect();
            let needs = if needs.is_empty() {
                String::from("nothing more")
            } else {
                needs.join(" or ")
            };
            let what = if c.repeat { "repetition" } else { "optional" };
            let (ts, us) = (show(kinds, t, eof), show(kinds, u, eof));
            let message = format!(
                "input `{ts} {us}` is rejected: this {what} commits on `{ts}` and then needs {needs}"
            );
            out.push(
                diag_lang::Diagnostic::new(
                    if deny {
                        diag_lang::Severity::Error
                    } else {
                        diag_lang::Severity::Warning
                    },
                    message,
                    diag_lang::Label::unlabelled(a.spans[c.expr]),
                )
                .with_help(format!(
                    "guard it so it commits only when its body follows, such as `(&({ts} !{us}) ...)`, or add `allow = [\"overlap\"]` to the rule if this is intended"
                ))
                .with_code(codes::OVERLAP),
            );
        }
    }
    Ok(out)
}

/// Whether `e` begins with a predicate (so the parser asks it before
/// committing).
pub(crate) fn begins_with_predicate(a: &Analysis<'_>, mut e: usize) -> bool {
    loop {
        match a.exprs[e] {
            Expr::And(_) | Expr::Not(_) => return true,
            Expr::Seq { start, len } if len > 0 => e = a.items[start as usize] as usize,
            Expr::Label { body, .. } => e = body as usize,
            _ => return false,
        }
    }
}

fn show(kinds: &Kinds, k: usize, eof: u16) -> String {
    if k == usize::from(eof) {
        return String::from("end of input");
    }
    String::from(kinds.name_at(k))
}

fn skipped(a: &mut Analysis<'_>, why: &str) {
    a.report.warning(
        codes::OVERLAP,
        syntax_lang::Span::empty(0),
        format!("the repetition-overlap check was not run: {why}"),
    );
}
