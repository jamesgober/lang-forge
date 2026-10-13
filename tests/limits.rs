//! The parser's limits (2.0.0-alpha.2): the depth limit and the step budget
//! stop a parse in time linear in the input, with one located diagnostic and
//! a lossless tree, and no path the parser cannot memoize multiplies its work.
//!
//! The regressions here are the ones 2.0.0-alpha.1 failed: Mox programs with
//! sixty nested array literals or `list(` calls never finished (each level
//! past the depth limit retried every alternative, and results that met the
//! limit could not be memoized), and unclosed blocks before the end of input
//! took exponential time in a grammar whose statement list recurses through
//! `NEWLINE` (one cut by the progress guard kept every enclosing result out
//! of the memo).

use std::time::{Duration, Instant};

use lang_forge::syntax_lang::Node;
use lang_forge::{Kind, Language, Parse, ParseBudget};
use proptest::prelude::*;

const MOX: &str = include_str!("sketches/mox.lsf");

fn mox() -> Language {
    Language::from_lsf(MOX).unwrap_or_else(|e| panic!("{e}"))
}

/// Runs `work` on a thread with `kib` KiB of stack.
fn on_stack<T: Send + 'static>(kib: usize, work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(kib * 1024)
        .spawn(work)
        .expect("thread starts")
        .join()
        .expect("no stack overflow")
}

/// The diagnostic codes of a parse, in order.
fn codes(parse: &Parse<'_>) -> Vec<String> {
    parse
        .diagnostics()
        .iter()
        .map(|d| d.code().map(|c| c.to_string()).unwrap_or_default())
        .collect()
}

/// Significant tokens: what the budget is proportional to.
fn significant(lang: &Language, src: &str) -> u64 {
    lang.lex(src).iter().filter(|t| !t.is_trivia()).count() as u64
}

/// The `ERROR` nodes of a tree, as the text each covers.
fn error_texts<'s>(lang: &Language, tree: &Node<Kind>, src: &'s str) -> Vec<&'s str> {
    let error = lang.kind("ERROR").expect("built in");
    core::iter::once(tree)
        .chain(tree.descendants())
        .filter(|n| *n.kind() == error)
        .map(|n| &src[n.span().start().to_usize()..n.span().end().to_usize()])
        .collect()
}

/// What a parse of `src` came to, for assertions off the parsing thread.
#[derive(Debug)]
struct Outcome {
    codes: Vec<String>,
    steps: u64,
    tokens: u64,
    lossless: bool,
    /// Where the first diagnostic points, as a byte offset.
    at: Option<usize>,
    /// The text of the last `ERROR` node.
    unparsed: Option<String>,
}

fn outcome(lang: &Language, src: &str) -> Outcome {
    let parse = lang.parse(src);
    Outcome {
        codes: codes(&parse),
        steps: parse.steps(),
        tokens: significant(lang, src),
        lossless: parse.tree().text(src) == Some(src),
        at: parse
            .diagnostics()
            .first()
            .map(|d| d.primary().span().start().to_usize()),
        unparsed: error_texts(lang, parse.tree(), src)
            .last()
            .map(|s| (*s).to_owned()),
    }
}

/// Mox nested `open … close` `levels` deep around `1`, as the right-hand
/// side of an assignment, followed by a statement that would be an error.
fn mox_nest(open: &str, close: &str, levels: usize) -> String {
    format!(
        "<?mox $x = {}1{};\n$y = ;\n",
        open.repeat(levels),
        close.repeat(levels)
    )
}

/// Nests `open … close` at 50, 60, 100 and 10,000 levels. Up to `fits`
/// levels fit under the depth limit; past it, the parse stops.
fn assert_mox_nesting_stops_in_linear_time(open: &'static str, close: &'static str, fits: usize) {
    let lang = mox();
    let started = Instant::now();
    let results = on_stack(1024, move || {
        [50, 60, 100, 10_000].map(|levels| (levels, outcome(&lang, &mox_nest(open, close, levels))))
    });
    let elapsed = started.elapsed();
    for (levels, out) in &results {
        assert!(out.lossless, "{levels} levels: {out:?}");
        // Linear: a bounded number of steps per token, however deep.
        assert!(out.steps <= 1_000 * out.tokens, "{levels} levels: {out:?}");
        if *levels <= fits {
            // Only the error in the statement after the nesting.
            assert_eq!(out.codes, ["LF1000"], "{levels} levels: {out:?}");
            continue;
        }
        // Past the limit the parse stops at once: one diagnostic, at the
        // deepest point, after the first descent's thousand-odd steps, at ten
        // thousand levels as at sixty. Nothing after the stop is parsed, so
        // the `$y = ;` error is not reported: it is in the unparsed `ERROR`
        // node.
        assert_eq!(out.codes, ["LF1003"], "{levels} levels: {out:?}");
        assert!(out.steps < 2_000, "{levels} levels: {out:?}");
        let at = out.at.expect("located");
        let nest_start = "<?mox $x = ".len();
        assert!(
            at >= nest_start + open.len() * 40 && at < nest_start + open.len() * levels,
            "{levels} levels: the stop is deep inside the nesting: {out:?}"
        );
        let unparsed = out.unparsed.as_deref().expect("an ERROR node");
        assert!(unparsed.ends_with("$y = ;"), "{levels} levels: {out:?}");
    }
    // Generous for a debug build; 2.0.0-alpha.1 never finished sixty levels
    // of arrays, or sixty-five of `list(`.
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn test_mox_nested_arrays_stop_in_linear_time() {
    assert_mox_nesting_stops_in_linear_time("[", "]", 53);
}

#[test]
fn test_mox_nested_list_calls_stop_in_linear_time() {
    assert_mox_nesting_stops_in_linear_time("list(", ")", 62);
}

#[test]
fn test_mox_nesting_just_under_the_limit_parses_cleanly() {
    // The deepest nesting the Mox sketch takes, with nothing else wrong.
    let lang = mox();
    let results = on_stack(1024, move || {
        [("[", "]", 53), ("list(", ")", 62)].map(|(open, close, levels)| {
            let src = format!(
                "<?mox $x = {}1{};\n",
                open.repeat(levels),
                close.repeat(levels)
            );
            outcome(&lang, &src)
        })
    });
    for out in results {
        assert!(out.codes.is_empty(), "{out:?}");
        assert!(out.lossless);
    }
}

/// A statement grammar whose statement list recurses through `NEWLINE`: at
/// the end of the input (which stands for a line break) the progress guard
/// cuts `stmts â†’ NEWLINE stmts`, and every statement speculates.
fn newline_language() -> Language {
    Language::from_lsf(
        "[language]\nname = \"nl\"\n[lexer]\nnewlines = true\n[rules]\n\
         file = \"stmts\"\nstmts = \"NEWLINE stmts | stmt (NEWLINE stmts)?\"\n\
         stmt = \"assign | expr\"\nassign = \"expr '=' expr\"\nblock = \"'{' stmts? '}'\"\n\
         [rules.expr]\noperand = \"IDENT | block\"\nlevels = [{ left = [\"+\"] }]\n",
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn test_progress_guard_cycles_stay_linear() {
    let lang = newline_language();
    let started = Instant::now();
    let results = on_stack(1024, move || {
        [10, 22, 40, 60, 10_000].map(|levels| {
            let src = format!("{}x", "{ ".repeat(levels));
            (levels, outcome(&lang, &src))
        })
    });
    for (levels, out) in &results {
        assert!(out.lossless, "{levels}: {out:?}");
        assert!(out.steps <= 64 * out.tokens, "{levels}: {out:?}");
        assert_eq!(out.codes.len(), 1, "{levels}: {out:?}");
    }
    // Under the limit the one error is the missing `}`; past it, the stop.
    assert_eq!(results[2].1.codes, ["LF1000"]);
    assert_eq!(results[4].1.codes, ["LF1003"]);
    // 2.0.0-alpha.1 took 2.6 s at 22 levels (release) and doubled per level.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// A grammar whose cost at the end of the input is exponential in its size:
/// `k` levels of `r = x | y`, both alternatives viable on the line break the
/// end of the input stands for, and a cycle back to the first level that the
/// progress guard cuts. No result inside the cycle can be remembered (each
/// depends on the outermost rule being active), so every level doubles the
/// work: the budget is what ends it.
fn hostile_grammar(k: usize) -> String {
    let mut rules = String::from("file = \"r0\"\n");
    for i in 0..k {
        rules.push_str(&format!(
            "r{i} = \"x{i} | y{i}\"\nx{i} = \"NEWLINE r{next} 'q'\"\ny{i} = \"NEWLINE r{next}\"\n",
            next = i + 1
        ));
    }
    rules.push_str(&format!("r{k} = \"NEWLINE r0\"\n"));
    format!("[language]\nname = \"hostile\"\n[lexer]\nnewlines = true\n[rules]\n{rules}")
}

#[test]
fn test_hostile_grammar_ends_with_a_budget_error() {
    let lang = Language::from_lsf(&hostile_grammar(40)).unwrap_or_else(|e| panic!("{e}"));
    let started = Instant::now();
    for src in ["", "\n", "\n\n\n"] {
        let parse = lang.parse(src);
        assert_eq!(
            codes(&parse).first().map(String::as_str),
            Some("LF9002"),
            "{src:?}"
        );
        assert_eq!(parse.tree().text(src), Some(src));
        let budget = ParseBudget::DEFAULT.steps(significant(&lang, src) as usize);
        assert!(parse.steps() <= budget, "{} > {budget}", parse.steps());
        assert!(parse.steps() >= budget - 64);
    }
    // A tighter budget ends it sooner.
    let tight = lang.with_parse_budget(ParseBudget::new(10_000, 0));
    let parse = tight.parse("");
    assert_eq!(codes(&parse), ["LF9002"]);
    assert!(parse.steps() <= 10_000);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

/// Speculation that scans far ahead from every token: `a` runs through every
/// identifier left and fails at the end, then `b` takes one. The memo lasts
/// one speculation, so the work is quadratic in the input. Its steps must
/// show it (tokens matched in a strict attempt are steps: in alpha.1 they
/// were not, and such a parse ran for seconds on a budget it never reached),
/// and the default budget must stop it.
#[test]
fn test_quadratic_speculation_is_counted_and_stopped() {
    let lang = Language::from_lsf(
        "[language]\nname = \"q\"\n[rules]\nfile = \"stmt*\"\nstmt = \"a | b\"\na = \"IDENT* ';'\"\nb = \"IDENT\"\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let unlimited = lang
        .clone()
        .with_parse_budget(ParseBudget::new(u64::MAX, 0));
    let steps = |n: usize| unlimited.parse(&"x ".repeat(n)).steps();
    let (one, two) = (steps(500), steps(1_000));
    assert!(two > 3 * one, "{one} {two}: the quadratic work is counted");

    let started = Instant::now();
    let src = "x ".repeat(20_000);
    let parse = lang.parse(&src);
    assert_eq!(codes(&parse), ["LF9002"]);
    assert_eq!(parse.tree().text(&src), Some(src.as_str()));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn test_budget_stop_keeps_the_tree_lossless() {
    let lang = Language::from_lsf(include_str!("../examples/schematics/mini.lsf"))
        .expect("forges")
        .with_parse_budget(ParseBudget::new(50, 0));
    let src = "let a = 1;\nlet b = 2;\n".repeat(20);
    let parse = lang.parse(&src);
    assert_eq!(codes(&parse), ["LF9002"]);
    assert_eq!(parse.steps(), 50);
    assert_eq!(parse.tree().text(&src), Some(src.as_str()));
    let message = parse.diagnostics()[0].message();
    assert_eq!(
        message,
        "the parse took more than 50 steps, the budget for this input"
    );
    // The statements before the stop are parsed; the rest is one node.
    let unparsed = error_texts(&lang, parse.tree(), &src);
    assert_eq!(unparsed.len(), 1);
    assert!(src.trim_end().ends_with(unparsed[0]));
    assert!(unparsed[0].len() < src.len());
}

#[test]
fn test_budget_covers_parse_file_and_injections() {
    let sketch = r#"
        [sketch]
        format = 2
        [language]
        name = "tpl"
        version = "1.0.0"
        extensions = ["tpl", "item"]
        files = { item = { start = "ref" } }
        [lexer.strings.QUOTED]
        open = '"'
        embedded = [{ token = "REF", regex = '\$[a-z]+', parse = "ref" }]
        [rules]
        file = "(QUOTED | ref)*"
        ref = "'$' IDENT"
    "#;
    let lang = Language::from_lsf(sketch).unwrap_or_else(|e| panic!("{e}"));
    // Injected parses count toward the steps.
    let plain = lang.parse("$a").steps();
    let injected = lang.parse(r#""$a $b""#).steps();
    assert!(injected > plain, "{injected} vs {plain}");

    let tight = lang.with_parse_budget(ParseBudget::new(3, 0));
    let src = "$a $b $c $d $e";
    let parse = tight.parse_file("tpl", src);
    assert_eq!(codes(&parse), ["LF9002"]);
    assert_eq!(parse.tree().text(src), Some(src));
    // `item` files start at `ref`: three steps (the rule's sequence and its
    // two tokens) fit in three, not in two.
    assert!(!tight.parse_file("item", "$a").has_errors());
    let tighter = tight.clone().with_parse_budget(ParseBudget::new(2, 0));
    assert_eq!(codes(&tighter.parse_file("item", "$a")), ["LF9002"]);

    // The budget is a setting of the value, not part of the language.
    let loaded = Language::from_image(&tight.to_image()).expect("loads");
    assert_eq!(tight.parse_budget(), ParseBudget::new(3, 0));
    assert_eq!(loaded.parse_budget(), ParseBudget::DEFAULT);
}

#[test]
fn test_ordinary_input_stays_far_below_the_default_budget() {
    let corpora = [
        (
            include_str!("../examples/schematics/mini.lsf"),
            "fn f(a, b) { let x = (a + b) * 2; while x < 10 { x = x + 1; } return g(x, -1); }\n",
        ),
        (
            include_str!("../examples/schematics/json.lsf"),
            r#"{"a": [1, 2, {"b": null, "c": [true, false, "s"]}], "d": -1.5e3}"#,
        ),
        (MOX, include_str!("mox/script.mox")),
        (MOX, include_str!("mox/template.mox")),
    ];
    for (sketch, src) in corpora {
        let lang = Language::from_lsf(sketch).unwrap_or_else(|e| panic!("{e}"));
        let parse = lang.parse(src);
        assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
        let tokens = significant(&lang, src);
        // At most a few dozen steps per token (3 to 15 measured); the default
        // allows 4,096.
        assert!(
            parse.steps() <= 64 * tokens,
            "{} steps for {tokens} tokens",
            parse.steps()
        );
    }
}

// ----- a linear step bound for any nesting -----

/// A small grammar with the shapes that cost: statements that speculate
/// (`pair` and `value ';'` both begin with a value), a Mox-style predicate in
/// front of every list entry (`&(value ':')`, which parses a whole value
/// before the entry does), calls, and parentheses.
const NEST: &str = r#"
[sketch]
format = 2
[language]
name = "nest"
version = "1.0.0"
[rules]
file  = "items:item*"
item  = "pair | value ';'"
pair  = "key:value '=' value:value ';'"
value = "list | call | group | IDENT | NUMBER"
list  = "'[' (entries:entry (',' entries:entry)*)? ']'"
entry = "(&(value ':') key:value ':')? value:value"
call  = "callee:IDENT '(' (args:value (',' args:value)*)? ')'"
group = "'(' inner:value ')'"
"#;

/// One level of nesting: the text before and after the inner value.
const LEVELS: [(&str, &str); 5] = [
    ("[", "]"),
    ("[k: ", "]"),
    ("f(", ")"),
    ("(", ")"),
    ("[1, ", ", 2]"),
];

/// Text a level may be cut short by, or have in place of its close.
const DEBRIS: [&str; 5] = ["", ";", "=", "]", ")"];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// However deep and however broken the nesting, the parser takes a
    /// bounded number of steps per token: with no budget to stop it, a path
    /// that multiplied its work with depth would show here as a step count
    /// far past the bound (or never finish). This grammar peaks at about 33
    /// steps per token, on lists nested just under the depth limit (each
    /// entry's predicate parses the entry once more); work that grew with
    /// depth even linearly would pass 64 well before the limit.
    #[test]
    fn prop_any_nesting_takes_linear_steps(
        levels in prop_oneof![
            // Mostly around the depth limit, where the work per token peaks.
            4 => proptest::collection::vec(0..LEVELS.len(), 0..90),
            1 => proptest::collection::vec(0..LEVELS.len(), 0..3_000),
        ],
        closed in 0usize..3_001,
        debris in 0..DEBRIS.len(),
        statements in 1usize..4,
    ) {
        let lang = Language::from_lsf(NEST)
            .unwrap_or_else(|e| panic!("{e}"))
            .with_parse_budget(ParseBudget::new(u64::MAX, 0));
        let mut one = String::new();
        for &l in &levels {
            one.push_str(LEVELS[l].0);
        }
        one.push('x');
        // Close some of the levels, innermost first, then the debris.
        for &l in levels.iter().rev().take(closed) {
            one.push_str(LEVELS[l].1);
        }
        one.push_str(DEBRIS[debris]);
        one.push_str(";\n");
        let src = one.repeat(statements);
        let (steps, tokens, lossless, stops) = on_stack(1024, {
            let src = src.clone();
            move || {
                let parse = lang.parse(&src);
                let stops = codes(&parse).iter().filter(|c| c.starts_with("LF1003") || c.starts_with("LF9")).count();
                (parse.steps(), significant(&lang, &src), parse.tree().text(&src) == Some(src.as_str()), stops)
            }
        });
        prop_assert!(lossless);
        prop_assert!(stops <= 1);
        prop_assert!(steps <= 64 * (tokens + 1), "{} steps for {} tokens: {:?}", steps, tokens, src);
    }
}
