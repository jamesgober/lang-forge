//! Token patterns: the LSF2 regex dialect (LSF2 §9.12) parsed into a small HIR.
//!
//! The dialect is grammar-lang's pattern syntax — Rust's `regex` crate minus
//! what has no meaning for longest-match tokens — plus `\p{XID_Start}`,
//! `\p{XID_Continue}` and their complements `\P{...}`, inside or outside
//! classes. Anchors, lazy repetition, flags, look-around, and back-references
//! are errors rather than silently ignored; so is any other Unicode property,
//! until unicode-lang ships general-category tables.
//!
//! Character semantics are full Unicode: a class is a set of scalar values,
//! and the NFA compiler lowers it to UTF-8 byte ranges. The shorthand classes
//! `\d`, `\w`, and `\s` are ASCII, which is what token definitions expect.
//!
//! Ported from grammar-lang 1.0 (same author and licence): grammar-lang's
//! engine is private to it and compiles every token into one automaton, while
//! lang-forge needs one automaton per token class (conditions and priorities
//! decide among candidates), the XID properties, and access to the tables for
//! the language image.

use alloc::{boxed::Box, vec, vec::Vec};

/// The largest count a `{n,m}` repetition may name.
const REPEAT_LIMIT: u32 = 1000;

/// The deepest group nesting a pattern may use. Parsing and NFA compilation
/// recurse once per level, so the bound keeps both off the edge of the stack.
const DEPTH_LIMIT: u32 = 64;

/// The largest Unicode scalar value.
const MAX_CHAR: u32 = 0x10_FFFF;

/// A parsed pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hir {
    /// Matches the empty string.
    Empty,
    /// Matches one scalar value in any of the sorted, disjoint, inclusive
    /// ranges. Never empty, never containing a surrogate.
    Class(Box<[(u32, u32)]>),
    /// Matches each part in turn.
    Concat(Vec<Hir>),
    /// Matches any one branch.
    Alt(Vec<Hir>),
    /// Matches the inner pattern between `min` and `max` times (`None` is
    /// unbounded).
    Repeat {
        hir: Box<Hir>,
        min: u32,
        max: Option<u32>,
    },
}

/// Why a pattern failed to parse, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PatternError {
    /// Byte offset into the pattern.
    pub(crate) offset: usize,
    /// What is wrong, as a lowercase phrase.
    pub(crate) reason: &'static str,
}

/// The reason given for a Unicode property the dialect does not support.
pub(crate) const UNSUPPORTED_PROPERTY: &str =
    "this Unicode property is not available; supported: XID_Start, XID_Continue";

/// The Unicode identifier properties, as ranges, computed when first needed,
/// and the patterns compiled so far in one forge.
#[derive(Default)]
pub(crate) struct Props {
    xid_start: Option<Box<[(u32, u32)]>>,
    xid_continue: Option<Box<[(u32, u32)]>>,
    compiled: alloc::collections::BTreeMap<(alloc::string::String, bool), super::Regex>,
}

impl Props {
    /// A pattern compiled earlier in this forge.
    pub(crate) fn cached(&self, pattern: &str, part: bool) -> Option<&super::Regex> {
        self.compiled
            .get(&(alloc::string::String::from(pattern), part))
    }

    /// Remembers a compiled pattern.
    pub(crate) fn cache(&mut self, pattern: &str, part: bool, regex: &super::Regex) {
        let _ = self
            .compiled
            .insert((alloc::string::String::from(pattern), part), regex.clone());
    }

    /// The ranges of `XID_Start` (`start`) or `XID_Continue`.
    pub(crate) fn xid(&mut self, start: bool) -> &[(u32, u32)] {
        let slot = if start {
            &mut self.xid_start
        } else {
            &mut self.xid_continue
        };
        slot.get_or_insert_with(|| xid_ranges(start))
    }
}

/// Every scalar value with `XID_Start` (`start`) or `XID_Continue`, as
/// sorted, merged ranges. unicode-lang's property tables are private, so the
/// ranges are recovered by asking about every scalar value once (a few
/// milliseconds); with `std` that happens once per process.
fn xid_ranges(start: bool) -> Box<[(u32, u32)]> {
    let holds: fn(char) -> bool = if start {
        unicode_lang::is_xid_start
    } else {
        unicode_lang::is_xid_continue
    };
    #[cfg(feature = "std")]
    {
        use std::sync::OnceLock;
        static START: OnceLock<Box<[(u32, u32)]>> = OnceLock::new();
        static CONTINUE: OnceLock<Box<[(u32, u32)]>> = OnceLock::new();
        let cell = if start { &START } else { &CONTINUE };
        cell.get_or_init(|| scan(holds)).clone()
    }
    #[cfg(not(feature = "std"))]
    {
        scan(holds)
    }
}

fn scan(holds: fn(char) -> bool) -> Box<[(u32, u32)]> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    let mut open: Option<u32> = None;
    for cp in 0..=MAX_CHAR + 1 {
        let inside = char::from_u32(cp).is_some_and(holds);
        match (inside, open) {
            (true, None) => open = Some(cp),
            (false, Some(lo)) => {
                out.push((lo, cp - 1));
                open = None;
            }
            _ => {}
        }
    }
    normalize(out)
}

/// Parses `pattern` into a HIR.
pub(crate) fn parse(pattern: &str, props: &mut Props) -> Result<Hir, PatternError> {
    let mut parser = Parser {
        src: pattern,
        pos: 0,
        depth: 0,
        props,
    };
    let hir = parser.alternation()?;
    if parser.pos < pattern.len() {
        // `alternation` only stops early at a `)` with no group open.
        return Err(parser.error_here("unmatched `)`"));
    }
    Ok(hir)
}

/// A recursive-descent parser over the pattern text.
struct Parser<'a, 'p> {
    src: &'a str,
    pos: usize,
    depth: u32,
    props: &'p mut Props,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        let mut chars = self.src[self.pos..].chars();
        let _ = chars.next();
        chars.next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn error_here(&self, reason: &'static str) -> PatternError {
        PatternError {
            offset: self.pos,
            reason,
        }
    }

    fn error_at(offset: usize, reason: &'static str) -> PatternError {
        PatternError { offset, reason }
    }

    /// `alternation := concat ('|' concat)*`
    fn alternation(&mut self) -> Result<Hir, PatternError> {
        let first = self.concat()?;
        if self.peek() != Some('|') {
            return Ok(first);
        }
        let mut branches = vec![first];
        while self.eat('|') {
            branches.push(self.concat()?);
        }
        Ok(Hir::Alt(branches))
    }

    /// `concat := repeat*`
    fn concat(&mut self) -> Result<Hir, PatternError> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            parts.push(self.repeat()?);
        }
        Ok(match parts.len() {
            0 => Hir::Empty,
            1 => parts.pop().unwrap_or(Hir::Empty),
            _ => Hir::Concat(parts),
        })
    }

    /// `repeat := atom quantifier?`
    fn repeat(&mut self) -> Result<Hir, PatternError> {
        let atom = self.atom()?;
        let start = self.pos;
        let (min, max) = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                let _ = self.bump();
                let bounds = self.counted(start)?;
                self.check_no_requantifier()?;
                return Ok(repeat(atom, bounds.0, bounds.1));
            }
            _ => return Ok(atom),
        };
        let _ = self.bump();
        self.check_no_requantifier()?;
        Ok(repeat(atom, min, max))
    }

    /// Rejects a quantifier directly after another one.
    fn check_no_requantifier(&self) -> Result<(), PatternError> {
        match self.peek() {
            Some('?') => Err(self.error_here(
                "lazy repetition is not supported; tokens always take the longest match",
            )),
            Some('*' | '+' | '{') => {
                Err(self.error_here("a repetition cannot be repeated; wrap it in a group"))
            }
            _ => Ok(()),
        }
    }

    /// The body of `{n}`, `{n,}`, or `{n,m}`, after the brace.
    fn counted(&mut self, open: usize) -> Result<(u32, Option<u32>), PatternError> {
        let min = self
            .number()?
            .ok_or_else(|| Self::error_at(open, "a counted repetition needs a minimum"))?;
        let max = if self.eat(',') {
            self.number()?
        } else {
            Some(min)
        };
        if !self.eat('}') {
            return Err(Self::error_at(open, "unclosed counted repetition"));
        }
        if let Some(max) = max {
            if max < min {
                return Err(Self::error_at(
                    open,
                    "the repetition maximum is below its minimum",
                ));
            }
        }
        Ok((min, max))
    }

    /// A decimal count, or `None` if no digit follows.
    fn number(&mut self) -> Result<Option<u32>, PatternError> {
        let start = self.pos;
        let mut value: u32 = 0;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            let _ = self.bump();
            value = value.saturating_mul(10).saturating_add(d);
        }
        if self.pos == start {
            return Ok(None);
        }
        if value > REPEAT_LIMIT {
            return Err(Self::error_at(
                start,
                "repetition counts are limited to 1000",
            ));
        }
        Ok(Some(value))
    }

    /// One atom: a group, class, `.`, escape, or literal character.
    fn atom(&mut self) -> Result<Hir, PatternError> {
        let start = self.pos;
        let Some(c) = self.bump() else {
            return Err(self.error_here("unexpected end of pattern"));
        };
        match c {
            '(' => self.group(start),
            '[' => self.class(start),
            '.' => Ok(Hir::Class(complement(&[('\n' as u32, '\n' as u32)]))),
            '\\' => match self.escape(start)? {
                Escape::Char(c) => Ok(single(c)),
                Escape::Class(ranges) => Ok(Hir::Class(ranges)),
            },
            '^' | '$' => Err(Self::error_at(
                start,
                "anchors are not supported; a token pattern always matches at the token start",
            )),
            '*' | '+' | '?' | '{' => Err(Self::error_at(
                start,
                "the repetition has nothing to repeat",
            )),
            c => Ok(single(c as u32)),
        }
    }

    /// A group, after its `(`.
    fn group(&mut self, open: usize) -> Result<Hir, PatternError> {
        if self.depth >= DEPTH_LIMIT {
            return Err(Self::error_at(open, "groups are nested too deeply"));
        }
        if self.eat('?') && !self.eat(':') {
            return Err(Self::error_at(
                open,
                "group flags are not supported; only `(?:...)` is accepted",
            ));
        }
        self.depth += 1;
        let inner = self.alternation()?;
        self.depth -= 1;
        if !self.eat(')') {
            return Err(Self::error_at(open, "unclosed group"));
        }
        Ok(inner)
    }

    /// A bracketed class, after its `[`.
    fn class(&mut self, open: usize) -> Result<Hir, PatternError> {
        let negated = self.eat('^');
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut first = true;
        loop {
            let item_start = self.pos;
            let Some(c) = self.bump() else {
                return Err(Self::error_at(open, "unclosed class"));
            };
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = match c {
                '[' => {
                    return Err(Self::error_at(
                        item_start,
                        "nested classes are not supported; escape `[` as `\\[`",
                    ));
                }
                '\\' => match self.escape(item_start)? {
                    Escape::Char(c) => c,
                    Escape::Class(set) => {
                        if self.peek() == Some('-') && !matches!(self.peek2(), Some(']') | None) {
                            return Err(self.error_here("a class range cannot start at a class"));
                        }
                        ranges.extend_from_slice(&set);
                        continue;
                    }
                },
                c => c as u32,
            };
            if self.peek() == Some('-') && !matches!(self.peek2(), Some(']') | None) {
                let _ = self.bump();
                let hi_start = self.pos;
                let hi = match self.bump() {
                    Some('\\') => match self.escape(hi_start)? {
                        Escape::Char(c) => c,
                        Escape::Class(_) => {
                            return Err(Self::error_at(
                                hi_start,
                                "a class range cannot end at a class",
                            ));
                        }
                    },
                    Some(c) => c as u32,
                    None => return Err(Self::error_at(open, "unclosed class")),
                };
                if hi < lo {
                    return Err(Self::error_at(
                        item_start,
                        "the class range is out of order",
                    ));
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        let set = normalize(ranges);
        let set = if negated { complement(&set) } else { set };
        if set.is_empty() {
            return Err(Self::error_at(open, "the class matches no character"));
        }
        Ok(Hir::Class(set))
    }

    /// An escape, after its `\`.
    fn escape(&mut self, start: usize) -> Result<Escape, PatternError> {
        let Some(c) = self.bump() else {
            return Err(Self::error_at(start, "the pattern ends with `\\`"));
        };
        let c = match c {
            'n' => '\n' as u32,
            't' => '\t' as u32,
            'r' => '\r' as u32,
            'f' => 0x0C,
            'v' => 0x0B,
            'x' => self.hex(start, 2)?,
            'u' => self.hex(start, 4)?,
            'd' => return Ok(Escape::Class(DIGIT.into())),
            'D' => return Ok(Escape::Class(complement(DIGIT))),
            'w' => return Ok(Escape::Class(WORD.into())),
            'W' => return Ok(Escape::Class(complement(WORD))),
            's' => return Ok(Escape::Class(SPACE.into())),
            'S' => return Ok(Escape::Class(complement(SPACE))),
            'p' | 'P' => return self.property(start, c == 'P'),
            c if c.is_ascii_punctuation() || c == ' ' => c as u32,
            _ => return Err(Self::error_at(start, "unknown escape")),
        };
        Ok(Escape::Char(c))
    }

    /// A Unicode property escape body, `{Name}`, after `\p` or `\P`.
    fn property(&mut self, start: usize, negated: bool) -> Result<Escape, PatternError> {
        if !self.eat('{') {
            return Err(Self::error_at(
                start,
                "a property escape is written `\\p{Name}`",
            ));
        }
        let name_start = self.pos;
        while self.peek().is_some_and(|c| c != '}') {
            let _ = self.bump();
        }
        let name_end = self.pos;
        if !self.eat('}') {
            return Err(Self::error_at(start, "unclosed property escape"));
        }
        let ranges: Box<[(u32, u32)]> = match &self.src[name_start..name_end] {
            "XID_Start" | "XIDS" => self.props.xid(true).into(),
            "XID_Continue" | "XIDC" => self.props.xid(false).into(),
            _ => return Err(Self::error_at(start, UNSUPPORTED_PROPERTY)),
        };
        Ok(Escape::Class(if negated {
            complement(&ranges)
        } else {
            ranges
        }))
    }

    /// A hex escape body: `{H...}` or exactly `digits` hex digits.
    fn hex(&mut self, start: usize, digits: usize) -> Result<u32, PatternError> {
        let braced = self.eat('{');
        let mut value: u32 = 0;
        let mut count = 0;
        loop {
            if braced && self.eat('}') {
                break;
            }
            if !braced && count == digits {
                break;
            }
            let Some(d) = self.peek().and_then(|c| c.to_digit(16)) else {
                return Err(Self::error_at(start, "malformed hex escape"));
            };
            let _ = self.bump();
            count += 1;
            if count > 8 {
                return Err(Self::error_at(start, "malformed hex escape"));
            }
            value = (value << 4) | d;
        }
        if count == 0 {
            return Err(Self::error_at(start, "malformed hex escape"));
        }
        if value > MAX_CHAR || (0xD800..=0xDFFF).contains(&value) {
            return Err(Self::error_at(
                start,
                "the hex escape is not a Unicode scalar value",
            ));
        }
        Ok(value)
    }
}

/// What an escape denotes.
enum Escape {
    Char(u32),
    Class(Box<[(u32, u32)]>),
}

/// `\d`: ASCII digits.
const DIGIT: &[(u32, u32)] = &[(0x30, 0x39)];
/// `\w`: ASCII letters, digits, and `_`.
const WORD: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)];
/// `\s`: ASCII whitespace — tab, line feed, vertical tab, form feed, carriage
/// return, and space.
const SPACE: &[(u32, u32)] = &[(0x09, 0x0D), (0x20, 0x20)];

fn single(c: u32) -> Hir {
    Hir::Class(Box::new([(c, c)]))
}

fn repeat(hir: Hir, min: u32, max: Option<u32>) -> Hir {
    Hir::Repeat {
        hir: Box::new(hir),
        min,
        max,
    }
}

/// Sorts and merges ranges, and removes the surrogate block.
fn normalize(mut ranges: Vec<(u32, u32)>) -> Box<[(u32, u32)]> {
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (lo, hi) in ranges {
        match merged.last_mut() {
            Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    let mut out = Vec::with_capacity(merged.len() + 1);
    for (lo, hi) in merged {
        // Split around U+D800..=U+DFFF, which no `char` can hold.
        if lo < 0xD800 {
            out.push((lo, hi.min(0xD7FF)));
        }
        if hi > 0xDFFF {
            out.push((lo.max(0xE000), hi));
        }
    }
    out.into_boxed_slice()
}

/// The scalar values not in `set` (which must be normalized).
fn complement(set: &[(u32, u32)]) -> Box<[(u32, u32)]> {
    let mut out = Vec::with_capacity(set.len() + 2);
    let mut next = 0u32;
    for &(lo, hi) in set {
        if lo > next {
            out.push((next, lo - 1));
        }
        next = hi + 1;
    }
    if next <= MAX_CHAR {
        out.push((next, MAX_CHAR));
    }
    normalize(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn parse_t(p: &str) -> Result<Hir, PatternError> {
        parse(p, &mut Props::default())
    }

    #[test]
    fn xid_properties() {
        let Hir::Class(r) = parse_t(r"\p{XID_Start}").unwrap() else {
            panic!()
        };
        let has = |r: &[(u32, u32)], c: u32| r.iter().any(|&(lo, hi)| lo <= c && c <= hi);
        assert!(has(&r, 0x3B1));
        assert!(!has(&r, u32::from(b'_')));
        let Hir::Class(r) = parse_t(r"[\p{XID_Start}_]").unwrap() else {
            panic!()
        };
        assert!(has(&r, 95));
        let Hir::Class(r) = parse_t(r"\P{XID_Continue}").unwrap() else {
            panic!()
        };
        assert!(has(&r, 32));
        assert!(!has(&r, 97));
        assert_eq!(parse_t(r"\p{Sm}").unwrap_err().reason, UNSUPPORTED_PROPERTY);
        assert_eq!(
            parse_t(r"\p{XID_Start").unwrap_err().reason,
            "unclosed property escape"
        );
        assert!(parse_t(r"\pL").is_err());
    }

    fn class(ranges: &[(u32, u32)]) -> Hir {
        Hir::Class(ranges.into())
    }

    #[test]
    fn literals_and_sequences() {
        assert_eq!(parse_t("a").unwrap(), class(&[(97, 97)]));
        assert_eq!(parse_t("").unwrap(), Hir::Empty);
        assert_eq!(
            parse_t("ab").unwrap(),
            Hir::Concat(vec![class(&[(97, 97)]), class(&[(98, 98)])])
        );
    }

    #[test]
    fn repetitions() {
        let a = || Box::new(class(&[(97, 97)]));
        assert_eq!(
            parse_t("a*").unwrap(),
            Hir::Repeat {
                hir: a(),
                min: 0,
                max: None
            }
        );
        assert_eq!(
            parse_t("a+").unwrap(),
            Hir::Repeat {
                hir: a(),
                min: 1,
                max: None
            }
        );
        assert_eq!(
            parse_t("a?").unwrap(),
            Hir::Repeat {
                hir: a(),
                min: 0,
                max: Some(1)
            }
        );
        assert_eq!(
            parse_t("a{3}").unwrap(),
            Hir::Repeat {
                hir: a(),
                min: 3,
                max: Some(3)
            }
        );
        assert_eq!(
            parse_t("a{2,}").unwrap(),
            Hir::Repeat {
                hir: a(),
                min: 2,
                max: None
            }
        );
        assert_eq!(
            parse_t("a{2,5}").unwrap(),
            Hir::Repeat {
                hir: a(),
                min: 2,
                max: Some(5)
            }
        );
    }

    #[test]
    fn classes() {
        assert_eq!(parse_t("[a-c]").unwrap(), class(&[(97, 99)]));
        assert_eq!(parse_t("[c-ea-b]").unwrap(), class(&[(97, 101)]));
        assert_eq!(parse_t("[-a]").unwrap(), class(&[(45, 45), (97, 97)]));
        assert_eq!(parse_t("[a-]").unwrap(), class(&[(45, 45), (97, 97)]));
        assert_eq!(parse_t("[]]").unwrap(), class(&[(93, 93)]));
        assert_eq!(parse_t(r"[\]\\]").unwrap(), class(&[(92, 93)]));
        assert_eq!(parse_t(r"[\d_]").unwrap(), class(&[(48, 57), (95, 95)]));
        assert_eq!(
            parse_t("[^a]").unwrap(),
            class(&[(0, 96), (98, 0xD7FF), (0xE000, MAX_CHAR)])
        );
        assert_eq!(
            parse_t(".").unwrap(),
            class(&[(0, 9), (11, 0xD7FF), (0xE000, MAX_CHAR)])
        );
        assert_eq!(
            parse_t(r"[\x{D000}-\x{E000}]").unwrap(),
            class(&[(0xD000, 0xD7FF), (0xE000, 0xE000)])
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(parse_t(r"\n").unwrap(), class(&[(10, 10)]));
        assert_eq!(parse_t(r"\.").unwrap(), class(&[(46, 46)]));
        assert_eq!(parse_t(r"\x41").unwrap(), class(&[(65, 65)]));
        assert_eq!(parse_t(r"\x{1F600}").unwrap(), class(&[(0x1F600, 0x1F600)]));
        assert_eq!(parse_t(r"é").unwrap(), class(&[(0xE9, 0xE9)]));
        assert_eq!(parse_t(r"\s").unwrap(), class(SPACE));
    }

    #[test]
    fn groups_and_alternation() {
        assert_eq!(
            parse_t("a|b").unwrap(),
            Hir::Alt(vec![class(&[(97, 97)]), class(&[(98, 98)])])
        );
        assert_eq!(parse_t("(?:a)").unwrap(), class(&[(97, 97)]));
        assert_eq!(
            parse_t("(a|)").unwrap(),
            Hir::Alt(vec![class(&[(97, 97)]), Hir::Empty])
        );
    }

    #[test]
    fn errors() {
        let err = |p: &str| parse(p, &mut Props::default()).unwrap_err();
        assert_eq!(err("(a").reason, "unclosed group");
        assert_eq!(err("a)").offset, 1);
        assert_eq!(err("[a").reason, "unclosed class");
        assert_eq!(err("*").reason, "the repetition has nothing to repeat");
        assert_eq!(err("a**").offset, 2);
        assert!(err("a+?").reason.starts_with("lazy"));
        assert!(err("^a").reason.starts_with("anchors"));
        assert_eq!(
            err("a{2,1}").reason,
            "the repetition maximum is below its minimum"
        );
        assert_eq!(
            err("a{1001}").reason,
            "repetition counts are limited to 1000"
        );
        assert_eq!(err("a{,3}").reason, "a counted repetition needs a minimum");
        assert_eq!(err(r"\q").reason, "unknown escape");
        assert_eq!(
            err(r"\x{D800}").reason,
            "the hex escape is not a Unicode scalar value"
        );
        assert_eq!(err(r"\xZZ").reason, "malformed hex escape");
        assert_eq!(err("[z-a]").reason, "the class range is out of order");
        assert_eq!(
            err(r"[^\x00-\x{10FFFF}]").reason,
            "the class matches no character"
        );
        assert_eq!(err(r"\").reason, "the pattern ends with `\\`");
        assert_eq!(
            err("(?i)a").reason,
            "group flags are not supported; only `(?:...)` is accepted"
        );
        let deep = "(".repeat(100) + &")".repeat(100);
        assert_eq!(err(&deep).reason, "groups are nested too deeply");
    }
}
