//! The LSF2 regex dialect: patterns parsed into a HIR and compiled, one per
//! token class, into a minimized DFA that finds the longest match.
//!
//! Each class gets its own automaton rather than one automaton for every
//! token: in LSF2 a class's match is only a *candidate* — its conditions
//! (`not_followed_by`, `when_prev`, …), priority, and category decide among
//! the candidates at a position (LSF2 §9.13) — so the lexer needs each class's
//! longest match on its own. A 256-bit first-byte set keeps the classes that
//! cannot begin at a position from running at all.
//!
//! The automaton runs over the pattern's *alphabet*: the characters are
//! split into the classes no part of the pattern tells apart (for
//! `\$[\p{XID_Start}_]\p{XID_Continue}*`: `$`, the XID_Start characters and
//! `_`, the other XID_Continue characters, and everything else), and each
//! class is one symbol. A Unicode property is then a single transition, so
//! the automaton has a handful of states instead of the hundreds a byte-level
//! UTF-8 automaton needs, and it is built in microseconds. ASCII characters
//! find their symbol in a 128-entry table; others by a binary search over the
//! alphabet's ranges.

mod dfa;
pub(crate) mod memo;
mod nfa;
pub(crate) mod parse;
mod slicemap;

use alloc::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

pub(crate) use dfa::Dfa;
pub(crate) use memo::Memo;
pub(crate) use parse::{FORGE_WORK, Hir, Props, UNSUPPORTED_PROPERTY};

/// The most symbols one pattern's alphabet may have. Each symbol is a column
/// of the automaton and a bucket of every determinization step, so this also
/// bounds the cost of a pattern written as many distinct classes.
pub(crate) const MAX_SYMBOLS: usize = 256;

/// Why a pattern could not be compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompileError {
    /// The automaton (or its alphabet) is larger than allowed.
    TooLarge,
    /// The pattern's alphabet has more than [`MAX_SYMBOLS`] symbols.
    TooManyClasses,
    /// Building the automaton would take more steps than the budget left.
    TooCostly,
    /// The pattern matches the empty string.
    Empty,
}

/// A pattern's alphabet: the symbol of each character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Alphabet {
    /// The symbol of each ASCII character.
    ascii: Box<[u16; 128]>,
    /// The symbols of the characters from U+0080 on: sorted, disjoint,
    /// inclusive ranges; characters in no range are symbol 0.
    ranges: Box<[(u32, u32, u16)]>,
}

impl Alphabet {
    /// The symbol of `c`.
    #[inline]
    fn symbol(&self, c: char) -> u16 {
        let cp = c as u32;
        if cp < 128 {
            return self.ascii[cp as usize];
        }
        match self.ranges.binary_search_by(|&(lo, hi, _)| {
            if hi < cp {
                core::cmp::Ordering::Less
            } else if lo > cp {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        }) {
            Ok(at) => self.ranges[at].2,
            Err(_) => 0,
        }
    }

    /// The ASCII table and the ranges.
    #[cfg(test)]
    pub(crate) fn parts(&self) -> (&[u16; 128], &[(u32, u32, u16)]) {
        (&self.ascii, &self.ranges)
    }

    /// Rebuilds an alphabet from an image, or `None` if the ranges are not
    /// sorted, disjoint, above ASCII, or name a symbol of `symbols` or more.
    pub(crate) fn from_parts(
        ascii: Box<[u16; 128]>,
        ranges: Box<[(u32, u32, u16)]>,
        symbols: usize,
    ) -> Option<Self> {
        let ok = ascii.iter().all(|&s| usize::from(s) < symbols)
            && ranges.iter().all(|&(lo, hi, s)| {
                128 <= lo && lo <= hi && hi <= 0x10_FFFF && usize::from(s) < symbols
            })
            && ranges.windows(2).all(|w| w[0].1 < w[1].0);
        ok.then_some(Self { ascii, ranges })
    }
}

/// The runs of symbols each class of a pattern contains.
type ClassRuns<'h> = BTreeMap<&'h [(u32, u32)], Vec<(u16, u16)>>;

/// Computes the alphabet of `hir`: one symbol per distinct set of the
/// pattern's classes a character belongs to (symbol 0 for none), and, for
/// each class, the runs of symbols it contains.
fn alphabet<'h>(hir: &'h Hir, work: &mut u64) -> Result<(Alphabet, ClassRuns<'h>), CompileError> {
    // The distinct classes, numbered.
    let mut classes: BTreeMap<&[(u32, u32)], u32> = BTreeMap::new();
    let mut stack = vec![hir];
    while let Some(h) = stack.pop() {
        match h {
            Hir::Empty => {}
            Hir::Class(ranges) => {
                let next = classes.len() as u32;
                let _ = classes.entry(&**ranges).or_insert(next);
            }
            Hir::Concat(parts) | Hir::Alt(parts) => stack.extend(parts),
            Hir::Repeat { hir, .. } => stack.push(hir),
        }
    }
    let words = classes.len().div_ceil(64).max(1);
    // Sweep the code space: at each boundary a class starts or stops.
    let mut events: Vec<(u32, bool, u32)> = Vec::new();
    for (ranges, &id) in &classes {
        for &(lo, hi) in ranges.iter() {
            events.push((lo, true, id));
            events.push((hi + 1, false, id));
        }
    }
    events.sort_unstable();
    // Each boundary costs a look at the membership words.
    let cost = (events.len() as u64).saturating_mul(words as u64 + 1);
    *work = work.checked_sub(cost).ok_or(CompileError::TooCostly)?;
    let mut member = vec![0u64; words];
    let mut symbols: BTreeMap<Vec<u64>, u16> = BTreeMap::new();
    let _ = symbols.insert(vec![0u64; words], 0);
    let mut class_symbols: Vec<Vec<u16>> = vec![Vec::new(); classes.len()];
    let mut intervals: Vec<(u32, u32, u16)> = Vec::new();
    let mut at = 0;
    while at < events.len() {
        let point = events[at].0;
        while at < events.len() && events[at].0 == point {
            let (_, start, id) = events[at];
            let (w, b) = ((id / 64) as usize, id % 64);
            if start {
                member[w] |= 1 << b;
            } else {
                member[w] &= !(1 << b);
            }
            at += 1;
        }
        let Some(&(end, _, _)) = events.get(at) else {
            break;
        };
        // `point..end` has membership `member`.
        let next = symbols.len() as u16;
        let symbol = *symbols.entry(member.clone()).or_insert(next);
        if symbols.len() > MAX_SYMBOLS {
            return Err(CompileError::TooManyClasses);
        }
        if symbol != 0 {
            match intervals.last_mut() {
                Some(last) if last.1 + 1 == point && last.2 == symbol => last.1 = end - 1,
                _ => intervals.push((point, end - 1, symbol)),
            }
            for (w, word) in member.iter().enumerate() {
                let mut bits = *word;
                while bits != 0 {
                    let b = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    class_symbols[w * 64 + b].push(symbol);
                }
            }
        }
    }
    let mut ascii = Box::new([0u16; 128]);
    let mut ranges = Vec::new();
    for &(lo, hi, s) in &intervals {
        for cp in lo..=hi.min(127) {
            ascii[cp as usize] = s;
        }
        if hi >= 128 {
            ranges.push((lo.max(128), hi, s));
        }
    }
    let mut runs: ClassRuns<'_> = BTreeMap::new();
    for (ranges, &id) in &classes {
        let list = &mut class_symbols[id as usize];
        list.sort_unstable();
        list.dedup();
        let mut out: Vec<(u16, u16)> = Vec::new();
        for &s in list.iter() {
            match out.last_mut() {
                Some(last) if last.1 + 1 == s => last.1 = s,
                _ => out.push((s, s)),
            }
        }
        let _ = runs.insert(*ranges, out);
    }
    Ok((
        Alphabet {
            ascii,
            ranges: ranges.into(),
        },
        runs,
    ))
}

/// One compiled pattern.
#[derive(Clone, Debug)]
pub(crate) struct Regex {
    dfa: Dfa,
    alphabet: Alphabet,
    /// The bytes a match can begin with.
    first: [u64; 4],
}

impl Regex {
    /// Compiles `hir` into an automaton of at most `max_states` states,
    /// taking the steps that needs from `work`. A pattern that matches the
    /// empty string is refused.
    pub(crate) fn compile(
        hir: &Hir,
        max_states: usize,
        work: &mut u64,
    ) -> Result<Self, CompileError> {
        Self::build(hir, max_states, false, work)
    }

    /// Compiles a delimiter part's pattern, which may match the empty string
    /// (`[ \t]*`): see [`matches_empty`](Self::matches_empty).
    pub(crate) fn compile_part(
        hir: &Hir,
        max_states: usize,
        work: &mut u64,
    ) -> Result<Self, CompileError> {
        Self::build(hir, max_states, true, work)
    }

    fn build(
        hir: &Hir,
        max_states: usize,
        allow_empty: bool,
        work: &mut u64,
    ) -> Result<Self, CompileError> {
        let (alphabet, runs) = alphabet(hir, work)?;
        // The NFA's states count against `work` too; a builder that would
        // make more than it holds stops early.
        let mut builder = nfa::Builder::new(&runs).with_limit(*work);
        let accept = builder.add(hir, 0);
        let nfa = accept.and_then(|_| builder.finish());
        let (Ok(accept), Ok(nfa)) = (accept, nfa) else {
            return Err(if *work <= nfa::STATE_LIMIT as u64 {
                CompileError::TooCostly
            } else {
                CompileError::TooLarge
            });
        };
        *work -= nfa.states.len() as u64;
        // Subset construction may need a few times the minimal state count.
        let dfa = dfa::build(
            &nfa,
            &[0],
            &[0],
            &[accept],
            max_states.saturating_mul(4),
            allow_empty,
            work,
        )
        .map_err(|e| match e {
            dfa::DfaError::TooLarge => CompileError::TooLarge,
            dfa::DfaError::TooCostly => CompileError::TooCostly,
            // With one token, "never wins" can only mean "matches nothing
            // at all", which a parsed pattern cannot; refuse it the same.
            dfa::DfaError::EmptyMatch(_) | dfa::DfaError::Shadowed { .. } => CompileError::Empty,
        })?;
        if dfa.states() > max_states {
            return Err(CompileError::TooLarge);
        }
        Ok(Self::assemble(dfa, alphabet))
    }

    fn assemble(dfa: Dfa, alphabet: Alphabet) -> Self {
        let mut first = [0u64; 4];
        for b in 0..128u8 {
            if dfa.starts_with(alphabet.ascii[usize::from(b)]) {
                first[usize::from(b >> 6)] |= 1 << (b & 63);
            }
        }
        // Any non-ASCII symbol that can begin a match makes every UTF-8 lead
        // byte a possible start (a conservative filter).
        if alphabet.ranges.iter().any(|&(_, _, s)| dfa.starts_with(s)) || dfa.starts_with(0) {
            for b in 0xC2..=0xF4u8 {
                first[usize::from(b >> 6)] |= 1 << (b & 63);
            }
        }
        Self {
            dfa,
            alphabet,
            first,
        }
    }

    /// Rebuilds a pattern from a language image's parts, or `None` if they
    /// are malformed.
    pub(crate) fn from_parts(
        table: Box<[u32]>,
        columns: Box<[u16]>,
        start: u32,
        ascii: Box<[u16; 128]>,
        ranges: Box<[(u32, u32, u16)]>,
    ) -> Option<Self> {
        let symbols = columns.len();
        let dfa = Dfa::from_parts(table, columns, start, 1)?;
        let alphabet = Alphabet::from_parts(ascii, ranges, symbols)?;
        Some(Self::assemble(dfa, alphabet))
    }

    /// The automaton's table, columns, and start row.
    #[cfg(test)]
    pub(crate) fn parts(&self) -> (&[u32], &[u16], u32) {
        self.dfa.parts()
    }

    /// The alphabet.
    #[cfg(test)]
    pub(crate) fn alphabet(&self) -> &Alphabet {
        &self.alphabet
    }

    /// Whether the pattern matches the empty string (only delimiter parts
    /// may).
    pub(crate) fn matches_empty(&self) -> bool {
        self.dfa.matches_empty()
    }

    /// The length of the longest match at the start of `text`, or `None` if
    /// the pattern matches no prefix (a pattern that matches the empty string
    /// always matches).
    #[cfg(test)]
    pub(crate) fn match_len(&self, text: &str) -> Option<usize> {
        match self.longest_match(text) {
            0 if self.matches_empty() => Some(0),
            0 => None,
            n => Some(n),
        }
    }

    /// The number of states.
    pub(crate) fn states(&self) -> usize {
        self.dfa.states()
    }

    /// Whether a match can begin with `byte`.
    #[inline]
    pub(crate) fn can_start(&self, byte: u8) -> bool {
        (self.first[(byte >> 6) as usize] >> (byte & 63)) & 1 != 0
    }

    /// The length in bytes of the longest match at the start of `text`, or 0.
    #[inline]
    pub(crate) fn longest_match(&self, text: &str) -> usize {
        match text.as_bytes().first() {
            Some(&b) if self.can_start(b) || self.matches_empty() => {
                let mut chars = text.chars();
                self.dfa
                    .longest_match(|| {
                        let c = chars.next()?;
                        Some((self.alphabet.symbol(c), c.len_utf8()))
                    })
                    .0
            }
            _ => 0,
        }
    }
}

impl Regex {
    /// The length in bytes of the longest match at byte position `at` of
    /// `src`, reading no further than `end`, or 0. Runs share what they learn
    /// through `memo`, so scanning a whole text this way takes time linear in
    /// it, however far past a token the automaton has to look. Every call
    /// with one memo must pass the same `src` and `end`.
    ///
    /// The first [`memo::SLACK`] bytes of a run are read without the memo: a
    /// run that ends that soon is all an ordinary token needs, and reading
    /// that much again costs a bounded amount per run.
    #[inline]
    pub(crate) fn longest_match_at<'a>(
        &'a self,
        src: &str,
        at: usize,
        end: usize,
        memo: &mut Memo<'a>,
    ) -> usize {
        let text = &src[at..end];
        let Some(&lead) = text.as_bytes().first() else {
            return 0;
        };
        if !self.can_start(lead) && !self.matches_empty() {
            return 0;
        }
        let mut chars = text.chars();
        let mut row = self.dfa.start_row();
        let mut best: usize = 0;
        let mut pos = at;
        let near = at + memo::SLACK;
        loop {
            let step = chars
                .next()
                .and_then(|c| Some((c, self.dfa.step(row, self.alphabet.symbol(c))?)));
            let Some((c, (next, accepting, last))) = step else {
                memo.count(pos - at);
                return best.saturating_sub(at);
            };
            pos += c.len_utf8();
            row = next;
            if accepting {
                best = pos;
                if last {
                    memo.count(pos - at);
                    return best - at;
                }
            }
            if pos > near {
                break;
            }
        }
        // A long run: from here on, use and feed the memo.
        memo.count(pos - at);
        memo.begin(at, src, end);
        let id = self.id();
        let mut kept = pos;
        if let Some(further) = memo.get(id, pos, row) {
            best = best.max(further);
        } else {
            for c in chars {
                let Some((next, accepting, last)) = self.dfa.step(row, self.alphabet.symbol(c))
                else {
                    break;
                };
                memo.count(c.len_utf8());
                pos += c.len_utf8();
                row = next;
                if accepting {
                    best = pos;
                    if last {
                        break;
                    }
                }
                if let Some(further) = memo.get(id, pos, row) {
                    best = best.max(further);
                    break;
                }
                kept = pos;
            }
        }
        memo.finish(self, at, kept, best);
        best.saturating_sub(at)
    }

    /// [`match_len`](Self::match_len) at byte position `at` of `src`, with a
    /// memo: see [`longest_match_at`](Self::longest_match_at).
    #[inline]
    pub(crate) fn match_len_at<'a>(
        &'a self,
        src: &str,
        at: usize,
        end: usize,
        memo: &mut Memo<'a>,
    ) -> Option<usize> {
        match self.longest_match_at(src, at, end, memo) {
            0 if self.matches_empty() => Some(0),
            0 => None,
            n => Some(n),
        }
    }

    /// This pattern's identity in a memo: its address, which no other
    /// pattern alive at the same time shares.
    #[inline]
    fn id(&self) -> usize {
        core::ptr::from_ref(self) as usize
    }

    /// Runs a scan from `from` again, up to `last`, calling `visit` with each
    /// position and row it passes: how a memo keeps a finished run.
    fn replay(
        &self,
        src: &str,
        from: usize,
        end: usize,
        last: usize,
        mut visit: impl FnMut(usize, u32),
    ) {
        let mut row = self.dfa.start_row();
        let mut pos = from;
        for c in src[from..end].chars() {
            if pos >= last {
                break;
            }
            let Some((next, _, _)) = self.dfa.step(row, self.alphabet.symbol(c)) else {
                break;
            };
            pos += c.len_utf8();
            row = next;
            visit(pos, row);
        }
    }
}

/// A set of characters: one regex class, such as `\p{XID_Continue}` or `[\[(]`,
/// used where a condition looks at a single character (`not_followed_by`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CharSet {
    /// Sorted, disjoint, inclusive scalar-value ranges.
    ranges: Box<[(u32, u32)]>,
}

impl CharSet {
    /// The class `hir` stands for, if it is a single class (or a single
    /// character).
    pub(crate) fn from_hir(hir: &Hir) -> Option<Self> {
        match hir {
            Hir::Class(ranges) => Some(Self {
                ranges: ranges.clone(),
            }),
            _ => None,
        }
    }

    /// Rebuilds a set from an image, or `None` if the ranges are not sorted,
    /// disjoint scalar-value ranges.
    pub(crate) fn from_ranges(ranges: Box<[(u32, u32)]>) -> Option<Self> {
        let ordered = ranges.iter().all(|&(lo, hi)| lo <= hi && hi <= 0x10_FFFF)
            && ranges.windows(2).all(|w| w[0].1 < w[1].0);
        ordered.then_some(Self { ranges })
    }

    /// Whether `c` is in the set.
    #[inline]
    pub(crate) fn contains(&self, c: char) -> bool {
        let c = c as u32;
        self.ranges
            .binary_search_by(|&(lo, hi)| {
                if hi < c {
                    core::cmp::Ordering::Less
                } else if lo > c {
                    core::cmp::Ordering::Greater
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }
}

// ----- the language image -----

impl crate::image::Image for Regex {
    fn put(&self, w: &mut crate::image::Writer) {
        let (table, columns, start) = self.dfa.parts();
        Box::<[u32]>::from(table).put(w);
        Box::<[u16]>::from(columns).put(w);
        start.put(w);
        self.alphabet.ascii.put(w);
        self.alphabet.ranges.put(w);
    }
    fn get(r: &mut crate::image::Reader<'_>) -> crate::image::Res<Self> {
        let table = Box::<[u32]>::get(r)?;
        let columns = Box::<[u16]>::get(r)?;
        let start = u32::get(r)?;
        let ascii = Box::<[u16; 128]>::get(r)?;
        let ranges = Box::<[(u32, u32, u16)]>::get(r)?;
        Self::from_parts(table, columns, start, ascii, ranges)
            .ok_or(crate::image::ImageError::Invalid)
    }
}

impl crate::image::Image for CharSet {
    fn put(&self, w: &mut crate::image::Writer) {
        crate::image::Image::put(&self.ranges, w);
    }
    fn get(r: &mut crate::image::Reader<'_>) -> crate::image::Res<Self> {
        let ranges = <Box<[(u32, u32)]> as crate::image::Image>::get(r)?;
        Self::from_ranges(ranges).ok_or(crate::image::ImageError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn regex(p: &str) -> Regex {
        let hir = parse::parse(p, &mut Props::default()).unwrap();
        Regex::compile(&hir, 4096, &mut { u64::MAX }).unwrap()
    }

    /// A small deterministic generator for the property tests below.
    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state >> 33
    }

    #[test]
    fn test_longest_match_at_agrees_with_a_fresh_run() {
        let patterns = [
            "-*b",
            "(--)*b|-(--)*x",
            "a(ba)*c?",
            "[a-c]*d|ab",
            "-+",
            "(ab|a)(bc|c)*x?",
            "é+-?b",
        ];
        let regexes: Vec<Regex> = patterns.iter().map(|p| regex(p)).collect();
        let pieces = ["-", "b", "x", "a", "c", "d", "é", "ba", "bc"];
        let mut seed = 7;
        // Long repeated pieces, so runs read well past the next start, which
        // is when the memo keeps what they read.
        for _ in 0..150 {
            let mut text = alloc::string::String::new();
            for _ in 0..lcg(&mut seed) % 12 {
                let piece = pieces[(lcg(&mut seed) % pieces.len() as u64) as usize];
                let times = 1 + (lcg(&mut seed) % 40) as usize;
                text.push_str(&piece.repeat(times));
            }
            let starts: Vec<usize> = (0..text.len())
                .filter(|&i| text.is_char_boundary(i))
                .collect();
            let mut memo = Memo::default();
            // Mostly forward, as a scan goes, with a jump back now and then.
            let mut k = 0;
            while k < starts.len() {
                let at = starts[k];
                for r in &regexes {
                    let fresh = r.longest_match(&text[at..]);
                    assert_eq!(
                        r.longest_match_at(&text, at, text.len(), &mut memo),
                        fresh,
                        "{text:?} at {at}"
                    );
                }
                k = if lcg(&mut seed) % 7 == 0 {
                    k.saturating_sub(3)
                } else {
                    k + 1 + (lcg(&mut seed) % 2) as usize
                };
            }
        }
    }

    #[test]
    fn test_longest_match_at_reads_each_character_a_bounded_number_of_times() {
        // `-*b` over dashes with no `b` runs to the end from every start; the
        // memo makes the later runs stop soon. The second pattern's state
        // depends on the parity of the dashes read, so its first two runs
        // read to the end. A scan without memory reads n / 2 bytes a dash.
        let dashes = regex("-*b");
        let parity = regex("(--)*b|-(--)*x");
        for n in [1_000usize, 4_000, 16_000] {
            let text = "-".repeat(n);
            let mut memo = Memo::default();
            for at in 0..n {
                assert_eq!(dashes.longest_match_at(&text, at, n, &mut memo), 0);
                assert_eq!(parity.longest_match_at(&text, at, n, &mut memo), 0);
            }
            // Each run reads up to `SLACK` bytes before it looks at the memo.
            let per_dash = 2 * (memo::SLACK as u64 + 8);
            assert!(
                memo.steps <= per_dash * n as u64,
                "n = {n}: {} bytes read",
                memo.steps
            );
        }
    }

    #[test]
    fn test_longest_match_and_first_bytes() {
        let r = regex(r"\$[\p{XID_Start}_]\p{XID_Continue}*");
        assert_eq!(r.longest_match("$name rest"), 5);
        assert_eq!(r.longest_match("$ünïcode!"), "$ünïcode".len());
        assert_eq!(r.longest_match("$1"), 0);
        assert_eq!(r.longest_match("x"), 0);
        assert_eq!(r.longest_match(""), 0);
        assert!(r.can_start(b'$'));
        assert!(!r.can_start(b'a'));
        assert!(r.states() < 8, "{} states", r.states());
    }

    #[test]
    fn test_backs_off_to_last_accepting_position() {
        let r = regex(r"\?>(\r?\n)?");
        assert_eq!(r.longest_match("?>\r\nx"), 4);
        assert_eq!(r.longest_match("?>\rx"), 2);
        let cast = regex(r"\([ \t]*int[ \t]*\)");
        assert_eq!(cast.longest_match("( int )x"), 7);
        assert_eq!(cast.longest_match("(integer)"), 0);
        let comment = regex(r"/\*([^*]|\*+[^*/])*\*+/");
        assert_eq!(comment.longest_match("/* c */x"), 7);
        assert_eq!(comment.longest_match("/* open"), 0);
    }

    #[test]
    fn test_unicode_classes_match_whole_characters() {
        let greek = regex("[α-ω]+");
        assert_eq!(greek.longest_match("αβγ€δ"), "αβγ".len());
        assert!(!greek.can_start(b'a'));
        assert!(greek.can_start(0xCE));
        let other = regex("[^α-ω]");
        assert_eq!(other.longest_match("€x"), "€".len());
        assert_eq!(other.longest_match("😀"), "😀".len());
    }

    #[test]
    fn test_parts_may_match_empty() {
        let hir = parse::parse("[ \t]*", &mut Props::default()).unwrap();
        let part = Regex::compile_part(&hir, 4096, &mut { u64::MAX }).unwrap();
        assert!(part.matches_empty());
        assert_eq!(part.match_len("x"), Some(0));
        assert_eq!(part.match_len("  \tx"), Some(3));
        assert_eq!(part.match_len(""), Some(0));
        let ident = regex("[a-z]+");
        assert!(!ident.matches_empty());
        assert_eq!(ident.match_len("1"), None);
    }

    #[test]
    fn test_compile_errors() {
        let hir = parse::parse("a*", &mut Props::default()).unwrap();
        assert_eq!(
            Regex::compile(&hir, 4096, &mut { u64::MAX }).unwrap_err(),
            CompileError::Empty
        );
        let hir = parse::parse("(a|b)*a(a|b){12}", &mut Props::default()).unwrap();
        assert_eq!(
            Regex::compile(&hir, 4096, &mut { u64::MAX }).unwrap_err(),
            CompileError::TooLarge
        );
    }

    #[test]
    fn test_parts_round_trip_and_validation() {
        let r = regex("[a-z]+[0-9é]");
        let (table, columns, start) = r.parts();
        let (ascii, ranges) = r.alphabet().parts();
        let rebuild = |table: Box<[u32]>, start: u32, ascii: Box<[u16; 128]>| {
            Regex::from_parts(table, columns.into(), start, ascii, ranges.into())
        };
        let again = rebuild(table.into(), start, Box::new(*ascii)).unwrap();
        assert_eq!(again.longest_match("abc1x"), 4);
        assert_eq!(again.longest_match("abé"), "abé".len());
        let mut bad: Box<[u32]> = table.into();
        let last = bad.len() - 1;
        bad[last] = 0x3FFF_FFF0;
        assert!(rebuild(bad, start, Box::new(*ascii)).is_none());
        assert!(rebuild(table.into(), start + 1, Box::new(*ascii)).is_none());
        let mut bad_ascii = Box::new(*ascii);
        bad_ascii[0] = 9999;
        assert!(rebuild(table.into(), start, bad_ascii).is_none());
    }

    #[test]
    fn test_char_set() {
        let hir = parse::parse(r"\p{XID_Continue}", &mut Props::default()).unwrap();
        let set = CharSet::from_hir(&hir).unwrap();
        assert!(set.contains('a'));
        assert!(set.contains('_'));
        assert!(set.contains('é'));
        assert!(!set.contains(' '));
        let hir = parse::parse("ab", &mut Props::default()).unwrap();
        assert!(CharSet::from_hir(&hir).is_none());
        assert!(CharSet::from_ranges(Box::new([(5, 3)])).is_none());
        assert!(CharSet::from_ranges(Box::new([(1, 3), (3, 4)])).is_none());
        assert!(CharSet::from_ranges(Box::new([(1, 3), (5, 6)])).is_some());
    }

    #[test]
    fn test_xid_patterns_compile_fast() {
        let started = std::time::Instant::now();
        let r = regex(
            r"\$[\p{XID_Start}_]\p{XID_Continue}*(\[(-?[0-9]+|[\p{XID_Start}_]\p{XID_Continue}*|\$[\p{XID_Start}_]\p{XID_Continue}*)\]|->[\p{XID_Start}_]\p{XID_Continue}*)?",
        );
        assert_eq!(r.longest_match("$a[0]x"), 5);
        assert_eq!(r.longest_match("$a[key]"), 7);
        assert_eq!(r.longest_match("$a[$i]"), 6);
        assert_eq!(r.longest_match("$o->p!"), 5);
        assert_eq!(r.longest_match("$o->"), 2);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}
