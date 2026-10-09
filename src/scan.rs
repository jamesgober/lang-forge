//! The format-2 lexer: a mode-stack machine built from an LSF2 `[lexer]`.
//!
//! The lexer state is a stack of frames (LSF2 §9.8): a *mode* frame lexes
//! with the tokens active in that mode; a *string* frame scans the content of
//! a string class that builds a node (text runs, escapes, embedded tokens,
//! interpolation openers, the close delimiter); a *hole* frame lexes the code
//! inside an interpolation in its mode, counting brackets so that the hole's
//! close text closes it only at depth zero. Tokens and string openers with
//! actions push, pop, and switch mode frames.
//!
//! At each position in a mode, every active candidate whose conditions hold is
//! collected — literals, custom classes (a regex or a literal), string
//! openers, comment openers, `NUMBER`, `IDENT`, whitespace — and the longest
//! wins; ties go to the higher priority, then the category in that order, then
//! declaration order (LSF2 §9.13). An identifier-shaped literal is a candidate
//! only when the whole identifier lexeme equals it, so `letter` is never `let`
//! `ter`. Characters at which nothing starts become the mode's `text` class,
//! or an `UNKNOWN` run reported once.
//!
//! Every byte lands in exactly one token, so trees stay lossless; tokens the
//! grammar needs but the input does not have (the close of an unterminated
//! string, `INDENT`/`DEDENT`) are zero-width. Work is linear in the input:
//! each candidate scans forward from the current position only, a text run
//! checks candidates only at bytes that can begin one, and the frame stack is
//! bounded by `max_mode_depth`.

use alloc::{boxed::Box, collections::BTreeMap, format, string::String, vec, vec::Vec};

use diag_lang::{Code, Diagnostic, Label, Severity};
use syntax_lang::{Span, Token};

use crate::{
    codes,
    error::Report,
    kind::Kind,
    lexer::{BOM, Keywords, char_at, is_unicode_space, line_end, utf8_len},
    regex::{CharSet, Props, Regex},
    schematic::IdentMode,
    spec2::{ActionSpec, CloseAt, Layout, LeadingZeros, Lexer2, NumbersSpec, Part},
};

/// The most DFA states one token regex may compile to (LSF2 §1.8, `LSF3201`).
const REGEX_STATES: usize = 4096;

/// The most DFA states all token regexes of a lexer may use together
/// (`LSF3202`; LSF2 states the limit per mode, this lexer applies it to all
/// modes together, which is stricter).
const ALL_REGEX_STATES: usize = 65_536;

/// The most lexer modes (LSF2 §1.8, `LSF9007`).
const MAX_MODES: usize = 256;

/// The id of the predefined `main` mode.
pub(crate) const MAIN: u16 = 0;

/// Tie-break categories, in priority order (LSF2 §9.13).
const CAT_LITERAL: u8 = 0;
const CAT_CLASS: u8 = 1;
const CAT_STRING: u8 = 2;
const CAT_COMMENT: u8 = 3;
const CAT_NUMBER: u8 = 4;
const CAT_IDENT: u8 = 5;
const CAT_SPACE: u8 = 6;

/// The built-in kinds a scanner produces.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Builtins {
    pub(crate) whitespace: Kind,
    pub(crate) comment: Kind,
    pub(crate) doc_comment: Kind,
    pub(crate) unknown: Kind,
    pub(crate) ident: Kind,
    pub(crate) number: Kind,
    pub(crate) newline: Kind,
    pub(crate) indent: Kind,
    pub(crate) dedent: Kind,
    pub(crate) shebang: Kind,
}

/// The kinds a string class produces.
#[derive(Clone, Debug)]
pub(crate) enum StringKinds {
    /// A class with no structure: one token.
    Token(Kind),
    /// A class that builds a node (LSF2 §9.5.5).
    Node {
        open: Kind,
        text: Kind,
        escape: Option<Kind>,
        embedded: Vec<Kind>,
        interp_open: Option<Kind>,
        interp_close: Option<Kind>,
        close: Kind,
    },
}

/// A lexer action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    Push(u16),
    Pop,
    Switch(u16),
}

/// How a custom class matches.
#[derive(Clone, Debug)]
pub(crate) enum Matcher {
    Regex(Regex),
    Literal(Box<[u8]>),
}

/// A custom token class (`[lexer.tokens]`) at run time.
#[derive(Clone, Debug)]
pub(crate) struct Class {
    pub(crate) kind: Kind,
    pub(crate) matcher: Matcher,
    pub(crate) priority: i8,
    pub(crate) action: Option<Act>,
    pub(crate) followed_by: Option<CharSet>,
    pub(crate) not_followed_by: Option<CharSet>,
    /// Sorted kind indexes the previous significant token must (not) be.
    pub(crate) when_prev: Option<Box<[u16]>>,
    pub(crate) unless_prev: Option<Box<[u16]>>,
    pub(crate) line_start: bool,
    pub(crate) indented: bool,
    pub(crate) column: Option<(u32, u32)>,
}

/// A comment (line or block) at run time.
#[derive(Clone, Debug)]
pub(crate) struct Comment {
    pub(crate) open: Box<[u8]>,
    /// `None` for a line comment.
    pub(crate) close: Option<Box<[u8]>>,
    pub(crate) nested: bool,
    pub(crate) doc: bool,
    pub(crate) not_followed_by: Option<CharSet>,
    pub(crate) stop_before: Box<[Box<[u8]>]>,
}

/// One delimiter part at run time.
#[derive(Clone, Debug)]
pub(crate) enum PartRt {
    Text(Box<[u8]>),
    Regex(Regex),
    Capture(Regex),
    Backref,
    Newline,
}

/// An interpolation hole at run time.
#[derive(Clone, Debug)]
pub(crate) struct Hole {
    pub(crate) open: Box<[u8]>,
    pub(crate) close: Box<[u8]>,
    pub(crate) mode: u16,
    pub(crate) when_next: Option<CharSet>,
}

/// A string class at run time.
#[derive(Clone, Debug)]
pub(crate) struct StrClass {
    pub(crate) open: Box<[PartRt]>,
    pub(crate) close: Box<[PartRt]>,
    pub(crate) escape: Option<char>,
    pub(crate) multiline: bool,
    pub(crate) next_line: bool,
    pub(crate) close_at: CloseAt,
    pub(crate) close_not_followed_by: Option<CharSet>,
    pub(crate) kinds: StringKinds,
    pub(crate) holes: Box<[Hole]>,
    pub(crate) embedded: Box<[Regex]>,
}

/// What a fixed text produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fixed {
    Literal(Kind),
    Class(u16),
    Comment(u16),
    String(u16),
}

/// One lexer mode at run time.
#[derive(Clone, Debug)]
pub(crate) struct Mode {
    /// Fixed texts grouped by first byte, longest first within a group.
    pub(crate) fixed: Box<[(Box<[u8]>, Fixed)]>,
    pub(crate) heads: Box<[u32]>,
    /// Active regex classes, in declaration order.
    pub(crate) regexes: Box<[u16]>,
    /// Active string classes whose openers are not a plain text.
    pub(crate) part_strings: Box<[u16]>,
    pub(crate) literals: bool,
    pub(crate) builtins: bool,
    pub(crate) trivia: bool,
    pub(crate) text: Option<Kind>,
    /// `eof`: `Some(true)` for `"error"`, `Some(false)` for `"ok"`; unset,
    /// the end of input is fine in the initial frame and an error in a pushed
    /// one (LSF2 §9.8.2).
    pub(crate) eof_error: Option<bool>,
    /// Actions by kind index, sorted.
    pub(crate) actions: Box<[(u16, Act)]>,
    /// The bytes some candidate of this mode can begin with.
    pub(crate) may_start: [u64; 4],
    /// The mode's name, for messages.
    pub(crate) name: Box<str>,
}

/// `[lexer.numbers]` at run time.
#[derive(Clone, Debug)]
pub(crate) struct Numbers {
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
    pub(crate) suffixes: Box<[Box<str>]>,
}

impl Numbers {
    fn new(spec: &NumbersSpec) -> Self {
        Self {
            radix: spec.radix,
            radix_any_case: spec.radix_any_case,
            separator: spec.separator,
            separator_anywhere: spec.separator_anywhere,
            floats: spec.floats,
            exponent: spec.exponent,
            leading_dot: spec.leading_dot,
            trailing_dot: spec.trailing_dot,
            hex_floats: spec.hex_floats,
            leading_zeros: spec.leading_zeros,
            suffixes: spec
                .suffixes
                .iter()
                .map(|s| Box::from(s.as_str()))
                .collect(),
        }
    }
}

/// `[layout]` at run time (see `layout.rs`).
#[derive(Clone, Debug)]
pub(crate) struct LayoutRt {
    pub(crate) indent: bool,
    pub(crate) open_after: Box<[u16]>,
    pub(crate) joins: Box<[(u16, u16)]>,
    pub(crate) newline_joins: Box<[(u16, u16)]>,
    pub(crate) explicit_join: Option<Box<[u8]>>,
    pub(crate) tab_width: u32,
    pub(crate) mixed_error: bool,
    /// 0 trivia, 1 significant, 2 terminators.
    pub(crate) newlines: u8,
    pub(crate) terminate_after: Box<[u16]>,
    pub(crate) continue_before: Box<[u16]>,
}

/// The format-2 lexer.
#[derive(Clone, Debug)]
pub(crate) struct Scanner {
    pub(crate) ident_mode: IdentMode,
    pub(crate) extra_start: Box<[char]>,
    pub(crate) extra_continue: Box<[char]>,
    pub(crate) require_nfc: bool,
    pub(crate) numbers: Numbers,
    pub(crate) keywords: Keywords,
    pub(crate) case_insensitive: bool,
    pub(crate) comments: Box<[Comment]>,
    pub(crate) classes: Box<[Class]>,
    pub(crate) strings: Box<[StrClass]>,
    pub(crate) modes: Box<[Mode]>,
    pub(crate) initial: u16,
    pub(crate) max_depth: u32,
    pub(crate) shebang: bool,
    pub(crate) brackets: Box<[(u16, u16)]>,
    pub(crate) k: Builtins,
    /// Line breaks are `NEWLINE` tokens.
    pub(crate) newlines: bool,
    pub(crate) tab_width: u32,
    pub(crate) layout: Option<LayoutRt>,
}

// ----------------------------------------------------------------------------
// Building
// ----------------------------------------------------------------------------

/// What the grammar compiler hands the scanner builder.
pub(crate) struct Build<'a, 's> {
    pub(crate) spec: &'a Lexer2<'s>,
    pub(crate) layout: Option<&'a Layout<'s>>,
    /// Grammar literals (and reserved-only keywords): text, kind, span.
    pub(crate) literals: &'a [(&'a str, Kind, Span)],
    /// Texts of the contextual keywords (lexed as `IDENT`).
    pub(crate) contextual: &'a dyn Fn(&str) -> bool,
    pub(crate) builtins: Builtins,
    /// Kinds of `spec.tokens`, in order.
    pub(crate) class_kinds: &'a [Kind],
    /// Kinds of `spec.classes`, in order.
    pub(crate) string_kinds: &'a [StringKinds],
    /// The kind of the format-1 `strings = [...]` form (`STRING`).
    pub(crate) array_string: Kind,
    /// The kind of each mode's `text` class, by mode name.
    pub(crate) text_kinds: &'a BTreeMap<&'a str, Kind>,
    /// Resolves a token reference (LSF2 §6.3).
    pub(crate) resolve: &'a dyn Fn(&str, Span, &mut Report) -> Option<Kind>,
}

/// A fixed text with what it produces and where it was declared.
struct FixedSpec {
    text: Box<[u8]>,
    what: Fixed,
    span: Span,
}

/// Per-mode activity while building.
#[derive(Clone, Default)]
struct Active {
    literals: Option<bool>,
    builtins: Option<bool>,
    trivia: Option<bool>,
    classes: Vec<u16>,
    comments: Vec<u16>,
    strings: Vec<u16>,
    array_strings: bool,
    literal_kinds: Vec<Kind>,
}

impl Scanner {
    /// Builds the scanner, reporting every problem with the `[lexer]` table.
    pub(crate) fn build(b: &Build<'_, '_>, props: &mut Props, report: &mut Report) -> Option<Self> {
        let spec = b.spec;
        let errors = report.errors();

        // ----- modes -----
        let mut mode_names: Vec<&str> = Vec::from(["main"]);
        for m in &spec.modes {
            if m.name == "main" {
                continue;
            }
            if mode_names.contains(&&*m.name) {
                report.error(
                    codes::MODE_NAME,
                    m.name_span,
                    format!("mode `{}` is declared twice", m.name),
                );
                continue;
            }
            mode_names.push(&m.name);
        }
        if mode_names.len() > MAX_MODES {
            report.error(
                codes::TOO_MANY_MODES,
                Span::empty(0),
                format!("a lexer has at most {MAX_MODES} modes"),
            );
            return None;
        }
        let mode_id = |name: &str, span: Span, report: &mut Report| -> Option<u16> {
            match mode_names.iter().position(|m| *m == name) {
                Some(i) => Some(i as u16),
                None => {
                    let help = crate::suggest::suggest(name, mode_names.iter().copied(), 2);
                    let message = format!("mode `{name}` is not declared");
                    match help {
                        Some(h) => report.error_help(codes::MODE_UNDECLARED, span, message, h),
                        None => report.error(codes::MODE_UNDECLARED, span, message),
                    }
                    None
                }
            }
        };
        let modes_of = |list: &Option<Vec<(alloc::borrow::Cow<'_, str>, Span)>>,
                        report: &mut Report|
         -> Vec<u16> {
            match list {
                None => Vec::from([MAIN]),
                Some(list) => list
                    .iter()
                    .filter_map(|(m, s)| mode_id(m, *s, report))
                    .collect(),
            }
        };

        let mut active: Vec<Active> = vec![Active::default(); mode_names.len()];
        // Everything without `modes` is in `main`.
        active[MAIN as usize].literals = Some(true);
        active[MAIN as usize].builtins = Some(true);
        active[MAIN as usize].trivia = Some(true);
        active[MAIN as usize].array_strings = true;

        // ----- custom classes -----
        let mut classes = Vec::with_capacity(spec.tokens.len());
        let mut regex_states = 0usize;
        for (i, (t, &kind)) in spec.tokens.iter().zip(b.class_kinds).enumerate() {
            let matcher = match &t.source.0 {
                crate::spec2::TokenSource::Regex(r) => {
                    match compile_regex(r, t.source.1, props, report, false) {
                        Some(regex) => {
                            regex_states += regex.states();
                            Matcher::Regex(regex)
                        }
                        None => continue,
                    }
                }
                crate::spec2::TokenSource::Literal(l) => {
                    if l.is_empty() {
                        continue;
                    }
                    Matcher::Literal(l.as_bytes().into())
                }
            };
            let action = t
                .action
                .as_ref()
                .and_then(|(a, s)| resolve_action(a, *s, &mode_id, report));
            let class = Class {
                kind,
                matcher,
                priority: t.priority,
                action,
                followed_by: t
                    .followed_by
                    .as_ref()
                    .and_then(|(r, s)| one_class(r, *s, props, report)),
                not_followed_by: t
                    .not_followed_by
                    .as_ref()
                    .and_then(|(r, s)| one_class(r, *s, props, report)),
                when_prev: t
                    .when_prev
                    .as_ref()
                    .map(|refs| kind_set(refs, b.resolve, report)),
                unless_prev: t
                    .unless_prev
                    .as_ref()
                    .map(|refs| kind_set(refs, b.resolve, report)),
                line_start: t.line_start || t.indented,
                indented: t.indented,
                column: t.column,
            };
            for m in modes_of(&t.modes, report) {
                active[m as usize].classes.push(i as u16);
            }
            classes.push(class);
        }
        if classes.len() != spec.tokens.len() {
            // Some class failed and was reported; indexes no longer line up.
            return None;
        }

        // ----- comments -----
        let mut comments = Vec::with_capacity(spec.comments.len());
        for (i, c) in spec.comments.iter().enumerate() {
            check_comment_delimiter(&c.open, c.span, report);
            if let Some(close) = &c.close {
                if close.is_empty() {
                    report.error(codes::DELIMITER, c.span, "a comment delimiter is empty");
                }
            }
            comments.push(Comment {
                open: c.open.as_bytes().into(),
                close: c.close.as_ref().map(|t| t.as_bytes().into()),
                nested: c.nested.unwrap_or(spec.nested_default),
                doc: c.doc,
                not_followed_by: c
                    .not_followed_by
                    .as_ref()
                    .and_then(|(r, s)| one_class(r, *s, props, report)),
                stop_before: c
                    .stop_before
                    .iter()
                    .map(|(t, _)| Box::<[u8]>::from(t.as_bytes()))
                    .collect(),
            });
            for m in modes_of(&c.modes, report) {
                active[m as usize].comments.push(i as u16);
            }
        }

        // ----- string classes -----
        let mut strings = Vec::with_capacity(spec.classes.len());
        for (i, (c, kinds)) in spec.classes.iter().zip(b.string_kinds).enumerate() {
            let open = compile_parts(&c.open, false, props, report);
            let close = compile_parts(&c.close, true, props, report);
            let (Some(open), Some(close)) = (open, close) else {
                return None;
            };
            let holes = c
                .interpolate
                .iter()
                .filter_map(|h| {
                    let mode = match &h.mode {
                        Some((m, s)) => mode_id(m, *s, report)?,
                        None => MAIN,
                    };
                    Some(Hole {
                        open: h.open.as_bytes().into(),
                        close: h.close.as_bytes().into(),
                        mode,
                        when_next: h
                            .when_next
                            .as_ref()
                            .and_then(|(r, s)| one_class(r, *s, props, report)),
                    })
                })
                .collect();
            let embedded = c
                .embedded
                .iter()
                .filter_map(|e| {
                    let r = compile_regex(&e.regex.0, e.regex.1, props, report, false)?;
                    regex_states += r.states();
                    Some(r)
                })
                .collect();
            strings.push(StrClass {
                open,
                close,
                escape: c.escape,
                multiline: c.multiline,
                next_line: c.next_line,
                close_at: c.close_at,
                close_not_followed_by: c
                    .close_not_followed_by
                    .as_ref()
                    .and_then(|(r, s)| one_class(r, *s, props, report)),
                kinds: kinds.clone(),
                holes,
                embedded,
            });
            for m in modes_of(&c.modes, report) {
                active[m as usize].strings.push(i as u16);
            }
        }
        // The format-1 array form: unnamed single-token classes of kind
        // `STRING`, active in `main`, after the named classes.
        let first_array = strings.len() as u16;
        for s in &spec.strings_array {
            strings.push(StrClass {
                open: Box::new([PartRt::Text(s.open.as_bytes().into())]),
                close: Box::new([PartRt::Text(s.close.as_bytes().into())]),
                escape: s.escape,
                multiline: s.multiline,
                next_line: false,
                close_at: CloseAt::Anywhere,
                close_not_followed_by: None,
                kinds: StringKinds::Token(b.array_string),
                holes: Box::new([]),
                embedded: Box::new([]),
            });
        }
        if regex_states > ALL_REGEX_STATES {
            report.error(
                codes::MODE_TOO_LARGE,
                Span::empty(0),
                format!(
                    "the token regexes need more than {ALL_REGEX_STATES} automaton states together"
                ),
            );
            return None;
        }

        // ----- mode tables -----
        for m in &spec.modes {
            let Some(id) = mode_id(&m.name, m.name_span, report) else {
                continue;
            };
            let a = &mut active[id as usize];
            if m.literals.is_some() {
                a.literals = m.literals;
            }
            if m.builtins.is_some() {
                a.builtins = m.builtins;
            }
            if m.trivia.is_some() {
                a.trivia = m.trivia;
            }
            for (name, span) in &m.strings {
                match spec.classes.iter().position(|c| c.name == *name) {
                    Some(i) => active[id as usize].strings.push(i as u16),
                    None => report.error(
                        codes::UNDEFINED,
                        *span,
                        format!("string class `{name}` is not declared"),
                    ),
                }
            }
            for (name, span) in &m.tokens {
                if let Some(kind) = (b.resolve)(name, *span, report) {
                    match b.class_kinds.iter().position(|k| *k == kind) {
                        Some(i) => active[id as usize].classes.push(i as u16),
                        None => active[id as usize].literal_kinds.push(kind),
                    }
                }
            }
        }
        // `inherit`: a mode starts from its parent's set. Resolved in an order
        // where parents come first; a cycle is refused.
        let inherit: Vec<Option<u16>> = (0..mode_names.len())
            .map(|i| {
                spec.modes
                    .iter()
                    .find(|m| m.name == mode_names[i])
                    .and_then(|m| m.inherit.as_ref())
                    .and_then(|(p, s)| mode_id(p, *s, report))
            })
            .collect();
        let mut done = vec![false; mode_names.len()];
        for start in 0..mode_names.len() {
            let mut chain = Vec::new();
            let mut at = Some(start as u16);
            while let Some(m) = at {
                if done[m as usize] {
                    break;
                }
                if chain.contains(&m) {
                    let span = spec
                        .modes
                        .iter()
                        .find(|x| x.name == mode_names[m as usize])
                        .map_or(Span::empty(0), |x| x.name_span);
                    report.error(
                        codes::MODE_CYCLE,
                        span,
                        format!("mode `{}` inherits from itself", mode_names[m as usize]),
                    );
                    return None;
                }
                chain.push(m);
                at = inherit[m as usize];
            }
            for &m in chain.iter().rev() {
                if let Some(parent) = inherit[m as usize] {
                    let parent_set = active[parent as usize].clone();
                    let a = &mut active[m as usize];
                    a.literals = a.literals.or(parent_set.literals);
                    a.builtins = a.builtins.or(parent_set.builtins);
                    a.trivia = a.trivia.or(parent_set.trivia);
                    a.array_strings |= parent_set.array_strings;
                    for c in parent_set.classes {
                        if !a.classes.contains(&c) {
                            a.classes.push(c);
                        }
                    }
                    for c in parent_set.comments {
                        if !a.comments.contains(&c) {
                            a.comments.push(c);
                        }
                    }
                    for c in parent_set.strings {
                        if !a.strings.contains(&c) {
                            a.strings.push(c);
                        }
                    }
                    a.literal_kinds.extend(parent_set.literal_kinds);
                }
                done[m as usize] = true;
            }
        }

        let mut modes = Vec::with_capacity(mode_names.len());
        for (id, name) in mode_names.iter().enumerate() {
            let a = &active[id];
            let mspec = spec.modes.iter().find(|m| m.name == *name);
            let literals = a.literals.unwrap_or(false);
            let builtins = a.builtins.unwrap_or(false);
            let trivia = a.trivia.unwrap_or(false);
            let mut fixed: Vec<FixedSpec> = Vec::new();
            for &(text, kind, span) in b.literals {
                let shaped_like_ident = is_ident_text(text, spec.ident.mode);
                let listed = a.literal_kinds.contains(&kind);
                if shaped_like_ident || !(literals || listed) {
                    continue;
                }
                fixed.push(FixedSpec {
                    text: text.as_bytes().into(),
                    what: Fixed::Literal(kind),
                    span,
                });
            }
            let mut regexes = Vec::new();
            for &ci in &a.classes {
                let tspec = &spec.tokens[ci as usize];
                match &classes[ci as usize].matcher {
                    Matcher::Regex(_) => regexes.push(ci),
                    Matcher::Literal(text) => fixed.push(FixedSpec {
                        text: text.clone(),
                        what: Fixed::Class(ci),
                        span: tspec.source.1,
                    }),
                }
            }
            regexes.sort_unstable();
            regexes.dedup();
            if trivia {
                for &ci in &a.comments {
                    fixed.push(FixedSpec {
                        text: comments[ci as usize].open.clone(),
                        what: Fixed::Comment(ci),
                        span: spec.comments[ci as usize].span,
                    });
                }
            }
            let mut part_strings = Vec::new();
            let mut string_ids: Vec<u16> = a.strings.clone();
            if a.array_strings {
                string_ids.extend(first_array..strings.len() as u16);
            }
            string_ids.sort_unstable();
            string_ids.dedup();
            for si in string_ids {
                match strings[si as usize].open.as_ref() {
                    [PartRt::Text(text)] => {
                        let span = spec.classes.get(si as usize).map_or_else(
                            || {
                                spec.strings_array
                                    .get((si - first_array) as usize)
                                    .map_or(Span::empty(0), |s| s.span)
                            },
                            |c| c.span,
                        );
                        fixed.push(FixedSpec {
                            text: text.clone(),
                            what: Fixed::String(si),
                            span,
                        });
                    }
                    _ => part_strings.push(si),
                }
            }
            // One text, one purpose: two fixed texts with equal text in one
            // mode could never both be lexed.
            fixed.sort_by(|x, y| {
                x.text[0]
                    .cmp(&y.text[0])
                    .then(y.text.len().cmp(&x.text.len()))
                    .then(x.text.cmp(&y.text))
            });
            for pair in fixed.windows(2) {
                if pair[0].text == pair[1].text && pair[0].what != pair[1].what {
                    let text = String::from_utf8_lossy(&pair[0].text);
                    let (first, second) = if pair[0].span.start() <= pair[1].span.start() {
                        (pair[0].span, pair[1].span)
                    } else {
                        (pair[1].span, pair[0].span)
                    };
                    let code = if matches!(pair[0].what, Fixed::Literal(_))
                        && matches!(pair[1].what, Fixed::Class(_))
                        || matches!(pair[1].what, Fixed::Literal(_))
                            && matches!(pair[0].what, Fixed::Class(_))
                    {
                        codes::LITERAL_IS_CLASS
                    } else {
                        codes::TEXT_TWICE
                    };
                    report.diagnostic(
                        Diagnostic::new(Severity::Error, format!("`{text}` is used for two different things"), Label::new(second, "used again here"))
                            .with_secondary(Label::new(first, "first used here"))
                            .with_note("in one lexer mode, a literal, token, comment delimiter, or string delimiter must each have its own text")
                            .with_code(code),
                    );
                }
            }
            fixed.dedup_by(|x, y| x.text == y.text);
            let mut heads = vec![0u32; 257];
            for f in &fixed {
                heads[f.text[0] as usize + 1] += 1;
            }
            for i in 0..256 {
                heads[i + 1] += heads[i];
            }
            let text = b.text_kinds.get(*name).copied();
            let eof_error = mspec.and_then(|m| m.eof_error);
            let mut actions: Vec<(u16, Act)> = Vec::new();
            if let Some(m) = mspec {
                for ((tokref, ts), (action, s)) in &m.actions {
                    let (Some(kind), Some(act)) = (
                        (b.resolve)(tokref, *ts, report),
                        resolve_action(action, *s, &mode_id, report),
                    ) else {
                        continue;
                    };
                    actions.push((kind.index(), act));
                }
            }
            actions.sort_by_key(|(k, _)| *k);
            actions.dedup_by_key(|(k, _)| *k);
            let mut mode = Mode {
                fixed: fixed.into_iter().map(|f| (f.text, f.what)).collect(),
                heads: heads.into(),
                regexes: regexes.into(),
                part_strings: part_strings.into(),
                literals,
                builtins,
                trivia,
                text,
                eof_error,
                actions: actions.into(),
                may_start: [0; 4],
                name: Box::from(*name),
            };
            mode.may_start = may_start(&mode, &classes, &strings, &spec.ident);
            modes.push(mode);
        }

        // ----- `stop_before` texts must be tokens of the comment's modes -----
        for (ci, c) in spec.comments.iter().enumerate() {
            for (text, span) in &c.stop_before {
                for (mi, mode) in modes.iter().enumerate() {
                    if !active[mi].comments.contains(&(ci as u16)) {
                        continue;
                    }
                    let fixed = mode.fixed.iter().any(|(t, _)| **t == *text.as_bytes());
                    let matched =
                        mode.regexes
                            .iter()
                            .any(|&k| match &classes[k as usize].matcher {
                                Matcher::Regex(r) => r.longest_match(text) == text.len(),
                                Matcher::Literal(l) => **l == *text.as_bytes(),
                            });
                    if !fixed && !matched {
                        report.error(
                            codes::STOP_BEFORE,
                            *span,
                            format!(
                                "`stop_before` text `{text}` is not a token of mode `{}`",
                                mode.name
                            ),
                        );
                    }
                }
            }
        }

        // ----- brackets -----
        let literal_kind = |text: &str| {
            b.literals
                .iter()
                .find(|(t, _, _)| *t == text)
                .map(|(_, k, _)| *k)
        };
        let brackets: Vec<(u16, u16)> = match &spec.brackets {
            Some(pairs) => {
                let mut out = Vec::new();
                let mut seen: Vec<&str> = Vec::new();
                for (open, close, span) in pairs {
                    for t in [open, close] {
                        if seen.contains(&&**t) {
                            report.error(
                                codes::BRACKET_TWICE,
                                *span,
                                format!("`{t}` is in two bracket pairs"),
                            );
                        }
                        seen.push(t);
                    }
                    match (literal_kind(open), literal_kind(close)) {
                        (Some(o), Some(c)) => out.push((o.index(), c.index())),
                        _ => report.error(
                            codes::BRACKET_NOT_LITERAL,
                            *span,
                            format!("bracket pair `{open}` `{close}` is not two grammar literals"),
                        ),
                    }
                }
                out
            }
            None => [("(", ")"), ("[", "]"), ("{", "}")]
                .iter()
                .filter_map(|(o, c)| Some((literal_kind(o)?.index(), literal_kind(c)?.index())))
                .collect(),
        };

        // ----- keywords -----
        let mut keywords = Vec::new();
        for &(text, kind, _) in b.literals {
            if is_ident_text(text, spec.ident.mode) && !(b.contextual)(text) {
                let key = if spec.keywords.case_insensitive {
                    text.to_ascii_lowercase()
                } else {
                    String::from(text)
                };
                keywords.push((Box::<str>::from(key.as_str()), kind));
            }
        }
        // Two keywords equal under ASCII case folding cannot both be lexed.
        if spec.keywords.case_insensitive {
            let mut sorted: Vec<&(Box<str>, Kind)> = keywords.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            for pair in sorted.windows(2) {
                if pair[0].0 == pair[1].0 {
                    report.error(codes::TEXT_TWICE, Span::empty(0), format!("keywords that differ only in case (`{}`) cannot both exist with `case = \"ascii-insensitive\"`", pair[0].0));
                }
            }
        }

        let initial = match &spec.initial_mode {
            Some((m, s)) => mode_id(m, *s, report).unwrap_or(MAIN),
            None => MAIN,
        };
        let layout = b.layout.map(|l| layout_rt(l, &brackets, b, report));
        let newlines =
            spec.newlines || layout.as_ref().is_some_and(|l| l.newlines != 0 || l.indent);

        if report.errors() > errors {
            return None;
        }
        Some(Self {
            ident_mode: spec.ident.mode,
            extra_start: spec.ident.extra_start.clone().into(),
            extra_continue: spec.ident.extra_continue.clone().into(),
            require_nfc: spec.ident.require_nfc,
            numbers: Numbers::new(&spec.numbers),
            keywords: Keywords::new(keywords),
            case_insensitive: spec.keywords.case_insensitive,
            comments: comments.into(),
            classes: classes.into(),
            strings: strings.into(),
            modes: modes.into(),
            initial,
            max_depth: spec.max_mode_depth,
            shebang: spec.shebang,
            brackets: brackets.into(),
            k: b.builtins,
            newlines,
            tab_width: spec.tab_width,
            layout,
        })
    }

    /// The mode called `name`.
    pub(crate) fn mode_named(&self, name: &str) -> Option<u16> {
        self.modes
            .iter()
            .position(|m| &*m.name == name)
            .map(|i| i as u16)
    }
}

fn layout_rt(
    l: &Layout<'_>,
    brackets: &[(u16, u16)],
    b: &Build<'_, '_>,
    report: &mut Report,
) -> LayoutRt {
    let kinds = |refs: &[(alloc::borrow::Cow<'_, str>, Span)], report: &mut Report| -> Box<[u16]> {
        let mut out: Vec<u16> = refs
            .iter()
            .filter_map(|(r, s)| (b.resolve)(r, *s, report))
            .map(|k| k.index())
            .collect();
        out.sort_unstable();
        out.dedup();
        out.into()
    };
    let pairs =
        |p: &Option<Vec<crate::spec2::Pair<'_>>>, report: &mut Report| -> Box<[(u16, u16)]> {
            match p {
                None => brackets.into(),
                Some(list) => list
                    .iter()
                    .filter_map(|(o, c, s)| {
                        let o = (b.resolve)(&format!("'{o}'"), *s, report)?;
                        let c = (b.resolve)(&format!("'{c}'"), *s, report)?;
                        Some((o.index(), c.index()))
                    })
                    .collect(),
            }
        };
    let joins = pairs(&l.implicit_join, report);
    let newline_joins = match &l.ignored_inside {
        None => joins.clone(),
        some => pairs(some, report),
    };
    LayoutRt {
        indent: l.indent,
        open_after: kinds(&l.open_after, report),
        joins,
        newline_joins,
        explicit_join: l.explicit_join.as_ref().map(|(t, _)| t.as_bytes().into()),
        tab_width: l.tab_width,
        mixed_error: l.mixed_error,
        newlines: match l.newlines {
            crate::spec2::NewlineMode::Trivia => 0,
            crate::spec2::NewlineMode::Significant => 1,
            crate::spec2::NewlineMode::Terminators => 2,
        },
        terminate_after: kinds(&l.terminate_after, report),
        continue_before: kinds(&l.continue_before, report),
    }
}

/// The bytes some candidate of `mode` can begin with. Conservative: a byte
/// outside the set never begins a token; one inside may.
fn may_start(
    mode: &Mode,
    classes: &[Class],
    strings: &[StrClass],
    ident: &crate::spec2::IdentSpec,
) -> [u64; 4] {
    let mut set = [0u64; 4];
    let mut add = |b: u8| set[(b >> 6) as usize] |= 1 << (b & 63);
    for (text, _) in mode.fixed.iter() {
        add(text[0]);
    }
    for &ci in mode.regexes.iter() {
        if let Matcher::Regex(r) = &classes[ci as usize].matcher {
            for byte in 0..=255u8 {
                if r.can_start(byte) {
                    add(byte);
                }
            }
        }
    }
    for &si in mode.part_strings.iter() {
        match strings[si as usize].open.first() {
            Some(PartRt::Text(t)) => add(t[0]),
            Some(PartRt::Regex(r) | PartRt::Capture(r)) => {
                for byte in 0..=255u8 {
                    if r.can_start(byte) || r.matches_empty() {
                        add(byte);
                    }
                }
            }
            Some(PartRt::Newline) => {
                add(b'\n');
                add(b'\r');
            }
            _ => {}
        }
    }
    if mode.builtins || mode.literals {
        for b in (b'a'..=b'z')
            .chain(b'A'..=b'Z')
            .chain(core::iter::once(b'_'))
        {
            add(b);
        }
        for c in ident.extra_start.iter().filter(|c| c.is_ascii()) {
            add(*c as u8);
        }
        for b in 0x80..=0xFFu8 {
            add(b);
        }
    }
    if mode.builtins {
        for b in b'0'..=b'9' {
            add(b);
        }
        add(b'.');
    }
    if mode.trivia {
        for b in [b' ', b'\t', 0x0B, 0x0C, b'\r', b'\n'] {
            add(b);
        }
        for b in 0x80..=0xFFu8 {
            add(b);
        }
    }
    set
}

fn is_ident_text(text: &str, mode: IdentMode) -> bool {
    crate::lexer::is_ident(text, mode)
}

/// Comment openers may not begin like an identifier, a number, or whitespace.
fn check_comment_delimiter(text: &str, span: Span, report: &mut Report) {
    let Some(first) = text.chars().next() else {
        return;
    };
    if text.contains(char::is_whitespace) {
        report.error(
            codes::DELIMITER,
            span,
            format!("comment delimiter `{text}` contains whitespace"),
        );
    } else if first.is_alphanumeric() || first == '_' {
        report.error(
            codes::DELIMITER,
            span,
            format!("comment delimiter `{text}` begins with a letter, digit, or `_`, which the lexer reads as a word or number"),
        );
    }
}

fn resolve_action(
    action: &ActionSpec<'_>,
    span: Span,
    mode_id: &dyn Fn(&str, Span, &mut Report) -> Option<u16>,
    report: &mut Report,
) -> Option<Act> {
    Some(match action {
        ActionSpec::Pop => Act::Pop,
        ActionSpec::Push(m) => Act::Push(mode_id(m, span, report)?),
        ActionSpec::Switch(m) => Act::Switch(mode_id(m, span, report)?),
    })
}

fn kind_set(
    refs: &[(alloc::borrow::Cow<'_, str>, Span)],
    resolve: &dyn Fn(&str, Span, &mut Report) -> Option<Kind>,
    report: &mut Report,
) -> Box<[u16]> {
    let mut out: Vec<u16> = refs
        .iter()
        .filter_map(|(r, s)| resolve(r, *s, report))
        .map(|k| k.index())
        .collect();
    out.sort_unstable();
    out.dedup();
    out.into()
}

/// Parses and compiles a token regex, reporting problems at `span`. Equal
/// patterns compile once per forge (sketches repeat them: mox's three
/// interpolated string classes share their embedded-variable pattern).
pub(crate) fn compile_regex(
    pattern: &str,
    span: Span,
    props: &mut Props,
    report: &mut Report,
    part: bool,
) -> Option<Regex> {
    if let Some(r) = props.cached(pattern, part) {
        return Some(r.clone());
    }
    let hir = parse_regex(pattern, span, props, report)?;
    let compiled = if part {
        Regex::compile_part(&hir, REGEX_STATES)
    } else {
        Regex::compile(&hir, REGEX_STATES)
    };
    match compiled {
        Ok(r) => {
            props.cache(pattern, part, &r);
            Some(r)
        }
        Err(crate::regex::CompileError::TooLarge) => {
            report.error(
                codes::REGEX_TOO_LARGE,
                span,
                format!("regex `{pattern}` needs more than {REGEX_STATES} automaton states"),
            );
            None
        }
        Err(crate::regex::CompileError::Empty) => {
            report.error(
                codes::REGEX_EMPTY,
                span,
                format!("regex `{pattern}` matches the empty string"),
            );
            None
        }
    }
}

fn parse_regex(
    pattern: &str,
    span: Span,
    props: &mut Props,
    report: &mut Report,
) -> Option<crate::regex::Hir> {
    match crate::regex::parse::parse(pattern, props) {
        Ok(hir) => Some(hir),
        Err(e) => {
            let code = if e.reason == crate::regex::UNSUPPORTED_PROPERTY {
                codes::REGEX_PROPERTY
            } else {
                codes::REGEX_SYNTAX
            };
            report.error(
                code,
                span,
                format!(
                    "invalid regex `{pattern}` at byte {}: {}",
                    e.offset, e.reason
                ),
            );
            None
        }
    }
}

/// A condition that must be exactly one character class.
fn one_class(pattern: &str, span: Span, props: &mut Props, report: &mut Report) -> Option<CharSet> {
    let hir = parse_regex(pattern, span, props, report)?;
    match CharSet::from_hir(&hir) {
        Some(set) => Some(set),
        None => {
            report.error(
                codes::ONE_CLASS,
                span,
                format!("`{pattern}` must be a single character or character class"),
            );
            None
        }
    }
}

fn compile_parts(
    parts: &[(Part<'_>, Span)],
    close: bool,
    props: &mut Props,
    report: &mut Report,
) -> Option<Box<[PartRt]>> {
    let mut out = Vec::with_capacity(parts.len());
    let mut captures = 0;
    for (part, span) in parts {
        out.push(match part {
            Part::Text(t) => PartRt::Text(t.as_bytes().into()),
            Part::Regex(r) => PartRt::Regex(compile_regex(r, *span, props, report, true)?),
            Part::Capture(r) => {
                captures += 1;
                if close {
                    report.error(codes::STRING_CLOSE, *span, "a `capture` part belongs in `open`; the close refers to it with `{ backref = true }`");
                    return None;
                }
                if captures > 1 {
                    report.error(codes::STRING_CLOSE, *span, "an opener has at most one `capture` part");
                    return None;
                }
                PartRt::Capture(compile_regex(r, *span, props, report, true)?)
            }
            Part::Backref => {
                if !close {
                    report.error(codes::STRING_CLOSE, *span, "`{ backref = true }` belongs in `close`");
                    return None;
                }
                PartRt::Backref
            }
            Part::Newline => PartRt::Newline,
        });
    }
    if out.is_empty() {
        return None;
    }
    // Parts match greedily without backtracking: a pattern part that can
    // take the next text part's first character makes the delimiter fail
    // wherever that character follows it.
    for (i, pair) in out.windows(2).enumerate() {
        if let (PartRt::Regex(r) | PartRt::Capture(r), PartRt::Text(next)) = (&pair[0], &pair[1]) {
            let first = core::str::from_utf8(next)
                .ok()
                .and_then(|t| t.chars().next());
            if let Some(c) = first {
                let mut buf = [0u8; 4];
                if r.longest_match(c.encode_utf8(&mut buf)) > 0 {
                    report.warning(
                        codes::DEAD_PART,
                        parts[i].1,
                        format!("this part can take `{c}`, which the next part needs, so the delimiter never matches where `{c}` follows it"),
                    );
                }
            }
        }
    }
    Some(out.into())
}

// ----------------------------------------------------------------------------
// Running
// ----------------------------------------------------------------------------

/// One frame of the lexer's stack.
#[derive(Clone, Copy, Debug)]
enum Frame {
    Mode(u16),
    Str {
        class: u16,
        capture: (u32, u32),
        opener: Span,
    },
    Hole {
        class: u16,
        hole: u16,
        depth: u32,
    },
}

/// A candidate token at the current position.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    len: usize,
    priority: i8,
    category: u8,
    order: u32,
    what: What,
}

#[derive(Clone, Copy, Debug)]
enum What {
    Fixed(Fixed),
    Class(u16),
    PartString { class: u16, capture: (u32, u32) },
    Number,
    Ident(Option<Kind>),
    Space,
    Newline,
}

impl Candidate {
    /// Whether `self` beats `other` (LSF2 §9.13).
    fn beats(&self, other: &Self) -> bool {
        (
            self.len,
            self.priority,
            core::cmp::Reverse(self.category),
            core::cmp::Reverse(self.order),
        ) > (
            other.len,
            other.priority,
            core::cmp::Reverse(other.category),
            core::cmp::Reverse(other.order),
        )
    }
}

struct Run<'a> {
    sc: &'a Scanner,
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    end: usize,
    tokens: &'a mut Vec<Token<Kind>>,
    diags: &'a mut Vec<Diagnostic>,
    frames: Vec<Frame>,
    /// The previous significant token's kind index.
    prev: Option<u16>,
    /// Scratch for a number's diagnostics while it is only a candidate.
    pending: Vec<Diagnostic>,
}

impl Scanner {
    /// Scans `src` in the initial mode.
    pub(crate) fn run(
        &self,
        src: &str,
        tokens: &mut Vec<Token<Kind>>,
        diags: &mut Vec<Diagnostic>,
    ) {
        self.run_range(src, 0, src.len(), self.initial, tokens, diags);
    }

    /// Scans `src[start..end]` starting in `mode`; spans stay offsets into
    /// `src`. `start` and `end` must be character boundaries.
    pub(crate) fn run_range(
        &self,
        src: &str,
        start: usize,
        end: usize,
        mode: u16,
        tokens: &mut Vec<Token<Kind>>,
        diags: &mut Vec<Diagnostic>,
    ) {
        let first = tokens.len();
        tokens.reserve((end - start) / 2 + 1);
        let mut run = Run {
            sc: self,
            src,
            bytes: src.as_bytes(),
            pos: start,
            end,
            tokens,
            diags,
            frames: Vec::from([Frame::Mode(mode.min(self.modes.len() as u16 - 1))]),
            prev: None,
            pending: Vec::new(),
        };
        if start == 0 {
            run.prologue();
        }
        while run.pos < run.end {
            run.step();
        }
        run.unwind();
        if let Some(layout) = &self.layout {
            crate::layout::apply(self, layout, src, first, tokens, diags);
        }
    }
}

impl Run<'_> {
    fn push(&mut self, kind: Kind, start: usize, end: usize) {
        self.tokens
            .push(Token::new(kind, Span::new(start as u32, end as u32)));
        if !syntax_lang::TokenKind::is_trivia(&kind) {
            self.prev = Some(kind.index());
        }
    }

    fn error(&mut self, code: Code, start: usize, end: usize, message: impl Into<Box<str>>) {
        self.diags.push(
            Diagnostic::new(
                Severity::Error,
                message,
                Label::unlabelled(Span::new(start as u32, end as u32)),
            )
            .with_code(code),
        );
    }

    /// A leading byte-order mark (trivia) and, if enabled, a `#!` line.
    fn prologue(&mut self) {
        let mut at = 0;
        if self.src[..self.end].starts_with(BOM) {
            at = BOM.len_utf8();
            self.push(self.sc.k.whitespace, 0, at);
            self.pos = at;
        }
        if self.sc.shebang && self.bytes[at..self.end].starts_with(b"#!") {
            let end = line_end(&self.bytes[..self.end], at);
            self.push(self.sc.k.shebang, at, end);
            self.pos = end;
        }
    }

    fn step(&mut self) {
        match self.frames.last().copied() {
            Some(Frame::Str {
                class,
                capture,
                opener,
            }) => self.string_content(class, capture, opener),
            Some(Frame::Hole { class, hole, depth }) => {
                let h = &self.sc.strings[class as usize].holes[hole as usize];
                if depth == 0 && self.bytes[self.pos..self.end].starts_with(&h.close) {
                    let close = match &self.sc.strings[class as usize].kinds {
                        StringKinds::Node {
                            interp_close: Some(k),
                            ..
                        } => *k,
                        _ => self.sc.k.unknown,
                    };
                    let end = self.pos + h.close.len();
                    self.push(close, self.pos, end);
                    self.pos = end;
                    let _ = self.frames.pop();
                    return;
                }
                self.token_in(h.mode);
            }
            Some(Frame::Mode(mode)) => self.token_in(mode),
            None => {
                self.frames.push(Frame::Mode(self.sc.initial));
            }
        }
    }

    /// The character at `at`, if any (before the range end).
    fn char_at(&self, at: usize) -> Option<char> {
        (at < self.end).then(|| char_at(self.src, at))
    }

    fn follow_ok(
        &self,
        at: usize,
        followed_by: &Option<CharSet>,
        not_followed_by: &Option<CharSet>,
    ) -> bool {
        let next = self.char_at(at);
        followed_by
            .as_ref()
            .is_none_or(|set| next.is_some_and(|c| set.contains(c)))
            && not_followed_by
                .as_ref()
                .is_none_or(|set| next.is_none_or(|c| !set.contains(c)))
    }

    fn class_ok(&self, class: &Class, len: usize) -> bool {
        if !self.follow_ok(self.pos + len, &class.followed_by, &class.not_followed_by) {
            return false;
        }
        if let Some(set) = &class.when_prev {
            if self.prev.is_none_or(|p| set.binary_search(&p).is_err()) {
                return false;
            }
        }
        if let Some(set) = &class.unless_prev {
            if self.prev.is_some_and(|p| set.binary_search(&p).is_ok()) {
                return false;
            }
        }
        if class.line_start && !self.at_line_start(class.indented) {
            return false;
        }
        if let Some((lo, hi)) = class.column {
            let column = self.column();
            if column < lo || column > hi {
                return false;
            }
        }
        true
    }

    /// Whether the current position starts a line (after spaces and tabs if
    /// `indented`).
    fn at_line_start(&self, indented: bool) -> bool {
        let mut at = self.pos;
        while at > 0 {
            match self.bytes[at - 1] {
                b'\n' => return true,
                b' ' | b'\t' if indented => at -= 1,
                _ => return false,
            }
        }
        true
    }

    /// The 1-based column of the current position (LSF2 §9.10). Positions
    /// past column 4096 report 4097: no condition reaches further, and the
    /// scan back stays bounded.
    fn column(&self) -> u32 {
        let tab = self.sc.tab_width.max(1);
        let mut start = self.pos;
        let mut steps = 0;
        while start > 0 && self.bytes[start - 1] != b'\n' {
            start -= 1;
            steps += 1;
            if steps > 4 * 4097 {
                return 4097;
            }
        }
        let mut column = 1u32;
        for c in self.src[start..self.pos].chars() {
            column = if c == '\t' {
                (column - 1) / tab * tab + tab + 1
            } else {
                column + 1
            };
            if column > 4096 {
                return 4097;
            }
        }
        column
    }

    /// Collects the candidates at the current position in `mode` and returns
    /// the winner.
    fn best(&mut self, mode: u16) -> Option<Candidate> {
        let sc = self.sc;
        let m = &sc.modes[mode as usize];
        let rest = &self.bytes[self.pos..self.end];
        let lead = rest[0];
        let mut best: Option<Candidate> = None;
        let consider = |c: Candidate, best: &mut Option<Candidate>| {
            if best.is_none_or(|b| c.beats(&b)) {
                *best = Some(c);
            }
        };
        // Fixed texts.
        let group = &m.fixed[m.heads[lead as usize] as usize..m.heads[lead as usize + 1] as usize];
        for (text, what) in group {
            if !rest.starts_with(text) {
                continue;
            }
            let len = text.len();
            let (category, order, priority, ok) = match *what {
                Fixed::Literal(k) => (CAT_LITERAL, u32::from(k.index()), 0, true),
                Fixed::Class(ci) => {
                    let class = &sc.classes[ci as usize];
                    (
                        CAT_CLASS,
                        u32::from(ci),
                        class.priority,
                        self.class_ok(class, len),
                    )
                }
                Fixed::Comment(ci) => {
                    let c = &sc.comments[ci as usize];
                    (
                        CAT_COMMENT,
                        u32::from(ci),
                        0,
                        self.follow_ok(self.pos + len, &None, &c.not_followed_by),
                    )
                }
                // A string opener has no conditions of its own.
                Fixed::String(si) => (CAT_STRING, u32::from(si), 0, true),
            };
            if ok {
                consider(
                    Candidate {
                        len,
                        priority,
                        category,
                        order,
                        what: What::Fixed(*what),
                    },
                    &mut best,
                );
            }
        }
        // Regex classes.
        for &ci in m.regexes.iter() {
            let class = &sc.classes[ci as usize];
            if let Matcher::Regex(r) = &class.matcher {
                if !r.can_start(lead) {
                    continue;
                }
                let len = r.longest_match(&self.src[self.pos..self.end]);
                if len > 0 && self.class_ok(class, len) {
                    consider(
                        Candidate {
                            len,
                            priority: class.priority,
                            category: CAT_CLASS,
                            order: u32::from(ci),
                            what: What::Class(ci),
                        },
                        &mut best,
                    );
                }
            }
        }
        // String openers with parts.
        for &si in m.part_strings.iter() {
            if let Some((len, capture)) = self.match_open(si) {
                consider(
                    Candidate {
                        len,
                        priority: 0,
                        category: CAT_STRING,
                        order: u32::from(si),
                        what: What::PartString { class: si, capture },
                    },
                    &mut best,
                );
            }
        }
        // Numbers.
        if m.builtins
            && (lead.is_ascii_digit()
                || (lead == b'.'
                    && sc.numbers.leading_dot
                    && rest.get(1).is_some_and(u8::is_ascii_digit)))
        {
            let mut pending = core::mem::take(&mut self.pending);
            pending.clear();
            let len = self.number(&mut pending) - self.pos;
            self.pending = pending;
            consider(
                Candidate {
                    len,
                    priority: 0,
                    category: CAT_NUMBER,
                    order: 0,
                    what: What::Number,
                },
                &mut best,
            );
        }
        // Identifiers and keywords.
        if m.builtins || m.literals {
            let c = if lead < 0x80 {
                char::from(lead)
            } else {
                char_at(self.src, self.pos)
            };
            if self.is_ident_start(c) {
                let end = self.ident_end(self.pos + c.len_utf8());
                let lexeme = &self.src[self.pos..end];
                let keyword = if m.literals {
                    self.keyword(lexeme)
                } else {
                    None
                };
                if keyword.is_some() || m.builtins {
                    let category = if keyword.is_some() {
                        CAT_LITERAL
                    } else {
                        CAT_IDENT
                    };
                    let order = keyword.map_or(0, |k| u32::from(k.index()));
                    consider(
                        Candidate {
                            len: end - self.pos,
                            priority: 0,
                            category,
                            order,
                            what: What::Ident(keyword),
                        },
                        &mut best,
                    );
                }
            }
        }
        // Whitespace.
        if m.trivia {
            if let Some(join) = sc.layout.as_ref().and_then(|l| l.explicit_join.as_deref()) {
                if rest.starts_with(join) {
                    let after = self.pos + join.len();
                    let nl = match self.bytes.get(after..self.end) {
                        Some([b'\n', ..]) => 1,
                        Some([b'\r', b'\n', ..]) => 2,
                        _ => 0,
                    };
                    if nl > 0 {
                        consider(
                            Candidate {
                                len: join.len() + nl,
                                priority: 0,
                                category: CAT_SPACE,
                                order: 0,
                                what: What::Space,
                            },
                            &mut best,
                        );
                    }
                }
            }
            let newline_len = match rest {
                [b'\n', ..] => 1,
                [b'\r', b'\n', ..] => 2,
                _ => 0,
            };
            if sc.newlines && newline_len > 0 {
                consider(
                    Candidate {
                        len: newline_len,
                        priority: 0,
                        category: CAT_SPACE,
                        order: 0,
                        what: What::Newline,
                    },
                    &mut best,
                );
            } else {
                let end = self.space_end(self.pos);
                if end > self.pos {
                    consider(
                        Candidate {
                            len: end - self.pos,
                            priority: 0,
                            category: CAT_SPACE,
                            order: 1,
                            what: What::Space,
                        },
                        &mut best,
                    );
                }
            }
        }
        best
    }

    /// Lexes one token (or text run) in `mode`.
    fn token_in(&mut self, mode: u16) {
        let start = self.pos;
        let Some(best) = self.best(mode) else {
            return self.nothing(mode);
        };
        let sc = self.sc;
        let mut end = start + best.len;
        let kind = match best.what {
            What::Fixed(Fixed::Literal(k)) => k,
            What::Fixed(Fixed::Class(ci)) | What::Class(ci) => {
                let class = &sc.classes[ci as usize];
                self.push(class.kind, start, end);
                self.pos = end;
                // The token's own action, then the actions of the mode it was
                // lexed in.
                if let Some(act) = class.action {
                    self.act(act, start, end);
                }
                self.after_token_in(class.kind, mode);
                return;
            }
            What::Fixed(Fixed::Comment(ci)) => {
                let c = &sc.comments[ci as usize];
                end = self.comment_end(c, start, end);
                let kind = if c.doc {
                    sc.k.doc_comment
                } else {
                    sc.k.comment
                };
                self.push(kind, start, end);
                self.pos = end;
                return;
            }
            What::Fixed(Fixed::String(si)) => return self.open_string(si, start, end, (0, 0)),
            What::PartString { class, capture } => {
                return self.open_string(class, start, end, capture);
            }
            What::Number => {
                let pending = core::mem::take(&mut self.pending);
                self.diags.extend(pending);
                sc.k.number
            }
            What::Ident(Some(k)) => k,
            What::Ident(None) => {
                if sc.require_nfc {
                    let lexeme = &self.src[start..end];
                    if !lexeme.is_ascii()
                        && !unicode_lang::is_normalized(lexeme, unicode_lang::Form::Nfc)
                    {
                        self.error(
                            codes::LEX_NOT_NFC,
                            start,
                            end,
                            format!("identifier `{lexeme}` is not in Unicode normal form C"),
                        );
                    }
                }
                sc.k.ident
            }
            What::Space => sc.k.whitespace,
            What::Newline => sc.k.newline,
        };
        self.push(kind, start, end);
        self.pos = end;
        self.after_token_in(kind, mode);
    }

    /// Bracket depth and the actions of `mode` after a token lexed in it.
    fn after_token_in(&mut self, kind: Kind, mode: u16) {
        let index = kind.index();
        // Bracket depth of the innermost hole.
        if let Some(Frame::Hole { depth, .. }) = self.frames.last_mut() {
            if self.sc.brackets.iter().any(|(o, _)| *o == index) {
                *depth += 1;
            } else if self.sc.brackets.iter().any(|(_, c)| *c == index) {
                *depth = depth.saturating_sub(1);
            }
        }
        let actions = &self.sc.modes[mode as usize].actions;
        if let Ok(at) = actions.binary_search_by_key(&index, |(k, _)| *k) {
            let act = actions[at].1;
            let (start, end) = (self.pos, self.pos);
            self.act(act, start, end);
        }
    }

    fn act(&mut self, act: Act, start: usize, end: usize) {
        match act {
            Act::Pop => {
                if self.frames.len() > 1 {
                    let _ = self.frames.pop();
                }
            }
            Act::Push(m) => {
                if self.frames.len() as u32 >= self.sc.max_depth {
                    self.error(
                        codes::LEX_TOO_DEEP,
                        start,
                        end,
                        "lexer modes are nested too deeply",
                    );
                } else {
                    self.frames.push(Frame::Mode(m));
                }
            }
            Act::Switch(m) => match self.frames.last_mut() {
                Some(frame @ Frame::Mode(_)) => *frame = Frame::Mode(m),
                _ => {
                    if self.frames.len() as u32 >= self.sc.max_depth {
                        self.error(
                            codes::LEX_TOO_DEEP,
                            start,
                            end,
                            "lexer modes are nested too deeply",
                        );
                    } else {
                        self.frames.push(Frame::Mode(m));
                    }
                }
            },
        }
    }

    /// Nothing starts here: a text run of the mode's `text` class, or an
    /// `UNKNOWN` run reported once.
    fn nothing(&mut self, mode: u16) {
        let start = self.pos;
        let mut end = start + utf8_len(self.bytes[start]).min(self.end - start);
        loop {
            if end >= self.end || self.starts_something(mode, end) {
                break;
            }
            end += utf8_len(self.bytes[end]).min(self.end - end);
        }
        match self.sc.modes[mode as usize].text {
            Some(kind) => {
                self.push(kind, start, end);
                self.pos = end;
            }
            None => {
                let text = &self.src[start..end];
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
                self.error(
                    codes::LEX_UNEXPECTED,
                    start,
                    end,
                    format!("unexpected {plural} {shown}"),
                );
                self.push(self.sc.k.unknown, start, end);
                self.pos = end;
            }
        }
    }

    /// Whether some token of `mode` starts at `at`.
    fn starts_something(&mut self, mode: u16, at: usize) -> bool {
        let lead = self.bytes[at];
        if (self.sc.modes[mode as usize].may_start[(lead >> 6) as usize] >> (lead & 63)) & 1 == 0 {
            return false;
        }
        let saved = self.pos;
        self.pos = at;
        let found = self.best(mode).is_some();
        self.pos = saved;
        found
    }

    fn is_ident_start(&self, c: char) -> bool {
        crate::lexer::is_ident_start(c, self.sc.ident_mode) || self.sc.extra_start.contains(&c)
    }

    fn is_ident_continue(&self, c: char) -> bool {
        crate::lexer::is_ident_continue(c, self.sc.ident_mode)
            || self.sc.extra_continue.contains(&c)
            || self.sc.extra_start.contains(&c)
    }

    fn ident_end(&self, mut at: usize) -> usize {
        while at < self.end {
            let b = self.bytes[at];
            if b.is_ascii_alphanumeric() || b == b'_' {
                at += 1;
                continue;
            }
            let c = char_at(self.src, at);
            if self.is_ident_continue(c) {
                at += c.len_utf8();
            } else {
                break;
            }
        }
        at
    }

    /// The reserved keyword `lexeme` is, under the case policy.
    fn keyword(&self, lexeme: &str) -> Option<Kind> {
        if !self.sc.case_insensitive {
            return self.sc.keywords.get(lexeme);
        }
        let mut buf = [0u8; 64];
        let bytes = lexeme.as_bytes();
        if bytes.len() > buf.len() {
            return None;
        }
        for (d, s) in buf.iter_mut().zip(bytes) {
            *d = s.to_ascii_lowercase();
        }
        core::str::from_utf8(&buf[..bytes.len()])
            .ok()
            .and_then(|k| self.sc.keywords.get(k))
    }

    fn space_end(&self, mut at: usize) -> usize {
        while at < self.end {
            match self.bytes[at] {
                b' ' | b'\t' | 0x0B | 0x0C => at += 1,
                b'\n' if !self.sc.newlines => at += 1,
                b'\r' if !self.sc.newlines || self.bytes.get(at + 1) != Some(&b'\n') => at += 1,
                b if b >= 0x80 && is_unicode_space(char_at(self.src, at)) => {
                    at += char_at(self.src, at).len_utf8()
                }
                _ => break,
            }
        }
        at
    }

    fn comment_end(&mut self, c: &Comment, start: usize, mut at: usize) -> usize {
        let bytes = &self.bytes[..self.end];
        match &c.close {
            None => {
                let mut end = line_end(bytes, at);
                for stop in c.stop_before.iter() {
                    if let Some(found) = find(&bytes[at..end], stop) {
                        end = end.min(at + found);
                    }
                }
                end
            }
            Some(close) => {
                let mut depth = 1usize;
                while at < bytes.len() {
                    let rest = &bytes[at..];
                    if rest.starts_with(close) {
                        depth -= 1;
                        at += close.len();
                        if depth == 0 {
                            return at;
                        }
                    } else if c.nested && rest.starts_with(&c.open) {
                        depth += 1;
                        at += c.open.len();
                    } else {
                        at += 1;
                    }
                }
                self.error(
                    codes::LEX_UNTERMINATED_COMMENT,
                    start,
                    start + c.open.len(),
                    "unterminated block comment",
                );
                bytes.len()
            }
        }
    }

    /// Matches the opener of part-string `si` here: its length and capture.
    fn match_open(&self, si: u16) -> Option<(usize, (u32, u32))> {
        let class = &self.sc.strings[si as usize];
        let mut at = self.pos;
        let mut capture = (0u32, 0u32);
        for part in class.open.iter() {
            let rest = &self.bytes[at..self.end];
            match part {
                PartRt::Text(t) => {
                    if !rest.starts_with(t) {
                        return None;
                    }
                    at += t.len();
                }
                PartRt::Regex(r) => at += r.match_len(&self.src[at..self.end])?,
                PartRt::Capture(r) => {
                    let len = r.match_len(&self.src[at..self.end])?;
                    capture = (at as u32, (at + len) as u32);
                    at += len;
                }
                PartRt::Newline => at += newline_len(rest)?,
                PartRt::Backref => return None,
            }
        }
        (at > self.pos).then_some((at - self.pos, capture))
    }

    /// Matches the close parts of `class` at `at`; the end, if they match.
    fn match_close_parts(
        &self,
        class: &StrClass,
        mut at: usize,
        capture: (u32, u32),
    ) -> Option<usize> {
        for part in class.close.iter() {
            let rest = &self.bytes[at..self.end];
            match part {
                PartRt::Text(t) => {
                    if !rest.starts_with(t) {
                        return None;
                    }
                    at += t.len();
                }
                PartRt::Regex(r) | PartRt::Capture(r) => {
                    at += r.match_len(&self.src[at..self.end])?
                }
                PartRt::Backref => {
                    let text = &self.bytes[capture.0 as usize..capture.1 as usize];
                    if !rest.starts_with(text) {
                        return None;
                    }
                    at += text.len();
                }
                PartRt::Newline => at += newline_len(rest)?,
            }
        }
        self.follow_ok(at, &None, &class.close_not_followed_by)
            .then_some(at)
    }

    /// Whether `class`'s close delimiter starts at the current position:
    /// `(start of the close token, end)`.
    fn match_close(
        &self,
        class: &StrClass,
        capture: (u32, u32),
        content_start: usize,
    ) -> Option<(usize, usize)> {
        let at = self.pos;
        match class.close_at {
            CloseAt::Anywhere => self
                .match_close_parts(class, at, capture)
                .map(|end| (at, end)),
            CloseAt::LineStart | CloseAt::LineStartIndented => {
                let indented = class.close_at == CloseAt::LineStartIndented;
                let skip = |mut p: usize| {
                    if indented {
                        while p < self.end && matches!(self.bytes[p], b' ' | b'\t') {
                            p += 1;
                        }
                    }
                    p
                };
                // The line break before the close belongs to it.
                if let Some(nl) = newline_len(&self.bytes[at..self.end]) {
                    if let Some(end) = self.match_close_parts(class, skip(at + nl), capture) {
                        return Some((at, end));
                    }
                }
                // Content that is empty, or starts right at a line start.
                let line_start = at == 0 || self.bytes[at - 1] == b'\n';
                if at == content_start && line_start {
                    if let Some(end) = self.match_close_parts(class, skip(at), capture) {
                        return Some((at, end));
                    }
                }
                None
            }
        }
    }

    /// Lexes a string opener: one token for a plain class, or the `_OPEN`
    /// token and a string frame for a class that builds a node.
    fn open_string(&mut self, si: u16, start: usize, mut open_end: usize, capture: (u32, u32)) {
        let sc = self.sc;
        let class = &sc.strings[si as usize];
        if class.next_line {
            // The content starts after the line break that ends the opener's
            // line; the rest of that line must be blank.
            let line = line_end(&self.bytes[..self.end], open_end);
            if self.bytes[open_end..line]
                .iter()
                .any(|b| !matches!(b, b' ' | b'\t' | b'\r'))
            {
                self.error(
                    codes::LEX_UNTERMINATED_STRING,
                    start,
                    line,
                    "the opener of this string must end its line",
                );
            }
            open_end = line;
            open_end += newline_len(&self.bytes[open_end..self.end]).unwrap_or(0);
        }
        let opener = Span::new(start as u32, open_end as u32);
        match class.kinds {
            StringKinds::Token(kind) => {
                self.pos = open_end;
                let end = self.single_string(class, capture, opener);
                self.push(kind, start, end);
                self.pos = end;
            }
            StringKinds::Node { open, .. } => {
                self.push(open, start, open_end);
                self.pos = open_end;
                // Past the budget the frame is not pushed: the string's
                // content is lexed as code, after one error (LSF2 §9.8.4).
                if self.frames.len() as u32 >= self.sc.max_depth {
                    self.error(
                        codes::LEX_TOO_DEEP,
                        start,
                        open_end,
                        "lexer modes are nested too deeply",
                    );
                } else {
                    self.frames.push(Frame::Str {
                        class: si,
                        capture,
                        opener,
                    });
                }
            }
        }
    }

    /// Scans a plain string's content and close from the current position;
    /// returns the token's end.
    fn single_string(&mut self, class: &StrClass, capture: (u32, u32), opener: Span) -> usize {
        let content_start = self.pos;
        loop {
            if self.pos >= self.end {
                self.error(
                    codes::LEX_UNTERMINATED_STRING,
                    opener.start().to_usize(),
                    opener.end().to_usize(),
                    "unterminated string",
                );
                return self.end;
            }
            if let Some((_, end)) = self.match_close(class, capture, content_start) {
                return end;
            }
            let c = char_at(self.src, self.pos);
            if Some(c) == class.escape {
                self.pos += c.len_utf8();
                if self.pos < self.end {
                    self.pos += utf8_len(self.bytes[self.pos]).min(self.end - self.pos);
                }
                continue;
            }
            if c == '\n' && !class.multiline {
                self.error(
                    codes::LEX_UNTERMINATED_STRING,
                    opener.start().to_usize(),
                    opener.end().to_usize(),
                    "unterminated string",
                );
                return if self.pos > content_start && self.bytes[self.pos - 1] == b'\r' {
                    self.pos - 1
                } else {
                    self.pos
                };
            }
            self.pos += c.len_utf8();
        }
    }

    /// Scans a node string's content from the current position until the
    /// next structure: an embedded token, an escape token, a hole, or the
    /// close.
    fn string_content(&mut self, si: u16, capture: (u32, u32), opener: Span) {
        let sc = self.sc;
        let class = &sc.strings[si as usize];
        let StringKinds::Node {
            text,
            escape,
            ref embedded,
            interp_open,
            close,
            ..
        } = class.kinds
        else {
            let _ = self.frames.pop();
            return;
        };
        let content_start = opener.end().to_usize();
        let run_start = self.pos;
        let flush = |run: &mut Self, upto: usize| {
            if upto > run_start {
                run.push(text, run_start, upto);
            }
        };
        loop {
            let at = self.pos;
            if at >= self.end {
                flush(self, at);
                return; // unwound at the end
            }
            if let Some((close_start, close_end)) = self.match_close(class, capture, content_start)
            {
                flush(self, close_start);
                self.push(close, close_start, close_end);
                self.pos = close_end;
                let _ = self.frames.pop();
                return;
            }
            let rest = &self.bytes[at..self.end];
            for (hi, hole) in class.holes.iter().enumerate() {
                if rest.starts_with(&hole.open)
                    && self.follow_ok(at + hole.open.len(), &hole.when_next, &None)
                {
                    flush(self, at);
                    let end = at + hole.open.len();
                    self.push(interp_open.unwrap_or(text), at, end);
                    self.pos = end;
                    if self.frames.len() as u32 >= sc.max_depth {
                        self.error(
                            codes::LEX_TOO_DEEP,
                            at,
                            end,
                            "lexer modes are nested too deeply",
                        );
                    } else {
                        self.frames.push(Frame::Hole {
                            class: si,
                            hole: hi as u16,
                            depth: 0,
                        });
                    }
                    return;
                }
            }
            let mut best: Option<(usize, usize)> = None;
            for (ei, r) in class.embedded.iter().enumerate() {
                let len = r.longest_match(&self.src[at..self.end]);
                if len > 0 && best.is_none_or(|(l, _)| len > l) {
                    best = Some((len, ei));
                }
            }
            if let Some((len, ei)) = best {
                flush(self, at);
                self.push(embedded[ei], at, at + len);
                self.pos = at + len;
                return;
            }
            let c = char_at(self.src, at);
            if Some(c) == class.escape {
                let mut end = at + c.len_utf8();
                if end < self.end {
                    end += utf8_len(self.bytes[end]).min(self.end - end);
                }
                if let Some(kind) = escape {
                    flush(self, at);
                    self.push(kind, at, end);
                    self.pos = end;
                    return;
                }
                self.pos = end;
                continue;
            }
            if c == '\n' && !class.multiline {
                let upto = if at > run_start && self.bytes[at - 1] == b'\r' {
                    at - 1
                } else {
                    at
                };
                flush(self, upto);
                self.error(
                    codes::LEX_UNTERMINATED_STRING,
                    opener.start().to_usize(),
                    opener.end().to_usize(),
                    "unterminated string",
                );
                self.push(close, upto, upto);
                self.pos = upto;
                let _ = self.frames.pop();
                return;
            }
            self.pos += c.len_utf8();
        }
    }

    /// Closes every frame still open at the end of the range.
    fn unwind(&mut self) {
        let at = self.end;
        if let [Frame::Mode(m)] = self.frames[..] {
            if self.sc.modes[m as usize].eof_error == Some(true) {
                let name = &self.sc.modes[m as usize].name;
                self.error(
                    codes::LEX_UNTERMINATED_MODE,
                    at,
                    at,
                    format!("the input ends inside `{name}`"),
                );
            }
        }
        while self.frames.len() > 1
            || matches!(
                self.frames.last(),
                Some(Frame::Str { .. } | Frame::Hole { .. })
            )
        {
            let Some(frame) = self.frames.pop() else {
                break;
            };
            match frame {
                Frame::Str { class, opener, .. } => {
                    if let StringKinds::Node { close, .. } = self.sc.strings[class as usize].kinds {
                        self.error(
                            codes::LEX_UNTERMINATED_STRING,
                            opener.start().to_usize(),
                            opener.end().to_usize(),
                            "unterminated string",
                        );
                        self.push(close, at, at);
                    }
                }
                Frame::Hole { class, .. } => {
                    if let StringKinds::Node {
                        interp_close: Some(k),
                        ..
                    } = self.sc.strings[class as usize].kinds
                    {
                        self.push(k, at, at);
                    }
                }
                Frame::Mode(m) => {
                    // A pushed frame (the loop leaves the initial one).
                    if self.sc.modes[m as usize].eof_error != Some(false) {
                        let name = &self.sc.modes[m as usize].name;
                        self.error(
                            codes::LEX_UNTERMINATED_MODE,
                            at,
                            at,
                            format!("the input ends inside `{name}`"),
                        );
                    }
                }
            }
        }
    }

    /// A number literal (LSF2 §9.6); diagnostics go to `out`. Returns its end.
    fn number(&self, out: &mut Vec<Diagnostic>) -> usize {
        let n = &self.sc.numbers;
        let bytes = &self.bytes[..self.end];
        let start = self.pos;
        let sep = n.separator;
        let is_sep = |at: usize| sep.is_some_and(|s| self.char_at(at) == Some(s));
        let sep_len = sep.map_or(1, char::len_utf8);
        let err = |out: &mut Vec<Diagnostic>, code: Code, s: usize, e: usize, m: String| {
            out.push(
                Diagnostic::new(
                    Severity::Error,
                    m,
                    Label::unlabelled(Span::new(s as u32, e as u32)),
                )
                .with_code(code),
            );
        };
        // Digits of `radix` with separators, from `at`; checks separator
        // placement. Returns the end.
        let digits = |mut at: usize, radix: u32, out: &mut Vec<Diagnostic>| {
            let first = at;
            let mut last_digit = false;
            while at < bytes.len() {
                if char::from(bytes[at]).is_digit(radix) {
                    last_digit = true;
                    at += 1;
                } else if is_sep(at) {
                    // A run of separators is one misplacement: `1__0` is one
                    // error, not two (the second `_` follows no digit).
                    let mut run = at + sep_len;
                    while is_sep(run) {
                        run += sep_len;
                    }
                    let next_digit = bytes
                        .get(run)
                        .is_some_and(|b| char::from(*b).is_digit(radix));
                    if !n.separator_anywhere && (!last_digit || !next_digit || run > at + sep_len) {
                        if !next_digit
                            && !char::from(*bytes.get(run).unwrap_or(&b' ')).is_alphanumeric()
                            && at > first
                        {
                            // A trailing separator (`1_`): reported, kept in the token.
                            err(
                                out,
                                codes::LEX_SEPARATOR,
                                at,
                                run,
                                String::from("a digit separator must sit between two digits"),
                            );
                            at = run;
                            break;
                        }
                        err(
                            out,
                            codes::LEX_SEPARATOR,
                            at,
                            run,
                            String::from("a digit separator must sit between two digits"),
                        );
                    }
                    last_digit = false;
                    at = run;
                } else {
                    break;
                }
            }
            at
        };
        let lower = |b: u8| b.to_ascii_lowercase();
        let prefix = bytes.get(start + 1).copied();
        let radix = match (bytes[start], prefix) {
            (b'0', Some(p)) if n.radix[0] && (p == b'x' || (n.radix_any_case && p == b'X')) => 16,
            (b'0', Some(p)) if n.radix[1] && (p == b'o' || (n.radix_any_case && p == b'O')) => 8,
            (b'0', Some(p)) if n.radix[2] && (p == b'b' || (n.radix_any_case && p == b'B')) => 2,
            _ => 10,
        };
        if radix != 10 {
            let digits_start = start + 2;
            let mut at = digits(digits_start, radix, out);
            let name = match radix {
                16 => "hexadecimal",
                8 => "octal",
                _ => "binary",
            };
            if at == digits_start {
                // `0x` alone, or `0xZ`: report what follows.
                let end = self.ident_end(at);
                if end > at {
                    let bad = char_at(self.src, at);
                    err(
                        out,
                        codes::LEX_BAD_DIGIT,
                        at,
                        at + bad.len_utf8(),
                        format!("invalid digit `{bad}` in {name} literal"),
                    );
                } else {
                    err(
                        out,
                        codes::LEX_NO_DIGITS,
                        start,
                        end,
                        format!("{name} literal has no digits"),
                    );
                }
                return end;
            }
            if radix == 16 && n.hex_floats && bytes.get(at) == Some(&b'.') {
                at = digits(at + 1, 16, out);
            }
            if radix == 16
                && n.hex_floats
                && matches!(bytes.get(at).copied().map(lower), Some(b'p'))
            {
                let sign = usize::from(matches!(bytes.get(at + 1), Some(b'+' | b'-')));
                if bytes.get(at + 1 + sign).is_some_and(u8::is_ascii_digit) {
                    at = digits(at + 1 + sign, 10, out);
                } else {
                    err(
                        out,
                        codes::LEX_EXPONENT,
                        at,
                        at + 1 + sign,
                        String::from("the exponent of a hexadecimal float has no digits"),
                    );
                    at += 1 + sign;
                }
            }
            return self.suffix(at, out);
        }
        // Decimal.
        let mut at = start;
        if bytes[start] != b'.' {
            at = digits(start, 10, out);
            let int = &self.src[start..at];
            if int.len() > 1
                && int.starts_with('0')
                && int
                    .as_bytes()
                    .get(1)
                    .copied()
                    .is_some_and(|b| b.is_ascii_digit())
            {
                match n.leading_zeros {
                    LeadingZeros::Decimal => {}
                    LeadingZeros::Error => err(
                        out,
                        codes::LEX_LEADING_ZERO,
                        start,
                        at,
                        format!(
                            "decimal literal `{int}` has a leading zero; write `0o{}` for octal",
                            int.trim_start_matches('0')
                        ),
                    ),
                    LeadingZeros::Octal => {
                        if let Some(bad) = int.bytes().position(|b| matches!(b, b'8' | b'9')) {
                            err(
                                out,
                                codes::LEX_BAD_DIGIT,
                                start + bad,
                                start + bad + 1,
                                format!(
                                    "invalid digit `{}` in octal literal",
                                    char::from(int.as_bytes()[bad])
                                ),
                            );
                        }
                    }
                }
            }
        }
        let mut float = false;
        if bytes.get(at) == Some(&b'.') {
            let digit_after = bytes.get(at + 1).is_some_and(u8::is_ascii_digit);
            if n.floats && digit_after {
                at = digits(at + 1, 10, out);
                float = true;
            } else if n.floats && n.trailing_dot && at > start {
                at += 1;
                float = true;
            }
        }
        let _ = float;
        if n.exponent && matches!(bytes.get(at), Some(b'e' | b'E')) {
            let sign = usize::from(matches!(bytes.get(at + 1), Some(b'+' | b'-')));
            if bytes.get(at + 1 + sign).is_some_and(u8::is_ascii_digit) && at > start {
                at = digits(at + 1 + sign, 10, out);
            }
        }
        self.suffix(at, out)
    }

    /// Letters running on after a number: an accepted suffix, or reported.
    fn suffix(&self, at: usize, out: &mut Vec<Diagnostic>) -> usize {
        let end = self.ident_end(at);
        if end > at {
            let run = &self.src[at..end];
            if !self.sc.numbers.suffixes.iter().any(|s| **s == *run) {
                out.push(
                    Diagnostic::new(
                        Severity::Error,
                        format!("invalid suffix `{run}` on number literal"),
                        Label::unlabelled(Span::new(at as u32, end as u32)),
                    )
                    .with_code(codes::LEX_NUMBER_SUFFIX),
                );
            }
        }
        end
    }
}

/// The length of a line break at the start of `bytes`.
fn newline_len(bytes: &[u8]) -> Option<usize> {
    match bytes {
        [b'\n', ..] => Some(1),
        [b'\r', b'\n', ..] => Some(2),
        _ => None,
    }
}

/// The position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
