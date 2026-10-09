//! Properties that must hold for all input: forging never panics, generated
//! programs always parse cleanly, and every tree is lossless and well nested.

use lang_forge::syntax_lang::{Node, Span};
use lang_forge::{Kind, Language};
use proptest::prelude::*;

const MINI: &str = include_str!("../examples/schematics/mini.lsf");
const SCHEMATICS: [&str; 4] = [
    MINI,
    include_str!("../examples/schematics/calc.lsf"),
    include_str!("../examples/schematics/json.lsf"),
    include_str!("../examples/schematics/conf.lsf"),
];

fn mini() -> Language {
    Language::from_lsf(MINI).expect("mini.lsf forges")
}

/// Every node's children lie inside it, in order, without overlap.
fn assert_well_nested(node: &Node<Kind>) -> Result<(), TestCaseError> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        let mut at = node.span().start();
        for child in node.children() {
            prop_assert!(
                child.span().start() >= at,
                "children overlap or go backwards"
            );
            at = child.span().end();
        }
        prop_assert!(at <= node.span().end(), "a child ends after its parent");
        stack.extend(node.child_nodes());
    }
    Ok(())
}

// ----- a generator of valid mini programs -----

fn ident() -> impl Strategy<Value = String> {
    // Keywords of mini are excluded: they cannot be identifiers.
    "[a-z][a-z0-9_]{0,6}".prop_filter("not a keyword", |s| {
        !matches!(
            s.as_str(),
            "fn" | "let" | "if" | "else" | "while" | "return" | "true" | "false"
        )
    })
}

fn expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (0u32..10_000).prop_map(|n| n.to_string()),
        ident(),
        "[a-z ]{0,8}".prop_map(|s| format!("\"{s}\"")),
        Just(String::from("true")),
        Just(String::from("false")),
    ];
    leaf.prop_recursive(5, 48, 4, |inner| {
        let ops = prop::sample::select(vec!["+", "-", "*", "/", "%", "&&", "||", "=="]);
        prop_oneof![
            // Comparisons do not chain in mini, so each is parenthesized.
            (inner.clone(), ops, inner.clone()).prop_map(|(a, op, b)| match op {
                "==" => format!("({a} == {b})"),
                _ => format!("{a} {op} {b}"),
            }),
            inner.clone().prop_map(|e| format!("({e})")),
            inner.clone().prop_map(|e| format!("-{e}")),
            inner.clone().prop_map(|e| format!("!({e})")),
            (ident(), prop::collection::vec(inner, 0..3))
                .prop_map(|(f, args)| format!("{f}({})", args.join(", "))),
        ]
    })
}

fn stmt() -> impl Strategy<Value = String> {
    let simple = prop_oneof![
        (ident(), expr()).prop_map(|(name, e)| format!("let {name} = {e};")),
        expr().prop_map(|e| format!("{e};")),
        (ident(), expr()).prop_map(|(name, e)| format!("{name} = {e};")),
        prop::option::of(expr()).prop_map(|e| match e {
            Some(e) => format!("return {e};"),
            None => String::from("return;"),
        }),
    ];
    simple.prop_recursive(3, 24, 4, |inner| {
        let block = prop::collection::vec(inner.clone(), 0..4)
            .prop_map(|body| format!("{{\n{}\n}}", body.join("\n")));
        prop_oneof![
            (expr(), block.clone(), prop::option::of(block.clone())).prop_map(
                |(c, then, otherwise)| match otherwise {
                    Some(o) => format!("if {c} {then} else {o}"),
                    None => format!("if {c} {then}"),
                }
            ),
            (expr(), block.clone()).prop_map(|(c, body)| format!("while {c} {body}")),
            block,
        ]
    })
}

fn program() -> impl Strategy<Value = String> {
    let function = (
        ident(),
        prop::collection::vec(ident(), 0..3),
        prop::collection::vec(stmt(), 0..4),
    )
        .prop_map(|(name, params, body)| {
            format!(
                "fn {name}({}) {{\n{}\n}}",
                params.join(", "),
                body.join("\n")
            )
        });
    let item = prop_oneof![function, stmt()];
    (
        prop::collection::vec(item, 0..6),
        "( |\t|\n|// note\n|/\\* c \\*/){0,3}",
    )
        .prop_map(|(items, filler)| items.join(&format!("\n{filler}")))
}

/// Text that mini's lexer has a token for, mixed freely.
const MINI_VOCABULARY: [&str; 30] = [
    "fn", "let", "if", "else", "while", "return", "true", "x", "y1", "0", "42", "\"s\"", "(", ")",
    "{", "}", ",", ";", "=", "==", "!=", "<", "+", "-", "*", "&&", "||", "!", "// c\n", "/* c */",
];

/// Cases per property: 512, or `PROPTEST_CASES` for a longer soak.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(512)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn prop_generated_programs_parse_cleanly(src in program()) {
        let lang = mini();
        let parse = lang.parse(&src);
        prop_assert!(!parse.has_errors(), "{:?} in:\n{}", parse.diagnostics().iter().map(|d| d.message()).collect::<Vec<_>>(), src);
        prop_assert_eq!(parse.tree().text(&src), Some(src.as_str()));
        assert_well_nested(parse.tree())?;
    }

    #[test]
    fn prop_token_soup_is_lossless_and_bounded(words in prop::collection::vec(0..MINI_VOCABULARY.len(), 0..60)) {
        let lang = mini();
        let src = words.iter().map(|&w| MINI_VOCABULARY[w]).collect::<Vec<_>>().join(" ");
        let parse = lang.parse(&src);
        prop_assert_eq!(parse.tree().text(&src), Some(src.as_str()));
        prop_assert_eq!(parse.tree().span(), Span::new(0, src.len() as u32));
        assert_well_nested(parse.tree())?;
        for d in parse.diagnostics() {
            prop_assert!(d.primary().span().end().to_usize() <= src.len());
        }
        // At most one problem per token, so recovery never floods.
        prop_assert!(parse.diagnostics().len() <= words.len() + 1);
    }

    #[test]
    fn prop_arbitrary_text_is_lossless_in_every_language(src in "\\PC{0,64}", which in 0..SCHEMATICS.len()) {
        let lang = Language::from_lsf(SCHEMATICS[which]).expect("example schematics forge");
        let parse = lang.parse(&src);
        prop_assert_eq!(parse.tree().text(&src), Some(src.as_str()));
        assert_well_nested(parse.tree())?;
        let tokens = lang.lex(&src);
        let mut at = 0;
        for token in &tokens {
            prop_assert_eq!(token.span().start().to_usize(), at);
            at = token.span().end().to_usize();
        }
        prop_assert_eq!(at, src.len());
    }

    /// A leading byte-order mark is trivia: the parse of the marked source
    /// is the parse of the unmarked one, three bytes along.
    #[test]
    fn prop_leading_byte_order_mark_changes_only_offsets(src in "\\PC{0,64}", which in 0..SCHEMATICS.len()) {
        let lang = Language::from_lsf(SCHEMATICS[which]).expect("example schematics forge");
        let marked = format!("\u{FEFF}{src}");
        let plain = lang.parse(&src);
        let parse = lang.parse(&marked);
        prop_assert_eq!(parse.tree().text(&marked), Some(marked.as_str()));
        assert_well_nested(parse.tree())?;
        let first = lang.lex(&marked)[0];
        prop_assert!(first.is_trivia());
        prop_assert_eq!(first.span().start().to_usize(), 0);
        let shifted = |d: &lang_forge::diag_lang::Diagnostic| {
            let span = d.primary().span();
            (d.message().to_owned(), span.start().to_usize() + 3, span.end().to_usize() + 3)
        };
        let expected: Vec<_> = plain.diagnostics().iter().map(shifted).collect();
        let found: Vec<_> = parse
            .diagnostics()
            .iter()
            .map(|d| {
                let span = d.primary().span();
                (d.message().to_owned(), span.start().to_usize(), span.end().to_usize())
            })
            .collect();
        prop_assert_eq!(found, expected);
    }

    /// The same holds for a schematic: a mark changes nothing but offsets.
    #[test]
    fn prop_schematic_byte_order_mark_changes_only_offsets(which in 0..SCHEMATICS.len(), cut in any::<prop::sample::Index>()) {
        // Whole schematics, and truncated ones that fail somewhere.
        let full = SCHEMATICS[which];
        let mut at = cut.index(full.len() + 1);
        while !full.is_char_boundary(at) {
            at -= 1;
        }
        let text = &full[..at];
        let marked = format!("\u{FEFF}{text}");
        // Problems of the whole document sit at offset 0 either way; every
        // other one moves along by the mark's three bytes.
        let located = |r: Result<Language, lang_forge::Error>, shift: usize| match r {
            Ok(lang) => Ok(lang.name().to_owned()),
            Err(e) => Err(e
                .diagnostics()
                .iter()
                .map(|d| {
                    let start = d.primary().span().start().to_usize();
                    let start = if start == 0 && d.primary().span().is_empty() { 0 } else { start + shift };
                    (d.message().to_owned(), start)
                })
                .collect::<Vec<_>>()),
        };
        prop_assert_eq!(
            located(Language::from_lsf(&marked), 0),
            located(Language::from_lsf(text), 3)
        );
    }

    #[test]
    fn prop_forge_never_panics_on_arbitrary_text(text in "\\PC{0,200}") {
        let _ = Language::from_lsf(&text);
    }

    #[test]
    fn prop_forge_never_panics_on_mutated_schematics(
        which in 0..SCHEMATICS.len(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), 0u8..3, "[\\[\\]{}()'\"|*+?=,#a-z_ \n]{0,3}"), 1..6),
    ) {
        let mut text = SCHEMATICS[which].to_owned();
        for (index, op, insert) in edits {
            let mut at = index.index(text.len() + 1);
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            match op {
                0 => text.insert_str(at, &insert),
                1 => {
                    let mut end = (at + insert.len().max(1)).min(text.len());
                    while !text.is_char_boundary(end) {
                        end += 1;
                    }
                    text.replace_range(at..end, "");
                }
                _ => {
                    let mut end = (at + 1).min(text.len());
                    while !text.is_char_boundary(end) {
                        end += 1;
                    }
                    text.replace_range(at..end, &insert);
                }
            }
        }
        match Language::from_lsf(&text) {
            Ok(lang) => {
                // A mutated schematic that still forges must still parse
                // anything without panicking.
                let parse = lang.parse("x = 1 + (2 * y); { [a, b] } \"s\" # c");
                prop_assert!(parse.tree().text(parse.source()).is_some());
            }
            Err(err) => {
                prop_assert!(!err.diagnostics().is_empty());
                for d in err.diagnostics() {
                    prop_assert!(d.primary().span().end().to_usize() <= text.len());
                }
            }
        }
    }
}
