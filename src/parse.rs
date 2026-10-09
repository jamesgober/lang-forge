//! [`Parse`]: the result of parsing source text with a forged language.

use alloc::{string::String, vec::Vec};
use core::fmt::Write as _;

use diag_lang::{Diagnostic, Severity};
use syntax_lang::{Element, Node, Span};

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
    injections: Vec<Injection<'a>>,
}

impl<'a> Parse<'a> {
    pub(crate) fn new(
        language: &'a Language,
        source: &'a str,
        tree: Node<Kind>,
        diagnostics: Vec<Diagnostic>,
        injections: Vec<Injection<'a>>,
    ) -> Self {
        Self {
            language,
            source,
            tree,
            diagnostics,
            injections,
        }
    }

    /// The injections found in the source (format 2), in source order: the
    /// ranges `[injections]` and `embedded` tokens with `parse` name. A
    /// self-injection carries its own [`tree`](Injection::tree), parsed with
    /// this language; an injection of another language, or one the sketch
    /// leaves to the editor, carries only its range.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(r#"
    ///     [sketch]
    ///     format = 2
    ///     [language]
    ///     name = "tpl"
    ///     version = "1.0.0"
    ///     [lexer.strings.QUOTED]
    ///     open = '"'
    ///     embedded = [{ token = "REF", regex = '\$[a-z]+', parse = "ref" }]
    ///     [rules]
    ///     file = "QUOTED*"
    ///     ref = "'$' IDENT"
    ///     "#)?;
    /// let parse = lang.parse(r#""hello $name""#);
    /// let injection = &parse.injections()[0];
    /// assert_eq!(injection.language(), "self");
    /// assert_eq!(&parse.source()[injection.span().start().to_usize()..injection.span().end().to_usize()], "$name");
    /// let tree = injection.tree().expect("a self-injection is parsed");
    /// assert_eq!(*tree.kind(), lang.kind("ref").expect("a rule"));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn injections(&self) -> &[Injection<'a>] {
        &self.injections
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
            // A field label (format 2) prefixes the element it labels.
            if let Some(label) = kind.label().and_then(|l| self.language.label_name(l)) {
                out.push_str(label);
                out.push(':');
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

/// A range of a parsed source covered by an injection (LSF2 §12): another
/// language's content, or a token of this language whose text has structure
/// of its own. See [`Parse::injections`].
///
/// # Examples
///
/// ```
/// use lang_forge::Language;
///
/// // `$name.field` inside a string is parsed by the `path` rule.
/// let lang = Language::from_lsf(
///     "[sketch]\nformat = 2\n[language]\nname = \"s\"\nversion = \"1.0.0\"\n\
///      [lexer.strings.DQ]\nopen = '\"'\nembedded = [{ token = \"DQ_VAR\", regex = '\\$[a-z]+(\\.[a-z]+)*', parse = \"path\" }]\n\
///      interpolate = [{ open = \"{\", close = \"}\", rule = \"path\" }]\n\
///      [lexer.tokens]\nVAR = { regex = '\\$[a-z]+' }\n\
///      [rules]\nfile = \"DQ*\"\npath = \"VAR ('.' IDENT)*\"\n",
/// )?;
/// let src = "\"hello $user.name\"";
/// let parse = lang.parse(src);
/// let injection = &parse.injections()[0];
/// assert_eq!((injection.id(), injection.language(), injection.is_editor()), ("DQ_VAR", "self", false));
/// assert_eq!(injection.span().start().to_usize(), 7);
/// let tree = injection.tree().expect("a self-injection is parsed");
/// assert_eq!(lang.kind_name(*tree.kind()), "path");
/// assert_eq!(tree.text(src), Some("$user.name"));
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Injection<'a> {
    id: &'a str,
    language: &'a str,
    span: Span,
    tree: Option<Node<Kind>>,
    editor: bool,
}

impl<'a> Injection<'a> {
    pub(crate) fn new(
        id: &'a str,
        language: &'a str,
        span: Span,
        tree: Option<Node<Kind>>,
        editor: bool,
    ) -> Self {
        Self {
            id,
            language,
            span,
            tree,
            editor,
        }
    }

    /// The injection's id from `[injections]`, or the token class's name for
    /// an `embedded` token with `parse`.
    #[must_use]
    pub fn id(&self) -> &'a str {
        self.id
    }

    /// The injected language: `"self"`, or the name the sketch gives.
    #[must_use]
    pub fn language(&self) -> &'a str {
        self.language
    }

    /// The range of the source the injection covers.
    #[must_use]
    pub fn span(&self) -> Span {
        self.span
    }

    /// The injected tree, for a self-injection; `None` for another language
    /// or an editor-resolved injection (and for one nested more than 16
    /// levels deep, which a warning reports).
    #[must_use]
    pub fn tree(&self) -> Option<&Node<Kind>> {
        self.tree.as_ref()
    }

    /// Whether the sketch leaves this injection to editors
    /// (`resolve = "editor"`).
    #[must_use]
    pub fn is_editor(&self) -> bool {
        self.editor
    }
}
