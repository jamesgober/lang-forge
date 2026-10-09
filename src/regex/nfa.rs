//! Thompson construction: a pattern becomes an NFA over the pattern's
//! *alphabet* — the classes of characters no part of the pattern tells apart
//! (see `alphabet` in `mod.rs`) — rather than over bytes, so a class as large
//! as `\p{XID_Continue}` is one transition, not a UTF-8 automaton of
//! hundreds of states.

use alloc::{collections::BTreeMap, vec, vec::Vec};

use super::parse::Hir;

/// The most NFA states the lexer may need. Patterns stay far below this; the
/// bound exists so a pattern like `(a{1000}){1000}` fails cleanly instead of
/// exhausting memory.
const STATE_LIMIT: usize = 1 << 20;

/// One NFA state.
#[derive(Clone, Debug)]
pub(crate) enum State {
    /// On a symbol in `lo..=hi`, move to `next`.
    Range { lo: u16, hi: u16, next: u32 },
    /// Move to any of these states without consuming input.
    Split(Vec<u32>),
    /// The pattern of token `0` has matched.
    Match(u32),
}

/// The combined NFA for every token.
#[derive(Debug)]
pub(crate) struct Nfa {
    pub(crate) states: Vec<State>,
    pub(crate) start: u32,
}

/// The NFA grew past [`STATE_LIMIT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TooLarge;

/// Each class of a pattern (as code-point ranges), as runs of the alphabet
/// symbols it contains.
pub(crate) type Symbols<'a> = BTreeMap<&'a [(u32, u32)], Vec<(u16, u16)>>;

/// Builds the NFA one token at a time.
pub(crate) struct Builder<'a> {
    states: Vec<State>,
    starts: Vec<u32>,
    /// Each class of the pattern, as runs of the symbols it contains.
    symbols: &'a Symbols<'a>,
}

impl<'a> Builder<'a> {
    pub(crate) fn new(symbols: &'a Symbols<'a>) -> Self {
        Self {
            states: Vec::new(),
            starts: Vec::new(),
            symbols,
        }
    }

    /// Adds a token whose pattern is `hir`; reaching its end yields
    /// `Match(token)`. Returns the id of that match state.
    pub(crate) fn add(&mut self, hir: &Hir, token: u32) -> Result<u32, TooLarge> {
        let accept = self.push(State::Match(token))?;
        let start = self.compile(hir, accept)?;
        self.starts.push(start);
        Ok(accept)
    }

    /// Joins the tokens under one start state.
    pub(crate) fn finish(mut self) -> Result<Nfa, TooLarge> {
        let starts = core::mem::take(&mut self.starts);
        let start = self.push(State::Split(starts))?;
        Ok(Nfa {
            states: self.states,
            start,
        })
    }

    fn push(&mut self, state: State) -> Result<u32, TooLarge> {
        if self.states.len() >= STATE_LIMIT {
            return Err(TooLarge);
        }
        self.states.push(state);
        Ok((self.states.len() - 1) as u32)
    }

    /// Compiles `hir` so that matching it leads to `next`; returns the
    /// fragment's entry state. Fragments are built back to front, so every
    /// state is created with its successor already known — only the loop of an
    /// unbounded repetition needs a patch.
    fn compile(&mut self, hir: &Hir, next: u32) -> Result<u32, TooLarge> {
        match hir {
            Hir::Empty => Ok(next),
            Hir::Class(ranges) => self.class(ranges, next),
            Hir::Concat(parts) => {
                let mut target = next;
                for part in parts.iter().rev() {
                    target = self.compile(part, target)?;
                }
                Ok(target)
            }
            Hir::Alt(branches) => {
                let mut entries = Vec::with_capacity(branches.len());
                for branch in branches {
                    entries.push(self.compile(branch, next)?);
                }
                self.push(State::Split(entries))
            }
            Hir::Repeat { hir, min, max } => {
                let mut target = match max {
                    None => {
                        // A loop: `entry` either runs the body back into
                        // itself or leaves.
                        let entry = self.push(State::Split(Vec::new()))?;
                        let body = self.compile(hir, entry)?;
                        self.states[entry as usize] = State::Split(vec![body, next]);
                        entry
                    }
                    Some(max) => {
                        // `max - min` optional copies, nested so each may
                        // stop early: (x(x(x)?)?)?
                        let mut target = next;
                        for _ in *min..*max {
                            let body = self.compile(hir, target)?;
                            target = self.push(State::Split(vec![body, next]))?;
                        }
                        target
                    }
                };
                for _ in 0..*min {
                    target = self.compile(hir, target)?;
                }
                Ok(target)
            }
        }
    }

    /// One character of the class `ranges`: a transition on each run of
    /// the symbols it contains.
    fn class(&mut self, ranges: &[(u32, u32)], next: u32) -> Result<u32, TooLarge> {
        let runs = self.symbols.get(ranges).cloned().unwrap_or_default();
        let mut entries = Vec::with_capacity(runs.len());
        for (lo, hi) in runs {
            entries.push(self.push(State::Range { lo, hi, next })?);
        }
        if entries.len() == 1 {
            Ok(entries[0])
        } else {
            self.push(State::Split(entries))
        }
    }
}
