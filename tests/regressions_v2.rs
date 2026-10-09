//! Regressions and budgets for 2.0: forging cost that grew with hostile
//! sketches (ISSUES P10: the left-recursion check and suggestion cost),
//! deep nesting in format-2 input, and image robustness at scale.

use std::time::{Duration, Instant};

use lang_forge::Language;

fn v2(sections: &str, rules: &str) -> String {
    format!(
        "[sketch]\nformat = 2\n[language]\nname = \"t\"\nversion = \"1.0.0\"\n{sections}\n[rules]\n{rules}"
    )
}

/// Runs `work` on a thread with `kib` KiB of stack.
fn on_stack<T: Send + 'static>(kib: usize, work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(kib * 1024)
        .spawn(work)
        .expect("thread starts")
        .join()
        .expect("no panic")
}

/// Generous: a debug build on a slow CI machine.
const SLOW: Duration = Duration::from_secs(20);

#[test]
fn test_p10_long_left_recursive_cycle_is_linear() {
    // r0 → r1 → … → r4999 → r0, all in left position: one cycle, found by
    // one strongly-connected-components pass (the 1.x check walked from
    // every rule).
    let n = 5000;
    let mut rules = String::new();
    for i in 0..n {
        rules.push_str(&format!("r{i} = \"r{} 'x' | 'y'\"\n", (i + 1) % n));
    }
    for format in [1, 2] {
        let sketch = if format == 1 {
            format!("[language]\nname = \"t\"\n[rules]\n{rules}")
        } else {
            v2("", &rules)
        };
        let started = Instant::now();
        let err = on_stack(1024, move || {
            Language::from_lsf(&sketch).expect_err("left-recursive")
        });
        assert!(
            started.elapsed() < SLOW,
            "format {format}: {:?}",
            started.elapsed()
        );
        let left: Vec<&str> = err
            .diagnostics()
            .iter()
            .map(|d| d.message())
            .filter(|m| m.contains("left-recursive"))
            .collect();
        // One cycle, reported once.
        assert_eq!(left.len(), 1, "format {format}");
    }
}

#[test]
fn test_p10_many_undefined_names_share_one_suggestion_budget() {
    // Thousands of long undefined names against thousands of long rule
    // names: each suggestion is an edit-distance search, and together they
    // are bounded by one budget instead of growing as names × names.
    let n = 3000;
    let pad = "q".repeat(60);
    let mut rules = String::from("file = \"");
    for i in 0..n {
        rules.push_str(&format!("missing_{pad}_{i} "));
    }
    rules.push_str("\"\n");
    for i in 0..n {
        rules.push_str(&format!("defined_{pad}_{i} = \"'k'\"\n"));
    }
    for format in [1, 2] {
        let sketch = if format == 1 {
            format!("[language]\nname = \"t\"\n[rules]\n{rules}")
        } else {
            v2("", &rules)
        };
        let started = Instant::now();
        let err = Language::from_lsf(&sketch).expect_err("undefined names");
        assert!(
            started.elapsed() < SLOW,
            "format {format}: {:?}",
            started.elapsed()
        );
        assert!(err.diagnostics().len() >= n, "format {format}");
    }
}

#[test]
fn test_deeply_nested_interpolation_is_bounded() {
    let lang = Language::from_lsf(&v2(
        "[lexer]\nmax_mode_depth = 64\n\
         [lexer.strings.T]\nopen = \"`\"\ninterpolate = [{ open = \"${\", close = \"}\", rule = \"value\" }]\n",
        "file = \"value*\"\nvalue = \"T | IDENT\"\n",
    ))
    .expect("forges");
    // `${`…`}` nested 100 000 deep: lexing is iterative, the mode stack stops
    // at 64, and the parser's depth limit reports the rest.
    let depth = 100_000;
    let src = format!("{}x{}", "`${".repeat(depth), "}`".repeat(depth));
    let parse = on_stack(1024, move || {
        let lang = lang;
        let parse = lang.parse(&src);
        assert_eq!(parse.tree().text(&src), Some(src.as_str()));
        parse
            .diagnostics()
            .iter()
            .map(|d| d.message().to_owned())
            .collect::<Vec<_>>()
    });
    assert!(
        parse
            .iter()
            .any(|m| m == "lexer modes are nested too deeply"),
        "{:?}",
        &parse[..parse.len().min(5)]
    );
}

#[test]
fn test_deeply_nested_predicates_and_groups_forge() {
    // A rule text nested 10 000 groups deep is refused by the rule-syntax
    // depth limit, not by a stack overflow.
    let rule = format!(
        "a = \"{}IDENT{}\"\n",
        "&(".repeat(10_000),
        ")".repeat(10_000)
    );
    let sketch = v2("", &rule);
    let err = on_stack(1024, move || {
        Language::from_lsf(&sketch).expect_err("too deep")
    });
    assert!(
        err.diagnostics()
            .iter()
            .any(|d| d.message().contains("deep")),
        "{err}"
    );
}

#[test]
fn test_hostile_regex_classes_are_refused_quickly() {
    for regex in [
        "(a|b)*a(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)(a|b)",
        "((((((((((a*)*)*)*)*)*)*)*)*)*)b",
        "[\\p{XID_Continue}--a]",
    ] {
        let sketch = v2(
            &format!("[lexer.tokens]\nX = {{ regex = '{regex}' }}\n"),
            "a = \"X\"\n",
        );
        let started = Instant::now();
        let _ = Language::from_lsf(&sketch);
        assert!(started.elapsed() < SLOW, "{regex}: {:?}", started.elapsed());
    }
}

#[test]
fn test_field_derivation_is_budgeted() {
    // A hidden rule over 5,000 keywords, used 10,000 times under a label:
    // every use's summary would list all 5,000 kinds (50 million entries).
    let n = 5000;
    let mut rules = String::from("file = \"(x:_k)*");
    for _ in 0..10_000 {
        rules.push_str(" _k");
    }
    rules.push_str("\"\n_k = \"");
    for i in 0..n {
        if i > 0 {
            rules.push_str(" | ");
        }
        rules.push_str(&format!("'w{i}'"));
    }
    rules.push_str("\"\n");
    let sketch = v2("[sketch.checks]\noverlap = \"allow\"\n", &rules);
    let started = Instant::now();
    let result = Language::from_lsf(&sketch);
    assert!(started.elapsed() < SLOW, "{:?}", started.elapsed());
    let err = result.expect_err("over the budget");
    assert!(
        err.diagnostics()
            .iter()
            .any(|d| d.code().is_some_and(|c| c.to_string() == "LSF9008")),
        "{err}"
    );
}

#[test]
fn test_huge_keyword_and_class_sets_forge() {
    let mut words = String::new();
    let mut alts = String::new();
    for i in 0..2000 {
        words.push_str(&format!("\"kw{i}\", "));
        if i > 0 {
            alts.push_str(" | ");
        }
        alts.push_str(&format!("'kw{i}'"));
    }
    let sketch = v2(
        &format!("[lexer.keywords]\ncontextual = [{words}]\n"),
        &format!("file = \"({alts})*\"\n"),
    );
    let started = Instant::now();
    let lang = Language::from_lsf(&sketch).expect("forges");
    assert!(started.elapsed() < SLOW, "{:?}", started.elapsed());
    assert!(!lang.parse("kw0 kw1999 kw7").has_errors());
    let image = lang.to_image();
    assert_eq!(
        Language::from_image(&image).expect("loads").to_image(),
        image
    );
}
