//! What the scanner has learned about how runs of its automata end, so it
//! never runs one over the same text in the same state twice.
//!
//! A longest-match run stops at the dead state, which can lie far past the
//! token the scanner then takes. Run again from the next position, the
//! automaton would read the same text again, and a pattern like `-*b` over a
//! long run of dashes with no `b` would cost time quadratic in the run. So the
//! memo keeps, for each `(position, state)` a run passed through, the furthest
//! accepting end that continuing from there reaches. A later run of the same
//! automaton that arrives at a kept pair knows the rest of its answer and
//! stops. Every pair is kept at most once, so the total work of all runs is
//! linear in the input for a given set of patterns.
//!
//! The memo only matters for long runs. A run reads its first [`SLACK`] bytes
//! without it, and a run that ends within them is never kept: a later run
//! reads at most that much of it again, a bounded cost per run. Ordinary
//! tokens are short or look a character past their end, so on ordinary input
//! the memo is never touched.
//!
//! Keeping is lazy as well. A long run only notes where it started and how
//! far it read. Its pairs are worked out, by running it once more, when the
//! next long run starts more than [`SLACK`] bytes before the end of what it
//! read; otherwise it is dropped.

use alloc::{vec, vec::Vec};

use super::Regex;

/// How many bytes a run reads before it looks at the memo, and how far past
/// the next run's start a finished run must have read to be kept.
pub(crate) const SLACK: usize = 32;

/// One kept pair: continuing automaton `id` in row `row` from byte position
/// `pos` reaches an accepting end at `best` at the furthest, or no accepting
/// end at all when `best` is 0.
#[derive(Clone, Copy)]
struct Entry {
    id: usize,
    pos: usize,
    row: u32,
    best: usize,
}

/// A finished run whose pairs are not kept yet.
#[derive(Clone, Copy)]
struct Pending<'a> {
    regex: &'a Regex,
    /// Where it started.
    from: usize,
    /// The last position it passed in a state worth keeping.
    last: usize,
    /// Its furthest accepting end, or 0.
    best: usize,
}

/// The memo of one scan.
#[derive(Default)]
pub(crate) struct Memo<'a> {
    /// Open-addressing index into `entries`: 0 is empty, else index + 1.
    slots: Vec<u32>,
    entries: Vec<Entry>,
    /// The furthest position any kept entry has; 0 while there is none,
    /// which no scan position (always past a run's first character) reaches.
    reach: usize,
    pending: Vec<Pending<'a>>,
    /// Bytes the scans read, for tests.
    #[cfg(test)]
    pub(crate) steps: u64,
}

impl<'a> Memo<'a> {
    /// Prepares for a run starting at byte position `at` of `src` (read no
    /// further than `end`, as every run of this memo is): keeps the pairs of
    /// finished runs a run from here might reach again, and forgets what no
    /// run from here can.
    pub(crate) fn begin(&mut self, at: usize, src: &str, end: usize) {
        if at >= self.reach && self.reach != 0 {
            self.clear();
        }
        if self.pending.is_empty() {
            return;
        }
        let mut pending = core::mem::take(&mut self.pending);
        for p in &pending {
            // A run that read only a little past `at` is not worth keeping:
            // a run from here reads at most `SLACK` bytes of it again, which
            // keeps the total linear without the cost of keeping it.
            if p.last <= at + SLACK {
                continue;
            }
            let id = p.regex.id();
            p.regex.replay(src, p.from, end, p.last, |pos, row| {
                if pos > at {
                    let best = if p.best > pos { p.best } else { 0 };
                    self.insert(Entry { id, pos, row, best });
                }
            });
        }
        pending.clear();
        self.pending = pending;
    }

    /// Notes a finished run of `regex` from `from` that passed positions up
    /// to `last` and found its furthest accepting end at `best` (0 for none).
    #[inline]
    pub(crate) fn finish(&mut self, regex: &'a Regex, from: usize, last: usize, best: usize) {
        // Later runs start at `from` or after, and `begin` drops a run that
        // read no more than `SLACK` past their start.
        if last > from + SLACK {
            self.pending.push(Pending {
                regex,
                from,
                last,
                best,
            });
        }
    }

    /// Counts bytes a scan read, for tests.
    #[inline]
    pub(crate) fn count(&mut self, _bytes: usize) {
        #[cfg(test)]
        {
            self.steps += _bytes as u64;
        }
    }

    /// What continuing automaton `id` from `pos` in `row` reaches, if a
    /// kept pair says: `Some(furthest accepting end)`, 0 for none.
    #[inline]
    pub(crate) fn get(&self, id: usize, pos: usize, row: u32) -> Option<usize> {
        if pos > self.reach {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut at = hash(id, pos, row) & mask;
        loop {
            let slot = self.slots[at];
            if slot == 0 {
                return None;
            }
            let e = &self.entries[slot as usize - 1];
            if e.id == id && e.pos == pos && e.row == row {
                return Some(e.best);
            }
            at = (at + 1) & mask;
        }
    }

    fn insert(&mut self, entry: Entry) {
        if self.get(entry.id, entry.pos, entry.row).is_some() {
            return;
        }
        if (self.entries.len() + 1) * 2 > self.slots.len() {
            self.grow();
        }
        self.reach = self.reach.max(entry.pos);
        self.entries.push(entry);
        self.place(self.entries.len() - 1);
    }

    fn place(&mut self, index: usize) {
        let e = self.entries[index];
        let mask = self.slots.len() - 1;
        let mut at = hash(e.id, e.pos, e.row) & mask;
        while self.slots[at] != 0 {
            at = (at + 1) & mask;
        }
        self.slots[at] = index as u32 + 1;
    }

    fn grow(&mut self) {
        let size = (self.slots.len() * 2).max(64);
        self.slots = vec![0; size];
        for i in 0..self.entries.len() {
            self.place(i);
        }
    }

    /// Forgets every kept pair, keeping the index no larger than the entries
    /// it held needed, so clearing costs what keeping them did.
    fn clear(&mut self) {
        let size = (self.entries.len() * 2).next_power_of_two().max(64);
        if size < self.slots.len() {
            self.slots = vec![0; size];
        } else {
            self.slots.fill(0);
        }
        self.entries.clear();
        self.reach = 0;
    }
}

#[inline]
fn hash(id: usize, pos: usize, row: u32) -> usize {
    let h = (id as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(17)
        ^ (pos as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ u64::from(row).wrapping_mul(0x1656_67B1_9E37_79F9);
    (h ^ (h >> 29)) as usize
}
