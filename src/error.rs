//! [`Error`]: why a schematic could not be forged, or a pipeline assembled.

use alloc::{boxed::Box, vec::Vec};
use core::fmt;

use diag_lang::{Diagnostic, Label, Severity};
use syntax_lang::Span;

/// Why a schematic could not be forged into a [`Language`](crate::Language),
/// or a capability pipeline could not be assembled.
///
/// Forging checks the whole schematic and reports every problem it finds, not
/// just the first, so one round of fixes is usually enough. Each problem is a
/// [`Diagnostic`] whose label points into the schematic text: byte offsets
/// counted from the start of the string handed to
/// [`Language::from_lsf`](crate::Language::from_lsf). Add that text to a fresh
/// [`SourceMap`](diag_lang::SourceMap) and the spans line up, so
/// [`Renderer`](diag_lang::Renderer) can draw each problem under the line at
/// fault.
///
/// `Display` prints the first problem as `line:column: message`, which is
/// enough for a log line or a test failure; render the diagnostics for the
/// full picture.
///
/// # What to do with one
///
/// An `Error` always means the schematic needs editing: forging is
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
        }
    }

    /// An error whose first diagnostic is already located.
    pub(crate) fn located(diagnostics: Vec<Diagnostic>, line: u32, column: u32) -> Self {
        Self {
            diagnostics,
            line,
            column,
        }
    }

    /// Every problem found, in the order they appear in the schematic.
    ///
    /// Never empty. Spans are byte offsets into the schematic text.
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

/// Collects the problems found while forging.
#[derive(Debug, Default)]
pub(crate) struct Report {
    diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// Records an error at `span`.
    pub(crate) fn error(&mut self, span: Span, message: impl Into<Box<str>>) {
        self.diagnostics.push(Diagnostic::new(
            Severity::Error,
            message,
            Label::unlabelled(span),
        ));
    }

    /// Records an error at `span` with a `help` line.
    pub(crate) fn error_help(
        &mut self,
        span: Span,
        message: impl Into<Box<str>>,
        help: impl Into<Box<str>>,
    ) {
        self.diagnostics.push(
            Diagnostic::new(Severity::Error, message, Label::unlabelled(span)).with_help(help),
        );
    }

    /// Records a fully built diagnostic.
    pub(crate) fn diagnostic(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Whether nothing has been recorded.
    pub(crate) fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Converts the report into an error, ordering problems by position.
    pub(crate) fn into_error(mut self, text: &str) -> Error {
        self.diagnostics
            .sort_by_key(|d| d.primary().span().start().to_u32());
        Error::new(self.diagnostics, text)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

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
        report.error(Span::new(4, 5), "second");
        report.error(Span::new(0, 1), "first");
        let err = report.into_error("abcdef");
        assert_eq!(err.to_string(), "1:1: first (and 1 more error)");

        let mut report = Report::default();
        for _ in 0..3 {
            report.error(Span::new(0, 0), "same");
        }
        assert_eq!(
            report.into_error("").to_string(),
            "1:1: same (and 2 more errors)"
        );
    }

    #[test]
    fn test_report_help_is_attached() {
        let mut report = Report::default();
        report.error_help(Span::new(0, 1), "bad", "fix it");
        assert!(!report.is_clean());
        let err = report.into_error("x");
        assert_eq!(err.diagnostics()[0].help().collect::<Vec<_>>(), ["fix it"]);
    }
}
