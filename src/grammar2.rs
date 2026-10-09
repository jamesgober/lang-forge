//! The format-2 grammar compiler: a checked LSF2 [`Schematic`] in, a
//! [`Grammar`] out.
//!
//! It shares the rule lowering, left-factoring, and fixpoint analysis of the
//! format-1 compiler (`grammar.rs`) and adds what format 2 brings: kinds
//! numbered deterministically (LSF2 §5.4), token classes, string classes and
//! the rules generated for those that build nodes (§9.5.5), contextual and
//! case-insensitive keywords, `WORD`, assertions, labels, predicates,
//! back-references, the mode-stack scanner, the repetition-overlap check
//! (§11.8, ISSUES M02), unused-rule and unused-token warnings, fields (§11.3),
//! `[ast]` supertypes, injections, and per-extension entry points.

use alloc::{
    borrow::Cow,
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    vec::Vec,
};

use syntax_lang::Span;

use crate::{
    codes,
    error::Report,
    grammar::{
        self, Analysis, Builder, CAT_BUILTIN, CAT_CLASS, CAT_EOF, CAT_LITERAL, CAT_NODE,
        CapabilityRef, Expr, Extra, Grammar, InjectionDef, Level, Lex, OPERATOR_LABELS, Parsed,
        Pratt, ProgramV2, Resolved, Rule, RuleBody, Shape, TokenFacts, V2Names,
    },
    kind::{Kind, MAX_KINDS},
    lexer,
    regex::Props,
    scan::{Build, Builtins, Scanner, StringKinds},
    schematic::{Body, RuleOptions, RuleSpec, Schematic},
    set::NO_SET,
    spec2::{self, Level as CheckLevel, NameKind, StringClass, V2},
};

/// The built-in token kinds of format 2, in numbering order (LSF2 §6.4).
pub(crate) const BUILTINS: [&str; 12] = [
    "WHITESPACE",
    "COMMENT",
    "DOC_COMMENT",
    "UNKNOWN",
    "IDENT",
    "NUMBER",
    "STRING",
    "NEWLINE",
    "INDENT",
    "DEDENT",
    "SHEBANG",
    "COLUMN_TRIVIA",
];
const K_WHITESPACE: u16 = 0;
const K_COMMENT: u16 = 1;
const K_DOC_COMMENT: u16 = 2;
const K_UNKNOWN: u16 = 3;
pub(crate) const K_IDENT: u16 = 4;
pub(crate) const K_NUMBER: u16 = 5;
pub(crate) const K_STRING: u16 = 6;
pub(crate) const K_NEWLINE: u16 = 7;
const K_INDENT: u16 = 8;
const K_DEDENT: u16 = 9;
const K_SHEBANG: u16 = 10;
const K_COLUMN_TRIVIA: u16 = 11;

/// Names reserved in format 2 besides the built-in kinds: rule-language
/// assertions and node kinds.
const RESERVED_EXTRA: [&str; 5] = ["WORD", "EOF", "LINE_START", "NL_BEFORE", "ERROR"];

/// A rule of the grammar: written in the sketch or generated for a string
/// class.
struct RuleRef<'a, 's> {
    name: &'a str,
    name_span: Span,
    body: &'a Body<'s>,
    options: &'a RuleOptions<'s>,
    generated: bool,
}

/// Compiles a format-2 schematic.
pub(crate) fn compile(
    schematic: &Schematic<'_>,
    locate: &dyn Fn(Span) -> (u32, u32),
    report: &mut Report,
) -> Option<Grammar> {
    let v2 = schematic.v2.as_deref()?;
    let lex = &v2.lexer;
    let errors_before = report.errors();
    let mut props = Props::default();

    // ----- token class names -----
    // Declared classes in numbering order: custom tokens, then string classes
    // with their generated kinds, then mode text classes (LSF2 §5.4).
    let mut class_names: Vec<(String, Span, ClassOrigin)> = Vec::new();
    for (i, t) in lex.tokens.iter().enumerate() {
        class_names.push((t.name.to_string(), t.name_span, ClassOrigin::Token(i)));
    }
    let mut string_kinds_pending: Vec<StringNames> = Vec::new();
    for c in &lex.classes {
        let names = StringNames::of(c);
        if c.is_node() {
            for n in names.tokens() {
                class_names.push((n, c.name_span, ClassOrigin::StringPart));
            }
        } else {
            class_names.push((c.name.to_string(), c.name_span, ClassOrigin::StringToken));
        }
        string_kinds_pending.push(names);
    }
    let mut text_modes: Vec<(String, String, Span)> = Vec::new();
    for m in &lex.modes {
        if let Some((text_name, span)) = &m.text {
            if spec2::check_name(text_name, NameKind::Class, *span, report) {
                if let Some(existing) = class_names.iter().find(|c| c.0 == **text_name) {
                    if !matches!(existing.2, ClassOrigin::Text) {
                        report.error(
                            codes::CLASS_NAME,
                            *span,
                            format!("text class `{text_name}` has the name of a declared class"),
                        );
                    }
                } else {
                    class_names.push((text_name.to_string(), *span, ClassOrigin::Text));
                }
                text_modes.push((m.name.to_string(), text_name.to_string(), *span));
            }
        }
    }
    {
        let mut seen: BTreeMap<&str, Span> = BTreeMap::new();
        for (name, span, origin) in &class_names {
            if BUILTINS.contains(&name.as_str()) || RESERVED_EXTRA.contains(&name.as_str()) {
                report.error(
                    codes::RESERVED,
                    *span,
                    format!(
                        "`{name}` is the name of a built-in kind and cannot name a token class"
                    ),
                );
            } else if let Some(first) = seen.insert(name, *span) {
                let code = if matches!(origin, ClassOrigin::StringPart) {
                    codes::GENERATED_NAME
                } else {
                    codes::CLASS_NAME
                };
                report.diagnostic(
                    diag_lang::Diagnostic::new(
                        diag_lang::Severity::Error,
                        format!("token class `{name}` is declared twice (or collides with a generated name)"),
                        diag_lang::Label::new(*span, "declared again here"),
                    )
                    .with_secondary(diag_lang::Label::new(first, "first declared here"))
                    .with_code(code),
                );
            }
        }
    }

    // ----- rules: written ones, then generated ones -----
    let mut generated: Vec<RuleSpec<'static>> = Vec::new();
    for (c, names) in lex.classes.iter().zip(&string_kinds_pending) {
        if !c.is_node() {
            continue;
        }
        let mut parts: Vec<String> = Vec::from([names.text.clone()]);
        if let Some(escape) = &names.escape {
            parts.push(escape.clone());
        }
        parts.extend(names.embedded.iter().cloned());
        if names.interp.is_some() {
            parts.push(format!("{}_INTERP", c.name));
        }
        let body = format!(
            "open:{} parts:({})* close:{}",
            names.open,
            parts.join(" | "),
            names.close
        );
        generated.push(generated_rule(&c.name, body, c.name_span));
        if let Some((open, close)) = &names.interp {
            let mut rules: Vec<&str> = Vec::new();
            for h in &c.interpolate {
                if !rules.contains(&&*h.rule.0) {
                    rules.push(&h.rule.0);
                }
            }
            let body = format!("open:{open} value:({}) close:{close}", rules.join(" | "));
            generated.push(generated_rule(
                &format!("{}_INTERP", c.name),
                body,
                c.name_span,
            ));
        }
    }
    let all: Vec<RuleRef<'_, '_>> = schematic
        .rules
        .iter()
        .map(|r| RuleRef {
            name: &r.name,
            name_span: r.name_span,
            body: &r.body,
            options: &r.options,
            generated: false,
        })
        .chain(generated.iter().map(|r| RuleRef {
            name: &r.name,
            name_span: r.name_span,
            body: &r.body,
            options: &r.options,
            generated: true,
        }))
        .collect();
    let user_rules = schematic.rules.len();

    let mut rule_ids: BTreeMap<&str, u32> = BTreeMap::new();
    for (i, r) in all.iter().enumerate() {
        if !r.generated {
            let _ = spec2::check_name(r.name, NameKind::Rule, r.name_span, report);
            if class_names.iter().any(|c| c.0 == r.name) {
                report.error(
                    codes::RULE_NAME,
                    r.name_span,
                    format!("rule `{}` has the name of a token class", r.name),
                );
            }
        }
        if rule_ids.insert(r.name, i as u32).is_some() && !r.generated {
            report.error(
                codes::RULE_NAME,
                r.name_span,
                format!("rule `{}` is defined twice", r.name),
            );
        }
    }

    let parsed: Vec<Parsed<'_>> = all.iter().map(|r| parse_rule_ref(r, report)).collect();

    // ----- literals -----
    let mut found: Vec<(&str, Span)> = Vec::new();
    for (r, parsed) in all.iter().zip(&parsed) {
        match (parsed, r.body) {
            (Parsed::Grammar(ast), _) => literals_of(ast, &mut found),
            (Parsed::Pratt { operand, thens }, Body::Pratt(pratt)) => {
                literals_of(operand, &mut found);
                for (level, then) in pratt.levels.iter().zip(thens) {
                    for (op, span) in &level.operators {
                        let is_class = class_names.iter().any(|c| c.0 == **op);
                        let is_builtin = BUILTINS.contains(&&**op);
                        if !is_class && !is_builtin {
                            found.push((op, *span));
                        }
                    }
                    if let Some(then) = then {
                        literals_of(then, &mut found);
                    }
                }
            }
            _ => {}
        }
    }
    for (kw, span) in &lex.keywords.reserved {
        found.push((kw, *span));
    }
    let mut literals: Vec<(&str, Span)> = Vec::new();
    {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (text, span) in found {
            if seen.insert(text) && check_literal(text, span, lex, &rule_ids, &class_names, report)
            {
                literals.push((text, span));
            }
        }
    }

    // Contextual keywords.
    let ident_mode = lex.ident.mode;
    let reserved: BTreeSet<&str> = lex.keywords.reserved.iter().map(|(t, _)| &**t).collect();
    let mut contextual: BTreeSet<&str> = BTreeSet::new();
    for (kw, span) in &lex.keywords.contextual {
        if !lexer::is_ident(kw, ident_mode) {
            report.error(
                codes::CONTEXTUAL_SHAPE,
                *span,
                format!(
                    "`{kw}` cannot be a contextual keyword: it does not read like an identifier"
                ),
            );
            continue;
        }
        if lex.keywords.default_contextual {
            report.warning(codes::CONTEXTUAL_REDUNDANT, *span, format!("`{kw}` is listed as contextual, which every keyword already is (`default = \"contextual\"`)"));
        }
        if reserved.contains(&**kw) {
            report.error(
                codes::CONTEXTUAL_SHAPE,
                *span,
                format!("`{kw}` is both reserved and contextual"),
            );
            continue;
        }
        let _ = contextual.insert(kw);
    }
    if lex.keywords.default_contextual {
        for (text, _) in &literals {
            if lexer::is_ident(text, ident_mode) && !reserved.contains(text) {
                let _ = contextual.insert(text);
            }
        }
    }

    // ----- kind numbering (LSF2 §5.4) -----
    let n_classes = class_names.len();
    let first_class = BUILTINS.len();
    let first_literal = first_class + n_classes;
    let n_tokens = first_literal + literals.len();
    let eof = n_tokens as u16;
    let error_index = n_tokens + 1;
    if n_tokens + 2 + all.len() > MAX_KINDS {
        report.error(
            codes::TOO_MANY_KINDS,
            Span::empty(0),
            format!("the language needs more than {MAX_KINDS} kinds"),
        );
        return None;
    }
    let mut names: Vec<Box<str>> = BUILTINS.iter().map(|n| Box::<str>::from(*n)).collect();
    names.extend(class_names.iter().map(|c| Box::<str>::from(c.0.as_str())));
    names.extend(literals.iter().map(|(t, _)| Box::<str>::from(*t)));
    names.push("EOF".into());
    names.push("ERROR".into());
    let class_index = |name: &str| {
        class_names
            .iter()
            .position(|c| c.0 == name)
            .map(|i| (first_class + i) as u16)
    };
    let literal_ids: BTreeMap<&str, u16> = literals
        .iter()
        .enumerate()
        .map(|(i, (t, _))| (*t, (first_literal + i) as u16))
        .collect();
    // Node kinds: written rules (not hidden), then operator nodes (assigned
    // while lowering), then generated nodes.
    let mut node_ids: BTreeMap<String, u16> = BTreeMap::new();
    let mut rule_nodes: Vec<Option<u16>> = vec_of(all.len(), None);
    for (i, r) in all.iter().enumerate().take(user_rules) {
        if r.name.starts_with('_') {
            continue;
        }
        let index = names.len() as u16;
        names.push(Box::from(r.name));
        let _ = node_ids.insert(r.name.to_string(), index);
        rule_nodes[i] = Some(index);
    }

    // ----- names a rule can use -----
    let mut v2names = V2Names::default();
    for (i, b) in BUILTINS.iter().enumerate() {
        let r = match *b {
            "WHITESPACE" | "COMMENT" | "DOC_COMMENT" | "UNKNOWN" | "SHEBANG" | "COLUMN_TRIVIA" => {
                Resolved::Trivia
            }
            "STRING" => continue,
            "NEWLINE" => {
                let significant = lex.newlines
                    || v2
                        .layout
                        .as_ref()
                        .is_some_and(|l| l.indent || l.newlines != spec2::NewlineMode::Trivia);
                if significant {
                    Resolved::Token(i as u16)
                } else {
                    Resolved::NotMatchable
                }
            }
            "INDENT" | "DEDENT" => {
                if v2.layout.as_ref().is_some_and(|l| l.indent) {
                    Resolved::Token(i as u16)
                } else {
                    Resolved::NotMatchable
                }
            }
            _ => Resolved::Token(i as u16),
        };
        let _ = v2names.classes.insert(b, r);
    }
    let _ = v2names.classes.insert("WORD", Resolved::Word);
    let _ = v2names.classes.insert("EOF", Resolved::Eof);
    let _ = v2names.classes.insert("LINE_START", Resolved::LineStart);
    let _ = v2names.classes.insert("NL_BEFORE", Resolved::NlBefore);
    let _ = v2names.classes.insert("ERROR", Resolved::NotMatchable);
    for (i, (name, _, origin)) in class_names.iter().enumerate() {
        let trivia = matches!(origin, ClassOrigin::Token(t) if lex.tokens[*t].trivia);
        let r = if trivia {
            Resolved::Trivia
        } else {
            Resolved::Token((first_class + i) as u16)
        };
        let _ = v2names.classes.entry(name.as_str()).or_insert(r);
    }
    for c in &lex.classes {
        if c.is_node() {
            if let Some(&id) = rule_ids.get(&*c.name) {
                let _ = v2names.classes.insert(&c.name, Resolved::Rule(id));
            }
        }
    }
    // `STRING` means every string class, in declaration order.
    for c in &lex.classes {
        if let Some(r @ (Resolved::Token(_) | Resolved::Rule(_))) = v2names.classes.get(&*c.name) {
            v2names.strings.push(*r)
        }
    }
    if !lex.strings_array.is_empty() {
        v2names.strings.push(Resolved::Token(K_STRING));
    }
    v2names.contextual = contextual.iter().copied().collect();

    // ----- lowering -----
    let mut builder = Builder::new(&rule_ids, &literal_ids, report);
    builder.v2 = Some(&v2names);
    let mut bodies = Vec::with_capacity(all.len());
    let mut pratt_specs = Vec::new();
    let mut op_labels = [0u16; 4];
    let mut have_op_labels = false;
    for (r, parsed) in all.iter().zip(&parsed) {
        builder.begin_rule();
        let body = match (parsed, r.body) {
            (Parsed::Grammar(ast), _) => builder.lower(ast).map(RuleBody::Expr),
            (Parsed::Pratt { operand, thens }, Body::Pratt(pratt)) => {
                if !have_op_labels {
                    for (slot, name) in op_labels.iter_mut().zip(OPERATOR_LABELS) {
                        *slot = builder.labels.intern(name).unwrap_or(0);
                    }
                    have_op_labels = true;
                }
                let operand = builder.lower(operand);
                let thens: Vec<Option<u32>> = thens
                    .iter()
                    .map(|t| t.as_ref().and_then(|t| builder.lower(t)))
                    .collect();
                let resolve_op = |op: &str| -> Option<(u16, bool)> {
                    if let Some(k) = class_index(op) {
                        return Some((k, false));
                    }
                    if let Some(i) = BUILTINS.iter().position(|b| *b == op) {
                        return Some((i as u16, false));
                    }
                    literal_ids.get(op).map(|&k| (k, contextual.contains(op)))
                };
                let levels =
                    builder.levels(pratt, &mut names, &mut node_ids, n_tokens, &resolve_op);
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
    // Generated nodes come last.
    for (i, r) in all.iter().enumerate().skip(user_rules) {
        let index = names.len() as u16;
        names.push(Box::from(r.name));
        let _ = node_ids.insert(r.name.to_string(), index);
        rule_nodes[i] = Some(index);
    }
    if names.len() > MAX_KINDS {
        builder.report.error(
            codes::TOO_MANY_KINDS,
            Span::empty(0),
            format!("the language needs more than {MAX_KINDS} kinds"),
        );
        return None;
    }

    // ----- start rule -----
    let start = start_rule(schematic, &rule_ids, user_rules, builder.report);
    let bodies: Vec<RuleBody> = bodies
        .into_iter()
        .map(|body| body.unwrap_or_else(|| RuleBody::Expr(builder.nothing())))
        .collect();

    // ----- kinds -----
    let newline_significant = lex.newlines
        || v2
            .layout
            .as_ref()
            .is_some_and(|l| l.indent || l.newlines != spec2::NewlineMode::Trivia);
    let values: Vec<Kind> = (0..names.len())
        .map(|i| {
            let trivia = match i {
                i if i < first_class => match i as u16 {
                    K_WHITESPACE | K_COMMENT | K_DOC_COMMENT | K_UNKNOWN | K_SHEBANG | K_COLUMN_TRIVIA => true,
                    K_NEWLINE => !newline_significant,
                    _ => false,
                },
                i if i < first_literal => matches!(class_names[i - first_class].2, ClassOrigin::Token(t) if lex.tokens[t].trivia),
                _ => false,
            };
            Kind::new(i as u16, trivia)
        })
        .collect();
    let cats: Vec<u8> = (0..names.len())
        .map(|i| match i {
            i if i < first_class => CAT_BUILTIN,
            i if i < first_literal => CAT_CLASS,
            i if i < n_tokens => CAT_LITERAL,
            i if i == n_tokens => CAT_EOF,
            _ => CAT_NODE,
        })
        .collect();
    let kinds = grammar::kind_table(names.clone(), values.clone(), cats, eof);
    let error = kinds.at(error_index);

    // ----- the scanner -----
    let kind_of = |i: usize| values[i];
    let class_kinds: Vec<Kind> = (0..lex.tokens.len())
        .map(|t| {
            let at = class_names
                .iter()
                .position(|c| matches!(c.2, ClassOrigin::Token(x) if x == t))
                .unwrap_or(0);
            kind_of(first_class + at)
        })
        .collect();
    let string_kinds: Vec<StringKinds> = lex
        .classes
        .iter()
        .zip(&string_kinds_pending)
        .map(|(c, n)| {
            let k = |name: &str| kind_of(class_index(name).map_or(K_UNKNOWN as usize, usize::from));
            if c.is_node() {
                StringKinds::Node {
                    open: k(&n.open),
                    text: k(&n.text),
                    escape: n.escape.as_deref().map(k),
                    embedded: n.embedded.iter().map(|e| k(e)).collect(),
                    interp_open: n.interp.as_ref().map(|(o, _)| k(o)),
                    interp_close: n.interp.as_ref().map(|(_, c)| k(c)),
                    close: k(&n.close),
                }
            } else {
                StringKinds::Token(k(&c.name))
            }
        })
        .collect();
    let text_kinds: BTreeMap<&str, Kind> = text_modes
        .iter()
        .filter_map(|(mode, text, _)| {
            Some((mode.as_str(), kind_of(usize::from(class_index(text)?))))
        })
        .collect();
    let lit_kinds: Vec<(&str, Kind, Span)> = literals
        .iter()
        .map(|(t, s)| (*t, kind_of(usize::from(literal_ids[t])), *s))
        .collect();
    let rule_names: BTreeSet<&str> = rule_ids.keys().copied().collect();
    let resolve = |text: &str, span: Span, report: &mut Report| -> Option<Kind> {
        tokref(
            text,
            span,
            &kinds,
            &literal_ids,
            &class_names,
            &rule_names,
            report,
        )
    };
    let is_contextual = |t: &str| contextual.contains(t);
    let build = Build {
        spec: lex,
        layout: v2.layout.as_ref(),
        literals: &lit_kinds,
        contextual: &is_contextual,
        builtins: Builtins {
            whitespace: kind_of(K_WHITESPACE as usize),
            comment: kind_of(K_COMMENT as usize),
            doc_comment: kind_of(K_DOC_COMMENT as usize),
            unknown: kind_of(K_UNKNOWN as usize),
            ident: kind_of(K_IDENT as usize),
            number: kind_of(K_NUMBER as usize),
            newline: kind_of(K_NEWLINE as usize),
            indent: kind_of(K_INDENT as usize),
            dedent: kind_of(K_DEDENT as usize),
            shebang: kind_of(K_SHEBANG as usize),
        },
        class_kinds: &class_kinds,
        string_kinds: &string_kinds,
        array_string: kind_of(K_STRING as usize),
        text_kinds: &text_kinds,
        resolve: &resolve,
    };
    let scanner = Scanner::build(&build, &mut props, builder.report);
    check_lexer_rules(lex, &literals, &rule_ids, builder.report);

    // ----- analysis -----
    let uses_lines = builder.uses_lines;
    let uses_backrefs = builder.uses_backrefs;
    let uses_predicates = builder.uses_predicates;
    let labels = core::mem::take(&mut builder.labels);
    let pratt_bytes = pratt_specs.len().saturating_mul(2 * n_tokens);
    if pratt_bytes > grammar::MAX_TABLE_BYTES {
        grammar::too_large(builder.report);
        return None;
    }
    let Builder {
        exprs,
        items,
        spans,
        report,
        ..
    } = builder;
    let mut analysis = Analysis::new(
        exprs,
        items,
        spans,
        n_tokens + 1,
        all.len(),
        pratt_bytes,
        report,
    )?;
    analysis.follow_all = true;
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
    let mut rules: Vec<Rule> = all
        .iter()
        .zip(&bodies)
        .zip(&rule_nodes)
        .map(|((r, body), node)| Rule {
            name: Box::from(r.name.trim_start_matches('_')),
            node: node.map(|i| kinds.at(i as usize)),
            body: *body,
            first: 0,
            nullable: false,
            sync: NO_SET,
        })
        .collect();
    let rule_name_list: Vec<&str> = all.iter().map(|r| r.name).collect();
    let rule_spans: Vec<Span> = all.iter().map(|r| r.name_span).collect();
    let keyword_kinds: Vec<u16> = literals
        .iter()
        .filter(|(t, _)| lexer::is_ident(t, ident_mode))
        .map(|(t, _)| literal_ids[t])
        .collect();
    let facts = TokenFacts {
        eof,
        ident: K_IDENT,
        keywords: &keyword_kinds,
    };
    analysis.analyse(&mut rules, &pratts, &rule_name_list, &rule_spans, &facts)?;
    let scanner = scanner?;
    if analysis.report.errors() > errors_before {
        return None;
    }
    let start = start?;

    // Recovery sync tokens per rule.
    let mut rule_sync: Vec<Vec<u16>> = vec_of(all.len(), Vec::new());
    for (i, r) in all.iter().enumerate() {
        for (tokref_text, span) in &r.options.sync {
            if let Some(k) = tokref(
                tokref_text,
                *span,
                &kinds,
                &literal_ids,
                &class_names,
                &rule_names,
                analysis.report,
            ) {
                rule_sync[i].push(k.index());
            }
        }
    }

    let mut contextual_list: Vec<(Box<str>, u16)> = contextual
        .iter()
        .filter_map(|t| {
            let text = if lex.keywords.case_insensitive {
                t.to_ascii_lowercase()
            } else {
                String::from(*t)
            };
            Some((Box::<str>::from(text.as_str()), *literal_ids.get(t)?))
        })
        .collect();
    contextual_list.sort();
    let shape = Shape {
        eof,
        error,
        newline: K_NEWLINE,
        ident: K_IDENT,
        number: K_NUMBER,
        strings: string_token_kinds(&string_kinds, kind_of(K_STRING as usize)),
        v2: Some(Box::new(ProgramV2 {
            contextual: contextual_list.into(),
            case_insensitive: lex.keywords.case_insensitive,
            op_labels,
            lines: uses_lines,
            backrefs: uses_backrefs,
            predicates: uses_predicates,
        })),
    };

    // Entry points that count as uses of a rule.
    let mut roots: Vec<u32> = Vec::from([start]);
    for c in &lex.classes {
        for h in &c.interpolate {
            if let Some(&id) = rule_ids.get(&*h.rule.0) {
                roots.push(id);
            } else {
                analysis.report.error(
                    codes::UNDEFINED,
                    h.rule.1,
                    format!("interpolation rule `{}` is not defined", h.rule.0),
                );
            }
        }
        for e in &c.embedded {
            if let Some((rule, span)) = &e.parse {
                match rule_ids.get(&**rule) {
                    Some(&id) => roots.push(id),
                    None => analysis.report.error(
                        codes::UNDEFINED,
                        *span,
                        format!("`parse` rule `{rule}` is not defined"),
                    ),
                }
            }
        }
    }
    let files = resolve_files(v2, &scanner, &rule_ids, analysis.report, &mut roots);
    let injections = resolve_injections(
        v2,
        &kinds,
        &rule_ids,
        &literal_ids,
        &class_names,
        &rule_names,
        &labels,
        start,
        analysis.report,
        &mut roots,
    );

    // Overlap check (M02), then warnings.
    if v2.checks.overlap != CheckLevel::Allow {
        let allowed: Vec<bool> = all.iter().map(|r| r.options.allow_overlap).collect();
        crate::overlap::check(
            &mut analysis,
            &rules,
            &pratts,
            &allowed,
            &kinds,
            v2.checks.overlap == CheckLevel::Deny,
            eof,
        );
    }
    if v2.checks.unused_rule != CheckLevel::Allow {
        unused_rules(
            &mut analysis,
            &all,
            &roots,
            v2.checks.unused_rule == CheckLevel::Deny,
        );
    }
    if v2.checks.unused_token != CheckLevel::Allow {
        unused_tokens(
            &mut analysis,
            &pratts,
            lex,
            &class_names,
            first_class,
            &scanner,
            v2.checks.unused_token == CheckLevel::Deny,
        );
    }
    if analysis.report.errors() > errors_before {
        return None;
    }

    let Ok(fields) = crate::fields::derive(&analysis, &rules, &pratts, &shape, labels.names.len())
    else {
        analysis.report.error_help(
            codes::TABLES_TOO_LARGE,
            Span::empty(0),
            "the grammar's field tables would pass their budget",
            "fields grow with the labelled parts of the grammar times the kinds they can hold; a hidden rule over a huge choice, used in many places, multiplies them",
        );
        return None;
    };
    let supertypes = resolve_supertypes(v2, &kinds, analysis.report);
    let embedded_parse: Vec<(u16, u32)> = lex
        .classes
        .iter()
        .zip(&string_kinds)
        .flat_map(|(c, k)| {
            let kinds: Vec<Kind> = match k {
                StringKinds::Node { embedded, .. } => embedded.clone(),
                StringKinds::Token(_) => Vec::new(),
            };
            c.embedded
                .iter()
                .zip(kinds)
                .filter_map(|(e, kind)| {
                    let (rule, _) = e.parse.as_ref()?;
                    Some((kind.index(), *rule_ids.get(&**rule)?))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let program = analysis.finish(
        rules,
        pratts,
        Some(start),
        shape,
        &rule_name_list,
        &rule_spans,
        &rule_sync,
    )?;
    let capabilities = schematic
        .capabilities
        .iter()
        .map(|(name, span)| {
            let (line, column) = locate(*span);
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
        lexer: Lex::V2(Box::new(scanner)),
        program,
        extra: Some(Box::new(Extra {
            display_name: v2.display_name.as_deref().map(Box::from),
            description: v2.description.as_deref().map(Box::from),
            edition: v2.edition.as_deref().map(Box::from),
            shebang_names: v2.shebang_names.iter().map(|s| Box::from(&**s)).collect(),
            labels: labels.names.into(),
            fields,
            supertypes,
            injections,
            files,
            embedded_parse: embedded_parse.into(),
            warnings: Box::new([]),
        })),
    })
}

/// Where a token class name came from.
#[derive(Clone, Copy, Debug)]
enum ClassOrigin {
    /// `[lexer.tokens]` entry `n`.
    Token(usize),
    /// A string class that is one token.
    StringToken,
    /// A token generated for a string class (or one of its embedded tokens).
    StringPart,
    /// A mode's text class.
    Text,
}

/// The generated names of a string class.
struct StringNames {
    open: String,
    text: String,
    escape: Option<String>,
    embedded: Vec<String>,
    interp: Option<(String, String)>,
    close: String,
}

impl StringNames {
    fn of(c: &StringClass<'_>) -> Self {
        let n = &c.name;
        Self {
            open: format!("{n}_OPEN"),
            text: format!("{n}_TEXT"),
            escape: c.escape_tokens.then(|| format!("{n}_ESCAPE")),
            embedded: c.embedded.iter().map(|e| e.token.0.to_string()).collect(),
            interp: (!c.interpolate.is_empty())
                .then(|| (format!("{n}_INTERP_OPEN"), format!("{n}_INTERP_CLOSE"))),
            close: format!("{n}_CLOSE"),
        }
    }

    /// The token kinds, in numbering order.
    fn tokens(&self) -> Vec<String> {
        let mut out = Vec::from([self.open.clone(), self.text.clone()]);
        out.extend(self.escape.iter().cloned());
        out.extend(self.embedded.iter().cloned());
        if let Some((o, c)) = &self.interp {
            out.push(o.clone());
            out.push(c.clone());
        }
        out.push(self.close.clone());
        out
    }
}

fn generated_rule(name: &str, body: String, span: Span) -> RuleSpec<'static> {
    RuleSpec {
        name: Cow::Owned(String::from(name)),
        name_span: span,
        body: Body::Grammar(
            crate::noml::Text {
                text: Cow::Owned(body),
                start: span.start().to_u32(),
                exact: false,
                multiline: false,
            },
            span,
        ),
        options: RuleOptions::default(),
    }
}

fn parse_rule_ref<'a>(r: &'a RuleRef<'a, '_>, report: &mut Report) -> Parsed<'a> {
    match r.body {
        Body::Grammar(text, span) => {
            crate::rule::parse(text, *span, true, report).map_or(Parsed::Failed, Parsed::Grammar)
        }
        Body::Pratt(pratt) => {
            let operand = crate::rule::parse(&pratt.operand.0, pratt.operand.1, true, report);
            let mut failed = operand.is_none();
            let thens = pratt
                .levels
                .iter()
                .map(|level| {
                    level.then.as_ref().and_then(|(text, span)| {
                        let ast = crate::rule::parse(text, *span, true, report);
                        failed |= ast.is_none();
                        ast
                    })
                })
                .collect();
            match operand {
                Some(operand) if !failed => Parsed::Pratt { operand, thens },
                _ => Parsed::Failed,
            }
        }
    }
}

/// Every literal of `ast`, in text order.
fn literals_of<'a>(ast: &crate::rule::Ast<'a>, found: &mut Vec<(&'a str, Span)>) {
    ast.walk(&mut |node| {
        if let crate::rule::Ast::Literal(text, span) = node {
            found.push((*text, *span));
        }
    });
}

fn vec_of<T: Clone>(n: usize, value: T) -> Vec<T> {
    alloc::vec![value; n]
}

/// Whether a grammar literal can be lexed as one token of its own and is
/// valid literal text (LSF2 §26: at most 64 bytes, no control or bidi
/// characters).
fn check_literal(
    text: &str,
    span: Span,
    lex: &spec2::Lexer2<'_>,
    rule_ids: &BTreeMap<&str, u32>,
    classes: &[(String, Span, ClassOrigin)],
    report: &mut Report,
) -> bool {
    let mode = lex.ident.mode;
    let Some(first) = text.chars().next() else {
        report.error(codes::LITERAL_SHAPE, span, "a literal cannot be empty");
        return false;
    };
    if text.len() > 64 || text.chars().any(spec2::is_forbidden_text_char) {
        report.error(
            codes::LITERAL_TEXT,
            span,
            format!("literal `{}` is longer than 64 bytes or contains a control or bidirectional character", text.escape_debug()),
        );
        return false;
    }
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
    let ident_start = lexer::is_ident_start(first, mode) || lex.ident.extra_start.contains(&first);
    if ident_start
        && !lexer::is_ident(text, mode)
        && !text.chars().all(|c| {
            lexer::is_ident_continue(c, mode)
                || lex.ident.extra_continue.contains(&c)
                || lex.ident.extra_start.contains(&c)
        })
    {
        report.error_help(
            codes::LITERAL_SHAPE,
            span,
            format!("literal `{text}` mixes identifier and symbol characters"),
            "split it into a keyword and a symbol",
        );
        return false;
    }
    if lexer::is_ident(text, mode) {
        if BUILTINS.contains(&text) || RESERVED_EXTRA.contains(&text) {
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
        if classes.iter().any(|c| c.0 == text) {
            report.error(
                codes::LITERAL_IS_CLASS,
                span,
                format!("`{text}` is both a keyword and a token class"),
            );
            return false;
        }
    }
    true
}

/// The start rule: `start` in `[language]`, or the first written rule.
fn start_rule(
    schematic: &Schematic<'_>,
    rule_ids: &BTreeMap<&str, u32>,
    user_rules: usize,
    report: &mut Report,
) -> Option<u32> {
    let (name, span) = match &schematic.start {
        Some((name, span)) => (&**name, *span),
        None => {
            let first = schematic.rules.first()?;
            (&*first.name, first.name_span)
        }
    };
    let Some(&id) = rule_ids.get(name).filter(|&&id| (id as usize) < user_rules) else {
        let help = report.suggest(name, rule_ids.keys().copied(), (name.len() / 3).max(1));
        let message = format!("start rule `{name}` is not defined");
        match help {
            Some(help) => report.error_help(codes::UNDEFINED, span, message, help),
            None => report.error(codes::UNDEFINED, span, message),
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

/// Resolves a token or kind reference (LSF2 §6.3).
fn tokref(
    text: &str,
    span: Span,
    kinds: &grammar::Kinds,
    literal_ids: &BTreeMap<&str, u16>,
    classes: &[(String, Span, ClassOrigin)],
    rules: &BTreeSet<&str>,
    report: &mut Report,
) -> Option<Kind> {
    if let Some(inner) = text
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .filter(|t| !t.is_empty())
    {
        return match literal_ids.get(inner) {
            Some(&k) => Some(kinds.at(k as usize)),
            None => {
                report.error(
                    codes::UNKNOWN_LITERAL,
                    span,
                    format!("`{inner}` is not a token of this language"),
                );
                None
            }
        };
    }
    if let Some(node) = text.strip_prefix("kind:") {
        return match kinds
            .all_named(node)
            .find(|k| kinds.cats[k.slot()] == CAT_NODE)
        {
            Some(k) => Some(k),
            None => {
                report.error(
                    codes::UNDEFINED,
                    span,
                    format!("`kind:{node}` names no node kind"),
                );
                None
            }
        };
    }
    let class_shaped = text.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if class_shaped && (BUILTINS.contains(&text) || classes.iter().any(|c| c.0 == text)) {
        return kinds.get(text);
    }
    if let Some(&k) = literal_ids.get(text) {
        if rules.contains(text) || classes.iter().any(|c| c.0 == text) {
            report.error(codes::AMBIGUOUS_REF, span, format!("`{text}` is a rule or class; write `kind:{text}` for the node or `'{text}'` for the literal"));
            return None;
        }
        return Some(kinds.at(k as usize));
    }
    if rules.contains(text) {
        report.error(
            codes::AMBIGUOUS_REF,
            span,
            format!(
                "`{text}` is a rule; write `kind:{text}` for the node or `'{text}'` for a literal"
            ),
        );
        return None;
    }
    report.error(
        codes::UNKNOWN_LITERAL,
        span,
        format!("`{text}` is not a token of this language"),
    );
    None
}

/// Lexer checks that need the grammar: stop texts, interpolation closes, and
/// `trailing_dot` warnings.
fn check_lexer_rules(
    lex: &spec2::Lexer2<'_>,
    literals: &[(&str, Span)],
    _rule_ids: &BTreeMap<&str, u32>,
    report: &mut Report,
) {
    if lex.numbers.trailing_dot {
        for (text, span) in literals {
            if let Some(rest) = text.strip_prefix('.') {
                let message = if rest.is_empty() {
                    String::from(
                        "with `trailing_dot`, a `.` right after a number is part of it: `1.x` lexes as `1.` then `x`",
                    )
                } else {
                    format!("with `trailing_dot`, `1{text}` lexes as `1.` then `{rest}`")
                };
                report.warning(codes::TRAILING_DOT, *span, message);
            }
        }
    }
    for c in &lex.classes {
        for h in &c.interpolate {
            if !literals.iter().any(|(t, _)| *t == h.close) {
                report.warning(
                    codes::HOLE_CLOSE,
                    h.span,
                    format!("hole close `{}` is not a literal of the grammar, so nothing inside the hole can open or close a bracket around it", h.close),
                );
            }
        }
    }
}

/// The string token kinds the parser describes as "a string".
fn string_token_kinds(kinds: &[StringKinds], string: Kind) -> Box<[u16]> {
    let mut out: Vec<u16> = kinds
        .iter()
        .filter_map(|k| match k {
            StringKinds::Token(k) => Some(k.index()),
            StringKinds::Node { .. } => None,
        })
        .collect();
    out.push(string.index());
    out.sort_unstable();
    out.dedup();
    out.into()
}

fn resolve_files(
    v2: &V2<'_>,
    scanner: &Scanner,
    rule_ids: &BTreeMap<&str, u32>,
    report: &mut Report,
    roots: &mut Vec<u32>,
) -> Box<[(Box<str>, u16, u32)]> {
    let mut out = Vec::new();
    for f in &v2.files {
        let mode = match &f.mode {
            Some((m, s)) => match scanner.mode_named(m) {
                Some(id) => id,
                None => {
                    report.error(
                        codes::FILES_ENTRY,
                        *s,
                        format!("`files` names mode `{m}`, which is not declared"),
                    );
                    continue;
                }
            },
            None => scanner.initial,
        };
        let start = match &f.start {
            Some((r, s)) => {
                match rule_ids.get(&**r) {
                    Some(&id) if !r.starts_with('_') => id,
                    _ => {
                        report.error(codes::FILES_ENTRY, *s, format!("`files` names start rule `{r}`, which is not a defined, visible rule"));
                        continue;
                    }
                }
            }
            None => u32::MAX,
        };
        if start != u32::MAX {
            roots.push(start);
        }
        out.push((Box::from(&*f.extension.0), mode, start));
    }
    out.into()
}

#[allow(clippy::too_many_arguments)]
fn resolve_injections(
    v2: &V2<'_>,
    kinds: &grammar::Kinds,
    rule_ids: &BTreeMap<&str, u32>,
    literal_ids: &BTreeMap<&str, u16>,
    classes: &[(String, Span, ClassOrigin)],
    rules: &BTreeSet<&str>,
    labels: &grammar::Labels,
    start: u32,
    report: &mut Report,
    roots: &mut Vec<u32>,
) -> Box<[InjectionDef]> {
    let mut out = Vec::new();
    for inj in &v2.injections {
        let (target, span) = (&*inj.target.0, inj.target.1);
        // `rule.label` names a field; anything else is a kind reference.
        let (kind, label) = match target
            .split_once('.')
            .filter(|(r, l)| rules.contains(r) && spec2::is_name(l, NameKind::Label))
        {
            Some((rule, label)) => {
                let Some(node) = kinds
                    .all_named(rule)
                    .find(|k| kinds.cats[k.slot()] == CAT_NODE)
                else {
                    report.error(
                        codes::INJECTION_TARGET,
                        span,
                        format!("injection target `{target}` names no node kind"),
                    );
                    continue;
                };
                let Some(&id) = labels.ids.get(label) else {
                    report.error(
                        codes::INJECTION_TARGET,
                        span,
                        format!("injection target `{target}` names no field"),
                    );
                    continue;
                };
                (node.index(), Some(id))
            }
            None => match tokref(target, span, kinds, literal_ids, classes, rules, report) {
                Some(k) => (k.index(), None),
                None => continue,
            },
        };
        let language = &*inj.language.0;
        let start_rule = match &inj.start {
            Some((r, s)) => match rule_ids.get(&**r) {
                Some(&id) => id,
                None => {
                    report.error(
                        codes::UNDEFINED,
                        *s,
                        format!("injection start rule `{r}` is not defined"),
                    );
                    continue;
                }
            },
            None => start,
        };
        if language == "self" && !inj.editor {
            if inj.combined {
                report.error_help(
                    codes::NOT_SUPPORTED,
                    inj.span,
                    "combined self-injections are not supported by lang-forge 2.0.0-alpha.1",
                    "parse each range on its own (`combined = false`), or resolve it in the editor (ROADMAP: alpha.2)",
                );
                continue;
            }
            roots.push(start_rule);
        } else if language != "self" && !spec2::is_name(language, NameKind::Language) {
            report.error(
                codes::INJECTION_LANGUAGE,
                inj.language.1,
                format!("injected language `{language}` is not a language name"),
            );
            continue;
        }
        if let Some((when, s)) = &inj.when {
            report.error_help(
                codes::NOT_SUPPORTED,
                *s,
                format!("injection `when` (`{when}`) is not supported by lang-forge 2.0.0-alpha.1"),
                "content-conditional injections are ROADMAP: lang-forge alpha.2",
            );
            continue;
        }
        let inner = inj
            .inner
            .unwrap_or_else(|| kinds.cats.get(usize::from(kind)) == Some(&CAT_CLASS));
        out.push(InjectionDef {
            id: Box::from(&*inj.id),
            kind,
            label,
            language: Box::from(language),
            editor: inj.editor,
            start: start_rule,
            inner,
            combined: inj.combined,
            scope: inj.scope.as_deref().map(Box::from),
        });
    }
    out.into()
}

fn resolve_supertypes(
    v2: &V2<'_>,
    kinds: &grammar::Kinds,
    report: &mut Report,
) -> grammar::Supertypes {
    let n = v2.ast.len();
    let names: BTreeMap<&str, usize> = v2
        .ast
        .iter()
        .enumerate()
        .map(|(i, s)| (&*s.name, i))
        .collect();
    for s in &v2.ast {
        if kinds.get(&s.name).is_some() {
            report.error(
                codes::SUPERTYPE_NAME,
                s.span,
                format!("supertype `{}` has the name of a kind", s.name),
            );
        }
    }
    // Expand supertypes of supertypes, depth first with an explicit stack;
    // each supertype is expanded once, and a cycle is refused.
    const NEW: u8 = 0;
    const OPEN: u8 = 1;
    let mut state = vec_of(n, NEW);
    let mut result: Vec<Vec<u16>> = vec_of(n, Vec::new());
    for root in 0..n {
        if state[root] != NEW {
            continue;
        }
        state[root] = OPEN;
        let mut stack: Vec<(usize, usize)> = Vec::from([(root, 0)]);
        while let Some(&(s, next)) = stack.last() {
            let members = &v2.ast[s].members;
            if next == members.len() {
                let _ = stack.pop();
                result[s].sort_unstable();
                result[s].dedup();
                state[s] = 2;
                if let Some(&(parent, _)) = stack.last() {
                    let done = result[s].clone();
                    result[parent].extend(done);
                }
                continue;
            }
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            let (member, span) = &members[next];
            let member = member.strip_prefix("kind:").unwrap_or(member);
            if let Some(&sub) = names.get(member) {
                match state[sub] {
                    OPEN => {
                        // The cycle: the open supertypes from `sub` down to
                        // here, then `sub` again.
                        let from = stack.iter().position(|&(t, _)| t == sub).unwrap_or(0);
                        let mut cycle = String::new();
                        for &(t, _) in &stack[from..] {
                            cycle.push_str(&v2.ast[t].name);
                            cycle.push_str(" → ");
                        }
                        cycle.push_str(&v2.ast[sub].name);
                        report.error(
                            codes::SUPERTYPE_MEMBER,
                            *span,
                            format!("supertype `{}` contains itself: {cycle}", v2.ast[sub].name),
                        );
                    }
                    NEW => {
                        state[sub] = OPEN;
                        stack.push((sub, 0));
                    }
                    _ => {
                        let done = result[sub].clone();
                        result[s].extend(done);
                    }
                }
                continue;
            }
            match kinds
                .all_named(member)
                .filter(|k| matches!(kinds.cats[k.slot()], CAT_NODE | CAT_CLASS))
                .last()
            {
                Some(k) => result[s].push(k.index()),
                None => report.error(
                    codes::SUPERTYPE_MEMBER,
                    *span,
                    format!("`{member}` is neither a node kind nor a supertype"),
                ),
            }
        }
    }
    v2.ast
        .iter()
        .zip(result)
        .map(|(s, members)| (Box::from(&*s.name), members.into()))
        .collect()
}

/// Warns about (or refuses) rules nothing reaches.
fn unused_rules(analysis: &mut Analysis<'_>, all: &[RuleRef<'_, '_>], roots: &[u32], deny: bool) {
    let mut reached = vec_of(all.len(), false);
    let mut stack: Vec<u32> = roots.to_vec();
    while let Some(r) = stack.pop() {
        if core::mem::replace(&mut reached[r as usize], true) {
            continue;
        }
        for &e in &analysis.owned[r as usize] {
            if let Expr::Rule(callee) = analysis.exprs[e as usize] {
                if !reached[callee as usize] {
                    stack.push(callee);
                }
            }
        }
    }
    for (i, r) in all.iter().enumerate() {
        if !reached[i] && !r.generated {
            check_result(
                analysis.report,
                codes::UNUSED_RULE,
                r.name_span,
                format!("rule `{}` is never used", r.name),
                deny,
            );
        }
    }
}

/// Warns about declared token classes no rule, operator, or mode uses.
fn unused_tokens(
    analysis: &mut Analysis<'_>,
    pratts: &[Pratt],
    lex: &spec2::Lexer2<'_>,
    classes: &[(String, Span, ClassOrigin)],
    first_class: usize,
    scanner: &Scanner,
    deny: bool,
) {
    let mut used = vec_of(classes.len(), false);
    let mark = |k: u16, used: &mut Vec<bool>| {
        let k = usize::from(k);
        if k >= first_class && k < first_class + used.len() {
            used[k - first_class] = true;
        }
    };
    for e in &analysis.exprs {
        if let Expr::Token(k) = e {
            mark(*k, &mut used);
        }
    }
    for p in pratts {
        for (k, l) in p
            .prefix
            .iter()
            .enumerate()
            .chain(p.after.iter().enumerate())
        {
            if *l != 0 {
                mark(k as u16, &mut used);
            }
        }
    }
    for m in scanner.modes.iter() {
        if let Some(t) = m.text {
            mark(t.index(), &mut used);
        }
        for (k, _) in m.actions.iter() {
            mark(*k, &mut used);
        }
    }
    for (i, (name, span, origin)) in classes.iter().enumerate() {
        let declared = matches!(origin, ClassOrigin::Token(t) if !lex.tokens[*t].trivia)
            || matches!(origin, ClassOrigin::StringToken);
        // A class with an action changes the lexer's mode: lexing uses it.
        let acts = matches!(origin, ClassOrigin::Token(t) if lex.tokens[*t].action.is_some());
        if declared && !used[i] && !acts {
            check_result(
                analysis.report,
                codes::UNUSED_TOKEN,
                *span,
                format!("token class `{name}` is never used"),
                deny,
            );
        }
    }
}

/// Records a check result as a warning, or as an error when the check is
/// set to `deny`.
pub(crate) fn check_result(
    report: &mut Report,
    code: diag_lang::Code,
    span: Span,
    message: String,
    deny: bool,
) {
    if deny {
        report.error(code, span, message);
    } else {
        report.warning(code, span, message);
    }
}
