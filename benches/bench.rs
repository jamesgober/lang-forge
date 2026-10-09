//! Criterion benchmarks: forging, lexing, and parsing (format 1), and the
//! format-2 Mox front end.
//!
//! Inputs are generated, so every run measures the same text: realistic mini
//! code and JSON of about 1 MB, a long operator chain, and token soup that
//! keeps the recovery paths busy.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use lang_forge::Language;

const MINI: &str = include_str!("../examples/schematics/mini.lsf");
const JSON: &str = include_str!("../examples/schematics/json.lsf");
const CALC: &str = include_str!("../examples/schematics/calc.lsf");
const MOX: &str = include_str!("../tests/sketches/mox.lsf");
const MOX_SCRIPT: &str = include_str!("../tests/mox/script.mox");
const MOX_TEMPLATE: &str = include_str!("../tests/mox/template.mox");
const SPECULATIVE: &str = r#"
[language]
name = "spec"

[rules]
file   = "stmt*"
stmt   = "assign | expr ';'"
assign = "expr '=' expr ';'"
block  = "'{' stmt* '}'"
call   = "IDENT '(' (expr (',' expr)*)? ')'"

[rules.expr]
operand = "call | IDENT | NUMBER | block"
levels  = [{ left = ["+", "-"] }, { left = ["*"] }]
"#;

/// About `bytes` of mini code: functions with loops, branches, and calls.
fn mini_source(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 512);
    let mut i = 0;
    while out.len() < bytes {
        out.push_str(&format!(
            "// Function number {i}.\n\
             fn work_{i}(a, b, limit) {{\n\
             \x20   let total = 0;\n\
             \x20   let step = (a + b) * 2 - limit % 7;\n\
             \x20   while total < limit && step != 0 {{\n\
             \x20       if total == 42 || !ready(total) {{\n\
             \x20           total = total + step / 3;\n\
             \x20       }} else {{\n\
             \x20           log(\"step\", total, -step);\n\
             \x20       }}\n\
             \x20       total = total + 1;\n\
             \x20   }}\n\
             \x20   return total;\n\
             }}\n\n"
        ));
        i += 1;
    }
    out
}

/// About `bytes` of JSON: an array of records.
fn json_source(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 256);
    out.push('[');
    let mut i = 0;
    while out.len() < bytes {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "\n  {{\"id\": {i}, \"name\": \"item-{i}\", \"price\": {}.{:02}, \"tags\": [\"a\", \"b\", \"c\"], \
             \"active\": true, \"parent\": null, \"dims\": {{\"w\": 1.5e2, \"h\": -3}}}}",
            i % 1000,
            i % 100
        ));
        i += 1;
    }
    out.push_str("\n]\n");
    out
}

fn bench_forge(c: &mut Criterion) {
    let mut group = c.benchmark_group("forge");
    for (name, schematic) in [("mini", MINI), ("json", JSON), ("calc", CALC)] {
        group.bench_function(name, |b| {
            b.iter(|| Language::from_lsf(black_box(schematic)))
        });
    }
    group.finish();
}

fn bench_lex(c: &mut Criterion) {
    let mini = Language::from_lsf(MINI).expect("forges");
    let json = Language::from_lsf(JSON).expect("forges");
    let mut group = c.benchmark_group("lex");
    for (name, lang, src) in [
        ("mini/1MB", &mini, mini_source(1 << 20)),
        ("json/1MB", &json, json_source(1 << 20)),
    ] {
        group.throughput(Throughput::Bytes(src.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), &src, |b, src| {
            b.iter(|| lang.lex(black_box(src)));
        });
    }
    group.finish();
}

fn bench_parse(c: &mut Criterion) {
    let mini = Language::from_lsf(MINI).expect("forges");
    let json = Language::from_lsf(JSON).expect("forges");
    let calc = Language::from_lsf(CALC).expect("forges");
    let mut group = c.benchmark_group("parse");
    group.sample_size(30);

    // Every statement speculates: assignment and expression statement both
    // begin with an expression, which may hold blocks of statements.
    let speculative = Language::from_lsf(SPECULATIVE).expect("forges");
    let mut statements = String::new();
    while statements.len() < 256 << 10 {
        statements.push_str("total = f(total, { x = 1; y + { z; }; }) + 2;\ncount(total);\n");
    }

    // The same kind of statements nested 60 blocks deep: every level's first
    // alternative fails only after parsing the whole level, so each level is
    // rewound and replayed — the memo's worst case.
    let mut nested = String::new();
    for _ in 0..60 {
        nested.push_str("{ ");
        for i in 0..200 {
            nested.push_str(&format!("f(x{i}) + 1; "));
        }
    }
    nested.push_str("x;");
    nested.push_str(&" };".repeat(60));

    let inputs = [
        ("speculative/256KB", &speculative, statements),
        ("speculative/nested_60", &speculative, nested),
        ("mini/4KB", &mini, mini_source(4 << 10)),
        ("mini/1MB", &mini, mini_source(1 << 20)),
        ("json/1MB", &json, json_source(1 << 20)),
        (
            "calc/chain_100k",
            &calc,
            format!("{};", vec!["x"; 100_000].join(" + ")),
        ),
    ];
    for (name, lang, src) in &inputs {
        group.throughput(Throughput::Bytes(src.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), src, |b, src| {
            b.iter(|| lang.parse(black_box(src)));
        });
    }

    // Recovery: every few tokens something is missing or out of place.
    let soup: String = (0..20_000)
        .map(|i| match i % 5 {
            0 => "let x = ;",
            1 => "f(a, b;",
            2 => ") } ,",
            3 => "while { }",
            _ => "y = 1 + * 2;",
        })
        .collect::<Vec<_>>()
        .join("\n");
    group.throughput(Throughput::Bytes(soup.len() as u64));
    group.bench_with_input(
        BenchmarkId::from_parameter("mini/errors"),
        &soup,
        |b, src| {
            b.iter(|| mini.parse(black_box(src)));
        },
    );
    group.finish();
}

/// About `bytes` of Mox script: the representative script's code repeated
/// inside one `<?mox` island.
fn mox_script(bytes: usize) -> String {
    let body = MOX_SCRIPT
        .strip_prefix("<?mox\n")
        .expect("the script opens an island");
    let mut out = String::from("<?mox\n");
    while out.len() < bytes {
        out.push_str(body);
    }
    out
}

/// About `bytes` of Mox template: text with `<?mox` and `<?=` islands.
fn mox_template(bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes {
        out.push_str(MOX_TEMPLATE);
    }
    out
}

/// Format 2 (new in 2.0): forging the Mox sketch (modes, string classes,
/// labels, the overlap check), loading its image, and lexing and parsing Mox
/// scripts and templates. Kept out of the 1.x groups, whose names and inputs
/// are unchanged so they compare against the 1.x baseline.
fn bench_v2(c: &mut Criterion) {
    let mut group = c.benchmark_group("v2");
    group.sample_size(30);
    group.bench_function("forge/mox", |b| {
        b.iter(|| Language::from_lsf(black_box(MOX)))
    });
    let mox = Language::from_lsf(MOX).expect("forges");
    let image = mox.to_image();
    group.bench_function("image/mox/to", |b| b.iter(|| black_box(&mox).to_image()));
    group.bench_function("image/mox/from", |b| {
        b.iter(|| Language::from_image(black_box(&image)))
    });
    for (name, src) in [
        ("mox_script/1MB", mox_script(1 << 20)),
        ("mox_template/256KB", mox_template(256 << 10)),
    ] {
        group.throughput(Throughput::Bytes(src.len() as u64));
        group.bench_with_input(BenchmarkId::new("lex", name), &src, |b, src| {
            b.iter(|| mox.lex(black_box(src)));
        });
        group.bench_with_input(BenchmarkId::new("parse", name), &src, |b, src| {
            b.iter(|| mox.parse(black_box(src)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_forge, bench_lex, bench_parse, bench_v2);
criterion_main!(benches);
