//! [`Error`]: why a sketch could not be forged, or a pipeline assembled.

use alloc::{boxed::Box, vec::Vec};
use core::fmt;

use diag_lang::{Code, Diagnostic, Label, Severity, SourceMap};
use syntax_lang::Span;

/// Why a sketch could not be forged into a [`Language`](crate::Language),
/// or a capability pipeline could not be assembled.
///
/// Forging checks the whole sketch and reports every problem it finds, not
/// just the first, so one round of fixes is usually enough. Each problem is a
/// [`Diagnostic`] with a [`Code`] (`LSF` and four digits, LSF2 §27) and a
/// label that points into the sketch. For a single document handed to
/// [`Language::from_lsf`](crate::Language::from_lsf) the spans are byte
/// offsets into that text: add it to a fresh
/// [`SourceMap`](diag_lang::SourceMap) and the spans line up. For a
/// [`Sketch`](crate::Sketch) of several files they are positions in the
/// sketch's own [`source_map`](crate::Sketch::source_map). Either way
/// [`Renderer`](diag_lang::Renderer) draws each problem under the line at
/// fault.
///
/// `Display` prints the first problem as `line:column: message` (with the
/// file's path in front for a multi-file sketch), which is enough for a log
/// line or a test failure; render the diagnostics for the full picture.
///
/// # What to do with one
///
/// An `Error` always means the sketch needs editing: forging is
/// deterministic, so retrying the same text fails the same way. Every message
/// names what was found and what was expected, and many carry a `help` line
/// with the fix.
///
/// # Examples
///
/// ```
/// use lang_forge::Language;
/// use lang_forge::diag_lang::{Renderer, SourceMap};
///
/// let schematic = r#"
/// [language]
/// name = "broken"
///
/// [rules]
/// program = "stmt*"
/// stmt    = "exprr ';'"
/// expr    = "NUMBER"
/// "#;
///
/// let err = Language::from_lsf(schematic).unwrap_err();
/// assert_eq!(err.to_string(), "7:12: undefined rule `exprr`");
/// assert_eq!(err.diagnostics().len(), 1);
/// assert_eq!(err.diagnostics()[0].code().map(|c| c.to_string()).as_deref(), Some("LSF4101"));
///
/// // Render it with source context.
/// let mut map = SourceMap::new();
/// map.add("broken.lsf", schematic).expect("fits");
/// let text = Renderer::new().render(&err.diagnostics()[0], &map);
/// assert!(text.contains("help: did you mean `expr`?"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    diagnostics: Vec<Diagnostic>,
    line: u32,
    column: u32,
    /// The file of the first problem, for multi-file sketches.
    path: Option<Box<str>>,
}

impl Error {
    /// An error from collected diagnostics, locating the first in `text`.
    pub(crate) fn new(diagnostics: Vec<Diagnostic>, text: &str) -> Self {
        let offset = diagnostics
            .first()
            .map_or(0, |d| d.primary().span().start().to_usize());
        let (line, column) = line_col(text, offset);
        Self {
            diagnostics,
            line,
            column,
            path: None,
        }
    }

    /// An error whose diagnostics have spans into `map`, locating the first
    /// in the file it falls in.
    pub(crate) fn in_map(diagnostics: Vec<Diagnostic>, map: &SourceMap) -> Self {
        let mut error = Self {
            diagnostics,
            line: 1,
            column: 1,
            path: None,
        };
        let at = error
            .diagnostics
            .first()
            .map(|d| d.primary().span().start());
        if let Some((id, local)) = at.and_then(|at| map.locate(at)) {
            if let Some(file) = map.source(id) {
                let (line, column) = line_col(file.text(), local.to_usize());
                error.line = line;
                error.column = column;
                error.path = Some(Box::from(file.name()));
            }
        }
        error
    }

    /// An error whose first diagnostic is already located.
    pub(crate) fn located(diagnostics: Vec<Diagnostic>, line: u32, column: u32) -> Self {
        Self {
            diagnostics,
            line,
            column,
            path: None,
        }
    }

    /// Every problem found, in the order they appear in the sketch.
    ///
    /// Never empty. Spans are byte offsets into the sketch text (positions in
    /// the sketch's source map for a multi-file [`Sketch`](crate::Sketch)).
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let err = Language::from_lsf("[language]\n[rules]\n").unwrap_err();
    /// let messages: Vec<&str> = err.diagnostics().iter().map(|d| d.message()).collect();
    /// assert_eq!(messages, ["missing `name` in [language]", "[rules] declares no rules"]);
    /// ```
    #[inline]
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self.diagnostics.first().map_or("", |d| d.message());
        if let Some(path) = &self.path {
            write!(f, "{path}:")?;
        }
        write!(f, "{}:{}: {}", self.line, self.column, message)?;
        match self.diagnostics.len() {
            0 | 1 => Ok(()),
            2 => f.write_str(" (and 1 more error)"),
            n => write!(f, " (and {} more errors)", n - 1),
        }
    }
}

impl core::error::Error for Error {}

/// The 1-based line and column (in characters) of byte `offset` in `text`.
pub(crate) fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
    let column = before[line_start..].chars().count() + 1;
    (saturate(line), saturate(column))
}

fn saturate(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// An error diagnostic with a code, a span, and a message.
pub(crate) fn coded(code: Code, span: Span, message: impl Into<Box<str>>) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, Label::unlabelled(span)).with_code(code)
}

/// Collects the problems found while forging.
#[derive(Debug)]
pub(crate) struct Report {
    diagnostics: Vec<Diagnostic>,
    errors: usize,
    /// The did-you-mean work budget shared by the whole forge (ISSUES P10).
    budget: u64,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            errors: 0,
            budget: crate::suggest::BUDGET,
        }
    }
}

impl Report {
    /// "did you mean `x`?" for the closest of `candidates` within `limit`
    /// edits, from the forge's bounded suggestion budget.
    pub(crate) fn suggest<'a>(
        &mut self,
        name: &str,
        candidates: impl Iterator<Item = &'a str>,
        limit: usize,
    ) -> Option<alloc::string::String> {
        crate::suggest::suggest_with(name, candidates, limit, &mut self.budget)
    }

    /// Records an error at `span`.
    pub(crate) fn error(&mut self, code: Code, span: Span, message: impl Into<Box<str>>) {
        self.diagnostic(coded(code, span, message));
    }

    /// Records an error at `span` with a `help` line.
    pub(crate) fn error_help(
        &mut self,
        code: Code,
        span: Span,
        message: impl Into<Box<str>>,
        help: impl Into<Box<str>>,
    ) {
        self.diagnostic(coded(code, span, message).with_help(help));
    }

    /// Records a warning at `span`: reported, but the sketch still forges.
    pub(crate) fn warning(&mut self, code: Code, span: Span, message: impl Into<Box<str>>) {
        self.diagnostic(
            Diagnostic::new(Severity::Warning, message, Label::unlabelled(span)).with_code(code),
        );
    }

    /// Records a fully built diagnostic.
    pub(crate) fn diagnostic(&mut self, diagnostic: Diagnostic) {
        if diagnostic.severity() == Severity::Error {
            self.errors += 1;
        }
        self.diagnostics.push(diagnostic);
    }

    /// Whether no error has been recorded (warnings may have been).
    pub(crate) fn is_clean(&self) -> bool {
        self.errors == 0
    }

    /// How many errors have been recorded.
    pub(crate) fn errors(&self) -> usize {
        self.errors
    }

    /// Sorts the problems by position, stably.
    fn sort(&mut self) {
        self.diagnostics
            .sort_by_key(|d| d.primary().span().start().to_u32());
    }

    /// Converts the report into an error, ordering problems by position.
    pub(crate) fn into_error(mut self, text: &str) -> Error {
        self.sort();
        Error::new(self.diagnostics, text)
    }

    /// Converts the report into an error for a single format-2 document:
    /// sorted by span start, span end, code, and message, with exact
    /// duplicates removed (LSF2 §5.6).
    pub(crate) fn into_error_v2(self, text: &str) -> Error {
        let diagnostics = sorted_for_sketch(self.diagnostics, &|_| 0);
        Error::new(diagnostics, text)
    }

    /// The warnings of a single format-2 document, in LSF2 §5.6 order.
    pub(crate) fn into_warnings_v2(self) -> Vec<Diagnostic> {
        sorted_for_sketch(self.diagnostics, &|_| 0)
    }

    /// Converts the report into an error whose spans are positions in `map`,
    /// sorted by (`rank` of the file, span start, span end, code, message)
    /// and with exact duplicates removed (LSF2 §5.6).
    pub(crate) fn into_sketch_error(self, map: &SourceMap, rank: &dyn Fn(Span) -> u32) -> Error {
        let diagnostics = sorted_for_sketch(self.diagnostics, rank);
        Error::in_map(diagnostics, map)
    }

    /// The warnings, in position order (once forging has succeeded).
    pub(crate) fn into_warnings(mut self) -> Vec<Diagnostic> {
        self.sort();
        self.diagnostics
    }

    /// The warnings of a multi-file sketch, in sketch order.
    pub(crate) fn into_sketch_warnings(self, rank: &dyn Fn(Span) -> u32) -> Vec<Diagnostic> {
        sorted_for_sketch(self.diagnostics, rank)
    }
}

/// LSF2 §5.6 ordering, with exact duplicates removed.
fn sorted_for_sketch(
    mut diagnostics: Vec<Diagnostic>,
    rank: &dyn Fn(Span) -> u32,
) -> Vec<Diagnostic> {
    diagnostics.sort_by(|a, b| {
        let (sa, sb) = (a.primary().span(), b.primary().span());
        rank(sa)
            .cmp(&rank(sb))
            .then(sa.start().cmp(&sb.start()))
            .then(sa.end().cmp(&sb.end()))
            .then(a.code().cmp(&b.code()))
            .then(a.message().cmp(b.message()))
    });
    diagnostics.dedup_by(|a, b| {
        a.primary().span() == b.primary().span()
            && a.code() == b.code()
            && a.message() == b.message()
    });
    diagnostics
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use alloc::string::ToString;

    use super::*;
    use crate::codes;

    #[test]
    fn test_line_col_counts_characters_not_bytes() {
        let text = "ab\nçd\n";
        assert_eq!(line_col(text, 0), (1, 1));
        assert_eq!(line_col(text, 3), (2, 1));
        // `d` sits after the two-byte `ç`.
        assert_eq!(line_col(text, 5), (2, 2));
        assert_eq!(line_col(text, 999), (3, 1));
    }

    #[test]
    fn test_line_col_inside_multibyte_char_does_not_panic() {
        assert_eq!(line_col("ç", 1), (1, 1));
    }

    #[test]
    fn test_display_counts_additional_errors() {
        let mut report = Report::default();
        report.error(codes::MISSING, Span::new(4, 5), "second");
        report.error(codes::MISSING, Span::new(0, 1), "first");
        let err = report.into_error("abcdef");
        assert_eq!(err.to_string(), "1:1: first (and 1 more error)");

        let mut report = Report::default();
        for _ in 0..3 {
            report.error(codes::MISSING, Span::new(0, 0), "same");
        }
        assert_eq!(
            report.into_error("").to_string(),
            "1:1: same (and 2 more errors)"
        );
    }

    #[test]
    fn test_report_help_is_attached() {
        let mut report = Report::default();
        report.error_help(codes::MISSING, Span::new(0, 1), "bad", "fix it");
        assert!(!report.is_clean());
        let err = report.into_error("x");
        assert_eq!(err.diagnostics()[0].help().collect::<Vec<_>>(), ["fix it"]);
        assert_eq!(err.diagnostics()[0].code(), Some(codes::MISSING));
    }

    #[test]
    fn test_report_warnings_keep_it_clean() {
        let mut report = Report::default();
        report.warning(codes::UNUSED_RULE, Span::new(0, 1), "unused");
        assert!(report.is_clean());
        assert_eq!(report.errors(), 0);
        let warnings = report.into_warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity(), Severity::Warning);
    }

    #[test]
    fn test_sketch_errors_name_the_file_and_dedup() {
        let mut map = SourceMap::new();
        let _ = map.add("a.lsf", "x\n").expect("fits");
        let _ = map.add("b.lsf", "yy\nzz\n").expect("fits");
        let mut report = Report::default();
        report.error(codes::MISSING, Span::new(6, 7), "late");
        report.error(codes::MISSING, Span::new(6, 7), "late");
        report.error(codes::MISSING, Span::new(1, 2), "early");
        let err = report.into_sketch_error(&map, &|_| 0);
        assert_eq!(err.diagnostics().len(), 2);
        assert_eq!(err.to_string(), "a.lsf:1:2: early (and 1 more error)");
    }
}
