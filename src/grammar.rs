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

use syntax_lang::Span;

use crate::{
    error::{Report, line_col},
    kind::{Kind, MAX_KINDS},
    lexer::{self, BUILTIN_TOKENS, FIRST_LITERAL, Lexer},
    rule::{self, Ast},
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
const MAX_TABLE_BYTES: usize = 256 << 20;

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

/// A forged language: identity, kinds, lexer, and parser tables.
#[derive(Clone, Debug)]
pub(crate) struct Grammar {
    pub(crate) name: Box<str>,
    pub(crate) version: Option<Box<str>>,
    pub(crate) extensions: Box<[Box<str>]>,
    pub(crate) capabilities: Box<[CapabilityRef]>,
    pub(crate) kinds: Kinds,
    pub(crate) lexer: Lexer,
    pub(crate) program: Program,
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
    names: Box<[Box<str>]>,
    values: Box<[Kind]>,
    /// Kind indexes ordered by name; `EOF` is left out (it is never in a tree).
    by_name: Box<[u16]>,
}

impl Kinds {
    /// The kind called `name`.
    pub(crate) fn get(&self, name: &str) -> Option<Kind> {
        self.by_name
            .binary_search_by(|&i| (*self.names[i as usize]).cmp(name))
            .ok()
            .map(|at| self.values[self.by_name[at] as usize])
    }

    /// The name of `kind`.
    pub(crate) fn name(&self, kind: Kind) -> &str {
        self.names.get(kind.index()).map_or("<unknown>", |n| n)
    }

    /// The name of the kind with index `index`.
    pub(crate) fn name_at(&self, index: usize) -> &str {
        self.names.get(index).map_or("<unknown>", |n| n)
    }

    /// The kind with index `index`.
    pub(crate) fn at(&self, index: usize) -> Kind {
        self.values[index]
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
}

impl Program {
    /// The children of a `Seq` or `Choice`.
    #[inline]
    pub(crate) fn children(&self, start: u32, len: u32) -> &[u32] {
        &self.items[start as usize..(start + len) as usize]
    }
}

/// A rule's parsed text.
enum Parsed<'t> {
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
        .map(|r| parse_rule(r, report))
        .collect();

    let mut rule_ids: BTreeMap<&str, u32> = BTreeMap::new();
    for (i, spec) in schematic.rules.iter().enumerate() {
        check_name(&spec.name, spec.name_span, "rule", true, report);
        let _ = rule_ids.insert(&spec.name, i as u32);
    }

    let literals = collect_literals(schematic, &parsed, &rule_ids, report);
    if FIRST_LITERAL as usize + literals.len() + 2 + schematic.rules.len() > MAX_KINDS {
        report.error(
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

    let mut builder = Builder {
        exprs: Vec::new(),
        items: Vec::new(),
        spans: Vec::new(),
        rule_ids: &rule_ids,
        literal_ids: &literal_ids,
        newlines: schematic.lexer.newlines,
        has_strings: !schematic.lexer.strings.is_empty(),
        factor_depth: 0,
        report,
    };

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
                let levels = builder.levels(pratt, &mut names, &mut node_ids, n_tokens);
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
    let kinds = kind_table(names, values, eof);
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
        .map(|(operand, thens, (levels, prefix, after))| Pratt {
            operand,
            prefix,
            after,
            levels: levels
                .into_iter()
                .zip(thens)
                .map(|(level, then)| Level { then, ..level })
                .collect(),
        })
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
        })
        .collect();
    let program = analysis.finish(rules, pratts, start, eof, error, schematic)?;

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
        lexer,
        program,
    })
}

fn parse_rule<'t>(spec: &'t RuleSpec<'_>, report: &mut Report) -> Parsed<'t> {
    match &spec.body {
        Body::Grammar(text, span) => {
            rule::parse(text, *span, report).map_or(Parsed::Failed, Parsed::Grammar)
        }
        Body::Pratt(pratt) => {
            let operand = rule::parse(&pratt.operand.0, pratt.operand.1, report);
            let mut failed = operand.is_none();
            let thens = pratt
                .levels
                .iter()
                .map(|level| {
                    level.then.as_ref().map(|(text, span)| {
                        let ast = rule::parse(text, *span, report);
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
            span,
            format!("`{name}` is not a valid {what} name"),
            "use letters, digits, and `_`, starting with a letter or `_`",
        );
    } else if RESERVED.contains(&name) {
        report.error(
            span,
            format!("`{name}` is the name of a built-in kind and cannot name a {what}"),
        );
    } else if !may_hide && name.starts_with('_') {
        report.error(span, format!("a {what} name cannot start with `_`"));
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
        report.error(span, "a literal cannot be empty");
        return false;
    };
    if text.contains(char::is_whitespace) {
        report.error_help(
            span,
            format!("literal `{text}` contains whitespace"),
            "write each word as its own literal",
        );
        return false;
    }
    if first.is_ascii_digit() {
        report.error(
            span,
            format!("literal `{text}` starts with a digit, which the lexer reads as a number"),
        );
        return false;
    }
    if lexer::is_ident_start(first, mode) && !lexer::is_ident(text, mode) {
        report.error_help(
            span,
            format!("literal `{text}` mixes identifier and symbol characters"),
            "split it into a keyword and a symbol",
        );
        return false;
    }
    if lexer::is_ident(text, mode) {
        if RESERVED.contains(&text) {
            report.error(
                span,
                format!("keyword `{text}` has the name of a built-in kind"),
            );
            return false;
        }
        if rule_ids.contains_key(text) {
            report.error_help(
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
        let help = suggest(name, rule_ids.keys().copied());
        match help {
            Some(help) => {
                report.error_help(span, format!("start rule `{name}` is not defined"), help)
            }
            None => report.error(span, format!("start rule `{name}` is not defined")),
        }
        return None;
    };
    if name.starts_with('_') {
        report.error_help(
            span,
            format!("the start rule `{name}` is hidden"),
            "the start rule's node is the root of every tree; remove the leading `_`",
        );
        return None;
    }
    Some(id)
}

/// "did you mean `x`?" for the closest name within a small edit distance.
fn suggest<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let limit = (name.chars().count() / 3).max(1);
    candidates
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| format!("did you mean `{c}`?"))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if ca == *cb {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

fn kind_table(names: Vec<Box<str>>, values: Vec<Kind>, eof: u16) -> Kinds {
    let mut by_name: Vec<u16> = (0..names.len() as u16).filter(|&i| i != eof).collect();
    by_name.sort_by(|&a, &b| names[a as usize].cmp(&names[b as usize]));
    Kinds {
        names: names.into(),
        values: values.into(),
        by_name: by_name.into(),
    }
}

/// Lowers rule ASTs into the expression arena.
struct Builder<'a, 'r> {
    exprs: Vec<Expr>,
    items: Vec<u32>,
    spans: Vec<Span>,
    rule_ids: &'a BTreeMap<&'a str, u32>,
    literal_ids: &'a BTreeMap<&'a str, u16>,
    newlines: bool,
    has_strings: bool,
    factor_depth: u32,
    report: &'r mut Report,
}

type LevelTables = (Vec<Level>, Box<[u8]>, Box<[u8]>);

impl Builder<'_, '_> {
    fn push(&mut self, expr: Expr, span: Span) -> u32 {
        self.exprs.push(expr);
        self.spans.push(span);
        self.exprs.len() as u32 - 1
    }

    fn lower(&mut self, ast: &Ast<'_>) -> Option<u32> {
        match ast {
            Ast::Literal(text, span) => {
                let kind = *self.literal_ids.get(text)?;
                Some(self.push(Expr::Token(kind), *span))
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
        }
    }

    fn resolve(&mut self, name: &str, span: Span) -> Option<u32> {
        if let Some(&id) = self.rule_ids.get(name) {
            return Some(self.push(Expr::Rule(id), span));
        }
        let class = match name {
            "IDENT" => lexer::IDENT,
            "NUMBER" => lexer::NUMBER,
            "STRING" if self.has_strings => lexer::STRING,
            "STRING" => {
                self.report.error_help(
                    span,
                    "the grammar uses STRING, but [lexer] declares no strings",
                    "add `strings = ['\"']` to [lexer]",
                );
                return None;
            }
            "NEWLINE" if self.newlines => lexer::NEWLINE,
            "NEWLINE" => {
                self.report.error_help(
                    span,
                    "the grammar uses NEWLINE, but line breaks are whitespace",
                    "set `newlines = true` in [lexer]",
                );
                return None;
            }
            "WHITESPACE" | "COMMENT" | "UNKNOWN" => {
                self.report.error(
                    span,
                    format!("`{name}` is trivia, which the parser never sees"),
                );
                return None;
            }
            "ERROR" | "EOF" => {
                self.report
                    .error(span, format!("`{name}` is not a token a rule can match"));
                return None;
            }
            _ => {
                let message = format!("undefined rule `{name}`");
                match suggest(name, self.rule_ids.keys().copied()) {
                    Some(help) => self.report.error_help(span, message, help),
                    None => self.report.error(span, message),
                }
                return None;
            }
        };
        Some(self.push(Expr::Token(class), span))
    }

    /// An expression that matches nothing: an empty choice.
    fn nothing(&mut self) -> u32 {
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
            (Expr::Token(x), Expr::Token(y)) => x == y,
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
            (Expr::Optional(x), Expr::Optional(y)) => self.same(x, y),
            _ => false,
        }
    }

    /// An expression rule's levels and operator tables.
    fn levels(
        &mut self,
        pratt: &crate::schematic::PrattSpec<'_>,
        names: &mut Vec<Box<str>>,
        node_ids: &mut BTreeMap<String, u16>,
        n_tokens: usize,
    ) -> Option<LevelTables> {
        if pratt.levels.len() > MAX_LEVELS {
            self.report.error(
                pratt.levels[MAX_LEVELS].span,
                format!("an expression rule has at most {MAX_LEVELS} levels"),
            );
            return None;
        }
        let mut prefix = vec![0u8; n_tokens].into_boxed_slice();
        let mut after = vec![0u8; n_tokens].into_boxed_slice();
        let mut levels = Vec::with_capacity(pratt.levels.len());
        let mut ok = true;
        for (i, spec) in pratt.levels.iter().enumerate() {
            let (name, span) = match &spec.node {
                Some((name, span)) => {
                    check_name(name, *span, "node", false, self.report);
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
            if let Some(&literal) = self.literal_ids.get(name.as_str()) {
                let _ = literal;
                self.report.error_help(
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
            let table = if spec.fixity == Fixity::Prefix {
                &mut prefix
            } else {
                &mut after
            };
            for (op, op_span) in &spec.operators {
                let Some(&kind) = self.literal_ids.get(&**op) else {
                    ok = false;
                    continue;
                };
                let slot = &mut table[kind as usize];
                if *slot != 0 {
                    let place = if spec.fixity == Fixity::Prefix {
                        "a prefix"
                    } else {
                        "an infix or postfix"
                    };
                    self.report.error(
                        *op_span,
                        format!("`{op}` is already {place} operator at an earlier level"),
                    );
                    ok = false;
                } else {
                    *slot = i as u8 + 1;
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
        ok.then_some((levels, prefix, after))
    }
}

/// Fixpoint analysis over the lowered rules.
struct Analysis<'r> {
    exprs: Vec<Expr>,
    items: Vec<u32>,
    spans: Vec<Span>,
    sets: Sets,
    first: Vec<SetId>,
    nullable: Vec<bool>,
    /// By rule: its FIRST set.
    rule_first: Vec<SetId>,
    /// Bytes of other tables counted against `MAX_TABLE_BYTES`.
    other_bytes: usize,
    report: &'r mut Report,
}

/// Where an expression's FIRST set comes from, when it is exactly another's.
#[derive(Clone, Copy)]
enum SameFirst {
    Expr(u32),
    Rule(u32),
}

/// Refuses a grammar whose tables would exceed `MAX_TABLE_BYTES`.
fn too_large(report: &mut Report) {
    report.error_help(
        Span::empty(0),
        format!(
            "the grammar's tables would need more than {} MiB",
            MAX_TABLE_BYTES >> 20
        ),
        "the parser's tables grow with the number of rules and alternatives times the number of distinct keywords and symbols",
    );
}

impl<'r> Analysis<'r> {
    /// Sets up the analysis, allocating every FIRST set — or refuses the
    /// grammar, before allocating them, if they would not fit the budget.
    fn new(
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
        // reference has its rule's set; `x*`, `x+`, and `x?` have `x`'s; and
        // a sequence that begins with a token has that token's. Sharing is
        // safe because the fixpoint only ever unions into a set, and a set
        // unioned with itself is unchanged.
        let same: Vec<Option<SameFirst>> = exprs
            .iter()
            .enumerate()
            .map(|(e, expr)| match *expr {
                Expr::Rule(r) => Some(SameFirst::Rule(r)),
                // Children sit before their parents.
                Expr::Repeat { body, .. } | Expr::Optional(body) if (body as usize) < e => {
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
        Some(Self {
            exprs,
            items,
            spans,
            sets,
            first,
            nullable,
            rule_first,
            other_bytes,
            report,
        })
    }

    fn children(&self, start: u32, len: u32) -> &[u32] {
        &self.items[start as usize..(start + len) as usize]
    }

    fn finish(
        mut self,
        mut rules: Vec<Rule>,
        pratts: Vec<Pratt>,
        start: Option<u32>,
        eof: u16,
        error: Kind,
        schematic: &Schematic<'_>,
    ) -> Option<Program> {
        for (rule, &first) in rules.iter_mut().zip(&self.rule_first) {
            rule.first = first;
        }
        let owned = self.rule_exprs(&rules, &pratts);
        let order = self.rule_order(&owned);
        self.first_sets(&mut rules, &pratts, &owned, &order);
        let mut live = vec![false; self.exprs.len()];
        for &e in owned.iter().flatten() {
            live[e as usize] = true;
        }
        self.check(&rules, &pratts, &live, schematic);
        if !self.report.is_clean() {
            return None;
        }
        let start = start?;
        let sync = self.follow_sets(&rules, &pratts, start, eof, &owned, &order)?;
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
            eof,
            error,
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
                        Expr::Repeat { body, .. } | Expr::Optional(body) => {
                            stack.push((body, false))
                        }
                        Expr::Token(_) | Expr::Rule(_) => {}
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
    fn rule_order(&self, owned: &[Vec<u32>]) -> Vec<u32> {
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

    /// FIRST and nullable for every expression and rule, to a fixpoint.
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
                changed |= self.update_rule_first(&mut rules[r as usize], pratts);
            }
            if !changed {
                return;
            }
        }
    }

    /// Recomputes one expression's FIRST set and nullability from its
    /// children; returns whether either grew.
    fn update_first(&mut self, e: usize, rules: &[Rule]) -> bool {
        let set = self.first[e];
        let mut changed = false;
        let nullable = match self.exprs[e] {
            // Token sets are filled when they are created.
            Expr::Token(_) => false,
            Expr::Rule(r) => {
                let rule = &rules[r as usize];
                changed |= self.sets.union(set, rule.first);
                rule.nullable
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
                all
            }
            Expr::Choice { start, len } => {
                let mut any = false;
                for k in start..start + len {
                    let item = self.items[k as usize] as usize;
                    changed |= self.sets.union(set, self.first[item]);
                    any |= self.nullable[item];
                }
                any
            }
            Expr::Repeat { body, min_one, .. } => {
                changed |= self.sets.union(set, self.first[body as usize]);
                !min_one || self.nullable[body as usize]
            }
            Expr::Optional(body) => {
                changed |= self.sets.union(set, self.first[body as usize]);
                true
            }
        };
        if nullable && !self.nullable[e] {
            self.nullable[e] = true;
            changed = true;
        }
        changed
    }

    /// Recomputes a rule's FIRST set and nullability from its body.
    fn update_rule_first(&mut self, rule: &mut Rule, pratts: &[Pratt]) -> bool {
        let mut changed = false;
        let (body_first, nullable) = match rule.body {
            RuleBody::Expr(e) => (self.first[e as usize], self.nullable[e as usize]),
            RuleBody::Pratt(p) => {
                let pratt = &pratts[p as usize];
                for (kind, level) in pratt.prefix.iter().enumerate() {
                    if *level != 0 {
                        changed |= self.sets.insert(rule.first, kind);
                    }
                }
                let operand = pratt.operand as usize;
                (self.first[operand], self.nullable[operand])
            }
        };
        changed |= self.sets.union(rule.first, body_first);
        if nullable && !rule.nullable {
            rule.nullable = true;
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
        schematic: &Schematic<'_>,
    ) {
        for (e, &alive) in live.iter().enumerate() {
            if !alive {
                continue;
            }
            if let Expr::Repeat { body, min_one, .. } = self.exprs[e] {
                if self.nullable[body as usize] {
                    let op = if min_one { '+' } else { '*' };
                    self.report.error_help(
                        self.spans[e],
                        format!("`{op}` repeats something that can match nothing, so it would never stop"),
                        "make the repeated part require at least one token",
                    );
                }
            }
        }
        for (rule, spec) in rules.iter().zip(&schematic.rules) {
            if let RuleBody::Pratt(p) = rule.body {
                let operand = pratts[p as usize].operand as usize;
                if self.nullable[operand] {
                    self.report.error(
                        self.spans[operand],
                        format!(
                            "the operand of expression rule `{}` can match nothing",
                            spec.name
                        ),
                    );
                }
            }
        }
        self.check_left_recursion(rules, pratts, schematic);
    }

    /// Rules that can begin with themselves, directly or through other rules.
    fn check_left_recursion(
        &mut self,
        rules: &[Rule],
        pratts: &[Pratt],
        schematic: &Schematic<'_>,
    ) {
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

        // Iterative depth-first search; a back edge closes a cycle.
        const NEW: u8 = 0;
        const OPEN: u8 = 1;
        const DONE: u8 = 2;
        let mut state = vec![NEW; rules.len()];
        let mut reported = vec![false; rules.len()];
        for root in 0..rules.len() {
            if state[root] != NEW {
                continue;
            }
            let mut path: Vec<(u32, usize)> = vec![(root as u32, 0)];
            state[root] = OPEN;
            while let Some(&mut (node, ref mut next)) = path.last_mut() {
                let Some(&target) = edges[node as usize].get(*next) else {
                    state[node as usize] = DONE;
                    let _ = path.pop();
                    continue;
                };
                *next += 1;
                match state[target as usize] {
                    NEW => {
                        state[target as usize] = OPEN;
                        path.push((target, 0));
                    }
                    OPEN => {
                        let from = path.iter().position(|(n, _)| *n == target).unwrap_or(0);
                        let cycle: Vec<u32> = path[from..].iter().map(|(n, _)| *n).collect();
                        if cycle.iter().any(|r| reported[*r as usize]) {
                            continue;
                        }
                        for r in &cycle {
                            reported[*r as usize] = true;
                        }
                        let head_at = cycle
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, r)| **r)
                            .map_or(0, |(i, _)| i);
                        let mut cycle = cycle;
                        cycle.rotate_left(head_at);
                        let head = cycle[0] as usize;
                        let chain = cycle
                            .iter()
                            .chain(core::iter::once(&cycle[0]))
                            .map(|r| &*schematic.rules[*r as usize].name)
                            .collect::<Vec<_>>()
                            .join(" → ");
                        self.report.error_help(
                            schematic.rules[head].name_span,
                            format!("rule `{}` is left-recursive: {chain}", schematic.rules[head].name),
                            "a rule cannot begin with itself; write lists as `item (',' item)*` and operators as an expression rule ([rules.name] with `levels`)",
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// The rules `e` can begin with.
    fn leftmost(&self, e: u32, out: &mut Vec<u32>) {
        match self.exprs[e as usize] {
            Expr::Token(_) => {}
            Expr::Rule(r) => out.push(r),
            Expr::Seq { start, len } => {
                for &item in self.children(start, len) {
                    self.leftmost(item, out);
                    if !self.nullable[item as usize] {
                        break;
                    }
                }
            }
            Expr::Choice { start, len } => {
                for &item in self.children(start, len) {
                    self.leftmost(item, out);
                }
            }
            Expr::Repeat { body, .. } | Expr::Optional(body) => self.leftmost(body, out),
        }
    }

    /// FOLLOW sets, and from them each repetition's stop set: the tokens that
    /// may legitimately come after it, at which recovery stops skipping.
    /// Returns the per-slot synchronization sets of every sequence.
    ///
    /// Token expressions need no FOLLOW set and get none, and neither does
    /// an expression with no rule reference or repetition inside it: FOLLOW
    /// is read only to pass it on to a rule and to stop a repetition, so
    /// such an expression's FOLLOW set would never be read. The per-slot sets
    /// of sequences are materialized only for slots that need them. Returns
    /// `None`, having reported it, if the sets would exceed the budget.
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
        let is_token = |exprs: &[Expr], e: usize| matches!(exprs[e], Expr::Token(_));
        // Whether an expression's FOLLOW set is read: true of rule
        // references and repetitions, and of everything that contains one.
        // Each rule's list has children before parents.
        let mut needs = vec![false; n];
        for &e in owned.iter().flatten() {
            needs[e as usize] = match self.exprs[e as usize] {
                Expr::Token(_) => false,
                Expr::Rule(_) | Expr::Repeat { .. } => true,
                Expr::Optional(body) => needs[body as usize],
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
                        Expr::Token(_) => {}
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
                        Expr::Optional(body) => {
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
        Some(sync.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_distance() {
        assert_eq!(edit_distance("expr", "expr"), 0);
        assert_eq!(edit_distance("exprr", "expr"), 1);
        assert_eq!(edit_distance("stmt", "smt"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn test_suggest_picks_closest_within_limit() {
        let names = ["expr", "stmt", "program"];
        assert_eq!(
            suggest("exprr", names.iter().copied()).as_deref(),
            Some("did you mean `expr`?")
        );
        assert_eq!(suggest("zzz", names.iter().copied()), None);
    }
}
