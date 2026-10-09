//! Subset construction, Hopcroft minimization, and the scanning table.
//!
//! The NFA's symbol ranges first partition the alphabet into equivalence
//! classes — symbols no transition tells apart share a column — which keeps
//! the table narrow. Subset construction then determinizes over those classes,
//! Hopcroft's algorithm merges states no input can distinguish, and the result
//! is laid out as one flat `u32` table:
//!
//! ```text
//! row r:  [ accept | next(class 0) | next(class 1) | ... ]
//! ```
//!
//! Row offsets are pre-multiplied, so a transition is one add and one load.
//! A transition into an accepting row also carries [`ACCEPTING`] in its high
//! bit, so the scan learns that a token ended without touching the target
//! row's accept word; it remembers only the last accepting row, and reads that
//! row's accept word once, when the scan stops. A transition into a row with
//! no live exits carries [`FINAL`] as well, so a token like `,` or `{` ends
//! without a wasted step. Row 0 is the dead state: every transition into it is
//! 0, and reaching it ends a scan.

use alloc::{boxed::Box, vec, vec::Vec};

use super::nfa::{Nfa, State};
use super::slicemap::SliceMap;

/// The most table cells (states × columns) one automaton may use while it is
/// built: 16 MiB of `u32`. Patterns whose determinization explodes, like
/// `(a|b)*a(a|b){20}`, hit it (or the caller's state limit) and fail cleanly.
const CELL_LIMIT: usize = 1 << 22;

/// The most NFA-state ids the subset keys may hold in total (64 MiB), which
/// bounds construction memory before the table limit is reached.
const KEY_LIMIT: usize = 1 << 24;

/// Set on a transition whose target row accepts a token.
const ACCEPTING: u32 = 1 << 31;
/// Set on a transition whose target row has no way out but the dead state,
/// so the token it ends cannot grow and the scan can stop at once.
const FINAL: u32 = 1 << 30;
/// The bits of a transition that hold the target row's offset.
const OFFSET: u32 = FINAL - 1;

/// Why the automaton could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DfaError {
    /// The automaton grew past [`CELL_LIMIT`].
    TooLarge,
    /// Token `0` matches the empty string.
    EmptyMatch(u32),
    /// Token `token` never wins a match; `by` wins every string it matches
    /// at least once.
    Shadowed { token: u32, by: u32 },
}

/// A minimized DFA laid out for scanning.
#[derive(Clone, Debug)]
pub(crate) struct Dfa {
    /// Rows of `stride` words; see the module docs.
    table: Box<[u32]>,
    /// Each symbol's column within a row (its class plus one).
    columns: Box<[u16]>,
    /// The start row's offset.
    start: u32,
}

impl Dfa {
    /// The longest match of the symbols `next` yields one at a time with
    /// their byte lengths, as `(length in bytes, token)`; the length is 0
    /// when nothing matches.
    #[inline]
    pub(crate) fn longest_match(
        &self,
        mut next_symbol: impl FnMut() -> Option<(u16, usize)>,
    ) -> (usize, u32) {
        let table = &*self.table;
        let mut row = self.start as usize;
        let (mut best_len, mut best_row) = (0, 0);
        let mut at = 0;
        while let Some((symbol, len)) = next_symbol() {
            let Some(&column) = self.columns.get(usize::from(symbol)) else {
                break;
            };
            let next = table[row + column as usize];
            if next == 0 {
                break;
            }
            at += len;
            row = (next & OFFSET) as usize;
            if next & ACCEPTING != 0 {
                best_len = at;
                best_row = row;
                if next & FINAL != 0 {
                    break;
                }
            }
        }
        if best_len == 0 {
            return (0, 0);
        }
        (best_len, table[best_row].wrapping_sub(1))
    }

    /// Whether a transition leaves the start state on `symbol`.
    pub(crate) fn starts_with(&self, symbol: u16) -> bool {
        self.columns
            .get(usize::from(symbol))
            .is_some_and(|&c| self.table[self.start as usize + c as usize] != 0)
    }

    /// Whether the empty string matches (the start row accepts).
    pub(crate) fn matches_empty(&self) -> bool {
        self.table[self.start as usize] != 0
    }

    /// The number of rows, the dead state included.
    pub(crate) fn states(&self) -> usize {
        self.table.len() / self.stride()
    }

    fn stride(&self) -> usize {
        self.columns.iter().copied().max().unwrap_or(0) as usize + 1
    }

    /// The table, columns, and start row, for the language image.
    pub(crate) fn parts(&self) -> (&[u32], &[u16], u32) {
        (&self.table, &self.columns, self.start)
    }

    /// Rebuilds an automaton from an image's parts, or `None` if they could
    /// make a scan read outside the table or report a token other than
    /// `0..tokens`.
    pub(crate) fn from_parts(
        table: Box<[u32]>,
        columns: Box<[u16]>,
        start: u32,
        tokens: u32,
    ) -> Option<Self> {
        let stride = columns.iter().copied().max().unwrap_or(0) as usize + 1;
        if columns.contains(&0) || table.is_empty() || table.len() % stride != 0 {
            return None;
        }
        let start_ok = (start as usize) % stride == 0 && (start as usize) < table.len();
        let rows_ok = table.chunks(stride).all(|row| {
            row[0] <= tokens
                && row[1..].iter().all(|&next| {
                    let offset = (next & OFFSET) as usize;
                    next == 0 || (offset % stride == 0 && offset < table.len())
                })
        });
        (start_ok && rows_ok).then_some(Self {
            table,
            columns,
            start,
        })
    }
}

/// Builds the scanning DFA for `nfa`.
///
/// `rank[t]` orders tokens for equal-length matches (lower wins); `order[t]`
/// is the declaration order, used only to pick which error to report first;
/// `matches[t]` is the NFA state that accepts token `t`.
pub(crate) fn build(
    nfa: &Nfa,
    rank: &[u32],
    order: &[u32],
    matches: &[u32],
    max_subsets: usize,
    allow_empty: bool,
) -> Result<Dfa, DfaError> {
    let (columns, classes) = symbol_classes(nfa);
    let subsets = Subsets::build(nfa, classes, &columns, rank, max_subsets)?;

    // A token in the start state's closure matches the empty string, which
    // would let the scanner stall without consuming input.
    let first_empty = subsets
        .sets
        .get(subsets.start as usize)
        .iter()
        .filter_map(|&s| match nfa.states[s as usize] {
            State::Match(t) => Some(t),
            _ => None,
        })
        .min_by_key(|&t| order[t as usize]);
    if let Some(token) = first_empty.filter(|_| !allow_empty) {
        return Err(DfaError::EmptyMatch(token));
    }

    // Every token must win somewhere, or declaring it was a mistake.
    let mut produced = vec![false; rank.len()];
    for &accept in &subsets.accept {
        if accept != 0 {
            produced[(accept - 1) as usize] = true;
        }
    }
    let shadowed = (0..rank.len() as u32)
        .filter(|&t| !produced[t as usize])
        .min_by_key(|&t| order[t as usize]);
    if let Some(token) = shadowed {
        let state = matches[token as usize];
        let by = (0..subsets.sets.len())
            .find(|&id| subsets.sets.get(id).binary_search(&state).is_ok())
            .map_or(token, |id| subsets.accept[id].saturating_sub(1));
        return Err(DfaError::Shadowed { token, by });
    }

    Ok(minimize(&subsets, classes, columns))
}

/// Partitions the alphabet into classes no NFA transition distinguishes.
/// Returns each symbol's column (class + 1, leaving column 0 for the accept
/// word) and the number of classes.
fn symbol_classes(nfa: &Nfa) -> (Box<[u16]>, usize) {
    let symbols = nfa
        .states
        .iter()
        .filter_map(|s| match *s {
            State::Range { hi, .. } => Some(usize::from(hi) + 1),
            _ => None,
        })
        .max()
        .unwrap_or(1);
    let mut boundary = vec![false; symbols + 1];
    for state in &nfa.states {
        if let State::Range { lo, hi, .. } = *state {
            boundary[usize::from(lo)] = true;
            boundary[usize::from(hi) + 1] = true;
        }
    }
    let mut columns = vec![0u16; symbols];
    let mut class = 0u16;
    for (symbol, column) in columns.iter_mut().enumerate() {
        if symbol > 0 && boundary[symbol] {
            class += 1;
        }
        *column = class + 1;
    }
    (columns.into(), class as usize + 1)
}

/// The determinized (not yet minimized) automaton.
struct Subsets {
    /// Each DFA state's set of important NFA states (ranges and matches).
    sets: SliceMap,
    /// `state * classes + class` → state. State 0 is the empty set.
    trans: Vec<u32>,
    /// Each state's winning token plus one, or 0.
    accept: Vec<u32>,
    start: u32,
}

impl Subsets {
    fn build(
        nfa: &Nfa,
        classes: usize,
        columns: &[u16],
        rank: &[u32],
        max_subsets: usize,
    ) -> Result<Self, DfaError> {
        let mut closure = Closure::new(nfa.states.len());
        let mut sets = SliceMap::new();
        let _ = sets.insert(&[]);
        let start_set = closure.of(nfa, &[nfa.start]);
        let (start, _) = sets.insert(start_set);

        let mut trans: Vec<u32> = Vec::new();
        let mut accept: Vec<u32> = Vec::new();
        let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); classes];
        let mut key: Vec<u32> = Vec::new();
        let mut id = 0;
        while id < sets.len() {
            key.clear();
            key.extend_from_slice(sets.get(id));

            let mut best: Option<u32> = None;
            for &s in &key {
                match nfa.states[s as usize] {
                    State::Range { lo, hi, next } => {
                        let first = columns[lo as usize] as usize - 1;
                        let last = columns[hi as usize] as usize - 1;
                        for bucket in &mut buckets[first..=last] {
                            bucket.push(next);
                        }
                    }
                    State::Match(t) => {
                        if best.is_none_or(|b| rank[t as usize] < rank[b as usize]) {
                            best = Some(t);
                        }
                    }
                    State::Split(_) => {}
                }
            }
            accept.push(best.map_or(0, |t| t + 1));

            for bucket in &mut buckets {
                let target = if bucket.is_empty() {
                    0
                } else {
                    let set = closure.of(nfa, bucket);
                    bucket.clear();
                    sets.insert(set).0
                };
                trans.push(target);
            }
            if sets.len() * (classes + 1) > CELL_LIMIT
                || sets.words() > KEY_LIMIT
                || sets.len() > max_subsets
            {
                return Err(DfaError::TooLarge);
            }
            id += 1;
        }
        Ok(Self {
            sets,
            trans,
            accept,
            start,
        })
    }
}

/// Epsilon closure with a generation-stamped visited set, so each call costs
/// only the states it reaches.
struct Closure {
    stamp: Vec<u32>,
    generation: u32,
    stack: Vec<u32>,
    out: Vec<u32>,
}

impl Closure {
    fn new(states: usize) -> Self {
        Self {
            stamp: vec![0; states],
            generation: 0,
            stack: Vec::new(),
            out: Vec::new(),
        }
    }

    /// The sorted important states reachable from `seeds` without input.
    fn of(&mut self, nfa: &Nfa, seeds: &[u32]) -> &[u32] {
        self.generation += 1;
        if self.generation == u32::MAX {
            self.stamp.fill(0);
            self.generation = 1;
        }
        self.out.clear();
        self.stack.extend_from_slice(seeds);
        while let Some(s) = self.stack.pop() {
            let slot = &mut self.stamp[s as usize];
            if *slot == self.generation {
                continue;
            }
            *slot = self.generation;
            match &nfa.states[s as usize] {
                State::Split(targets) => self.stack.extend_from_slice(targets),
                State::Range { .. } | State::Match(_) => self.out.push(s),
            }
        }
        self.out.sort_unstable();
        &self.out
    }
}

/// Merges equivalent states (Hopcroft) and lays out the scanning table.
fn minimize(dfa: &Subsets, classes: usize, columns: Box<[u16]>) -> Dfa {
    let n = dfa.accept.len();
    let block = hopcroft(dfa, classes);

    // Number the blocks: the dead state's block is row 0, the rest in
    // breadth-first order from the start, so rows a scan visits together sit
    // near each other.
    let blocks = block.iter().copied().max().map_or(0, |m| m as usize + 1);
    let mut row_of = vec![u32::MAX; blocks];
    let mut rep = Vec::with_capacity(blocks);
    row_of[block[0] as usize] = 0;
    rep.push(0usize);
    let start_block = block[dfa.start as usize] as usize;
    if row_of[start_block] == u32::MAX {
        row_of[start_block] = rep.len() as u32;
        rep.push(dfa.start as usize);
    }
    let mut head = 0;
    while head < rep.len() {
        let state = rep[head];
        head += 1;
        for &target in &dfa.trans[state * classes..(state + 1) * classes] {
            let b = block[target as usize] as usize;
            if row_of[b] == u32::MAX {
                row_of[b] = rep.len() as u32;
                rep.push(target as usize);
            }
        }
    }
    debug_assert!(rep.len() <= n);

    // Every state of a block accepts the same token and leads to the same
    // blocks, so any one state decides both flags.
    let mut accepting = vec![false; blocks];
    let mut exits = vec![false; blocks];
    for (state, &b) in block.iter().enumerate() {
        accepting[b as usize] = dfa.accept[state] != 0;
        exits[b as usize] = dfa.trans[state * classes..(state + 1) * classes]
            .iter()
            .any(|&t| block[t as usize] != block[0]);
    }

    let stride = classes + 1;
    let mut table = vec![0u32; rep.len() * stride];
    for (row, &state) in rep.iter().enumerate() {
        let base = row * stride;
        table[base] = dfa.accept[state];
        for (class, &target) in dfa.trans[state * classes..(state + 1) * classes]
            .iter()
            .enumerate()
        {
            let target = block[target as usize] as usize;
            let mut flags = 0;
            if accepting[target] {
                flags |= ACCEPTING;
                if !exits[target] {
                    flags |= FINAL;
                }
            }
            table[base + 1 + class] = (row_of[target] * stride as u32) | flags;
        }
    }
    Dfa {
        table: table.into_boxed_slice(),
        columns,
        start: row_of[start_block] * stride as u32,
    }
}

/// Hopcroft's partition refinement: the coarsest partition of the states that
/// respects acceptance and transitions. Returns each state's block.
fn hopcroft(dfa: &Subsets, classes: usize) -> Vec<u32> {
    let n = dfa.accept.len();

    // Predecessors by class, as CSR: preds[(c, t)] lists every s with
    // trans(s, c) = t.
    let mut pred_start = vec![0u32; classes * n + 1];
    for s in 0..n {
        for c in 0..classes {
            let t = dfa.trans[s * classes + c] as usize;
            pred_start[c * n + t + 1] += 1;
        }
    }
    for i in 1..pred_start.len() {
        pred_start[i] += pred_start[i - 1];
    }
    let mut fill = pred_start.clone();
    let mut preds = vec![0u32; classes * n];
    for s in 0..n {
        for c in 0..classes {
            let t = dfa.trans[s * classes + c] as usize;
            let slot = &mut fill[c * n + t];
            preds[*slot as usize] = s as u32;
            *slot += 1;
        }
    }

    let mut partition = Partition::by_key(&dfa.accept);
    let mut work: Vec<u32> = (0..partition.blocks() as u32).collect();
    let mut in_work = vec![true; partition.blocks()];
    let mut splitter: Vec<u32> = Vec::new();
    let mut splits: Vec<(u32, u32)> = Vec::new();
    while let Some(a) = work.pop() {
        in_work[a as usize] = false;
        splitter.clear();
        splitter.extend_from_slice(partition.members(a));
        for c in 0..classes {
            for &t in &splitter {
                let at = c * n + t as usize;
                for &s in &preds[pred_start[at] as usize..pred_start[at + 1] as usize] {
                    partition.mark(s);
                }
            }
            partition.split(&mut splits);
            for (old, new) in splits.drain(..) {
                in_work.push(false);
                let pick = if in_work[old as usize] || partition.size(new) <= partition.size(old) {
                    new
                } else {
                    old
                };
                if !in_work[pick as usize] {
                    in_work[pick as usize] = true;
                    work.push(pick);
                }
            }
        }
    }
    partition.block
}

/// A refinable partition: blocks are contiguous runs of `elems`, and marking
/// an element swaps it into the marked prefix of its block, so a split costs
/// only the marked elements.
struct Partition {
    elems: Vec<u32>,
    loc: Vec<u32>,
    block: Vec<u32>,
    start: Vec<u32>,
    end: Vec<u32>,
    /// The end of each block's marked prefix.
    mid: Vec<u32>,
    touched: Vec<u32>,
}

impl Partition {
    /// One block per distinct key value.
    fn by_key(keys: &[u32]) -> Self {
        let n = keys.len();
        let mut elems: Vec<u32> = (0..n as u32).collect();
        elems.sort_by_key(|&s| (keys[s as usize], s));
        let mut loc = vec![0u32; n];
        let mut block = vec![0u32; n];
        let (mut start, mut end) = (Vec::new(), Vec::new());
        for (i, &s) in elems.iter().enumerate() {
            loc[s as usize] = i as u32;
            if i == 0 || keys[s as usize] != keys[elems[i - 1] as usize] {
                if i > 0 {
                    end.push(i as u32);
                }
                start.push(i as u32);
            }
            block[s as usize] = (start.len() - 1) as u32;
        }
        end.push(n as u32);
        let mid = start.clone();
        Self {
            elems,
            loc,
            block,
            start,
            end,
            mid,
            touched: Vec::new(),
        }
    }

    fn blocks(&self) -> usize {
        self.start.len()
    }

    fn size(&self, b: u32) -> u32 {
        self.end[b as usize] - self.start[b as usize]
    }

    fn members(&self, b: u32) -> &[u32] {
        &self.elems[self.start[b as usize] as usize..self.end[b as usize] as usize]
    }

    fn mark(&mut self, s: u32) {
        let b = self.block[s as usize] as usize;
        let i = self.loc[s as usize];
        let m = self.mid[b];
        if i < m {
            return;
        }
        let other = self.elems[m as usize];
        self.elems[m as usize] = s;
        self.elems[i as usize] = other;
        self.loc[s as usize] = m;
        self.loc[other as usize] = i;
        if m == self.start[b] {
            self.touched.push(b as u32);
        }
        self.mid[b] = m + 1;
    }

    /// Splits every partly marked block, moving the marked part to a new
    /// block, and clears all marks. Appends `(old, new)` pairs to `out`.
    fn split(&mut self, out: &mut Vec<(u32, u32)>) {
        let mut touched = core::mem::take(&mut self.touched);
        for &b in &touched {
            let b = b as usize;
            let (start, mid, end) = (self.start[b], self.mid[b], self.end[b]);
            if mid == end {
                self.mid[b] = start;
                continue;
            }
            let new = self.start.len() as u32;
            self.start.push(start);
            self.end.push(mid);
            self.mid.push(start);
            for &s in &self.elems[start as usize..mid as usize] {
                self.block[s as usize] = new;
            }
            self.start[b] = mid;
            self.mid[b] = mid;
            out.push((b as u32, new));
        }
        touched.clear();
        self.touched = touched;
    }
}
