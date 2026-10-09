# lang-forge &mdash; API Reference

> Complete reference for every public item in `lang-forge`, with examples, and
> for the `.lsf` schematic format it reads.
> **Status: stable (1.0).** The surface below, the schematic format, and the
> parser's guarantees are the `1.0` contract; they follow
> [Semantic Versioning](#stability) and will not change in a breaking way
> before `2.0`. See [`../dev/ROADMAP.md`](../dev/ROADMAP.md).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick start](#quick-start)
- [The schematic](#the-schematic)
  - [`[language]`](#language-1)
  - [`[lexer]`](#lexer)
  - [`[rules]`](#rules)
  - [Expression rules](#expression-rules)
  - [`[capabilities]`](#capabilities-1)
  - [NOML in a schematic](#noml-in-a-schematic)
- [Concepts](#concepts)
  - [Tokens and the derived lexer](#tokens-and-the-derived-lexer)
  - [Kinds and the tree](#kinds-and-the-tree)
  - [How parsing decides](#how-parsing-decides)
  - [Error recovery](#error-recovery)
  - [What forging checks](#what-forging-checks)
- [`Language`](#language)
  - [`Language::from_lsf`](#languagefrom_lsf)
  - [`Language::name`](#languagename)
  - [`Language::version`](#languageversion)
  - [`Language::extensions`](#languageextensions)
  - [`Language::capabilities`](#languagecapabilities)
  - [`Language::kind`](#languagekind)
  - [`Language::kind_name`](#languagekind_name)
  - [`Language::lex`](#languagelex)
  - [`Language::parse`](#languageparse)
  - [`Language::pipeline`](#languagepipeline)
  - [`FromStr for Language`](#fromstr-for-language)
- [`Parse`](#parse)
  - [`Parse::tree`](#parsetree)
  - [`Parse::source`](#parsesource)
  - [`Parse::language`](#parselanguage)
  - [`Parse::diagnostics`](#parsediagnostics)
  - [`Parse::has_errors`](#parsehas_errors)
  - [`Parse::report`](#parsereport)
  - [`Parse::into_tree`](#parseinto_tree)
  - [`Parse::dump`](#parsedump)
- [`Kind`](#kind)
- [`Capability`](#capability)
- [`Error`](#error)
  - [`Error::diagnostics`](#errordiagnostics)
- [Re-exports](#re-exports)
- [Feature flags](#feature-flags)
- [Guide: from schematic to interpreter](#guide-from-schematic-to-interpreter)
- [Guide: a line-based language](#guide-a-line-based-language)
- [Stability](#stability)

## Overview

lang-forge turns a description of a language into a working front end for it.
The description is a **schematic**: a short NOML document with the extension
`.lsf` that says what the language is called, how its tokens look, what its
grammar is, which operators it has and how tightly they bind, and which
capabilities (passes) it includes. Forging a schematic reads it, checks it, and
compiles it into tables; the resulting [`Language`](#language) lexes and parses
source text into a lossless [`syntax_lang`](https://docs.rs/syntax-lang) tree.

| Item | What it is |
|---|---|
| [`Language`](#language) | A forged language. Immutable, `Send + Sync`, `Clone`. |
| [`Parse`](#parse) | The result of parsing: tree, source, diagnostics. |
| [`Kind`](#kind) | The kind of a token or node. `Copy`, compares like an enum. |
| [`Capability`](#capability) | A boxed `pass_lang::Pass<Parse>`, included by a schematic by name. |
| [`Error`](#error) | Every problem with a schematic, as diagnostics with spans. |

Parsing never fails. Malformed input yields a complete tree — missing pieces
left out, unexpected tokens wrapped in `ERROR` nodes — and one diagnostic per
problem. The tree always covers every byte of the source, so it can be printed
back exactly. It is the family's lossless CST, the tree the formatter,
incremental reparser, language server, and tree-sitter crates of the `-lang`
family are designed to consume; the adapters that connect a forged language to
those crates are not part of lang-forge and arrive with LexerSketch.

## Installation

```toml
[dependencies]
lang-forge = "1"
```

The crate is `no_std`-compatible: disable default features and it needs only
`alloc`.

```toml
[dependencies]
lang-forge = { version = "1", default-features = false }
```

MSRV: Rust 1.85 (Rust 2024 edition). `syntax-lang`, `diag-lang`, and
`pass-lang` are re-exported; see [Re-exports](#re-exports).

## Quick start

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r#"
    [language]
    name = "greet"

    [lexer]
    strings = ['"']

    [rules]
    script   = "greeting*"
    greeting = "'hello' (IDENT | STRING) '!'"
"#)?;

let parse = lang.parse("hello world! hello \"you there\"!");
assert!(!parse.has_errors());
assert_eq!(parse.tree().child_nodes().count(), 2);

let broken = lang.parse("hello world hello you!");
assert_eq!(broken.diagnostics()[0].message(), "expected `!`, found `hello`");
# Ok::<(), lang_forge::Error>(())
```

## The schematic

A schematic has up to four tables. Only `[language] name` and at least one
rule are required. Unknown tables and keys are errors — a misspelled setting is
far more often a mistake than an intention.

```toml
[language]                       # required
name       = "mini"              # required
version    = "0.1.0"
extensions = ["mini"]
start      = "program"

[lexer]
identifiers     = "xid"
newlines        = false
line_comments   = ["//"]
block_comments  = [["/*", "*/"]]
nested_comments = true
strings         = ['"']

[rules]                          # required: at least one rule
program = "stmt*"
stmt    = "'let' IDENT '=' expr ';' | expr ';'"
group   = "'(' expr ')'"

[rules.expr]
operand = "NUMBER | IDENT | group"
levels  = [{ left = ["+", "-"] }, { left = ["*", "/"] }]

[capabilities]
include = ["unused-variables"]
```

### `[language]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | *(required)* | The language's name, for [`Language::name`](#languagename). Must not be blank. |
| `version` | string | none | The language's version, kept as written. |
| `extensions` | array of strings | `[]` | Source file extensions, without the dot (`"calc"`, not `".calc"`). |
| `start` | string | the first rule | The rule whose node is the root of every tree. It must exist and must not be hidden. |

### `[lexer]`

| Key | Type | Default | Meaning |
|---|---|---|---|
| `identifiers` | `"xid"` or `"ascii"` | `"xid"` | `"xid"`: Unicode identifiers per UAX #31 (XID_Start then XID_Continue, plus `_`). `"ascii"`: letters, digits, and `_`. |
| `newlines` | boolean | `false` | `true` makes each line break (`\n` or `\r\n`) a `NEWLINE` token the grammar can match; otherwise line breaks are whitespace. |
| `line_comments` | array of strings | `[]` | Each string starts a comment that runs to the end of the line. |
| `block_comments` | array of `[open, close]` pairs | `[]` | Delimited comments. |
| `nested_comments` | boolean | `false` | Whether block comments nest: `/* a /* b */ c */` is one comment. |
| `strings` | array of strings or tables | `[]` | String literals; see below. The grammar can use `STRING` only if at least one is declared. |

A string entry is either a delimiter — `'"'` means strings open and close with
`"` and use `\` as the escape character — or a table:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `open` | string | *(required)* | The opening delimiter. |
| `close` | string | `open` | The closing delimiter. |
| `escape` | one-character string | `"\\"` | The escape character, which makes the next character literal. `""` for none. |
| `multiline` | boolean | `false` | Whether the string may contain line breaks. |

```toml
strings = [
    '"',                                                       # "text with \" escapes"
    "'",                                                       # 'c'
    { open = '#"', close = '"#', escape = "", multiline = true }, # #"raw, over lines"#
]
```

Comment and string delimiters must not begin with a letter, digit, `_`, or
whitespace (the lexer would read an identifier, a number, or whitespace there
first), and no text may serve two purposes — a symbol cannot also open a
comment.

### `[rules]`

Each entry is a rule. A string rule is written in the rule language:

| Element | Matches |
|---|---|
| `'text'` or `"text"` | A literal: a keyword if it reads like an identifier (`'let'`), a symbol otherwise (`'+='`). |
| `IDENT` | An identifier that is not a keyword. |
| `NUMBER` | A number literal. |
| `STRING` | A string literal (needs `[lexer] strings`). |
| `NEWLINE` | A line break (needs `[lexer] newlines = true`). The end of the input counts as a line break too — wherever a `NEWLINE` may come, alone, in a choice, in a repetition, or through a rule — so the last line needs none. |
| `name` | The rule `name`, which builds a child node. |
| `a b` | `a` then `b`. |
| `a \| b` | `a`, or else `b`: alternatives are tried in order and the first that matches wins. |
| `a*` / `a+` / `a?` | Zero or more / one or more / zero or one `a`. |
| `( ... )` | Grouping. |

Whitespace, including line breaks, separates elements, so long rules can be
written over several lines in a `"""` string. A literal cannot contain
whitespace or span lines, and a literal written in `'...'` cannot contain `'`
(write `"'"` instead).

**Nodes.** A rule builds a node named after itself. A rule whose name starts
with `_` is *hidden*: it builds no node, and what it matches is placed directly
in the parent. Rule names use letters, digits, and `_`, and cannot be one of
the built-in kind names (`IDENT`, `NUMBER`, `STRING`, `NEWLINE`, `WHITESPACE`,
`COMMENT`, `UNKNOWN`, `ERROR`, `EOF`) or the same as a keyword.

**The start rule** is the first rule in `[rules]`, or the one named by
`[language] start`. Its node is the root of every tree and spans the whole
source.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r#"
    [language]
    name = "lists"

    [rules]
    file  = "_item*"
    _item = "list | IDENT"
    list  = "'[' (_item (',' _item)*)? ']'"
"#)?;

// `_item` is hidden: identifiers and lists sit directly in their parent.
let parse = lang.parse("a [b, [c]] d");
assert!(!parse.has_errors());
let names: Vec<&str> = parse.tree().descendants().map(|n| lang.kind_name(*n.kind())).collect();
assert_eq!(names, ["file", "list", "list"]);
# Ok::<(), lang_forge::Error>(())
```

### Expression rules

A rule written as a table, `[rules.name]`, is an expression rule: operands
combined by operators, parsed by precedence climbing. Operators need no left
recursion, which the rule language does not allow.

| Key | Type | Meaning |
|---|---|---|
| `operand` | string | What an operand is, in the rule language. Usually literals and token classes plus a rule for parenthesized groups. Must not match the empty string. |
| `levels` | array of tables | Operator levels, **lowest precedence first**. |

Each level has exactly one of these keys, naming its operators (a string or an
array of strings, written without quotes inside):

| Key | Operators that are |
|---|---|
| `left` | binary, left-associative: `a - b - c` is `(a - b) - c` |
| `right` | binary, right-associative: `a ^ b ^ c` is `a ^ (b ^ c)` |
| `none` | binary, non-associative: `a < b < c` is an error |
| `prefix` | unary, before the operand: `-a` |
| `postfix` | unary, after the operand: `a!` |

and optionally:

| Key | Meaning |
|---|---|
| `then` | More grammar after the operator, in the rule language: `then = "args? ')'"` on a postfix `(` makes calls; `then = "expr ':'"` on a right-associative `?` makes `a ? b : c`. |
| `node` | The name of the node the level builds. Defaults: `binary` for `left`, `right`, and `none`; `prefix`; `postfix`. Several levels may share a name. |

Each operator application builds a node around its operands, so
`1 + 2 * 3` is `expr(binary(1 + binary(2 * 3)))`. A prefix operator applies to
everything at higher levels: with `-` declared above `*`, `-a * b` is
`(-a) * b`; with it below `^`, `-a ^ b` is `-(a ^ b)`. The same token may be a
prefix operator and an infix one (`-`), but not two kinds of operator after an
operand.

Give multi-token operands a rule of their own — `group = "'(' expr ')'"` and
`operand = "NUMBER | group"` — so each operand is one child of its operator
node.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r#"
    [language]
    name = "ops"

    [rules]
    file  = "expr"
    args  = "expr (',' expr)*"
    group = "'(' expr ')'"

    [rules.expr]
    operand = "NUMBER | IDENT | group"
    levels  = [
        { right   = ["?"], then = "expr ':'", node = "ternary" },
        { none    = ["==", "<"],              node = "compare" },
        { left    = ["+", "-"] },
        { left    = ["*", "/"] },
        { prefix  = ["-", "!"] },
        { right   = ["^"] },
        { postfix = ["("], then = "args? ')'", node = "call" },
    ]
"#)?;

let shape = |src: &str| -> Vec<String> {
    let parse = lang.parse(src);
    assert!(!parse.has_errors(), "{src}");
    parse
        .tree()
        .descendants()
        .map(|n| lang.kind_name(*n.kind()).to_owned())
        .filter(|k| k != "file" && k != "expr")
        .collect()
};
assert_eq!(shape("1 + 2 * 3"), ["binary", "binary"]);
assert_eq!(shape("-a ^ b"), ["prefix", "binary"]);
assert_eq!(shape("f(x, 1)"), ["call", "args"]);
assert_eq!(shape("a < b ? x : y"), ["ternary", "compare"]);

let chained = lang.parse("a < b < c");
assert_eq!(chained.diagnostics()[0].message(), "`<` cannot be chained; add parentheses");
# Ok::<(), lang_forge::Error>(())
```

### `[capabilities]`

| Key | Type | Meaning |
|---|---|---|
| `include` | array of strings | The capabilities the language includes, in the order their passes run. Each name may appear once. |

A capability is a pass, matched by name; see [`Language::pipeline`](#languagepipeline).

### NOML in a schematic

A schematic is a NOML document, restricted to the part of NOML that reads the
same on every machine: tables (`[a]`, `[a.b]`), keys (bare, quoted, and
dotted), the four TOML string forms (`"..."` with escapes, `'...'` literal,
and the `"""` and `'''` multi-line forms), booleans, numbers, arrays, and
inline tables. Inline tables may span lines, and arrays and inline tables may
end with a trailing comma, as NOML allows. Comments start with `#`.

NOML's dynamic features — `env(...)` and other function calls, `@native`
types, includes — are refused: a schematic must forge the same language
wherever it is forged. Arrays of tables (`[[...]]`) are not used. Strings in a
schematic are not interpolated.

What NOML and TOML call invalid is refused, even where no setting would read
it: a table defined by dotted keys (`expr.operand = "..."`) cannot be defined
again by a header (`[rules.expr]`), though a header may add a sub-table to it;
a key cannot be a multi-line string; and a number, date, or time must have
one of TOML's forms (`1_000`, `0x1F`, `6.02e23`, `inf`, `1979-05-27`,
`07:32:00`, ...) — `1abc` or a lone `-` is an error, not a number. No setting
takes a number, date, or time, so a well-formed one is reported as the wrong
type.

A leading UTF-8 byte-order mark is skipped; spans still count it.

Limits: a schematic is at most 8 MiB, and nests at most 64 levels deep,
counting every level together — each part of a `[header]` or dotted key, each
array, and each inline table (real schematics nest four or five). The parser
tables that grow with the number of rules and alternatives times the number of
distinct keywords and symbols are capped at 256 MiB; a grammar that would need
more is refused before they are allocated. Real grammars need well under a
megabyte; 30,000 keywords need about 120 MB.

## Concepts

### Tokens and the derived lexer

The lexer is derived from the schematic rather than written. It produces one
token for every piece of the source; none is dropped.

| Token kind | What it is |
|---|---|
| Keywords | Every literal in the grammar that reads like an identifier. A keyword is never an `IDENT`. |
| Symbols | Every other literal. Matched longest first, so `==` wins over `=` and `//` (a comment) over `/`. |
| `IDENT` | An identifier that is not a keyword. |
| `NUMBER` | Digits with optional `_` separators, an optional fraction (`.` and a digit), and an optional exponent (`e`/`E`, sign, digits); or `0x`, `0o`, `0b` and digits of that radix. Letters or digits run on after a number are kept in the same token and reported (`invalid suffix`, `invalid digit`). |
| `STRING` | A string literal in one of the declared forms, delimiters included. |
| `NEWLINE` | A line break, when `newlines = true`. |
| `WHITESPACE` | Spaces, tabs, form feeds, and line breaks (unless they are `NEWLINE`), plus Unicode's other pattern whitespace. Trivia. |
| `COMMENT` | A line or block comment, delimiters included. Trivia. |
| `UNKNOWN` | A run of characters that begins no token, reported once. Trivia, so the parser carries on around it. |

A UTF-8 byte-order mark (U+FEFF) at the very start of the source is not an
`UNKNOWN` character: it begins the first `WHITESPACE` token, together with any
whitespace after it, so the tree keeps it and nothing reports it. Elsewhere,
U+FEFF is an unexpected character.

Literals must be lexable as themselves: a literal cannot be empty, contain
whitespace, start with a digit, or start like an identifier and then continue
with symbol characters (`'x+'`). In `identifiers = "ascii"` mode a non-ASCII
letter is not an identifier character, so `'λ'` is a symbol.

### Kinds and the tree

Every token and node has a [`Kind`](#kind), named as follows:

| Name | Kind of |
|---|---|
| a rule name, such as `let_stmt` | the node the rule builds |
| a literal's text, such as `let` or `+=` | the keyword or symbol token |
| a level's `node`, or `binary`, `prefix`, `postfix` | an operator node |
| `IDENT`, `NUMBER`, `STRING`, `NEWLINE` | the token classes |
| `WHITESPACE`, `COMMENT`, `UNKNOWN` | trivia tokens |
| `ERROR` | a node wrapping tokens the parser skipped |

The tree is a [`syntax_lang::Node<Kind>`](https://docs.rs/syntax-lang). It is
lossless: its tokens, in order, are exactly the source. Trivia between two
tokens sits in the innermost node that encloses both, placed before the next
node, so a node's span starts at its first significant token and ends at its
last; comments between statements belong to the block, not to the statement
after them. The root covers the whole source, leading and trailing trivia
included.

### How parsing decides

The grammar is read as a parsing expression grammar: alternatives are tried in
order and the first that matches wins; repetitions and optionals are greedy and
commit as soon as their body can begin. Within that, the parser does as little
work as possible:

- **FIRST-set pruning.** Before trying anything, the parser checks which
  alternatives can begin with the current token. Almost always exactly one can,
  and the parser commits to it at once, like a predictive LL(1) parser.
- **Speculation, only when needed.** When several alternatives can begin with
  the same token, each is tried in turn in a strict mode that fails fast and
  records nothing, and the parser rewinds between attempts.
- **Memoized attempts.** Every strict attempt at a rule is remembered by rule
  and position for as long as the outermost speculation lasts, so alternatives
  that share a prefix through different rules — `assign = expr '=' expr ';'`
  against `expr ';'` — parse that prefix once. Without this, nested input
  would cost time exponential in its depth; with it, parsing stays linear in
  the input (at most, each level of speculative nesting replays what it holds
  once more). The memo's memory stays proportional to the input however deeply
  it nests: remembered events stay where the parser produced them and are
  moved only when a rewind would discard them, once, with nested replays kept
  as references.
- **Left-factoring.** Adjacent alternatives that begin with the same elements
  are merged when the language is forged — `x a | x b` becomes `x (a | b)` —
  which keeps ordered choice's meaning while reading `x` only once.

Because the first matching alternative wins, list longer alternatives before
shorter ones that begin the same way: in `IDENT | IDENT '(' ')'` the second
alternative can never match, and forging says so.

### Error recovery

Parsing always completes:

- **A missing token** is reported and treated as present. If the token right
  after the current one is the expected one, the current token is skipped as a
  stray instead.
- **A token no rule wants** is reported and wrapped in an `ERROR` node, and the
  enclosing repetition carries on. Skipping stops at any token an enclosing
  construct is waiting for — the `}` that closes a block, the `;` that ends a
  statement — so one mistake does not swallow the rest of the input.
- **When several alternatives fail,** the one that got furthest into the input
  is re-run in recovering mode, so the error reported is the deepest one.
- **At most one syntax error is reported per token** (a lexical error, such as
  an unterminated string, may sit on the same token), and a run of skipped tokens is
  one error.
- **Input after the start rule** is wrapped in one `ERROR` node with one error.
- **Input nested too deeply** is reported once and not followed, so no input
  can exhaust the stack. While speculating over such input, each decision
  tries only its first viable alternative, which keeps that speculation
  linear; the code after it is parsed in full. Every recursive step of the parser counts against a
  limit of 768 grammar levels, which needs at most about 256 KiB of stack in a
  release build and 768 KiB in a debug build — within every default thread,
  including the 1 MiB Windows main thread — and allows well over a hundred
  levels of nested parentheses in typical grammars.

On input without errors, recovery never engages and the tree is exactly the
one the grammar describes.

### What forging checks

[`Language::from_lsf`](#languagefrom_lsf) reports every problem it finds, not
just the first. The checks, by stage:

| Stage | Refused |
|---|---|
| Reading | NOML syntax errors; duplicate keys and tables; a header redefining a table defined by dotted keys; multi-line string keys; malformed numbers, dates, and times; NOML's dynamic features; arrays of tables; more than 64 levels of nesting; schematics over 8 MiB. |
| Layout | Unknown tables or keys; missing `[language]`, `name`, or `[rules]`; wrong value types; blank names; extensions with a dot; empty or misshapen delimiters (a block comment is exactly two strings); a multi-character escape; empty or duplicate capability names. |
| Rules | Malformed rule text; invalid or reserved rule and node names; undefined rules (with a suggestion); token classes that are not available (`STRING` without strings, `NEWLINE` without `newlines = true`) or are trivia; literals the lexer cannot produce; a keyword that is also a rule name; an operator twice in the same position; a hidden or undefined start rule. |
| Lexer | A text used for two things (symbol and comment, comment and string, ...); comment and string delimiters the lexer could never reach. |
| Grammar | Left recursion, direct or through other rules (named as a chain, `a → b → a`); `*` or `+` over something that can match nothing; an expression operand that can match nothing; alternatives that can never match; parser tables over 256 MiB. |

## `Language`

```rust,ignore
#[derive(Clone, Debug)]
pub struct Language { /* private */ }
```

A forged language: a lexer, a parser, and the kinds of its tree. Forge it once
with [`from_lsf`](#languagefrom_lsf), then [`parse`](#languageparse) as often
as needed. It is immutable once forged; `Send` and `Sync`, so one language can
parse on many threads at once.

```rust
use std::sync::Arc;
use lang_forge::Language;

let lang = Arc::new(Language::from_lsf(
    "[language]\nname = \"nums\"\n[rules]\nlist = \"NUMBER (',' NUMBER)*\"\n",
)?);
let workers: Vec<_> = (0..4)
    .map(|i| {
        let lang = Arc::clone(&lang);
        std::thread::spawn(move || lang.parse(&format!("{i}, {}", i * 2)).has_errors())
    })
    .collect();
for worker in workers {
    assert!(!worker.join().expect("parsed"));
}
# Ok::<(), lang_forge::Error>(())
```

### `Language::from_lsf`

```rust,ignore
pub fn from_lsf(schematic: &str) -> Result<Language, Error>
```

Forges a language from the text of a `.lsf` schematic: reads it, checks it
against the schematic layout, compiles its grammar, and analyses it. Every
problem is reported at once.

| Parameter | Meaning |
|---|---|
| `schematic` | The schematic's text. Diagnostic spans in the returned [`Error`](#error) are byte offsets into it. |

**Errors:** an [`Error`](#error) with one diagnostic per problem; see
[What forging checks](#what-forging-checks).

```rust
use lang_forge::Language;

// From a file, with errors rendered against it.
let path = "examples/schematics/json.lsf";
let text = std::fs::read_to_string(path).expect("readable");
let json = Language::from_lsf(&text).unwrap_or_else(|err| panic!("{path}:{err}"));
assert_eq!(json.name(), "json");
```

```rust
use lang_forge::Language;
use lang_forge::diag_lang::{Renderer, SourceMap};

let schematic = "[language]\nname = \"t\"\n[rules]\nfile = \"item*\"\nitems = \"NUMBER\"\n";
let err = Language::from_lsf(schematic).unwrap_err();

let mut map = SourceMap::new();
map.add("t.lsf", schematic).expect("fits");
let rendered = Renderer::new().render(&err.diagnostics()[0], &map);
assert!(rendered.contains("error: undefined rule `item`"));
assert!(rendered.contains("help: did you mean `items`?"));
```

```rust
use lang_forge::Language;

// Several problems, reported together, in source order.
let err = Language::from_lsf(
    "[language]\nname = \"t\"\nkind = \"x\"\n[rules]\na = \"a 'x'\"\nb = \"(NUMBER?)*\"\n",
)
.unwrap_err();
let messages: Vec<&str> = err.diagnostics().iter().map(|d| d.message()).collect();
assert_eq!(messages, [
    "unknown key `kind` in [language]",
    "rule `a` is left-recursive: a → a",
    "`*` repeats something that can match nothing, so it would never stop",
]);
```

### `Language::name`

```rust,ignore
pub fn name(&self) -> &str
```

The language's name, from `[language] name`.

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"Mox\"\n[rules]\nm = \"IDENT\"\n")?;
assert_eq!(lang.name(), "Mox");
# Ok::<(), lang_forge::Error>(())
```

### `Language::version`

```rust,ignore
pub fn version(&self) -> Option<&str>
```

The language's version, from `[language] version`, kept as written; `None` if
the schematic gives none.

```rust
use lang_forge::Language;

let with = Language::from_lsf("[language]\nname = \"a\"\nversion = \"2.1.0-beta\"\n[rules]\na = \"IDENT\"\n")?;
let without = Language::from_lsf("[language]\nname = \"b\"\n[rules]\nb = \"IDENT\"\n")?;
assert_eq!(with.version(), Some("2.1.0-beta"));
assert_eq!(without.version(), None);
# Ok::<(), lang_forge::Error>(())
```

### `Language::extensions`

```rust,ignore
pub fn extensions(&self) -> impl ExactSizeIterator<Item = &str>
```

The file extensions of the language's source files, without the dot, in
schematic order.

```rust
use std::path::Path;
use lang_forge::Language;

let conf = Language::from_lsf(
    "[language]\nname = \"conf\"\nextensions = [\"conf\", \"ini\"]\n[rules]\nf = \"IDENT*\"\n",
)?;
let handles = |path: &str| {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| conf.extensions().any(|x| x == e))
};
assert!(handles("server.ini"));
assert!(!handles("server.toml"));
assert_eq!(conf.extensions().len(), 2);
# Ok::<(), lang_forge::Error>(())
```

### `Language::capabilities`

```rust,ignore
pub fn capabilities(&self) -> impl ExactSizeIterator<Item = &str>
```

The capabilities the schematic includes, from `[capabilities] include`, in the
order their passes run.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[language]\nname = \"iron\"\n[rules]\nf = \"IDENT*\"\n\
     [capabilities]\ninclude = [\"borrow-check\", \"thermal\"]\n",
)?;
assert_eq!(lang.capabilities().collect::<Vec<_>>(), ["borrow-check", "thermal"]);
# Ok::<(), lang_forge::Error>(())
```

### `Language::kind`

```rust,ignore
pub fn kind(&self, name: &str) -> Option<Kind>
```

The kind called `name`, or `None` if the language has no such kind. See
[Kinds and the tree](#kinds-and-the-tree) for how kinds are named. Hidden rules
have no kind, and `EOF` is never in a tree.

| Parameter | Meaning |
|---|---|
| `name` | A rule name, a literal's text, an operator node name, or a built-in kind name. |

The lookup is a binary search over the kind names; look kinds up once and keep
them, then compare kinds while walking trees.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[language]\nname = \"k\"\n[rules]\nfile = \"_x*\"\n_x = \"assign\"\nassign = \"IDENT ':=' NUMBER\"\n",
)?;
assert!(lang.kind("assign").is_some());
assert!(lang.kind(":=").is_some());
assert!(lang.kind("NUMBER").is_some());
assert!(lang.kind("ERROR").is_some());
assert!(lang.kind("_x").is_none());   // hidden
assert!(lang.kind("EOF").is_none());  // never in a tree
# Ok::<(), lang_forge::Error>(())
```

```rust
use lang_forge::Language;

// Count assignments by comparing kinds while walking.
let lang = Language::from_lsf(
    "[language]\nname = \"k\"\n[rules]\nfile = \"assign*\"\nassign = \"IDENT ':=' NUMBER\"\n",
)?;
let assign = lang.kind("assign").expect("a rule");
let parse = lang.parse("a := 1 b := 2 c := 3");
assert_eq!(parse.tree().descendants().filter(|n| *n.kind() == assign).count(), 3);
# Ok::<(), lang_forge::Error>(())
```

### `Language::kind_name`

```rust,ignore
pub fn kind_name(&self, kind: Kind) -> &str
```

The name of `kind`: the inverse of [`kind`](#languagekind).

A kind is only meaningful to the language that made it. Given a kind from
another language, `kind_name` cannot tell: it returns whatever name this
language has at that kind's position in its kind table — usually a wrong one —
or `"<unknown>"` when this language has fewer kinds than that.

| Parameter | Meaning |
|---|---|
| `kind` | A kind from this language's trees, tokens, or [`kind`](#languagekind). |

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"'go' NUMBER\"\n")?;
let parse = lang.parse("go 7");
let names: Vec<&str> = parse.tree().tokens().map(|t| lang.kind_name(*t.kind())).collect();
assert_eq!(names, ["go", "WHITESPACE", "NUMBER"]);
assert_eq!(lang.kind_name(*parse.tree().kind()), "item");

// Another language's kinds get a wrong name, or none.
let other = Language::from_lsf(
    "[language]\nname = \"y\"\n[rules]\nlist = \"'[' (pair (',' pair)*)? ']'\"\npair = \"IDENT ':' NUMBER\"\n",
)?;
assert_eq!(lang.kind_name(other.kind("[").expect("a symbol")), "go");
assert_eq!(lang.kind_name(other.kind("pair").expect("a rule")), "<unknown>");
# Ok::<(), lang_forge::Error>(())
```

### `Language::lex`

```rust,ignore
pub fn lex(&self, source: &str) -> Vec<syntax_lang::Token<Kind>>
```

Splits `source` into tokens, trivia included. The tokens are contiguous and
cover the whole source, which makes this the stream a syntax highlighter wants.
Characters that begin no token come back as `UNKNOWN` tokens;
[`parse`](#languageparse) reports them, `lex` does not. A source of 4 GiB or
more (which 32-bit spans cannot address) yields no tokens.

| Parameter | Meaning |
|---|---|
| `source` | The text to tokenize. Token spans are byte offsets into it. |

```rust
use lang_forge::Language;
use lang_forge::syntax_lang::TokenKind;

let lang = Language::from_lsf(
    "[language]\nname = \"x\"\n[lexer]\nline_comments = [\"--\"]\n[rules]\nfile = \"IDENT*\"\n",
)?;
let tokens = lang.lex("alpha -- note\nbeta");
assert_eq!(tokens.len(), 5);
let significant: Vec<&str> = tokens
    .iter()
    .filter(|t| !t.is_trivia())
    .map(|t| lang.kind_name(*t.kind()))
    .collect();
assert_eq!(significant, ["IDENT", "IDENT"]);
# Ok::<(), lang_forge::Error>(())
```

```rust
use lang_forge::Language;

// Rebuild the source from the tokens: nothing is lost.
let lang = Language::from_lsf(
    "[language]\nname = \"x\"\n[lexer]\nstrings = ['\"']\n[rules]\nf = \"(IDENT | STRING | '=')*\"\n",
)?;
let source = "name = \"forge\"  @ odd\n";
let rebuilt: String = lang
    .lex(source)
    .iter()
    .map(|t| &source[t.span().start().to_usize()..t.span().end().to_usize()])
    .collect();
assert_eq!(rebuilt, source);
let unknown = lang.kind("UNKNOWN").expect("built in");
assert_eq!(lang.lex(source).iter().filter(|t| *t.kind() == unknown).count(), 1);
# Ok::<(), lang_forge::Error>(())
```

### `Language::parse`

```rust,ignore
pub fn parse<'a>(&'a self, source: &'a str) -> Parse<'a>
```

Parses `source` into a lossless syntax tree. Never fails: problems become
diagnostics on the returned [`Parse`](#parse), and the tree is complete
regardless. See [Error recovery](#error-recovery).

| Parameter | Meaning |
|---|---|
| `source` | The text to parse. The `Parse` borrows it; spans are byte offsets into it. |

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[language]\nname = \"block\"\n[rules]\nblock = \"'{' stmt* '}'\"\nstmt = \"IDENT ';'\"\n",
)?;
let good = lang.parse("{ a; b; }");
assert!(!good.has_errors());

// A stray `;` is skipped into an `ERROR` node; the rest still parses.
let bad = lang.parse("{ a; ; b; }");
assert_eq!(bad.diagnostics().len(), 1);
assert_eq!(bad.diagnostics()[0].message(), "expected stmt, found `;`");
let stmt = lang.kind("stmt").expect("a rule");
assert_eq!(bad.tree().child_nodes().filter(|n| *n.kind() == stmt).count(), 2);
# Ok::<(), lang_forge::Error>(())
```

```rust
use lang_forge::Language;

// Deeply nested input is reported, not followed.
let lang = Language::from_lsf(
    "[language]\nname = \"p\"\n[rules]\nvalue = \"NUMBER | '(' value ')'\"\n",
)?;
let source = format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000));
let parse = lang.parse(&source);
assert!(parse.diagnostics()[0].message().contains("nested too deeply"));
assert_eq!(parse.tree().text(&source).map(str::len), Some(source.len()));
# Ok::<(), lang_forge::Error>(())
```

### `Language::pipeline`

```rust,ignore
pub fn pipeline<'a>(
    &self,
    passes: impl IntoIterator<Item = Capability>,
) -> Result<pass_lang::PassManager<Parse<'a>>, Error>
```

Assembles the language's capability pipeline from a registry of passes. The
registry may hold passes for many languages: the pipeline takes the ones whose
[`Pass::name`](https://docs.rs/pass-lang) the schematic's
`[capabilities] include` lists, in that order, and ignores the rest. Run it over
each [`Parse`](#parse) with `PassManager::run`.

| Parameter | Meaning |
|---|---|
| `passes` | The available passes, each boxed as a [`Capability`](#capability). |

**Errors:** an [`Error`](#error) with a diagnostic, pointing at the capability
in the schematic, for every included capability with no pass or more than one.

```rust
use std::sync::{Arc, Mutex};
use lang_forge::pass_lang::{Outcome, Pass, PassError};
use lang_forge::{Capability, Language, Parse};

/// Records the order passes run in.
struct Step(&'static str, Arc<Mutex<Vec<&'static str>>>);

impl<'a> Pass<Parse<'a>> for Step {
    fn name(&self) -> &'static str {
        self.0
    }
    fn run(&mut self, _: &mut Parse<'a>) -> Result<Outcome, PassError> {
        self.1.lock().map_err(|_| PassError::new("poisoned"))?.push(self.0);
        Ok(Outcome::Unchanged)
    }
}

let lang = Language::from_lsf(
    "[language]\nname = \"p\"\n[rules]\nf = \"IDENT*\"\n[capabilities]\ninclude = [\"types\", \"lints\"]\n",
)?;
let log = Arc::new(Mutex::new(Vec::new()));
let registry: Vec<Capability> = vec![
    Box::new(Step("lints", log.clone())),
    Box::new(Step("formatting", log.clone())), // not included: ignored
    Box::new(Step("types", log.clone())),
];
let mut pipeline = lang.pipeline(registry)?;
assert_eq!(pipeline.len(), 2);

let mut parse = lang.parse("a b");
pipeline.run(&mut parse).expect("both passes run");
assert_eq!(*log.lock().expect("lock"), ["types", "lints"]); // schematic order
# Ok::<(), lang_forge::Error>(())
```

```rust
use lang_forge::{Capability, Language};

let lang = Language::from_lsf(
    "[language]\nname = \"p\"\n[rules]\nf = \"IDENT*\"\n[capabilities]\ninclude = [\"borrow-check\"]\n",
)?;
let Err(err) = lang.pipeline(Vec::<Capability>::new()) else {
    panic!("a missing capability is an error");
};
assert_eq!(err.to_string(), "6:12: capability `borrow-check` has no pass");
# Ok::<(), lang_forge::Error>(())
```

### `FromStr for Language`

```rust,ignore
impl FromStr for Language { type Err = Error; }
```

Forges a language with `str::parse`; the same as
[`from_lsf`](#languagefrom_lsf).

```rust
use lang_forge::Language;

let lang: Language = "[language]\nname = \"n\"\n[rules]\nn = \"NUMBER\"\n".parse()?;
assert_eq!(lang.name(), "n");
# Ok::<(), lang_forge::Error>(())
```

## `Parse`

```rust,ignore
#[derive(Clone, Debug)]
pub struct Parse<'a> { /* private */ }
```

The result of [`Language::parse`](#languageparse): a lossless syntax tree, the
source it came from, and the problems found while building it. It borrows the
language and the source, so names, text, and diagnostics can all be resolved
from it alone. It is also the unit capability passes run over.

### `Parse::tree`

```rust,ignore
pub fn tree(&self) -> &syntax_lang::Node<Kind>
```

The root of the tree: a node of the start rule's kind covering the whole
source. Walk it with the `syntax_lang::Node` API — `children`, `child_nodes`,
`child_tokens`, `descendants`, `tokens` — and slice the source with
`Node::text`. Every walk is iterative.

```rust
use lang_forge::Language;
use lang_forge::syntax_lang::Element;

let lang = Language::from_lsf(
    "[language]\nname = \"kv\"\n[lexer]\nstrings = ['\"']\n[rules]\nfile = \"pair*\"\npair = \"IDENT '=' STRING\"\n",
)?;
let parse = lang.parse("host = \"example.org\"\nport = \"8080\"");
let ident = lang.kind("IDENT").expect("built in");
let keys: Vec<&str> = parse
    .tree()
    .child_nodes()
    .filter_map(|pair| pair.children().find(|c| matches!(c, Element::Token(t) if *t.kind() == ident)))
    .map(|key| &parse.source()[key.span().start().to_usize()..key.span().end().to_usize()])
    .collect();
assert_eq!(keys, ["host", "port"]);
# Ok::<(), lang_forge::Error>(())
```

### `Parse::source`

```rust,ignore
pub fn source(&self) -> &'a str
```

The source text that was parsed. Spans in the tree and the diagnostics are byte
offsets into it.

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"w\"\n[rules]\nwords = \"IDENT*\"\n")?;
let parse = lang.parse("alpha beta");
assert_eq!(parse.source(), "alpha beta");
assert_eq!(parse.tree().text(parse.source()), Some("alpha beta"));
# Ok::<(), lang_forge::Error>(())
```

### `Parse::language`

```rust,ignore
pub fn language(&self) -> &'a Language
```

The language that parsed the source — for looking kinds up and naming them,
which is how a capability pass, given only the `Parse`, finds its way around.

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"w\"\n[rules]\nwords = \"IDENT*\"\n")?;
let parse = lang.parse("a b");
let ident = parse.language().kind("IDENT").expect("built in");
assert_eq!(parse.tree().tokens().filter(|t| *t.kind() == ident).count(), 2);
# Ok::<(), lang_forge::Error>(())
```

### `Parse::diagnostics`

```rust,ignore
pub fn diagnostics(&self) -> &[diag_lang::Diagnostic]
```

The problems found, in source order: lexical errors (unexpected characters,
unterminated strings and comments, malformed numbers), syntax errors, and
anything capability passes have [`report`](#parsereport)ed. Spans are byte
offsets into the source; add it to a fresh `SourceMap` and `Renderer` draws
each one under the line at fault.

```rust
use lang_forge::Language;
use lang_forge::diag_lang::{Renderer, SourceMap};

let lang = Language::from_lsf(
    "[language]\nname = \"calls\"\n[rules]\nfile = \"call*\"\ncall = \"IDENT '(' ')' ';'\"\n",
)?;
let source = "start();\nstop(;\n";
let parse = lang.parse(source);

let mut map = SourceMap::new();
map.add("main.calls", source).expect("fits");
let rendered = Renderer::new().render(&parse.diagnostics()[0], &map);
assert!(rendered.contains("expected `)`, found `;`"));
assert!(rendered.contains("main.calls:2:6"));
# Ok::<(), lang_forge::Error>(())
```

```rust
use lang_forge::Language;

// Lexical and syntax errors together, in source order.
let lang = Language::from_lsf(
    "[language]\nname = \"s\"\n[lexer]\nstrings = ['\"']\n[rules]\nf = \"(STRING ';')*\"\n",
)?;
let parse = lang.parse("\"ok\"; \"open;\n\"next\";");
let messages: Vec<&str> = parse.diagnostics().iter().map(|d| d.message()).collect();
assert_eq!(messages, ["unterminated string", "expected `;`, found a string"]);
# Ok::<(), lang_forge::Error>(())
```

### `Parse::has_errors`

```rust,ignore
pub fn has_errors(&self) -> bool
```

Whether any diagnostic has `Severity::Error` — as opposed to warnings and notes
that capability passes may add.

```rust
use lang_forge::Language;
use lang_forge::diag_lang::{Diagnostic, Label, Severity};

let lang = Language::from_lsf("[language]\nname = \"n\"\n[rules]\nn = \"NUMBER\"\n")?;
let mut parse = lang.parse("42");
assert!(!parse.has_errors());
let span = parse.tree().span();
parse.report(Diagnostic::new(Severity::Warning, "suspicious", Label::unlabelled(span)));
assert!(!parse.has_errors());
assert!(lang.parse("x").has_errors());
# Ok::<(), lang_forge::Error>(())
```

### `Parse::report`

```rust,ignore
pub fn report(&mut self, diagnostic: diag_lang::Diagnostic)
```

Adds a diagnostic: the way a capability pass reports what it finds.

| Parameter | Meaning |
|---|---|
| `diagnostic` | Any diagnostic; its spans should be byte offsets into the source. |

```rust
use lang_forge::Language;
use lang_forge::diag_lang::{Diagnostic, Label, Severity};

let lang = Language::from_lsf("[language]\nname = \"n\"\n[rules]\nlist = \"NUMBER*\"\n")?;
let mut parse = lang.parse("1 2 3");
let last = parse.tree().tokens().last().map(|t| t.span()).expect("a token");
parse.report(
    Diagnostic::new(Severity::Warning, "odd count", Label::new(last, "third number"))
        .with_help("numbers come in pairs"),
);
assert_eq!(parse.diagnostics().len(), 1);
# Ok::<(), lang_forge::Error>(())
```

### `Parse::into_tree`

```rust,ignore
pub fn into_tree(self) -> syntax_lang::Node<Kind>
```

Takes the tree, dropping the diagnostics and the borrows — for keeping a tree
after the source or the language goes away.

```rust
use lang_forge::Language;
use lang_forge::syntax_lang::Node;
use lang_forge::Kind;

fn tree_of(source: String) -> Result<Node<Kind>, lang_forge::Error> {
    let lang = Language::from_lsf("[language]\nname = \"w\"\n[rules]\nwords = \"IDENT*\"\n")?;
    Ok(lang.parse(&source).into_tree())
}
let tree = tree_of(String::from("a b c"))?;
assert_eq!(tree.tokens().count(), 5);
# Ok::<(), lang_forge::Error>(())
```

### `Parse::dump`

```rust,ignore
pub fn dump(&self) -> String
```

The tree as indented text, one node or token per line: a kind name and a byte
range, and for tokens the text. For tests, snapshots, and debugging a grammar.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[language]\nname = \"set\"\n[rules]\nstmt = \"'set' IDENT NUMBER\"\n",
)?;
assert_eq!(
    lang.parse("set x 1").dump(),
    "stmt@0..7\n  set@0..3 \"set\"\n  WHITESPACE@3..4 \" \"\n  IDENT@4..5 \"x\"\n  \
     WHITESPACE@5..6 \" \"\n  NUMBER@6..7 \"1\"\n",
);
# Ok::<(), lang_forge::Error>(())
```

## `Kind`

```rust,ignore
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kind { /* private */ }

impl syntax_lang::TokenKind for Kind { /* is_trivia */ }
impl Debug for Kind { /* Kind(index) */ }
```

The kind of a token or node. A schematic declares its vocabulary as text, so a
forged language cannot have a Rust `enum` of its own; `Kind` stands in for one:
a two-byte `Copy` value assigned when the language is forged. Tokens and nodes
share the type, as `syntax-lang` expects. Get kinds from
[`Language::kind`](#languagekind) and name them with
[`Language::kind_name`](#languagekind_name).

`TokenKind::is_trivia` answers without the language: whitespace, comments,
`UNKNOWN`, and — unless the schematic sets `newlines = true` — `NEWLINE` are
trivia. A kind belongs to the language that produced it.

```rust
use lang_forge::Language;
use lang_forge::syntax_lang::TokenKind;

let lang = Language::from_lsf(
    "[language]\nname = \"t\"\n[lexer]\nline_comments = [\"#\"]\n[rules]\nf = \"IDENT*\"\n",
)?;
let comment = lang.kind("COMMENT").expect("built in");
let ident = lang.kind("IDENT").expect("built in");
assert!(comment.is_trivia());
assert!(!ident.is_trivia());
assert_ne!(comment, ident);
# Ok::<(), lang_forge::Error>(())
```

```rust
use std::collections::HashMap;
use lang_forge::Language;

// Kinds are `Hash` and `Ord`: tally a tree by kind.
let lang = Language::from_lsf("[language]\nname = \"t\"\n[rules]\nf = \"(IDENT | NUMBER)*\"\n")?;
let parse = lang.parse("a 1 b 2 3");
let mut tally = HashMap::new();
for token in parse.tree().tokens() {
    *tally.entry(*token.kind()).or_insert(0) += 1;
}
assert_eq!(tally[&lang.kind("NUMBER").expect("built in")], 3);
# Ok::<(), lang_forge::Error>(())
```

## `Capability`

```rust,ignore
pub type Capability = Box<dyn for<'a> pass_lang::Pass<Parse<'a>>>;
```

A capability pass, boxed for [`Language::pipeline`](#languagepipeline). A
capability is a `pass_lang::Pass` over a [`Parse`](#parse), named by its
`Pass::name`. Implement the pass for every lifetime —
`impl<'a> Pass<Parse<'a>> for MyPass` — and box it.

A pass reads the tree and the source through the `Parse` it is given, reports
findings with [`Parse::report`](#parsereport), and returns
`Outcome::Unchanged` (the tree itself is not modified). Returning a
`PassError` stops the pipeline; the error carries the pass's name.

```rust
use lang_forge::diag_lang::{Diagnostic, Label, Severity};
use lang_forge::pass_lang::{Outcome, Pass, PassError};
use lang_forge::{Capability, Language, Parse};

/// Warns about numbers written with a leading zero.
struct LeadingZeros;

impl<'a> Pass<Parse<'a>> for LeadingZeros {
    fn name(&self) -> &'static str {
        "leading-zeros"
    }

    fn run(&mut self, parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
        let number = parse.language().kind("NUMBER").ok_or_else(|| PassError::new("no numbers"))?;
        let source = parse.source();
        let found: Vec<_> = parse
            .tree()
            .tokens()
            .filter(|t| *t.kind() == number)
            .filter(|t| {
                let text = &source[t.span().start().to_usize()..t.span().end().to_usize()];
                text.len() > 1 && text.starts_with('0') && text.as_bytes()[1].is_ascii_digit()
            })
            .map(|t| t.span())
            .collect();
        for span in found {
            parse.report(Diagnostic::new(Severity::Warning, "leading zero", Label::unlabelled(span)));
        }
        Ok(Outcome::Unchanged)
    }
}

let lang = Language::from_lsf(
    "[language]\nname = \"n\"\n[rules]\nf = \"NUMBER*\"\n[capabilities]\ninclude = [\"leading-zeros\"]\n",
)?;
let registry: Vec<Capability> = vec![Box::new(LeadingZeros)];
let mut pipeline = lang.pipeline(registry)?;
let mut parse = lang.parse("10 007 0 0x1F");
pipeline.run(&mut parse).expect("runs");
assert_eq!(parse.diagnostics().len(), 1);
# Ok::<(), lang_forge::Error>(())
```

## `Error`

```rust,ignore
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error { /* private */ }

impl Display for Error { /* line:column: message (and N more errors) */ }
impl core::error::Error for Error {}
```

Why a schematic could not be forged, or a capability pipeline could not be
assembled. It carries one [`Diagnostic`](https://docs.rs/diag-lang) per
problem, in source order, each with a span into the schematic text. `Display`
prints the first as `line:column: message`, with a count of the rest.

An `Error` always means the schematic (or the registry) needs changing:
forging is deterministic, so retrying the same text fails the same way. Every
message names what was found and what was expected, and many carry a `help`
line with the fix.

```rust
use lang_forge::Language;

let err = Language::from_lsf("[language]\nname = \"t\"\n[rules]\nf = \"a b\"\n").unwrap_err();
assert_eq!(err.to_string(), "4:6: undefined rule `a` (and 1 more error)");
let as_error: &dyn std::error::Error = &err;
assert!(as_error.source().is_none());
```

### `Error::diagnostics`

```rust,ignore
pub fn diagnostics(&self) -> &[diag_lang::Diagnostic]
```

Every problem found, in source order. Never empty. Spans are byte offsets into
the schematic text.

```rust
use lang_forge::Language;

let err = Language::from_lsf("[language]\n[rules]\n").unwrap_err();
let messages: Vec<&str> = err.diagnostics().iter().map(|d| d.message()).collect();
assert_eq!(messages, ["missing `name` in [language]", "[rules] declares no rules"]);
```

```rust
use lang_forge::Language;
use lang_forge::diag_lang::{Renderer, SourceMap};

// Render every problem, as a command-line tool would.
let schematic = "[language]\nname = \"t\"\n[lexer]\nline_comments = [\"#\"]\n[rules]\nf = \"'#' IDENT\"\n";
let err = Language::from_lsf(schematic).unwrap_err();
let mut map = SourceMap::new();
map.add("t.lsf", schematic).expect("fits");
let report: String = err
    .diagnostics()
    .iter()
    .map(|d| Renderer::new().render(d, &map))
    .collect();
assert!(report.contains("`#` is used for two different things"));
assert!(report.contains("first used here"));
```

## Re-exports

```rust,ignore
pub use diag_lang;
pub use pass_lang;
pub use syntax_lang;
```

Trees are `syntax_lang` trees, problems are `diag_lang` diagnostics, and
capabilities are `pass_lang` passes. The three crates are re-exported whole, so
code that walks trees, renders diagnostics, or writes passes names the same
versions lang-forge was built against without depending on them separately.

```rust
use lang_forge::diag_lang::Severity;
use lang_forge::pass_lang::Outcome;
use lang_forge::syntax_lang::{Span, TokenKind};

assert_eq!(Severity::Error.as_str(), "error");
assert_eq!(Outcome::Unchanged, Outcome::Unchanged);
assert_eq!(Span::new(2, 5).len(), 3);
let _ = <lang_forge::Kind as TokenKind>::is_trivia;
```

## Feature flags

| Feature | Default | What it enables |
|---|---|---|
| `std` | yes | The standard library. Without it the crate is `no_std` and needs only `alloc`; every API is available either way. |

## Guide: from schematic to interpreter

A schematic gives you a tree; giving it meaning is a walk. This is the core of
the `calc` example: evaluate expressions by matching kinds.

```rust
use lang_forge::{Kind, Language};
use lang_forge::syntax_lang::{Element, Node, TokenKind};

let lang = Language::from_lsf(r#"
    [language]
    name = "arith"

    [rules]
    expr  = "sum"
    group = "'(' sum ')'"

    [rules.sum]
    operand = "NUMBER | group"
    levels  = [{ left = ["+", "-"] }, { left = ["*", "/"] }, { prefix = ["-"] }]
"#)?;

struct Eval<'s> {
    src: &'s str,
    number: Kind,
    binary: Kind,
    prefix: Kind,
}

impl Eval<'_> {
    fn element(&self, e: &Element<Kind>) -> f64 {
        match e {
            Element::Token(t) if *t.kind() == self.number => self.src
                [t.span().start().to_usize()..t.span().end().to_usize()]
                .parse()
                .unwrap_or(f64::NAN),
            Element::Token(_) => f64::NAN,
            Element::Node(n) => self.node(n),
        }
    }

    fn node(&self, n: &Node<Kind>) -> f64 {
        // Skip whitespace and comments.
        let parts: Vec<&Element<Kind>> = n
            .children()
            .filter(|c| !matches!(c, Element::Token(t) if t.kind().is_trivia()))
            .collect();
        let text = |e: &Element<Kind>| &self.src[e.span().start().to_usize()..e.span().end().to_usize()];
        if *n.kind() == self.binary {
            let (a, b) = (self.element(parts[0]), self.element(parts[2]));
            return match text(parts[1]) {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                _ => a / b,
            };
        }
        if *n.kind() == self.prefix {
            return -self.element(parts[1]);
        }
        // `expr` and `sum` hold one child; `group` is `( sum )`.
        match parts.as_slice() {
            [only] => self.element(only),
            [_, inner, _] => self.element(inner),
            _ => f64::NAN,
        }
    }
}

let src = "2 * (3 + 4) - -1";
let parse = lang.parse(src);
assert!(!parse.has_errors());
let eval = Eval {
    src,
    number: lang.kind("NUMBER").expect("built in"),
    binary: lang.kind("binary").expect("operator node"),
    prefix: lang.kind("prefix").expect("operator node"),
};
assert_eq!(eval.node(parse.tree()), 15.0);
# Ok::<(), lang_forge::Error>(())
```

## Guide: a line-based language

For formats where line breaks matter, set `newlines = true` and use `NEWLINE`
in rules. Blank lines become `NEWLINE` tokens too, so allow them where they may
appear; the end of the input counts as a line break, so the last line needs
none.

```rust
use lang_forge::Language;

let conf = Language::from_lsf(r#"
    [language]
    name = "conf"

    [lexer]
    newlines      = true
    line_comments = [";"]
    strings       = ['"']

    [rules]
    file    = "(section | pair | NEWLINE)*"
    section = "'[' IDENT ']' NEWLINE"
    pair    = "IDENT '=' (STRING | NUMBER | IDENT) NEWLINE"
"#)?;

let text = "; server settings\n[server]\nhost = \"example.org\"\nport = 8080\n\n[log]\nlevel = info";
let parse = conf.parse(text);
assert!(!parse.has_errors());
let pair = conf.kind("pair").expect("a rule");
assert_eq!(parse.tree().descendants().filter(|n| *n.kind() == pair).count(), 3);

let broken = conf.parse("[server]\nhost = \"a\" port = 1\n");
assert_eq!(broken.diagnostics()[0].message(), "expected a line break, found identifier `port`");
# Ok::<(), lang_forge::Error>(())
```

## Stability

As of `1.0.0` the public API is frozen. lang-forge follows
[Semantic Versioning](https://semver.org/); within the `1.x` series:

- The **surface** will not change in a breaking way: [`Language`](#language)
  (`from_lsf`, `name`, `version`, `extensions`, `capabilities`, `kind`,
  `kind_name`, `lex`, `parse`, `pipeline`) with `Clone`, `Debug`, `Send`,
  `Sync`, and `FromStr`; [`Parse`](#parse) (`tree`, `source`, `language`,
  `diagnostics`, `has_errors`, `report`, `into_tree`, `dump`) with `Clone` and
  `Debug`; [`Kind`](#kind) with `Copy`, `Eq`, `Ord`, `Hash`, `Debug`, and
  `TokenKind`; the [`Capability`](#capability) alias; [`Error`](#error)
  (`diagnostics`) with `Clone`, `Debug`, `PartialEq`, `Eq`, `Display`, and
  `core::error::Error`; and the [re-exports](#re-exports). New methods and
  types are minor additions.
- The **schematic format** holds: every table, key, value form, and default in
  [The schematic](#the-schematic) keeps its meaning, and the rule language and
  expression rules keep theirs. New keys and new rule-language forms may be
  added in a minor release. A schematic that forges under one `1.x` release
  forges under every later one, with two exceptions, both bug fixes: a
  schematic a later release finds would hang or crash the parser, or exhaust
  memory past the documented limits, may be refused; and a schematic that is
  not valid NOML (which 1.0.0 accepted by mistake, for example a table
  redefined by a header after dotted keys, a multi-line key, or a malformed
  number) may be refused. 1.0.1 applies both; see its release notes.
- The **trees for valid input** hold: for input that parses without an error,
  the tree — its nodes, their kinds and names, and where trivia sits — is
  fixed by the schematic and the rules in [How parsing decides](#how-parsing-decides)
  and [Kinds and the tree](#kinds-and-the-tree). Kind naming, including the
  built-in names and the default operator node names, is fixed.
- The **guarantees** hold: parsing never fails and never panics; every tree is
  lossless; recursion is bounded, so no input can exhaust the stack (the
  limit of 768 grammar levels may be raised, never lowered); forging never
  panics; and `dump` keeps its format.
- `syntax-lang`, `diag-lang`, and `pass-lang` are **public dependencies**: the
  tree, diagnostic, and pass types in the API come from their `1.x` series.
  Moving to a new major version of any of them would be a major release here.
- MSRV (Rust 1.85) is a compatibility surface: raising it is a documented
  minor change, never a patch.

What is **not** promised: the wording of diagnostics and of `Error`'s
`Display`; for input with errors, the exact shape of the recovered tree and
which errors are reported (recovery may get better); the order problems are
found in beyond source order; the `Debug` output of any type; how much stack
the parser needs beyond the bound stated in [Error recovery](#error-recovery);
and performance figures — the benchmarks are tracked, but they are
measurements, not guarantees.

Features left for later minor releases, all additive: tree-sitter grammar
emission through `treesitter-lang`, named token classes beyond the built-ins,
custom token patterns, contextual keywords, and a reusable parse session.

See [`../dev/ROADMAP.md`](../dev/ROADMAP.md) and
[`../CHANGELOG.md`](../CHANGELOG.md).
