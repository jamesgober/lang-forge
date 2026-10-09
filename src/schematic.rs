//! The schematic: a read `.lsf` document checked against the schematic layout.
//!
//! ```toml
//! [language]          # required
//! name       = "calc" # required
//! version    = "1.0.0"
//! extensions = ["calc"]
//! start      = "program"   # default: the first rule
//!
//! [lexer]             # optional
//! identifiers     = "xid"  # or "ascii"
//! newlines        = false  # true: line breaks are NEWLINE tokens
//! line_comments   = ["//"]
//! block_comments  = [["/*", "*/"]]
//! nested_comments = false
//! strings         = ['"', { open = "#\"", close = "\"#", escape = "" }]
//!
//! [rules]             # required
//! program = "stmt*"
//! stmt    = "expr ';'"
//!
//! [rules.expr]        # a Pratt expression rule
//! operand = "NUMBER | '(' expr ')'"
//! levels  = [{ left = ["+", "-"] }, { left = ["*", "/"] }, { prefix = ["-"] }]
//!
//! [capabilities]      # optional
//! include = ["strict-types"]
//! ```
//!
//! Every setting is checked for presence, type, and spelling, and every
//! problem is reported — an unknown key is far more often a typo than an
//! intention, so it is an error rather than something to ignore.

use alloc::{borrow::Cow, boxed::Box, collections::BTreeSet, format, vec::Vec};

use syntax_lang::Span;

use crate::{
    codes,
    error::Report,
    noml::{Entry, Table, Text, Value, ValueKind},
};

/// A schematic, checked for layout but not yet for grammar.
#[derive(Debug)]
pub(crate) struct Schematic<'s> {
    /// The sketch format: 1 (lang-forge 1.x's schematic) or 2 (LSF2).
    pub(crate) format: u8,
    /// What format 2 adds; `None` for a format-1 schematic.
    pub(crate) v2: Option<Box<crate::spec2::V2<'s>>>,
    pub(crate) name: Cow<'s, str>,
    pub(crate) version: Option<Cow<'s, str>>,
    pub(crate) extensions: Vec<Cow<'s, str>>,
    pub(crate) start: Option<(Cow<'s, str>, Span)>,
    pub(crate) lexer: LexerSpec<'s>,
    pub(crate) rules: Vec<RuleSpec<'s>>,
    pub(crate) capabilities: Vec<(Cow<'s, str>, Span)>,
}

/// How identifiers are recognized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdentMode {
    /// Unicode identifiers (UAX #31 XID_Start / XID_Continue, plus `_`).
    Xid,
    /// ASCII letters, digits, and `_`.
    Ascii,
}

/// The `[lexer]` table.
#[derive(Debug)]
pub(crate) struct LexerSpec<'s> {
    pub(crate) identifiers: IdentMode,
    pub(crate) newlines: bool,
    pub(crate) line_comments: Vec<(Cow<'s, str>, Span)>,
    pub(crate) block_comments: Vec<BlockSpec<'s>>,
    pub(crate) nested_comments: bool,
    pub(crate) strings: Vec<StringSpec<'s>>,
}

/// A block comment's delimiters.
#[derive(Debug)]
pub(crate) struct BlockSpec<'s> {
    pub(crate) open: Cow<'s, str>,
    pub(crate) close: Cow<'s, str>,
    pub(crate) span: Span,
}

/// One kind of string literal.
#[derive(Debug)]
pub(crate) struct StringSpec<'s> {
    pub(crate) open: Cow<'s, str>,
    pub(crate) close: Cow<'s, str>,
    pub(crate) escape: Option<char>,
    pub(crate) multiline: bool,
    pub(crate) span: Span,
}

/// One entry of `[rules]`.
#[derive(Debug)]
pub(crate) struct RuleSpec<'s> {
    pub(crate) name: Cow<'s, str>,
    pub(crate) name_span: Span,
    pub(crate) body: Body<'s>,
    /// Format-2 rule options (all defaults in format 1).
    pub(crate) options: RuleOptions<'s>,
}

/// The options of a format-2 `[rules.<name>]` table with `rule`.
#[derive(Debug, Default)]
pub(crate) struct RuleOptions<'s> {
    /// `allow = ["overlap"]`: an intended `LSF4301` divergence.
    pub(crate) allow_overlap: bool,
    /// Extra recovery synchronization tokens (token references).
    pub(crate) sync: Vec<(Cow<'s, str>, Span)>,
}

/// A rule's definition.
#[derive(Debug)]
pub(crate) enum Body<'s> {
    /// A rule written in the rule language.
    Grammar(Text<'s>, Span),
    /// An expression rule driven by an operator table.
    Pratt(PrattSpec<'s>),
}

/// An expression rule: an operand and operator levels, lowest first.
#[derive(Debug)]
pub(crate) struct PrattSpec<'s> {
    pub(crate) operand: (Text<'s>, Span),
    pub(crate) levels: Vec<LevelSpec<'s>>,
}

/// Where an operator sits relative to its operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fixity {
    Left,
    Right,
    NonAssoc,
    Prefix,
    Postfix,
}

impl Fixity {
    pub(crate) const KEYS: [(&'static str, Fixity); 5] = [
        ("left", Fixity::Left),
        ("right", Fixity::Right),
        ("none", Fixity::NonAssoc),
        ("prefix", Fixity::Prefix),
        ("postfix", Fixity::Postfix),
    ];
}

/// One precedence level of an expression rule.
#[derive(Debug)]
pub(crate) struct LevelSpec<'s> {
    pub(crate) fixity: Fixity,
    /// Format 2: the level's `prec` (checked, carried for `dynamic`).
    pub(crate) prec: Option<(i64, Span)>,
    pub(crate) operators: Vec<(Cow<'s, str>, Span)>,
    pub(crate) then: Option<(Text<'s>, Span)>,
    pub(crate) node: Option<(Cow<'s, str>, Span)>,
    pub(crate) span: Span,
}

/// Interprets a read document of either format (LSF2 §2.1): no `[sketch]`
/// table, or `[sketch]` with only `format = 1`, is format 1, read exactly as
/// lang-forge 1.x read it; `format = 2` is format 2. Problems go to `report`;
/// the result is `None` when a required part is missing.
pub(crate) fn interpret<'s>(mut root: Table<'s>, report: &mut Report) -> Option<Schematic<'s>> {
    let Some(at) = root.entries.iter().position(|e| e.key == "sketch") else {
        return interpret_v1(root, report);
    };
    match crate::spec2::format_of(&root.entries[at], report)? {
        1 => {
            // `[sketch] format = 1` adds nothing: read the rest as format 1.
            let _ = root.entries.remove(at);
            interpret_v1(root, report)
        }
        _ => crate::spec2::interpret(root, report),
    }
}

/// Interprets a format-1 document: lang-forge 1.x's schematic, unchanged.
fn interpret_v1<'s>(root: Table<'s>, report: &mut Report) -> Option<Schematic<'s>> {
    let mut language = None;
    let mut lexer = None;
    let mut rules = None;
    let mut capabilities = Vec::new();
    for entry in root.entries {
        let key_span = entry.key_span;
        match &*entry.key {
            // `Some(None)`: present, but not a table (already reported).
            "language" => language = Some(table(entry, report)),
            "lexer" => lexer = table(entry, report),
            "rules" => rules = Some(table(entry, report).map(|t| (t, key_span))),
            "capabilities" => {
                if let Some(t) = table(entry, report) {
                    capabilities = read_capabilities(t, report);
                }
            }
            other => report.error_help(
                codes::UNKNOWN_SECTION,
                key_span,
                format!("unknown section `{other}`"),
                "a schematic has [language], [lexer], [rules], and [capabilities]",
            ),
        }
    }

    let lexer = lexer.map_or_else(LexerSpec::default, |t| read_lexer(t, report));
    let identity = match language {
        Some(Some(t)) => read_language(t, report),
        Some(None) => None,
        None => {
            report.error(codes::MISSING, Span::empty(0), "missing [language] table");
            None
        }
    };
    let rules = match rules {
        Some(Some((t, span))) => Some(read_rules(t, span, report)),
        Some(None) => None,
        None => {
            report.error(codes::MISSING, Span::empty(0), "missing [rules] table");
            None
        }
    };
    let (name, version, extensions, start) = identity?;
    let rules = rules?;
    Some(Schematic {
        format: 1,
        v2: None,
        name,
        version,
        extensions,
        start,
        lexer,
        rules,
        capabilities,
    })
}

type Identity<'s> = (
    Cow<'s, str>,
    Option<Cow<'s, str>>,
    Vec<Cow<'s, str>>,
    Option<(Cow<'s, str>, Span)>,
);

fn read_language<'s>(t: Table<'s>, report: &mut Report) -> Option<Identity<'s>> {
    let header = t.span;
    let mut name = None;
    let mut name_given = false;
    let mut version = None;
    let mut extensions = Vec::new();
    let mut start = None;
    for entry in t.entries {
        match &*entry.key {
            "name" => {
                name_given = true;
                if let Some((text, span)) = string(entry, report) {
                    if text.trim().is_empty() {
                        report.error(codes::LANGUAGE_NAME, span, "the language name is empty");
                    } else {
                        name = Some(text);
                    }
                }
            }
            "version" => version = string(entry, report).map(|(text, _)| text),
            "extensions" => {
                for (ext, span) in strings(entry, report) {
                    if ext.is_empty() {
                        report.error(codes::EXTENSION, span, "an extension is empty");
                    } else if let Some(bare) = ext.strip_prefix('.') {
                        report.error_help(
                            codes::EXTENSION,
                            span,
                            format!("extension `{ext}` starts with a dot"),
                            format!("write `{bare}`"),
                        );
                    } else {
                        extensions.push(ext);
                    }
                }
            }
            "start" => start = string(entry, report),
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[language]",
                &["name", "version", "extensions", "start"],
            ),
        }
    }
    if !name_given {
        report.error(codes::MISSING, header, "missing `name` in [language]");
    }
    Some((name?, version, extensions, start))
}

impl Default for LexerSpec<'_> {
    fn default() -> Self {
        Self {
            identifiers: IdentMode::Xid,
            newlines: false,
            line_comments: Vec::new(),
            block_comments: Vec::new(),
            nested_comments: false,
            strings: Vec::new(),
        }
    }
}

fn read_lexer<'s>(t: Table<'s>, report: &mut Report) -> LexerSpec<'s> {
    let mut spec = LexerSpec::default();
    for entry in t.entries {
        match &*entry.key {
            "identifiers" => {
                if let Some((mode, span)) = string(entry, report) {
                    match &*mode {
                        "xid" => spec.identifiers = IdentMode::Xid,
                        "ascii" => spec.identifiers = IdentMode::Ascii,
                        other => report.error_help(
                            codes::IDENT_STYLE,
                            span,
                            format!("unknown identifier style `{other}`"),
                            "use \"xid\" (Unicode identifiers) or \"ascii\"",
                        ),
                    }
                }
            }
            "newlines" => spec.newlines = boolean(entry, report).unwrap_or(false),
            "nested_comments" => spec.nested_comments = boolean(entry, report).unwrap_or(false),
            "line_comments" => {
                for (open, span) in strings(entry, report) {
                    if open.is_empty() {
                        report.error(codes::DELIMITER, span, "a comment delimiter is empty");
                    } else {
                        spec.line_comments.push((open, span));
                    }
                }
            }
            "block_comments" => {
                let Some(items) = array(entry, report) else {
                    continue;
                };
                for item in items {
                    if let Some(block) = block_comment(item, report) {
                        spec.block_comments.push(block);
                    }
                }
            }
            "strings" => {
                let Some(items) = array(entry, report) else {
                    continue;
                };
                for item in items {
                    if let Some(s) = string_spec(item, report) {
                        spec.strings.push(s);
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[lexer]",
                &[
                    "identifiers",
                    "newlines",
                    "line_comments",
                    "block_comments",
                    "nested_comments",
                    "strings",
                ],
            ),
        }
    }
    spec
}

pub(crate) fn block_comment<'s>(item: Value<'s>, report: &mut Report) -> Option<BlockSpec<'s>> {
    let span = item.span;
    let shape_error = |report: &mut Report| {
        report.error_help(
            codes::WRONG_TYPE,
            span,
            "a block comment is a pair of delimiters",
            "write it as [\"/*\", \"*/\"]",
        );
    };
    let ValueKind::Array(pair) = item.kind else {
        shape_error(report);
        return None;
    };
    // Exactly two items, both strings: anything else in the pair is an
    // error, never silently dropped.
    if pair.len() != 2 {
        shape_error(report);
        return None;
    }
    let mut texts = Vec::with_capacity(2);
    for v in pair {
        match v.kind {
            ValueKind::Str(t) => texts.push(t.text),
            other => report.error_help(
                codes::WRONG_TYPE,
                v.span,
                format!(
                    "a block comment delimiter must be a string, found {}",
                    item_type(&other)
                ),
                "write it as [\"/*\", \"*/\"]",
            ),
        }
    }
    let mut texts = texts.into_iter();
    let (Some(open), Some(close)) = (texts.next(), texts.next()) else {
        return None;
    };
    if open.is_empty() || close.is_empty() {
        report.error(codes::DELIMITER, span, "a comment delimiter is empty");
        return None;
    }
    Some(BlockSpec { open, close, span })
}

pub(crate) fn string_spec<'s>(item: Value<'s>, report: &mut Report) -> Option<StringSpec<'s>> {
    let span = item.span;
    match item.kind {
        ValueKind::Str(open) => {
            if open.text.is_empty() {
                report.error(codes::STRING_CLOSE, span, "a string delimiter is empty");
                return None;
            }
            Some(StringSpec {
                close: open.text.clone(),
                open: open.text,
                escape: Some('\\'),
                multiline: false,
                span,
            })
        }
        ValueKind::Table(t) => {
            let mut open = None;
            let mut close = None;
            let mut escape = Some('\\');
            let mut multiline = false;
            for entry in t.entries {
                match &*entry.key {
                    "open" => open = string(entry, report),
                    "close" => close = string(entry, report),
                    "escape" => {
                        if let Some((text, at)) = string(entry, report) {
                            let mut chars = text.chars();
                            escape = match (chars.next(), chars.next()) {
                                (None, _) => None,
                                (Some(c), None) if c.is_ascii() && !c.is_ascii_whitespace() => {
                                    Some(c)
                                }
                                _ => {
                                    report.error_help(
                                        codes::ESCAPE,
                                        at,
                                        format!("escape `{text}` is not a single character"),
                                        "use one ASCII character such as \"\\\\\", or \"\" for none",
                                    );
                                    None
                                }
                            };
                        }
                    }
                    "multiline" => multiline = boolean(entry, report).unwrap_or(false),
                    other => unknown_key(
                        report,
                        entry.key_span,
                        other,
                        "a string",
                        &["open", "close", "escape", "multiline"],
                    ),
                }
            }
            let Some((open, open_span)) = open else {
                report.error(codes::MISSING, span, "a string needs an `open` delimiter");
                return None;
            };
            let close = close.map_or_else(|| open.clone(), |(c, _)| c);
            if open.is_empty() || close.is_empty() {
                report.error(
                    codes::STRING_CLOSE,
                    open_span,
                    "a string delimiter is empty",
                );
                return None;
            }
            Some(StringSpec {
                open,
                close,
                escape,
                multiline,
                span,
            })
        }
        _ => {
            report.error_help(
                codes::WRONG_TYPE,
                span,
                format!(
                    "expected a string delimiter, found {}",
                    item_type(&item.kind)
                ),
                "write \"\\\"\" or { open = \"\\\"\", escape = \"\\\\\" }",
            );
            None
        }
    }
}

pub(crate) fn item_type(kind: &ValueKind<'_>) -> &'static str {
    kind.type_name()
}

fn read_rules<'s>(t: Table<'s>, span: Span, report: &mut Report) -> Vec<RuleSpec<'s>> {
    let declared = t.entries.len();
    let mut rules = Vec::with_capacity(declared);
    for entry in t.entries {
        let name_span = entry.key_span;
        let value_span = entry.value.span;
        let body = match entry.value.kind {
            ValueKind::Str(text) => Body::Grammar(text, value_span),
            ValueKind::Table(table) => match read_pratt(table, value_span, report) {
                Some(pratt) => Body::Pratt(pratt),
                None => continue,
            },
            other => {
                report.error(
                    codes::WRONG_TYPE,
                    value_span,
                    format!(
                        "rule `{}` must be a string or a table, found {}",
                        entry.key,
                        item_type(&other)
                    ),
                );
                continue;
            }
        };
        rules.push(RuleSpec {
            name: entry.key,
            name_span,
            body,
            options: RuleOptions::default(),
        });
    }
    if rules.is_empty() && declared == 0 {
        report.error(codes::MISSING, span, "[rules] declares no rules");
    }
    rules
}

fn read_pratt<'s>(t: Table<'s>, span: Span, report: &mut Report) -> Option<PrattSpec<'s>> {
    let mut operand = None;
    let mut levels = None;
    for entry in t.entries {
        match &*entry.key {
            "operand" => operand = text(entry, report),
            "levels" => {
                let Some(items) = array(entry, report) else {
                    continue;
                };
                levels = Some(
                    items
                        .into_iter()
                        .filter_map(|item| read_level(item, report))
                        .collect::<Vec<_>>(),
                );
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "an expression rule",
                &["operand", "levels"],
            ),
        }
    }
    let Some(operand) = operand else {
        report.error(
            codes::MISSING,
            span,
            "an expression rule needs an `operand`",
        );
        return None;
    };
    Some(PrattSpec {
        operand,
        levels: levels.unwrap_or_default(),
    })
}

fn read_level<'s>(item: Value<'s>, report: &mut Report) -> Option<LevelSpec<'s>> {
    let span = item.span;
    let ValueKind::Table(t) = item.kind else {
        report.error_help(
            codes::WRONG_TYPE,
            span,
            format!(
                "an operator level must be a table, found {}",
                item_type(&item.kind)
            ),
            "write it as { left = [\"+\", \"-\"] }",
        );
        return None;
    };
    let mut fixity = None;
    let mut operators = Vec::new();
    let mut then = None;
    let mut node = None;
    for entry in t.entries {
        if let Some(&(_, f)) = Fixity::KEYS.iter().find(|(k, _)| *k == entry.key) {
            if fixity.is_some() {
                report.error(codes::WRONG_TYPE, entry.key_span, "an operator level has exactly one of `left`, `right`, `none`, `prefix`, or `postfix`");
                continue;
            }
            fixity = Some(f);
            operators = operator_list(entry, report);
            continue;
        }
        match &*entry.key {
            "then" => then = text(entry, report),
            "node" => node = string(entry, report),
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "an operator level",
                &["left", "right", "none", "prefix", "postfix", "then", "node"],
            ),
        }
    }
    let Some(fixity) = fixity else {
        report.error_help(
            codes::MISSING,
            span,
            "an operator level names no operators",
            "add one of `left`, `right`, `none`, `prefix`, or `postfix`",
        );
        return None;
    };
    if operators.is_empty() {
        report.error(codes::MISSING, span, "an operator level names no operators");
        return None;
    }
    Some(LevelSpec {
        fixity,
        prec: None,
        operators,
        then,
        node,
        span,
    })
}

/// A single operator string or an array of them.
fn operator_list<'s>(entry: Entry<'s>, report: &mut Report) -> Vec<(Cow<'s, str>, Span)> {
    if let ValueKind::Str(t) = entry.value.kind {
        return Vec::from([(t.text, entry.value.span)]);
    }
    strings(entry, report)
}

fn read_capabilities<'s>(t: Table<'s>, report: &mut Report) -> Vec<(Cow<'s, str>, Span)> {
    let mut include = Vec::new();
    // Names already included, for a duplicate check that stays O(log n) per
    // name rather than a scan of the list.
    let mut seen: BTreeSet<Cow<'s, str>> = BTreeSet::new();
    for entry in t.entries {
        match &*entry.key {
            "include" => {
                for (name, span) in strings(entry, report) {
                    if name.trim().is_empty() {
                        report.error(codes::CAPABILITY_NAME, span, "a capability name is empty");
                    } else if name.trim() != name {
                        report.error(
                            codes::CAPABILITY_NAME,
                            span,
                            format!("capability name `{name}` has surrounding whitespace"),
                        );
                    } else if !seen.insert(name.clone()) {
                        report.error(
                            codes::CAPABILITY_NAME,
                            span,
                            format!("capability `{name}` is included twice"),
                        );
                    } else {
                        include.push((name, span));
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[capabilities]",
                &["include"],
            ),
        }
    }
    include
}

pub(crate) fn unknown_key(report: &mut Report, span: Span, key: &str, place: &str, known: &[&str]) {
    let list = known
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    report.error_help(
        codes::UNKNOWN_KEY,
        span,
        format!("unknown key `{key}` in {place}"),
        format!("expected one of {list}"),
    );
}

pub(crate) fn expected(report: &mut Report, entry: &Entry<'_>, what: &str) {
    report.error(
        codes::WRONG_TYPE,
        entry.value.span,
        format!(
            "`{}` must be {what}, found {}",
            entry.key,
            entry.value.type_name()
        ),
    );
}

pub(crate) fn table<'s>(entry: Entry<'s>, report: &mut Report) -> Option<Table<'s>> {
    if let ValueKind::Table(t) = entry.value.kind {
        return Some(t);
    }
    expected(report, &entry, "a table");
    None
}

pub(crate) fn array<'s>(entry: Entry<'s>, report: &mut Report) -> Option<Vec<Value<'s>>> {
    if let ValueKind::Array(items) = entry.value.kind {
        return Some(items);
    }
    expected(report, &entry, "an array");
    None
}

pub(crate) fn boolean(entry: Entry<'_>, report: &mut Report) -> Option<bool> {
    if let ValueKind::Bool(b) = entry.value.kind {
        return Some(b);
    }
    expected(report, &entry, "a boolean");
    None
}

pub(crate) fn text<'s>(entry: Entry<'s>, report: &mut Report) -> Option<(Text<'s>, Span)> {
    let span = entry.value.span;
    if let ValueKind::Str(t) = entry.value.kind {
        return Some((t, span));
    }
    expected(report, &entry, "a string");
    None
}

pub(crate) fn string<'s>(entry: Entry<'s>, report: &mut Report) -> Option<(Cow<'s, str>, Span)> {
    text(entry, report).map(|(t, span)| (t.text, span))
}

/// An array of strings; non-string items are reported and skipped.
pub(crate) fn strings<'s>(entry: Entry<'s>, report: &mut Report) -> Vec<(Cow<'s, str>, Span)> {
    let key = entry.key.clone();
    let Some(items) = array(entry, report) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let span = item.span;
        match item.kind {
            ValueKind::Str(t) => out.push((t.text, span)),
            other => report.error(
                codes::WRONG_TYPE,
                span,
                format!("`{key}` must hold strings, found {}", item_type(&other)),
            ),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use alloc::string::String;

    /// The layout example at the top of this module is a schematic that
    /// forges, so the documentation cannot drift into one that does not.
    #[test]
    fn test_module_example_forges() {
        let mut example = String::new();
        let mut inside = false;
        for line in include_str!("schematic.rs").lines() {
            match line.trim_end() {
                "//! ```toml" => inside = true,
                "//! ```" if inside => break,
                line if inside => {
                    let text = line.strip_prefix("//!").unwrap_or(line);
                    example.push_str(text.strip_prefix(' ').unwrap_or(text));
                    example.push('\n');
                }
                _ => {}
            }
        }
        let lang = crate::Language::from_lsf(&example).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(lang.name(), "calc");
        assert!(!lang.parse("-(1 + 2) * 3; 4 / 5;").has_errors());
    }
}
