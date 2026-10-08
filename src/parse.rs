//! [`Parse`]: the result of parsing source text with a forged language.

use alloc::{string::String, vec::Vec};
use core::fmt::Write as _;

use diag_lang::{Diagnostic, Severity};
use syntax_lang::{Element, Node};

use crate::{Language, kind::Kind};

/// The result of [`Language::parse`]: a lossless syntax tree and the problems
/// found while building it.
///
/// Parsing never fails. Malformed input still yields a complete tree — missing
/// pieces are left out, unexpected tokens are wrapped in `ERROR` nodes — and a
/// [`Diagnostic`] for each problem, so an editor can highlight and navigate a
/// half-typed file and a compiler can report every syntax error in one pass.
/// The tree is lossless either way: it covers every byte of the source,
/// whitespace and comments included.
///
/// A `Parse` borrows the language and the source it was made from, so names,
/// text, and diagnostics can be resolved from it alone. It is also the unit
/// that capability passes run over; see [`Language::pipeline`].
///
/// # Examples
///
/// ```
/// use lang_forge::Language;
///
/// let lang = Language::from_lsf(
///     r#"
///     [language]
///     name = "pairs"
///
///     [rules]
///     file = "pair*"
///     pair = "IDENT '=' NUMBER ';'"
///     "#,
/// )?;
///
/// let parse = lang.parse("a = 1;\nb = ;\n");
/// assert!(parse.has_errors());
/// assert_eq!(parse.diagnostics()[0].message(), "expected a number, found `;`");
///
/// // Still a full tree, covering the whole source.
/// assert_eq!(parse.tree().text(parse.source()), Some("a = 1;\nb = ;\n"));
/// assert_eq!(parse.tree().child_nodes().count(), 2);
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Parse<'a> {
    language: &'a Language,
    source: &'a str,
    tree: Node<Kind>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Parse<'a> {
    pub(crate) fn new(
        language: &'a Language,
        source: &'a str,
        tree: Node<Kind>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            language,
            source,
            tree,
            diagnostics,
        }
    }

    /// The root of the syntax tree: a node of the start rule's kind that
    /// covers the whole source.
    ///
    /// Walk it with the [`syntax_lang::Node`] API — [`children`](Node::children),
    /// [`descendants`](Node::descendants), [`tokens`](Node::tokens) — and slice
    /// the source with [`Node::text`]. Every walk is iterative, so even very
    /// deep trees are safe to traverse.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"list\"\n[rules]\nlist = \"'[' (NUMBER (',' NUMBER)*)? ']'\"\n",
    /// )?;
    /// let parse = lang.parse("[1, 2, 3]");
    /// let number = lang.kind("NUMBER").expect("built in");
    /// let numbers: Vec<&str> = parse
    ///     .tree()
    ///     .tokens()
    ///     .filter(|t| *t.kind() == number)
    ///     .map(|t| &parse.source()[t.span().start().to_usize()..t.span().end().to_usize()])
    ///     .collect();
    /// assert_eq!(numbers, ["1", "2", "3"]);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn tree(&self) -> &Node<Kind> {
        &self.tree
    }

    /// The source text that was parsed. Spans in the tree and the
    /// diagnostics are byte offsets into it.
    #[inline]
    #[must_use]
    pub fn source(&self) -> &'a str {
        self.source
    }

    /// The language that parsed the source, for looking kinds up and naming
    /// them.
    #[inline]
    #[must_use]
    pub fn language(&self) -> &'a Language {
        self.language
    }

    /// The problems found, in source order: lexical errors (unexpected
    /// characters, unterminated strings and comments, malformed numbers),
    /// syntax errors, and anything capability passes have [`report`]ed.
    ///
    /// Spans are byte offsets into [`source`](Self::source). Add the source to
    /// a fresh [`SourceMap`](diag_lang::SourceMap) and
    /// [`Renderer`](diag_lang::Renderer) draws each one under the line at
    /// fault.
    ///
    /// [`report`]: Self::report
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    /// use lang_forge::diag_lang::{Renderer, SourceMap};
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"calls\"\n[rules]\nfile = \"call*\"\ncall = \"IDENT '(' ')' ';'\"\n",
    /// )?;
    /// let source = "start();\nstop(;\n";
    /// let parse = lang.parse(source);
    ///
    /// let mut map = SourceMap::new();
    /// map.add("main.calls", source).expect("fits");
    /// let report = Renderer::new().render(&parse.diagnostics()[0], &map);
    /// assert!(report.contains("expected `)`, found `;`"));
    /// assert!(report.contains("main.calls:2:6"));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether any diagnostic is an error (rather than a warning or a note a
    /// capability pass added).
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity() == Severity::Error)
    }

    /// Adds a diagnostic — the way a capability pass reports what it finds.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    /// use lang_forge::diag_lang::{Diagnostic, Label, Severity};
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"n\"\n[rules]\nn = \"NUMBER\"\n")?;
    /// let mut parse = lang.parse("42");
    /// let span = parse.tree().span();
    /// parse.report(Diagnostic::new(Severity::Warning, "the answer", Label::unlabelled(span)));
    ///
    /// assert_eq!(parse.diagnostics().len(), 1);
    /// assert!(!parse.has_errors());
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn report(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Takes the tree, dropping the diagnostics and the borrows.
    #[inline]
    #[must_use]
    pub fn into_tree(self) -> Node<Kind> {
        self.tree
    }

    /// The tree as indented text, one node or token per line: each line is a
    /// kind name and a byte range, and each token also shows its text.
    ///
    /// Meant for tests, snapshots, and debugging a grammar. The format is
    /// stable within a major version.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"let\"\n[rules]\nstmt = \"'let' IDENT '=' NUMBER\"\n",
    /// )?;
    /// assert_eq!(
    ///     lang.parse("let x = 1").dump(),
    ///     "stmt@0..9\n  \
    ///        let@0..3 \"let\"\n  \
    ///        WHITESPACE@3..4 \" \"\n  \
    ///        IDENT@4..5 \"x\"\n  \
    ///        WHITESPACE@5..6 \" \"\n  \
    ///        =@6..7 \"=\"\n  \
    ///        WHITESPACE@7..8 \" \"\n  \
    ///        NUMBER@8..9 \"1\"\n",
    /// );
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let mut stack: Vec<(&Element<Kind>, usize)> = Vec::new();
        let line = |out: &mut String, depth: usize, kind: Kind, start: u32, end: u32| {
            for _ in 0..depth {
                out.push_str("  ");
            }
            // Writing to a `String` cannot fail.
            let _ = write!(out, "{}@{start}..{end}", self.language.kind_name(kind));
        };
        let span = self.tree.span();
        line(
            &mut out,
            0,
            *self.tree.kind(),
            span.start().to_u32(),
            span.end().to_u32(),
        );
        out.push('\n');
        stack.extend(self.tree.children().map(|c| (c, 1)));
        stack.reverse();
        while let Some((element, depth)) = stack.pop() {
            let span = element.span();
            line(
                &mut out,
                depth,
                *element.kind(),
                span.start().to_u32(),
                span.end().to_u32(),
            );
            match element {
                Element::Node(node) => {
                    let from = stack.len();
                    stack.extend(node.children().map(|c| (c, depth + 1)));
                    stack[from..].reverse();
                }
                Element::Token(_) => {
                    let text = &self.source[span.start().to_usize()..span.end().to_usize()];
                    // Writing to a `String` cannot fail.
                    let _ = write!(out, " {text:?}");
                }
            }
            out.push('\n');
        }
        out
    }
}
