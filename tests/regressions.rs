//! Regressions: inputs an adversarial review used to break the crate's
//! guarantees — stack overflows, exponential and quadratic parse times,
//! recursion without progress, end-of-input line breaks, and spans that split
//! a character. Each must now hold.

use std::time::{Duration, Instant};

use lang_forge::Language;

fn forge(rules: &str) -> Language {
    Language::from_lsf(&format!("[language]\nname = \"r\"\n[rules]\n{rules}"))
        .unwrap_or_else(|e| panic!("{e}"))
}

fn messages(lang: &Language, src: &str) -> Vec<String> {
    lang.parse(src)
        .diagnostics()
        .iter()
        .map(|d| d.message().to_owned())
        .collect()
}

/// Runs `work` on a thread with `kib` KiB of stack, as small threads and the
/// 1 MiB Windows main thread provide.
fn on_stack<T: Send + 'static>(kib: usize, work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(kib * 1024)
        .spawn(work)
        .expect("thread starts")
        .join()
        .expect("no stack overflow")
}

/// `levels` optionals nested inside one rule: each level is recursion the
/// parser must count, not just rule invocations.
fn nested_optionals(levels: usize) -> String {
    let mut body = String::from("a?");
    for _ in 0..levels {
        body = format!("'x' ({body})?");
    }
    format!("a = \"{body}\"\n")
}

/// Alternatives that share a prefix through different rules, nested.
fn nested_speculation(levels: usize) -> String {
    let mut body = String::from("p a | q 'w'");
    for _ in 0..levels {
        body = format!("p ({body}) | q 'w'");
    }
    format!("a = \"{body}\"\np = \"'x'\"\nq = \"'x'\"\n")
}

#[test]
fn test_nested_optionals_stay_within_the_stack() {
    for levels in [4, 20, 63] {
        let lang = forge(&nested_optionals(levels));
        let deep = on_stack(1024, move || {
            let src = "x ".repeat(5_000);
            let parse = lang.parse(&src);
            parse
                .diagnostics()
                .iter()
                .any(|d| d.message().contains("nested too deeply"))
        });
        assert!(deep, "{levels} levels: the depth limit should be reported");
    }
}

#[test]
fn test_nested_speculation_stays_within_the_stack() {
    for levels in [2, 16] {
        let lang = forge(&nested_speculation(levels));
        let ok = on_stack(1024, move || {
            let src = "x ".repeat(4_000) + "w";
            lang.parse(&src).tree().text(&src).map(str::len) == Some(src.len())
        });
        assert!(ok);
    }
}

#[test]
fn test_shared_prefixes_parse_in_linear_time() {
    // `assign` and `expr ';'` both begin with `expr`, which nests blocks of
    // statements. Without memoization each level doubled the work.
    let lang = forge(
        "file = \"stmt*\"\nstmt = \"assign | expr ';'\"\nassign = \"expr '=' expr ';'\"\n\
         block = \"'{' stmt* '}'\"\n[rules.expr]\noperand = \"IDENT | NUMBER | block\"\n\
         levels = [{ left = [\"+\"] }]\n",
    );
    let started = Instant::now();
    let results = on_stack(4096, move || {
        // Within the depth limit, and well past it.
        [60, 400].map(|n| {
            let src = format!("{}x;{}", "{ ".repeat(n), "};".repeat(n));
            let parse = lang.parse(&src);
            let deep = parse
                .diagnostics()
                .iter()
                .any(|d| d.message().contains("nested too deeply"));
            (parse.has_errors(), deep)
        })
    });
    assert_eq!(results, [(false, false), (true, true)]);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn test_synthetic_speculation_is_not_exponential() {
    let lang = forge("a = \"b '!' | c '?' | 'z'\"\nb = \"'(' a ')'\"\nc = \"'(' a ')'\"\n");
    let n = 60;
    let src = format!("{}z{}", "(".repeat(n), ")?".repeat(n));
    let started = Instant::now();
    assert!(!lang.parse(&src).has_errors());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn test_failing_speculation_is_not_quadratic() {
    // Valid except for the last token: every strict attempt fails at the end.
    let lang = forge("a = \"p a | q 'w'\"\np = \"'x' NUMBER*\"\nq = \"'x' NUMBER*\"\n");
    let src = "x 1 2 3 ".repeat(300) + "x 9 !";
    let started = Instant::now();
    let parse = on_stack(4096, move || lang.parse(&src).diagnostics().len());
    assert!(parse >= 1);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn test_recovery_never_recurses_without_progress() {
    let lang = forge("a = \"('q' 'r' | 'z'?) 'y' a?\"\n");
    let parse = lang.parse("q");
    let found: Vec<&str> = parse.diagnostics().iter().map(|d| d.message()).collect();
    assert!(
        !found.iter().any(|m| m.contains("nested too deeply")),
        "{found:?}"
    );
    assert!(parse.tree().descendants().count() < 10);
}

#[test]
fn test_end_of_input_is_a_line_break_everywhere() {
    for (entry, extra) in [
        ("IDENT NEWLINE", ""),
        ("IDENT (NEWLINE | ';')", ""),
        ("IDENT NEWLINE+", ""),
        ("IDENT nl", "nl = \"NEWLINE\"\n"),
        ("IDENT _nl", "_nl = \"NEWLINE\"\n"),
        ("IDENT NEWLINE?", ""),
    ] {
        let lang = Language::from_lsf(&format!(
            "[language]\nname = \"n\"\n[lexer]\nnewlines = true\n[rules]\nfile = \"entry*\"\nentry = \"{entry}\"\n{extra}"
        ))
        .unwrap_or_else(|e| panic!("{entry}: {e}"));
        assert_eq!(messages(&lang, "a\nb"), Vec::<String>::new(), "{entry}");
        assert_eq!(messages(&lang, "a\nb\n"), Vec::<String>::new(), "{entry}");
    }
}

#[test]
fn test_trailing_line_breaks_still_terminate() {
    let lang = Language::from_lsf(
        "[language]\nname = \"n\"\n[lexer]\nnewlines = true\n[rules]\nfile = \"(entry | NEWLINE)*\"\nentry = \"IDENT NEWLINE*\"\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(messages(&lang, "a\n\n\nb").is_empty());
    assert!(messages(&lang, "").is_empty());
}

#[test]
fn test_long_dotted_keys_are_refused_not_overflowed() {
    let schematic = format!("[lexer]\n{} = 1\n", vec!["k"; 300_000].join("."));
    let message = on_stack(1024, move || {
        Language::from_lsf(&schematic)
            .map(|_| ())
            .unwrap_err()
            .diagnostics()[0]
            .message()
            .to_owned()
    });
    assert_eq!(message, "a dotted key has more than 64 parts");
}

#[test]
fn test_escape_error_span_covers_the_whole_character() {
    let schematic = "[language]\nname = \"\\é\"\n";
    let err = Language::from_lsf(schematic).unwrap_err();
    let span = err.diagnostics()[0].primary().span();
    assert_eq!(
        &schematic[span.start().to_usize()..span.end().to_usize()],
        "\\é"
    );
}

#[test]
fn test_long_rule_chains_forge_quickly() {
    let n = 16_000;
    let mut rules = String::new();
    for i in 0..n {
        rules.push_str(&format!("r{i} = \"r{}\"\n", i + 1));
    }
    rules.push_str(&format!("r{n} = \"NUMBER\"\n"));
    let started = Instant::now();
    let lang = forge(&rules);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // The chain nests 16,000 rules deep: too deep to follow, so it is
    // reported rather than overflowing.
    let deep = on_stack(1024, move || {
        lang.parse("7")
            .diagnostics()
            .iter()
            .any(|d| d.message().contains("nested too deeply"))
    });
    assert!(deep);
}

#[test]
fn test_many_literals_forge_in_bounded_memory_and_time() {
    let alternatives: Vec<String> = (0..30_000).map(|i| format!("'k{i}' 'z'")).collect();
    let started = Instant::now();
    let lang = forge(&format!("a = \"{}\"\n", alternatives.join(" | ")));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(!lang.parse("k29999 z").has_errors());
}

#[test]
fn test_realistic_nesting_fits_under_the_limit() {
    let lang = Language::from_lsf(include_str!("../examples/schematics/mini.lsf")).expect("forges");
    let parens = format!("x = {}1{};", "(".repeat(100), ")".repeat(100));
    let else_ifs = format!("if a {{ }}{} else {{ }}", " else if a { }".repeat(100));
    let blocks = format!("{}x;{}", "{ ".repeat(100), "} ".repeat(100));
    on_stack(1024, move || {
        for src in [parens, else_ifs, blocks] {
            let parse = lang.parse(&src);
            assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
        }
    });
}
