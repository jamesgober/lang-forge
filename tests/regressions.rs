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

// ----- 1.0.1 -----

/// Statements that begin alike (an assignment and an expression statement
/// both begin with an expression), plus a `let` form that never speculates.
fn speculative_language() -> Language {
    forge(
        "file = \"stmt*\"\nstmt = \"assign | expr ';' | 'let' expr ';'\"\n\
         assign = \"expr '=' expr ';'\"\nblock = \"'{' stmt* '}'\"\n\
         [rules.expr]\noperand = \"IDENT | NUMBER | block | '(' expr ')'\"\n\
         levels = [{ left = [\"+\"] }]\n",
    )
}

#[test]
fn test_depth_limit_does_not_cripple_later_speculation() {
    // Once one construct hit the depth limit, every later choice tried only
    // its first alternative, so `x;` (which needs the second) was an error.
    let tails = "x;\ny = 1;\nz + 2;\n";
    let lang = speculative_language();
    assert!(!lang.parse(tails).has_errors());
    let results = on_stack(1024, move || {
        let nest = format!("{}1{}", "(".repeat(1_000), ")".repeat(1_000));
        // Deep inside a statement that speculates, and inside one that does not.
        [format!("{nest};"), format!("let {nest};")].map(|head| {
            let src = format!("{head}\n{tails}");
            let parse = lang.parse(&src);
            let lossless = parse.tree().text(&src) == Some(src.as_str());
            let deep = parse
                .diagnostics()
                .iter()
                .any(|d| d.message().contains("nested too deeply"));
            // Every error lies inside the deep construct, none in the tail.
            let contained = parse
                .diagnostics()
                .iter()
                .all(|d| d.primary().span().end().to_usize() <= head.len());
            (lossless, deep, contained)
        })
    });
    assert_eq!(results, [(true, true, true); 2]);
}

#[test]
fn test_depth_limit_still_bounds_speculation_time() {
    // What the flag exists for: within one speculation, input past the depth
    // limit must not make every level try every alternative.
    let lang = speculative_language();
    let started = Instant::now();
    let reported = on_stack(4096, move || {
        let mut src = String::new();
        for _ in 0..20 {
            src.push_str(&format!("{}x;{}\n", "{ ".repeat(400), "};".repeat(400)));
        }
        lang.parse(&src)
            .diagnostics()
            .iter()
            .filter(|d| d.message().contains("nested too deeply"))
            .count()
    });
    assert_eq!(reported, 1);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn test_nested_speculation_stays_linear_in_depth() {
    // Blocks nested in blocks, where every level's statement first tries the
    // alternative that fails only after parsing the whole level. The memo
    // used to copy each level's events again at every enclosing level.
    let lang = speculative_language();
    let source = |depth: usize| {
        let width = 12_000 / depth;
        let mut src = String::new();
        for _ in 0..depth {
            src.push_str("{ ");
            for i in 0..width {
                src.push_str(&format!("x{i} + 1; "));
            }
        }
        src.push_str("x;");
        src.push_str(&" };".repeat(depth));
        src
    };
    let time = |depth: usize| {
        let lang = lang.clone();
        let src = source(depth);
        on_stack(4096, move || {
            let started = Instant::now();
            let parse = lang.parse(&src);
            (parse.has_errors(), started.elapsed())
        })
    };
    let (flat_errors, flat) = time(1);
    let (nested_errors, nested) = time(60);
    assert!(!flat_errors && !nested_errors);
    // The same statements, nested 60 deep: replaying each level once costs
    // a few times more, not sixty.
    assert!(
        nested < flat * 12 + Duration::from_millis(250),
        "flat {flat:?}, nested {nested:?}"
    );
}

#[test]
fn test_hostile_schematic_tables_are_refused_before_allocation() {
    // 30,000 distinct keywords and 100,000 optional choices between them:
    // about 2.4 MB of schematic whose token sets would have taken gigabytes.
    let mut rules = String::new();
    for r in 0..100 {
        rules.push_str(&format!("r{r} = \""));
        for g in r * 1000..(r + 1) * 1000 {
            let (a, b) = ((2 * g) % 30_000, (2 * g + 1) % 30_000);
            rules.push_str(&format!("('k{a}' | 'k{b}')? "));
        }
        rules.push_str("\"\n");
    }
    let started = Instant::now();
    let err = Language::from_lsf(&format!("[language]\nname = \"h\"\n[rules]\n{rules}"))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(
        err.diagnostics()[0].message(),
        "the grammar's tables would need more than 256 MiB"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn test_many_expression_rules_count_against_the_table_budget() {
    // Each expression rule has two operator tables over every token kind.
    let literals: Vec<String> = (0..15_000).map(|i| format!("'k{i}'")).collect();
    let mut rules = format!("a = \"{}\"\n", literals.join(" "));
    for i in 0..15_000 {
        rules.push_str(&format!("[rules.e{i}]\noperand = \"'k{i}'\"\n"));
    }
    let err = Language::from_lsf(&format!("[language]\nname = \"p\"\n[rules]\n{rules}"))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(
        err.diagnostics()[0].message(),
        "the grammar's tables would need more than 256 MiB"
    );
}

#[test]
fn test_oversized_schematic_is_refused() {
    let schematic = format!(
        "[language]\nname = \"big\"\n[rules]\na = \"NUMBER\"\n{}",
        "# padding\n".repeat(900_000)
    );
    let err = Language::from_lsf(&schematic).map(|_| ()).unwrap_err();
    assert_eq!(
        err.diagnostics()[0].message(),
        "the schematic is larger than 8 MiB"
    );
}

#[test]
fn test_nesting_bomb_schematic_is_refused_on_a_small_stack() {
    // 64 inline tables, each under a 64-part dotted key: each limit allowed
    // its 64 levels, so together they nested about 4,096 tables deep.
    let key = vec!["k"; 64].join(".");
    let mut bomb = String::from("1");
    for _ in 0..64 {
        bomb = format!("{{ {key} = {bomb} }}");
    }
    let message = on_stack(256, move || {
        Language::from_lsf(&format!("[lexer]\nx = {bomb}\n"))
            .map(|_| ())
            .unwrap_err()
            .diagnostics()[0]
            .message()
            .to_owned()
    });
    assert_eq!(message, "the schematic nests more than 64 levels deep");
}

#[test]
fn test_byte_order_mark_is_trivia_in_source_and_schematic() {
    let bom = '\u{FEFF}';
    let lang = Language::from_lsf(&format!(
        "{bom}[language]\nname = \"b\"\n[lexer]\nline_comments = [\"#\"]\n[rules]\nfile = \"IDENT*\"\n"
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    for rest in ["a b", "  a b", "", "# c\na", "\n\nb"] {
        let src = format!("{bom}{rest}");
        let parse = lang.parse(&src);
        assert!(!parse.has_errors(), "{src:?}: {:?}", parse.diagnostics());
        assert_eq!(parse.tree().text(&src), Some(src.as_str()));
        // The mark opens the first whitespace token, with any that follows.
        let first = lang.lex(&src)[0];
        assert_eq!(lang.kind_name(*first.kind()), "WHITESPACE");
        let space = rest.len() - rest.trim_start().len();
        assert_eq!(first.span().end().to_usize(), 3 + space, "{src:?}");
    }
    // Only a leading one: elsewhere U+FEFF is an unexpected character.
    assert_eq!(
        messages(&lang, &format!("a {bom}b")),
        [format!("unexpected character `{bom}`")]
    );
}
