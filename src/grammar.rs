//! The grammar compiler: a checked schematic in, the tables a [`Language`]
//! runs on out.
//!
//! Rules are lowered into one flat arena of expressions (children always sit
//! before their parents), alternatives that begin alike are left-factored, and
//! the classic FIRST, nullable, and FOLLOW sets are computed to a fixpoint.
//! Those sets drive everything the parser decides at run time: which
//! alternatives are worth trying for the token at hand, whether a repetition
//! continues, and where error recovery may stop skipping. Grammars the parser
//! could not run — left recursion, repetitions of something that matches
//! nothing, alternatives that can never be reached — are refused here, with
//! the rule and the fix named.
//!
//! [`Language`]: crate::Language

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use diag_lang::Diagnostic;
use syntax_lang::Span;

use crate::{
    codes,
    error::{Report, line_col},
    kind::{Kind, MAX_KINDS, MAX_LABELS},
    lexer::{self, BUILTIN_TOKENS, FIRST_LITERAL, Lexer},
    rule::{self, Ast},
    scan::Scanner,
    schematic::{Body, Fixity, RuleSpec, Schematic},
    set::{NO_SET, SetId, Sets},
};

/// How many rounds of left-factoring may nest. Factoring only speeds parsing
/// up, so stopping early is safe; the bound keeps hostile grammars from
/// recursing without limit.
const MAX_FACTOR_DEPTH: u32 = 64;

/// The most precedence levels one expression rule may have.
const MAX_LEVELS: usize = 255;

/// The most memory the forged tables whose size grows with the number of
/// expressions times the number of token kinds may take: the token sets
/// (FIRST, FOLLOW, and recovery sets are bitsets over every token kind) and
/// the operator tables of expression rules. Real grammars need well under a
/// megabyte; one with 30,000 distinct keywords about 120 MB. A schematic
/// that would need more is refused before anything that size is allocated.
pub(crate) const MAX_TABLE_BYTES: usize = 256 << 20;

/// Names that belong to built-in kinds and cannot name a rule or node.
const RESERVED: [&str; 9] = [
    "UNKNOWN",
    "WHITESPACE",
    "COMMENT",
    "NEWLINE",
    "IDENT",
    "NUMBER",
    "STRING",
    "ERROR",
    "EOF",
];

/// The kind categories of the kind table.
pub(crate) const CAT_BUILTIN: u8 = 0;
pub(crate) const CAT_CLASS: u8 = 1;
pub(crate) const CAT_LITERAL: u8 = 2;
pub(crate) const CAT_NODE: u8 = 3;
pub(crate) const CAT_EOF: u8 = 4;

/// A forged language: identity, kinds, lexer, and parser tables.
#[derive(Clone, Debug)]
pub(crate) struct Grammar {
    pub(crate) name: Box<str>,
    pub(crate) version: Option<Box<str>>,
    pub(crate) extensions: Box<[Box<str>]>,
    pub(crate) capabilities: Box<[CapabilityRef]>,
    pub(crate) kinds: Kinds,
    pub(crate) lexer: Lex,
    pub(crate) program: Program,
    /// What a format-2 sketch adds; `None` for format 1.
    pub(crate) extra: Option<Box<Extra>>,
}

/// The lexer of a forged language.
#[derive(Clone, Debug)]
pub(crate) enum Lex {
    /// Format 1: lang-forge 1.x's derived lexer, unchanged (boxed, as the
    /// scanner is, so a `Lex` is one pointer either way).
    V1(Box<Lexer>),
    /// Format 2: the mode-stack scanner.
    V2(Box<Scanner>),
}

impl Lex {
    /// Scans `src` into `tokens`, reporting malformed input to `diags`.
    pub(crate) fn run(
        &self,
        src: &str,
        tokens: &mut Vec<syntax_lang::Token<Kind>>,
        diags: &mut Vec<Diagnostic>,
    ) {
        match self {
            Lex::V1(lexer) => lexer.run(src, tokens, diags),
            Lex::V2(scanner) => scanner.run(src, tokens, diags),
        }
    }
}

/// Fields by node kind index: each kind's field definitions.
pub(crate) type FieldTable = Box<[(u16, Box<[FieldDef]>)]>;

/// `[ast]` supertypes: each name with its member kind indexes.
pub(crate) type Supertypes = Box<[(Box<str>, Box<[u16]>)]>;

/// The format-2 parts of a forged language.
#[derive(Clone, Debug, Default)]
pub(crate) struct Extra {
    pub(crate) display_name: Option<Box<str>>,
    pub(crate) description: Option<Box<str>>,
    pub(crate) edition: Option<Box<str>>,
    pub(crate) shebang_names: Box<[Box<str>]>,
    /// Field label names, by id.
    pub(crate) labels: Box<[Box<str>]>,
    /// Fields by node kind index (sorted by kind index).
    pub(crate) fields: FieldTable,
    /// `[ast]` supertypes: name and member kind indexes (supertypes expanded).
    pub(crate) supertypes: Supertypes,
    /// `[injections]`.
    pub(crate) injections: Box<[InjectionDef]>,
    /// `[language] files`: extension, initial mode, start rule.
    pub(crate) files: Box<[(Box<str>, u16, u32)]>,
    /// Embedded tokens to parse as self-injections: token kind index → rule.
    pub(crate) embedded_parse: Box<[(u16, u32)]>,
    /// Warnings produced while forging.
    pub(crate) warnings: Box<[Diagnostic]>,
}

/// A field of a node kind (LSF2 §11.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldDef {
    pub(crate) label: u16,
    /// 0 one, 1 optional, 2 many.
    pub(crate) cardinality: u8,
    /// Kind indexes the field can hold, sorted.
    pub(crate) kinds: Box<[u16]>,
}

/// One `[injections]` entry, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InjectionDef {
    pub(crate) id: Box<str>,
    /// The target: a kind index, or a field (node kind index and label).
    pub(crate) kind: u16,
    pub(crate) label: Option<u16>,
    pub(crate) language: Box<str>,
    pub(crate) editor: bool,
    /// The rule a `self` injection parses with.
    pub(crate) start: u32,
    pub(crate) inner: bool,
    pub(crate) combined: bool,
    pub(crate) scope: Option<Box<str>>,
}

/// A capability named by the schematic, with where it was named.
#[derive(Clone, Debug)]
pub(crate) struct CapabilityRef {
    pub(crate) name: Box<str>,
    pub(crate) span: Span,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

/// The kind table: names by index, and a sorted index for lookup by name.
#[derive(Clone, Debug)]
pub(crate) struct Kinds {
    pub(crate) names: Box<[Box<str>]>,
    pub(crate) values: Box<[Kind]>,
    /// Kind indexes ordered by name; `EOF` is left out (it is never in a tree).
    pub(crate) by_name: Box<[u16]>,
    /// Each kind's category (`CAT_*`).
    pub(crate) cats: Box<[u8]>,
}

impl Kinds {
    /// The kind called `name`.
    pub(crate) fn get(&self, name: &str) -> Option<Kind> {
        // Equal names (a format-2 operator node named like a keyword) sit
        // together in index order; the first, the token, is the plain name's.
        let at = self
            .by_name
            .partition_point(|&i| (*self.names[i as usize]).cmp(name) == core::cmp::Ordering::Less);
        let &index = self.by_name.get(at)?;
        (*self.names[index as usize] == *name).then(|| self.values[index as usize])
    }

    /// The kinds called `name`, in index order (two at most: a keyword and
    /// a format-2 operator node of the same name).
    pub(crate) fn all_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = Kind> + 'a {
        let at = self
            .by_name
            .partition_point(|&i| (*self.names[i as usize]).cmp(name) == core::cmp::Ordering::Less);
        self.by_name[at..]
            .iter()
            .take_while(move |&&i| *self.names[i as usize] == *name)
            .map(move |&i| self.values[i as usize])
    }

    /// The name of `kind`.
    pub(crate) fn name(&self, kind: Kind) -> &str {
        self.names.get(kind.slot()).map_or("<unknown>", |n| n)
    }

    /// The name of the kind with index `index`.
    pub(crate) fn name_at(&self, index: usize) -> &str {
        self.names.get(index).map_or("<unknown>", |n| n)
    }

    /// The kind with index `index`.
    pub(crate) fn at(&self, index: usize) -> Kind {
        self.values[index]
    }

    /// The number of kinds, `EOF` included.
    pub(crate) fn len(&self) -> usize {
        self.names.len()
    }
}

/// One expression of the flat rule arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expr {
    /// A token, by kind index.
    Token(u16),
    /// A rule invocation.
    Rule(u32),
    /// `program.items[start..start + len]`, one after another.
    Seq { start: u32, len: u32 },
    /// `program.items[start..start + len]`, tried in order.
    Choice { start: u32, len: u32 },
    /// `body*` or `body+`; `stop` holds the tokens that may follow it.
    Repeat {
        body: u32,
        min_one: bool,
        stop: SetId,
    },
    /// `body?`.
    Optional(u32),
    /// A contextual keyword (format 2): the keyword's kind, or an `IDENT`
    /// whose text is the keyword's.
    Keyword(u16),
    /// `WORD` (format 2): an `IDENT` or any keyword.
    Word,
    /// `label:body` (format 2).
    Label { label: u16, body: u32 },
    /// `&body` (format 2).
    And(u32),
    /// `!body` (format 2).
    Not(u32),
    /// `body=label` (format 2).
    BackRef { label: u16, body: u32 },
    /// `EOF` (format 2): zero-width, at the end of the input.
    Eof,
    /// `LINE_START` (format 2): zero-width, the next token begins a line.
    LineStart,
    /// `NL_BEFORE` (format 2): zero-width, a line break precedes the next
    /// token.
    NlBefore,
}

/// A rule at run time.
#[derive(Clone, Debug)]
pub(crate) struct Rule {
    pub(crate) name: Box<str>,
    /// The node the rule builds; `None` for a hidden (`_`-prefixed) rule.
    pub(crate) node: Option<Kind>,
    pub(crate) body: RuleBody,
    pub(crate) first: SetId,
    pub(crate) nullable: bool,
    /// Extra recovery stop tokens (`[rules.x] sync`), or `NO_SET`.
    pub(crate) sync: SetId,
}

/// What a rule runs.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RuleBody {
    Expr(u32),
    Pratt(u32),
}

/// An expression rule's operator tables.
#[derive(Clone, Debug)]
pub(crate) struct Pratt {
    pub(crate) operand: u32,
    /// By token kind: the prefix level plus one, or zero.
    pub(crate) prefix: Box<[u8]>,
    /// By token kind: the infix or postfix level plus one, or zero.
    pub(crate) after: Box<[u8]>,
    pub(crate) levels: Box<[Level]>,
    /// Contextual keywords used as operators (format 2): keyword kind index,
    /// prefix level plus one, infix or postfix level plus one.
    pub(crate) contextual: Box<[(u16, u8, u8)]>,
}

/// One precedence level at run time.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Level {
    pub(crate) node: Kind,
    pub(crate) fixity: Fixity,
    /// Binding power on the left: the level binds if this is at least the
    /// caller's minimum.
    pub(crate) lbp: u16,
    /// The minimum binding power for the operand that follows the operator.
    pub(crate) rbp: u16,
    pub(crate) then: Option<u32>,
}

/// The parser's tables.
#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub(crate) exprs: Box<[Expr]>,
    pub(crate) items: Box<[u32]>,
    pub(crate) first: Box<[SetId]>,
    pub(crate) nullable: Box<[bool]>,
    pub(crate) rules: Box<[Rule]>,
    pub(crate) pratts: Box<[Pratt]>,
    pub(crate) sets: Sets,
    /// By item slot of a `Seq`: the tokens the rest of the sequence can begin
    /// with, which recovery inside the item must not skip — or `NO_SET` when
    /// the item is a token or nothing follows it.
    pub(crate) sync: Box<[SetId]>,
    pub(crate) start: u32,
    /// The bit standing for the end of input in token sets.
    pub(crate) eof: u16,
    /// The node kind that wraps skipped tokens.
    pub(crate) error: Kind,
    /// Kind indexes of the built-in classes the parser names in messages.
    pub(crate) newline: u16,
    pub(crate) ident: u16,
    pub(crate) number: u16,
    /// Kind indexes described as "a string".
    pub(crate) strings: Box<[u16]>,
    /// Format-2 features the parser must look for.
    pub(crate) v2: Option<Box<ProgramV2>>,
}

/// The parser's format-2 tables.
#[derive(Clone, Debug)]
pub(crate) struct ProgramV2 {
    /// Contextual keyword texts (lower case when keywords ignore ASCII case)
    /// and their kind indexes, sorted by text.
    pub(crate) contextual: Box<[(Box<str>, u16)]>,
    /// Keywords compare under ASCII case folding.
    pub(crate) case_insensitive: bool,
    /// The labels of operator-node children: lhs, op, rhs, operand.
    pub(crate) op_labels: [u16; 4],
    /// Whether any rule uses `LINE_START` / `NL_BEFORE`.
    pub(crate) lines: bool,
    /// Whether any rule uses a text back-reference.
    pub(crate) backrefs: bool,
    /// Whether any rule uses a predicate.
    pub(crate) predicates: bool,
}

impl Program {
    /// The children of a `Seq` or `Choice`.
    #[inline]
    pub(crate) fn children(&self, start: u32, len: u32) -> &[u32] {
        &self.items[start as usize..(start + len) as usize]
    }
}

/// A rule's parsed text.
pub(crate) enum Parsed<'t> {
    Grammar(Ast<'t>),
    Pratt {
        operand: Ast<'t>,
        thens: Vec<Option<Ast<'t>>>,
    },
    Failed,
}

/// Compiles a schematic. `text` is the schematic source, for locating
/// capabilities. Problems go to `report`.
pub(crate) fn compile(
    schematic: &Schematic<'_>,
    text: &str,
    report: &mut Report,
) -> Option<Grammar> {
    let parsed: Vec<Parsed<'_>> = schematic
        .rules
        .iter()
        .map(|r| parse_rule(r, false, report))
        .collect();

    let mut rule_ids: BTreeMap<&str, u32> = BTreeMap::new();
    for (i, spec) in schematic.rules.iter().enumerate() {
        check_name(&spec.name, spec.name_span, "rule", true, report);
        let _ = rule_ids.insert(&spec.name, i as u32);
    }

    let literals = collect_literals(schematic, &parsed, &rule_ids, report);
    if FIRST_LITERAL as usize + literals.len() + 2 + schematic.rules.len() > MAX_KINDS {
        report.error(
            codes::TOO_MANY_KINDS,
            Span::empty(0),
            format!("the language needs more than {MAX_KINDS} kinds"),
        );
        return None;
    }
    let literal_ids: BTreeMap<&str, u16> = literals
        .iter()
        .enumerate()
        .map(|(i, (text, _))| (*text, FIRST_LITERAL + i as u16))
        .collect();
    let n_tokens = FIRST_LITERAL as usize + literals.len();
    // Every expression rule has two operator tables of a byte per token kind;
    // they count against the same budget as the token sets.
    let pratt_rules = schematic
        .rules
        .iter()
        .filter(|r| matches!(r.body, Body::Pratt(_)))
        .count();
    let pratt_bytes = pratt_rules.saturating_mul(2 * n_tokens);
    if pratt_bytes > MAX_TABLE_BYTES {
        too_large(report);
        return None;
    }

    let mut builder = Builder::new(&rule_ids, &literal_ids, report);
    builder.newlines = schematic.lexer.newlines;
    builder.has_strings = !schematic.lexer.strings.is_empty();

    // Kind names: tokens, then EOF and ERROR, then nodes.
    let mut names: Vec<Box<str>> = BUILTIN_TOKENS
        .iter()
        .map(|n| Box::<str>::from(*n))
        .collect();
    names.extend(literals.iter().map(|(text, _)| Box::<str>::from(*text)));
    names.push("EOF".into());
    names.push("ERROR".into());
    let mut node_ids: BTreeMap<String, u16> = BTreeMap::new();
    let mut rule_nodes = Vec::with_capacity(schematic.rules.len());
    for spec in &schematic.rules {
        if spec.name.starts_with('_') {
            rule_nodes.push(None);
            continue;
        }
        let index = names.len() as u16;
        names.push(Box::from(&*spec.name));
        let _ = node_ids.insert(spec.name.to_string(), index);
        rule_nodes.push(Some(index));
    }

    // Lower every rule.
    let mut bodies = Vec::with_capacity(schematic.rules.len());
    let mut pratt_specs = Vec::new();
    for (spec, parsed) in schematic.rules.iter().zip(&parsed) {
        let body = match (parsed, &spec.body) {
            (Parsed::Grammar(ast), _) => builder.lower(ast).map(RuleBody::Expr),
            (Parsed::Pratt { operand, thens }, Body::Pratt(pratt)) => {
                let operand = builder.lower(operand);
                let thens: Vec<Option<u32>> = thens
                    .iter()
                    .map(|t| t.as_ref().and_then(|t| builder.lower(t)))
                    .collect();
                let levels = builder.levels(pratt, &mut names, &mut node_ids, n_tokens, &|op| {
                    literal_ids.get(op).map(|&k| (k, false))
                });
                match (operand, levels) {
                    (Some(operand), Some(levels)) => {
                        pratt_specs.push((operand, thens, levels));
                        Some(RuleBody::Pratt(pratt_specs.len() as u32 - 1))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        bodies.push(body);
    }
    if names.len() > MAX_KINDS {
        builder.report.error(
            codes::TOO_MANY_KINDS,
            Span::empty(0),
            format!("the language needs more than {MAX_KINDS} kinds"),
        );
        return None;
    }

    let start = start_rule(schematic, &rule_ids, builder.report);
    // A rule that failed to lower is analysed as matching nothing at all —
    // no tokens, not even the empty string — so the analysis still checks
    // every other rule without inventing problems of its own.
    let bodies: Vec<RuleBody> = bodies
        .into_iter()
        .map(|body| body.unwrap_or_else(|| RuleBody::Expr(builder.nothing())))
        .collect();

    let lexer = Lexer::build(
        &schematic.lexer,
        &literals
            .iter()
            .enumerate()
            .map(|(i, (text, span))| (*text, Kind::new(FIRST_LITERAL + i as u16, false), *span))
            .collect::<Vec<_>>(),
        builder.report,
    );

    // Kind values, with trivia flags from the lexer.
    let values: Vec<Kind> = (0..names.len())
        .map(|i| {
            if i < FIRST_LITERAL as usize {
                lexer.builtin(i as u16)
            } else {
                Kind::new(i as u16, false)
            }
        })
        .collect();
    let eof = n_tokens as u16;
    let cats: Vec<u8> = (0..names.len())
        .map(|i| match i {
            i if i < FIRST_LITERAL as usize => CAT_BUILTIN,
            i if i < n_tokens => CAT_LITERAL,
            i if i == n_tokens => CAT_EOF,
            _ => CAT_NODE,
        })
        .collect();
    let kinds = kind_table(names, values, cats, eof);
    let error = kinds.at(n_tokens + 1);

    let Builder {
        exprs,
        items,
        spans,
        report,
        ..
    } = builder;
    let analysis = Analysis::new(
        exprs,
        items,
        spans,
        n_tokens + 1,
        schematic.rules.len(),
        pratt_bytes,
        report,
    )?;
    let pratts: Vec<Pratt> = pratt_specs
        .into_iter()
        .map(
            |(operand, thens, (levels, prefix, after, contextual))| Pratt {
                operand,
                prefix,
                after,
                levels: levels
                    .into_iter()
                    .zip(thens)
                    .map(|(level, then)| Level { then, ..level })
                    .collect(),
                contextual,
            },
        )
        .collect();
    let rules: Vec<Rule> = schematic
        .rules
        .iter()
        .zip(bodies)
        .zip(rule_nodes)
        .map(|((spec, body), node)| Rule {
            name: Box::from(spec.name.trim_start_matches('_')),
            node: node.map(|i| kinds.at(i as usize)),
            body,
            first: 0,
            nullable: false,
            sync: NO_SET,
        })
        .collect();
    let shape = Shape {
        eof,
        error,
        newline: lexer::NEWLINE,
        ident: lexer::IDENT,
        number: lexer::NUMBER,
        strings: Box::new([lexer::STRING]),
        v2: None,
    };
    let names: Vec<&str> = schematic.rules.iter().map(|r| &*r.name).collect();
    let spans: Vec<Span> = schematic.rules.iter().map(|r| r.name_span).collect();
    let program = analysis.finish(rules, pratts, start, shape, &names, &spans, &Vec::new())?;

    let capabilities = schematic
        .capabilities
        .iter()
        .map(|(name, span)| {
            let (line, column) = line_col(text, span.start().to_usize());
            CapabilityRef {
                name: Box::from(&**name),
                span: *span,
                line,
                column,
            }
        })
        .collect();

    Some(Grammar {
        name: Box::from(&*schematic.name),
        version: schematic.version.as_deref().map(Box::from),
        extensions: schematic
            .extensions
            .iter()
            .map(|e| Box::from(&**e))
            .collect(),
        capabilities,
        kinds,
        lexer: Lex::V1(Box::new(lexer)),
        program,
        extra: None,
    })
}

/// What the analysis needs to know about the kinds, besides the grammar.
pub(crate) struct Shape {
    pub(crate) eof: u16,
    pub(crate) error: Kind,
    pub(crate) newline: u16,
    pub(crate) ident: u16,
    pub(crate) number: u16,
    pub(crate) strings: Box<[u16]>,
    pub(crate) v2: Option<Box<ProgramV2>>,
}

/// Parses one rule's text (and an expression rule's operand and `then`s).
pub(crate) fn parse_rule<'t>(spec: &'t RuleSpec<'_>, v2: bool, report: &mut Report) -> Parsed<'t> {
    match &spec.body {
        Body::Grammar(text, span) => {
            rule::parse(text, *span, v2, report).map_or(Parsed::Failed, Parsed::Grammar)
        }
        Body::Pratt(pratt) => {
            let operand = rule::parse(&pratt.operand.0, pratt.operand.1, v2, report);
            let mut failed = operand.is_none();
            let thens = pratt
                .levels
                .iter()
                .map(|level| {
                    level.then.as_ref().map(|(text, span)| {
                        let ast = rule::parse(text, *span, v2, report);
                        failed |= ast.is_none();
                        ast
                    })
                })
                .map(Option::flatten)
                .collect();
            match operand {
                Some(operand) if !failed => Parsed::Pratt { operand, thens },
                _ => Parsed::Failed,
            }
        }
    }
}

/// Checks a rule or node name.
fn check_name(name: &str, span: Span, what: &str, may_hide: bool, report: &mut Report) {
    let valid = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && name != "_";
    if !valid {
        report.error_help(
            codes::RULE_NAME,
            span,
            format!("`{name}` is not a valid {what} name"),
            "use letters, digits, and `_`, starting with a letter or `_`",
        );
    } else if RESERVED.contains(&name) {
        report.error(
            codes::RESERVED,
            span,
            format!("`{name}` is the name of a built-in kind and cannot name a {what}"),
        );
    } else if !may_hide && name.starts_with('_') {
        report.error(
            codes::RULE_NAME,
            span,
            format!("a {what} name cannot start with `_`"),
        );
    }
}

/// Every literal the grammar uses, in order of first appearance, validated.
fn collect_literals<'t>(
    schematic: &'t Schematic<'_>,
    parsed: &[Parsed<'t>],
    rule_ids: &BTreeMap<&str, u32>,
    report: &mut Report,
) -> Vec<(&'t str, Span)> {
    let mut found: Vec<(&'t str, Span)> = Vec::new();
    let visit = |ast: &Ast<'t>, found: &mut Vec<(&'t str, Span)>| {
        ast.walk(&mut |node: &Ast<'t>| {
            if let Ast::Literal(text, span) = node {
                found.push((*text, *span));
            }
        });
    };
    for (spec, parsed) in schematic.rules.iter().zip(parsed) {
        match (parsed, &spec.body) {
            (Parsed::Grammar(ast), _) => visit(ast, &mut found),
            (Parsed::Pratt { operand, thens }, Body::Pratt(pratt)) => {
                visit(operand, &mut found);
                for (level, then) in pratt.levels.iter().zip(thens) {
                    found.extend(level.operators.iter().map(|(op, span)| (&**op, *span)));
                    if let Some(then) = then {
                        visit(then, &mut found);
                    }
                }
            }
            _ => {}
        }
    }
    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    let mut literals = Vec::new();
    for (text, span) in found {
        if seen.insert(text, ()).is_none() && check_literal(text, span, schematic, rule_ids, report)
        {
            literals.push((text, span));
        }
    }
    literals
}

/// Whether `text` can be lexed as a single keyword or symbol of its own.
fn check_literal(
    text: &str,
    span: Span,
    schematic: &Schematic<'_>,
    rule_ids: &BTreeMap<&str, u32>,
    report: &mut Report,
) -> bool {
    let mode = schematic.lexer.identifiers;
    let Some(first) = text.chars().next() else {
        report.error(codes::LITERAL_SHAPE, span, "a literal cannot be empty");
        return false;
    };
    if text.contains(char::is_whitespace) {
        report.error_help(
            codes::LITERAL_SHAPE,
            span,
            format!("literal `{text}` contains whitespace"),
            "write each word as its own literal",
        );
        return false;
    }
    if first.is_ascii_digit() {
        report.error(
            codes::LITERAL_SHAPE,
            span,
            format!("literal `{text}` starts with a digit, which the lexer reads as a number"),
        );
        return false;
    }
    if lexer::is_ident_start(first, mode) && !lexer::is_ident(text, mode) {
        report.error_help(
            codes::LITERAL_SHAPE,
            span,
            format!("literal `{text}` mixes identifier and symbol characters"),
            "split it into a keyword and a symbol",
        );
        return false;
    }
    if lexer::is_ident(text, mode) {
        if RESERVED.contains(&text) {
            report.error(
                codes::RESERVED,
                span,
                format!("keyword `{text}` has the name of a built-in kind"),
            );
            return false;
        }
        if rule_ids.contains_key(text) {
            report.error_help(
                codes::RESERVED,
                span,
                format!("`{text}` is both a keyword and a rule"),
                "rename the rule; kinds share one namespace",
            );
            return false;
        }
    }
    true
}

/// The start rule: `start` in `[language]`, or the first rule.
fn start_rule(
    schematic: &Schematic<'_>,
    rule_ids: &BTreeMap<&str, u32>,
    report: &mut Report,
) -> Option<u32> {
    let (name, span) = match &schematic.start {
        Some((name, span)) => (&**name, *span),
        None => {
            let first = schematic.rules.first()?;
            (&*first.name, first.name_span)
        }
    };
    let Some(&id) = rule_ids.get(name) else {
        let help = suggest(report, name, rule_ids.keys().copied());
        match help {
            Some(help) => report.error_help(
                codes::UNDEFINED,
                span,
                format!("start rule `{name}` is not defined"),
                help,
            ),
            None => report.error(
                codes::UNDEFINED,
                span,
                format!("start rule `{name}` is not defined"),
            ),
        }
        return None;
    };
    if name.starts_with('_') {
        report.error_help(
            codes::RULE_NAME,
            span,
            format!("the start rule `{name}` is hidden"),
            "the start rule's node is the root of every tree; remove the leading `_`",
        );
        return None;
    }
    Some(id)
}

/// "did you mean `x`?" for the closest name within a third of the name's
/// length in edits (at least one), from the forge's bounded budget.
fn suggest<'a>(
    report: &mut Report,
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<String> {
    let limit = (name.chars().count() / 3).max(1);
    report.suggest(name, candidates, limit)
}

pub(crate) fn kind_table(
    names: Vec<Box<str>>,
    values: Vec<Kind>,
    cats: Vec<u8>,
    eof: u16,
) -> Kinds {
    let mut by_name: Vec<u16> = (0..names.len() as u16).filter(|&i| i != eof).collect();
    by_name.sort_by(|&a, &b| names[a as usize].cmp(&names[b as usize]).then(a.cmp(&b)));
    Kinds {
        names: names.into(),
        values: values.into(),
        by_name: by_name.into(),
        cats: cats.into(),
    }
}

/// What a name in a format-2 rule resolves to, besides a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A token kind.
    Token(u16),
    /// A rule (a string class that builds a node).
    Rule(u32),
    /// `WORD`.
    Word,
    /// `EOF`.
    Eof,
    /// `LINE_START`.
    LineStart,
    /// `NL_BEFORE`.
    NlBefore,
    /// A trivia class, which the parser never sees.
    Trivia,
    /// A name that is a kind but not something a rule can match.
    NotMatchable,
}

/// The names a format-2 rule can use besides rules.
#[derive(Debug, Default)]
pub(crate) struct V2Names<'a> {
    /// Token classes, built-ins, and assertions by name.
    pub(crate) classes: BTreeMap<&'a str, Resolved>,
    /// What `STRING` stands for: every string class, in declaration order.
    pub(crate) strings: Vec<Resolved>,
    /// Texts of the contextual keywords.
    pub(crate) contextual: alloc::collections::BTreeSet<&'a str>,
}

/// The field labels of a language, numbered by first occurrence.
#[derive(Debug, Default)]
pub(crate) struct Labels {
    pub(crate) names: Vec<Box<str>>,
    pub(crate) ids: BTreeMap<Box<str>, u16>,
}

impl Labels {
    /// The id of `name`, numbering it if new; `None` past the limit.
    pub(crate) fn intern(&mut self, name: &str) -> Option<u16> {
        if let Some(&id) = self.ids.get(name) {
            return Some(id);
        }
        if self.names.len() >= MAX_LABELS {
            return None;
        }
        let id = self.names.len() as u16;
        self.names.push(Box::from(name));
        let _ = self.ids.insert(Box::from(name), id);
        Some(id)
    }
}

/// The labels operator nodes give their children (LSF2 §11.3).
pub(crate) const OPERATOR_LABELS: [&str; 4] = ["lhs", "op", "rhs", "operand"];

/// Lowers rule ASTs into the expression arena.
pub(crate) struct Builder<'a, 'r> {
    pub(crate) exprs: Vec<Expr>,
    pub(crate) items: Vec<u32>,
    pub(crate) spans: Vec<Span>,
    rule_ids: &'a BTreeMap<&'a str, u32>,
    literal_ids: &'a BTreeMap<&'a str, u16>,
    pub(crate) newlines: bool,
    pub(crate) has_strings: bool,
    factor_depth: u32,
    pub(crate) report: &'r mut Report,
    /// Format-2 names; `None` for format 1.
    pub(crate) v2: Option<&'a V2Names<'a>>,
    pub(crate) labels: Labels,
    /// The labels the current rule has used so far, for back-references.
    scope: Vec<u16>,
    /// Format-2 features seen.
    pub(crate) uses_lines: bool,
    pub(crate) uses_backrefs: bool,
    pub(crate) uses_predicates: bool,
}

/// An expression rule's levels, its prefix and infix/postfix tables, and its
/// contextual operators.
pub(crate) type LevelTables = (Vec<Level>, Box<[u8]>, Box<[u8]>, Box<[(u16, u8, u8)]>);

impl<'a, 'r> Builder<'a, 'r> {
    pub(crate) fn new(
        rule_ids: &'a BTreeMap<&'a str, u32>,
        literal_ids: &'a BTreeMap<&'a str, u16>,
        report: &'r mut Report,
    ) -> Self {
        Self {
            exprs: Vec::new(),
            items: Vec::new(),
            spans: Vec::new(),
            rule_ids,
            literal_ids,
            newlines: false,
            has_strings: false,
            factor_depth: 0,
            report,
            v2: None,
            labels: Labels::default(),
            scope: Vec::new(),
            uses_lines: false,
            uses_backrefs: false,
            uses_predicates: false,
        }
    }

    /// Starts lowering a new rule: back-references see only its labels.
    pub(crate) fn begin_rule(&mut self) {
        self.scope.clear();
    }

    fn push(&mut self, expr: Expr, span: Span) -> u32 {
        self.exprs.push(expr);
        self.spans.push(span);
        self.exprs.len() as u32 - 1
    }

    pub(crate) fn lower(&mut self, ast: &Ast<'_>) -> Option<u32> {
        match ast {
            Ast::Literal(text, span) => {
                let kind = *self.literal_ids.get(text)?;
                let contextual = self.v2.is_some_and(|v| v.contextual.contains(text));
                Some(self.push(
                    if contextual {
                        Expr::Keyword(kind)
                    } else {
                        Expr::Token(kind)
                    },
                    *span,
                ))
            }
            Ast::Name(name, span) => self.resolve(name, *span),
            Ast::Seq(parts, span) => {
                let mut items = Vec::with_capacity(parts.len());
                let mut ok = true;
                for part in parts {
                    match self.lower(part) {
                        Some(id) => self.splice(id, &mut items),
                        None => ok = false,
                    }
                }
                ok.then(|| self.seq(items, *span))
            }
            Ast::Choice(alternatives, span) => {
                let mut alts = Vec::with_capacity(alternatives.len());
                let mut ok = true;
                for alt in alternatives {
                    match self.lower(alt) {
                        Some(id) => match self.exprs[id as usize] {
                            Expr::Choice { start, len } => {
                                for k in start..start + len {
                                    let child = self.items[k as usize];
                                    alts.push((self.seq_items(child), self.spans[child as usize]));
                                }
                            }
                            _ => alts.push((self.seq_items(id), alt.span())),
                        },
                        None => ok = false,
                    }
                }
                ok.then(|| self.factor(alts, *span))
            }
            Ast::Repeat {
                body,
                min_one,
                span,
            } => {
                let body = self.lower(body)?;
                Some(self.push(
                    Expr::Repeat {
                        body,
                        min_one: *min_one,
                        stop: 0,
                    },
                    *span,
                ))
            }
            Ast::Optional(body, span) => {
                let body = self.lower(body)?;
                Some(self.push(Expr::Optional(body), *span))
            }
            Ast::Label {
                label,
                label_span,
                body,
                span,
            } => {
                let id = self.label(label, *label_span, body);
                let body = self.lower(body)?;
                let id = id?;
                self.scope.push(id);
                Some(self.push(Expr::Label { label: id, body }, *span))
            }
            Ast::And(body, span) | Ast::Not(body, span) => {
                self.uses_predicates = true;
                let positive = matches!(ast, Ast::And(..));
                let body = self.lower(body)?;
                Some(self.push(
                    if positive {
                        Expr::And(body)
                    } else {
                        Expr::Not(body)
                    },
                    *span,
                ))
            }
            Ast::BackRef {
                body,
                label,
                label_span,
                span,
            } => {
                self.uses_backrefs = true;
                let body = self.lower(body)?;
                if !matches!(
                    self.exprs[body as usize],
                    Expr::Token(_) | Expr::Keyword(_) | Expr::Word
                ) {
                    self.report.error(
                        codes::BACKREF_NOT_TOKEN,
                        *span,
                        "a text back-reference must follow a token (a literal, a class, or `WORD`)",
                    );
                    return None;
                }
                let id = self.labels.ids.get(*label).copied();
                match id.filter(|id| self.scope.contains(id)) {
                    Some(id) => Some(self.push(Expr::BackRef { label: id, body }, *span)),
                    None => {
                        self.report.error(
                            codes::BACKREF_LATER,
                            *label_span,
                            format!("`={label}` refers to label `{label}`, which does not occur earlier in this rule"),
                        );
                        None
                    }
                }
            }
            Ast::Hook(name, span) => {
                self.report.error_help(
                    codes::NOT_SUPPORTED,
                    *span,
                    format!(
                        "predicate hooks (`@{name}`) are not supported by lang-forge 2.0.0-alpha.1"
                    ),
                    "hooks need the capability runtime (ROADMAP: lang-forge alpha.2)",
                );
                None
            }
            Ast::Mode { mode, span, .. } => {
                self.report.error_help(
                    codes::NOT_SUPPORTED,
                    *span,
                    format!("parser-driven modes (`%{mode}(...)`) are not supported by lang-forge 2.0.0-alpha.1"),
                    "lazy lexing under parser control is ROADMAP: lang-forge alpha.2",
                );
                None
            }
        }
    }

    /// Checks and numbers a label; `None` (reported) if it cannot be used.
    fn label(&mut self, name: &str, span: Span, body: &Ast<'_>) -> Option<u16> {
        if !crate::spec2::is_name(name, crate::spec2::NameKind::Label) {
            self.report.error(
                codes::LABEL_NAME,
                span,
                format!("label `{name}` must match `[a-z][a-z0-9_]*` (at most 64 bytes)"),
            );
            return None;
        }
        if OPERATOR_LABELS.contains(&name) {
            self.report.error(
                codes::LABEL_RESERVED,
                span,
                format!("`{name}` is reserved for operator nodes"),
            );
            return None;
        }
        if name == "kind" {
            self.report.error(
                codes::LABEL_RESERVED,
                span,
                "label `kind` is reserved (it is the `kind:` prefix of picks)",
            );
            return None;
        }
        let zero_width = match body {
            Ast::And(..) | Ast::Not(..) | Ast::Hook(..) => true,
            Ast::Name(n, _) => matches!(*n, "EOF" | "LINE_START" | "NL_BEFORE"),
            _ => false,
        };
        if zero_width {
            self.report.error(
                codes::LABEL_ZERO_WIDTH,
                span,
                "a predicate adds nothing to the tree; it cannot be labelled",
            );
            return None;
        }
        match self.labels.intern(name) {
            Some(id) => Some(id),
            None => {
                self.report.error(
                    codes::TOO_MANY_LABELS,
                    span,
                    format!("a language has at most {MAX_LABELS} field labels"),
                );
                None
            }
        }
    }

    fn resolve(&mut self, name: &str, span: Span) -> Option<u32> {
        if let Some(&id) = self.rule_ids.get(name) {
            return Some(self.push(Expr::Rule(id), span));
        }
        if let Some(v2) = self.v2 {
            return self.resolve_v2(v2, name, span);
        }
        let class = match name {
            "IDENT" => lexer::IDENT,
            "NUMBER" => lexer::NUMBER,
            "STRING" if self.has_strings => lexer::STRING,
            "STRING" => {
                self.report.error_help(
                    codes::UNDEFINED,
                    span,
                    "the grammar uses STRING, but [lexer] declares no strings",
                    "add `strings = ['\"']` to [lexer]",
                );
                return None;
            }
            "NEWLINE" if self.newlines => lexer::NEWLINE,
            "NEWLINE" => {
                self.report.error_help(
                    codes::UNDEFINED,
                    span,
                    "the grammar uses NEWLINE, but line breaks are whitespace",
                    "set `newlines = true` in [lexer]",
                );
                return None;
            }
            "WHITESPACE" | "COMMENT" | "UNKNOWN" => {
                self.report.error(
                    codes::UNDEFINED,
                    span,
                    format!("`{name}` is trivia, which the parser never sees"),
                );
                return None;
            }
            "ERROR" | "EOF" => {
                self.report.error(
                    codes::UNDEFINED,
                    span,
                    format!("`{name}` is not a token a rule can match"),
                );
                return None;
            }
            _ => {
                let message = format!("undefined rule `{name}`");
                match suggest(self.report, name, self.rule_ids.keys().copied()) {
                    Some(help) => self
                        .report
                        .error_help(codes::UNDEFINED, span, message, help),
                    None => self.report.error(codes::UNDEFINED, span, message),
                }
                return None;
            }
        };
        Some(self.push(Expr::Token(class), span))
    }

    fn resolve_v2(&mut self, v2: &V2Names<'_>, name: &str, span: Span) -> Option<u32> {
        if name == "STRING" {
            return match v2.strings.as_slice() {
                [] => {
                    self.report.error_help(
                        codes::UNDEFINED,
                        span,
                        "the grammar uses STRING, but [lexer] declares no string classes",
                        "declare a class such as `[lexer.strings.DQ]` with `open = '\"'`",
                    );
                    None
                }
                [one] => self.resolved(*one, name, span),
                many => {
                    let mut ids = Vec::with_capacity(many.len());
                    for r in many {
                        ids.push(self.resolved(*r, name, span)?);
                    }
                    let start = self.items.len() as u32;
                    let len = ids.len() as u32;
                    self.items.extend(ids);
                    Some(self.push(Expr::Choice { start, len }, span))
                }
            };
        }
        match v2.classes.get(name) {
            Some(r) => self.resolved(*r, name, span),
            None => {
                let message = if name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                {
                    format!("undefined token class `{name}`")
                } else {
                    format!("undefined rule `{name}`")
                };
                let candidates = self
                    .rule_ids
                    .keys()
                    .copied()
                    .chain(v2.classes.keys().copied());
                match suggest(self.report, name, candidates) {
                    Some(help) => self
                        .report
                        .error_help(codes::UNDEFINED, span, message, help),
                    None => self.report.error(codes::UNDEFINED, span, message),
                }
                None
            }
        }
    }

    fn resolved(&mut self, r: Resolved, name: &str, span: Span) -> Option<u32> {
        let expr = match r {
            Resolved::Token(k) => Expr::Token(k),
            Resolved::Rule(id) => Expr::Rule(id),
            Resolved::Word => Expr::Word,
            Resolved::Eof => Expr::Eof,
            Resolved::LineStart => {
                self.uses_lines = true;
                Expr::LineStart
            }
            Resolved::NlBefore => {
                self.uses_lines = true;
                Expr::NlBefore
            }
            Resolved::Trivia => {
                self.report.error(
                    codes::UNDEFINED,
                    span,
                    format!("`{name}` is trivia, which the parser never sees"),
                );
                return None;
            }
            Resolved::NotMatchable => {
                self.report.error(
                    codes::UNDEFINED,
                    span,
                    format!("`{name}` is not a token a rule can match"),
                );
                return None;
            }
        };
        Some(self.push(expr, span))
    }

    /// An expression that matches nothing: an empty choice.
    pub(crate) fn nothing(&mut self) -> u32 {
        let start = self.items.len() as u32;
        self.push(Expr::Choice { start, len: 0 }, Span::empty(0))
    }

    /// A rule's elements in sequence; a single element stands alone.
    fn seq(&mut self, items: Vec<u32>, span: Span) -> u32 {
        if items.len() == 1 {
            return items[0];
        }
        let start = self.items.len() as u32;
        let len = items.len() as u32;
        self.items.extend(items);
        self.push(Expr::Seq { start, len }, span)
    }

    /// Appends `id` to `items`, flattening a nested sequence.
    fn splice(&self, id: u32, items: &mut Vec<u32>) {
        match self.exprs[id as usize] {
            Expr::Seq { start, len } => {
                items.extend_from_slice(&self.items[start as usize..(start + len) as usize])
            }
            _ => items.push(id),
        }
    }

    fn seq_items(&self, id: u32) -> Vec<u32> {
        let mut items = Vec::new();
        self.splice(id, &mut items);
        items
    }

    /// Builds a choice, merging adjacent alternatives that begin with the same
    /// elements: `x a | x b` becomes `x (a | b)`. Ordered choice is unchanged
    /// by this — `x` matches the same way in both alternatives — but the
    /// parser now reads `x` once instead of trying it twice.
    fn factor(&mut self, alts: Vec<(Vec<u32>, Span)>, span: Span) -> u32 {
        let mut out: Vec<(Vec<u32>, Span)> = Vec::with_capacity(alts.len());
        let mut i = 0;
        while i < alts.len() {
            let mut j = i + 1;
            if self.factor_depth < MAX_FACTOR_DEPTH && !alts[i].0.is_empty() {
                while j < alts.len()
                    && !alts[j].0.is_empty()
                    && self.same(alts[i].0[0], alts[j].0[0])
                {
                    j += 1;
                }
            }
            if j - i == 1 {
                out.push(alts[i].clone());
                i += 1;
                continue;
            }
            let group = &alts[i..j];
            let mut shared = 1;
            while group.iter().all(|a| a.0.len() > shared)
                && group[1..]
                    .iter()
                    .all(|a| self.same(group[0].0[shared], a.0[shared]))
            {
                shared += 1;
            }
            if let Some(k) = group.iter().position(|a| a.0.len() == shared) {
                for dead in &group[k + 1..] {
                    self.report.error_help(
                        codes::DEAD_ALTERNATIVE,
                        dead.1,
                        "this alternative can never match",
                        "an earlier alternative matches the same beginning and is tried first; list longer alternatives before shorter ones",
                    );
                }
            }
            let rests: Vec<(Vec<u32>, Span)> = group
                .iter()
                .map(|a| (a.0[shared..].to_vec(), a.1))
                .collect();
            let cover = group.iter().fold(group[0].1, |acc, a| acc.merge(a.1));
            let mut merged = group[0].0[..shared].to_vec();
            self.factor_depth += 1;
            let tail = self.factor(rests, cover);
            self.factor_depth -= 1;
            self.splice(tail, &mut merged);
            out.push((merged, cover));
            i = j;
        }
        if out.len() == 1 {
            let (items, cover) = out.remove(0);
            return self.seq(items, cover);
        }
        let ids: Vec<u32> = out
            .into_iter()
            .map(|(items, cover)| self.seq(items, cover))
            .collect();
        let start = self.items.len() as u32;
        let len = ids.len() as u32;
        self.items.extend(ids);
        self.push(Expr::Choice { start, len }, span)
    }

    /// Structural equality of two expressions.
    fn same(&self, a: u32, b: u32) -> bool {
        if a == b {
            return true;
        }
        match (self.exprs[a as usize], self.exprs[b as usize]) {
            (Expr::Token(x), Expr::Token(y)) | (Expr::Keyword(x), Expr::Keyword(y)) => x == y,
            (Expr::Rule(x), Expr::Rule(y)) => x == y,
            (Expr::Seq { start: s1, len: l1 }, Expr::Seq { start: s2, len: l2 })
            | (Expr::Choice { start: s1, len: l1 }, Expr::Choice { start: s2, len: l2 }) => {
                l1 == l2
                    && (0..l1).all(|k| {
                        self.same(self.items[(s1 + k) as usize], self.items[(s2 + k) as usize])
                    })
            }
            (
                Expr::Repeat {
                    body: b1,
                    min_one: m1,
                    ..
                },
                Expr::Repeat {
                    body: b2,
                    min_one: m2,
                    ..
                },
            ) => m1 == m2 && self.same(b1, b2),
            (Expr::Optional(x), Expr::Optional(y))
            | (Expr::And(x), Expr::And(y))
            | (Expr::Not(x), Expr::Not(y)) => self.same(x, y),
            (
                Expr::Label {
                    label: l1,
                    body: b1,
                },
                Expr::Label {
                    label: l2,
                    body: b2,
                },
            )
            | (
                Expr::BackRef {
                    label: l1,
                    body: b1,
                },
                Expr::BackRef {
                    label: l2,
                    body: b2,
                },
            ) => l1 == l2 && self.same(b1, b2),
            (Expr::Word, Expr::Word)
            | (Expr::Eof, Expr::Eof)
            | (Expr::LineStart, Expr::LineStart)
            | (Expr::NlBefore, Expr::NlBefore) => true,
            _ => false,
        }
    }

    /// An expression rule's levels and operator tables. `operator` resolves
    /// an operator entry to its kind index and whether it is a contextual
    /// keyword (recognized from an `IDENT`'s text).
    pub(crate) fn levels(
        &mut self,
        pratt: &crate::schematic::PrattSpec<'_>,
        names: &mut Vec<Box<str>>,
        node_ids: &mut BTreeMap<String, u16>,
        n_tokens: usize,
        operator: &dyn Fn(&str) -> Option<(u16, bool)>,
    ) -> Option<LevelTables> {
        if pratt.levels.len() > MAX_LEVELS {
            self.report.error(
                codes::TOO_MANY_LEVELS,
                pratt.levels[MAX_LEVELS].span,
                format!("an expression rule has at most {MAX_LEVELS} levels"),
            );
            return None;
        }
        let mut prefix = vec![0u8; n_tokens].into_boxed_slice();
        let mut after = vec![0u8; n_tokens].into_boxed_slice();
        let mut contextual: Vec<(u16, u8, u8)> = Vec::new();
        let mut levels = Vec::with_capacity(pratt.levels.len());
        let mut ok = true;
        for (i, spec) in pratt.levels.iter().enumerate() {
            let (name, span) = match &spec.node {
                Some((name, span)) => {
                    if self.v2.is_some() {
                        if !crate::spec2::is_name(name, crate::spec2::NameKind::Rule)
                            || name.starts_with('_')
                        {
                            self.report.error(
                                codes::RULE_NAME,
                                *span,
                                format!("node name `{name}` must match `[a-z][a-z0-9_]*`"),
                            );
                        }
                    } else {
                        check_name(name, *span, "node", false, self.report);
                    }
                    (name.to_string(), *span)
                }
                None => (
                    String::from(match spec.fixity {
                        Fixity::Prefix => "prefix",
                        Fixity::Postfix => "postfix",
                        _ => "binary",
                    }),
                    spec.span,
                ),
            };
            // Format 1 gives every kind its own name. Format 2 tells a node
            // from a literal by reference syntax (`kind:x` versus `'x'`,
            // LSF2 §6.3), so an operator node may share a keyword's name.
            if self.v2.is_none() && self.literal_ids.contains_key(name.as_str()) {
                self.report.error_help(
                    codes::RESERVED,
                    span,
                    format!("node name `{name}` is also a keyword"),
                    "set a different `node` name on this level",
                );
                ok = false;
            }
            let node = match node_ids.get(&name) {
                Some(&index) => index,
                None => {
                    let index = names.len() as u16;
                    names.push(Box::from(name.as_str()));
                    let _ = node_ids.insert(name, index);
                    index
                }
            };
            let lbp = 2 * (i as u16 + 1);
            let rbp = match spec.fixity {
                Fixity::Right | Fixity::Prefix | Fixity::Postfix => lbp,
                Fixity::Left | Fixity::NonAssoc => lbp + 1,
            };
            for (op, op_span) in &spec.operators {
                let Some((kind, is_contextual)) = operator(op) else {
                    ok = false;
                    continue;
                };
                let is_prefix = spec.fixity == Fixity::Prefix;
                let taken = if is_contextual {
                    let entry = match contextual.iter().position(|c| c.0 == kind) {
                        Some(at) => &mut contextual[at],
                        None => {
                            contextual.push((kind, 0, 0));
                            let last = contextual.len() - 1;
                            &mut contextual[last]
                        }
                    };
                    let slot = if is_prefix {
                        &mut entry.1
                    } else {
                        &mut entry.2
                    };
                    let taken = *slot != 0;
                    if !taken {
                        *slot = i as u8 + 1;
                    }
                    taken
                } else {
                    let table = if is_prefix { &mut prefix } else { &mut after };
                    let slot = &mut table[kind as usize];
                    let taken = *slot != 0;
                    if !taken {
                        *slot = i as u8 + 1;
                    }
                    taken
                };
                if taken {
                    let place = if is_prefix {
                        "a prefix"
                    } else {
                        "an infix or postfix"
                    };
                    self.report.error(
                        codes::OPERATOR_TWICE,
                        *op_span,
                        format!("`{op}` is already {place} operator at an earlier level"),
                    );
                    ok = false;
                }
            }
            levels.push(Level {
                node: Kind::new(node, false),
                fixity: spec.fixity,
                lbp,
                rbp,
                then: None,
            });
        }
        contextual.sort_unstable_by_key(|c| c.0);
        ok.then_some((levels, prefix, after, contextual.into()))
    }
}

/// Fixpoint analysis over the lowered rules.
pub(crate) struct Analysis<'r> {
    pub(crate) exprs: Vec<Expr>,
    pub(crate) items: Vec<u32>,
    pub(crate) spans: Vec<Span>,
    pub(crate) sets: Sets,
    pub(crate) first: Vec<SetId>,
    pub(crate) nullable: Vec<bool>,
    /// Whether an expression can succeed without consuming input. For a
    /// format-1 grammar this is exactly `nullable`; predicates and assertions
    /// are `empty` but take their body's FIRST set for the parser's pruning.
    pub(crate) empty: Vec<bool>,
    /// By rule: its FIRST set.
    rule_first: Vec<SetId>,
    /// By rule: whether it can succeed without consuming input.
    rule_empty: Vec<bool>,
    /// Bytes of other tables counted against `MAX_TABLE_BYTES`.
    other_bytes: usize,
    pub(crate) report: &'r mut Report,
    /// FOLLOW sets where they were computed (`NO_SET` elsewhere).
    pub(crate) follow: Vec<SetId>,
    /// Each rule's FOLLOW set, once computed.
    pub(crate) rule_follow: Vec<SetId>,
    /// Each rule's expressions, children before parents.
    pub(crate) owned: Vec<Vec<u32>>,
    /// Compute FOLLOW sets for every repetition and optional (format 2's
    /// overlap check reads them).
    pub(crate) follow_all: bool,
}

/// What the analysis needs to know about tokens beyond the grammar.
pub(crate) struct TokenFacts<'a> {
    /// The end-of-input bit.
    pub(crate) eof: u16,
    /// `IDENT`, which a contextual keyword also matches.
    pub(crate) ident: u16,
    /// Every keyword kind, which `WORD` matches.
    pub(crate) keywords: &'a [u16],
}

/// Where an expression's FIRST set comes from, when it is exactly another's.
#[derive(Clone, Copy)]
enum SameFirst {
    Expr(u32),
    Rule(u32),
}

/// Refuses a grammar whose tables would exceed `MAX_TABLE_BYTES`.
pub(crate) fn too_large(report: &mut Report) {
    report.error_help(
        codes::TABLES_TOO_LARGE,
        Span::empty(0),
        format!(
            "the grammar's tables would need more than {} MiB",
            MAX_TABLE_BYTES >> 20
        ),
        "the parser's tables grow with the number of rules and alternatives times the number of distinct keywords and symbols",
    );
}

/// The children of an expression that has exactly one: its body.
fn body_of(expr: Expr) -> Option<u32> {
    match expr {
        Expr::Repeat { body, .. }
        | Expr::Optional(body)
        | Expr::Label { body, .. }
        | Expr::And(body)
        | Expr::Not(body)
        | Expr::BackRef { body, .. } => Some(body),
        _ => None,
    }
}

impl<'r> Analysis<'r> {
    /// Sets up the analysis, allocating every FIRST set — or refuses the
    /// grammar, before allocating them, if they would not fit the budget.
    pub(crate) fn new(
        exprs: Vec<Expr>,
        items: Vec<u32>,
        spans: Vec<Span>,
        bits: usize,
        rules: usize,
        other_bytes: usize,
        report: &'r mut Report,
    ) -> Option<Self> {
        let mut sets = Sets::new(bits);
        // A set is shared wherever two FIRST sets are equal by construction,
        // so the tables grow with the grammar's choices, not its every
        // expression: every token expression of a kind shares one set; a rule
        // reference has its rule's set; `x*`, `x+`, `x?`, `label:x`, `&x`
        // and `x=label` have `x`'s; and a sequence that begins with a token
        // has that token's. Sharing is safe because the fixpoint only ever
        // unions into a set, and a set unioned with itself is unchanged.
        let same: Vec<Option<SameFirst>> = exprs
            .iter()
            .enumerate()
            .map(|(e, expr)| match *expr {
                Expr::Rule(r) => Some(SameFirst::Rule(r)),
                // Children sit before their parents.
                Expr::Repeat { body, .. }
                | Expr::Optional(body)
                | Expr::Label { body, .. }
                | Expr::And(body)
                | Expr::BackRef { body, .. }
                    if (body as usize) < e =>
                {
                    Some(SameFirst::Expr(body))
                }
                Expr::Seq { start, len } if len > 0 => {
                    let head = items[start as usize];
                    (matches!(exprs[head as usize], Expr::Token(_)) && (head as usize) < e)
                        .then_some(SameFirst::Expr(head))
                }
                _ => None,
            })
            .collect();
        let mut kinds_used = vec![false; bits];
        let mut needed = rules;
        for (expr, same) in exprs.iter().zip(&same) {
            match (*expr, same) {
                (Expr::Token(kind), _) => {
                    if !core::mem::replace(&mut kinds_used[kind as usize], true) {
                        needed += 1;
                    }
                }
                (_, Some(_)) => {}
                (_, None) => needed += 1,
            }
        }
        if other_bytes.saturating_add(sets.bytes_for(needed)) > MAX_TABLE_BYTES {
            too_large(report);
            return None;
        }

        let rule_first: Vec<SetId> = (0..rules).map(|_| sets.alloc()).collect();
        let mut singletons = vec![NO_SET; bits];
        let mut first: Vec<SetId> = Vec::with_capacity(exprs.len());
        for (expr, same) in exprs.iter().zip(&same) {
            let set = match (*expr, *same) {
                (Expr::Token(kind), _) => {
                    let slot = &mut singletons[kind as usize];
                    if *slot == NO_SET {
                        *slot = sets.alloc();
                        let _ = sets.insert(*slot, kind as usize);
                    }
                    *slot
                }
                (_, Some(SameFirst::Rule(r))) => rule_first[r as usize],
                (_, Some(SameFirst::Expr(c))) => first[c as usize],
                (_, None) => sets.alloc(),
            };
            first.push(set);
        }
        let nullable = vec![false; exprs.len()];
        let empty = vec![false; exprs.len()];
        Some(Self {
            exprs,
            items,
            spans,
            sets,
            first,
            nullable,
            empty,
            rule_first,
            rule_empty: vec![false; rules],
            other_bytes,
            report,
            follow: Vec::new(),
            rule_follow: Vec::new(),
            owned: Vec::new(),
            follow_all: false,
        })
    }

    pub(crate) fn children(&self, start: u32, len: u32) -> &[u32] {
        &self.items[start as usize..(start + len) as usize]
    }

    /// Computes FIRST, nullable, and the checks; `None` (reported) if the
    /// grammar is refused.
    pub(crate) fn analyse(
        &mut self,
        rules: &mut [Rule],
        pratts: &[Pratt],
        names: &[&str],
        name_spans: &[Span],
        tokens: &TokenFacts<'_>,
    ) -> Option<()> {
        for (rule, &first) in rules.iter_mut().zip(&self.rule_first) {
            rule.first = first;
        }
        // Token-like expressions whose FIRST sets are not a single kind.
        for e in 0..self.exprs.len() {
            match self.exprs[e] {
                // A contextual keyword is its own kind in FIRST sets: the
                // parser tests an `IDENT` against them by its text, so a
                // repetition of `'async'` does not commit on every name.
                Expr::Keyword(kind) => {
                    let set = self.first[e];
                    let _ = self.sets.insert(set, kind as usize);
                }
                Expr::Word => {
                    let set = self.first[e];
                    let _ = self.sets.insert(set, tokens.ident as usize);
                    for &k in tokens.keywords {
                        let _ = self.sets.insert(set, k as usize);
                    }
                }
                Expr::Eof => {
                    let set = self.first[e];
                    let _ = self.sets.insert(set, tokens.eof as usize);
                }
                Expr::BackRef { body, .. } => {
                    let set = self.first[e];
                    let body_first = self.first[body as usize];
                    let _ = self.sets.union(set, body_first);
                }
                _ => {}
            }
        }
        self.owned = self.rule_exprs(rules, pratts);
        let owned = core::mem::take(&mut self.owned);
        let order = self.rule_order(&owned);
        self.first_sets(rules, pratts, &owned, &order);
        let mut live = vec![false; self.exprs.len()];
        for &e in owned.iter().flatten() {
            live[e as usize] = true;
        }
        self.check(rules, pratts, &live, names, name_spans);
        self.owned = owned;
        self.report.is_clean().then_some(())
    }

    /// Computes FOLLOW and recovery sets and assembles the program.
    // The analysis's outputs, passed through once; a struct for them would
    // only be built here and taken apart below.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish(
        mut self,
        mut rules: Vec<Rule>,
        pratts: Vec<Pratt>,
        start: Option<u32>,
        shape: Shape,
        names: &[&str],
        name_spans: &[Span],
        rule_sync: &[Vec<u16>],
    ) -> Option<Program> {
        if self.owned.is_empty() && !rules.is_empty() {
            let facts = TokenFacts {
                eof: shape.eof,
                ident: shape.ident,
                keywords: &[],
            };
            self.analyse(&mut rules, &pratts, names, name_spans, &facts)?;
        }
        let start = start?;
        let owned = core::mem::take(&mut self.owned);
        let order = self.rule_order(&owned);
        let sync = self.follow_sets(&rules, &pratts, start, shape.eof, &owned, &order)?;
        self.owned = owned;
        for (rule, kinds) in rules.iter_mut().zip(rule_sync) {
            if !kinds.is_empty() {
                let set = self.sets.alloc();
                for &k in kinds {
                    let _ = self.sets.insert(set, k as usize);
                }
                rule.sync = set;
            }
        }
        Some(Program {
            exprs: self.exprs.into(),
            items: self.items.into(),
            first: self.first.into(),
            nullable: self.nullable.into(),
            rules: rules.into(),
            pratts: pratts.into(),
            sets: self.sets,
            sync,
            start,
            eof: shape.eof,
            error: shape.error,
            newline: shape.newline,
            ident: shape.ident,
            number: shape.number,
            strings: shape.strings,
            v2: shape.v2,
        })
    }

    /// Each rule's expressions, children before parents. Left-factoring
    /// leaves the shared prefixes of merged alternatives behind unused; they
    /// belong to no rule and are ignored from here on.
    fn rule_exprs(&self, rules: &[Rule], pratts: &[Pratt]) -> Vec<Vec<u32>> {
        rules
            .iter()
            .map(|rule| {
                let roots: Vec<u32> = match rule.body {
                    RuleBody::Expr(e) => vec![e],
                    RuleBody::Pratt(p) => {
                        let pratt = &pratts[p as usize];
                        core::iter::once(pratt.operand)
                            .chain(pratt.levels.iter().filter_map(|l| l.then))
                            .collect()
                    }
                };
                // Iterative post-order; the expressions form a tree.
                let mut out = Vec::new();
                let mut stack: Vec<(u32, bool)> = roots.iter().rev().map(|&r| (r, false)).collect();
                while let Some((e, expanded)) = stack.pop() {
                    if expanded {
                        out.push(e);
                        continue;
                    }
                    stack.push((e, true));
                    match self.exprs[e as usize] {
                        Expr::Seq { start, len } | Expr::Choice { start, len } => {
                            stack.extend(
                                self.children(start, len).iter().rev().map(|&c| (c, false)),
                            );
                        }
                        expr => {
                            if let Some(body) = body_of(expr) {
                                stack.push((body, false));
                            }
                        }
                    }
                }
                out
            })
            .collect()
    }

    /// The rules ordered callees first (a depth-first post-order over rule
    /// references). Visiting rules in this order lets FIRST information flow
    /// through a whole chain of rules in one pass, and FOLLOW information
    /// (visiting in reverse) the other way; only cycles need further passes.
    pub(crate) fn rule_order(&self, owned: &[Vec<u32>]) -> Vec<u32> {
        let callees: Vec<Vec<u32>> = owned
            .iter()
            .map(|list| {
                list.iter()
                    .filter_map(|&e| match self.exprs[e as usize] {
                        Expr::Rule(r) => Some(r),
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        let mut seen = vec![false; owned.len()];
        let mut order = Vec::with_capacity(owned.len());
        for root in 0..owned.len() {
            if core::mem::replace(&mut seen[root], true) {
                continue;
            }
            let mut stack: Vec<(u32, usize)> = vec![(root as u32, 0)];
            while let Some(&mut (rule, ref mut next)) = stack.last_mut() {
                match callees[rule as usize].get(*next) {
                    Some(&callee) => {
                        *next += 1;
                        if !core::mem::replace(&mut seen[callee as usize], true) {
                            stack.push((callee, 0));
                        }
                    }
                    None => {
                        order.push(rule);
                        let _ = stack.pop();
                    }
                }
            }
        }
        order
    }

    /// FIRST, nullable, and empty for every expression and rule, to a
    /// fixpoint.
    fn first_sets(
        &mut self,
        rules: &mut [Rule],
        pratts: &[Pratt],
        owned: &[Vec<u32>],
        order: &[u32],
    ) {
        loop {
            let mut changed = false;
            for &r in order {
                for &e in &owned[r as usize] {
                    changed |= self.update_first(e as usize, rules);
                }
                changed |= self.update_rule_first(r as usize, &mut rules[r as usize], pratts);
            }
            if !changed {
                return;
            }
        }
    }

    /// Recomputes one expression's FIRST set, nullability, and emptiness
    /// from its children; returns whether any grew.
    fn update_first(&mut self, e: usize, rules: &[Rule]) -> bool {
        let set = self.first[e];
        let mut changed = false;
        let (nullable, empty) = match self.exprs[e] {
            // Token sets are filled when they are created.
            Expr::Token(_) | Expr::Keyword(_) | Expr::Word => (false, false),
            Expr::Rule(r) => {
                let rule = &rules[r as usize];
                changed |= self.sets.union(set, rule.first);
                (rule.nullable, self.rule_empty[r as usize])
            }
            Expr::Seq { start, len } => {
                let mut all = true;
                for k in start..start + len {
                    let item = self.items[k as usize] as usize;
                    changed |= self.sets.union(set, self.first[item]);
                    if !self.nullable[item] {
                        all = false;
                        break;
                    }
                }
                let all_empty =
                    (start..start + len).all(|k| self.empty[self.items[k as usize] as usize]);
                (all, all_empty)
            }
            Expr::Choice { start, len } => {
                let mut any = false;
                let mut any_empty = false;
                for k in start..start + len {
                    let item = self.items[k as usize] as usize;
                    changed |= self.sets.union(set, self.first[item]);
                    any |= self.nullable[item];
                    any_empty |= self.empty[item];
                }
                (any, any_empty)
            }
            Expr::Repeat { body, min_one, .. } => {
                changed |= self.sets.union(set, self.first[body as usize]);
                (
                    !min_one || self.nullable[body as usize],
                    !min_one || self.empty[body as usize],
                )
            }
            Expr::Optional(body) => {
                changed |= self.sets.union(set, self.first[body as usize]);
                (true, true)
            }
            Expr::Label { body, .. } | Expr::BackRef { body, .. } => {
                changed |= self.sets.union(set, self.first[body as usize]);
                (self.nullable[body as usize], self.empty[body as usize])
            }
            // A positive predicate can only succeed where its body can begin,
            // so it takes the body's FIRST set (and nullability) for the
            // parser's pruning; it never consumes input.
            Expr::And(body) => {
                changed |= self.sets.union(set, self.first[body as usize]);
                (self.nullable[body as usize], true)
            }
            // `!x` and the line assertions can succeed before any token.
            Expr::Not(_) | Expr::LineStart | Expr::NlBefore => (true, true),
            // The end of the input is the `eof` bit, which the parser's FIRST
            // test also matches; set by the format-2 compiler.
            Expr::Eof => (false, true),
        };
        if nullable && !self.nullable[e] {
            self.nullable[e] = true;
            changed = true;
        }
        if empty && !self.empty[e] {
            self.empty[e] = true;
            changed = true;
        }
        changed
    }

    /// Recomputes a rule's FIRST set, nullability, and emptiness from its
    /// body.
    fn update_rule_first(&mut self, r: usize, rule: &mut Rule, pratts: &[Pratt]) -> bool {
        let mut changed = false;
        let (body_first, nullable, empty) = match rule.body {
            RuleBody::Expr(e) => (
                self.first[e as usize],
                self.nullable[e as usize],
                self.empty[e as usize],
            ),
            RuleBody::Pratt(p) => {
                let pratt = &pratts[p as usize];
                for (kind, level) in pratt.prefix.iter().enumerate() {
                    if *level != 0 {
                        changed |= self.sets.insert(rule.first, kind);
                    }
                }
                for &(kind, prefix, _) in pratt.contextual.iter() {
                    if prefix != 0 {
                        changed |= self.sets.insert(rule.first, kind as usize);
                    }
                }
                let operand = pratt.operand as usize;
                (
                    self.first[operand],
                    self.nullable[operand],
                    self.empty[operand],
                )
            }
        };
        changed |= self.sets.union(rule.first, body_first);
        if nullable && !rule.nullable {
            rule.nullable = true;
            changed = true;
        }
        if empty && !self.rule_empty[r] {
            self.rule_empty[r] = true;
            changed = true;
        }
        changed
    }

    /// Refuses what the parser could not run.
    fn check(
        &mut self,
        rules: &[Rule],
        pratts: &[Pratt],
        live: &[bool],
        names: &[&str],
        name_spans: &[Span],
    ) {
        for (e, &alive) in live.iter().enumerate() {
            if !alive {
                continue;
            }
            if let Expr::Repeat { body, min_one, .. } = self.exprs[e] {
                if self.empty[body as usize] {
                    let op = if min_one { '+' } else { '*' };
                    self.report.error_help(
                        codes::EMPTY_REPETITION,
                        self.spans[e],
                        format!("`{op}` repeats something that can match nothing, so it would never stop"),
                        "make the repeated part require at least one token",
                    );
                }
            }
        }
        for (rule, name) in rules.iter().zip(names) {
            if let RuleBody::Pratt(p) = rule.body {
                let operand = pratts[p as usize].operand as usize;
                if self.empty[operand] {
                    self.report.error(
                        codes::EMPTY_OPERAND,
                        self.spans[operand],
                        format!("the operand of expression rule `{name}` can match nothing"),
                    );
                }
            }
        }
        self.check_left_recursion(rules, pratts, names, name_spans);
    }

    /// Rules that can begin with themselves, directly or through other rules
    /// (ISSUES P10: linear). The "can begin with" graph is built once; its
    /// strongly connected components (Tarjan, iterative) are the cycles. Each
    /// component with a cycle is reported once, with one cycle through its
    /// smallest rule found by a breadth-first search inside the component.
    fn check_left_recursion(
        &mut self,
        rules: &[Rule],
        pratts: &[Pratt],
        names: &[&str],
        name_spans: &[Span],
    ) {
        let n = rules.len();
        let edges: Vec<Vec<u32>> = rules
            .iter()
            .map(|rule| {
                let root = match rule.body {
                    RuleBody::Expr(e) => e,
                    RuleBody::Pratt(p) => pratts[p as usize].operand,
                };
                let mut out = Vec::new();
                self.leftmost(root, &mut out);
                out.sort_unstable();
                out.dedup();
                out
            })
            .collect();

        // Tarjan's algorithm with an explicit stack.
        const UNSEEN: u32 = u32::MAX;
        let mut index = vec![UNSEEN; n];
        let mut low = vec![0u32; n];
        let mut on_stack = vec![false; n];
        let mut stack: Vec<u32> = Vec::new();
        let mut component = vec![UNSEEN; n];
        let mut components: Vec<Vec<u32>> = Vec::new();
        let mut next_index = 0u32;
        for root in 0..n {
            if index[root] != UNSEEN {
                continue;
            }
            let mut work: Vec<(u32, usize)> = vec![(root as u32, 0)];
            index[root] = next_index;
            low[root] = next_index;
            next_index += 1;
            stack.push(root as u32);
            on_stack[root] = true;
            while let Some(&mut (v, ref mut next)) = work.last_mut() {
                let v = v as usize;
                if let Some(&w) = edges[v].get(*next) {
                    *next += 1;
                    let w = w as usize;
                    if index[w] == UNSEEN {
                        index[w] = next_index;
                        low[w] = next_index;
                        next_index += 1;
                        stack.push(w as u32);
                        on_stack[w] = true;
                        work.push((w as u32, 0));
                    } else if on_stack[w] {
                        low[v] = low[v].min(index[w]);
                    }
                    continue;
                }
                let _ = work.pop();
                if let Some(&(parent, _)) = work.last() {
                    let p = parent as usize;
                    low[p] = low[p].min(low[v]);
                }
                if low[v] == index[v] {
                    let id = components.len() as u32;
                    let mut members = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w as usize] = false;
                        component[w as usize] = id;
                        members.push(w);
                        if w as usize == v {
                            break;
                        }
                    }
                    components.push(members);
                }
            }
        }

        let mut reports: Vec<(u32, Vec<u32>)> = Vec::new();
        for (id, members) in components.iter().enumerate() {
            let head = members.iter().copied().min().unwrap_or(0);
            let cyclic = members.len() > 1 || edges[head as usize].contains(&head);
            if !cyclic {
                continue;
            }
            // Shortest cycle through `head` within the component: a BFS from
            // `head` back to it, over edges that stay inside the component.
            let mut parent: BTreeMap<u32, u32> = BTreeMap::new();
            let mut queue = alloc::collections::VecDeque::from([head]);
            let mut closing = None;
            'search: while let Some(v) = queue.pop_front() {
                for &w in &edges[v as usize] {
                    if component[w as usize] != id as u32 {
                        continue;
                    }
                    if w == head {
                        closing = Some(v);
                        break 'search;
                    }
                    if w != head && !parent.contains_key(&w) {
                        let _ = parent.insert(w, v);
                        queue.push_back(w);
                    }
                }
            }
            let mut cycle = Vec::new();
            let mut at = closing.unwrap_or(head);
            while at != head {
                cycle.push(at);
                at = parent.get(&at).copied().unwrap_or(head);
            }
            cycle.push(head);
            cycle.reverse();
            reports.push((head, cycle));
        }
        reports.sort_by_key(|(head, _)| *head);
        for (head, cycle) in reports {
            let chain = cycle
                .iter()
                .chain(core::iter::once(&head))
                .map(|r| names[*r as usize])
                .collect::<Vec<_>>()
                .join(" → ");
            self.report.error_help(
                codes::LEFT_RECURSION,
                name_spans[head as usize],
                format!("rule `{}` is left-recursive: {chain}", names[head as usize]),
                "a rule cannot begin with itself; write lists as `item (',' item)*` and operators as an expression rule ([rules.name] with `levels`)",
            );
        }
    }

    /// The rules `e` can begin with — through predicates too, whose bodies
    /// are tried at the same position.
    fn leftmost(&self, e: u32, out: &mut Vec<u32>) {
        match self.exprs[e as usize] {
            Expr::Token(_)
            | Expr::Keyword(_)
            | Expr::Word
            | Expr::Eof
            | Expr::LineStart
            | Expr::NlBefore => {}
            Expr::Rule(r) => out.push(r),
            Expr::Seq { start, len } => {
                for &item in self.children(start, len) {
                    self.leftmost(item, out);
                    if !self.nullable[item as usize] && !self.empty[item as usize] {
                        break;
                    }
                }
            }
            Expr::Choice { start, len } => {
                for &item in self.children(start, len) {
                    self.leftmost(item, out);
                }
            }
            expr => {
                if let Some(body) = body_of(expr) {
                    self.leftmost(body, out);
                }
            }
        }
    }

    /// FOLLOW sets, and from them each repetition's stop set: the tokens that
    /// may legitimately come after it, at which recovery stops skipping.
    /// Returns the per-slot synchronization sets of every sequence.
    ///
    /// Token expressions need no FOLLOW set and get none, and neither does
    /// an expression with no rule reference or repetition inside it: FOLLOW
    /// is read only to pass it on to a rule and to stop a repetition, so
    /// such an expression's FOLLOW set would never be read. (Format 2's
    /// overlap check also reads every optional's.) The per-slot sets of
    /// sequences are materialized only for slots that need them. Predicate
    /// bodies are only ever tried strictly, so FOLLOW does not flow into
    /// them. Returns `None`, having reported it, if the sets would exceed the
    /// budget.
    fn follow_sets(
        &mut self,
        rules: &[Rule],
        pratts: &[Pratt],
        start: u32,
        eof: u16,
        owned: &[Vec<u32>],
        order: &[u32],
    ) -> Option<Box<[SetId]>> {
        let n = self.exprs.len();
        let is_token = |exprs: &[Expr], e: usize| {
            matches!(exprs[e], Expr::Token(_) | Expr::Keyword(_) | Expr::Word)
        };
        // Whether an expression's FOLLOW set is read: true of rule
        // references and repetitions, and of everything that contains one.
        // Each rule's list has children before parents.
        let mut needs = vec![false; n];
        for &e in owned.iter().flatten() {
            needs[e as usize] = match self.exprs[e as usize] {
                Expr::Token(_)
                | Expr::Keyword(_)
                | Expr::Word
                | Expr::Eof
                | Expr::LineStart
                | Expr::NlBefore
                | Expr::And(_)
                | Expr::Not(_) => false,
                Expr::Rule(_) | Expr::Repeat { .. } => true,
                Expr::Optional(body) => self.follow_all || needs[body as usize],
                Expr::Label { body, .. } | Expr::BackRef { body, .. } => needs[body as usize],
                Expr::Seq { start, len } | Expr::Choice { start, len } => {
                    self.children(start, len).iter().any(|&c| needs[c as usize])
                }
            };
        }

        // Count the sets before allocating any, against the budget.
        let mut count = rules.len() + pratts.len();
        for &e in owned.iter().flatten() {
            count += usize::from(needs[e as usize]);
            if let Expr::Seq { start, len } = self.exprs[e as usize] {
                for &item in self.children(start, len) {
                    if !is_token(&self.exprs, item as usize) {
                        // At most an after-first set and a sync set.
                        count += 1 + usize::from(needs[item as usize]);
                    }
                }
            }
        }
        let used = self.other_bytes.saturating_add(self.sets.bytes());
        if used.saturating_add(self.sets.bytes_for(count)) > MAX_TABLE_BYTES {
            too_large(self.report);
            return None;
        }

        let mut follow = vec![NO_SET; n];
        for &e in owned.iter().flatten() {
            if needs[e as usize] {
                follow[e as usize] = self.sets.alloc();
            }
        }
        let rule_follow: Vec<SetId> = (0..rules.len()).map(|_| self.sets.alloc()).collect();
        let _ = self.sets.insert(rule_follow[start as usize], eof as usize);

        // Per sequence slot that needs them: FIRST of what comes after it and
        // whether all of that can match nothing (for FOLLOW), and FIRST of
        // every later item (the synchronization set for recovery, needed by
        // every slot that is not a token).
        let mut after_first = vec![NO_SET; self.items.len()];
        let mut after_nullable = vec![false; self.items.len()];
        let mut sync = vec![NO_SET; self.items.len()];
        let mut rest = self.sets.scratch();
        let mut later = self.sets.scratch();
        for list in owned {
            for &e in list {
                let Expr::Seq { start, len } = self.exprs[e as usize] else {
                    continue;
                };
                Sets::clear_scratch(&mut rest);
                Sets::clear_scratch(&mut later);
                let mut nullable = true;
                for k in (start..start + len).rev() {
                    let item = self.items[k as usize] as usize;
                    if !is_token(&self.exprs, item) {
                        if needs[item] {
                            after_first[k as usize] = self.sets.alloc_from(&rest);
                            after_nullable[k as usize] = nullable;
                        }
                        if later.iter().any(|w| *w != 0) {
                            sync[k as usize] = self.sets.alloc_from(&later);
                        }
                    }
                    if !self.nullable[item] {
                        Sets::clear_scratch(&mut rest);
                        nullable = false;
                    }
                    self.sets.or_into(&mut rest, self.first[item]);
                    self.sets.or_into(&mut later, self.first[item]);
                }
            }
        }
        // Per expression rule: the operators that may follow an operand.
        let ops_after: Vec<SetId> = pratts
            .iter()
            .map(|pratt| {
                let set = self.sets.alloc();
                for (kind, level) in pratt.after.iter().enumerate() {
                    if *level != 0 {
                        let _ = self.sets.insert(set, kind);
                    }
                }
                set
            })
            .collect();

        // Callers first, so FOLLOW flows down a chain of rules in one pass.
        loop {
            let mut changed = false;
            for &r in order.iter().rev() {
                let rule = &rules[r as usize];
                let own_follow = rule_follow[r as usize];
                match rule.body {
                    RuleBody::Expr(e) => changed |= self.sets.join(follow[e as usize], own_follow),
                    RuleBody::Pratt(p) => {
                        let pratt = &pratts[p as usize];
                        let operand = follow[pratt.operand as usize];
                        changed |= self.sets.join(operand, ops_after[p as usize]);
                        changed |= self.sets.join(operand, own_follow);
                        for level in pratt.levels.iter() {
                            let Some(then) = level.then else { continue };
                            let then = follow[then as usize];
                            if level.fixity == Fixity::Postfix {
                                changed |= self.sets.join(then, ops_after[p as usize]);
                                changed |= self.sets.join(then, own_follow);
                            } else {
                                changed |= self.sets.join(then, rule.first);
                            }
                        }
                    }
                }
                for &e in owned[r as usize].iter().rev() {
                    let own = follow[e as usize];
                    match self.exprs[e as usize] {
                        Expr::Token(_)
                        | Expr::Keyword(_)
                        | Expr::Word
                        | Expr::Eof
                        | Expr::LineStart
                        | Expr::NlBefore
                        | Expr::And(_)
                        | Expr::Not(_) => {}
                        Expr::Rule(callee) => {
                            changed |= self.sets.join(rule_follow[callee as usize], own);
                        }
                        Expr::Seq { start, len } => {
                            for k in start..start + len {
                                let item = follow[self.items[k as usize] as usize];
                                changed |= self.sets.join(item, after_first[k as usize]);
                                if after_nullable[k as usize] {
                                    changed |= self.sets.join(item, own);
                                }
                            }
                        }
                        Expr::Choice { start, len } => {
                            for k in start..start + len {
                                let item = follow[self.items[k as usize] as usize];
                                changed |= self.sets.join(item, own);
                            }
                        }
                        Expr::Repeat { body, .. } => {
                            let body_follow = follow[body as usize];
                            changed |= self.sets.join(body_follow, self.first[body as usize]);
                            changed |= self.sets.join(body_follow, own);
                        }
                        Expr::Optional(body)
                        | Expr::Label { body, .. }
                        | Expr::BackRef { body, .. } => {
                            changed |= self.sets.join(follow[body as usize], own)
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for (expr, &stop) in self.exprs.iter_mut().zip(&follow) {
            if let Expr::Repeat { body, min_one, .. } = *expr {
                *expr = Expr::Repeat {
                    body,
                    min_one,
                    stop,
                };
            }
        }
        self.follow = follow;
        self.rule_follow = rule_follow;
        Some(sync.into())
    }
}
