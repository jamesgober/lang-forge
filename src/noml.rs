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
//! trailing comma, as NOML allows; everything else follows TOML.

use alloc::{borrow::Cow, format, string::String, vec::Vec};

use diag_lang::{Diagnostic, Label, Severity};
use syntax_lang::Span;

/// How deeply arrays and inline tables may nest. Real schematics nest two or
/// three levels; the limit keeps hostile input from exhausting the stack.
const MAX_NESTING: u32 = 64;

/// A table: its entries in source order.
#[derive(Debug)]
pub(crate) struct Table<'s> {
    pub(crate) entries: Vec<Entry<'s>>,
    /// The table header or inline table, or the key that implied the table.
    pub(crate) span: Span,
    /// Defined by a `[header]` (so a second header is a duplicate).
    explicit: bool,
    /// An inline table, which is complete as written and cannot be extended.
    sealed: bool,
}

impl Default for Table<'_> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            span: Span::empty(0),
            explicit: false,
            sealed: false,
        }
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
    /// A number, kept as written: no schematic setting takes one, so it is only
    /// ever reported as the wrong type.
    Number,
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
            ValueKind::Array(_) => "an array",
            ValueKind::Table(_) => "a table",
        }
    }
}

/// Reads a schematic into its root table.
pub(crate) fn read(text: &str) -> Result<Table<'_>, Diagnostic> {
    if u32::try_from(text.len()).is_err() {
        return Err(error(Span::empty(0), "the schematic is larger than 4 GiB"));
    }
    let mut reader = Reader {
        src: text,
        bytes: text.as_bytes(),
        pos: 0,
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
                Some(b'[') => current = self.header(&mut root)?,
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
            let index = match table.entries.iter().position(|e| e.key == key) {
                Some(index) => index,
                None => {
                    table.entries.push(Entry {
                        key,
                        key_span,
                        value: Value {
                            kind: ValueKind::Table(Table {
                                span,
                                ..Table::default()
                            }),
                            span,
                        },
                    });
                    table.entries.len() - 1
                }
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
                if child.explicit {
                    return Err(error(
                        span,
                        format!(
                            "table [{}] is defined twice",
                            self.src[open + 1..span.end().to_usize() - 1].trim()
                        ),
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
        self.skip_spaces();
        if self.peek() != Some(b'=') {
            return Err(self.unexpected("`=` after the key"));
        }
        self.pos += 1;
        self.skip_spaces();
        let value = self.value()?;
        Ok((keys, value))
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
            Some(b'"' | b'\'') => {
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
            Some(b'0'..=b'9' | b'+' | b'-') => {
                while self.peek().is_some_and(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-')
                }) {
                    self.pos += 1;
                }
                ValueKind::Number
            }
            Some(b) if b.is_ascii_alphabetic() => {
                let span = self.word_span();
                let word = &self.src[span.start().to_usize()..span.end().to_usize()];
                self.pos = span.end().to_usize();
                match word {
                    "true" => ValueKind::Bool(true),
                    "false" => ValueKind::Bool(false),
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

    /// Runs `parse` one nesting level deeper, refusing to go past the limit.
    fn nested<T>(&mut self, parse: fn(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth >= MAX_NESTING {
            return Err(error(
                Span::new(self.pos as u32, self.pos as u32 + 1),
                format!("arrays and inline tables nest more than {MAX_NESTING} levels deep"),
            ));
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
        let existing = table.entries.iter().position(|e| e.key == key);
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
            table.entries.push(Entry {
                key,
                key_span,
                value,
            });
            return Ok(());
        }
        let index = match existing {
            Some(index) => index,
            None => {
                table.entries.push(Entry {
                    key,
                    key_span,
                    value: Value {
                        kind: ValueKind::Table(Table {
                            span: key_span,
                            ..Table::default()
                        }),
                        span: key_span,
                    },
                });
                table.entries.len() - 1
            }
        };
        let entry = &mut table.entries[index];
        match &mut entry.value.kind {
            ValueKind::Table(child) if !child.sealed && !child.explicit => table = child,
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
        assert!(message(&deep).contains("nest more than 64 levels"));
        let ok = format!("a = {}{}\n", "[".repeat(60), "]".repeat(60));
        assert!(read(&ok).is_ok());
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
