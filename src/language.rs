//! [`Language`]: a language forged from a schematic.

use alloc::{boxed::Box, format, vec::Vec};
use core::str::FromStr;

use diag_lang::{Diagnostic, Label, Severity};
use pass_lang::{Outcome, Pass, PassError, PassManager};
use syntax_lang::{Span, Token};

use crate::{
    Error, Parse,
    error::Report,
    grammar::{self, Grammar},
    kind::Kind,
    noml, parser, schematic,
};

/// A capability pass, boxed for [`Language::pipeline`].
///
/// A capability is a [`pass_lang::Pass`] over a [`Parse`], named by its
/// [`Pass::name`]. Implement the pass for every lifetime —
/// `impl<'a> Pass<Parse<'a>> for MyPass` — and box it as a `Capability`; the
/// language then runs it whenever its schematic includes that name.
///
/// # Examples
///
/// ```
/// use lang_forge::pass_lang::{Outcome, Pass, PassError};
/// use lang_forge::{Capability, Parse};
///
/// struct CountNodes(usize);
///
/// impl<'a> Pass<Parse<'a>> for CountNodes {
///     fn name(&self) -> &'static str {
///         "count-nodes"
///     }
///
///     fn run(&mut self, parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
///         self.0 = parse.tree().descendants().count();
///         Ok(Outcome::Unchanged)
///     }
/// }
///
/// let capability: Capability = Box::new(CountNodes(0));
/// assert_eq!(capability.name(), "count-nodes");
/// ```
pub type Capability = Box<dyn for<'a> Pass<Parse<'a>>>;

/// A language forged from a `.lsf` schematic: a lexer, a parser, and the kinds
/// of its syntax tree.
///
/// Forge one with [`Language::from_lsf`] (or `str::parse`), then call
/// [`parse`](Self::parse) as often as needed. Forging does all the analysis
/// up front — every rule resolved, every set computed, every conflict
/// refused — so parsing is a walk over precomputed tables that never fails and
/// never panics: malformed input yields a complete tree plus diagnostics.
///
/// A `Language` is immutable once forged. It is `Send` and `Sync`, so one
/// language can parse on many threads at once, and `Clone` when a copy is
/// needed.
///
/// # The schematic
///
/// A schematic is a NOML document with up to four tables: `[language]` (the
/// name, and optionally the version, file extensions, and start rule),
/// `[lexer]` (identifier style, significant newlines, comments, strings),
/// `[rules]` (the grammar), and `[capabilities]` (passes the language
/// includes). The full reference is in `docs/API.md`.
///
/// # Examples
///
/// ```
/// use lang_forge::Language;
///
/// let calc = Language::from_lsf(
///     r##"
///     [language]
///     name       = "calc"
///     version    = "1.0.0"
///     extensions = ["calc"]
///
///     [lexer]
///     line_comments = ["#"]
///
///     [rules]
///     program = "stmt*"
///     stmt    = "'let' IDENT '=' expr ';' | expr ';'"
///
///     [rules.expr]
///     operand = "NUMBER | IDENT | '(' expr ')'"
///     levels  = [
///         { left   = ["+", "-"] },
///         { left   = ["*", "/"] },
///         { prefix = ["-"] },
///     ]
///     "##,
/// )?;
///
/// let parse = calc.parse("let x = 2 * (3 + 4); # seven, doubled\n-x;");
/// assert!(!parse.has_errors());
///
/// let stmt = calc.kind("stmt").expect("a rule");
/// assert_eq!(parse.tree().child_nodes().filter(|n| *n.kind() == stmt).count(), 2);
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Language {
    grammar: Grammar,
}

impl Language {
    /// Forges a language from the text of a `.lsf` schematic.
    ///
    /// The schematic is read, checked against the schematic layout, and its
    /// grammar compiled and analysed. Everything wrong with it is reported at
    /// once.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] carrying one diagnostic per problem, with spans
    /// into `schematic`: NOML syntax errors; unknown, missing, or mistyped
    /// settings; malformed rules; undefined rules (with a suggestion);
    /// literals the lexer cannot produce; delimiters used twice; left
    /// recursion; repetitions of something that can match nothing;
    /// alternatives that can never match; and the use of NOML's dynamic
    /// features, which would make the language depend on where it was forged.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let json = Language::from_lsf(
    ///     r#"
    ///     [language]
    ///     name = "json"
    ///
    ///     [lexer]
    ///     strings = ['"']
    ///
    ///     [rules]
    ///     document = "value"
    ///     value    = "object | array | STRING | NUMBER | 'true' | 'false' | 'null'"
    ///     object   = "'{' (member (',' member)*)? '}'"
    ///     member   = "STRING ':' value"
    ///     array    = "'[' (value (',' value)*)? ']'"
    ///     "#,
    /// )?;
    /// assert!(!json.parse(r#"{"a": [1, true, {"b": null}]}"#).has_errors());
    /// assert!(json.parse(r#"{"a": }"#).has_errors());
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    ///
    /// A left-recursive rule is refused, with the fix:
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let err = Language::from_lsf(
    ///     "[language]\nname = \"bad\"\n[rules]\nsum = \"sum '+' NUMBER | NUMBER\"\n",
    /// )
    /// .unwrap_err();
    /// assert_eq!(err.to_string(), "4:1: rule `sum` is left-recursive: sum → sum");
    /// ```
    pub fn from_lsf(schematic: &str) -> Result<Self, Error> {
        let mut report = Report::default();
        let root = match noml::read(schematic) {
            Ok(root) => root,
            Err(diagnostic) => {
                report.diagnostic(diagnostic);
                return Err(report.into_error(schematic));
            }
        };
        let spec = schematic::interpret(root, &mut report);
        let grammar = spec.and_then(|spec| grammar::compile(&spec, schematic, &mut report));
        match grammar {
            Some(grammar) if report.is_clean() => Ok(Self { grammar }),
            _ => {
                if report.is_clean() {
                    report.error(Span::empty(0), "the schematic could not be forged");
                }
                Err(report.into_error(schematic))
            }
        }
    }

    /// The compiled tables, for the crate's own tests.
    #[cfg(test)]
    pub(crate) fn grammar(&self) -> &Grammar {
        &self.grammar
    }

    /// The language's name, from `[language] name`.
    #[inline]
    #[must_use]
    pub fn name(&self) -> &str {
        &self.grammar.name
    }

    /// The language's version, from `[language] version`, if given.
    ///
    /// The text is kept as written; lang-forge does not interpret it.
    #[inline]
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.grammar.version.as_deref()
    }

    /// The file extensions of the language's source files, without the dot,
    /// from `[language] extensions`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"mox\"\nextensions = [\"mox\", \"mx\"]\n[rules]\nfile = \"IDENT*\"\n",
    /// )?;
    /// assert_eq!(lang.extensions().collect::<Vec<_>>(), ["mox", "mx"]);
    /// assert!(lang.extensions().any(|e| e == "mx"));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn extensions(&self) -> impl ExactSizeIterator<Item = &str> {
        self.grammar.extensions.iter().map(|e| &**e)
    }

    /// The capabilities the schematic includes, in the order their passes
    /// run, from `[capabilities] include`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"iron\"\n[rules]\nfile = \"IDENT*\"\n\
    ///      [capabilities]\ninclude = [\"borrow-check\", \"thermal\"]\n",
    /// )?;
    /// assert_eq!(lang.capabilities().collect::<Vec<_>>(), ["borrow-check", "thermal"]);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = &str> {
        self.grammar.capabilities.iter().map(|c| &*c.name)
    }

    /// The kind called `name`, or `None` if the language has no such kind.
    ///
    /// Rule names name the nodes rules build (hidden `_` rules build none);
    /// a keyword or symbol is named by its text; Pratt levels add their node
    /// names (`binary`, `prefix`, `postfix` unless renamed); and every
    /// language has `IDENT`, `NUMBER`, `STRING`, `NEWLINE`, `WHITESPACE`,
    /// `COMMENT`, `UNKNOWN`, and `ERROR`. See [`Kind`] for the full table.
    ///
    /// The lookup is a binary search; look kinds up once and keep them.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"'go' NUMBER\"\n")?;
    /// assert!(lang.kind("item").is_some());
    /// assert!(lang.kind("go").is_some());
    /// assert!(lang.kind("ERROR").is_some());
    /// assert!(lang.kind("missing").is_none());
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn kind(&self, name: &str) -> Option<Kind> {
        self.grammar.kinds.get(name)
    }

    /// The name of `kind`: the inverse of [`kind`](Self::kind).
    ///
    /// A kind is only meaningful to the language that made it. Given a kind
    /// from another language, `kind_name` cannot tell: it returns whatever
    /// name this language has at that kind's position in its kind table —
    /// usually a wrong one — or `"<unknown>"` when this language has fewer
    /// kinds than that.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"'go' NUMBER\"\n")?;
    /// let parse = lang.parse("go 7");
    /// let names: Vec<&str> = parse.tree().tokens().map(|t| lang.kind_name(*t.kind())).collect();
    /// assert_eq!(names, ["go", "WHITESPACE", "NUMBER"]);
    ///
    /// // Another language's kinds get a wrong name, or none.
    /// let other = Language::from_lsf(
    ///     "[language]\nname = \"y\"\n[rules]\nlist = \"'[' (pair (',' pair)*)? ']'\"\npair = \"IDENT ':' NUMBER\"\n",
    /// )?;
    /// assert_eq!(lang.kind_name(other.kind("[").expect("a symbol")), "go");
    /// assert_eq!(lang.kind_name(other.kind("pair").expect("a rule")), "<unknown>");
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn kind_name(&self, kind: Kind) -> &str {
        self.grammar.kinds.name(kind)
    }

    /// Splits `source` into tokens, trivia included.
    ///
    /// The tokens are contiguous and cover the whole source, so this is the
    /// stream a syntax highlighter wants. Characters that begin no token come
    /// back as `UNKNOWN` tokens; [`parse`](Self::parse) reports them, `lex`
    /// does not. A source of 4 GiB or more, which spans cannot address,
    /// yields no tokens.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    /// use lang_forge::syntax_lang::TokenKind;
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"x\"\n[lexer]\nline_comments = [\"--\"]\n[rules]\nfile = \"IDENT*\"\n",
    /// )?;
    /// let tokens = lang.lex("alpha -- note\nbeta");
    /// let significant: Vec<&str> = tokens
    ///     .iter()
    ///     .filter(|t| !t.is_trivia())
    ///     .map(|t| lang.kind_name(*t.kind()))
    ///     .collect();
    /// assert_eq!(significant, ["IDENT", "IDENT"]);
    /// assert_eq!(tokens.len(), 5); // IDENT, WHITESPACE, COMMENT, WHITESPACE, IDENT
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn lex(&self, source: &str) -> Vec<Token<Kind>> {
        let mut tokens = Vec::new();
        if u32::try_from(source.len()).is_ok() {
            let mut diagnostics = Vec::new();
            self.grammar
                .lexer
                .run(source, &mut tokens, &mut diagnostics);
        }
        tokens
    }

    /// Parses `source` into a lossless syntax tree.
    ///
    /// Never fails: problems become diagnostics on the returned [`Parse`] and
    /// the tree is complete regardless, with unexpected tokens wrapped in
    /// `ERROR` nodes. Input nested too deeply to parse is reported rather
    /// than followed: the parser recurses at most 768 grammar levels, which
    /// needs at most about 256 KiB of stack in a release build (768 KiB in a
    /// debug build) and allows well over a hundred levels of nesting in
    /// typical grammars.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"block\"\n[rules]\nblock = \"'{' stmt* '}'\"\nstmt = \"IDENT ';'\"\n",
    /// )?;
    ///
    /// let good = lang.parse("{ a; b; }");
    /// assert!(!good.has_errors());
    ///
    /// // A stray `;` is skipped, the rest still parses.
    /// let bad = lang.parse("{ a; ; b; }");
    /// assert_eq!(bad.diagnostics().len(), 1);
    /// assert_eq!(bad.diagnostics()[0].message(), "expected stmt, found `;`");
    /// let error = lang.kind("ERROR").expect("built in");
    /// assert_eq!(bad.tree().descendants().filter(|n| *n.kind() == error).count(), 1);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn parse<'a>(&'a self, source: &'a str) -> Parse<'a> {
        let (tree, diagnostics) = parser::parse(&self.grammar, source);
        Parse::new(self, source, tree, diagnostics)
    }

    /// Assembles the language's capability pipeline from a registry of passes.
    ///
    /// `passes` may hold passes for many languages; the pipeline takes the
    /// ones whose [`Pass::name`] the schematic's `[capabilities] include`
    /// lists, in that order, and ignores the rest. Run it over each
    /// [`Parse`] with [`PassManager::run`].
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] with a diagnostic, pointing into the schematic,
    /// for every included capability that has no pass in `passes` or more
    /// than one.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::diag_lang::{Diagnostic, Label, Severity};
    /// use lang_forge::pass_lang::{Outcome, Pass, PassError};
    /// use lang_forge::{Capability, Language, Parse};
    ///
    /// /// Warns about every identifier written in capitals.
    /// struct Shouting;
    ///
    /// impl<'a> Pass<Parse<'a>> for Shouting {
    ///     fn name(&self) -> &'static str {
    ///         "no-shouting"
    ///     }
    ///
    ///     fn run(&mut self, parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
    ///         let ident = parse.language().kind("IDENT").ok_or_else(|| PassError::new("no IDENT"))?;
    ///         let loud: Vec<_> = parse
    ///             .tree()
    ///             .tokens()
    ///             .filter(|t| *t.kind() == ident)
    ///             .filter(|t| {
    ///                 let text = &parse.source()[t.span().start().to_usize()..t.span().end().to_usize()];
    ///                 text.len() > 1 && text.chars().all(|c| c.is_ascii_uppercase())
    ///             })
    ///             .map(|t| t.span())
    ///             .collect();
    ///         for span in loud {
    ///             parse.report(Diagnostic::new(Severity::Warning, "no need to shout", Label::unlabelled(span)));
    ///         }
    ///         Ok(Outcome::Unchanged)
    ///     }
    /// }
    ///
    /// let lang = Language::from_lsf(
    ///     "[language]\nname = \"words\"\n[rules]\nfile = \"IDENT*\"\n\
    ///      [capabilities]\ninclude = [\"no-shouting\"]\n",
    /// )?;
    /// let registry: Vec<Capability> = vec![Box::new(Shouting)];
    /// let mut pipeline = lang.pipeline(registry)?;
    ///
    /// let mut parse = lang.parse("quiet LOUD calm");
    /// pipeline.run(&mut parse).expect("the pass succeeds");
    /// assert_eq!(parse.diagnostics().len(), 1);
    /// assert_eq!(parse.diagnostics()[0].message(), "no need to shout");
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn pipeline<'a>(
        &self,
        passes: impl IntoIterator<Item = Capability>,
    ) -> Result<PassManager<Parse<'a>>, Error> {
        let mut available: Vec<Option<Capability>> = passes.into_iter().map(Some).collect();
        let mut manager = PassManager::new();
        let mut problems = Vec::new();
        let mut location = None;
        for capability in self.grammar.capabilities.iter() {
            let mut matching = available
                .iter()
                .enumerate()
                .filter(|(_, p)| p.as_ref().is_some_and(|p| p.name() == &*capability.name))
                .map(|(i, _)| i);
            let (first, second) = (matching.next(), matching.next());
            let problem = match (first, second) {
                (Some(i), None) => {
                    if let Some(pass) = available[i].take() {
                        let _ = manager.add(Plugged(pass));
                    }
                    continue;
                }
                (None, _) => Diagnostic::new(
                    Severity::Error,
                    format!("capability `{}` has no pass", capability.name),
                    Label::unlabelled(capability.span),
                )
                .with_help(format!(
                    "pass a capability whose `name()` is \"{}\"",
                    capability.name
                )),
                (Some(_), Some(_)) => Diagnostic::new(
                    Severity::Error,
                    format!("capability `{}` has more than one pass", capability.name),
                    Label::unlabelled(capability.span),
                ),
            };
            if location.is_none() {
                location = Some((capability.line, capability.column));
            }
            problems.push(problem);
        }
        match location {
            None => Ok(manager),
            Some((line, column)) => Err(Error::located(problems, line, column)),
        }
    }
}

impl FromStr for Language {
    type Err = Error;

    /// Forges a language; the same as [`Language::from_lsf`].
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang: Language = "[language]\nname = \"n\"\n[rules]\nn = \"NUMBER\"\n".parse()?;
    /// assert_eq!(lang.name(), "n");
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    fn from_str(schematic: &str) -> Result<Self, Error> {
        Self::from_lsf(schematic)
    }
}

/// A boxed capability, as the pass manager stores passes.
struct Plugged(Capability);

impl<'a> Pass<Parse<'a>> for Plugged {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn run(&mut self, unit: &mut Parse<'a>) -> Result<Outcome, PassError> {
        self.0.run(unit)
    }
}
