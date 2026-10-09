//! [`Language`]: a language forged from a schematic.

use alloc::{boxed::Box, format, vec::Vec};
use core::str::FromStr;

use diag_lang::{Diagnostic, Label, Severity};
use pass_lang::{Outcome, Pass, PassError, PassManager};
use syntax_lang::{Node, Span, Token};

use crate::{
    Error, Parse, codes,
    error::Report,
    grammar::{self, Grammar, Lex},
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
/// # The sketch
///
/// A sketch (also called a schematic) is a NOML document. A format-1 sketch —
/// the format of lang-forge 1.x, read exactly as 1.x read it — has up to four
/// tables: `[language]` (the name, and optionally the version, file
/// extensions, and start rule), `[lexer]` (identifier style, significant
/// newlines, comments, strings), `[rules]` (the grammar), and
/// `[capabilities]` (passes the language includes). A sketch that begins
/// with `[sketch] format = 2` is read as LSF2, which adds token classes,
/// lexer modes, string classes with interpolation and counted delimiters,
/// keyword policies, layout, field labels, predicates, `[ast]`, and
/// `[injections]`. The full reference is in `docs/API.md`; a sketch split
/// over several files is forged from a [`Sketch`](crate::Sketch).
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
                report.diagnostic(*diagnostic);
                return Err(report.into_error(schematic));
            }
        };
        let modules = crate::spec2::modules(&root);
        if let Some((_, span)) = modules.first() {
            report.error_help(
                codes::MODULE_NOT_FOUND,
                *span,
                "this sketch lists `modules`, which `Language::from_lsf` cannot load",
                "forge it from a `Sketch` holding the entry and every module",
            );
            return Err(report.into_error_v2(schematic));
        }
        let spec = schematic::interpret(root, &mut report);
        let format = spec.as_ref().map_or(1, |s| s.format);
        if let Some(role) = spec.as_ref().and_then(|s| s.v2.as_ref()).map(|v| v.role) {
            if role != crate::spec2::Role::Language {
                report.error(
                    codes::FILE_ROLE,
                    Span::empty(0),
                    "a part or mixin cannot be forged on its own; forge the entry that lists it",
                );
            }
        }
        let grammar = spec.and_then(|spec| {
            if spec.format == 2 {
                let locate =
                    |span: Span| crate::error::line_col(schematic, span.start().to_usize());
                crate::grammar2::compile(&spec, &locate, &mut report)
            } else {
                grammar::compile(&spec, schematic, &mut report)
            }
        });
        Self::finish(grammar, report, format, |report| {
            if format == 2 {
                (report.into_error_v2(schematic), Vec::new())
            } else {
                (report.into_error(schematic), Vec::new())
            }
        })
    }

    /// Wraps a compiled grammar, or turns the report into the error.
    pub(crate) fn finish(
        grammar: Option<Grammar>,
        mut report: Report,
        format: u8,
        fail: impl FnOnce(Report) -> (Error, Vec<Diagnostic>),
    ) -> Result<Self, Error> {
        match grammar {
            Some(mut grammar) if report.is_clean() => {
                if let Some(extra) = grammar.extra.as_mut() {
                    let warnings = if format == 2 {
                        report.into_warnings_v2()
                    } else {
                        report.into_warnings()
                    };
                    extra.warnings = warnings.into();
                }
                Ok(Self { grammar })
            }
            _ => {
                if report.is_clean() {
                    report.error(
                        codes::MISSING,
                        Span::empty(0),
                        "the schematic could not be forged",
                    );
                }
                Err(fail(report).0)
            }
        }
    }

    /// Wraps compiled tables (the language image).
    pub(crate) fn from_grammar(grammar: Grammar) -> Self {
        Self { grammar }
    }

    /// The compiled tables, for the image writer.
    pub(crate) fn tables(&self) -> &Grammar {
        &self.grammar
    }

    /// The compiled tables, for the crate's own tests.
    #[cfg(test)]
    pub(crate) fn grammar(&self) -> &Grammar {
        &self.grammar
    }

    /// The sketch format the language was forged from: `1` or `2`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let v1 = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nx = \"IDENT\"\n")?;
    /// assert_eq!(v1.format(), 1);
    /// let v2 = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n[rules]\nx = \"IDENT\"\n",
    /// )?;
    /// assert_eq!(v2.format(), 2);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn format(&self) -> u8 {
        if self.grammar.extra.is_some() { 2 } else { 1 }
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
        let kinds = &self.grammar.kinds;
        if self.grammar.extra.is_some() {
            // Format 2 reference syntax (LSF2 §6.3): `kind:x` is the node
            // `x`, `'x'` the literal `x`; a plain name shared by a keyword
            // and an operator node names the keyword.
            if let Some(node) = name.strip_prefix("kind:") {
                return kinds
                    .all_named(node)
                    .find(|k| kinds.cats[k.slot()] == grammar::CAT_NODE);
            }
            if let Some(literal) = name
                .strip_prefix('\'')
                .and_then(|n| n.strip_suffix('\''))
                .filter(|n| !n.is_empty())
            {
                return kinds
                    .all_named(literal)
                    .find(|k| kinds.cats[k.slot()] == grammar::CAT_LITERAL);
            }
        }
        kinds.get(name)
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

    /// How many kinds the language has: valid [`Kind::index`] values are
    /// `0..kind_count()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"'go' NUMBER\"\n")?;
    /// let all: Vec<&str> = (0..lang.kind_count() as u16)
    ///     .filter_map(|i| lang.kind_at(i))
    ///     .map(|k| lang.kind_name(k))
    ///     .collect();
    /// assert!(all.contains(&"go") && all.contains(&"item") && all.contains(&"ERROR"));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn kind_count(&self) -> usize {
        self.grammar.kinds.len()
    }

    /// The kind with index `index` (see [`Kind::index`]), or `None` if the
    /// language has no such kind (ISSUES M04).
    ///
    /// The kind comes back with its trivia flag set as this language sets it,
    /// so it compares equal to the kinds in this language's trees. The index
    /// of the internal end-of-input marker has no kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"NUMBER\"\n")?;
    /// let space = lang.kind("WHITESPACE").expect("built in");
    /// assert_eq!(lang.kind_at(space.index()), Some(space));
    /// assert_eq!(lang.kind_at(u16::MAX), None);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn kind_at(&self, index: u16) -> Option<Kind> {
        let kinds = &self.grammar.kinds;
        let i = usize::from(index);
        (i < kinds.len() && kinds.cats[i] != grammar::CAT_EOF).then(|| kinds.at(i))
    }

    /// The kind of every tree's root: the start rule's node.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nfile = \"NUMBER*\"\n")?;
    /// assert_eq!(lang.root_kind(), lang.kind("file").expect("a rule"));
    /// assert_eq!(*lang.parse("1 2").tree().kind(), lang.root_kind());
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn root_kind(&self) -> Kind {
        let program = &self.grammar.program;
        program.rules[program.start as usize]
            .node
            .unwrap_or(program.error)
    }

    /// The name of field label `label` (format 2), or `None` if the
    /// language has no such label.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n\
    ///      [rules]\nlet_stmt = \"'let' name:IDENT '=' value:NUMBER\"\n",
    /// )?;
    /// let name = lang.label_id("name").expect("a label");
    /// assert_eq!(lang.label_name(name), Some("name"));
    /// assert_eq!(lang.label_id("missing"), None);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn label_name(&self, label: u16) -> Option<&str> {
        let extra = self.grammar.extra.as_ref()?;
        extra.labels.get(usize::from(label)).map(|l| &**l)
    }

    /// The id of the field label called `name`, the number lower-lang's
    /// `Pick::Label` takes. Labels are numbered by first occurrence over the
    /// rules in order (LSF2 §5.4).
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n\
    ///      [rules]\nassign = \"target:IDENT '=' value:NUMBER\"\n",
    /// )?;
    /// assert_eq!(lang.label_id("target"), Some(0));
    /// assert_eq!(lang.label_id("value"), Some(1));
    /// assert_eq!(lang.label_id("missing"), None);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn label_id(&self, name: &str) -> Option<u16> {
        let extra = self.grammar.extra.as_ref()?;
        extra
            .labels
            .iter()
            .position(|l| &**l == name)
            .map(|i| i as u16)
    }

    /// The field label of `parent`'s child at `index` (counting every child,
    /// trivia included, as [`Node::children`] yields them), or `None` for an
    /// unlabelled child or an index out of range.
    ///
    /// This has the shape of lower-lang's `Labeler::label`, so the adapter
    /// that hands a format-2 tree's labels to `Lowerer::with_labeler` is one
    /// line: `fn label(&self, p: &Node<Kind>, i: usize) -> Option<u16> {
    /// self.0.field_label(p, i) }`. Labels live on the tree itself (each
    /// child's kind carries the label of its edge), so this works on any tree
    /// the language built, cloned or not.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"w\"\nversion = \"1.0.0\"\n\
    ///      [rules]\nwhile_stmt = \"'while' cond:IDENT body:block\"\nblock = \"'{' '}'\"\n",
    /// )?;
    /// let parse = lang.parse("while ready { }");
    /// let root = parse.tree();
    /// let names: Vec<Option<&str>> = (0..root.len())
    ///     .map(|i| lang.field_label(root, i).and_then(|l| lang.label_name(l)))
    ///     .collect();
    /// assert_eq!(names, [None, None, Some("cond"), None, Some("body")]);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn field_label(&self, parent: &Node<Kind>, index: usize) -> Option<u16> {
        parent.children().nth(index).and_then(|c| c.kind().label())
    }

    /// The fields of node kind `kind` (format 2): every label its children
    /// can carry, with the kinds the field can hold and its cardinality,
    /// derived from the grammar (LSF2 §11.3). Empty for a kind with no
    /// labelled children, and for every kind of a format-1 language.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::{Cardinality, Language};
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"c\"\nversion = \"1.0.0\"\n\
    ///      [rules]\ncall = \"callee:IDENT '(' (args:NUMBER (',' args:NUMBER)*)? ')' tail:';'?\"\n",
    /// )?;
    /// let call = lang.kind("call").expect("a rule");
    /// let fields: Vec<(&str, Cardinality)> =
    ///     lang.fields(call).map(|f| (f.name(), f.cardinality())).collect();
    /// assert_eq!(
    ///     fields,
    ///     [("callee", Cardinality::One), ("args", Cardinality::Many), ("tail", Cardinality::Optional)]
    /// );
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn fields(&self, kind: Kind) -> impl Iterator<Item = crate::Field<'_>> {
        let defs: &[grammar::FieldDef] = self
            .grammar
            .extra
            .as_ref()
            .and_then(|e| {
                e.fields
                    .binary_search_by_key(&kind.index(), |(k, _)| *k)
                    .ok()
                    .map(|at| &*e.fields[at].1)
            })
            .unwrap_or(&[]);
        defs.iter().map(move |d| crate::Field::new(self, d))
    }

    /// The members of `[ast]` supertype `name` (format 2), with supertypes
    /// of supertypes expanded, or `None` if there is no such supertype.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"s\"\nversion = \"1.0.0\"\n\
    ///      [rules]\nfile = \"(num | word)*\"\nnum = \"NUMBER\"\nword = \"IDENT\"\n\
    ///      [ast]\nAtom = [\"num\", \"word\"]\n",
    /// )?;
    /// let atoms: Vec<&str> = lang.supertype("Atom").expect("declared").map(|k| lang.kind_name(k)).collect();
    /// assert_eq!(atoms, ["num", "word"]);
    /// assert_eq!(lang.supertypes().collect::<Vec<_>>(), ["Atom"]);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn supertype(&self, name: &str) -> Option<impl ExactSizeIterator<Item = Kind> + '_> {
        let extra = self.grammar.extra.as_ref()?;
        let (_, members) = extra.supertypes.iter().find(|(n, _)| &**n == name)?;
        Some(
            members
                .iter()
                .map(|&i| self.grammar.kinds.at(usize::from(i))),
        )
    }

    /// The names of the `[ast]` supertypes, in sketch order.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"s\"\nversion = \"1.0.0\"\n\
    ///      [rules]\nfile = \"(num | word)*\"\nnum = \"NUMBER\"\nword = \"IDENT\"\n\
    ///      [ast]\nLiteral = [\"num\"]\nAtom = [\"Literal\", \"word\"]\n",
    /// )?;
    /// assert_eq!(lang.supertypes().collect::<Vec<_>>(), ["Literal", "Atom"]);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn supertypes(&self) -> impl Iterator<Item = &str> {
        self.grammar
            .extra
            .iter()
            .flat_map(|e| e.supertypes.iter().map(|(n, _)| &**n))
    }

    /// Warnings found while forging (format 2): checks set to `warn`, such
    /// as unused rules (`LSF4302`) and unused token classes (`LSF3401`), and
    /// keys LSF2 specifies that this release does not check. Spans are into
    /// the sketch, as for [`Error`].
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"w\"\nversion = \"1.0.0\"\n\
    ///      [rules]\nfile = \"IDENT*\"\nforgotten = \"NUMBER\"\n",
    /// )?;
    /// let warning = &lang.warnings()[0];
    /// assert_eq!(warning.code().map(|c| c.to_string()).as_deref(), Some("LSF4302"));
    /// assert_eq!(warning.message(), "rule `forgotten` is never used");
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn warnings(&self) -> &[Diagnostic] {
        self.grammar.extra.as_ref().map_or(&[], |e| &e.warnings)
    }

    /// The language's display name, from `[language] display_name`
    /// (format 2), or its [`name`](Self::name).
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"mox\"\nversion = \"0.1.0\"\n\
    ///      display_name = \"Mox\"\ndescription = \"A modern PHP.\"\nedition = \"2026\"\n\
    ///      [rules]\nfile = \"IDENT*\"\n",
    /// )?;
    /// assert_eq!(lang.display_name(), "Mox");
    /// assert_eq!(lang.description(), Some("A modern PHP."));
    /// assert_eq!(lang.edition(), Some("2026"));
    ///
    /// let plain = Language::from_lsf("[language]\nname = \"p\"\n[rules]\nfile = \"IDENT*\"\n")?;
    /// assert_eq!((plain.display_name(), plain.description(), plain.edition()), ("p", None, None));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.grammar
            .extra
            .as_ref()
            .and_then(|e| e.display_name.as_deref())
            .unwrap_or(&self.grammar.name)
    }

    /// `[language] description` (format 2), if given (see
    /// [`display_name`](Self::display_name) for an example).
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.grammar.extra.as_ref()?.description.as_deref()
    }

    /// `[language] edition` (format 2), if given (see
    /// [`display_name`](Self::display_name) for an example).
    #[must_use]
    pub fn edition(&self) -> Option<&str> {
        self.grammar.extra.as_ref()?.edition.as_deref()
    }

    /// `[language] shebang_names` (format 2): the interpreter names that
    /// identify the language in a `#!` line, for editors.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"m\"\nversion = \"0.1.0\"\n\
    ///      shebang_names = [\"m\", \"mscript\"]\n[rules]\nfile = \"IDENT*\"\n",
    /// )?;
    /// assert_eq!(lang.shebang_names().collect::<Vec<_>>(), ["m", "mscript"]);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn shebang_names(&self) -> impl Iterator<Item = &str> {
        self.grammar
            .extra
            .iter()
            .flat_map(|e| e.shebang_names.iter().map(|s| &**s))
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
        crate::inject::finish(self, source, tree, diagnostics)
    }

    /// Parses `source` as a file with `extension`: with the lexer mode and
    /// start rule `[language] files` gives that extension (format 2), or as
    /// [`parse`](Self::parse) does when it gives none.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n\
    ///      extensions = [\"x\", \"xs\"]\nfiles = { xs = { start = \"item\" } }\n\
    ///      [rules]\nfile = \"item*\"\nitem = \"NUMBER\"\n",
    /// )?;
    /// assert_eq!(lang.parse_file("xs", "7").tree().kind(), &lang.kind("item").expect("a rule"));
    /// assert_eq!(lang.parse_file("x", "7 8").tree().kind(), &lang.kind("file").expect("a rule"));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn parse_file<'a>(&'a self, extension: &str, source: &'a str) -> Parse<'a> {
        let entry = self.grammar.extra.as_ref().and_then(|e| {
            e.files
                .iter()
                .find(|(ext, _, _)| &**ext == extension)
                .map(|(_, mode, start)| (*mode, *start))
        });
        let (Some((mode, start)), Lex::V2(scanner)) = (entry, &self.grammar.lexer) else {
            return self.parse(source);
        };
        if u32::try_from(source.len()).is_err() {
            return self.parse(source);
        }
        let mut tokens = Vec::new();
        let mut diagnostics = Vec::new();
        scanner.run_range(source, 0, source.len(), mode, &mut tokens, &mut diagnostics);
        let start = if start == u32::MAX {
            self.grammar.program.start
        } else {
            start
        };
        let (tree, diagnostics) =
            parser::parse_tokens(&self.grammar, source, &tokens, start, diagnostics);
        crate::inject::finish(self, source, tree, diagnostics)
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
                ))
                .with_code(codes::CAPABILITY_MISSING),
                (Some(_), Some(_)) => Diagnostic::new(
                    Severity::Error,
                    format!("capability `{}` has more than one pass", capability.name),
                    Label::unlabelled(capability.span),
                )
                .with_code(codes::CAPABILITY_AMBIGUOUS),
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
