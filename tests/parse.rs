//! Parsing with forged languages: the example schematics, the token stream,
//! recovery, the `Parse` API, and capability pipelines.

use lang_forge::diag_lang::{Diagnostic, Label, Severity};
use lang_forge::pass_lang::{Outcome, Pass, PassError};
use lang_forge::syntax_lang::{Element, Node, TokenKind};
use lang_forge::{Capability, Kind, Language, Parse};

fn mini() -> Language {
    Language::from_lsf(include_str!("../examples/schematics/mini.lsf")).expect("mini.lsf forges")
}

fn json() -> Language {
    Language::from_lsf(include_str!("../examples/schematics/json.lsf")).expect("json.lsf forges")
}

fn conf() -> Language {
    Language::from_lsf(include_str!("../examples/schematics/conf.lsf")).expect("conf.lsf forges")
}

fn calc() -> Language {
    Language::from_lsf(include_str!("../examples/schematics/calc.lsf")).expect("calc.lsf forges")
}

fn messages(parse: &Parse<'_>) -> Vec<String> {
    parse
        .diagnostics()
        .iter()
        .map(|d| d.message().to_owned())
        .collect()
}

/// Node kinds in pre-order, by name.
fn nodes(lang: &Language, node: &Node<Kind>) -> Vec<String> {
    node.descendants()
        .map(|n| lang.kind_name(*n.kind()).to_owned())
        .collect()
}

const MINI_PROGRAM: &str = r#"
/* Fibonacci, iteratively. /* nested */ still a comment */
fn fib(n) {
    let a = 0;
    let b = 1;
    while n > 0 {
        let t = a + b;
        a = b;
        b = t;
        n = n - 1;
    }
    return a;
}

if fib(10) == 55 && !false {
    print("ok", fib(10));
} else if true {
    print("unreachable");
} else {
    return;
}
"#;

#[test]
fn test_parse_mini_program_is_clean_and_lossless() {
    let lang = mini();
    let parse = lang.parse(MINI_PROGRAM);
    assert_eq!(messages(&parse), Vec::<String>::new());
    assert_eq!(parse.tree().text(MINI_PROGRAM), Some(MINI_PROGRAM));
    let kinds = nodes(&lang, parse.tree());
    assert_eq!(kinds.iter().filter(|k| *k == "function").count(), 1);
    assert_eq!(kinds.iter().filter(|k| *k == "call").count(), 4);
    assert_eq!(kinds.iter().filter(|k| *k == "compare").count(), 2);
    assert_eq!(kinds.iter().filter(|k| *k == "assign").count(), 3);
    assert_eq!(kinds.iter().filter(|k| *k == "unary").count(), 1);
}

#[test]
fn test_parse_mini_precedence_shapes() {
    let lang = mini();
    let parse = lang.parse("x = a || b && c == d + e * -f(g);");
    assert!(!parse.has_errors(), "{:?}", messages(&parse));
    let shape: Vec<String> = nodes(&lang, parse.tree())
        .into_iter()
        .filter(|k| !matches!(k.as_str(), "expr" | "stmt" | "program" | "args"))
        .collect();
    assert_eq!(
        shape,
        [
            "assign", "binary", "binary", "compare", "binary", "binary", "unary", "call"
        ]
    );
}

#[test]
fn test_parse_mini_dump_snapshot() {
    let lang = mini();
    let parse = lang.parse("let x = (1);");
    assert_eq!(
        parse.dump(),
        "program@0..12\n  \
           stmt@0..12\n    \
             let@0..3 \"let\"\n    \
             WHITESPACE@3..4 \" \"\n    \
             IDENT@4..5 \"x\"\n    \
             WHITESPACE@5..6 \" \"\n    \
             =@6..7 \"=\"\n    \
             WHITESPACE@7..8 \" \"\n    \
             expr@8..11\n      \
               group@8..11\n        \
                 (@8..9 \"(\"\n        \
                 expr@9..10\n          \
                   NUMBER@9..10 \"1\"\n        \
                 )@10..11 \")\"\n    \
             ;@11..12 \";\"\n"
    );
}

#[test]
fn test_parse_mini_recovers_and_keeps_going() {
    let lang = mini();
    let src = "fn f( { let = 1; }\nlet ok = 2;\nprint(ok;\nlet fine = 3;\n";
    let parse = lang.parse(src);
    assert_eq!(
        messages(&parse),
        [
            "expected `)`, found `{`",
            "expected an identifier, found `=`",
            "expected `)`, found `;`",
        ]
    );
    // Everything after the mistakes still parsed into statements.
    let stmt = lang.kind("stmt").expect("rule");
    let top_level = parse
        .tree()
        .child_nodes()
        .filter(|n| *n.kind() == stmt)
        .count();
    assert_eq!(top_level, 3);
    assert_eq!(parse.tree().text(src), Some(src));
}

#[test]
fn test_parse_json_documents() {
    let lang = json();
    for good in [
        "null",
        "[]",
        "{}",
        r#"{"a": [1, -2.5e-3, true, false, null, "s\"q"], "b": {"c": {}}}"#,
        "  [ 0 ,\n 1 ]  ",
    ] {
        let parse = lang.parse(good);
        assert!(!parse.has_errors(), "{good}: {:?}", messages(&parse));
    }
    let cases: [(&str, &str); 5] = [
        ("{\"a\" 1}", "expected `:`, found number `1`"),
        ("[1, 2,]", "expected value, found `]`"),
        ("{\"a\": 1,}", "expected member, found `}`"),
        ("[1] [2]", "expected the end of the input, found `[`"),
        ("{'a': 1}", "unexpected character `'`"),
    ];
    for (bad, first) in cases {
        let parse = lang.parse(bad);
        assert_eq!(
            messages(&parse).first().map(String::as_str),
            Some(first),
            "{bad}"
        );
    }
}

#[test]
fn test_parse_conf_is_line_based() {
    let lang = conf();
    let src = "; settings\n[server]\nhost = \"localhost\"\nport = 8080\n\n[log]\nlevel = info\ncolour = on";
    let parse = lang.parse(src);
    assert!(!parse.has_errors(), "{:?}", messages(&parse));
    let pair = lang.kind("pair").expect("rule");
    assert_eq!(
        parse
            .tree()
            .descendants()
            .filter(|n| *n.kind() == pair)
            .count(),
        4
    );

    let parse = lang.parse("a = 1 b = 2\n");
    assert_eq!(
        messages(&parse),
        ["expected a line break, found identifier `b`"]
    );
}

#[test]
fn test_parse_calc_comments_are_trivia() {
    let lang = calc();
    let parse = lang.parse("# header\n1 + # inline\n 2; # trailing");
    assert!(!parse.has_errors());
    let comment = lang.kind("COMMENT").expect("built in");
    assert!(comment.is_trivia());
    assert_eq!(
        parse
            .tree()
            .tokens()
            .filter(|t| *t.kind() == comment)
            .count(),
        3
    );
    // Leading and trailing comments sit in the root, outside the statement.
    let first_stmt = parse.tree().child_nodes().next().expect("a statement");
    assert_eq!(first_stmt.text(parse.source()), Some("1 + # inline\n 2;"));
}

#[test]
fn test_lex_covers_source_contiguously() {
    let lang = mini();
    let src = "let s = \"a\\\"b\"; // done\n@ x";
    let tokens = lang.lex(src);
    let mut at = 0;
    for token in &tokens {
        assert_eq!(token.span().start().to_usize(), at);
        at = token.span().end().to_usize();
    }
    assert_eq!(at, src.len());
    let names: Vec<&str> = tokens.iter().map(|t| lang.kind_name(*t.kind())).collect();
    assert_eq!(
        names,
        [
            "let",
            "WHITESPACE",
            "IDENT",
            "WHITESPACE",
            "=",
            "WHITESPACE",
            "STRING",
            ";",
            "WHITESPACE",
            "COMMENT",
            "WHITESPACE",
            "UNKNOWN",
            "WHITESPACE",
            "IDENT"
        ]
    );
    assert!(lang.lex("").is_empty());
}

#[test]
fn test_parse_reports_lexical_errors() {
    let lang = mini();
    let parse = lang.parse("let s = \"open;\nlet n = 0x;\n/* never closed");
    assert_eq!(
        messages(&parse),
        [
            "unterminated string",
            "expected `;`, found `let`",
            "hexadecimal literal has no digits",
            "unterminated block comment",
        ]
    );
}

#[test]
fn test_parse_api_accessors() {
    let lang = calc();
    let src = "1 + 2;";
    let mut parse = lang.parse(src);
    assert_eq!(parse.source(), src);
    assert_eq!(parse.language().name(), "calc");
    assert!(!parse.has_errors());

    parse.report(Diagnostic::new(
        Severity::Note,
        "just so you know",
        Label::unlabelled(parse.tree().span()),
    ));
    assert_eq!(parse.diagnostics().len(), 1);
    assert!(!parse.has_errors());
    parse.report(Diagnostic::new(
        Severity::Error,
        "no",
        Label::unlabelled(parse.tree().span()),
    ));
    assert!(parse.has_errors());

    let tree = parse.clone().into_tree();
    assert_eq!(&tree, parse.tree());
}

#[test]
fn test_parse_empty_and_blank_sources() {
    let lang = mini();
    for src in ["", "   \n\t", "// only a comment"] {
        let parse = lang.parse(src);
        assert!(!parse.has_errors(), "{src:?}");
        assert_eq!(parse.tree().text(src), Some(src));
        assert_eq!(parse.tree().child_nodes().count(), 0);
    }
}

#[test]
fn test_parse_deep_nesting_reports_instead_of_overflowing() {
    let lang = calc();
    let depth = 50_000;
    let src = format!("{}1{};", "(".repeat(depth), ")".repeat(depth));
    let handle = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let parse = lang.parse(&src);
            let deep = messages(&parse)
                .iter()
                .filter(|m| m.contains("nested too deeply"))
                .count();
            (deep, parse.tree().text(&src).map(str::len))
        })
        .expect("thread starts");
    let (deep, len) = handle.join().expect("no stack overflow");
    assert_eq!(deep, 1);
    assert_eq!(len, Some(2 * depth + 2));
}

#[test]
fn test_parse_long_flat_input() {
    let lang = mini();
    let mut src = String::new();
    for i in 0..20_000 {
        src.push_str(&format!("let v{i} = v{i} * 2 + {i};\n"));
    }
    let parse = lang.parse(&src);
    assert!(!parse.has_errors());
    assert_eq!(parse.tree().child_nodes().count(), 20_000);
}

#[test]
fn test_parse_long_operator_chain() {
    let lang = calc();
    let src = format!("{};", vec!["1"; 50_000].join(" + "));
    let parse = lang.parse(&src);
    assert!(!parse.has_errors());
    let binary = lang.kind("binary").expect("node");
    assert_eq!(
        parse
            .tree()
            .descendants()
            .filter(|n| *n.kind() == binary)
            .count(),
        49_999
    );
}

#[test]
fn test_parse_is_usable_across_threads() {
    let lang = std::sync::Arc::new(mini());
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let lang = std::sync::Arc::clone(&lang);
            std::thread::spawn(move || {
                let src = format!("let t{i} = {i} * {i};");
                !lang.parse(&src).has_errors()
            })
        })
        .collect();
    for handle in handles {
        assert!(handle.join().expect("thread"));
    }
}

#[test]
fn test_parse_trivia_kinds_and_error_nodes() {
    let lang = mini();
    let parse = lang.parse("let x = 1; ; ; let y = 2;");
    let error = lang.kind("ERROR").expect("built in");
    let errors: Vec<&Node<Kind>> = parse
        .tree()
        .descendants()
        .filter(|n| *n.kind() == error)
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].text(parse.source()), Some("; ;"));
    // Error nodes hold the skipped tokens and the trivia between them.
    let inner: Vec<bool> = errors[0]
        .children()
        .map(|c| matches!(c, Element::Token(t) if t.is_trivia()))
        .collect();
    assert_eq!(inner, [false, true, false]);
}

// ----- capability pipelines -----

/// Appends its name to a shared log when it runs.
struct Logger {
    name: &'static str,
    log: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
}

impl<'a> Pass<Parse<'a>> for Logger {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&mut self, _parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
        self.log
            .lock()
            .map_err(|_| PassError::new("poisoned"))?
            .push(self.name);
        Ok(Outcome::Unchanged)
    }
}

/// Fails, to show errors carry the pass name.
struct Failing;

impl<'a> Pass<Parse<'a>> for Failing {
    fn name(&self) -> &'static str {
        "strict"
    }

    fn run(&mut self, parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
        if parse.tree().child_nodes().count() > 1 {
            return Err(PassError::new("only one item allowed"));
        }
        Ok(Outcome::Unchanged)
    }
}

fn with_capabilities(include: &str) -> Language {
    Language::from_lsf(&format!(
        "[language]\nname = \"p\"\n[rules]\nfile = \"item*\"\nitem = \"IDENT\"\n[capabilities]\ninclude = [{include}]\n"
    ))
    .expect("forges")
}

#[test]
fn test_pipeline_runs_capabilities_in_schematic_order() {
    let lang = with_capabilities("\"second\", \"first\"");
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let registry: Vec<Capability> = vec![
        Box::new(Logger {
            name: "first",
            log: log.clone(),
        }),
        Box::new(Logger {
            name: "unrelated",
            log: log.clone(),
        }),
        Box::new(Logger {
            name: "second",
            log: log.clone(),
        }),
    ];
    let mut pipeline = lang.pipeline(registry).expect("all capabilities present");
    assert_eq!(pipeline.len(), 2);
    let mut parse = lang.parse("a b");
    let report = pipeline.run(&mut parse).expect("passes succeed");
    assert_eq!(report.runs().len(), 2);
    assert_eq!(*log.lock().expect("lock"), ["second", "first"]);
}

#[test]
fn test_pipeline_reports_missing_and_ambiguous_capabilities() {
    let lang = with_capabilities("\"needed\", \"twice\"");
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let registry: Vec<Capability> = vec![
        Box::new(Logger {
            name: "twice",
            log: log.clone(),
        }),
        Box::new(Logger { name: "twice", log }),
    ];
    let Err(err) = lang.pipeline(registry) else {
        panic!("an incomplete registry must be refused");
    };
    let found: Vec<&str> = err.diagnostics().iter().map(|d| d.message()).collect();
    assert_eq!(
        found,
        [
            "capability `needed` has no pass",
            "capability `twice` has more than one pass"
        ]
    );
    assert_eq!(
        err.to_string(),
        "7:12: capability `needed` has no pass (and 1 more error)"
    );
}

#[test]
fn test_pipeline_without_capabilities_is_empty() {
    let lang = mini();
    let empty: Vec<Capability> = Vec::new();
    assert!(
        lang.pipeline(empty).is_err(),
        "mini includes `unused-variables`"
    );
    let plain =
        Language::from_lsf("[language]\nname = \"n\"\n[rules]\nn = \"NUMBER\"\n").expect("forges");
    let pipeline = plain
        .pipeline(Vec::<Capability>::new())
        .expect("nothing to assemble");
    assert!(pipeline.is_empty());
}

#[test]
fn test_pipeline_pass_errors_name_the_pass() {
    let lang = with_capabilities("\"strict\"");
    let registry: Vec<Capability> = vec![Box::new(Failing)];
    let mut pipeline = lang.pipeline(registry).expect("assembled");
    let mut parse = lang.parse("a b");
    let err = pipeline.run(&mut parse).expect_err("two items");
    assert_eq!(err.pass(), "strict");
    assert_eq!(err.message(), "only one item allowed");
}
