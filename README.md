<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>lang-forge</b>
    <br>
    <sub><sup>LEXERSKETCH GENERATOR</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/lang-forge"><img alt="Crates.io" src="https://img.shields.io/crates/v/lang-forge"></a>
    <a href="https://crates.io/crates/lang-forge"><img alt="Downloads" src="https://img.shields.io/crates/d/lang-forge?color=%230099ff"></a>
    <a href="https://docs.rs/lang-forge"><img alt="docs.rs" src="https://img.shields.io/docsrs/lang-forge"></a>
    <a href="https://github.com/jamesgober/lang-forge/actions"><img alt="CI" src="https://github.com/jamesgober/lang-forge/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        <strong>lang-forge</strong> is LexerSketch: describe a language in a short <code>.lsf</code> schematic &mdash; its tokens, its grammar, its operators and their precedence &mdash; and get a working front end for it. <code>Language::from_lsf</code> reads the schematic, checks it, and compiles it into tables; <code>Language::parse</code> then turns source text into a lossless syntax tree, with a diagnostic for everything malformed. There is no code to generate and nothing to build: the language is ready the moment the schematic is forged.
    </p>
    <p>
        It is the capstone of the <code>-lang</code> language-construction family. Trees are <a href="https://crates.io/crates/syntax-lang"><code>syntax-lang</code></a> trees, the lossless CST that the family's formatter, incremental reparser, language server, and tree-sitter crates are designed to consume; the adapters that connect a forged language to those crates are not part of lang-forge and arrive with LexerSketch. Problems are <a href="https://crates.io/crates/diag-lang"><code>diag-lang</code></a> diagnostics, rendered with carets under the source; and the passes a language includes run on <a href="https://crates.io/crates/pass-lang"><code>pass-lang</code></a>. A schematic never bakes a feature in &mdash; it names the capabilities it wants, and the language wires those passes in.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). <code>no_std</code>-compatible (needs only <code>alloc</code>), <code>#![forbid(unsafe_code)]</code>, no dependencies outside the <code>-lang</code> family.
    </p>
    <blockquote>
        <strong>1.0.0 is the API freeze.</strong> The public surface, the schematic format, and the parser's guarantees are stable and follow Semantic Versioning &mdash; no breaking changes before <code>2.0</code>. See <a href="./docs/API.md#stability"><code>docs/API.md</code></a> for the frozen surface and the SemVer promise, and <a href="./CHANGELOG.md"><code>CHANGELOG.md</code></a>.
    </blockquote>
</div>

<hr>
<br>

## The model

Four types and one alias, one per job:

- A **[`Language`](./docs/API.md#language)** is a forged language: a lexer, a parser, and the kinds of its tree. It is immutable, `Send` and `Sync`, and parses as often as you like.
- A **[`Parse`](./docs/API.md#parse)** is one result: the lossless tree, the source, and the diagnostics. Parsing never fails.
- A **[`Kind`](./docs/API.md#kind)** names a token or node in the tree. It is a small `Copy` value that compares like an enum.
- An **[`Error`](./docs/API.md#error)** lists everything wrong with a schematic, each problem pointing into the schematic text.
- A **[`Capability`](./docs/API.md#capability)** is a boxed `pass-lang` pass that a schematic can include by name.

<br>

What the crate guarantees:

| Guarantee | How it is held |
|---|---|
| Parsing never fails, never panics, and never overflows the stack. | Recovery is built into the parser. Recursion is bounded at 768 grammar levels — at most about 256 KiB of stack in release builds, 768 KiB in debug — and deeper input is reported, not followed. Property tests throw random text and token soup at every example language. |
| Every tree is lossless: it covers every byte of the source, whitespace and comments included. | Tested on every parse in the suite, and as a property over arbitrary input. |
| On input without errors, the tree is the one the grammar describes. | A strict reference parser runs in the tests; whenever the recovering parser reports no error, the two trees must be identical. |
| A schematic the parser could not run is refused when it is forged. | Left recursion, repetitions that could loop forever, unreachable alternatives, and literals the lexer cannot produce are all errors, with the rule and the fix named. |
| Forging a hostile schematic takes bounded memory. | The schematic's size (8 MiB), nesting (64 levels), and the tables that grow with rules times token kinds (256 MiB) are capped, and checked before anything that size is allocated. A counting allocator in [`tests/memory.rs`](./tests/memory.rs) measures it. |

<hr>
<br>

## Installation

```toml
[dependencies]
lang-forge = "1"
```

Or from the terminal:

```bash
cargo add lang-forge
```

`syntax-lang`, `diag-lang`, and `pass-lang` are re-exported as `lang_forge::syntax_lang`, `lang_forge::diag_lang`, and `lang_forge::pass_lang`, so you need not depend on them yourself. MSRV: Rust 1.85 (Rust 2024 edition).

<hr>
<br>

## Quick start

A calculator, from schematic to evaluated tree:

```rust
use lang_forge::Language;

let calc = Language::from_lsf(r##"
    [language]
    name = "calc"

    [lexer]
    line_comments = ["#"]

    [rules]
    program = "stmt*"
    stmt    = "'let' IDENT '=' expr ';' | expr ';'"
    group   = "'(' expr ')'"

    [rules.expr]
    operand = "NUMBER | IDENT | group"
    levels  = [
        { left   = ["+", "-"] },
        { left   = ["*", "/"] },
        { prefix = ["-"] },
        { right  = ["^"] },
    ]
"##)?;

let parse = calc.parse("let r = 2;\nlet area = 3 * r ^ 2;  # precedence decides\n");
assert!(!parse.has_errors());

// `^` binds tighter than `*`: the product's right operand is `r ^ 2`.
let binary = calc.kind("binary").expect("the default operator node");
let product = parse.tree().descendants().find(|n| *n.kind() == binary).expect("3 * r ^ 2");
assert_eq!(product.text(parse.source()), Some("3 * r ^ 2"));
# Ok::<(), lang_forge::Error>(())
```

`parse.dump()` shows the tree, one node or token per line:

```text
program@0..55
  stmt@0..10
    let@0..3 "let"
    WHITESPACE@3..4 " "
    IDENT@4..5 "r"
    ...
```

### Mistakes in source text

Every problem becomes a diagnostic, and the tree is complete regardless — so an editor can still highlight and navigate a half-typed file:

```rust
use lang_forge::Language;
use lang_forge::diag_lang::{Renderer, SourceMap};

let lang = Language::from_lsf(
    "[language]\nname = \"calls\"\n[rules]\nfile = \"call*\"\ncall = \"IDENT '(' (IDENT (',' IDENT)*)? ')' ';'\"\n",
)?;
let source = "open(file);\nread(file, buffer;\nclose(file);\n";
let parse = lang.parse(source);
assert_eq!(parse.diagnostics().len(), 1);
assert_eq!(parse.tree().text(source), Some(source));

let mut map = SourceMap::new();
map.add("main.calls", source).expect("fits");
let report = Renderer::new().render(&parse.diagnostics()[0], &map);
assert!(report.contains("error: expected `)`, found `;`"));
assert!(report.contains("main.calls:2:18"));
# Ok::<(), lang_forge::Error>(())
```

```text
error: expected `)`, found `;`
 --> main.calls:2:18
  |
2 | read(file, buffer;
  |                  ^
```

### Mistakes in the schematic

Forging reports everything wrong at once, each problem pointing into the schematic, many with the fix:

```rust
use lang_forge::Language;

let err = Language::from_lsf(r#"
[language]
name = "broken"

[rules]
program = "statment*"
statement = "expr ';'"
expr = "expr '+' NUMBER | NUMBER"
"#).unwrap_err();

let messages: Vec<&str> = err.diagnostics().iter().map(|d| d.message()).collect();
assert_eq!(messages, [
    "undefined rule `statment`",
    "rule `expr` is left-recursive: expr → expr",
]);
let help: Vec<&str> = err.diagnostics()[0].help().collect();
assert_eq!(help, ["did you mean `statement`?"]);
```

<hr>
<br>

## A schematic, table by table

A schematic is a NOML document — the TOML-compatible core of it — with up to four tables. Everything except `[language] name` and `[rules]` is optional.

```toml
[language]
name       = "mini"            # required
version    = "0.1.0"
extensions = ["mini"]          # without the dot
start      = "program"         # the root rule; default: the first rule

[lexer]
identifiers     = "xid"        # Unicode identifiers (UAX #31); or "ascii"
newlines        = false        # true: line breaks are NEWLINE tokens
line_comments   = ["//"]
block_comments  = [["/*", "*/"]]
nested_comments = true
strings         = ['"', { open = "#\"", close = "\"#", escape = "", multiline = true }]

[rules]
program  = "_item*"            # `_` hides a rule: its children join the parent
_item    = "function | stmt"
function = "'fn' IDENT params block"
params   = "'(' (IDENT (',' IDENT)*)? ')'"
block    = "'{' stmt* '}'"
stmt     = "'let' IDENT '=' expr ';' | 'return' expr? ';' | expr ';'"
args     = "expr (',' expr)*"
group    = "'(' expr ')'"

[rules.expr]                   # an expression rule: operand and operator levels
operand = "NUMBER | STRING | IDENT | group"
levels  = [                    # lowest precedence first
    { right   = ["="],                 node = "assign" },
    { left    = ["||"] },
    { left    = ["&&"] },
    { none    = ["==", "!=", "<", ">"], node = "compare" },
    { left    = ["+", "-"] },
    { left    = ["*", "/", "%"] },
    { prefix  = ["-", "!"],            node = "unary" },
    { postfix = ["("], then = "args? ')'", node = "call" },
]

[capabilities]
include = ["unused-variables"] # passes, run in this order
```

**Rules** are sequences of elements: quoted literals (`'let'`, `'+='` — keywords and symbols, collected from the grammar into the lexer automatically), the token classes `IDENT`, `NUMBER`, `STRING`, and `NEWLINE`, rule names, grouping with `( )`, `|` for alternatives tried in order, and `*`, `+`, `?`. Each rule builds a node named after itself.

**Expression rules** handle operators without left recursion: list the levels from loosest to tightest, each `left`, `right`, or `none` (binary), `prefix`, or `postfix`. A `then` adds grammar after the operator — calls, indexing, `? :`. Operator nodes are named `binary`, `prefix`, and `postfix` unless a level sets `node`.

**The lexer** is derived, not written: every literal in the grammar becomes a keyword (if it reads like an identifier) or a symbol, matched longest first. Numbers (`42`, `1_000`, `3.25e-4`, `0xFF`, `0o17`, `0b1010`), identifiers, whitespace, and the comments and strings `[lexer]` declares are built in.

The full reference — every key, type, default, and the semantics of parsing and recovery — is in [`docs/API.md`](./docs/API.md#the-schematic).

<hr>
<br>

## Capabilities

LexerSketch never bakes a language feature in. A schematic names the capabilities it includes; the language assembles them, in order, from whatever registry of passes you provide, and refuses if one is missing:

```rust
use lang_forge::diag_lang::{Diagnostic, Label, Severity};
use lang_forge::pass_lang::{Outcome, Pass, PassError};
use lang_forge::{Capability, Language, Parse};

/// Warns about empty blocks.
struct EmptyBlocks;

impl<'a> Pass<Parse<'a>> for EmptyBlocks {
    fn name(&self) -> &'static str {
        "empty-blocks"
    }

    fn run(&mut self, parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
        let block = parse.language().kind("block").ok_or_else(|| PassError::new("no blocks"))?;
        let empty: Vec<_> = parse
            .tree()
            .descendants()
            .filter(|n| *n.kind() == block && n.child_nodes().next().is_none())
            .map(|n| n.span())
            .collect();
        for span in empty {
            parse.report(Diagnostic::new(Severity::Warning, "empty block", Label::unlabelled(span)));
        }
        Ok(Outcome::Unchanged)
    }
}

let lang = Language::from_lsf(
    "[language]\nname = \"blocks\"\n[rules]\nfile = \"block*\"\nblock = \"'{' IDENT* '}'\"\n\
     [capabilities]\ninclude = [\"empty-blocks\"]\n",
)?;

let registry: Vec<Capability> = vec![Box::new(EmptyBlocks)];
let mut pipeline = lang.pipeline(registry)?;

let mut parse = lang.parse("{ a b } { }");
pipeline.run(&mut parse).expect("the pass runs");
assert_eq!(parse.diagnostics()[0].message(), "empty block");
assert!(!parse.has_errors()); // a warning, not an error
# Ok::<(), lang_forge::Error>(())
```

<hr>
<br>

## Examples

Five runnable examples ship in [`examples/`](./examples), with four schematics in [`examples/schematics/`](./examples/schematics): `calc.lsf`, `json.lsf`, `mini.lsf` (functions, control flow, eight precedence levels), and `conf.lsf` (an INI-style format where line breaks matter).

- **Calc** — forges `calc.lsf`, parses a program, and evaluates it by walking the tree: the smallest complete interpreter.
  ```bash
  cargo run --example calc
  ```
- **Check** — a command-line checker for any forged language: forge a schematic, parse a file, render every problem with source context, and optionally print the tree.
  ```bash
  cargo run --example check                                  # a built-in sample with two mistakes
  cargo run --example check -- --tree schematic.lsf file.src
  ```
- **JSON** — a JSON validator from `json.lsf`, accepting good documents and showing the first error in bad ones.
  ```bash
  cargo run --example json
  ```
- **Capabilities** — `mini.lsf` includes `unused-variables`; this example registers that pass (and one the language does not ask for), assembles the pipeline, and reports an unused binding.
  ```bash
  cargo run --example capabilities
  ```
- **Highlight** — syntax highlighting from `Language::lex` alone: every token, trivia included, painted by kind.
  ```bash
  cargo run --example highlight
  ```

<hr>
<br>

## Performance

Forging does the analysis once — every rule resolved, every set computed — so parsing consults precomputed tables instead of the grammar. The lexer dispatches on a 256-entry byte-class table, runs tight ASCII loops for identifiers, numbers, and whitespace, matches keywords through an open-addressing hash table and symbols longest-first from a short per-byte list, and only decodes UTF-8 on non-ASCII bytes. The parser tests FIRST sets as bitsets before trying anything, so it commits to the one viable alternative in almost every decision and speculates only where the grammar is genuinely ambiguous at one token; speculative attempts are memoized by rule and position, so alternatives that share a prefix never parse it twice and parsing stays linear in the input (at most, each level of speculative nesting replays what it holds once more). A remembered attempt's events are kept where the parser produced them, and moved — once, with nested replays kept as references — only when a failed alternative rewinds over them, so the memo's memory stays proportional to the input however deeply it nests. It records a flat list of events instead of building the tree as it goes, which makes speculation a truncation and operator nodes a forward link rather than an insertion. Error paths are kept cold and out of line, so the hot recursive functions stay small.

Measured with the benchmarks in [`benches/`](./benches), x86_64, Rust stable, release profile. The parse figures cover lexing, parsing, building the tree, and dropping it:

| Benchmark | What it measures | Windows | Linux (WSL2) |
|---|---|---:|---:|
| `forge/mini` | Read, check, and compile `mini.lsf`. | ~38 µs | ~23 µs |
| `forge/json` | The same for `json.lsf`. | ~15 µs | ~9 µs |
| `lex/mini/1MB` | Tokenize 1 MB of mini code. | ~3.7 ms (270 MiB/s) | ~1.8 ms (555 MiB/s) |
| `lex/json/1MB` | Tokenize 1 MB of JSON. | ~3.9 ms | ~2.1 ms |
| `parse/mini/4KB` | Parse a 4 KB mini file. | ~94 µs | ~72 µs |
| `parse/mini/1MB` | Parse 1 MB of mini code (147,000 nodes). | ~57 ms | ~24 ms |
| `parse/json/1MB` | Parse 1 MB of JSON. | ~60 ms | ~34 ms |
| `parse/speculative/256KB` | A grammar where every statement speculates (assignment or expression, nested blocks). | ~33 ms | ~13 ms |
| `parse/speculative/nested_60` | The same statements nested 60 blocks deep, every level rewinding its first alternative (146 KB). | ~38 ms | — |
| `parse/calc/chain_100k` | A 100,000-operand sum: 100,000 nested operator nodes. | ~37 ms | ~14 ms |
| `parse/mini/errors` | 180 KB where every statement has a mistake. | ~25 ms | ~8.8 ms |

On large inputs the cost is dominated by the tree itself: on Linux, for the 1 MB mini file, lexing takes about 2 ms and the parser about 3 ms, while allocating the tree's nodes takes about 6 ms and freeing them 13 ms. Run them yourself:

```bash
cargo bench --bench bench
```

Criterion writes per-benchmark reports to `target/criterion/`. Numbers vary by CPU and allocator; use the trend across runs, not a single absolute.

<hr>
<br>

## Design notes

- **Interpreted, not generated.** A forged language is tables plus a small interpreter, not Rust source to compile. Changing a schematic takes microseconds, a tool can forge languages at run time, and every language shares one well-tested parser.
- **Ordered choice, pruned by FIRST sets.** Alternatives are tried in order and the first that matches wins, as in a parsing expression grammar, which is forgiving to write. FIRST sets prune the attempt before it starts, so for nearly every decision exactly one alternative remains and the parser commits at once. Alternatives that begin alike are left-factored when the language is forged, which removes most of the remaining ambiguity — and reveals alternatives that could never match.
- **Recovery that respects structure.** A missing token is assumed; an unwanted token is skipped into an `ERROR` node; and skipping stops at any token an enclosing construct is waiting for, so one mistake does not swallow the rest of a block. The parser reports at most one syntax error per token.
- **Errors, not panics.** Schematic problems are values with spans; source problems are diagnostics. Every recursive step of the parser counts against a fixed limit, so input nested beyond it is reported instead of followed and no input can exhaust the stack. Input nested too deeply limits only the speculation it is part of; the code after it is parsed in full.
- **Budgets on hostile schematics.** A schematic is at most 8 MiB and nests at most 64 levels (table, key, array, and inline-table levels counted together); the parser tables that grow with rules times token kinds are capped at 256 MiB and refused before they are allocated; duplicate-key checks are logarithmic, not scans.
- **The schematic is static.** NOML's dynamic features — environment lookups, includes, native types — are refused, so a schematic forges the same language on every machine. What NOML (and TOML) call invalid is refused too, even where no setting would use it.
- **Byte-order marks.** A leading UTF-8 byte-order mark is skipped in a schematic, and in source text it is trivia: it begins the first `WHITESPACE` token, so trees stay lossless.

<hr>
<br>

## Testing

The suite runs on Windows, Linux (WSL2 Ubuntu), and macOS through the CI matrix, on stable and the 1.85 MSRV:

```bash
cargo test                       # unit + integration + property + doctests
cargo clippy --all-targets --all-features -- -D warnings
cargo bench --bench bench
```

The property tests in [`tests/proptests.rs`](./tests/proptests.rs) generate random valid mini programs, which must parse without a single diagnostic; random token soup and arbitrary Unicode text, which must parse into lossless, properly nested trees in every example language; and arbitrary and randomly mutated schematics, which must forge or fail cleanly. Further properties, in the parser's unit tests, hold the recovering parser to a strict reference parser on every input it accepts without error, and require memoization never to change a tree or a diagnostic on input within the depth limit. [`tests/regressions.rs`](./tests/regressions.rs) keeps every input an adversarial review used against the crate — stack-exhausting grammars, exponential and quadratic speculation, recursion without progress, hostile schematics — and holds them to time and stack bounds; [`tests/memory.rs`](./tests/memory.rs) measures peak heap use with a counting allocator and holds forging and parsing to memory bounds. Every `rust` example in this README and in [`docs/API.md`](./docs/API.md) is compiled and run as a doctest.

<hr>
<br>

## Cross-platform support

- Linux (x86_64, aarch64)
- macOS (x86_64, Apple Silicon)
- Windows (x86_64)

The crate uses no operating-system facilities and no platform-specific code; a schematic forges the same language, and a source parses into the same tree, on every platform.

<hr>
<br>

## Contributing

See [`REPS.md`](./REPS.md) for the engineering standards every change is held to, and [`dev/ROADMAP.md`](./dev/ROADMAP.md) for what may come in 1.x. Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> &mdash; <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b> &mdash; <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>James Gober <me@jamesgober.com>.</strong></sup>
</div>
