//! The format-2 reader: a read LSF2 document checked against the syntax
//! sections of LSF2 (§7–§14) and turned into a [`Schematic`] with its
//! [`V2`] additions.
//!
//! Every key the spec lists for `[sketch]`, `[language]`, `[lexer]` and its
//! sub-tables, `[layout]`, `[rules]`, `[injections]`, `[hooks]`, `[ast]`, and
//! `[capabilities]` is read, type-checked, range-checked, and name-checked
//! here (unknown keys are `LSF1002` with a did-you-mean). Sections that other
//! crates consume (`[semantics]`, `[types]`, `[runtime]`, `[stdlib]`, `[exec]`,
//! `[tooling]`, `[lints]`, `[macros]`, `[emit]`, `[migrate]`) are accepted as
//! well-formed NOML and not interpreted: they are not the syntax forge's to
//! check. Keys LSF2 specifies but this release does not implement are refused
//! with `LSF1007`, never ignored.

use alloc::{borrow::Cow, boxed::Box, collections::BTreeSet, format, string::String, vec::Vec};

use syntax_lang::Span;

use crate::{
    codes,
    error::Report,
    noml::{Entry, Table, Text, Value, ValueKind},
    schematic::{
        self, Body, Fixity, IdentMode, LevelSpec, LexerSpec, PrattSpec, RuleOptions, RuleSpec,
        Schematic, StringSpec,
    },
    suggest,
};

/// The default mode-stack budget at run time (LSF2 §9.1).
const DEFAULT_MODE_DEPTH: u32 = 256;

/// What format 2 adds to a schematic.
#[derive(Debug, Default)]
pub(crate) struct V2<'s> {
    /// `[sketch] kind`.
    pub(crate) role: Role,
    pub(crate) display_name: Option<Cow<'s, str>>,
    pub(crate) description: Option<Cow<'s, str>>,
    pub(crate) edition: Option<Cow<'s, str>>,
    pub(crate) files: Vec<FileSpec<'s>>,
    pub(crate) shebang_names: Vec<Cow<'s, str>>,
    pub(crate) checks: Checks,
    pub(crate) lexer: Lexer2<'s>,
    pub(crate) layout: Option<Layout<'s>>,
    pub(crate) ast: Vec<Supertype<'s>>,
    pub(crate) injections: Vec<Injection<'s>>,
}

/// `[sketch] kind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Role {
    #[default]
    Language,
    Part,
    Mixin,
}

/// One `[language] files` entry.
#[derive(Debug)]
pub(crate) struct FileSpec<'s> {
    pub(crate) extension: (Cow<'s, str>, Span),
    pub(crate) mode: Option<(Cow<'s, str>, Span)>,
    pub(crate) start: Option<(Cow<'s, str>, Span)>,
}

/// The level of a forge check (`[sketch.checks]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Deny,
    Warn,
    Allow,
}

/// `[sketch.checks]`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Checks {
    pub(crate) overlap: Level,
    pub(crate) unused_rule: Level,
    pub(crate) unused_token: Level,
}

impl Default for Checks {
    fn default() -> Self {
        Self {
            overlap: Level::Deny,
            unused_rule: Level::Warn,
            unused_token: Level::Warn,
        }
    }
}

/// The format-2 `[lexer]` table and its sub-tables.
#[derive(Debug)]
pub(crate) struct Lexer2<'s> {
    pub(crate) ident: IdentSpec,
    pub(crate) newlines: bool,
    pub(crate) comments: Vec<CommentSpec<'s>>,
    pub(crate) nested_default: bool,
    /// The format-1 `strings = [...]` form: unnamed classes of kind `STRING`.
    pub(crate) strings_array: Vec<StringSpec<'s>>,
    pub(crate) classes: Vec<StringClass<'s>>,
    pub(crate) brackets: Option<Vec<Pair<'s>>>,
    pub(crate) initial_mode: Option<(Cow<'s, str>, Span)>,
    pub(crate) shebang: bool,
    pub(crate) max_mode_depth: u32,
    pub(crate) keywords: KeywordsSpec<'s>,
    pub(crate) numbers: NumbersSpec,
    pub(crate) tokens: Vec<TokenSpec<'s>>,
    pub(crate) modes: Vec<ModeSpec<'s>>,
    /// `[lexer.columns] tab_width`.
    pub(crate) tab_width: u32,
}

impl Default for Lexer2<'_> {
    fn default() -> Self {
        Self {
            ident: IdentSpec::default(),
            newlines: false,
            comments: Vec::new(),
            nested_default: false,
            strings_array: Vec::new(),
            classes: Vec::new(),
            brackets: None,
            initial_mode: None,
            shebang: false,
            max_mode_depth: DEFAULT_MODE_DEPTH,
            keywords: KeywordsSpec::default(),
            numbers: NumbersSpec::default(),
            tokens: Vec::new(),
            modes: Vec::new(),
            tab_width: 1,
        }
    }
}

/// `[lexer] identifiers` in either form.
#[derive(Clone, Debug)]
pub(crate) struct IdentSpec {
    pub(crate) mode: IdentMode,
    pub(crate) extra_start: Vec<char>,
    pub(crate) extra_continue: Vec<char>,
    pub(crate) require_nfc: bool,
    pub(crate) span: Span,
}

impl Default for IdentSpec {
    fn default() -> Self {
        Self {
            mode: IdentMode::Xid,
            extra_start: Vec::new(),
            extra_continue: Vec::new(),
            require_nfc: false,
            span: Span::empty(0),
        }
    }
}

/// A line or block comment, in either form.
#[derive(Debug)]
pub(crate) struct CommentSpec<'s> {
    pub(crate) open: Cow<'s, str>,
    /// `None` for a line comment.
    pub(crate) close: Option<Cow<'s, str>>,
    pub(crate) doc: bool,
    pub(crate) nested: Option<bool>,
    pub(crate) not_followed_by: Option<(Cow<'s, str>, Span)>,
    pub(crate) stop_before: Vec<(Cow<'s, str>, Span)>,
    pub(crate) modes: Option<Vec<(Cow<'s, str>, Span)>>,
    pub(crate) span: Span,
}

/// One delimiter part (LSF2 §9.5.3).
#[derive(Debug, Clone)]
pub(crate) enum Part<'s> {
    Text(Cow<'s, str>),
    Regex(Cow<'s, str>),
    Capture(Cow<'s, str>),
    Backref,
    Newline,
}

/// Where a string's close delimiter may appear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseAt {
    Anywhere,
    LineStart,
    LineStartIndented,
}

/// A named string class (`[lexer.strings.<CLASS>]`).
#[derive(Debug)]
pub(crate) struct StringClass<'s> {
    pub(crate) name: Cow<'s, str>,
    pub(crate) name_span: Span,
    pub(crate) open: Vec<(Part<'s>, Span)>,
    pub(crate) close: Vec<(Part<'s>, Span)>,
    pub(crate) escape: Option<char>,
    pub(crate) multiline: bool,
    pub(crate) interpolate: Vec<Interp<'s>>,
    pub(crate) embedded: Vec<Embedded<'s>>,
    pub(crate) escape_tokens: bool,
    /// `body = "next-line"`.
    pub(crate) next_line: bool,
    pub(crate) close_at: CloseAt,
    pub(crate) close_not_followed_by: Option<(Cow<'s, str>, Span)>,
    pub(crate) modes: Option<Vec<(Cow<'s, str>, Span)>>,
    pub(crate) span: Span,
}

impl StringClass<'_> {
    /// Whether the class builds a node (LSF2 §9.5.5) rather than one token.
    pub(crate) fn is_node(&self) -> bool {
        !self.interpolate.is_empty() || !self.embedded.is_empty() || self.escape_tokens
    }
}

/// An interpolation hole of a string class.
#[derive(Debug)]
pub(crate) struct Interp<'s> {
    pub(crate) open: Cow<'s, str>,
    pub(crate) close: Cow<'s, str>,
    pub(crate) rule: (Cow<'s, str>, Span),
    pub(crate) mode: Option<(Cow<'s, str>, Span)>,
    pub(crate) when_next: Option<(Cow<'s, str>, Span)>,
    pub(crate) span: Span,
}

/// An embedded token of a string class.
#[derive(Debug)]
pub(crate) struct Embedded<'s> {
    pub(crate) token: (Cow<'s, str>, Span),
    pub(crate) regex: (Cow<'s, str>, Span),
    pub(crate) parse: Option<(Cow<'s, str>, Span)>,
}

/// `[lexer.keywords]`.
#[derive(Debug, Default)]
pub(crate) struct KeywordsSpec<'s> {
    pub(crate) default_contextual: bool,
    pub(crate) contextual: Vec<(Cow<'s, str>, Span)>,
    pub(crate) reserved: Vec<(Cow<'s, str>, Span)>,
    pub(crate) case_insensitive: bool,
}

/// What a decimal literal with a leading zero means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeadingZeros {
    Decimal,
    Error,
    Octal,
}

/// `[lexer.numbers]`.
#[derive(Clone, Debug)]
pub(crate) struct NumbersSpec {
    /// `0x`, `0o`, `0b` allowed.
    pub(crate) radix: [bool; 3],
    pub(crate) radix_any_case: bool,
    pub(crate) separator: Option<char>,
    pub(crate) separator_anywhere: bool,
    pub(crate) floats: bool,
    pub(crate) exponent: bool,
    pub(crate) leading_dot: bool,
    pub(crate) trailing_dot: bool,
    pub(crate) hex_floats: bool,
    pub(crate) leading_zeros: LeadingZeros,
    pub(crate) suffixes: Vec<String>,
}

impl Default for NumbersSpec {
    fn default() -> Self {
        Self {
            radix: [true; 3],
            radix_any_case: false,
            separator: Some('_'),
            separator_anywhere: false,
            floats: true,
            exponent: true,
            leading_dot: false,
            trailing_dot: false,
            hex_floats: false,
            leading_zeros: LeadingZeros::Decimal,
            suffixes: Vec::new(),
        }
    }
}

/// How a custom token class is matched.
#[derive(Debug)]
pub(crate) enum TokenSource<'s> {
    Regex(Cow<'s, str>),
    Literal(Cow<'s, str>),
}

/// A lexer action.
#[derive(Clone, Debug)]
pub(crate) enum ActionSpec<'s> {
    Push(Cow<'s, str>),
    Pop,
    Switch(Cow<'s, str>),
}

/// One `[lexer.tokens]` entry.
#[derive(Debug)]
pub(crate) struct TokenSpec<'s> {
    pub(crate) name: Cow<'s, str>,
    pub(crate) name_span: Span,
    pub(crate) source: (TokenSource<'s>, Span),
    pub(crate) trivia: bool,
    pub(crate) priority: i8,
    pub(crate) modes: Option<Vec<(Cow<'s, str>, Span)>>,
    pub(crate) action: Option<(ActionSpec<'s>, Span)>,
    pub(crate) followed_by: Option<(Cow<'s, str>, Span)>,
    pub(crate) not_followed_by: Option<(Cow<'s, str>, Span)>,
    pub(crate) when_prev: Option<Vec<(Cow<'s, str>, Span)>>,
    pub(crate) unless_prev: Option<Vec<(Cow<'s, str>, Span)>>,
    pub(crate) line_start: bool,
    pub(crate) indented: bool,
    pub(crate) column: Option<(u32, u32)>,
}

/// One `[lexer.modes.<mode>]` table.
#[derive(Debug)]
pub(crate) struct ModeSpec<'s> {
    pub(crate) name: Cow<'s, str>,
    pub(crate) name_span: Span,
    pub(crate) inherit: Option<(Cow<'s, str>, Span)>,
    pub(crate) literals: Option<bool>,
    pub(crate) builtins: Option<bool>,
    pub(crate) trivia: Option<bool>,
    pub(crate) tokens: Vec<(Cow<'s, str>, Span)>,
    pub(crate) strings: Vec<(Cow<'s, str>, Span)>,
    pub(crate) text: Option<(Cow<'s, str>, Span)>,
    pub(crate) actions: Vec<ModeAction<'s>>,
    pub(crate) eof_error: Option<bool>,
}

/// `[layout]` and `[layout.newlines]`.
/// A text pair with its span: `[open, close]` in `brackets` and the
/// layout's join lists.
pub(crate) type Pair<'s> = (Cow<'s, str>, Cow<'s, str>, Span);

/// A mode's `actions` entry: the token reference and the action, with spans.
pub(crate) type ModeAction<'s> = ((Cow<'s, str>, Span), (ActionSpec<'s>, Span));

#[derive(Debug)]
pub(crate) struct Layout<'s> {
    pub(crate) indent: bool,
    pub(crate) open_after: Vec<(Cow<'s, str>, Span)>,
    pub(crate) implicit_join: Option<Vec<Pair<'s>>>,
    pub(crate) explicit_join: Option<(Cow<'s, str>, Span)>,
    pub(crate) tab_width: u32,
    pub(crate) mixed_error: bool,
    pub(crate) newlines: NewlineMode,
    pub(crate) terminate_after: Vec<(Cow<'s, str>, Span)>,
    pub(crate) continue_before: Vec<(Cow<'s, str>, Span)>,
    pub(crate) ignored_inside: Option<Vec<Pair<'s>>>,
}

/// `[layout.newlines] mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NewlineMode {
    Trivia,
    Significant,
    Terminators,
}

/// One `[ast]` supertype.
#[derive(Debug)]
pub(crate) struct Supertype<'s> {
    pub(crate) name: Cow<'s, str>,
    pub(crate) span: Span,
    pub(crate) members: Vec<(Cow<'s, str>, Span)>,
}

/// One `[injections.<id>]` table.
#[derive(Debug)]
pub(crate) struct Injection<'s> {
    pub(crate) id: Cow<'s, str>,
    pub(crate) span: Span,
    pub(crate) target: (Cow<'s, str>, Span),
    pub(crate) language: (Cow<'s, str>, Span),
    /// `resolve = "editor"`.
    pub(crate) editor: bool,
    pub(crate) start: Option<(Cow<'s, str>, Span)>,
    /// `content`: `Some(true)` for `inner`, `Some(false)` for `whole`.
    pub(crate) inner: Option<bool>,
    pub(crate) combined: bool,
    pub(crate) when: Option<(Cow<'s, str>, Span)>,
    pub(crate) scope: Option<Cow<'s, str>>,
}

/// The sections format 2 knows but lang-forge does not interpret (other
/// crates consume them).
const FOREIGN_SECTIONS: [&str; 10] = [
    "semantics",
    "types",
    "runtime",
    "stdlib",
    "exec",
    "tooling",
    "lints",
    "macros",
    "emit",
    "migrate",
];

/// Every section a format-2 sketch may have.
const SECTIONS: [&str; 20] = [
    "sketch",
    "language",
    "lexer",
    "layout",
    "rules",
    "injections",
    "hooks",
    "ast",
    "capabilities",
    "compose",
    "semantics",
    "types",
    "runtime",
    "stdlib",
    "exec",
    "tooling",
    "lints",
    "macros",
    "emit",
    "migrate",
];

/// The `format` of a `[sketch]` entry, or `None` (reported) when it is
/// missing, malformed, or unknown. A format-1 `[sketch]` may hold nothing else.
pub(crate) fn format_of(entry: &Entry<'_>, report: &mut Report) -> Option<i64> {
    let ValueKind::Table(t) = &entry.value.kind else {
        report.error(
            codes::WRONG_TYPE,
            entry.value.span,
            format!(
                "`sketch` must be a table, found {}",
                entry.value.type_name()
            ),
        );
        return None;
    };
    let Some(format) = t.entries.iter().find(|e| e.key == "format") else {
        report.error_help(
            codes::FORMAT_MISSING,
            t.span,
            "`[sketch]` needs `format = 2`",
            "add `format = 2`, or remove the table for a format-1 sketch",
        );
        return None;
    };
    let number = match format.value.kind {
        ValueKind::Number(Some(n)) => n,
        _ => {
            report.error(
                codes::WRONG_TYPE,
                format.value.span,
                format!(
                    "`format` must be an integer, found {}",
                    format.value.type_name()
                ),
            );
            return None;
        }
    };
    match number {
        1 => {
            if let Some(other) = t.entries.iter().find(|e| e.key != "format") {
                report.error_help(
                    codes::NEEDS_FORMAT_2,
                    other.key_span,
                    format!("`{}` in [sketch] needs `format = 2`", other.key),
                    "a format-1 sketch's [sketch] table holds only `format = 1`",
                );
                return None;
            }
            Some(1)
        }
        2 => Some(2),
        n if n > 2 => {
            report.error(
                codes::FORMAT_TOO_NEW,
                format.value.span,
                format!("sketch format {n} is newer than this lang-forge, which reads formats 1–2"),
            );
            None
        }
        n => {
            report.error(
                codes::OUT_OF_RANGE,
                format.value.span,
                format!("sketch format {n} does not exist; formats are 1 and 2"),
            );
            None
        }
    }
}

/// Interprets a format-2 document.
pub(crate) fn interpret<'s>(mut root: Table<'s>, report: &mut Report) -> Option<Schematic<'s>> {
    // Multi-line strings read CRLF as LF in format 2 (LSF2 §1.4).
    root.rebase(0);
    let mut v2 = V2::default();
    let mut identity = None;
    let mut language_seen = false;
    let mut lexer_table = None;
    let mut rules = None;
    let mut capabilities = Vec::new();
    for entry in root.entries {
        let key_span = entry.key_span;
        match &*entry.key {
            "sketch" => {
                if let Some(t) = schematic::table(entry, report) {
                    read_sketch(t, &mut v2, report);
                }
            }
            "language" => {
                language_seen = true;
                if let Some(t) = schematic::table(entry, report) {
                    identity = read_language(t, &mut v2, report);
                }
            }
            "lexer" => lexer_table = schematic::table(entry, report),
            "rules" => rules = Some(schematic::table(entry, report).map(|t| (t, key_span))),
            "capabilities" => {
                if let Some(t) = schematic::table(entry, report) {
                    capabilities = read_capabilities(t, report);
                }
            }
            "layout" => {
                if let Some(t) = schematic::table(entry, report) {
                    v2.layout = read_layout(t, report);
                }
            }
            "ast" => {
                if let Some(t) = schematic::table(entry, report) {
                    v2.ast = read_ast(t, report);
                }
            }
            "injections" => {
                if let Some(t) = schematic::table(entry, report) {
                    v2.injections = read_injections(t, report);
                }
            }
            "hooks" => {
                if let Some(t) = schematic::table(entry, report) {
                    read_hooks(t, report);
                }
            }
            "compose" => report.error_help(
                codes::NOT_SUPPORTED,
                key_span,
                "`[compose]` is not supported by lang-forge 2.0.0-alpha.1",
                "composition (`extends`, `[compose]`) is ls-sketch's; forge the composed sketch (ROADMAP: lang-forge alpha.2)",
            ),
            other if FOREIGN_SECTIONS.contains(&other) => {
                // Consumed by other crates; well-formed NOML is all the
                // syntax forge asks of it.
            }
            other => unknown_section(report, key_span, other),
        }
    }
    if !language_seen && v2.role == Role::Language {
        report.error(codes::MISSING, Span::empty(0), "missing [language] table");
    }
    let lexer = match lexer_table {
        Some(t) => read_lexer(t, report),
        None => Lexer2::default(),
    };
    let rules = match rules {
        Some(Some((t, span))) => Some(read_rules(t, span, report)),
        Some(None) => None,
        None => {
            report.error(codes::MISSING, Span::empty(0), "missing [rules] table");
            None
        }
    };
    if lexer.newlines
        && v2
            .layout
            .as_ref()
            .is_some_and(|l| l.newlines != NewlineMode::Trivia)
    {
        report.error(
            codes::NEWLINES_TWICE,
            lexer.ident.span,
            "`[lexer] newlines` and `[layout.newlines]` both set the newline policy; keep one",
        );
    }
    let identity = identity?;
    let rules = rules?;
    let format1_lexer = LexerSpec {
        identifiers: lexer.ident.mode,
        newlines: lexer.newlines
            || v2
                .layout
                .as_ref()
                .is_some_and(|l| l.newlines != NewlineMode::Trivia),
        line_comments: Vec::new(),
        block_comments: Vec::new(),
        nested_comments: lexer.nested_default,
        strings: Vec::new(),
    };
    v2.lexer = lexer;
    Some(Schematic {
        format: 2,
        name: identity.name,
        version: identity.version,
        extensions: identity.extensions,
        start: identity.start,
        lexer: format1_lexer,
        rules,
        capabilities,
        v2: Some(Box::new(v2)),
    })
}

/// `[language]` identity keys.
struct Identity<'s> {
    name: Cow<'s, str>,
    version: Option<Cow<'s, str>>,
    extensions: Vec<Cow<'s, str>>,
    start: Option<(Cow<'s, str>, Span)>,
}

fn unknown_section(report: &mut Report, span: Span, key: &str) {
    let message = format!("unknown section `{key}`");
    match suggest::suggest(key, SECTIONS.iter().copied(), 2) {
        Some(help) => report.error_help(codes::UNKNOWN_SECTION, span, message, help),
        None => report.error_help(
            codes::UNKNOWN_SECTION,
            span,
            message,
            "a format-2 sketch's sections are listed in LSF2 §7–§25",
        ),
    }
}

/// Reports an unknown key, with a did-you-mean or the valid names.
pub(crate) fn unknown_key(report: &mut Report, span: Span, key: &str, place: &str, known: &[&str]) {
    let message = format!("unknown key `{key}` in {place}");
    let help = suggest::suggest(key, known.iter().copied(), 2).unwrap_or_else(|| {
        let list = known
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("expected one of {list}")
    });
    report.error_help(codes::UNKNOWN_KEY, span, message, help);
}

fn read_sketch(t: Table<'_>, v2: &mut V2<'_>, report: &mut Report) {
    for entry in t.entries {
        match &*entry.key {
            "format" => {} // checked by `format_of`
            "kind" => {
                if let Some((kind, span)) = schematic::string(entry, report) {
                    v2.role = match &*kind {
                        "language" => Role::Language,
                        "part" => Role::Part,
                        "mixin" => Role::Mixin,
                        other => {
                            report.error(
                                codes::OUT_OF_RANGE,
                                span,
                                format!("unknown sketch kind `{other}`; use `language`, `part`, or `mixin`"),
                            );
                            Role::Language
                        }
                    };
                }
            }
            "modules" => {
                // Kept with the document; the sketch loader reads them.
            }
            "requires" => {
                let span = entry.value.span;
                let _ = schematic::strings(entry, report);
                report.error_help(
                    codes::NOT_SUPPORTED,
                    span,
                    "`requires` (mixins) is not supported by lang-forge 2.0.0-alpha.1",
                    "mixins are composed by `extends`, which is ls-sketch's (ROADMAP: lang-forge alpha.2)",
                );
            }
            "checks" => {
                if let Some(t) = schematic::table(entry, report) {
                    v2.checks = read_checks(t, report);
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[sketch]",
                &["format", "kind", "modules", "requires", "checks"],
            ),
        }
    }
}

/// The `[sketch] modules` of a read document (for the sketch loader), checked
/// for type only.
pub(crate) fn modules<'a>(root: &'a Table<'_>) -> Vec<(&'a str, Span)> {
    let Some(sketch) = root.entries.iter().find(|e| e.key == "sketch") else {
        return Vec::new();
    };
    let ValueKind::Table(t) = &sketch.value.kind else {
        return Vec::new();
    };
    let Some(modules) = t.entries.iter().find(|e| e.key == "modules") else {
        return Vec::new();
    };
    match &modules.value.kind {
        ValueKind::Array(items) => items
            .iter()
            .filter_map(|v| match &v.kind {
                ValueKind::Str(text) => Some((&*text.text, v.span)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn read_checks(t: Table<'_>, report: &mut Report) -> Checks {
    let mut checks = Checks::default();
    for entry in t.entries {
        let key = entry.key.clone();
        let slot = match &*key {
            "overlap" => Some(&mut checks.overlap),
            "unused_rule" => Some(&mut checks.unused_rule),
            "unused_token" => Some(&mut checks.unused_token),
            // `unmapped_node` belongs to the semantics stage (ls-sema).
            "unmapped_node" => None,
            other => {
                unknown_key(
                    report,
                    entry.key_span,
                    other,
                    "[sketch.checks]",
                    &["overlap", "unused_rule", "unused_token", "unmapped_node"],
                );
                continue;
            }
        };
        if let Some((value, span)) = schematic::string(entry, report) {
            let level = match &*value {
                "deny" => Level::Deny,
                "warn" => Level::Warn,
                "allow" => Level::Allow,
                other => {
                    report.error(
                        codes::OUT_OF_RANGE,
                        span,
                        format!(
                            "`{key}` must be \"deny\", \"warn\", or \"allow\", found `{other}`"
                        ),
                    );
                    continue;
                }
            };
            if let Some(slot) = slot {
                *slot = level;
            }
        }
    }
    checks
}

fn read_language<'s>(t: Table<'s>, v2: &mut V2<'s>, report: &mut Report) -> Option<Identity<'s>> {
    let header = t.span;
    let mut name = None;
    let mut name_given = false;
    let mut version = None;
    let mut extensions = Vec::new();
    let mut start = None;
    let mut seen_ext: BTreeSet<Cow<'s, str>> = BTreeSet::new();
    for entry in t.entries {
        match &*entry.key {
            "name" => {
                name_given = true;
                if let Some((text, span)) = schematic::string(entry, report) {
                    if check_name(&text, NameKind::Language, span, report) {
                        name = Some(text);
                    }
                }
            }
            "display_name" | "description" => {
                let key = entry.key.clone();
                if let Some((text, span)) = schematic::string(entry, report) {
                    let limit = if key == "description" { 64 << 10 } else { 4096 };
                    check_free_text(&key, &text, limit, span, report);
                    if key == "description" {
                        v2.description = Some(text);
                    } else {
                        v2.display_name = Some(text);
                    }
                }
            }
            "version" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    if !is_semver(&text) {
                        report.error_help(
                            codes::VERSION,
                            span,
                            format!("`version` `{text}` is not a SemVer version"),
                            "write MAJOR.MINOR.PATCH, such as `1.2.0`",
                        );
                    }
                    version = Some(text);
                }
            }
            "edition" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    let ok = text.len() <= 32
                        && text
                            .bytes()
                            .next()
                            .is_some_and(|b| b.is_ascii_alphanumeric())
                        && text
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
                    if !ok {
                        report.error(
                            codes::EDITION,
                            span,
                            format!(
                                "edition `{text}` must match `[0-9A-Za-z][0-9A-Za-z.-]{{0,31}}`"
                            ),
                        );
                    }
                    v2.edition = Some(text);
                }
            }
            "extensions" => {
                for (ext, span) in schematic::strings(entry, report) {
                    if let Some(bare) = ext.strip_prefix('.') {
                        report.error_help(
                            codes::EXTENSION,
                            span,
                            format!("extension `{ext}` starts with a dot"),
                            format!("write `{bare}`"),
                        );
                        continue;
                    }
                    let ok = !ext.is_empty()
                        && ext.len() <= 32
                        && ext
                            .bytes()
                            .next()
                            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                        && ext.bytes().all(|b| {
                            b.is_ascii_lowercase()
                                || b.is_ascii_digit()
                                || matches!(b, b'_' | b'.' | b'-')
                        });
                    if !ok {
                        report.error(
                            codes::EXTENSION,
                            span,
                            format!("extension `{ext}` must match `[a-z0-9][a-z0-9_.-]{{0,31}}`"),
                        );
                    } else if !seen_ext.insert(ext.clone()) {
                        report.error(
                            codes::EXTENSION_TWICE,
                            span,
                            format!("extension `{ext}` is listed twice"),
                        );
                    } else {
                        extensions.push(ext);
                    }
                }
            }
            "start" => start = schematic::string(entry, report),
            "extends" => {
                let span = entry.value.span;
                let _ = schematic::strings(entry, report);
                report.error_help(
                    codes::NOT_SUPPORTED,
                    span,
                    "`extends` is not supported by lang-forge 2.0.0-alpha.1",
                    "composition is ls-sketch's; forge the composed sketch (ROADMAP: lang-forge alpha.2)",
                );
            }
            "files" => {
                if let Some(t) = schematic::table(entry, report) {
                    v2.files = read_files(t, report);
                }
            }
            "shebang_names" => {
                for (n, span) in schematic::strings(entry, report) {
                    let ok = !n.is_empty()
                        && n.bytes().all(|b| {
                            b.is_ascii_lowercase()
                                || b.is_ascii_digit()
                                || matches!(b, b'_' | b'.' | b'-')
                        });
                    if ok {
                        v2.shebang_names.push(n);
                    } else {
                        report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!("shebang name `{n}` must match `[a-z0-9_.-]+`"),
                        );
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[language]",
                &[
                    "name",
                    "display_name",
                    "description",
                    "version",
                    "edition",
                    "extensions",
                    "start",
                    "extends",
                    "files",
                    "shebang_names",
                ],
            ),
        }
    }
    if !name_given {
        report.error(codes::MISSING, header, "missing `name` in [language]");
    }
    if version.is_none() {
        report.error(
            codes::MISSING,
            header,
            "missing `version` in [language] (format 2 requires it)",
        );
    }
    for file in &v2.files {
        if !extensions.contains(&file.extension.0) {
            report.error(
                codes::FILES_ENTRY,
                file.extension.1,
                format!(
                    "`files` names extension `{}`, which `extensions` does not list",
                    file.extension.0
                ),
            );
        }
    }
    Some(Identity {
        name: name?,
        version,
        extensions,
        start,
    })
}

fn read_files<'s>(t: Table<'s>, report: &mut Report) -> Vec<FileSpec<'s>> {
    let mut out = Vec::new();
    for entry in t.entries {
        let extension = (entry.key.clone(), entry.key_span);
        let Some(t) = schematic::table(entry, report) else {
            continue;
        };
        let mut file = FileSpec {
            extension,
            mode: None,
            start: None,
        };
        for entry in t.entries {
            match &*entry.key {
                "mode" => file.mode = schematic::string(entry, report),
                "start" => file.start = schematic::string(entry, report),
                other => unknown_key(
                    report,
                    entry.key_span,
                    other,
                    "a `files` entry",
                    &["mode", "start"],
                ),
            }
        }
        out.push(file);
    }
    out
}

fn read_lexer<'s>(t: Table<'s>, report: &mut Report) -> Lexer2<'s> {
    let mut spec = Lexer2::default();
    let mut array_strings = None;
    let mut named_strings = None;
    for entry in t.entries {
        let key_span = entry.key_span;
        match &*entry.key {
            "identifiers" => spec.ident = read_identifiers(entry, report),
            "newlines" => spec.newlines = schematic::boolean(entry, report).unwrap_or(false),
            "nested_comments" => {
                spec.nested_default = schematic::boolean(entry, report).unwrap_or(false);
            }
            "line_comments" | "block_comments" => {
                let block = entry.key == "block_comments";
                let Some(items) = schematic::array(entry, report) else {
                    continue;
                };
                for item in items {
                    if let Some(c) = read_comment(item, block, report) {
                        spec.comments.push(c);
                    }
                }
            }
            "strings" => {
                if let ValueKind::Table(t) = entry.value.kind {
                    named_strings = Some(key_span);
                    for class in t.entries {
                        if let Some(c) = read_string_class(class, report) {
                            spec.classes.push(c);
                        }
                    }
                } else {
                    array_strings = Some(key_span);
                    let Some(items) = schematic::array(entry, report) else {
                        continue;
                    };
                    for item in items {
                        if let Some(s) = schematic::string_spec(item, report) {
                            spec.strings_array.push(s);
                        }
                    }
                }
            }
            "brackets" => {
                if let Some(pairs) = pairs(entry, report) {
                    spec.brackets = Some(pairs);
                }
            }
            "initial_mode" => spec.initial_mode = schematic::string(entry, report),
            "shebang" => spec.shebang = schematic::boolean(entry, report).unwrap_or(false),
            "max_mode_depth" => {
                if let Some(n) = integer(entry, 1, 4096, report) {
                    spec.max_mode_depth = n as u32;
                }
            }
            "keywords" => {
                if let Some(t) = schematic::table(entry, report) {
                    spec.keywords = read_keywords(t, report);
                }
            }
            "numbers" => {
                if let Some(t) = schematic::table(entry, report) {
                    spec.numbers = read_numbers(t, report);
                }
            }
            "tokens" => {
                if let Some(t) = schematic::table(entry, report) {
                    for token in t.entries {
                        if let Some(token) = read_token(token, report) {
                            spec.tokens.push(token);
                        }
                    }
                }
            }
            "modes" => {
                if let Some(t) = schematic::table(entry, report) {
                    for mode in t.entries {
                        if let Some(mode) = read_mode(mode, report) {
                            spec.modes.push(mode);
                        }
                    }
                }
            }
            "split" => {
                let span = entry.key_span;
                if let Some(t) = schematic::table(entry, report) {
                    for split in t.entries {
                        let _ = schematic::strings(split, report);
                    }
                }
                report.error_help(
                    codes::NOT_SUPPORTED,
                    span,
                    "`[lexer.split]` (token splitting) is not supported by lang-forge 2.0.0-alpha.1",
                    "splitting a token the parser asks for needs split state in speculation and the memo (ROADMAP: lang-forge alpha.2)",
                );
            }
            "columns" => {
                if let Some(t) = schematic::table(entry, report) {
                    for entry in t.entries {
                        match &*entry.key {
                            "tab_width" => {
                                if let Some(n) = integer(entry, 1, 16, report) {
                                    spec.tab_width = n as u32;
                                }
                            }
                            "trivia" => report.error_help(
                                codes::NOT_SUPPORTED,
                                entry.key_span,
                                "`[lexer.columns] trivia` is not supported by lang-forge 2.0.0-alpha.1",
                                "column-range trivia (fixed-form sources) is ROADMAP: lang-forge alpha.2",
                            ),
                            other => unknown_key(
                                report,
                                entry.key_span,
                                other,
                                "[lexer.columns]",
                                &["tab_width", "trivia"],
                            ),
                        }
                    }
                }
            }
            other => unknown_key(
                report,
                key_span,
                other,
                "[lexer]",
                &[
                    "identifiers",
                    "newlines",
                    "line_comments",
                    "block_comments",
                    "nested_comments",
                    "strings",
                    "brackets",
                    "initial_mode",
                    "shebang",
                    "max_mode_depth",
                    "keywords",
                    "numbers",
                    "tokens",
                    "modes",
                    "split",
                    "columns",
                ],
            ),
        }
    }
    if let (Some(_), Some(named)) = (array_strings, named_strings) {
        report.error(
            codes::STRINGS_MIXED,
            named,
            "use either `strings = [...]` or named string classes `[lexer.strings.<CLASS>]`",
        );
    }
    spec
}

fn read_identifiers(entry: Entry<'_>, report: &mut Report) -> IdentSpec {
    let span = entry.value.span;
    let mut spec = IdentSpec {
        span,
        ..IdentSpec::default()
    };
    let style = |text: &str, span: Span, report: &mut Report| match text {
        "xid" => Some(IdentMode::Xid),
        "ascii" => Some(IdentMode::Ascii),
        other => {
            report.error_help(
                codes::IDENT_STYLE,
                span,
                format!("unknown identifier style `{other}`"),
                "use \"xid\" (Unicode identifiers) or \"ascii\" (or a table)",
            );
            None
        }
    };
    match entry.value.kind {
        ValueKind::Str(text) => {
            if let Some(mode) = style(&text.text, span, report) {
                spec.mode = mode;
            }
        }
        ValueKind::Table(t) => {
            for entry in t.entries {
                match &*entry.key {
                    "style" => {
                        if let Some((text, span)) = schematic::string(entry, report) {
                            if let Some(mode) = style(&text, span, report) {
                                spec.mode = mode;
                            }
                        }
                    }
                    "extra_start" | "extra_continue" => {
                        let start = entry.key == "extra_start";
                        if let Some((text, span)) = schematic::string(entry, report) {
                            let chars: Vec<char> = text.chars().collect();
                            if let Some(bad) = chars.iter().find(|c| {
                                c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'')
                            }) {
                                report.error(
                                    codes::EXTRA_CHARS,
                                    span,
                                    format!(
                                        "`{}` cannot be part of an identifier",
                                        bad.escape_debug()
                                    ),
                                );
                            } else if start {
                                spec.extra_start = chars;
                            } else {
                                spec.extra_continue = chars;
                            }
                        }
                    }
                    "normalize" => {
                        if let Some((text, span)) = schematic::string(entry, report) {
                            match &*text {
                                "none" | "nfc" => {}
                                "require-nfc" => spec.require_nfc = true,
                                other => report.error(
                                    codes::OUT_OF_RANGE,
                                    span,
                                    format!("`normalize` must be \"none\", \"require-nfc\", or \"nfc\", found `{other}`"),
                                ),
                            }
                        }
                    }
                    "confusables" => {
                        if let Some((text, span)) = schematic::string(entry, report) {
                            match &*text {
                                "allow" => {}
                                "warn" | "deny" => report.warning(
                                    codes::NOT_SUPPORTED,
                                    span,
                                    "`confusables` is not checked: unicode-lang has no UAX #39 data yet (LSF2 §31)",
                                ),
                                other => report.error(
                                    codes::OUT_OF_RANGE,
                                    span,
                                    format!("`confusables` must be \"allow\", \"warn\", or \"deny\", found `{other}`"),
                                ),
                            }
                        }
                    }
                    other => unknown_key(
                        report,
                        entry.key_span,
                        other,
                        "`identifiers`",
                        &[
                            "style",
                            "extra_start",
                            "extra_continue",
                            "normalize",
                            "confusables",
                        ],
                    ),
                }
            }
        }
        other => report.error(
            codes::WRONG_TYPE,
            span,
            format!(
                "`identifiers` must be a string or a table, found {}",
                other.type_name()
            ),
        ),
    }
    spec
}

fn read_comment<'s>(item: Value<'s>, block: bool, report: &mut Report) -> Option<CommentSpec<'s>> {
    let span = item.span;
    let mut spec = CommentSpec {
        open: Cow::Borrowed(""),
        close: None,
        doc: false,
        nested: None,
        not_followed_by: None,
        stop_before: Vec::new(),
        modes: None,
        span,
    };
    match item.kind {
        ValueKind::Str(t) if !block => spec.open = t.text,
        ValueKind::Array(_) if block => {
            let pair = schematic::block_comment(item, report)?;
            spec.open = pair.open;
            spec.close = Some(pair.close);
            return Some(spec);
        }
        ValueKind::Table(t) => {
            let mut open = None;
            for entry in t.entries {
                match &*entry.key {
                    "open" => open = schematic::string(entry, report),
                    "close" if block => spec.close = schematic::string(entry, report).map(|c| c.0),
                    "doc" => spec.doc = schematic::boolean(entry, report).unwrap_or(false),
                    "nested" if block => spec.nested = schematic::boolean(entry, report),
                    "not_followed_by" => spec.not_followed_by = schematic::string(entry, report),
                    "stop_before" if !block => spec.stop_before = schematic::strings(entry, report),
                    "modes" => spec.modes = Some(schematic::strings(entry, report)),
                    other => unknown_key(
                        report,
                        entry.key_span,
                        other,
                        if block {
                            "a block comment"
                        } else {
                            "a line comment"
                        },
                        if block {
                            &["open", "close", "doc", "nested", "not_followed_by", "modes"]
                        } else {
                            &["open", "doc", "not_followed_by", "stop_before", "modes"]
                        },
                    ),
                }
            }
            let Some((open, _)) = open else {
                report.error(codes::MISSING, span, "a comment needs an `open` delimiter");
                return None;
            };
            spec.open = open;
            if block && spec.close.is_none() {
                report.error(
                    codes::MISSING,
                    span,
                    "a block comment needs a `close` delimiter",
                );
                return None;
            }
        }
        other => {
            report.error_help(
                codes::WRONG_TYPE,
                span,
                format!(
                    "a {} comment must be {}, found {}",
                    if block { "block" } else { "line" },
                    if block {
                        "a pair or a table"
                    } else {
                        "a string or a table"
                    },
                    other.type_name()
                ),
                if block {
                    "write [\"/*\", \"*/\"] or { open = \"/*\", close = \"*/\" }"
                } else {
                    "write \"//\" or { open = \"//\" }"
                },
            );
            return None;
        }
    }
    if spec.open.is_empty() || spec.close.as_deref() == Some("") {
        report.error(codes::DELIMITER, span, "a comment delimiter is empty");
        return None;
    }
    Some(spec)
}

fn read_parts<'s>(entry: Entry<'s>, report: &mut Report) -> Vec<(Part<'s>, Span)> {
    let span = entry.value.span;
    match entry.value.kind {
        ValueKind::Str(t) => {
            if t.text.is_empty() {
                report.error(codes::DELIMITER, span, "a string delimiter is empty");
                return Vec::new();
            }
            Vec::from([(Part::Text(t.text), span)])
        }
        ValueKind::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                let span = item.span;
                match item.kind {
                    ValueKind::Str(t) if !t.text.is_empty() => parts.push((Part::Text(t.text), span)),
                    ValueKind::Table(t) if t.entries.len() == 1 => {
                        let Some(entry) = t.entries.into_iter().next() else {
                            continue;
                        };
                        let part = match &*entry.key {
                            "regex" => schematic::string(entry, report).map(|(r, _)| Part::Regex(r)),
                            "capture" => schematic::string(entry, report).map(|(r, _)| Part::Capture(r)),
                            "backref" => match schematic::boolean(entry, report) {
                                Some(true) => Some(Part::Backref),
                                Some(false) => {
                                    report.error(codes::OUT_OF_RANGE, span, "write `{ backref = true }`");
                                    None
                                }
                                None => None,
                            },
                            "newline" => match schematic::boolean(entry, report) {
                                Some(true) => Some(Part::Newline),
                                Some(false) => {
                                    report.error(codes::OUT_OF_RANGE, span, "write `{ newline = true }`");
                                    None
                                }
                                None => None,
                            },
                            other => {
                                unknown_key(
                                    report,
                                    entry.key_span,
                                    other,
                                    "a delimiter part",
                                    &["regex", "capture", "backref", "newline"],
                                );
                                None
                            }
                        };
                        if let Some(part) = part {
                            parts.push((part, span));
                        }
                    }
                    other => report.error_help(
                        codes::WRONG_TYPE,
                        span,
                        format!("a delimiter part must be a non-empty string or a one-key table, found {}", other.type_name()),
                        "parts are \"text\", { regex = \"...\" }, { capture = \"...\" }, { backref = true }, { newline = true }",
                    ),
                }
            }
            parts
        }
        other => {
            report.error(
                codes::WRONG_TYPE,
                span,
                format!(
                    "a delimiter must be a string or an array of parts, found {}",
                    other.type_name()
                ),
            );
            Vec::new()
        }
    }
}

fn read_string_class<'s>(entry: Entry<'s>, report: &mut Report) -> Option<StringClass<'s>> {
    let name = entry.key.clone();
    let name_span = entry.key_span;
    let span = entry.value.span;
    let ok_name = check_name(&name, NameKind::Class, name_span, report);
    let t = schematic::table(entry, report)?;
    let mut class = StringClass {
        name,
        name_span,
        open: Vec::new(),
        close: Vec::new(),
        escape: Some('\\'),
        multiline: false,
        interpolate: Vec::new(),
        embedded: Vec::new(),
        escape_tokens: false,
        next_line: false,
        close_at: CloseAt::Anywhere,
        close_not_followed_by: None,
        modes: None,
        span,
    };
    let mut close_given = false;
    for entry in t.entries {
        let key_span = entry.key_span;
        match &*entry.key {
            "open" => class.open = read_parts(entry, report),
            "close" => {
                close_given = true;
                class.close = read_parts(entry, report);
            }
            "escape" => {
                if let Some((text, at)) = schematic::string(entry, report) {
                    let mut chars = text.chars();
                    class.escape = match (chars.next(), chars.next()) {
                        (None, _) => None,
                        (Some(c), None) if !c.is_whitespace() && !c.is_control() => Some(c),
                        _ => {
                            report.error_help(
                                codes::ESCAPE,
                                at,
                                format!("escape `{text}` is not a single character"),
                                "use one character such as \"\\\\\", or \"\" for none",
                            );
                            None
                        }
                    };
                }
            }
            "escapes" => check_escapes(entry, report),
            "multiline" => class.multiline = schematic::boolean(entry, report).unwrap_or(false),
            "interpolate" => {
                let Some(items) = schematic::array(entry, report) else {
                    continue;
                };
                for item in items {
                    if let Some(interp) = read_interp(item, report) {
                        class.interpolate.push(interp);
                    }
                }
            }
            "embedded" => {
                let Some(items) = schematic::array(entry, report) else {
                    continue;
                };
                for item in items {
                    if let Some(e) = read_embedded(item, report) {
                        class.embedded.push(e);
                    }
                }
            }
            "escape_tokens" => {
                class.escape_tokens = schematic::boolean(entry, report).unwrap_or(false)
            }
            "body" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    match &*text {
                        "inline" => class.next_line = false,
                        "next-line" => class.next_line = true,
                        other => report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!("`body` must be \"inline\" or \"next-line\", found `{other}`"),
                        ),
                    }
                }
            }
            "rest_of_line" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    match &*text {
                        "empty" => {}
                        "code" => report.error_help(
                            codes::NOT_SUPPORTED,
                            span,
                            "`rest_of_line = \"code\"` is not supported by lang-forge 2.0.0-alpha.1",
                            "heredocs whose opener line continues with code (shell, Ruby) need the pending-heredoc queue (ROADMAP: lang-forge alpha.2)",
                        ),
                        other => report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!("`rest_of_line` must be \"empty\" or \"code\", found `{other}`"),
                        ),
                    }
                }
            }
            "close_at" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    class.close_at = match &*text {
                        "anywhere" => CloseAt::Anywhere,
                        "line-start" => CloseAt::LineStart,
                        "line-start-indented" => CloseAt::LineStartIndented,
                        other => {
                            report.error(
                                codes::OUT_OF_RANGE,
                                span,
                                format!("`close_at` must be \"anywhere\", \"line-start\", or \"line-start-indented\", found `{other}`"),
                            );
                            CloseAt::Anywhere
                        }
                    };
                }
            }
            "close_not_followed_by" => {
                class.close_not_followed_by = schematic::string(entry, report)
            }
            "dedent" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    if !matches!(&*text, "none" | "closing-indent") {
                        report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!(
                                "`dedent` must be \"none\" or \"closing-indent\", found `{text}`"
                            ),
                        );
                    }
                }
            }
            "modes" => class.modes = Some(schematic::strings(entry, report)),
            other => unknown_key(
                report,
                key_span,
                other,
                "a string class",
                &[
                    "open",
                    "close",
                    "escape",
                    "escapes",
                    "multiline",
                    "interpolate",
                    "embedded",
                    "escape_tokens",
                    "body",
                    "rest_of_line",
                    "close_at",
                    "close_not_followed_by",
                    "dedent",
                    "modes",
                ],
            ),
        }
    }
    if class.open.is_empty() {
        report.error(
            codes::MISSING,
            span,
            format!("string class `{}` needs an `open` delimiter", class.name),
        );
        return None;
    }
    if !close_given {
        match class.open.as_slice() {
            [(Part::Text(t), s)] => class.close = Vec::from([(Part::Text(t.clone()), *s)]),
            _ => {
                report.error(
                    codes::STRING_CLOSE,
                    span,
                    format!(
                        "string class `{}` has an opener with parts, so it needs a `close`",
                        class.name
                    ),
                );
                return None;
            }
        }
    }
    if class.close.is_empty() {
        return None;
    }
    ok_name.then_some(class)
}

/// Checks the shape of an `escapes` value (LSF2 §9.5.6). Decoding a
/// literal's value is lower-lang's; the lexer never depends on it.
fn check_escapes(entry: Entry<'_>, report: &mut Report) {
    let span = entry.value.span;
    match entry.value.kind {
        ValueKind::Str(t) => {
            if !matches!(&*t.text, "none" | "quotes" | "standard") {
                report.error(
                    codes::OUT_OF_RANGE,
                    span,
                    format!("`escapes` must be \"none\", \"quotes\", \"standard\", or a table, found `{}`", t.text),
                );
            }
        }
        ValueKind::Table(t) => {
            for entry in t.entries {
                match &*entry.key {
                    "simple" => {
                        if let Some(t) = schematic::table(entry, report) {
                            for e in t.entries {
                                let key_span = e.key_span;
                                let key = e.key.clone();
                                let _ = schematic::string(e, report);
                                if key.chars().count() != 1 {
                                    report.error(
                                        codes::ESCAPE,
                                        key_span,
                                        format!("simple escape `{key}` is not one character"),
                                    );
                                }
                            }
                        }
                    }
                    "octal" => {
                        let _ = integer(entry, 0, 3, report);
                    }
                    "hex" | "unicode" => {
                        let _ = schematic::table(entry, report);
                    }
                    "unknown" => {
                        if let Some((text, span)) = schematic::string(entry, report) {
                            if !matches!(&*text, "error" | "keep") {
                                report.error(
                                    codes::OUT_OF_RANGE,
                                    span,
                                    format!(
                                        "`unknown` must be \"error\" or \"keep\", found `{text}`"
                                    ),
                                );
                            }
                        }
                    }
                    "bytes" => {
                        let _ = schematic::boolean(entry, report);
                    }
                    other => unknown_key(
                        report,
                        entry.key_span,
                        other,
                        "`escapes`",
                        &["simple", "octal", "hex", "unicode", "unknown", "bytes"],
                    ),
                }
            }
        }
        other => report.error(
            codes::WRONG_TYPE,
            span,
            format!(
                "`escapes` must be a string or a table, found {}",
                other.type_name()
            ),
        ),
    }
}

fn read_interp<'s>(item: Value<'s>, report: &mut Report) -> Option<Interp<'s>> {
    let span = item.span;
    let ValueKind::Table(t) = item.kind else {
        report.error(
            codes::WRONG_TYPE,
            span,
            "an `interpolate` entry must be a table",
        );
        return None;
    };
    let (mut open, mut close, mut rule, mut mode, mut when_next) = (None, None, None, None, None);
    for entry in t.entries {
        match &*entry.key {
            "open" => open = schematic::string(entry, report),
            "close" => close = schematic::string(entry, report),
            "rule" => rule = schematic::string(entry, report),
            "mode" => mode = schematic::string(entry, report),
            "when_next" => when_next = schematic::string(entry, report),
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "an `interpolate` entry",
                &["open", "close", "rule", "mode", "when_next"],
            ),
        }
    }
    let (Some((open, open_span)), Some((close, close_span)), Some(rule)) = (open, close, rule)
    else {
        report.error(
            codes::MISSING,
            span,
            "an `interpolate` entry needs `open`, `close`, and `rule`",
        );
        return None;
    };
    if open.is_empty() {
        report.error(
            codes::DELIMITER,
            open_span,
            "an interpolation opener is empty",
        );
        return None;
    }
    if close.is_empty() {
        report.error(
            codes::HOLE_CLOSE,
            close_span,
            "an interpolation's close text is empty",
        );
        return None;
    }
    Some(Interp {
        open,
        close,
        rule,
        mode,
        when_next,
        span,
    })
}

fn read_embedded<'s>(item: Value<'s>, report: &mut Report) -> Option<Embedded<'s>> {
    let span = item.span;
    let ValueKind::Table(t) = item.kind else {
        report.error(
            codes::WRONG_TYPE,
            span,
            "an `embedded` entry must be a table",
        );
        return None;
    };
    let (mut token, mut regex, mut parse) = (None, None, None);
    for entry in t.entries {
        match &*entry.key {
            "token" => token = schematic::string(entry, report),
            "regex" => regex = schematic::string(entry, report),
            "parse" => parse = schematic::string(entry, report),
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "an `embedded` entry",
                &["token", "regex", "parse"],
            ),
        }
    }
    let (Some(token), Some(regex)) = (token, regex) else {
        report.error(
            codes::MISSING,
            span,
            "an `embedded` entry needs `token` and `regex`",
        );
        return None;
    };
    check_name(&token.0, NameKind::Class, token.1, report).then_some(Embedded {
        token,
        regex,
        parse,
    })
}

fn read_keywords<'s>(t: Table<'s>, report: &mut Report) -> KeywordsSpec<'s> {
    let mut spec = KeywordsSpec::default();
    for entry in t.entries {
        match &*entry.key {
            "default" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    match &*text {
                        "reserved" => spec.default_contextual = false,
                        "contextual" => spec.default_contextual = true,
                        other => report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!(
                                "`default` must be \"reserved\" or \"contextual\", found `{other}`"
                            ),
                        ),
                    }
                }
            }
            "contextual" => spec.contextual = schematic::strings(entry, report),
            "reserved" => spec.reserved = schematic::strings(entry, report),
            "case" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    match &*text {
                        "sensitive" => spec.case_insensitive = false,
                        "ascii-insensitive" => spec.case_insensitive = true,
                        other => report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!("`case` must be \"sensitive\" or \"ascii-insensitive\", found `{other}`"),
                        ),
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[lexer.keywords]",
                &["default", "contextual", "reserved", "case"],
            ),
        }
    }
    spec
}

fn read_numbers(t: Table<'_>, report: &mut Report) -> NumbersSpec {
    let mut spec = NumbersSpec::default();
    for entry in t.entries {
        let key = entry.key.clone();
        match &*key {
            "radix" => {
                spec.radix = [false; 3];
                for (r, span) in schematic::strings(entry, report) {
                    match &*r {
                        "0x" => spec.radix[0] = true,
                        "0o" => spec.radix[1] = true,
                        "0b" => spec.radix[2] = true,
                        other => report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!("radix `{other}` must be \"0x\", \"0o\", or \"0b\""),
                        ),
                    }
                }
            }
            "radix_case" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    match &*text {
                        "lower" => spec.radix_any_case = false,
                        "any" => spec.radix_any_case = true,
                        other => report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!("`radix_case` must be \"lower\" or \"any\", found `{other}`"),
                        ),
                    }
                }
            }
            "separators" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    let mut chars = text.chars();
                    match (chars.next(), chars.next()) {
                        (None, _) => spec.separator = None,
                        (Some(c), None) if !c.is_alphanumeric() && !c.is_whitespace() && c != '.' => spec.separator = Some(c),
                        _ => report.error(
                            codes::SEPARATORS,
                            span,
                            "`separators` must be one character (not a letter, digit, `.` or space) or empty",
                        ),
                    }
                }
            }
            "separator_rule" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    match &*text {
                        "between-digits" => spec.separator_anywhere = false,
                        "anywhere" => spec.separator_anywhere = true,
                        other => report.error(codes::OUT_OF_RANGE, span, format!("`separator_rule` must be \"between-digits\" or \"anywhere\", found `{other}`")),
                    }
                }
            }
            "floats" | "exponent" | "leading_dot" | "trailing_dot" | "hex_floats" => {
                let value = schematic::boolean(entry, report).unwrap_or(false);
                match &*key {
                    "floats" => spec.floats = value,
                    "exponent" => spec.exponent = value,
                    "leading_dot" => spec.leading_dot = value,
                    "trailing_dot" => spec.trailing_dot = value,
                    _ => spec.hex_floats = value,
                }
            }
            "leading_zeros" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    spec.leading_zeros = match &*text {
                        "decimal" => LeadingZeros::Decimal,
                        "error" => LeadingZeros::Error,
                        "octal" => LeadingZeros::Octal,
                        other => {
                            report.error(codes::OUT_OF_RANGE, span, format!("`leading_zeros` must be \"decimal\", \"error\", or \"octal\", found `{other}`"));
                            LeadingZeros::Decimal
                        }
                    };
                }
            }
            "suffixes" => {
                for (suffix, span) in schematic::strings(entry, report) {
                    let ok = suffix
                        .bytes()
                        .next()
                        .is_some_and(|b| b.is_ascii_lowercase())
                        && suffix
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
                    if ok {
                        spec.suffixes.push(String::from(&*suffix));
                    } else {
                        report.error(
                            codes::SUFFIX,
                            span,
                            format!("suffix `{suffix}` must match `[a-z][a-z0-9_]*`"),
                        );
                    }
                }
            }
            "run_on" => {
                if let Some((text, span)) = schematic::string(entry, report) {
                    if text != "error" {
                        report.error(
                            codes::OUT_OF_RANGE,
                            span,
                            format!("`run_on` must be \"error\", found `{text}`"),
                        );
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[lexer.numbers]",
                &[
                    "radix",
                    "radix_case",
                    "separators",
                    "separator_rule",
                    "floats",
                    "exponent",
                    "leading_dot",
                    "trailing_dot",
                    "hex_floats",
                    "leading_zeros",
                    "suffixes",
                    "run_on",
                ],
            ),
        }
    }
    spec
}

fn read_action<'s>(
    text: Cow<'s, str>,
    span: Span,
    report: &mut Report,
) -> Option<(ActionSpec<'s>, Span)> {
    let mut words = text.split_whitespace();
    let action = match (words.next(), words.next(), words.next()) {
        (Some("pop"), None, None) => ActionSpec::Pop,
        (Some(verb @ ("push" | "switch")), Some(mode), None) => {
            let mode = match &text {
                Cow::Borrowed(t) => {
                    let at = t.rfind(mode).unwrap_or(0);
                    Cow::Borrowed(&t[at..at + mode.len()])
                }
                Cow::Owned(_) => Cow::Owned(String::from(mode)),
            };
            if verb == "push" {
                ActionSpec::Push(mode)
            } else {
                ActionSpec::Switch(mode)
            }
        }
        _ => {
            report.error_help(
                codes::OUT_OF_RANGE,
                span,
                format!("unknown lexer action `{text}`"),
                "actions are \"push <mode>\", \"pop\", and \"switch <mode>\"",
            );
            return None;
        }
    };
    Some((action, span))
}

fn read_token<'s>(entry: Entry<'s>, report: &mut Report) -> Option<TokenSpec<'s>> {
    let name = entry.key.clone();
    let name_span = entry.key_span;
    let span = entry.value.span;
    let ok = check_name(&name, NameKind::Class, name_span, report);
    let t = schematic::table(entry, report)?;
    let mut sources: Vec<(TokenSource<'s>, Span)> = Vec::new();
    let mut token = TokenSpec {
        name,
        name_span,
        source: (TokenSource::Literal(Cow::Borrowed("")), span),
        trivia: false,
        priority: 0,
        modes: None,
        action: None,
        followed_by: None,
        not_followed_by: None,
        when_prev: None,
        unless_prev: None,
        line_start: false,
        indented: false,
        column: None,
    };
    for entry in t.entries {
        let key_span = entry.key_span;
        match &*entry.key {
            "regex" => {
                if let Some((r, s)) = schematic::string(entry, report) {
                    sources.push((TokenSource::Regex(r), s));
                }
            }
            "literal" => {
                if let Some((r, s)) = schematic::string(entry, report) {
                    if r.is_empty() {
                        report.error(codes::LITERAL_SHAPE, s, "a token literal is empty");
                    } else {
                        sources.push((TokenSource::Literal(r), s));
                    }
                }
            }
            "hook" => {
                let s = entry.value.span;
                let _ = schematic::string(entry, report);
                report.error_help(
                    codes::NOT_SUPPORTED,
                    s,
                    "scanner hooks are not supported by lang-forge 2.0.0-alpha.1",
                    "hooks need the capability runtime (ROADMAP: lang-forge alpha.2)",
                );
                sources.push((TokenSource::Literal(Cow::Borrowed("")), s));
            }
            "trivia" => token.trivia = schematic::boolean(entry, report).unwrap_or(false),
            "priority" => {
                if let Some(n) = integer(entry, -100, 100, report) {
                    token.priority = n as i8;
                }
            }
            "modes" => token.modes = Some(schematic::strings(entry, report)),
            "action" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    token.action = read_action(text, s, report);
                }
            }
            "followed_by" => token.followed_by = schematic::string(entry, report),
            "not_followed_by" => token.not_followed_by = schematic::string(entry, report),
            "when_prev" => token.when_prev = Some(schematic::strings(entry, report)),
            "unless_prev" => token.unless_prev = Some(schematic::strings(entry, report)),
            "line_start" => token.line_start = schematic::boolean(entry, report).unwrap_or(false),
            "indented" => token.indented = schematic::boolean(entry, report).unwrap_or(false),
            "column" => {
                let s = entry.value.span;
                match entry.value.kind {
                    ValueKind::Number(Some(n)) if (1..=4096).contains(&n) => {
                        token.column = Some((n as u32, n as u32))
                    }
                    ValueKind::Array(items) if items.len() == 2 => {
                        let bounds: Vec<i64> = items
                            .iter()
                            .filter_map(|v| match v.kind {
                                ValueKind::Number(Some(n)) => Some(n),
                                _ => None,
                            })
                            .collect();
                        match bounds.as_slice() {
                            [lo, hi] if 1 <= *lo && lo <= hi && *hi <= 4096 => {
                                token.column = Some((*lo as u32, *hi as u32))
                            }
                            _ => report.error(
                                codes::OUT_OF_RANGE,
                                s,
                                "`column` must be [min, max] with 1 ≤ min ≤ max ≤ 4096",
                            ),
                        }
                    }
                    _ => report.error(
                        codes::OUT_OF_RANGE,
                        s,
                        "`column` must be an integer 1–4096 or [min, max]",
                    ),
                }
            }
            "doc" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    check_free_text("doc", &text, 64 << 10, s, report);
                }
            }
            other => unknown_key(
                report,
                key_span,
                other,
                "a token class",
                &[
                    "regex",
                    "literal",
                    "hook",
                    "trivia",
                    "priority",
                    "modes",
                    "action",
                    "followed_by",
                    "not_followed_by",
                    "when_prev",
                    "unless_prev",
                    "line_start",
                    "indented",
                    "column",
                    "doc",
                ],
            ),
        }
    }
    if sources.len() != 1 {
        report.error(
            codes::CLASS_SOURCE,
            span,
            format!(
                "token class `{}` needs exactly one of `regex`, `literal`, or `hook`",
                token.name
            ),
        );
        return None;
    }
    token.source = sources.pop()?;
    ok.then_some(token)
}

fn read_mode<'s>(entry: Entry<'s>, report: &mut Report) -> Option<ModeSpec<'s>> {
    let name = entry.key.clone();
    let name_span = entry.key_span;
    let ok = check_name(&name, NameKind::Mode, name_span, report);
    let t = schematic::table(entry, report)?;
    let mut mode = ModeSpec {
        name,
        name_span,
        inherit: None,
        literals: None,
        builtins: None,
        trivia: None,
        tokens: Vec::new(),
        strings: Vec::new(),
        text: None,
        actions: Vec::new(),
        eof_error: None,
    };
    for entry in t.entries {
        match &*entry.key {
            "inherit" => mode.inherit = schematic::string(entry, report),
            "literals" => mode.literals = schematic::boolean(entry, report),
            "builtins" => mode.builtins = schematic::boolean(entry, report),
            "trivia" => mode.trivia = schematic::boolean(entry, report),
            "tokens" => mode.tokens = schematic::strings(entry, report),
            "strings" => mode.strings = schematic::strings(entry, report),
            "text" => mode.text = schematic::string(entry, report),
            "actions" => {
                if let Some(t) = schematic::table(entry, report) {
                    for e in t.entries {
                        let key = (e.key.clone(), e.key_span);
                        if let Some((text, s)) = schematic::string(e, report) {
                            if let Some(action) = read_action(text, s, report) {
                                mode.actions.push((key, action));
                            }
                        }
                    }
                }
            }
            "eof" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    match &*text {
                        "ok" => mode.eof_error = Some(false),
                        "error" => mode.eof_error = Some(true),
                        other => report.error(
                            codes::OUT_OF_RANGE,
                            s,
                            format!("`eof` must be \"ok\" or \"error\", found `{other}`"),
                        ),
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "a lexer mode",
                &[
                    "inherit", "literals", "builtins", "trivia", "tokens", "strings", "text",
                    "actions", "eof",
                ],
            ),
        }
    }
    ok.then_some(mode)
}

fn read_layout<'s>(t: Table<'s>, report: &mut Report) -> Option<Layout<'s>> {
    let mut layout = Layout {
        indent: false,
        open_after: Vec::new(),
        implicit_join: None,
        explicit_join: None,
        tab_width: 8,
        mixed_error: true,
        newlines: NewlineMode::Trivia,
        terminate_after: Vec::new(),
        continue_before: Vec::new(),
        ignored_inside: None,
    };
    for entry in t.entries {
        match &*entry.key {
            "style" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    match &*text {
                        "none" => layout.indent = false,
                        "indent" => layout.indent = true,
                        "hook" => report.error_help(
                            codes::NOT_SUPPORTED,
                            s,
                            "`style = \"hook\"` is not supported by lang-forge 2.0.0-alpha.1",
                            "layout hooks need the capability runtime (ROADMAP: lang-forge alpha.2)",
                        ),
                        other => report.error(codes::OUT_OF_RANGE, s, format!("`style` must be \"none\", \"indent\", or \"hook\", found `{other}`")),
                    }
                }
            }
            "hook" => {
                let s = entry.value.span;
                report.error(
                    codes::NOT_SUPPORTED,
                    s,
                    "layout hooks are not supported by lang-forge 2.0.0-alpha.1",
                );
            }
            "open_after" => layout.open_after = schematic::strings(entry, report),
            "implicit_join" => layout.implicit_join = pairs(entry, report),
            "explicit_join" => layout.explicit_join = schematic::string(entry, report),
            "tabs" => {
                if let Some(t) = schematic::table(entry, report) {
                    for e in t.entries {
                        match &*e.key {
                            "width" => {
                                if let Some(n) = integer(e, 1, 16, report) {
                                    layout.tab_width = n as u32;
                                }
                            }
                            "mixed" => {
                                if let Some((text, s)) = schematic::string(e, report) {
                                    match &*text {
                                        "error" => layout.mixed_error = true,
                                        "allow" => layout.mixed_error = false,
                                        other => report.error(codes::OUT_OF_RANGE, s, format!("`mixed` must be \"error\" or \"allow\", found `{other}`")),
                                    }
                                }
                            }
                            other => unknown_key(
                                report,
                                e.key_span,
                                other,
                                "`tabs`",
                                &["width", "mixed"],
                            ),
                        }
                    }
                }
            }
            "blank_lines" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    if text != "ignore" {
                        report.error(
                            codes::OUT_OF_RANGE,
                            s,
                            format!("`blank_lines` must be \"ignore\", found `{text}`"),
                        );
                    }
                }
            }
            "newlines" => {
                if let Some(t) = schematic::table(entry, report) {
                    for e in t.entries {
                        match &*e.key {
                            "mode" => {
                                if let Some((text, s)) = schematic::string(e, report) {
                                    layout.newlines = match &*text {
                                        "trivia" => NewlineMode::Trivia,
                                        "significant" => NewlineMode::Significant,
                                        "terminators" => NewlineMode::Terminators,
                                        other => {
                                            report.error(codes::OUT_OF_RANGE, s, format!("`mode` must be \"trivia\", \"significant\", or \"terminators\", found `{other}`"));
                                            NewlineMode::Trivia
                                        }
                                    };
                                }
                            }
                            "terminate_after" => {
                                layout.terminate_after = schematic::strings(e, report)
                            }
                            "continue_before" => {
                                layout.continue_before = schematic::strings(e, report)
                            }
                            "ignored_inside" => layout.ignored_inside = pairs(e, report),
                            "soft_terminators" => {
                                let s = e.value.span;
                                report.error_help(
                                    codes::NOT_SUPPORTED,
                                    s,
                                    "`soft_terminators` is not supported by lang-forge 2.0.0-alpha.1",
                                    "zero-width soft terminators are a parser extension (ROADMAP: lang-forge alpha.2)",
                                );
                            }
                            other => unknown_key(
                                report,
                                e.key_span,
                                other,
                                "[layout.newlines]",
                                &[
                                    "mode",
                                    "terminate_after",
                                    "continue_before",
                                    "soft_terminators",
                                    "ignored_inside",
                                ],
                            ),
                        }
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "[layout]",
                &[
                    "style",
                    "hook",
                    "open_after",
                    "implicit_join",
                    "explicit_join",
                    "tabs",
                    "blank_lines",
                    "newlines",
                ],
            ),
        }
    }
    Some(layout)
}

fn read_rules<'s>(t: Table<'s>, span: Span, report: &mut Report) -> Vec<RuleSpec<'s>> {
    let declared = t.entries.len();
    let mut rules = Vec::with_capacity(declared);
    for entry in t.entries {
        let name_span = entry.key_span;
        let value_span = entry.value.span;
        let name = entry.key;
        let (body, options) = match entry.value.kind {
            ValueKind::Str(text) => (Body::Grammar(text, value_span), RuleOptions::default()),
            ValueKind::Table(table) => {
                if table.entries.iter().any(|e| e.key == "rule") {
                    match read_rule_options(table, value_span, report) {
                        Some(pair) => pair,
                        None => continue,
                    }
                } else {
                    match read_pratt(table, value_span, report) {
                        Some(pratt) => (Body::Pratt(pratt), RuleOptions::default()),
                        None => continue,
                    }
                }
            }
            other => {
                report.error(
                    codes::WRONG_TYPE,
                    value_span,
                    format!(
                        "rule `{name}` must be a string or a table, found {}",
                        other.type_name()
                    ),
                );
                continue;
            }
        };
        rules.push(RuleSpec {
            name,
            name_span,
            body,
            options,
        });
    }
    if rules.is_empty() && declared == 0 {
        report.error(codes::MISSING, span, "[rules] declares no rules");
    }
    rules
}

fn read_rule_options<'s>(
    t: Table<'s>,
    span: Span,
    report: &mut Report,
) -> Option<(Body<'s>, RuleOptions<'s>)> {
    let mut rule = None;
    let mut options = RuleOptions::default();
    for entry in t.entries {
        match &*entry.key {
            "rule" => rule = schematic::text(entry, report),
            "newlines" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    match &*text {
                        "inherit" => {}
                        "trivia" | "significant" => report.error_help(
                            codes::NOT_SUPPORTED,
                            s,
                            "rule-scoped `newlines` is not supported by lang-forge 2.0.0-alpha.1",
                            "per-rule newline policies are ROADMAP: lang-forge alpha.2",
                        ),
                        other => report.error(codes::OUT_OF_RANGE, s, format!("`newlines` must be \"inherit\", \"trivia\", or \"significant\", found `{other}`")),
                    }
                }
            }
            "sync" => options.sync = schematic::strings(entry, report),
            "doc" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    check_free_text("doc", &text, 64 << 10, s, report);
                }
            }
            "allow" => {
                for (item, s) in schematic::strings(entry, report) {
                    if item == "overlap" {
                        options.allow_overlap = true;
                    } else {
                        report.error(
                            codes::OUT_OF_RANGE,
                            s,
                            format!("`allow` accepts \"overlap\", found `{item}`"),
                        );
                    }
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "a rule table",
                &["rule", "newlines", "sync", "doc", "allow"],
            ),
        }
    }
    let Some((text, text_span)) = rule else {
        report.error(codes::MISSING, span, "a rule table needs `rule`");
        return None;
    };
    Some((Body::Grammar(text, text_span), options))
}

fn read_pratt<'s>(t: Table<'s>, span: Span, report: &mut Report) -> Option<PrattSpec<'s>> {
    let mut operand: Option<(Text<'s>, Span)> = None;
    let mut levels = None;
    for entry in t.entries {
        match &*entry.key {
            "operand" => operand = schematic::text(entry, report),
            "levels" => {
                let Some(items) = schematic::array(entry, report) else {
                    continue;
                };
                levels = Some(
                    items
                        .into_iter()
                        .filter_map(|item| read_level(item, report))
                        .collect::<Vec<_>>(),
                );
            }
            "newlines" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    if text != "inherit" {
                        report.error(codes::NOT_SUPPORTED, s, "rule-scoped `newlines` is not supported by lang-forge 2.0.0-alpha.1 (ROADMAP: alpha.2)");
                    }
                }
            }
            "dynamic" => {
                let s = entry.value.span;
                report.error_help(
                    codes::NOT_SUPPORTED,
                    s,
                    "`dynamic` operators are not supported by lang-forge 2.0.0-alpha.1",
                    "user-defined operators with a fixity pre-scan are ROADMAP: lang-forge alpha.2",
                );
            }
            "doc" => {
                if let Some((text, s)) = schematic::string(entry, report) {
                    check_free_text("doc", &text, 64 << 10, s, report);
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "an expression rule",
                &["operand", "levels", "newlines", "dynamic", "doc"],
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
    let levels = levels.unwrap_or_default();
    // `prec` must be given on every level or none, strictly increasing.
    let given = levels.iter().filter(|l| l.prec.is_some()).count();
    if given != 0 && given != levels.len() {
        report.error(
            codes::PREC,
            span,
            "`prec` must be given on every level or on none",
        );
    } else if given != 0 {
        for pair in levels.windows(2) {
            if let (Some((a, _)), Some((b, s))) = (pair[0].prec, pair[1].prec) {
                if b <= a {
                    report.error(
                        codes::PREC,
                        s,
                        "`prec` must increase strictly from level to level",
                    );
                }
            }
        }
    }
    Some(PrattSpec { operand, levels })
}

fn read_level<'s>(item: Value<'s>, report: &mut Report) -> Option<LevelSpec<'s>> {
    let span = item.span;
    let ValueKind::Table(t) = item.kind else {
        report.error_help(
            codes::WRONG_TYPE,
            span,
            format!(
                "an operator level must be a table, found {}",
                item.kind.type_name()
            ),
            "write it as { left = [\"+\", \"-\"] }",
        );
        return None;
    };
    let mut fixity = None;
    let mut operators = Vec::new();
    let mut then = None;
    let mut node = None;
    let mut prec = None;
    for entry in t.entries {
        if let Some(&(_, f)) = Fixity::KEYS.iter().find(|(k, _)| *k == entry.key) {
            if fixity.is_some() {
                report.error(codes::WRONG_TYPE, entry.key_span, "an operator level has exactly one of `left`, `right`, `none`, `prefix`, or `postfix`");
                continue;
            }
            fixity = Some(f);
            operators = if let ValueKind::Str(t) = entry.value.kind {
                Vec::from([(t.text, entry.value.span)])
            } else {
                schematic::strings(entry, report)
            };
            continue;
        }
        match &*entry.key {
            "then" => then = schematic::text(entry, report),
            "node" => node = schematic::string(entry, report),
            "prec" => {
                let s = entry.value.span;
                if let Some(n) = integer(entry, 0, 255, report) {
                    prec = Some((n, s));
                }
            }
            other => unknown_key(
                report,
                entry.key_span,
                other,
                "an operator level",
                &[
                    "left", "right", "none", "prefix", "postfix", "then", "node", "prec",
                ],
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
        prec,
        operators,
        then,
        node,
        span,
    })
}

fn read_capabilities<'s>(t: Table<'s>, report: &mut Report) -> Vec<(Cow<'s, str>, Span)> {
    let mut include = Vec::new();
    let mut seen: BTreeSet<Cow<'s, str>> = BTreeSet::new();
    for entry in t.entries {
        match &*entry.key {
            "include" => {
                for (name, span) in schematic::strings(entry, report) {
                    if !is_capability_name(&name) {
                        report.error(
                            codes::CAPABILITY_NAME,
                            span,
                            format!("capability name `{name}` must match `([a-z][a-z0-9-]*:)?[a-z][a-z0-9-]*(.[a-z][a-z0-9-]*)*`"),
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
            _ => {
                // `[capabilities."<name>"]`: a capability's declaration.
                let name = entry.key.clone();
                let key_span = entry.key_span;
                if !is_capability_name(&name) {
                    report.error(
                        codes::CAPABILITY_NAME,
                        key_span,
                        format!("capability name `{name}` is malformed"),
                    );
                }
                let Some(t) = schematic::table(entry, report) else {
                    continue;
                };
                for e in t.entries {
                    match &*e.key {
                        "kind" => {
                            if let Some((kind, s)) = schematic::string(e, report) {
                                const KINDS: [&str; 14] = [
                                    "analysis",
                                    "lint",
                                    "lower",
                                    "scanner",
                                    "reclassify",
                                    "predicate",
                                    "layout",
                                    "fixity",
                                    "type",
                                    "runtime",
                                    "host",
                                    "backend",
                                    "emit",
                                    "tool",
                                ];
                                if !KINDS.contains(&&*kind) {
                                    report.error(
                                        codes::OUT_OF_RANGE,
                                        s,
                                        format!("unknown capability kind `{kind}`"),
                                    );
                                }
                            }
                        }
                        "source" | "version" | "stage" => {
                            let _ = schematic::string(e, report);
                        }
                        "options" => {
                            let _ = schematic::table(e, report);
                        }
                        other => unknown_key(
                            report,
                            e.key_span,
                            other,
                            "a capability",
                            &["kind", "source", "version", "options", "stage"],
                        ),
                    }
                }
            }
        }
    }
    include
}

fn read_ast<'s>(t: Table<'s>, report: &mut Report) -> Vec<Supertype<'s>> {
    let mut out = Vec::new();
    for entry in t.entries {
        let name = entry.key.clone();
        let span = entry.key_span;
        let ok = check_name(&name, NameKind::Supertype, span, report);
        let members = schematic::strings(entry, report);
        if ok {
            out.push(Supertype {
                name,
                span,
                members,
            });
        }
    }
    out
}

fn read_injections<'s>(t: Table<'s>, report: &mut Report) -> Vec<Injection<'s>> {
    let mut out = Vec::new();
    for entry in t.entries {
        let id = entry.key.clone();
        let span = entry.key_span;
        let ok = check_name(&id, NameKind::Id, span, report);
        let Some(t) = schematic::table(entry, report) else {
            continue;
        };
        let mut target = None;
        let mut language = None;
        let mut injection = Injection {
            id,
            span,
            target: (Cow::Borrowed(""), span),
            language: (Cow::Borrowed(""), span),
            editor: false,
            start: None,
            inner: None,
            combined: false,
            when: None,
            scope: None,
        };
        for e in t.entries {
            match &*e.key {
                "target" => target = schematic::string(e, report),
                "language" => language = schematic::string(e, report),
                "resolve" => {
                    if let Some((text, s)) = schematic::string(e, report) {
                        match &*text {
                            "sketch" => injection.editor = false,
                            "editor" => injection.editor = true,
                            other => report.error(
                                codes::OUT_OF_RANGE,
                                s,
                                format!(
                                    "`resolve` must be \"sketch\" or \"editor\", found `{other}`"
                                ),
                            ),
                        }
                    }
                }
                "start" => injection.start = schematic::string(e, report),
                "content" => {
                    if let Some((text, s)) = schematic::string(e, report) {
                        match &*text {
                            "whole" => injection.inner = Some(false),
                            "inner" => injection.inner = Some(true),
                            other => report.error(
                                codes::OUT_OF_RANGE,
                                s,
                                format!(
                                    "`content` must be \"whole\" or \"inner\", found `{other}`"
                                ),
                            ),
                        }
                    }
                }
                "combined" => injection.combined = schematic::boolean(e, report).unwrap_or(false),
                "when" => injection.when = schematic::string(e, report),
                "scope" => {
                    if let Some((text, s)) = schematic::string(e, report) {
                        let ok = text.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                            && text.bytes().all(|b| {
                                b.is_ascii_lowercase()
                                    || b.is_ascii_digit()
                                    || matches!(b, b'.' | b'-')
                            });
                        if ok {
                            injection.scope = Some(text);
                        } else {
                            report.error(
                                codes::OUT_OF_RANGE,
                                s,
                                format!("scope `{text}` must match `[a-z][a-z0-9.-]*`"),
                            );
                        }
                    }
                }
                other => unknown_key(
                    report,
                    e.key_span,
                    other,
                    "an injection",
                    &[
                        "target", "language", "resolve", "start", "content", "combined", "when",
                        "scope",
                    ],
                ),
            }
        }
        let (Some(target), Some(language)) = (target, language) else {
            report.error(
                codes::MISSING,
                span,
                "an injection needs `target` and `language`",
            );
            continue;
        };
        injection.target = target;
        injection.language = language;
        if ok {
            out.push(injection);
        }
    }
    out
}

/// Checks `[hooks]`. Hooks act through capabilities: a `lower` hook in
/// lowering, a `fixity` hook for `dynamic` operators; the hooks the syntax
/// forge would call (`scanner`, `reclassify`, `predicate`, `layout`) are used
/// through keys this release refuses (`hook = ...` tokens, `@hook`, `style =
/// "hook"`), so a declaration alone is only checked.
fn read_hooks(t: Table<'_>, report: &mut Report) {
    for entry in t.entries {
        let name = entry.key.clone();
        let span = entry.key_span;
        let _ = check_name(&name, NameKind::Id, span, report);
        let Some(t) = schematic::table(entry, report) else {
            continue;
        };
        let mut kind = None;
        let mut capability = false;
        for e in t.entries {
            match &*e.key {
                "kind" => kind = schematic::string(e, report),
                "capability" => {
                    if let Some((c, s)) = schematic::string(e, report) {
                        capability = true;
                        if !is_capability_name(&c) {
                            report.error(
                                codes::CAPABILITY_NAME,
                                s,
                                format!("capability name `{c}` is malformed"),
                            );
                        }
                    }
                }
                "tokens" | "into" => {
                    let _ = schematic::strings(e, report);
                }
                "token" => {
                    let _ = schematic::string(e, report);
                }
                "options" => {
                    let _ = schematic::table(e, report);
                }
                other => unknown_key(
                    report,
                    e.key_span,
                    other,
                    "a hook",
                    &["kind", "capability", "tokens", "token", "into", "options"],
                ),
            }
        }
        let Some(kind) = kind else {
            report.error(
                codes::HOOK_REQUIRED,
                span,
                format!("hook `{name}` needs a `kind`"),
            );
            continue;
        };
        if !capability {
            report.error(
                codes::HOOK_REQUIRED,
                span,
                format!("hook `{name}` needs a `capability`"),
            );
        }
        if !matches!(
            &*kind.0,
            "lower" | "fixity" | "scanner" | "reclassify" | "predicate" | "layout"
        ) {
            report.error(
                codes::HOOK_KIND,
                kind.1,
                format!("unknown hook kind `{}`; use scanner, reclassify, predicate, layout, fixity, or lower", kind.0),
            );
        }
    }
}

/// A pair list, `[[open, close], ...]`.
fn pairs<'s>(entry: Entry<'s>, report: &mut Report) -> Option<Vec<Pair<'s>>> {
    let key = entry.key.clone();
    let items = schematic::array(entry, report)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let span = item.span;
        let pair = match item.kind {
            ValueKind::Array(pair) if pair.len() == 2 => {
                let mut texts = pair.into_iter().filter_map(|v| match v.kind {
                    ValueKind::Str(t) if !t.text.is_empty() => Some(t.text),
                    _ => None,
                });
                match (texts.next(), texts.next()) {
                    (Some(a), Some(b)) => Some((a, b, span)),
                    _ => None,
                }
            }
            _ => None,
        };
        match pair {
            Some(pair) => out.push(pair),
            None => report.error(
                codes::WRONG_TYPE,
                span,
                format!("`{key}` holds pairs of non-empty strings, such as [\"(\", \")\"]"),
            ),
        }
    }
    Some(out)
}

/// An integer in `lo..=hi`.
fn integer(entry: Entry<'_>, lo: i64, hi: i64, report: &mut Report) -> Option<i64> {
    match entry.value.kind {
        ValueKind::Number(Some(n)) if (lo..=hi).contains(&n) => Some(n),
        ValueKind::Number(Some(n)) => {
            report.error(
                codes::OUT_OF_RANGE,
                entry.value.span,
                format!("`{}` must be between {lo} and {hi}, found {n}", entry.key),
            );
            None
        }
        _ => {
            report.error(
                codes::WRONG_TYPE,
                entry.value.span,
                format!(
                    "`{}` must be an integer, found {}",
                    entry.key,
                    entry.value.type_name()
                ),
            );
            None
        }
    }
}

/// The name grammars of LSF2 §6.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameKind {
    Language,
    Rule,
    Class,
    Label,
    Mode,
    Supertype,
    Id,
}

/// Whether `name` matches its §6.1 grammar; reports it otherwise.
pub(crate) fn check_name(name: &str, kind: NameKind, span: Span, report: &mut Report) -> bool {
    if is_name(name, kind) {
        return true;
    }
    let (grammar, code, what) = match kind {
        NameKind::Language => ("[a-z][a-z0-9_]*", codes::LANGUAGE_NAME, "language name"),
        NameKind::Rule => ("_?[a-z][a-z0-9_]*", codes::RULE_NAME, "rule name"),
        NameKind::Class => ("[A-Z][A-Z0-9_]*", codes::CLASS_NAME, "token class"),
        NameKind::Label => ("[a-z][a-z0-9_]*", codes::LABEL_NAME, "label"),
        NameKind::Mode => ("[a-z][a-z0-9_]*", codes::MODE_NAME, "mode name"),
        NameKind::Supertype => ("[A-Z][A-Za-z0-9]*", codes::SUPERTYPE_NAME, "supertype name"),
        NameKind::Id => ("[a-z][a-z0-9_]*", codes::INJECTION, "id"),
    };
    report.error(
        code,
        span,
        format!("{what} `{name}` must match `{grammar}` (at most 64 bytes)"),
    );
    false
}

/// Whether `name` matches its §6.1 grammar.
pub(crate) fn is_name(name: &str, kind: NameKind) -> bool {
    let b = name.as_bytes();
    if b.is_empty() || b.len() > 64 {
        return false;
    }
    let lower_rest = |rest: &[u8]| {
        rest.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
    };
    match kind {
        NameKind::Language | NameKind::Label | NameKind::Mode | NameKind::Id => {
            b[0].is_ascii_lowercase() && lower_rest(&b[1..])
        }
        NameKind::Rule => {
            let body = b.strip_prefix(b"_").unwrap_or(b);
            !body.is_empty() && body[0].is_ascii_lowercase() && lower_rest(&body[1..])
        }
        NameKind::Class => {
            b[0].is_ascii_uppercase()
                && b[1..]
                    .iter()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
        }
        NameKind::Supertype => {
            b[0].is_ascii_uppercase() && b[1..].iter().all(u8::is_ascii_alphanumeric)
        }
    }
}

fn is_capability_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    let word = |w: &str| {
        let b = w.as_bytes();
        !b.is_empty()
            && b[0].is_ascii_lowercase()
            && b.iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
    };
    let rest = match name.split_once(':') {
        Some((ns, rest)) => {
            if !word(ns) {
                return false;
            }
            rest
        }
        None => name,
    };
    rest.split('.').all(word)
}

/// Checks free text (LSF2 §6.2).
fn check_free_text(key: &str, text: &str, limit: usize, span: Span, report: &mut Report) {
    if text.len() > limit {
        report.error(
            codes::FREE_TEXT,
            span,
            format!("`{key}` is longer than {limit} bytes"),
        );
        return;
    }
    if let Some(bad) = text
        .chars()
        .find(|&c| is_forbidden_text_char(c) && c != '\t' && c != '\n')
    {
        report.error(
            codes::FREE_TEXT,
            span,
            format!(
                "`{key}` contains a forbidden character (U+{:04X})",
                u32::from(bad)
            ),
        );
    }
}

/// C0 and C1 controls, bidirectional formatting characters (Trojan Source),
/// and the Unicode line and paragraph separators.
pub(crate) fn is_forbidden_text_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
}

/// Whether `text` is a SemVer 2.0 version.
pub(crate) fn is_semver(text: &str) -> bool {
    let (rest, build) = match text.split_once('+') {
        Some((rest, build)) => (rest, Some(build)),
        None => (text, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (rest, None),
    };
    let numeric = |p: &str| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && (p == "0" || !p.starts_with('0'))
    };
    let ident =
        |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| numeric(p))
        && pre.is_none_or(|pre| {
            pre.split('.')
                .all(|p| ident(p) && (!p.bytes().all(|b| b.is_ascii_digit()) || numeric(p)))
        })
        && build.is_none_or(|b| b.split('.').all(ident))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semver() {
        for ok in [
            "0.1.0",
            "1.2.3",
            "1.0.0-alpha.1",
            "1.0.0-0.3.7",
            "1.0.0+build.5",
            "10.20.30-rc.1+x",
        ] {
            assert!(is_semver(ok), "{ok}");
        }
        for bad in [
            "1.2", "01.2.3", "1.2.3-", "1.2.3-01", "1.2.3+", "a.b.c", "1.2.3-α", "",
        ] {
            assert!(!is_semver(bad), "{bad}");
        }
    }

    #[test]
    fn test_names() {
        assert!(is_name("mox", NameKind::Language));
        assert!(!is_name("Mox!", NameKind::Language));
        assert!(is_name("_stmt", NameKind::Rule));
        assert!(!is_name("_", NameKind::Rule));
        assert!(!is_name("STMT", NameKind::Rule));
        assert!(is_name("DQ_STRING", NameKind::Class));
        assert!(!is_name("Dq", NameKind::Class));
        assert!(is_name("Expression", NameKind::Supertype));
        assert!(!is_name(&"a".repeat(65), NameKind::Label));
        assert!(is_capability_name("lint:core"));
        assert!(is_capability_name("mox.lower"));
        assert!(is_capability_name("borrow-cc"));
        assert!(!is_capability_name("Mox"));
        assert!(!is_capability_name("a:"));
        assert!(!is_capability_name("a..b"));
    }

    #[test]
    fn test_forbidden_text() {
        assert!(is_forbidden_text_char('\u{202E}'));
        assert!(is_forbidden_text_char('\u{85}'));
        assert!(!is_forbidden_text_char('é'));
    }
}
