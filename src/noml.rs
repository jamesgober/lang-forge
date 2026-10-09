//! The schematic reader: the static, TOML-compatible core of NOML.
//!
//! A `.lsf` schematic is a NOML document, but only the part of NOML that reads
//! the same on every machine: tables, dotted keys, strings, booleans, numbers,
//! arrays, and inline tables. NOML's dynamic features — `env(...)` and other
//! function calls, `@native` types, includes — would make a language depend on
//! the environment it was forged in, so they are rejected with a message that
//! says so.
//!
//! The reader keeps a span for every key and value, which is what lets the
//! later stages point at the exact rule, literal, or setting at fault. Strings
//! borrow from the schematic unless an escape forces a copy, and such strings
//! remember that their offsets no longer map one-to-one onto the source.
//!
//! Inline tables may span lines and arrays and inline tables may end with a
//! trailing comma, as NOML allows; everything else follows TOML, including
//! its rules for what is invalid: a table defined by dotted keys cannot be
//! defined again by a `[header]`, keys cannot be multi-line strings, and a
//! number, date, or time must be well formed even though no schematic
//! setting takes one. A leading byte-order mark is skipped.

use alloc::{borrow::Cow, collections::BTreeMap, format, string::String, vec::Vec};

use diag_lang::{Diagnostic, Label, Severity};
use syntax_lang::Span;

/// How deeply a schematic may nest, counting every level together: each part
/// of a `[header]` or dotted key, each array, and each inline table. Real
/// schematics nest four or five levels (`[rules.expr]`, `levels`, a level
/// table, its operator array), and nothing deeper than that can be a valid
/// schematic; the limit keeps hostile input from exhausting the stack, both
/// while reading and when the document is dropped.
const MAX_NESTING: u32 = 64;

/// The largest schematic accepted, in bytes. Real schematics are a few
/// kilobytes; machine-generated ones with tens of thousands of literals are
/// well under a megabyte. The limit bounds everything forging does in
/// proportion to the input (a worst-case schematic of this size needs about
/// 150 MB before its tables are even sized).
pub(crate) const MAX_SCHEMATIC: usize = 8 << 20;

/// A table: its entries in source order.
#[derive(Debug)]
pub(crate) struct Table<'s> {
    pub(crate) entries: Vec<Entry<'s>>,
    /// Entry indexes by key. Every insertion checks its key for a duplicate,
    /// so a scan of `entries` there made reading a table quadratic.
    index: BTreeMap<Cow<'s, str>, usize>,
    /// The table header or inline table, or the key that implied the table.
    pub(crate) span: Span,
    /// Defined by a `[header]` (so a second header is a duplicate).
    explicit: bool,
    /// An inline table, which is complete as written and cannot be extended.
    sealed: bool,
    /// Defined by a dotted key (`a.b = 1` defines `a`), so a `[header]` may
    /// not define it again.
    dotted: bool,
}

impl Default for Table<'_> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: BTreeMap::new(),
            span: Span::empty(0),
            explicit: false,
            sealed: false,
            dotted: false,
        }
    }
}

impl<'s> Table<'s> {
    /// The index of the entry called `key`.
    fn find(&self, key: &str) -> Option<usize> {
        self.index.get(key).copied()
    }

    /// Appends `entry`, whose key must be new; returns its index.
    fn push(&mut self, entry: Entry<'s>) -> usize {
        let at = self.entries.len();
        let _ = self.index.insert(entry.key.clone(), at);
        self.entries.push(entry);
        at
    }
}

/// One `key = value` pair, or a sub-table under its key.
#[derive(Debug)]
pub(crate) struct Entry<'s> {
    pub(crate) key: Cow<'s, str>,
    pub(crate) key_span: Span,
    pub(crate) value: Value<'s>,
}

/// A value and the span of source it was read from.
#[derive(Debug)]
pub(crate) struct Value<'s> {
    pub(crate) kind: ValueKind<'s>,
    pub(crate) span: Span,
}

/// The shape of a value.
#[derive(Debug)]
pub(crate) enum ValueKind<'s> {
    Str(Text<'s>),
    Bool(bool),
    /// A number, checked but not kept: no schematic setting takes one, so it
    /// is only ever reported as the wrong type.
    Number,
    /// A TOML date, time, or date-time; like a number, never a valid setting.
    DateTime,
    Array(Vec<Value<'s>>),
    Table(Table<'s>),
}

/// A string value.
#[derive(Debug)]
pub(crate) struct Text<'s> {
    pub(crate) text: Cow<'s, str>,
    /// Byte offset of the first content character in the schematic.
    pub(crate) start: u32,
    /// Whether `text` is exactly the source at `start`, so an offset into
    /// `text` plus `start` is an offset into the schematic.
    pub(crate) exact: bool,
}

impl Text<'_> {
    /// The schematic span of `text[from..to]`, or `whole` when escapes broke
    /// the correspondence between the two.
    pub(crate) fn span(&self, from: usize, to: usize, whole: Span) -> Span {
        if self.exact {
            Span::new(self.start + from as u32, self.start + to as u32)
        } else {
            whole
        }
    }
}

impl Value<'_> {
    /// A short description of the value's type, for error messages.
    pub(crate) fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }
}

impl ValueKind<'_> {
    /// A short description of the type, for error messages.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            ValueKind::Str(_) => "a string",
            ValueKind::Bool(_) => "a boolean",
            ValueKind::Number => "a number",
            ValueKind::DateTime => "a date or time",
            ValueKind::Array(_) => "an array",
            ValueKind::Table(_) => "a table",
        }
    }
}

/// Reads a schematic into its root table.
pub(crate) fn read(text: &str) -> Result<Table<'_>, Diagnostic> {
    if text.len() > MAX_SCHEMATIC {
        return Err(error(
            Span::empty(0),
            format!("the schematic is larger than {} MiB", MAX_SCHEMATIC >> 20),
        ));
    }
    let mut reader = Reader {
        src: text,
        bytes: text.as_bytes(),
        // A byte-order mark is an encoding signature, not content. Spans stay
        // offsets into `text`, mark included.
        pos: if text.starts_with('\u{FEFF}') { 3 } else { 0 },
        depth: 0,
    };
    reader.document()
}

fn error(span: Span, message: impl Into<alloc::boxed::Box<str>>) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, Label::unlabelled(span))
}

struct Reader<'s> {
    src: &'s str,
    bytes: &'s [u8],
    pos: usize,
    /// The nesting level of the table or array being read: the number of
    /// header parts, dotted-key parts, arrays, and inline tables around the
    /// current position. Never more than `MAX_NESTING`.
    depth: u32,
}

type Result<T, E = Diagnostic> = core::result::Result<T, E>;

/// A dotted key: each part and its span.
type Keys<'s> = Vec<(Cow<'s, str>, Span)>;

impl<'s> Reader<'s> {
    fn document(&mut self) -> Result<Table<'s>> {
        let mut root = Table {
            explicit: true,
            ..Table::default()
        };
        // The index path from the root to the table that `key = value` lines
        // currently fill.
        let mut current: Vec<usize> = Vec::new();
        loop {
            self.skip_blank_lines();
            match self.peek() {
                None => return Ok(root),
                Some(b'[') => {
                    current = self.header(&mut root)?;
                    // A header has at most `MAX_NESTING` parts.
                    self.depth = current.len() as u32;
                }
                Some(_) => {
                    let (keys, value) = self.key_value()?;
                    insert_at(&mut root, &current, keys, value)?;
                    self.end_of_line()?;
                }
            }
        }
    }

    /// `[a.b]` — opens (or re-opens an implied) table.
    fn header(&mut self, root: &mut Table<'s>) -> Result<Vec<usize>> {
        let open = self.pos;
        self.pos += 1;
        if self.peek() == Some(b'[') {
            return Err(error(
                self.span_from(open),
                "arrays of tables (`[[...]]`) are not used in a schematic",
            ));
        }
        self.skip_spaces();
        let keys = self.key_path()?;
        self.skip_spaces();
        if self.peek() != Some(b']') {
            return Err(self.unexpected("`]` to close the table header"));
        }
        self.pos += 1;
        let span = self.span_from(open);
        self.end_of_line()?;

        let count = keys.len();
        let mut path = Vec::with_capacity(count);
        let mut table = root;
        for (i, (key, key_span)) in keys.into_iter().enumerate() {
            let last = i + 1 == count;
            let index = match table.find(&key) {
                Some(index) => index,
                None => table.push(Entry {
                    key,
                    key_span,
                    value: Value {
                        kind: ValueKind::Table(Table {
                            span,
                            ..Table::default()
                        }),
                        span,
                    },
                }),
            };
            let entry = &mut table.entries[index];
            let ValueKind::Table(child) = &mut entry.value.kind else {
                return Err(error(
                    key_span,
                    format!("`{}` is already defined as a value", entry.key),
                ));
            };
            if child.sealed {
                return Err(error(
                    key_span,
                    format!("`{}` is an inline table and cannot be extended", entry.key),
                ));
            }
            if last {
                let name = self.src[open + 1..span.end().to_usize() - 1].trim();
                if child.explicit {
                    return Err(error(span, format!("table [{name}] is defined twice")));
                }
                // TOML: dotted keys define their tables; a header may add
                // sub-tables to one, but not define it again.
                if child.dotted {
                    return Err(error(
                        span,
                        format!("table [{name}] is already defined by dotted keys"),
                    ));
                }
                child.explicit = true;
                child.span = span;
            }
            path.push(index);
            table = child;
        }
        Ok(path)
    }

    /// `a.b = value`.
    fn key_value(&mut self) -> Result<(Keys<'s>, Value<'s>)> {
        let keys = self.key_path()?;
        // Every part but the last opens a table one level deeper, and the
        // value, if it is an array or inline table, is one level below that:
        // the levels add up with those around the key, not each on its own.
        let outer = self.depth;
        let inner = outer + keys.len() as u32 - 1;
        if inner > MAX_NESTING {
            let (_, last) = keys[keys.len() - 1];
            return Err(too_deep(last));
        }
        self.skip_spaces();
        if self.peek() != Some(b'=') {
            return Err(self.unexpected("`=` after the key"));
        }
        self.pos += 1;
        self.skip_spaces();
        self.depth = inner;
        let value = self.value();
        self.depth = outer;
        Ok((keys, value?))
    }

    /// One or more keys joined by dots.
    fn key_path(&mut self) -> Result<Keys<'s>> {
        let mut keys = Vec::new();
        loop {
            let key = self.key()?;
            // Every part is a level of table nesting; like arrays and inline
            // tables, a dotted key or header may not nest without limit.
            if keys.len() >= MAX_NESTING as usize {
                return Err(error(
                    key.1,
                    format!("a dotted key has more than {MAX_NESTING} parts"),
                ));
            }
            keys.push(key);
            self.skip_spaces();
            if self.peek() == Some(b'.') {
                self.pos += 1;
                self.skip_spaces();
            } else {
                return Ok(keys);
            }
        }
    }

    /// A bare key or a quoted key.
    fn key(&mut self) -> Result<(Cow<'s, str>, Span)> {
        let start = self.pos;
        match self.peek() {
            Some(quote @ (b'"' | b'\'')) => {
                if self.bytes[start..].starts_with(&[quote; 3]) {
                    return Err(error(
                        Span::new(start as u32, start as u32 + 3),
                        "a key cannot be a multi-line string",
                    ));
                }
                let text = self.string()?;
                Ok((text.text, self.span_from(start)))
            }
            Some(b) if is_bare_key(b) => {
                while self.peek().is_some_and(is_bare_key) {
                    self.pos += 1;
                }
                Ok((
                    Cow::Borrowed(&self.src[start..self.pos]),
                    self.span_from(start),
                ))
            }
            _ => Err(self.unexpected("a key")),
        }
    }

    fn value(&mut self) -> Result<Value<'s>> {
        let start = self.pos;
        let kind = match self.peek() {
            Some(b'"' | b'\'') => ValueKind::Str(self.string()?),
            Some(b'[') => ValueKind::Array(self.nested(Self::array)?),
            Some(b'{') => ValueKind::Table(self.nested(Self::inline_table)?),
            Some(b'@') => {
                return Err(error(
                    self.word_span(),
                    "NOML native types (`@...`) are not allowed in a schematic",
                ));
            }
            Some(b'0'..=b'9' | b'+' | b'-') => self.number()?,
            Some(b) if b.is_ascii_alphabetic() => {
                let span = self.word_span();
                let word = &self.src[span.start().to_usize()..span.end().to_usize()];
                self.pos = span.end().to_usize();
                match word {
                    "true" => ValueKind::Bool(true),
                    "false" => ValueKind::Bool(false),
                    "inf" | "nan" => ValueKind::Number,
                    _ if self.peek() == Some(b'(') => {
                        return Err(Diagnostic::new(
                            Severity::Error,
                            format!("NOML function calls such as `{word}(...)` are not allowed in a schematic"),
                            Label::unlabelled(span),
                        )
                        .with_note("a schematic must forge the same language on every machine"));
                    }
                    _ => {
                        return Err(error(span, format!("expected a value, found `{word}`")));
                    }
                }
            }
            _ => return Err(self.unexpected("a value")),
        };
        Ok(Value {
            kind,
            span: self.span_from(start),
        })
    }

    /// A number, date, or time, checked against TOML's forms: decimal
    /// integers and floats with `_` only between digits and no leading
    /// zeros; `0x`, `0o`, and `0b` integers; `inf` and `nan` with a sign;
    /// and RFC 3339 dates, times, and date-times (the time may follow the
    /// date after a space). Anything else — `1abc`, a lone `-`, `1__0`,
    /// `0123` — is refused rather than accepted as some number.
    fn number(&mut self) -> Result<ValueKind<'s>> {
        let start = self.pos;
        let scan = |bytes: &[u8], mut at: usize| {
            while bytes.get(at).is_some_and(|&b| {
                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-' | b':')
            }) {
                at += 1;
            }
            at
        };
        let mut end = scan(self.bytes, start);
        // `1979-05-27 07:32:00`: TOML lets a space stand for the `T`.
        if is_date(&self.src[start..end])
            && self.bytes.get(end) == Some(&b' ')
            && self.bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
            && self.bytes.get(end + 2).is_some_and(u8::is_ascii_digit)
            && self.bytes.get(end + 3) == Some(&b':')
        {
            end = scan(self.bytes, end + 1);
        }
        self.pos = end;
        let text = &self.src[start..end];
        if is_number(text) {
            return Ok(ValueKind::Number);
        }
        if is_date_time(text) {
            return Ok(ValueKind::DateTime);
        }
        // Shaped like a date (`YYYY-MM-...`) or a time: say which was meant.
        let b = text.as_bytes();
        let date_shaped = b.len() >= 8 && b[4] == b'-' && b[..4].iter().all(u8::is_ascii_digit);
        let what = if text.contains(':') || date_shaped {
            "date or time"
        } else {
            "number"
        };
        Err(error(
            self.span_from(start),
            format!("invalid {what} `{text}`"),
        ))
    }

    /// Runs `parse` one nesting level deeper, refusing to go past the limit.
    fn nested<T>(&mut self, parse: fn(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth >= MAX_NESTING {
            return Err(too_deep(Span::new(self.pos as u32, self.pos as u32 + 1)));
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn array(&mut self) -> Result<Vec<Value<'s>>> {
        self.pos += 1; // `[`
        let mut items = Vec::new();
        loop {
            self.skip_blank_lines();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(items);
            }
            items.push(self.value()?);
            self.skip_blank_lines();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {}
                _ => return Err(self.unexpected("`,` or `]` in the array")),
            }
        }
    }

    fn inline_table(&mut self) -> Result<Table<'s>> {
        let open = self.pos;
        self.pos += 1; // `{`
        let mut table = Table::default();
        loop {
            self.skip_blank_lines();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                table.span = self.span_from(open);
                table.sealed = true;
                return Ok(table);
            }
            let (keys, value) = self.key_value()?;
            insert(&mut table, keys, value)?;
            self.skip_blank_lines();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {}
                _ => return Err(self.unexpected("`,` or `}` in the inline table")),
            }
        }
    }

    /// Any of the four string forms.
    fn string(&mut self) -> Result<Text<'s>> {
        let open = self.pos;
        let quote = self.bytes[self.pos];
        let triple =
            self.bytes[self.pos..].starts_with(if quote == b'"' { b"\"\"\"" } else { b"'''" });
        if triple {
            self.pos += 3;
            // A newline right after the opening delimiter is trimmed.
            if self.bytes[self.pos..].starts_with(b"\r\n") {
                self.pos += 2;
            } else if self.peek() == Some(b'\n') {
                self.pos += 1;
            }
        } else {
            self.pos += 1;
        }
        let start = self.pos;
        let delimiter = Span::new(open as u32, (open + if triple { 3 } else { 1 }) as u32);
        let unterminated = || error(delimiter, "unterminated string");
        let mut owned: Option<String> = None;
        let mut run = start; // start of the not-yet-copied run
        loop {
            let at = self.pos;
            let Some(b) = self.peek() else {
                return Err(unterminated());
            };
            if b == quote {
                if !triple {
                    let text = finish(self.src, owned, run, at);
                    self.pos += 1;
                    return Ok(self.text(text, start));
                }
                if self.bytes[at..].starts_with(&[quote; 3]) {
                    // Up to two quotes may sit right before the closing three.
                    let extra = self.bytes[at + 3..]
                        .iter()
                        .take(2)
                        .take_while(|c| **c == quote)
                        .count();
                    let end = at + extra;
                    let text = finish(self.src, owned, run, end);
                    self.pos = end + 3;
                    return Ok(self.text(text, start));
                }
                self.pos += 1;
                continue;
            }
            match b {
                b'\\' if quote == b'"' => {
                    let buf = owned.get_or_insert_with(String::new);
                    buf.push_str(&self.src[run..at]);
                    self.escape(buf, triple, delimiter)?;
                    run = self.pos;
                }
                b'\n' if !triple => return Err(unterminated()),
                b'\r' if !triple && self.bytes.get(at + 1) == Some(&b'\n') => {
                    return Err(unterminated());
                }
                b'\r' if triple && self.bytes.get(at + 1) == Some(&b'\n') => self.pos += 2,
                b'\t' => self.pos += 1,
                b'\n' => self.pos += 1,
                0..=0x1f | 0x7f => {
                    return Err(error(
                        Span::new(at as u32, at as u32 + 1),
                        "control characters must be escaped in a string",
                    ));
                }
                _ => self.pos += 1,
            }
        }
    }

    fn text(&self, text: (Cow<'s, str>, bool), start: usize) -> Text<'s> {
        Text {
            text: text.0,
            start: start as u32,
            exact: text.1,
        }
    }

    /// Decodes the escape at `self.pos` into `buf`.
    fn escape(&mut self, buf: &mut String, multiline: bool, delimiter: Span) -> Result<()> {
        let at = self.pos;
        self.pos += 1; // `\`
        let Some(code) = self.peek() else {
            return Err(error(delimiter, "unterminated string"));
        };
        self.pos += 1;
        let ch = match code {
            b'b' => '\u{8}',
            b't' => '\t',
            b'n' => '\n',
            b'f' => '\u{c}',
            b'r' => '\r',
            b'e' => '\u{1b}',
            b'"' => '"',
            b'\\' => '\\',
            b'u' | b'U' => {
                let len = if code == b'u' { 4 } else { 8 };
                let digits = self.src.get(self.pos..self.pos + len).unwrap_or("");
                let ch = (digits.len() == len && digits.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| u32::from_str_radix(digits, 16).ok())
                    .flatten()
                    .and_then(char::from_u32);
                let Some(ch) = ch else {
                    return Err(error(
                        Span::new(at as u32, (self.pos + digits.len()) as u32),
                        "invalid Unicode escape",
                    ));
                };
                self.pos += len;
                ch
            }
            b' ' | b'\t' | b'\r' | b'\n' if multiline => {
                // A line-ending backslash swallows the line break and any
                // whitespace that follows it.
                self.pos -= 1;
                let rest = &self.bytes[self.pos..];
                let line_end = rest.iter().position(|b| *b == b'\n');
                let only_space = line_end.is_some_and(|end| {
                    rest[..end]
                        .iter()
                        .all(|b| matches!(b, b' ' | b'\t' | b'\r'))
                });
                if !only_space {
                    return Err(error(self.span_from(at), "invalid escape"));
                }
                while self
                    .peek()
                    .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
                {
                    self.pos += 1;
                }
                return Ok(());
            }
            _ => {
                // Span the whole escaped character, which may be multi-byte.
                let width = self.src[at + 1..].chars().next().map_or(1, char::len_utf8);
                return Err(error(
                    Span::new(at as u32, (at + 1 + width) as u32),
                    "invalid escape",
                ));
            }
        };
        buf.push(ch);
        Ok(())
    }

    /// Skips spaces and tabs.
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    /// Skips whitespace, line breaks, and comments.
    fn skip_blank_lines(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'#') => self.skip_comment(),
                _ => return,
            }
        }
    }

    fn skip_comment(&mut self) {
        while self.peek().is_some_and(|b| b != b'\n') {
            self.pos += 1;
        }
    }

    /// Requires the rest of the line to be blank or a comment.
    fn end_of_line(&mut self) -> Result<()> {
        self.skip_spaces();
        match self.peek() {
            None | Some(b'\n') => Ok(()),
            Some(b'\r') if self.bytes.get(self.pos + 1) == Some(&b'\n') => Ok(()),
            Some(b'#') => {
                self.skip_comment();
                Ok(())
            }
            _ => Err(self.unexpected("the end of the line")),
        }
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(start as u32, self.pos as u32)
    }

    /// The span of the run of word characters at the current position.
    fn word_span(&self) -> Span {
        let mut end = self.pos + 1;
        while self.bytes.get(end).is_some_and(|b| is_bare_key(*b)) {
            end += 1;
        }
        Span::new(self.pos as u32, end.min(self.bytes.len()) as u32)
    }

    /// "expected X, found Y" at the current position.
    fn unexpected(&self, expected: &str) -> Diagnostic {
        let found = match self.src[self.pos..].chars().next() {
            None => {
                return error(
                    Span::empty(self.pos as u32),
                    format!("expected {expected}, found the end of the schematic"),
                );
            }
            Some('\n' | '\r') => String::from("the end of the line"),
            Some(c) if c.is_control() => format!("`{}`", c.escape_debug()),
            Some(c) => format!("`{c}`"),
        };
        let width = self.src[self.pos..]
            .chars()
            .next()
            .map_or(0, char::len_utf8);
        error(
            Span::new(self.pos as u32, (self.pos + width) as u32),
            format!("expected {expected}, found {found}"),
        )
    }
}

/// Completes a string: borrowed when nothing was copied, owned otherwise.
fn finish(src: &str, owned: Option<String>, run: usize, end: usize) -> (Cow<'_, str>, bool) {
    match owned {
        None => (Cow::Borrowed(&src[run..end]), true),
        Some(mut buf) => {
            buf.push_str(&src[run..end]);
            (Cow::Owned(buf), false)
        }
    }
}

fn is_bare_key(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn too_deep(span: Span) -> Diagnostic {
    error(
        span,
        format!("the schematic nests more than {MAX_NESTING} levels deep"),
    )
}

/// Whether `text` is one or more digits of `radix`, with each `_` between
/// two digits.
fn is_digits(text: &str, radix: u32) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.iter().enumerate().all(|(i, &b)| {
            if b == b'_' {
                // Neither first nor last, and between two digits.
                i > 0 && i + 1 < bytes.len() && bytes[i - 1] != b'_' && bytes[i + 1] != b'_'
            } else {
                char::from(b).is_digit(radix)
            }
        })
}

/// Whether `text` is a TOML integer or float.
fn is_number(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']);
    let body = unsigned.unwrap_or(text);
    if matches!(body, "inf" | "nan") {
        return true;
    }
    // Radix prefixes take no sign.
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = body.strip_prefix(prefix) {
            return unsigned.is_none() && is_digits(digits, radix);
        }
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let (int, fraction) = match mantissa.split_once('.') {
        Some((int, fraction)) => (int, Some(fraction)),
        None => (mantissa, None),
    };
    // The integer part has no leading zeros; the exponent may.
    is_digits(int, 10)
        && (int == "0" || !int.starts_with('0'))
        && fraction.is_none_or(|f| is_digits(f, 10))
        && exponent.is_none_or(|e| is_digits(e.strip_prefix(['+', '-']).unwrap_or(e), 10))
}

/// Whether `text` is a TOML local date, `YYYY-MM-DD`, of a real day.
fn is_date(text: &str) -> bool {
    let b = text.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let (Some(year), Some(month), Some(day)) =
        (number_at(b, 0, 4), number_at(b, 5, 2), number_at(b, 8, 2))
    else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return false,
    };
    (1..=days).contains(&day)
}

/// Whether `text` is a TOML time (`HH:MM`, with optional `:SS` and
/// fraction), optionally followed by a UTC offset when `offset` allows one.
fn is_time(text: &str, offset: bool) -> bool {
    let b = text.as_bytes();
    let (Some(hour), Some(minute)) = (number_at(b, 0, 2), number_at(b, 3, 2)) else {
        return false;
    };
    if b.len() < 5 || b[2] != b':' || hour > 23 || minute > 59 {
        return false;
    }
    let mut at = 5;
    if b.get(at) == Some(&b':') {
        match number_at(b, at + 1, 2) {
            // 60 is a leap second.
            Some(second) if second <= 60 => at += 3,
            _ => return false,
        }
        if b.get(at) == Some(&b'.') {
            let digits = b[at + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            if digits == 0 {
                return false;
            }
            at += 1 + digits;
        }
    }
    let rest = &text[at..];
    if rest.is_empty() {
        return true;
    }
    if !offset {
        return false;
    }
    if matches!(rest, "Z" | "z") {
        return true;
    }
    let o = rest.as_bytes();
    o.len() == 6
        && matches!(o[0], b'+' | b'-')
        && o[3] == b':'
        && number_at(o, 1, 2).is_some_and(|h| h <= 23)
        && number_at(o, 4, 2).is_some_and(|m| m <= 59)
}

/// Whether `text` is a TOML date, time, or date-time (`T`, `t`, or a space
/// between the date and the time).
fn is_date_time(text: &str) -> bool {
    if is_date(text) {
        return true;
    }
    if text.len() > 11 && is_date(&text[..10]) && matches!(text.as_bytes()[10], b'T' | b't' | b' ')
    {
        return is_time(&text[11..], true);
    }
    is_time(text, false)
}

/// The decimal number in `b[at..at + len]`, if those are all digits.
fn number_at(b: &[u8], at: usize, len: usize) -> Option<u32> {
    let digits = b.get(at..at + len)?;
    digits.iter().try_fold(0u32, |n, &d| {
        d.is_ascii_digit().then(|| n * 10 + u32::from(d - b'0'))
    })
}

/// Inserts `value` at the dotted `keys` below the table `path` leads to.
fn insert_at<'s>(
    table: &mut Table<'s>,
    path: &[usize],
    keys: Keys<'s>,
    value: Value<'s>,
) -> Result<()> {
    match path.split_first() {
        None => insert(table, keys, value),
        Some((&index, rest)) => match &mut table.entries[index].value.kind {
            ValueKind::Table(child) => insert_at(child, rest, keys, value),
            // Paths only ever lead through tables: `header` checked each step.
            _ => insert(table, keys, value),
        },
    }
}

/// Inserts `value` at the dotted `keys` below `table`.
fn insert<'s>(table: &mut Table<'s>, keys: Keys<'s>, value: Value<'s>) -> Result<()> {
    let mut table = table;
    let count = keys.len();
    for (i, (key, key_span)) in keys.into_iter().enumerate() {
        let existing = table.find(&key);
        if i + 1 == count {
            if let Some(index) = existing {
                let first = table.entries[index].key_span;
                return Err(Diagnostic::new(
                    Severity::Error,
                    format!("`{key}` is defined twice"),
                    Label::new(key_span, "defined again here"),
                )
                .with_secondary(Label::new(first, "first defined here")));
            }
            let _ = table.push(Entry {
                key,
                key_span,
                value,
            });
            return Ok(());
        }
        let index = match existing {
            Some(index) => index,
            None => table.push(Entry {
                key,
                key_span,
                value: Value {
                    kind: ValueKind::Table(Table {
                        span: key_span,
                        ..Table::default()
                    }),
                    span: key_span,
                },
            }),
        };
        let entry = &mut table.entries[index];
        match &mut entry.value.kind {
            ValueKind::Table(child) if !child.sealed && !child.explicit => {
                child.dotted = true;
                table = child;
            }
            _ => {
                return Err(error(
                    key_span,
                    format!(
                        "`{}` is already defined and cannot be extended with a dotted key",
                        entry.key
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn str_of<'a>(table: &'a Table<'_>, key: &str) -> &'a str {
        let entry = table.entries.iter().find(|e| e.key == key).unwrap();
        match &entry.value.kind {
            ValueKind::Str(t) => &t.text,
            other => panic!("not a string: {other:?}"),
        }
    }

    fn table_of<'a, 's>(table: &'a Table<'s>, key: &str) -> &'a Table<'s> {
        let entry = table.entries.iter().find(|e| e.key == key).unwrap();
        match &entry.value.kind {
            ValueKind::Table(t) => t,
            other => panic!("not a table: {other:?}"),
        }
    }

    fn message(text: &str) -> String {
        String::from(read(text).unwrap_err().message())
    }

    #[test]
    fn test_read_tables_keys_and_strings() {
        let doc = read(
            "# comment\n[language]\nname = \"calc\" # trailing\n'quoted key' = 'lit'\n\n[rules]\na.b = \"x\"\n",
        )
        .unwrap();
        let language = table_of(&doc, "language");
        assert_eq!(str_of(language, "name"), "calc");
        assert_eq!(str_of(language, "quoted key"), "lit");
        let rules = table_of(&doc, "rules");
        assert_eq!(str_of(table_of(rules, "a"), "b"), "x");
    }

    #[test]
    fn test_read_dotted_header_after_implied_table() {
        let doc = read("[rules]\nx = \"a\"\n[rules.expr]\noperand = \"b\"\n").unwrap();
        let rules = table_of(&doc, "rules");
        assert_eq!(str_of(table_of(rules, "expr"), "operand"), "b");
        let names: Vec<&str> = rules.entries.iter().map(|e| &*e.key).collect();
        assert_eq!(names, ["x", "expr"]);
    }

    #[test]
    fn test_read_string_escapes_and_exactness() {
        let doc = read("a = \"x\\ty\\u00e9\"\nb = \"plain\"\n").unwrap();
        let a = &doc.entries[0].value.kind;
        let ValueKind::Str(a) = a else { panic!() };
        assert_eq!(a.text, "x\ty\u{e9}");
        assert!(!a.exact);
        let ValueKind::Str(b) = &doc.entries[1].value.kind else {
            panic!()
        };
        assert!(b.exact);
        assert_eq!(b.start, 22);
        assert!(matches!(b.text, Cow::Borrowed(_)));
    }

    #[test]
    fn test_read_multiline_strings() {
        let doc = read("a = \"\"\"\nline one\nline two\"\"\"\nb = '''\n'quoted' \\n'''\nc = \"\"\"x \\\n   y\"\"\"\n").unwrap();
        assert_eq!(str_of(&doc, "a"), "line one\nline two");
        assert_eq!(str_of(&doc, "b"), "'quoted' \\n");
        assert_eq!(str_of(&doc, "c"), "x y");
    }

    #[test]
    fn test_read_multiline_allows_quotes_before_close() {
        let doc = read("a = \"\"\"say \"hi\"\"\"\"\n").unwrap();
        assert_eq!(str_of(&doc, "a"), "say \"hi\"");
    }

    #[test]
    fn test_read_arrays_and_inline_tables_span_lines() {
        let doc =
            read("a = [\n  \"x\", # one\n  [\"y\", \"z\"],\n]\nb = { k = true,\n  j = false, }\n")
                .unwrap();
        let ValueKind::Array(items) = &doc.entries[0].value.kind else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        let b = table_of(&doc, "b");
        assert!(matches!(b.entries[0].value.kind, ValueKind::Bool(true)));
        assert!(matches!(b.entries[1].value.kind, ValueKind::Bool(false)));
    }

    #[test]
    fn test_read_numbers_are_recognized() {
        let doc = read("a = 1_000\nb = -2.5e3\n").unwrap();
        assert!(matches!(doc.entries[0].value.kind, ValueKind::Number));
        assert_eq!(doc.entries[1].value.type_name(), "a number");
    }

    #[test]
    fn test_read_crlf_line_endings() {
        let doc = read("[language]\r\nname = \"x\"\r\n").unwrap();
        assert_eq!(str_of(table_of(&doc, "language"), "name"), "x");
    }

    #[test]
    fn test_read_rejects_duplicates() {
        assert_eq!(message("a = \"x\"\na = \"y\"\n"), "`a` is defined twice");
        assert_eq!(message("[t]\n[t]\n"), "table [t] is defined twice");
        assert_eq!(
            message("a = \"x\"\n[a]\n"),
            "`a` is already defined as a value"
        );
        assert_eq!(
            message("a = { b = \"x\" }\n[a]\n"),
            "`a` is an inline table and cannot be extended"
        );
        assert_eq!(
            message("a = { b = \"x\" }\na.c = \"y\"\n"),
            "`a` is already defined and cannot be extended with a dotted key"
        );
    }

    #[test]
    fn test_read_rejects_dynamic_noml() {
        assert_eq!(
            message("a = env(\"HOME\")\n"),
            "NOML function calls such as `env(...)` are not allowed in a schematic"
        );
        assert_eq!(
            message("a = @size(\"1kb\")\n"),
            "NOML native types (`@...`) are not allowed in a schematic"
        );
        assert_eq!(
            message("[[t]]\n"),
            "arrays of tables (`[[...]]`) are not used in a schematic"
        );
    }

    #[test]
    fn test_read_reports_syntax_errors() {
        assert_eq!(
            message("a \"x\"\n"),
            "expected `=` after the key, found `\"`"
        );
        assert_eq!(
            message("a = \"x\" b\n"),
            "expected the end of the line, found `b`"
        );
        assert_eq!(message("a = \"x\n"), "unterminated string");
        assert_eq!(message("a = \"x"), "unterminated string");
        assert_eq!(message("a = nope\n"), "expected a value, found `nope`");
        assert_eq!(message("a = \"\\q\"\n"), "invalid escape");
        assert_eq!(message("a = \"\\u12\"\n"), "invalid Unicode escape");
        assert_eq!(
            message("a = \"\u{1}\"\n"),
            "control characters must be escaped in a string"
        );
        assert_eq!(
            message("[t\n"),
            "expected `]` to close the table header, found the end of the line"
        );
        assert_eq!(
            message("a = [\"x\" \"y\"]\n"),
            "expected `,` or `]` in the array, found `\"`"
        );
        assert_eq!(
            message("a ="),
            "expected a value, found the end of the schematic"
        );
    }

    #[test]
    fn test_read_invalid_escape_spans_whole_character() {
        let text = "a = \"\\\u{e9}\"\n";
        let err = read(text).unwrap_err();
        assert_eq!(err.message(), "invalid escape");
        let span = err.primary().span();
        assert_eq!(
            &text[span.start().to_usize()..span.end().to_usize()],
            "\\\u{e9}"
        );
    }

    #[test]
    fn test_read_limits_dotted_keys() {
        let long = format!("{} = 1\n", ["k"; 100].join("."));
        assert_eq!(message(&long), "a dotted key has more than 64 parts");
        let header = format!("[{}]\n", ["k"; 100].join("."));
        assert_eq!(message(&header), "a dotted key has more than 64 parts");
        let ok = format!("{} = 1\n", ["k"; 64].join("."));
        assert!(read(&ok).is_ok());
    }

    #[test]
    fn test_read_limits_nesting() {
        let deep = format!("a = {}{}\n", "[".repeat(100), "]".repeat(100));
        assert_eq!(
            message(&deep),
            "the schematic nests more than 64 levels deep"
        );
        let ok = format!("a = {}{}\n", "[".repeat(60), "]".repeat(60));
        assert!(read(&ok).is_ok());
        let ok = format!("a = {}{}\n", "[".repeat(64), "]".repeat(64));
        assert!(read(&ok).is_ok());
    }

    #[test]
    fn test_read_limits_total_nesting_across_keys_and_values() {
        // Each limit on its own allowed 64 levels; together they nested
        // 64 inline tables of 64-part dotted keys, a 4096-deep table tree.
        let key = ["k"; 64].join(".");
        let mut bomb = String::from("1");
        for _ in 0..64 {
            bomb = format!("{{ {key} = {bomb} }}");
        }
        assert_eq!(
            message(&format!("a = {bomb}\n")),
            "the schematic nests more than 64 levels deep"
        );
        // Header parts, dotted parts, and values add up.
        let half = ["k"; 32].join(".");
        assert!(read(&format!("[{half}]\n{half} = 1\n")).is_ok());
        assert!(read(&format!("[{half}]\n{half}.x = 1\n")).is_ok());
        assert_eq!(
            message(&format!("[{half}]\n{half}.x.y = 1\n")),
            "the schematic nests more than 64 levels deep"
        );
        assert!(
            read(&format!(
                "[{half}]\nx = {}{}\n",
                "[".repeat(32),
                "]".repeat(32)
            ))
            .is_ok()
        );
        assert_eq!(
            message(&format!(
                "[{half}]\nx = {}{}\n",
                "[".repeat(33),
                "]".repeat(33)
            )),
            "the schematic nests more than 64 levels deep"
        );
        let inline = format!("x = {}1{}\n", "{ y = ".repeat(40), " }".repeat(40));
        assert!(read(&format!("[{half}]\n{inline}")).is_err());
        assert!(read(&inline).is_ok());
    }

    #[test]
    fn test_read_skips_a_leading_byte_order_mark() {
        let text = "\u{FEFF}[language]\nname = \"x\"\n";
        let doc = read(text).unwrap();
        let language = table_of(&doc, "language");
        assert_eq!(str_of(language, "name"), "x");
        // Spans stay offsets into the text, mark included.
        assert_eq!(doc.entries[0].key_span, Span::new(4, 12));
        // Anywhere else it is not whitespace.
        assert!(read("a = 1\n\u{FEFF}b = 2\n").is_err());
    }

    #[test]
    fn test_read_rejects_header_redefining_a_dotted_table() {
        assert_eq!(
            message("a.b.c = 1\n[a.b]\n"),
            "table [a.b] is already defined by dotted keys"
        );
        assert_eq!(
            message("a.b = 1\n[a]\n"),
            "table [a] is already defined by dotted keys"
        );
        assert_eq!(
            message("[t]\nx.y = 1\n[t.x]\n"),
            "table [t.x] is already defined by dotted keys"
        );
        // A header may still add a sub-table to one (TOML allows it).
        let doc = read("[f]\napple.color = \"red\"\n[f.apple.texture]\nsmooth = true\n").unwrap();
        let apple = table_of(table_of(&doc, "f"), "apple");
        assert!(matches!(
            table_of(apple, "texture").entries[0].value.kind,
            ValueKind::Bool(true)
        ));
        // And a header-implied table may be defined later.
        assert!(read("[a.b.c]\n[a]\nx = 1\n").is_ok());
    }

    #[test]
    fn test_read_rejects_multiline_string_keys() {
        assert_eq!(
            message("\"\"\"k\"\"\" = 1\n"),
            "a key cannot be a multi-line string"
        );
        assert_eq!(
            message("'''k''' = 1\n"),
            "a key cannot be a multi-line string"
        );
        assert_eq!(
            message("[\"\"\"t\"\"\"]\n"),
            "a key cannot be a multi-line string"
        );
        let doc = read("\"\" = 'x'\n'q' . 'b' = 'y'\n").unwrap();
        assert_eq!(str_of(&doc, ""), "x");
    }

    #[test]
    fn test_read_accepts_every_toml_number_date_and_time() {
        let numbers = [
            "0",
            "+99",
            "-17",
            "1_000",
            "5_349_221",
            "0xDEADBEEF",
            "0xdead_beef",
            "0o755",
            "0b1101_0110",
            "+1.0",
            "3.1415",
            "-0.01",
            "5e+22",
            "1e06",
            "-2E-2",
            "6.626e-34",
            "224_617.445_991",
            "inf",
            "+inf",
            "-inf",
            "nan",
            "+nan",
            "-nan",
            "-0",
            "0.0",
        ];
        for n in numbers {
            let text = format!("a = {n}\n");
            let doc = read(&text).unwrap_or_else(|e| panic!("{n}: {e:?}"));
            assert!(
                matches!(doc.entries[0].value.kind, ValueKind::Number),
                "{n}"
            );
        }
        let dates = [
            "1979-05-27T07:32:00Z",
            "1979-05-27T00:32:00-07:00",
            "1979-05-27T00:32:00.999999+07:00",
            "1979-05-27 07:32:00Z",
            "1979-05-27t07:32:00",
            "1979-05-27T07:32",
            "1979-05-27",
            "2024-02-29",
            "07:32:00",
            "00:32:00.999999",
            "07:32",
            "23:59:60",
        ];
        for d in dates {
            let text = format!("a = {d}\n");
            let doc = read(&text).unwrap_or_else(|e| panic!("{d}: {e:?}"));
            assert!(
                matches!(doc.entries[0].value.kind, ValueKind::DateTime),
                "{d}"
            );
        }
    }

    #[test]
    fn test_read_rejects_malformed_numbers_dates_and_times() {
        for n in [
            "1abc", "-", "+", "1__0", "1_", "0123", "+0x1", "0X1F", "0xG", "0x", "0b2", "1.",
            "+.5", "1e", "1e+", "1.e5", "1_.5", "1.5_", "--1", "1-2", "0x_1", "1e5.5", "-0b1",
        ] {
            assert_eq!(
                message(&format!("a = {n}\n")),
                format!("invalid number `{n}`"),
                "{n}"
            );
        }
        for d in [
            "1979-13-01",
            "2023-02-29",
            "1979-05-32",
            "24:00:00",
            "07:60",
            "07:32:61",
            "07:32:00Z",
            "1979-05-27T07",
            "1979-05-27T07:32:00+7:00",
            "1979-05-27T07:32:00.Z",
            "1979-05-27X07:32",
        ] {
            assert_eq!(
                message(&format!("a = {d}\n")),
                format!("invalid date or time `{d}`"),
                "{d}"
            );
        }
        // The error spans the whole malformed value.
        let err = read("a = 12ab\n").unwrap_err();
        assert_eq!(err.primary().span(), Span::new(4, 8));
    }

    #[test]
    fn test_read_duplicate_checks_scale_linearly() {
        // 200,000 keys in one table, and 200,000 distinct dotted paths: a
        // scan for each key made this take minutes.
        let mut text = String::from("[t]\n");
        for i in 0..200_000 {
            text.push_str(&format!("k{i} = 1\n"));
        }
        for i in 0..200_000 {
            text.push_str(&format!("d.p{i} = 1\n"));
        }
        let started = std::time::Instant::now();
        let doc = read(&text).unwrap();
        assert_eq!(table_of(&doc, "t").entries.len(), 200_001);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        text.push_str("k77777 = 2\n");
        assert_eq!(message(&text), "`k77777` is defined twice");
    }

    #[test]
    fn test_read_spans_point_at_values() {
        let text = "name = \"calc\"\n";
        let doc = read(text).unwrap();
        let entry = &doc.entries[0];
        assert_eq!(entry.key_span, Span::new(0, 4));
        assert_eq!(entry.value.span, Span::new(7, 13));
    }
}
