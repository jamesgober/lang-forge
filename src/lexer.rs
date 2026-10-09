//! The forged lexer: tables built once from the schematic, and a scanner that
//! turns source text into a lossless token stream.
//!
//! Every byte of the source lands in exactly one token, trivia included, so
//! concatenating the tokens reproduces the input. The scanner dispatches on a
//! 256-entry byte-class table; identifiers, numbers, and whitespace run tight
//! ASCII loops and fall back to Unicode decoding only on non-ASCII bytes;
//! keywords, symbols, and comment and string delimiters are fixed texts
//! matched longest-first from a short list per leading byte.

use alloc::{boxed::Box, format, string::String, vec::Vec};

use diag_lang::{Diagnostic, Label, Severity};
use syntax_lang::{Span, Token};

use crate::{
    error::Report,
    kind::Kind,
    schematic::{IdentMode, LexerSpec},
};

/// The UTF-8 byte-order mark, skipped as whitespace at the start of a source.
pub(crate) const BOM: char = '\u{FEFF}';

/// Kind indexes of the built-in token classes. Literal tokens follow them.
pub(crate) const UNKNOWN: u16 = 0;
pub(crate) const WHITESPACE: u16 = 1;
pub(crate) const COMMENT: u16 = 2;
pub(crate) const NEWLINE: u16 = 3;
pub(crate) const IDENT: u16 = 4;
pub(crate) const NUMBER: u16 = 5;
pub(crate) const STRING: u16 = 6;
pub(crate) const FIRST_LITERAL: u16 = 7;

/// The names of the built-in token classes, by kind index.
pub(crate) const BUILTIN_TOKENS: [&str; FIRST_LITERAL as usize] = [
    "UNKNOWN",
    "WHITESPACE",
    "COMMENT",
    "NEWLINE",
    "IDENT",
    "NUMBER",
    "STRING",
];

// Byte classes. A byte may carry several (a non-ASCII lead byte can begin
// both a symbol and an identifier).
const C_SPACE: u8 = 1;
const C_CR: u8 = 2;
const C_LF: u8 = 4;
const C_IDENT: u8 = 8;
const C_DIGIT: u8 = 16;
const C_FIXED: u8 = 32;
const C_HIGH: u8 = 64;

/// What a matched fixed text produces.
#[derive(Clone, Copy, Debug)]
enum Action {
    Symbol(Kind),
    LineComment,
    BlockComment { close: u32 },
    Str { rule: u32 },
}

/// One fixed text: a symbol, or a comment or string opener.
#[derive(Clone, Debug)]
struct Pattern {
    text: Box<[u8]>,
    action: Action,
}

/// How one kind of string literal is scanned.
#[derive(Clone, Debug)]
struct StringRule {
    close: Box<[u8]>,
    escape: Option<u8>,
    multiline: bool,
}

/// The forged lexer.
#[derive(Clone, Debug)]
pub(crate) struct Lexer {
    mode: IdentMode,
    newlines: bool,
    nested: bool,
    class: [u8; 256],
    /// Patterns grouped by leading byte, longest first within a group.
    patterns: Box<[Pattern]>,
    /// `patterns[heads[b]..heads[b + 1]]` start with byte `b`.
    heads: Box<[u32; 257]>,
    block_closes: Box<[Box<[u8]>]>,
    block_opens: Box<[Box<[u8]>]>,
    strings: Box<[StringRule]>,
    keywords: Keywords,
    // The built-in kinds, with their trivia flags for this language.
    k_unknown: Kind,
    k_whitespace: Kind,
    k_comment: Kind,
    k_newline: Kind,
    k_ident: Kind,
    k_number: Kind,
    k_string: Kind,
}

/// Whether `c` can begin an identifier.
#[inline]
pub(crate) fn is_ident_start(c: char, mode: IdentMode) -> bool {
    c.is_ascii_alphabetic()
        || c == '_'
        || (mode == IdentMode::Xid && !c.is_ascii() && unicode_lang::is_xid_start(c))
}

/// Whether `c` can continue an identifier.
#[inline]
pub(crate) fn is_ident_continue(c: char, mode: IdentMode) -> bool {
    c.is_ascii_alphanumeric()
        || c == '_'
        || (mode == IdentMode::Xid && !c.is_ascii() && unicode_lang::is_xid_continue(c))
}

/// Whether `text` lexes as one identifier.
pub(crate) fn is_ident(text: &str, mode: IdentMode) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| is_ident_start(c, mode))
        && chars.all(|c| is_ident_continue(c, mode))
}

/// Non-ASCII whitespace the lexer treats as whitespace: Unicode's
/// Pattern_White_Space characters outside ASCII (UAX #31).
#[inline]
fn is_unicode_space(c: char) -> bool {
    matches!(
        c,
        '\u{85}' | '\u{200E}' | '\u{200F}' | '\u{2028}' | '\u{2029}'
    )
}

/// The length of the UTF-8 sequence that starts with `lead`.
#[inline]
fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

impl Lexer {
    /// Builds the lexer for a schematic's `[lexer]` table and the literals its
    /// grammar uses. Conflicting delimiters are reported.
    pub(crate) fn build(
        spec: &LexerSpec<'_>,
        literals: &[(&str, Kind, Span)],
        report: &mut Report,
    ) -> Self {
        let mut patterns: Vec<(Box<[u8]>, Action, Span)> = Vec::new();
        let mut keywords = Vec::new();
        for &(text, kind, span) in literals {
            if is_ident(text, spec.identifiers) {
                keywords.push((Box::<str>::from(text), kind));
            } else {
                patterns.push((text.as_bytes().into(), Action::Symbol(kind), span));
            }
        }
        for (open, span) in &spec.line_comments {
            check_delimiter(open, *span, "comment", spec.identifiers, report);
            patterns.push((open.as_bytes().into(), Action::LineComment, *span));
        }
        let mut block_opens = Vec::new();
        let mut block_closes = Vec::new();
        for block in &spec.block_comments {
            check_delimiter(&block.open, block.span, "comment", spec.identifiers, report);
            let close = block_closes.len() as u32;
            block_opens.push(Box::<[u8]>::from(block.open.as_bytes()));
            block_closes.push(Box::<[u8]>::from(block.close.as_bytes()));
            patterns.push((
                block.open.as_bytes().into(),
                Action::BlockComment { close },
                block.span,
            ));
        }
        let mut strings = Vec::new();
        for s in &spec.strings {
            check_delimiter(&s.open, s.span, "string", spec.identifiers, report);
            let rule = strings.len() as u32;
            strings.push(StringRule {
                close: s.close.as_bytes().into(),
                escape: s.escape.map(|c| c as u8),
                multiline: s.multiline,
            });
            patterns.push((s.open.as_bytes().into(), Action::Str { rule }, s.span));
        }

        // Empty texts were rejected while reading the schematic; never index one.
        patterns.retain(|p| !p.0.is_empty());
        // Two patterns with the same text could never both be matched.
        patterns.sort_by(|a, b| {
            a.0[0]
                .cmp(&b.0[0])
                .then(b.0.len().cmp(&a.0.len()))
                .then(a.0.cmp(&b.0))
        });
        for pair in patterns.windows(2) {
            if pair[0].0 == pair[1].0 {
                let text = String::from_utf8_lossy(&pair[0].0);
                let (first, second) = if pair[0].2.start() <= pair[1].2.start() {
                    (pair[0].2, pair[1].2)
                } else {
                    (pair[1].2, pair[0].2)
                };
                report.diagnostic(
                    Diagnostic::new(
                        Severity::Error,
                        format!("`{text}` is used for two different things"),
                        Label::new(second, "used again here"),
                    )
                    .with_secondary(Label::new(first, "first used here"))
                    .with_note("a symbol, comment delimiter, or string delimiter must each have its own text"),
                );
            }
        }

        let mut class = [0u8; 256];
        for b in [b' ', b'\t', 0x0B, 0x0C] {
            class[b as usize] |= C_SPACE;
        }
        class[b'\r' as usize] |= C_CR;
        class[b'\n' as usize] |= C_LF;
        for b in (b'a'..=b'z')
            .chain(b'A'..=b'Z')
            .chain(core::iter::once(b'_'))
        {
            class[b as usize] |= C_IDENT;
        }
        for b in b'0'..=b'9' {
            class[b as usize] |= C_DIGIT;
        }
        for b in 0x80..=0xFFu8 {
            class[b as usize] |= C_HIGH;
        }
        let mut heads = Box::new([0u32; 257]);
        for (text, _, _) in &patterns {
            class[text[0] as usize] |= C_FIXED;
            heads[text[0] as usize + 1] += 1;
        }
        for b in 0..256 {
            heads[b + 1] += heads[b];
        }

        let builtin = |index: u16, trivia: bool| Kind::new(index, trivia);
        Self {
            mode: spec.identifiers,
            newlines: spec.newlines,
            nested: spec.nested_comments,
            class,
            patterns: patterns
                .into_iter()
                .map(|(text, action, _)| Pattern { text, action })
                .collect(),
            heads,
            block_closes: block_closes.into(),
            block_opens: block_opens.into(),
            strings: strings.into(),
            keywords: Keywords::new(keywords),
            k_unknown: builtin(UNKNOWN, true),
            k_whitespace: builtin(WHITESPACE, true),
            k_comment: builtin(COMMENT, true),
            k_newline: builtin(NEWLINE, !spec.newlines),
            k_ident: builtin(IDENT, false),
            k_number: builtin(NUMBER, false),
            k_string: builtin(STRING, false),
        }
    }

    /// The built-in kind with index `index`, flagged for this language.
    pub(crate) fn builtin(&self, index: u16) -> Kind {
        match index {
            UNKNOWN => self.k_unknown,
            WHITESPACE => self.k_whitespace,
            COMMENT => self.k_comment,
            NEWLINE => self.k_newline,
            IDENT => self.k_ident,
            NUMBER => self.k_number,
            _ => self.k_string,
        }
    }

    /// Scans `src` into `tokens`, reporting malformed input to `diags`.
    ///
    /// `src` must be shorter than 4 GiB (spans are 32-bit); callers check.
    pub(crate) fn run(
        &self,
        src: &str,
        tokens: &mut Vec<Token<Kind>>,
        diags: &mut Vec<Diagnostic>,
    ) {
        let bytes = src.as_bytes();
        // Source code averages a token every two to three bytes.
        tokens.reserve(bytes.len() / 2 + 1);
        let mut pos = 0;
        // A leading byte-order mark is an encoding signature, not text: it
        // opens the first whitespace token, together with any whitespace
        // after it, so the tree keeps it (it stays lossless) and nothing
        // reports it. A U+FEFF anywhere else is an ordinary character.
        if let Some(rest) = src.strip_prefix(BOM) {
            let end = self.space_end(src, src.len() - rest.len());
            tokens.push(Token::new(self.k_whitespace, Span::new(0, end as u32)));
            pos = end;
        }
        while pos < bytes.len() {
            let class = self.class[bytes[pos] as usize];
            let (end, kind) = if class & C_FIXED != 0 {
                match self.fixed(src, pos, diags) {
                    Some(hit) => hit,
                    None => self.other(src, pos, class, diags),
                }
            } else {
                self.other(src, pos, class, diags)
            };
            tokens.push(Token::new(kind, Span::new(pos as u32, end as u32)));
            pos = end;
        }
    }

    /// The longest fixed text at `pos`, if any, and the token it begins.
    fn fixed(&self, src: &str, pos: usize, diags: &mut Vec<Diagnostic>) -> Option<(usize, Kind)> {
        let bytes = src.as_bytes();
        let lead = bytes[pos] as usize;
        let group = &self.patterns[self.heads[lead] as usize..self.heads[lead + 1] as usize];
        let rest = &bytes[pos..];
        let pattern = group.iter().find(|p| rest.starts_with(&p.text))?;
        let after = pos + pattern.text.len();
        Some(match pattern.action {
            Action::Symbol(kind) => (after, kind),
            Action::LineComment => (line_end(bytes, after), self.k_comment),
            Action::BlockComment { close } => (
                self.block_comment(src, pos, after, close as usize, diags),
                self.k_comment,
            ),
            Action::Str { rule } => (
                self.string(bytes, pos, after, &self.strings[rule as usize], diags),
                self.k_string,
            ),
        })
    }

    /// A token that does not begin with a fixed text.
    fn other(
        &self,
        src: &str,
        pos: usize,
        class: u8,
        diags: &mut Vec<Diagnostic>,
    ) -> (usize, Kind) {
        let bytes = src.as_bytes();
        if class & C_IDENT != 0 {
            let end = self.ident_end(src, pos + 1);
            let kind = self.keywords.get(&src[pos..end]).unwrap_or(self.k_ident);
            return (end, kind);
        }
        if class & C_DIGIT != 0 {
            return (self.number(src, pos, diags), self.k_number);
        }
        if class & C_LF != 0 && self.newlines {
            return (pos + 1, self.k_newline);
        }
        if class & C_CR != 0 && self.newlines && bytes.get(pos + 1) == Some(&b'\n') {
            return (pos + 2, self.k_newline);
        }
        if class & (C_SPACE | C_CR | C_LF) != 0 {
            return (self.space_end(src, pos), self.k_whitespace);
        }
        if class & C_HIGH != 0 {
            let c = char_at(src, pos);
            if is_ident_start(c, self.mode) {
                let end = self.ident_end(src, pos + c.len_utf8());
                let kind = self.keywords.get(&src[pos..end]).unwrap_or(self.k_ident);
                return (end, kind);
            }
            if is_unicode_space(c) {
                return (self.space_end(src, pos), self.k_whitespace);
            }
        }
        (self.unknown(src, pos, diags), self.k_unknown)
    }

    /// The end of the identifier whose remaining characters start at `pos`.
    #[inline]
    fn ident_end(&self, src: &str, mut pos: usize) -> usize {
        let bytes = src.as_bytes();
        loop {
            while pos < bytes.len() && self.class[bytes[pos] as usize] & (C_IDENT | C_DIGIT) != 0 {
                pos += 1;
            }
            if pos < bytes.len() && bytes[pos] >= 0x80 && self.mode == IdentMode::Xid {
                let c = char_at(src, pos);
                if is_ident_continue(c, self.mode) {
                    pos += c.len_utf8();
                    continue;
                }
            }
            return pos;
        }
    }

    /// The end of the run of whitespace starting at `pos`. Line breaks belong to
    /// the run unless they are significant.
    fn space_end(&self, src: &str, mut pos: usize) -> usize {
        let bytes = src.as_bytes();
        while pos < bytes.len() {
            let class = self.class[bytes[pos] as usize];
            if class & C_SPACE != 0 {
                pos += 1;
            } else if class & (C_LF | C_CR) != 0 {
                let breaks_line = class & C_LF != 0 || bytes.get(pos + 1) == Some(&b'\n');
                if self.newlines && breaks_line {
                    break;
                }
                pos += 1;
            } else if class & C_HIGH != 0 && is_unicode_space(char_at(src, pos)) {
                pos += char_at(src, pos).len_utf8();
            } else {
                break;
            }
        }
        pos
    }

    /// A number literal: decimal with optional fraction and exponent, or
    /// `0x`/`0o`/`0b` with digits of that radix; `_` may separate digits.
    fn number(&self, src: &str, start: usize, diags: &mut Vec<Diagnostic>) -> usize {
        let bytes = src.as_bytes();
        let digit =
            |i: usize, radix: u32| bytes.get(i).is_some_and(|b| char::from(*b).is_digit(radix));
        let radix = match (bytes[start], bytes.get(start + 1)) {
            (b'0', Some(b'x' | b'X')) => 16,
            (b'0', Some(b'o' | b'O')) => 8,
            (b'0', Some(b'b' | b'B')) => 2,
            _ => 10,
        };
        if radix != 10 {
            let digits_start = start + 2;
            let end = self.ident_end(src, digits_start);
            let digits = &src[digits_start..end];
            let name = match radix {
                16 => "hexadecimal",
                8 => "octal",
                _ => "binary",
            };
            if let Some(bad) = digits.chars().find(|c| *c != '_' && !c.is_digit(radix)) {
                let at = digits_start + digits.find(bad).unwrap_or(0);
                diags.push(error(
                    Span::new(at as u32, (at + bad.len_utf8()) as u32),
                    format!("invalid digit `{bad}` in {name} literal"),
                ));
            } else if !digits.bytes().any(|b| b != b'_') {
                diags.push(error(
                    Span::new(start as u32, end as u32),
                    format!("{name} literal has no digits"),
                ));
            }
            return end;
        }
        let mut pos = start;
        while bytes
            .get(pos)
            .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
        {
            pos += 1;
        }
        if bytes.get(pos) == Some(&b'.') && digit(pos + 1, 10) {
            pos += 1;
            while bytes
                .get(pos)
                .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
            {
                pos += 1;
            }
        }
        if matches!(bytes.get(pos), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(bytes.get(pos + 1), Some(b'+' | b'-')));
            if digit(pos + 1 + sign, 10) {
                pos += 1 + sign;
                while bytes
                    .get(pos)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
                {
                    pos += 1;
                }
            }
        }
        let end = self.ident_end(src, pos);
        if end > pos {
            diags.push(error(
                Span::new(pos as u32, end as u32),
                format!("invalid suffix `{}` on number literal", &src[pos..end]),
            ));
        }
        end
    }

    /// A block comment opened at `start`, whose body begins at `pos`.
    fn block_comment(
        &self,
        src: &str,
        start: usize,
        pos: usize,
        close: usize,
        diags: &mut Vec<Diagnostic>,
    ) -> usize {
        let bytes = src.as_bytes();
        let close_text = &self.block_closes[close];
        let open_text = &self.block_opens[close];
        let unterminated = |diags: &mut Vec<Diagnostic>| {
            diags.push(error(
                Span::new(start as u32, (start + open_text.len()) as u32),
                "unterminated block comment",
            ));
            bytes.len()
        };
        if !self.nested {
            // A single search for the closing text; `find` on `str` is fast.
            let close_str = core::str::from_utf8(close_text).unwrap_or("");
            return match src[pos..].find(close_str) {
                Some(at) => pos + at + close_text.len(),
                None => unterminated(diags),
            };
        }
        let mut depth = 1usize;
        let mut i = pos;
        while i < bytes.len() {
            let rest = &bytes[i..];
            if rest.starts_with(close_text) {
                depth -= 1;
                i += close_text.len();
                if depth == 0 {
                    return i;
                }
            } else if rest.starts_with(open_text) {
                depth += 1;
                i += open_text.len();
            } else {
                i += 1;
            }
        }
        unterminated(diags)
    }

    /// A string opened at `start`, whose body begins at `pos`.
    fn string(
        &self,
        bytes: &[u8],
        start: usize,
        pos: usize,
        rule: &StringRule,
        diags: &mut Vec<Diagnostic>,
    ) -> usize {
        let close0 = rule.close[0];
        let mut i = pos;
        loop {
            let Some(&b) = bytes.get(i) else {
                diags.push(error(
                    Span::new(start as u32, pos as u32),
                    "unterminated string",
                ));
                return bytes.len();
            };
            if b == close0 && bytes[i..].starts_with(&rule.close) {
                return i + rule.close.len();
            }
            if Some(b) == rule.escape {
                i += 1;
                if let Some(&next) = bytes.get(i) {
                    i += utf8_len(next);
                }
                continue;
            }
            if b == b'\n' && !rule.multiline {
                diags.push(error(
                    Span::new(start as u32, pos as u32),
                    "unterminated string",
                ));
                return if i > pos && bytes[i - 1] == b'\r' {
                    i - 1
                } else {
                    i
                };
            }
            i += 1;
        }
    }

    /// A run of characters that begin no token, reported once.
    fn unknown(&self, src: &str, start: usize, diags: &mut Vec<Diagnostic>) -> usize {
        let mut end = start + char_at(src, start).len_utf8();
        while end < src.len() && self.begins_nothing(src, end) {
            end += char_at(src, end).len_utf8();
        }
        let text = &src[start..end];
        let shown = if text.chars().all(|c| {
            c.is_ascii_graphic() || (!c.is_ascii() && !c.is_whitespace() && !c.is_control())
        }) {
            format!("`{text}`")
        } else {
            text.chars()
                .map(|c| format!("U+{:04X}", u32::from(c)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let plural = if text.chars().nth(1).is_some() {
            "characters"
        } else {
            "character"
        };
        diags.push(error(
            Span::new(start as u32, end as u32),
            format!("unexpected {plural} {shown}"),
        ));
        end
    }

    /// Whether no token can begin at `pos`.
    fn begins_nothing(&self, src: &str, pos: usize) -> bool {
        let class = self.class[src.as_bytes()[pos] as usize];
        if class & C_FIXED != 0 {
            let lead = src.as_bytes()[pos] as usize;
            let rest = &src.as_bytes()[pos..];
            if self.patterns[self.heads[lead] as usize..self.heads[lead + 1] as usize]
                .iter()
                .any(|p| rest.starts_with(&p.text))
            {
                return false;
            }
        }
        if class & (C_IDENT | C_DIGIT | C_SPACE | C_CR | C_LF) != 0 {
            return false;
        }
        if class & C_HIGH != 0 {
            let c = char_at(src, pos);
            return !is_ident_start(c, self.mode) && !is_unicode_space(c);
        }
        true
    }
}

/// The character starting at byte `pos`, which must be a char boundary.
#[inline]
fn char_at(src: &str, pos: usize) -> char {
    src[pos..].chars().next().unwrap_or('\0')
}

/// The end of a line comment's text: before the line break (and before the
/// `\r` of a `\r\n`).
fn line_end(bytes: &[u8], pos: usize) -> usize {
    match bytes[pos..].iter().position(|b| *b == b'\n') {
        Some(at) => {
            let lf = pos + at;
            if lf > pos && bytes[lf - 1] == b'\r' {
                lf - 1
            } else {
                lf
            }
        }
        None => bytes.len(),
    }
}

fn error(span: Span, message: impl Into<Box<str>>) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, Label::unlabelled(span))
}

/// Rejects comment and string delimiters the scanner could never reach.
fn check_delimiter(text: &str, span: Span, what: &str, mode: IdentMode, report: &mut Report) {
    let Some(first) = text.chars().next() else {
        return;
    };
    if first.is_whitespace() || text.contains(char::is_whitespace) {
        report.error(
            span,
            format!("{what} delimiter `{text}` contains whitespace"),
        );
    } else if first.is_ascii_digit() {
        report.error(
            span,
            format!("{what} delimiter `{text}` starts with a digit, which begins a number"),
        );
    } else if is_ident_start(first, mode) {
        report.error(
            span,
            format!(
                "{what} delimiter `{text}` starts like an identifier, which the lexer reads first"
            ),
        );
    }
}

/// Keyword lookup: an open-addressing hash table over the keyword texts.
#[derive(Clone, Debug, Default)]
struct Keywords {
    /// `0` for an empty slot, otherwise an index into `entries` plus one.
    slots: Box<[u32]>,
    entries: Box<[(Box<str>, Kind)]>,
    min_len: usize,
    max_len: usize,
}

impl Keywords {
    fn new(entries: Vec<(Box<str>, Kind)>) -> Self {
        if entries.is_empty() {
            return Self::default();
        }
        let capacity = (entries.len() * 2).next_power_of_two();
        let mask = capacity - 1;
        let mut slots = alloc::vec![0u32; capacity].into_boxed_slice();
        for (i, (text, _)) in entries.iter().enumerate() {
            let mut at = hash(text.as_bytes()) & mask;
            while slots[at] != 0 {
                at = (at + 1) & mask;
            }
            slots[at] = i as u32 + 1;
        }
        Self {
            min_len: entries.iter().map(|e| e.0.len()).min().unwrap_or(0),
            max_len: entries.iter().map(|e| e.0.len()).max().unwrap_or(0),
            slots,
            entries: entries.into(),
        }
    }

    #[inline]
    fn get(&self, text: &str) -> Option<Kind> {
        if text.len() < self.min_len || text.len() > self.max_len || self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut at = hash(text.as_bytes()) & mask;
        loop {
            let slot = self.slots[at];
            if slot == 0 {
                return None;
            }
            let (word, kind) = &self.entries[slot as usize - 1];
            if **word == *text {
                return Some(*kind);
            }
            at = (at + 1) & mask;
        }
    }
}

/// FxHash-style multiplicative hash: fast on the short strings keywords are.
#[inline]
fn hash(bytes: &[u8]) -> usize {
    let mut h: u64 = 0;
    for &b in bytes {
        h = (h.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    (h ^ (h >> 32)) as usize
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use alloc::{borrow::Cow, string::ToString};

    use super::*;
    use crate::schematic::{BlockSpec, StringSpec};

    fn spec() -> LexerSpec<'static> {
        LexerSpec {
            identifiers: IdentMode::Xid,
            newlines: false,
            line_comments: alloc::vec![(Cow::Borrowed("//"), Span::new(0, 1))],
            block_comments: alloc::vec![BlockSpec {
                open: Cow::Borrowed("/*"),
                close: Cow::Borrowed("*/"),
                span: Span::new(0, 1),
            }],
            nested_comments: false,
            strings: alloc::vec![StringSpec {
                open: Cow::Borrowed("\""),
                close: Cow::Borrowed("\""),
                escape: Some('\\'),
                multiline: false,
                span: Span::new(0, 1),
            }],
        }
    }

    fn lexer_with(spec: &LexerSpec<'_>) -> Lexer {
        let lits = [
            ("let", Kind::new(7, false), Span::new(0, 1)),
            ("=", Kind::new(8, false), Span::new(0, 1)),
            ("==", Kind::new(9, false), Span::new(0, 1)),
            ("+", Kind::new(10, false), Span::new(0, 1)),
            ("/", Kind::new(11, false), Span::new(0, 1)),
            ("→", Kind::new(12, false), Span::new(0, 1)),
        ];
        let mut report = Report::default();
        let lexer = Lexer::build(spec, &lits, &mut report);
        assert!(report.is_clean());
        lexer
    }

    /// Lexes `src` into `name:text` pairs, with diagnostics.
    fn lex(lexer: &Lexer, src: &str) -> (Vec<String>, Vec<String>) {
        let mut tokens = Vec::new();
        let mut diags = Vec::new();
        lexer.run(src, &mut tokens, &mut diags);
        let mut covered = 0;
        let names = tokens
            .iter()
            .map(|t| {
                assert_eq!(
                    t.span().start().to_usize(),
                    covered,
                    "tokens must be contiguous"
                );
                covered = t.span().end().to_usize();
                let i = t.kind().index();
                let name = BUILTIN_TOKENS
                    .get(i)
                    .map_or_else(|| format!("#{i}"), |n| n.to_string());
                format!(
                    "{name}:{}",
                    &src[t.span().start().to_usize()..t.span().end().to_usize()]
                )
            })
            .collect();
        assert_eq!(covered, src.len(), "tokens must cover the source");
        (
            names,
            diags.iter().map(|d| d.message().to_string()).collect(),
        )
    }

    #[test]
    fn test_lex_keywords_symbols_and_longest_match() {
        let lexer = lexer_with(&spec());
        let (tokens, diags) = lex(&lexer, "let x==y = lettuce+1");
        assert!(diags.is_empty());
        assert_eq!(
            tokens,
            [
                "#7:let",
                "WHITESPACE: ",
                "IDENT:x",
                "#9:==",
                "IDENT:y",
                "WHITESPACE: ",
                "#8:=",
                "WHITESPACE: ",
                "IDENT:lettuce",
                "#10:+",
                "NUMBER:1"
            ]
        );
    }

    #[test]
    fn test_lex_comments_and_strings() {
        let lexer = lexer_with(&spec());
        let (tokens, diags) = lex(&lexer, "a // note\r\n/* b */ \"s\\\"t\" / c");
        assert!(diags.is_empty());
        assert_eq!(
            tokens,
            [
                "IDENT:a",
                "WHITESPACE: ",
                "COMMENT:// note",
                "WHITESPACE:\r\n",
                "COMMENT:/* b */",
                "WHITESPACE: ",
                "STRING:\"s\\\"t\"",
                "WHITESPACE: ",
                "#11:/",
                "WHITESPACE: ",
                "IDENT:c"
            ]
        );
    }

    #[test]
    fn test_lex_unicode_identifiers_and_symbols() {
        let lexer = lexer_with(&spec());
        let (tokens, diags) = lex(&lexer, "naïve→ö_1\u{2028}x");
        assert!(diags.is_empty());
        assert_eq!(
            tokens,
            [
                "IDENT:naïve",
                "#12:→",
                "IDENT:ö_1",
                "WHITESPACE:\u{2028}",
                "IDENT:x"
            ]
        );
    }

    #[test]
    fn test_lex_ascii_mode_rejects_unicode_identifiers() {
        let mut s = spec();
        s.identifiers = IdentMode::Ascii;
        let lexer = lexer_with(&s);
        let (tokens, diags) = lex(&lexer, "aé");
        assert_eq!(tokens, ["IDENT:a", "UNKNOWN:é"]);
        assert_eq!(diags, ["unexpected character `é`"]);
    }

    #[test]
    fn test_lex_numbers() {
        let lexer = lexer_with(&spec());
        let (tokens, diags) = lex(&lexer, "1_000 3.25e-4 0xFF 0b1012 1.x 1..2 2e 7px 0x");
        assert_eq!(
            tokens,
            [
                "NUMBER:1_000",
                "WHITESPACE: ",
                "NUMBER:3.25e-4",
                "WHITESPACE: ",
                "NUMBER:0xFF",
                "WHITESPACE: ",
                "NUMBER:0b1012",
                "WHITESPACE: ",
                "NUMBER:1",
                "UNKNOWN:.",
                "IDENT:x",
                "WHITESPACE: ",
                "NUMBER:1",
                "UNKNOWN:..",
                "NUMBER:2",
                "WHITESPACE: ",
                "NUMBER:2e",
                "WHITESPACE: ",
                "NUMBER:7px",
                "WHITESPACE: ",
                "NUMBER:0x"
            ]
        );
        assert_eq!(
            diags,
            [
                "invalid digit `2` in binary literal",
                "unexpected character `.`",
                "unexpected characters `..`",
                "invalid suffix `e` on number literal",
                "invalid suffix `px` on number literal",
                "hexadecimal literal has no digits"
            ]
        );
    }

    #[test]
    fn test_lex_unterminated_forms_are_reported() {
        let lexer = lexer_with(&spec());
        let (tokens, diags) = lex(&lexer, "\"open\nx /* never");
        assert_eq!(
            tokens,
            [
                "STRING:\"open",
                "WHITESPACE:\n",
                "IDENT:x",
                "WHITESPACE: ",
                "COMMENT:/* never"
            ]
        );
        assert_eq!(diags, ["unterminated string", "unterminated block comment"]);
    }

    #[test]
    fn test_lex_unknown_runs_merge() {
        let lexer = lexer_with(&spec());
        let (tokens, diags) = lex(&lexer, "a @#$ b\u{a0}c");
        assert_eq!(
            tokens,
            [
                "IDENT:a",
                "WHITESPACE: ",
                "UNKNOWN:@#$",
                "WHITESPACE: ",
                "IDENT:b",
                "UNKNOWN:\u{a0}",
                "IDENT:c"
            ]
        );
        assert_eq!(
            diags,
            ["unexpected characters `@#$`", "unexpected character U+00A0"]
        );
    }

    #[test]
    fn test_lex_significant_newlines() {
        let mut s = spec();
        s.newlines = true;
        let lexer = lexer_with(&s);
        let (tokens, _) = lex(&lexer, "a \r\n\nb\r c");
        assert_eq!(
            tokens,
            [
                "IDENT:a",
                "WHITESPACE: ",
                "NEWLINE:\r\n",
                "NEWLINE:\n",
                "IDENT:b",
                "WHITESPACE:\r ",
                "IDENT:c"
            ]
        );
        assert!(!syntax_lang::TokenKind::is_trivia(&lexer.builtin(NEWLINE)));
    }

    #[test]
    fn test_lex_leading_byte_order_mark_is_whitespace() {
        let lexer = lexer_with(&spec());
        let (tokens, diags) = lex(&lexer, "\u{FEFF}  let x");
        assert!(diags.is_empty());
        assert_eq!(
            tokens,
            ["WHITESPACE:\u{FEFF}  ", "#7:let", "WHITESPACE: ", "IDENT:x"]
        );
        let (tokens, diags) = lex(&lexer, "\u{FEFF}");
        assert!(diags.is_empty());
        assert_eq!(tokens, ["WHITESPACE:\u{FEFF}"]);
        // Significant line breaks stay their own tokens.
        let mut s = spec();
        s.newlines = true;
        let lexer = lexer_with(&s);
        let (tokens, _) = lex(&lexer, "\u{FEFF} \nx");
        assert_eq!(tokens, ["WHITESPACE:\u{FEFF} ", "NEWLINE:\n", "IDENT:x"]);
        // Anywhere else it is an unknown character.
        let (tokens, diags) = lex(&lexer, "x\u{FEFF}");
        assert_eq!(tokens, ["IDENT:x", "UNKNOWN:\u{FEFF}"]);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn test_lex_nested_block_comments() {
        let mut s = spec();
        s.nested_comments = true;
        let lexer = lexer_with(&s);
        let (tokens, diags) = lex(&lexer, "/* a /* b */ c */d");
        assert!(diags.is_empty());
        assert_eq!(tokens, ["COMMENT:/* a /* b */ c */", "IDENT:d"]);
    }

    #[test]
    fn test_lex_raw_multiline_string() {
        let mut s = spec();
        s.strings = alloc::vec![StringSpec {
            open: Cow::Borrowed("r\""),
            close: Cow::Borrowed("\""),
            escape: None,
            multiline: true,
            span: Span::new(0, 1),
        }];
        let mut report = Report::default();
        let _ = Lexer::build(&s, &[], &mut report);
        assert_eq!(
            report.into_error("").diagnostics()[0].message(),
            "string delimiter `r\"` starts like an identifier, which the lexer reads first"
        );
        s.strings[0].open = Cow::Borrowed("#\"");
        let lexer = lexer_with(&s);
        let (tokens, diags) = lex(&lexer, "#\"a\\\nb\"x");
        assert!(diags.is_empty());
        assert_eq!(tokens, ["STRING:#\"a\\\nb\"", "IDENT:x"]);
    }

    #[test]
    fn test_build_reports_duplicate_texts() {
        let mut s = spec();
        s.line_comments.push((Cow::Borrowed("/*"), Span::new(5, 6)));
        let mut report = Report::default();
        let _ = Lexer::build(&s, &[], &mut report);
        let err = report.into_error("");
        assert_eq!(
            err.diagnostics()[0].message(),
            "`/*` is used for two different things"
        );
    }

    #[test]
    fn test_keywords_lookup() {
        let k = Keywords::new(alloc::vec![
            ("if".into(), Kind::new(1, false)),
            ("else".into(), Kind::new(2, false)),
            ("while".into(), Kind::new(3, false)),
        ]);
        assert_eq!(k.get("else"), Some(Kind::new(2, false)));
        assert_eq!(k.get("whilst"), None);
        assert_eq!(k.get("i"), None);
        assert_eq!(Keywords::default().get("if"), None);
    }

    #[test]
    fn test_is_ident() {
        assert!(is_ident("_x1", IdentMode::Ascii));
        assert!(is_ident("über", IdentMode::Xid));
        assert!(!is_ident("über", IdentMode::Ascii));
        assert!(!is_ident("1x", IdentMode::Xid));
        assert!(!is_ident("", IdentMode::Xid));
        assert!(!is_ident("a-b", IdentMode::Xid));
    }
}
