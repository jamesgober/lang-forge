//! Mox (the LexerSketch flagship, `_lexersketch/sketches/mox.lsf`): its
//! syntax sections forge, and representative templates and scripts parse.
//!
//! `tests/sketches/mox.lsf` is a copy of the flagship sketch with one change:
//! `elseif_clause` acknowledges an overlap the check finds (an `else:` of the
//! alternative syntax after a braces-form `elseif`), exactly as the sketch
//! already does for `if_stmt`. `test_flagship_elseif_divergence_is_found`
//! keeps the finding visible.

use std::collections::BTreeSet;

use lang_forge::Language;
use lang_forge::diag_lang::Severity;
use lang_forge::syntax_lang::Node;

const MOX: &str = include_str!("sketches/mox.lsf");
const SCRIPT: &str = include_str!("mox/script.mox");
const TEMPLATE: &str = include_str!("mox/template.mox");

fn mox() -> Language {
    Language::from_lsf(MOX).unwrap_or_else(|e| panic!("{e}"))
}

/// Every node kind name in `tree`.
fn node_kinds(lang: &Language, tree: &Node<lang_forge::Kind>) -> BTreeSet<String> {
    core::iter::once(tree)
        .chain(tree.descendants())
        .map(|n| lang.kind_name(*n.kind()).to_owned())
        .collect()
}

fn clean(lang: &Language, src: &str) -> String {
    let parse = lang.parse(src);
    assert!(parse.diagnostics().is_empty(), "{:#?}", parse.diagnostics());
    assert_eq!(parse.tree().text(src), Some(src), "lossless");
    parse.dump()
}

#[test]
fn test_mox_forges_with_only_expected_warnings() {
    let lang = mox();
    assert_eq!(lang.format(), 2);
    assert_eq!(lang.name(), "mox");
    assert_eq!(lang.display_name(), "Mox");
    assert_eq!(lang.extensions().collect::<Vec<_>>(), ["mox"]);
    // Warnings: confusables not checked yet (LSF1007), and the documented
    // consequences of `trailing_dot` (LSF3120). Nothing else.
    let codes: BTreeSet<String> = lang
        .warnings()
        .iter()
        .filter_map(|d| d.code().map(|c| c.to_string()))
        .collect();
    assert_eq!(
        codes,
        BTreeSet::from(["LSF1007".to_owned(), "LSF3120".to_owned()])
    );
    // The supertypes and fields the lowering needs are there.
    assert!(lang.supertypes().count() > 0);
    let if_stmt = lang.kind("if_stmt").expect("a rule");
    let fields: Vec<&str> = lang.fields(if_stmt).map(|f| f.name()).collect();
    assert_eq!(fields, ["cond", "then", "else"]);
}

#[test]
fn test_flagship_elseif_divergence_is_found() {
    // The flagship text: `elseif_clause` as a plain rule without `allow`.
    let flagship = MOX
        .replace(
            "\n# lang-forge copy: same divergence as `if_stmt` (an `else:` of the alternative syntax after a\n\
             # braces-form `elseif`), found by the overlap check; acknowledged the same way.\n\
             [rules.elseif_clause]\n\
             rule  = \"'elseif' '(' cond:expr ')' then:_stmt else:(elseif_clause | else_clause)?\"\n\
             allow = [\"overlap\"]\n",
            "",
        )
        .replace(
            "else_clause   = \"'else' body:_stmt\"\n",
            "else_clause   = \"'else' body:_stmt\"\n\
             elseif_clause = \"'elseif' '(' cond:expr ')' then:_stmt else:(elseif_clause | else_clause)?\"\n",
        );
    assert_ne!(flagship, MOX);
    let err = Language::from_lsf(&flagship).expect_err("the overlap is denied");
    let found: Vec<(String, String)> = err
        .diagnostics()
        .iter()
        .filter(|d| d.severity() == Severity::Error)
        .map(|d| {
            (
                d.code().map(|c| c.to_string()).unwrap_or_default(),
                d.message().to_owned(),
            )
        })
        .collect();
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].0, "LSF4301");
    assert!(
        found[0]
            .1
            .starts_with("input `else :` is rejected: this optional commits on `else`"),
        "{found:#?}"
    );
}

#[test]
fn test_mox_script_parses_cleanly() {
    let lang = mox();
    let dump = clean(&lang, SCRIPT);
    let parse = lang.parse(SCRIPT);
    let kinds = node_kinds(&lang, parse.tree());
    for kind in [
        "namespace_decl",
        "use_stmt",
        "class_decl",
        "trait_use",
        "trait_rule",
        "method",
        "property",
        "class_const",
        "enum_decl",
        "enum_case",
        "fn_decl",
        "closure",
        "closure_use",
        "arrow_fn",
        "match_expr",
        "match_arm",
        "foreach_stmt",
        "if_stmt",
        "elseif_clause",
        "else_clause",
        "try_stmt",
        "catch_clause",
        "yield_expr",
        "new_expr",
        "anon_class",
        "static_stmt",
        "unset_stmt",
        "exit_expr",
        "attribute",
        "DQ_STRING",
        "HEREDOC",
        "binary",
        "prefix",
        "call",
        "member",
        "index",
    ] {
        assert!(
            kinds.contains(kind),
            "no `{kind}` node in the script's tree"
        );
    }
    // Contextual keywords where Mox uses them…
    for line in [
        "async@779..784 \"async\"",
        "op:await@830..835 \"await\"",
        "op:spawn@916..921 \"spawn\"",
    ] {
        assert!(dump.contains(line), "missing {line}");
    }
    // …interpolation, simple variables in strings, heredoc and nowdoc…
    for line in [
        "parts:DQ_VAR@1098..1102 \"$key\"",
        "parts:DQ_VAR@1107..1117 \"$row[role]\"",
        "open:HEREDOC_OPEN@1512..1520 \"<<<HTML\\n\"",
        "parts:HEREDOC_VAR@1605..1616 \"$this->name\"",
        "close:HEREDOC_CLOSE@1660..1677 \"\\n            HTML\"",
        "rhs:NOWDOC@1694..1756",
        "name:MAGIC_CONST@2787..2795 \"__LINE__\"",
    ] {
        assert!(dump.contains(line), "missing {line}");
    }
    // …and the casts are operators.
    assert!(dump.contains("op:INT_CAST"));
    assert!(dump.contains("op:FLOAT_CAST"));
}

#[test]
fn test_mox_string_variables_are_self_injections() {
    let lang = mox();
    let parse = lang.parse(SCRIPT);
    let injected: Vec<(&str, &str, &str)> = parse
        .injections()
        .iter()
        .map(|i| {
            let tree = i.tree().expect("a self-injection is parsed");
            (
                i.id(),
                &SCRIPT[i.span().start().to_usize()..i.span().end().to_usize()],
                lang.kind_name(*tree.kind()),
            )
        })
        .collect();
    assert_eq!(
        injected,
        [
            ("DQ_VAR", "$key", "simple_interp"),
            ("DQ_VAR", "$row[role]", "simple_interp"),
            ("HEREDOC_VAR", "$this->name", "simple_interp"),
        ]
    );
    // The injected tree is labelled like any other.
    let row = parse.injections()[1].tree().expect("parsed");
    let labels: Vec<&str> = (0..row.len())
        .filter_map(|i| lang.field_label(row, i).and_then(|l| lang.label_name(l)))
        .collect();
    assert_eq!(labels, ["var", "key"]);
}

#[test]
fn test_mox_template_islands() {
    let lang = mox();
    let dump = clean(&lang, TEMPLATE);
    assert!(
        dump.starts_with("file@0..458\n  head:TEMPLATE_TEXT@0..36"),
        "{dump}"
    );
    let parse = lang.parse(TEMPLATE);
    let kinds = node_kinds(&lang, parse.tree());
    for kind in [
        "echo_tag",
        "template_chunk",
        "if_stmt",
        "foreach_stmt",
        "alt_block",
        "else_alt",
    ] {
        assert!(
            kinds.contains(kind),
            "no `{kind}` node in the template's tree"
        );
    }
    // Every byte outside the islands is template text, and the editor gets
    // each such range as an HTML injection.
    let text = lang.kind("TEMPLATE_TEXT").expect("mode text");
    let text_spans: Vec<(usize, usize)> = parse
        .tree()
        .tokens()
        .filter(|t| *t.kind() == text)
        .map(|t| (t.span().start().to_usize(), t.span().end().to_usize()))
        .collect();
    let html: Vec<(usize, usize)> = parse
        .injections()
        .iter()
        .inspect(|i| {
            assert_eq!(i.language(), "html");
            assert!(i.is_editor());
            assert!(i.tree().is_none());
        })
        .map(|i| (i.span().start().to_usize(), i.span().end().to_usize()))
        .collect();
    assert_eq!(html, text_spans);
    assert_eq!(html.len(), 10);
    // The comment stops before `?>`, so the island closes.
    assert!(dump.contains("COMMENT@"), "{dump}");
}

#[test]
fn test_mox_contextual_words_stay_identifiers_elsewhere() {
    let lang = mox();
    clean(
        &lang,
        "<?mox\nfunction spawn($await) { return $await; }\n$async = spawn(1);\nclass Await { public function async() {} }\n",
    );
}

#[test]
fn test_mox_errors_recover_and_stay_lossless() {
    let lang = mox();
    let broken = "<?mox\nfunction f( { $x = ; }\nclass { }\necho \"a {$b\";\n$c = <<<EOT\nno end";
    let parse = lang.parse(broken);
    assert!(parse.has_errors());
    assert_eq!(parse.tree().text(broken), Some(broken));
    for d in parse.diagnostics() {
        let code = d.code().map(|c| c.to_string()).unwrap_or_default();
        assert!(
            code.starts_with("LF0") || code.starts_with("LF1"),
            "{code}: {}",
            d.message()
        );
    }
    // Every prefix of the script parses losslessly (truncated input is the
    // common editor case).
    let mut cut = 0;
    while cut < SCRIPT.len() {
        if SCRIPT.is_char_boundary(cut) {
            let prefix = &SCRIPT[..cut];
            assert_eq!(
                lang.parse(prefix).tree().text(prefix),
                Some(prefix),
                "prefix of {cut} bytes"
            );
        }
        cut += 37;
    }
}

#[test]
fn test_mox_lex_has_no_unknown_tokens() {
    let lang = mox();
    let unknown = lang.kind("UNKNOWN").expect("builtin");
    for src in [SCRIPT, TEMPLATE] {
        let tokens = lang.lex(src);
        assert!(tokens.iter().all(|t| *t.kind() != unknown));
        assert_eq!(
            tokens.last().map(|t| t.span().end().to_usize()),
            Some(src.len())
        );
        assert!(tokens.iter().filter(|t| t.is_trivia()).count() > 0);
    }
}

#[test]
fn test_mox_image_round_trip_and_determinism() {
    let lang = mox();
    let image = lang.to_image();
    assert_eq!(image, mox().to_image(), "forging is deterministic");
    let loaded = Language::from_image(&image).expect("its own image");
    for src in [SCRIPT, TEMPLATE] {
        assert_eq!(loaded.parse(src).dump(), lang.parse(src).dump());
    }
    assert_eq!(loaded.to_image(), image);
    assert_eq!(
        loaded.fields(loaded.kind("if_stmt").expect("rule")).count(),
        3
    );
    // Warnings are not stored in the image.
    assert!(loaded.warnings().is_empty());
}
