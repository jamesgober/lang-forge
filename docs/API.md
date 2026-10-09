# lang-forge &mdash; API Reference

> Complete reference for every public item in `lang-forge`, with examples, and
> for the `.lsf` schematic format it reads.
> **Status: `2.0.0-alpha.1`, a pre-release of 2.0.** Everything 1.x provides
> is kept, and format-1 sketches forge and parse exactly as in 1.x; the
> format-2 (LSF2) syntax, multi-file sketches, language images, field labels,
> and diagnostic codes are new and may still change before `2.0.0`. See
> [Stability](#stability), the CHANGELOG's migration guide, and
> [`../dev/ROADMAP.md`](../dev/ROADMAP.md).

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
- [Format 2 (LSF2)](#format-2-lsf2)
  - [Token classes](#token-classes)
  - [Modes](#modes)
  - [Strings](#strings)
  - [Keywords](#keywords)
  - [Numbers, comments, identifiers](#numbers-comments-identifiers)
  - [Layout](#layout)
  - [Labels and fields](#labels-and-fields)
  - [Predicates, back-references, and position assertions](#predicates-back-references-and-position-assertions)
  - [Rule options and the overlap check](#rule-options-and-the-overlap-check)
  - [Injections and `[ast]`](#injections-and-ast)
  - [Kinds in format 2](#kinds-in-format-2)
  - [LSF2 coverage](#lsf2-coverage)
- [Diagnostic codes](#diagnostic-codes)
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
  - [`Language::from_sketch`](#languagefrom_sketch)
  - [`Language::to_image` and `Language::from_image`](#languageto_image-and-languagefrom_image)
  - [`Language::format`](#languageformat)
  - [`Language::display_name`, `description`, `edition`, `shebang_names`](#languagedisplay_name-description-edition-shebang_names)
  - [`Language::kind_count`](#languagekind_count)
  - [`Language::kind_at`](#languagekind_at)
  - [`Language::root_kind`](#languageroot_kind)
  - [`Language::label_name` and `Language::label_id`](#languagelabel_name-and-languagelabel_id)
  - [`Language::field_label`](#languagefield_label)
  - [`Language::fields`](#languagefields)
  - [`Language::supertype` and `Language::supertypes`](#languagesupertype-and-languagesupertypes)
  - [`Language::warnings`](#languagewarnings)
  - [`Language::parse_file`](#languageparse_file)
- [`Parse`](#parse)
  - [`Parse::tree`](#parsetree)
  - [`Parse::source`](#parsesource)
  - [`Parse::language`](#parselanguage)
  - [`Parse::diagnostics`](#parsediagnostics)
  - [`Parse::has_errors`](#parsehas_errors)
  - [`Parse::report`](#parsereport)
  - [`Parse::into_tree`](#parseinto_tree)
  - [`Parse::dump`](#parsedump)
  - [`Parse::injections`](#parseinjections)
- [`Kind`](#kind)
- [`Injection`](#injection)
- [`Field` and `Cardinality`](#field-and-cardinality)
- [`Sketch`](#sketch)
- [`ImageError`](#imageerror)
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
| [`Parse`](#parse) | The result of parsing: tree, source, diagnostics, injections. |
| [`Kind`](#kind) | The kind of a token or node. `Copy`, compares like an enum; dense index; carries a field label in format-2 trees. |
| [`Field`, `Cardinality`](#field-and-cardinality) | A labelled field of a node kind (format 2). |
| [`Injection`](#injection) | A range of a parse covered by an injection (format 2). |
| [`Sketch`](#sketch) | The files of a multi-file sketch. |
| [`ImageError`](#imageerror) | Why bytes did not load as a language image. |
| [`Capability`](#capability) | A boxed `pass_lang::Pass<Parse>`, included by a schematic by name. |
| [`Error`](#error) | Every problem with a schematic, as coded diagnostics with spans. |

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
lang-forge = "2.0.0-alpha.1"
```

The crate is `no_std`-compatible: disable default features and it needs only
`alloc`.

```toml
[dependencies]
lang-forge = { version = "2.0.0-alpha.1", default-features = false }
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

## Format 2 (LSF2)

A sketch chooses its format in `[sketch]`:

| The sketch says | It is read as |
|---|---|
| no `[sketch]` table | format 1: exactly lang-forge 1.x |
| `[sketch]` with only `format = 1` | format 1: exactly lang-forge 1.x |
| `[sketch] format = 2` | format 2 (LSF2), described here |
| `format = 3` or higher | refused: `LSF1101` "sketch format 3 is newer than this lang-forge" |

Format 1 is not "the old reader with bugs": it is the 1.x reader itself, kept
unchanged, and every 1.x test runs against it. Everything above this section
describes format 1; every format-1 key keeps its meaning in format 2.

Format 2 is a superset of format 1 with tightenings (LSF2 §2.3): `[language]
name` must be a language name (`[a-z][a-z0-9_]*`, free text goes to
`display_name`), `version` is required and must be SemVer, rule names are
`_?[a-z][a-z0-9_]*` (all-uppercase names are token classes), and a greedy
repetition that would silently reject valid input is an error (`LSF4301`).
Unknown sections and keys are refused, with a suggestion when one is near;
sections that other LexerSketch crates read (`[semantics]`, `[types]`,
`[runtime]`, `[stdlib]`, `[exec]`, `[tooling]`, `[lints]`, `[macros]`,
`[emit]`, `[migrate]`) are accepted and left to them.

The full format is specified in LexerSketch's `specs/LSF2.md`; this section
shows what lang-forge does with it, feature by feature, and the
[coverage table](#lsf2-coverage) lists every key of LSF2's syntax sections
with its status in this release.

### Token classes

`[lexer.tokens]` declares token classes of the language's own: a regular
expression or a fixed text, with conditions and mode actions. A class name
is `[A-Z][A-Z0-9_]*` and is used in rules like `IDENT`.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r#"
[sketch]
format = 2

[language]
name = "vars"
version = "1.0.0"

[lexer.tokens]
VARIABLE = { regex = '\$[\p{XID_Start}_]\p{XID_Continue}*' }
LIFETIME = { regex = "'[a-z]+" }
PRAGMA   = { regex = "@[a-z]+", trivia = true }

[rules]
file = "(VARIABLE | LIFETIME)*"
"#)?;

let names: Vec<&str> = lang
    .lex("$name 'a @inline $ü")
    .iter()
    .filter(|t| !lang.kind_name(*t.kind()).starts_with("WHITESPACE"))
    .map(|t| lang.kind_name(*t.kind()))
    .collect();
assert_eq!(names, ["VARIABLE", "LIFETIME", "PRAGMA", "VARIABLE"]);
# Ok::<(), lang_forge::Error>(())
```

The regex dialect (LSF2 §9.12) is the one regular languages need: literals,
`.`, classes with ranges and negation, `\d \w \s` (ASCII), `\p{XID_Start}`
and `\p{XID_Continue}`, groups, `|`, `* + ?`, and bounded `{m,n}`. There are no
back-references or lookaround, so every class compiles to a DFA when the
sketch is forged and matches in linear time. A regex that matches the empty
string is refused (`LSF3203`), and so is one whose automaton would pass the
budget (`LSF3201`, for example `(a|b)*a(a|b){12}`).

How a token is chosen (LSF2 §9.13): of the candidates active in the current
mode whose conditions hold, the longest match wins; on equal length, the
higher `priority`, then the category (grammar literal, token class, string
opener, comment opener, `NUMBER`, `IDENT`, whitespace), then the earlier
declaration. An identifier-shaped literal only matches a whole identifier
(`letter` is never `let` + `ter`), and contextual keywords always lex as
`IDENT`. Conditions: `followed_by` / `not_followed_by` (one character
class), `when_prev` / `unless_prev` (the previous significant token, as for
regex literals in JavaScript), `line_start` (with `indented`), and `column`.

### Modes

A mode is a set of active tokens; the lexer keeps a stack of them. `push`,
`pop`, and `switch` actions on tokens (or a mode's `actions` table) move
between them, and a mode's `text` class takes every run of characters at
which no other token of the mode starts — the template text of PHP, Mox, or
Jinja:

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r#"
[sketch]
format = 2

[language]
name = "page"
version = "1.0.0"

[lexer]
initial_mode = "html"

[lexer.tokens]
OPEN  = { literal = "<%", modes = ["html"], action = "switch main" }
CLOSE = { literal = "%>", action = "switch html" }

[lexer.modes.html]
tokens = ["OPEN"]
text = "HTML"

[rules]
page = "(HTML | OPEN items:IDENT* CLOSE)*"
"#)?;

let parse = lang.parse("<p><% user %></p>");
assert!(!parse.has_errors());
assert!(parse.dump().contains("HTML@0..3 \"<p>\""));
# Ok::<(), lang_forge::Error>(())
```

The stack is bounded by `[lexer] max_mode_depth` (default 256): past it, one
error is reported and no further frame is pushed. A pushed frame still open
at the end of the input is an error (`LF0010`) unless its mode says
`eof = "ok"`.

### Strings

Named string classes, `[lexer.strings.NAME]`, add what format 1's `strings`
array cannot express: delimiters made of parts (captured and back-referenced
text for raw strings and heredocs, regexes, line breaks), interpolation holes
parsed by a rule, embedded tokens, escape tokens, closing delimiters that
must start a line, and multi-line bodies. A class with interpolation,
embedded tokens, or escape tokens builds a **node** of generated kinds
(`NAME_OPEN`, `NAME_TEXT`, `NAME_INTERP`, `NAME_CLOSE`, …, labelled `open`,
`parts`, `close`); otherwise it is one token. `STRING` in a rule means any
string class.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r##"
[sketch]
format = 2

[language]
name = "strs"
version = "1.0.0"

[lexer.strings.DQ]
open = '"'
interpolate = [{ open = "{", close = "}", rule = "expr" }]

[lexer.strings.RAW]
open = ["r", { capture = "#*" }, '"']
close = ['"', { backref = true }]
escape = ""

[lexer.strings.HEREDOC]
open = ["<<<", { capture = '[A-Z]+' }, { newline = true }]
close = [{ backref = true }]
close_at = "line-start-indented"
multiline = true

[rules]
file = "expr*"
expr = "STRING | IDENT"
"##)?;

let src = "\"a {b} c\" r#\"say \"hi\"\"# <<<EOT\n  text\n  EOT";
let parse = lang.parse(src);
assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
let dump = parse.dump();
assert!(dump.contains("parts:DQ_INTERP@3..6"));
assert!(dump.contains("RAW@10..23"));
assert!(dump.contains("HEREDOC@24..43"));
# Ok::<(), lang_forge::Error>(())
```

An unterminated string ends at the end of the input (or of the line, unless
`multiline`), with one `LF0002` error and zero-width closing tokens, so the
tree stays lossless and well formed. An `embedded` token with `parse = "rule"`
is a self-injection: its text is parsed by that rule, and the tree is
attached to the [`Parse`](#parse) ([`Parse::injections`](#parseinjections)).

### Keywords

`[lexer.keywords]` makes keywords contextual, reserved, or case-insensitive:

| Key | Effect |
|---|---|
| `contextual = ["async", "await"]` | lexed as `IDENT`, matched as the keyword only where the grammar asks for it; elsewhere an ordinary identifier. In the tree the token has the keyword's kind where it was matched as one. |
| `default = "contextual"` | every keyword is contextual except those in `reserved` (SQL's huge non-reserved sets) |
| `reserved = ["goto"]` | a keyword even if no rule uses it |
| `case = "ascii-insensitive"` | keywords match regardless of ASCII case (`SELECT`, `Select`) |

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r#"
[sketch]
format = 2

[language]
name = "ctx"
version = "1.0.0"

[lexer.keywords]
contextual = ["async", "await"]

[rules]
file = "stmt*"
stmt = "'async'? 'fn' name:IDENT ';' | expr ';'"

[rules.expr]
operand = "IDENT | NUMBER"
levels = [{ prefix = ["await"] }]
"#)?;

// `async` and `await` are keywords where the grammar wants them, and names
// everywhere else.
let parse = lang.parse("async fn async; await x; await;");
assert!(!parse.has_errors());
let dump = parse.dump();
assert!(dump.contains("async@0..5"));
assert!(dump.contains("name:IDENT@9..14 \"async\""));
assert!(dump.contains("op:await@16..21"));
assert!(dump.contains("IDENT@25..30 \"await\""));
# Ok::<(), lang_forge::Error>(())
```

### Numbers, comments, identifiers

`[lexer.numbers]` chooses the literal forms (`radix`, `radix_case`,
`separators`, `separator_rule`, `floats`, `exponent`, `leading_dot`,
`trailing_dot`, `hex_floats`, `leading_zeros`, `suffixes`); malformed
literals are one `LF0004`–`LF0008` or `LF0014` error each and stay one token.
Comments take a table form with `doc` (a `DOC_COMMENT` token),
`not_followed_by`, `stop_before` (PHP's `// … ?>`), `nested`, and `modes`.
`identifiers` takes a table form with `style`, `extra_start`,
`extra_continue`, and `normalize = "require-nfc"` (an identifier not in NFC
is an `LF0009` error). `shebang = true` makes a first `#!` line a `SHEBANG`
trivia token.

### Layout

`[layout] style = "indent"` applies the offside rule: `INDENT` and `DEDENT`
tokens from indentation, line breaks as `NEWLINE` per
`[layout.newlines] mode` (`trivia`, `significant`, or `terminators` with
`terminate_after` / `continue_before`), and no layout inside `implicit_join`
brackets or after an `explicit_join` text.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(r#"
[sketch]
format = 2

[language]
name = "py"
version = "1.0.0"

[layout]
style = "indent"
open_after = [":"]
newlines = { mode = "significant" }

[rules]
file = "stmt*"
stmt = "simple | block"
simple = "IDENT+ NEWLINE"
block = "'if' cond:IDENT ':' NEWLINE INDENT body:stmt+ DEDENT"
"#)?;

let parse = lang.parse("if a:\n    x\n    if b:\n        y\nz\n");
assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
# Ok::<(), lang_forge::Error>(())
```

### Labels and fields

A format-2 rule can label its elements, `name:element`: a token, a node, or a
group (every child of the group carries the label). The label is stored on
the tree **edge** — in the child's [`Kind`](#kind) — so every tree a format-2
language builds carries its fields, cloned or not. Pratt operator nodes are
labelled `lhs`, `op`, `rhs` (binary) and `op`, `operand` (prefix, postfix).

[`Language::field_label`](#languagefield_label) has the shape of lower-lang's
`Labeler::label`, so handing a forged tree's labels to lower-lang is one line;
[`Language::fields`](#languagefields) lists the fields of a node kind, with
their kinds and cardinality, derived from the grammar.

### Predicates, back-references, and position assertions

| Element | Meaning |
|---|---|
| `&e` | succeeds if `e` matches here; consumes nothing, builds nothing |
| `!e` | succeeds if `e` does not match here |
| `IDENT=name` | an `IDENT` whose text equals the text labelled `name` earlier in the rule (`'end' IDENT=name` for `begin a … end a`) |
| `EOF` | the end of the input, zero-width |
| `LINE_START` | the next token is the first on its line |
| `NL_BEFORE` | a line break precedes the next token (restricted productions: `return` / `throw` in JavaScript) |
| `WORD` | an identifier or any keyword (member names that may be keywords) |

Predicates make the overlap check's suggested fix expressible: a
trailing-comma list `x (&(',' !')') ',' x)* ','?` no longer commits on the
last comma.

### Rule options and the overlap check

A rule can be a table, `[rules.name]` with `rule = "…"`, to take options:
`sync` (extra recovery tokens for this rule), `allow = ["overlap"]`, and `doc`.
Expression rules take `prec` on levels.

The overlap check (`LSF4301`, LSF2 §11.8) refuses a greedy `*`, `+`, or `?`
that commits on a token `t` after which its body and its follower diverge,
with a witness input:

```rust
use lang_forge::Language;

let err = Language::from_lsf(r#"
[sketch]
format = 2

[language]
name = "o"
version = "1.0.0"

[rules]
file = "'if' IDENT tail* IDENT ';'"
tail = "IDENT ':'"
"#).unwrap_err();
assert_eq!(
    err.diagnostics()[0].message(),
    "input `IDENT ;` is rejected: this repetition commits on `IDENT` and then needs `:`"
);
```

The check is LL(2): deeper divergences are not detected. A rule acknowledges
an intended case with `allow = ["overlap"]`, and `[sketch.checks]` sets the
level of `overlap`, `unused_rule` (`LSF4302`), and `unused_token`
(`LSF3401`) to `deny`, `warn`, or `allow`. Warnings are in
[`Language::warnings`](#languagewarnings).

### Injections and `[ast]`

`[injections.id]` marks ranges of a source as another language: `target` (a
token class or node kind), `language` (`"self"` or a name), `resolve`
(`sketch` or `editor`), `start`, `content`, `combined`, and `scope`.
Self-injections are parsed and attached to the [`Parse`](#parse); every other
injection is listed with its range for the caller or the editor. `[ast]`
declares supertypes (`Expression = ["binary", "call", …]`, nested
supertypes expanded), read with [`Language::supertype`](#languagesupertype).
`[hooks]` entries are validated (names, kinds, keys); a construct that would
need a hook to run is refused as not supported in this release.

### Kinds in format 2

Format-2 kinds are numbered as LSF2 §5.4 prescribes, so the numbering is
public and stable for a given sketch: the 12 built-in kinds first
(`WHITESPACE` 0, `COMMENT`, `DOC_COMMENT`, `UNKNOWN`, `IDENT` 4, `NUMBER` 5,
`STRING` 6, `NEWLINE` 7, `INDENT`, `DEDENT`, `SHEBANG`, `COLUMN_TRIVIA`), then
token classes and generated string kinds, literals, `ERROR`, rule nodes,
operator nodes, and generated string nodes. [`Kind::index`](#kind) is the
number; [`Language::kind_at`](#languagekind_at) is the inverse. An operator
node may share a keyword's name (`instanceof`); `kind("kind:instanceof")` is
the node and `kind("'instanceof'")` the keyword.

### LSF2 coverage

Every key of LSF2's syntax sections (§3, §4, §7–§14) and its status in
`2.0.0-alpha.1`. **Yes**: read and applied. **Checked**: read and validated;
its effect belongs to another crate (named), so lang-forge applies nothing.
**Refused**: read and refused — with `LSF1007` naming the roadmap item
unless another code is given — never silently ignored. Items marked *alpha.2* are scheduled in
[`../dev/ROADMAP.md`](../dev/ROADMAP.md) with the reason.

| Section | Key | Status |
|---|---|---|
| §3 files | multi-file sketch: entry, `modules`, parts, `SourceMap`, paths (§3.1–§3.4) | Yes ([`Sketch`](#sketch)) |
| §3 files | `[sketch] kind = "mixin"` | Refused: a mixin is neither an entry nor a part (`LSF2022`, `LSF2024`); *alpha.2*, with composition |
| §3 files | sketch references (§3.5) | Refused with `extends` (*alpha.2*) |
| §4 composition | `[language] extends`, `[compose]`, `[sketch] requires`, mixins | Refused (*alpha.2*) |
| `[sketch]` | `format` | Yes |
| `[sketch]` | `kind` (`language`, `part`) | Yes |
| `[sketch]` | `modules` | Yes |
| `[sketch]` | `requires` | Refused (*alpha.2*) |
| `[sketch.checks]` | `overlap`, `unused_rule`, `unused_token` | Yes |
| `[sketch.checks]` | `unmapped_node` | Checked (level read; the check is `[semantics]`', not lang-forge's) |
| `[language]` | `name`, `version`, `extensions`, `start` | Yes (format-2 tightenings applied) |
| `[language]` | `display_name`, `description`, `edition`, `shebang_names` | Yes (validated and exposed) |
| `[language]` | `files` (`mode`, `start` per extension) | Yes ([`Language::parse_file`](#languageparse_file)) |
| `[language]` | `extends` | Refused (*alpha.2*) |
| `[lexer]` | `identifiers` (string and table: `style`, `extra_start`, `extra_continue`, `normalize`) | Yes |
| `[lexer]` | `identifiers.confusables` | Checked; `warn`/`deny` give an `LSF1007` warning: UAX #39 data is not in unicode-lang yet |
| `[lexer]` | `newlines`, `line_comments`, `block_comments`, `nested_comments`, `strings` (array) | Yes |
| `[lexer]` | `brackets`, `initial_mode`, `shebang`, `max_mode_depth` | Yes |
| comments (table form) | `open`, `close`, `doc`, `nested`, `not_followed_by`, `stop_before`, `modes` | Yes |
| `[lexer.keywords]` | `default`, `contextual`, `reserved`, `case` | Yes |
| `[lexer.strings.X]` | `open`, `close` (strings and parts: text, `regex`, `capture`, `backref`, `newline`) | Yes |
| `[lexer.strings.X]` | `escape`, `multiline`, `interpolate` (`open`, `close`, `rule`, `mode`, `when_next`), `embedded` (`token`, `regex`, `parse`), `escape_tokens` | Yes |
| `[lexer.strings.X]` | `body`, `close_at`, `close_not_followed_by`, `modes` | Yes |
| `[lexer.strings.X]` | `rest_of_line = "empty"` | Yes |
| `[lexer.strings.X]` | `rest_of_line = "code"` | Refused (*alpha.2*: needs a pending-heredoc queue) |
| `[lexer.strings.X]` | `escapes` (named and table: `simple`, `octal`, `hex`, `unicode`, `unknown`, `bytes`), `dedent` | Checked (value decoding is lower-lang's) |
| `[lexer.numbers]` | `radix`, `radix_case`, `separators`, `separator_rule`, `floats`, `exponent`, `leading_dot`, `trailing_dot`, `hex_floats`, `leading_zeros`, `suffixes`, `run_on` | Yes |
| `[lexer.tokens]` | `regex`, `literal`, `trivia`, `priority`, `modes`, `action`, `followed_by`, `not_followed_by`, `when_prev`, `unless_prev`, `line_start`, `indented`, `column` | Yes |
| `[lexer.tokens]` | `doc` | Checked (free text; for tools) |
| `[lexer.tokens]` | `hook` | Refused (*alpha.2*: scanner hooks) |
| `[lexer.modes.m]` | `inherit`, `literals`, `builtins`, `trivia`, `tokens`, `strings`, `text`, `actions`, `eof` | Yes |
| `[lexer.split]` | token splitting | Refused (*alpha.2*) |
| `[lexer.columns]` | `tab_width` | Yes |
| `[lexer.columns]` | `trivia` | Refused (*alpha.2*) |
| `[layout]` | `style` (`none`, `indent`), `open_after`, `implicit_join`, `explicit_join`, `tabs` (`width`, `mixed`), `blank_lines` | Yes |
| `[layout]` | `style = "hook"`, `hook` | Refused (*alpha.2*: layout hooks) |
| `[layout.newlines]` | `mode`, `terminate_after`, `continue_before`, `ignored_inside` | Yes |
| `[layout.newlines]` | `soft_terminators` | Refused (*alpha.2*) |
| `[rules]` | string rules: labels, `&` `!`, `IDENT=label`, `WORD`, `EOF`, `LINE_START`, `NL_BEFORE`, `INDENT`, `DEDENT`, `STRING` as any string class | Yes |
| `[rules]` | `@hook` (predicate hooks), `%mode(...)` | Refused (*alpha.2*) |
| `[rules.r]` | `rule`, `sync`, `allow`, `doc` | Yes |
| `[rules.r]` | `newlines` (rule-scoped) | Refused (*alpha.2*) |
| expression rules | `operand`, `levels` (`left`, `right`, `none`, `prefix`, `postfix`, `then`, `node`, `prec`), contextual-keyword operators, `doc` | Yes |
| expression rules | `newlines`, `dynamic` | Refused (*alpha.2*) |
| `[injections.id]` | `target`, `language`, `resolve`, `start`, `content`, `combined`, `scope` | Yes (self-injections parsed; others listed as ranges) |
| `[injections.id]` | `when`; `combined = true` on a self-injection | Refused (*alpha.2*) |
| `[hooks.id]` | `kind`, `capability`, `tokens`, `token`, `into`, `options` | Checked (hooks are declared and validated; running one needs the hook constructs refused above) |
| `[ast]` | supertypes | Yes |

Checks LSF2 lists that this release does not run: `LSF3113` (DFA ambiguity
between classes), `LSF3125` shadowing analysis beyond format 1's literal
check, and `LSF4303`. They are on the alpha.2 list.

## Diagnostic codes

Every diagnostic lang-forge produces carries a [`diag_lang::Code`](https://docs.rs/diag-lang),
shown in the rendered header (`error[LSF4101]: undefined rule `item``) and
stable once released: a code is never reused for a different problem.
Format-1 diagnostics carry codes too; their messages are unchanged.

| Code | Where | Meaning |
|---|---|---|
| `LSF0001`–`LSF0002` | sketch | invalid NOML; a NOML feature a sketch may not use |
| `LSF1001`–`LSF1007` | sketch | unknown section or key, wrong type, missing, needs format 2, out of range, not supported in this release |
| `LSF1101`–`LSF1107` | sketch | format, version, edition, extensions, `files` |
| `LSF1201`–`LSF1202` | sketch | ambiguous or unknown token reference |
| `LSF2002`–`LSF2024` | sketch | multi-file sketches |
| `LSF3101`–`LSF3125` | sketch | lexer keys and checks |
| `LSF3201`–`LSF3206` | sketch | token regexes |
| `LSF3301`–`LSF3303` | sketch | modes |
| `LSF3401` | sketch | unused token class (warning by default) |
| `LSF4101`–`LSF4112` | sketch | rule text: undefined names, names, labels, `prec`, back-references, syntax |
| `LSF4301`–`LSF4310` | sketch | grammar checks: overlap, unused rule, left recursion, empty repetition, dead alternative, expression-rule checks |
| `LSF4401`–`LSF4405` | sketch | injections and hooks |
| `LSF4501`–`LSF4502` | sketch | `[ast]` |
| `LSF7001`, `LSF7004`–`LSF7005` | sketch, pipeline | capability names; a capability missing from, or ambiguous in, the registry |
| `LSF8001`–`LSF8003` | sketch | paths |
| `LSF8101`, `LSF8201`–`LSF8202` | sketch | language names, free text, literal text |
| `LSF9001`–`LSF9008` | sketch | limits |
| `LF0001`–`LF0014` | source | lexical errors: unexpected characters, unterminated strings and comments, malformed numbers, NFC, modes, indentation |
| `LF1000`–`LF1004` | source | parse errors: expected token, leftover input, chained non-associative operator, nesting too deep, back-reference mismatch |
| `LF9001` | source | a source larger than 4 GiB |

Source codes follow diag-lang's phase ranges, so "is this a lexical error?"
is a range check:

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"c\"\n[rules]\nfile = \"(IDENT ';')*\"\n")?;
let parse = lang.parse("a; ` b");
let lexical = |d: &&lang_forge::diag_lang::Diagnostic| d.code().is_some_and(|c| c.prefix() == "LF" && c.number() < 1000);
assert_eq!(parse.diagnostics().iter().filter(lexical).count(), 1); // the backtick
assert_eq!(parse.diagnostics().iter().filter(|d| !lexical(d)).count(), 1); // the missing `;`
# Ok::<(), lang_forge::Error>(())
```

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
// The header carries the diagnostic's code (2.0).
assert!(rendered.contains("error[LSF4101]: undefined rule `item`"));
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

### `Language::from_sketch`

```rust,ignore
pub fn from_sketch(sketch: &Sketch) -> Result<Language, Error>
```

Forges a language from a [`Sketch`](#sketch) of one or more files: the entry
first, then the parts its `[sketch] modules` lists (LSF2 §3). The parts merge
as one document; every diagnostic points into the file it is about, through
the sketch's `SourceMap`. A one-file sketch forges exactly as
[`from_lsf`](#languagefrom_lsf) forges its text.

**Errors:** everything `from_lsf` reports, plus a module not in the sketch or
listed twice (`LSF2023`, `LSF2005`), a part not marked `kind = "part"` or
holding `[language]` or `modules` (`LSF2024`, `LSF2022`, `LSF2003`), a key
defined in two files (`LSF2002`, with both places labelled), and a missing
`[language] start` when rules span files (`LSF2006`).

```rust
use lang_forge::{Language, Sketch};

let mut sketch = Sketch::new();
sketch.add("calc.lsf", "[sketch]\nformat = 2\nmodules = [\"rules/expr.lsf\"]\n\
    [language]\nname = \"calc\"\nversion = \"1.0.0\"\nstart = \"program\"\n\
    [rules]\nprogram = \"stmt*\"\nstmt = \"value:expr ';'\"\n")?;
sketch.add("rules/expr.lsf", "[sketch]\nformat = 2\nkind = \"part\"\n\
    [rules.expr]\noperand = \"NUMBER\"\nlevels = [{ left = [\"+\"] }, { left = [\"*\"] }]\n")?;
let calc = Language::from_sketch(&sketch)?;
assert!(!calc.parse("1 + 2 * 3;").has_errors());

// Problems are reported in the file they are in.
let mut broken = Sketch::new();
broken.add("a.lsf", "[sketch]\nformat = 2\nmodules = [\"b.lsf\"]\n[language]\nname = \"a\"\nversion = \"1.0.0\"\nstart = \"x\"\n[rules]\nx = \"y\"\n")?;
broken.add("b.lsf", "[sketch]\nformat = 2\nkind = \"part\"\n[rules]\ny = \"missing\"\n")?;
let err = Language::from_sketch(&broken).unwrap_err();
assert_eq!(err.to_string(), "b.lsf:5:6: undefined rule `missing`");
# Ok::<(), lang_forge::Error>(())
```

### `Language::to_image` and `Language::from_image`

```rust,ignore
pub fn to_image(&self) -> Vec<u8>
pub fn from_image(bytes: &[u8]) -> Result<Language, ImageError>
```

A forged language as bytes (a `.lsl` image, ISSUES M12), and back. Loading an
image skips forging entirely. Images are deterministic: the same sketch,
forged by the same lang-forge, writes the same bytes on every platform and
every run. Forge-time [`warnings`](#languagewarnings) are not stored.

`from_image` treats its input as untrusted: the header, length, and FNV-1a
hash are checked first, every length is checked against the bytes left
before anything is allocated, every text is checked to be UTF-8, and once
decoded, every index the lexer and parser follow at run time is checked to be
in range. Bytes either load as a language that parses any input without
panicking, or are refused. Property tests mutate real images (byte flips,
small integers written over lengths and indices, truncations), re-seal the
hash so the decoder rather than the hash judges them, and require exactly
that.

**Errors:** [`ImageError`](#imageerror).

```rust
use lang_forge::{ImageError, Language};

let lang = Language::from_lsf("[language]\nname = \"sum\"\n[rules]\nsum = \"NUMBER ('+' NUMBER)*\"\n")?;
let image = lang.to_image();
let loaded = Language::from_image(&image).expect("its own image");
assert_eq!(loaded.parse("1 + 2").dump(), lang.parse("1 + 2").dump());
assert_eq!(loaded.to_image(), image);

assert_eq!(Language::from_image(b"not an image").unwrap_err(), ImageError::NotAnImage);
# Ok::<(), lang_forge::Error>(())
```

### `Language::format`

```rust,ignore
pub fn format(&self) -> u8
```

The sketch format the language was forged from: `1` or `2`.

```rust
use lang_forge::Language;

let v1 = Language::from_lsf("[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n")?;
let v2 = Language::from_lsf("[sketch]\nformat = 2\n[language]\nname = \"a\"\nversion = \"1.0.0\"\n[rules]\na = \"IDENT\"\n")?;
assert_eq!((v1.format(), v2.format()), (1, 2));
# Ok::<(), lang_forge::Error>(())
```

### `Language::display_name`, `description`, `edition`, `shebang_names`

```rust,ignore
pub fn display_name(&self) -> &str
pub fn description(&self) -> Option<&str>
pub fn edition(&self) -> Option<&str>
pub fn shebang_names(&self) -> impl Iterator<Item = &str>
```

The format-2 `[language]` identity keys. `display_name` falls back to
[`name`](#languagename); the others are `None` or empty for a format-1
language.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"mox\"\nversion = \"0.1.0\"\ndisplay_name = \"Mox\"\n\
     description = \"A modern PHP.\"\nedition = \"1\"\nshebang_names = [\"mox\"]\n[rules]\nfile = \"IDENT*\"\n",
)?;
assert_eq!(lang.display_name(), "Mox");
assert_eq!(lang.description(), Some("A modern PHP."));
assert_eq!(lang.edition(), Some("1"));
assert_eq!(lang.shebang_names().collect::<Vec<_>>(), ["mox"]);
# Ok::<(), lang_forge::Error>(())
```

### `Language::kind_count`

```rust,ignore
pub fn kind_count(&self) -> usize
```

The number of kind indices the language uses, so kinds can be kept in dense
tables (`Vec` indexed by [`Kind::index`](#kind)) instead of maps (ISSUES M04).
One index, the internal end-of-input marker, has no kind; see
[`kind_at`](#languagekind_at).

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"t\"\n[rules]\nf = \"(IDENT | NUMBER)*\"\n")?;
let mut counts = vec![0usize; lang.kind_count()];
for token in lang.parse("a 1 b").tree().tokens() {
    counts[usize::from(token.kind().index())] += 1;
}
assert_eq!(counts[usize::from(lang.kind("IDENT").expect("built in").index())], 2);
# Ok::<(), lang_forge::Error>(())
```

### `Language::kind_at`

```rust,ignore
pub fn kind_at(&self, index: u16) -> Option<Kind>
```

The kind with index `index`, the inverse of [`Kind::index`](#kind); `None`
past [`kind_count`](#languagekind_count) and for the end-of-input marker.

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"NUMBER\"\n")?;
let number = lang.kind("NUMBER").expect("built in");
assert_eq!(lang.kind_at(number.index()), Some(number));
assert_eq!(lang.kind_at(u16::MAX), None);
# Ok::<(), lang_forge::Error>(())
```

### `Language::root_kind`

```rust,ignore
pub fn root_kind(&self) -> Kind
```

The kind of every tree's root: the start rule's node.

```rust
use lang_forge::Language;

let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nfile = \"NUMBER*\"\n")?;
assert_eq!(*lang.parse("1 2").tree().kind(), lang.root_kind());
# Ok::<(), lang_forge::Error>(())
```

### `Language::label_name` and `Language::label_id`

```rust,ignore
pub fn label_name(&self, label: u16) -> Option<&str>
pub fn label_id(&self, name: &str) -> Option<u16>
```

Field labels (format 2) by number and by name. Labels are numbered by first
occurrence over the rules in order (LSF2 §5.4); the number is what lower-lang's
`Pick::Label` takes. A format-1 language has no labels.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n\
     [rules]\nassign = \"target:IDENT '=' value:NUMBER\"\n",
)?;
assert_eq!(lang.label_id("value"), Some(1));
assert_eq!(lang.label_name(0), Some("target"));
assert_eq!(lang.label_id("missing"), None);
# Ok::<(), lang_forge::Error>(())
```

### `Language::field_label`

```rust,ignore
pub fn field_label(&self, parent: &Node<Kind>, index: usize) -> Option<u16>
```

The label of `parent`'s child at `index` (every child, trivia included, as
`Node::children` yields them), or `None` for an unlabelled child or an index
out of range. This is lower-lang's `Labeler::label` (ISSUES P16): the adapter
is `fn label(&self, p: &Node<Kind>, i: usize) -> Option<u16> { self.0.field_label(p, i) }`.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"w\"\nversion = \"1.0.0\"\n\
     [rules]\nwhile_stmt = \"'while' cond:IDENT body:block\"\nblock = \"'{' '}'\"\n",
)?;
let parse = lang.parse("while ready { }");
let root = parse.tree();
let names: Vec<Option<&str>> =
    (0..root.len()).map(|i| lang.field_label(root, i).and_then(|l| lang.label_name(l))).collect();
assert_eq!(names, [None, None, Some("cond"), None, Some("body")]);
# Ok::<(), lang_forge::Error>(())
```

### `Language::fields`

```rust,ignore
pub fn fields(&self, kind: Kind) -> impl Iterator<Item = Field<'_>>
```

The fields of node kind `kind`: every label its children can carry, with the
kinds the field can hold and its [`Cardinality`](#field-and-cardinality),
derived from the grammar (LSF2 §11.3). Empty for a kind with no labelled
children and for every format-1 kind.

```rust
use lang_forge::{Cardinality, Language};

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"c\"\nversion = \"1.0.0\"\n\
     [rules]\ncall = \"callee:IDENT '(' (args:NUMBER (',' args:NUMBER)*)? ')' tail:';'?\"\n",
)?;
let call = lang.kind("call").expect("a rule");
let fields: Vec<(&str, Cardinality)> = lang.fields(call).map(|f| (f.name(), f.cardinality())).collect();
assert_eq!(fields, [("callee", Cardinality::One), ("args", Cardinality::Many), ("tail", Cardinality::Optional)]);
# Ok::<(), lang_forge::Error>(())
```

### `Language::supertype` and `Language::supertypes`

```rust,ignore
pub fn supertype(&self, name: &str) -> Option<impl ExactSizeIterator<Item = Kind> + '_>
pub fn supertypes(&self) -> impl Iterator<Item = &str>
```

The `[ast]` supertypes (format 2): the members of one, with nested
supertypes expanded, and the names of all of them in sketch order.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"s\"\nversion = \"1.0.0\"\n\
     [rules]\nfile = \"(num | word)*\"\nnum = \"NUMBER\"\nword = \"IDENT\"\n\
     [ast]\nLiteral = [\"num\"]\nAtom = [\"Literal\", \"word\"]\n",
)?;
let atoms: Vec<&str> = lang.supertype("Atom").expect("declared").map(|k| lang.kind_name(k)).collect();
assert_eq!(atoms, ["num", "word"]);
assert_eq!(lang.supertypes().collect::<Vec<_>>(), ["Literal", "Atom"]);
# Ok::<(), lang_forge::Error>(())
```

### `Language::warnings`

```rust,ignore
pub fn warnings(&self) -> &[Diagnostic]
```

What forging found that did not stop it (format 2): checks set to `warn`
(unused rules `LSF4302`, unused token classes `LSF3401`, `overlap` when
relaxed), documented consequences of a setting (`LSF3120` for
`trailing_dot`), and keys this release reads but does not yet check
(`LSF1007`). Spans point into the sketch.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"w\"\nversion = \"1.0.0\"\n\
     [rules]\nfile = \"IDENT*\"\nforgotten = \"NUMBER\"\n",
)?;
assert_eq!(lang.warnings()[0].message(), "rule `forgotten` is never used");
# Ok::<(), lang_forge::Error>(())
```

### `Language::parse_file`

```rust,ignore
pub fn parse_file<'a>(&'a self, extension: &str, source: &'a str) -> Parse<'a>
```

Parses `source` as a file with extension `extension`, using the initial mode
and start rule `[language] files` gives that extension (format 2), or as
[`parse`](#languageparse) does when it gives none.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n\
     extensions = [\"x\", \"xs\"]\nfiles = { xs = { start = \"item\" } }\n\
     [rules]\nfile = \"item*\"\nitem = \"IDENT ';'\"\n",
)?;
let whole = lang.parse_file("x", "a; b;");
let single = lang.parse_file("xs", "a;");
assert_eq!(lang.kind_name(*whole.tree().kind()), "file");
assert_eq!(lang.kind_name(*single.tree().kind()), "item");
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

### `Parse::injections`

```rust,ignore
pub fn injections(&self) -> &[Injection<'a>]
```

The injections in this parse (format 2), in source order: self-injections
with their parsed trees, and other languages' ranges for the caller or the
editor. See [`Injection`](#injection). Empty for a format-1 language.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"p\"\nversion = \"1.0.0\"\n\
     [lexer]\ninitial_mode = \"page\"\n\
     [lexer.tokens]\nOPEN = { literal = \"<%\", modes = [\"page\"], action = \"switch main\" }\n\
     CLOSE = { literal = \"%>\", action = \"switch page\" }\n\
     [lexer.modes.page]\ntokens = [\"OPEN\"]\ntext = \"HTML\"\n\
     [injections.html]\ntarget = \"HTML\"\nlanguage = \"html\"\nresolve = \"editor\"\n\
     [rules]\nfile = \"(HTML | OPEN IDENT* CLOSE)*\"\n",
)?;
let parse = lang.parse("<b><% x %></b>");
let ranges: Vec<(&str, bool)> = parse.injections().iter().map(|i| (i.language(), i.is_editor())).collect();
assert_eq!(ranges, [("html", true), ("html", true)]);
# Ok::<(), lang_forge::Error>(())
```

## `Kind`

```rust,ignore
#[derive(Clone, Copy)]
pub struct Kind { /* private */ }

impl Kind {
    pub const fn index(self) -> u16;
    pub const fn label(self) -> Option<u16>;
    pub const fn unlabelled(self) -> Kind;
}

impl PartialEq, Eq, PartialOrd, Ord, Hash for Kind { /* by kind, ignoring the label */ }
impl syntax_lang::TokenKind for Kind { /* is_trivia */ }
impl Debug for Kind { /* Kind(index) */ }
```

The kind of a token or node. A schematic declares its vocabulary as text, so a
forged language cannot have a Rust `enum` of its own; `Kind` stands in for one:
a four-byte `Copy` value assigned when the language is forged. Tokens and nodes
share the type, as `syntax-lang` expects. Get kinds from
[`Language::kind`](#languagekind) and name them with
[`Language::kind_name`](#languagekind_name).

`index` is the kind's number in its language, dense from 0 (ISSUES M04), so
kinds can index tables; [`Language::kind_at`](#languagekind_at) is the
inverse. In a format-2 tree a kind also carries the **field label** of the
edge from its parent (`label`); equality, ordering, and hashing ignore the
label, so a labelled `IDENT` still equals `lang.kind("IDENT")`, and
`unlabelled` drops it. Format-1 kinds never carry a label.

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
use lang_forge::Language;

// A labelled kind compares equal to the plain kind.
let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"t\"\nversion = \"1.0.0\"\n[rules]\nf = \"name:IDENT\"\n",
)?;
let parse = lang.parse("x");
let token = parse.tree().tokens().next().expect("one token");
let ident = lang.kind("IDENT").expect("built in");
assert_eq!(*token.kind(), ident);
assert_eq!(token.kind().label().and_then(|l| lang.label_name(l)), Some("name"));
assert_eq!(token.kind().unlabelled().label(), None);
assert_eq!(ident.index(), 4);
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

## `Injection`

```rust,ignore
#[derive(Clone, Debug)]
pub struct Injection<'a> { /* private */ }

impl<'a> Injection<'a> {
    pub fn id(&self) -> &'a str;
    pub fn language(&self) -> &'a str;
    pub fn span(&self) -> Span;
    pub fn tree(&self) -> Option<&Node<Kind>>;
    pub fn is_editor(&self) -> bool;
}
```

A range of a parsed source covered by an injection (LSF2 §12): `id` is the
`[injections]` id (or the embedded token class's name), `language` is
`"self"` or the injected language's name, and `span` is the range. A
self-injection is parsed with the same language and its tree is in `tree`;
injections of other languages, and those with `resolve = "editor"`, have no
tree — the caller or the editor handles them. Injected trees are searched for
further injections to a depth of 16.

```rust
use lang_forge::Language;

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"s\"\nversion = \"1.0.0\"\n\
     [lexer.tokens]\nVAR = { regex = '\\$[a-z]+' }\n\
     [lexer.strings.DQ]\nopen = '\"'\nembedded = [{ token = \"DQ_VAR\", regex = '\\$[a-z]+(\\.[a-z]+)*', parse = \"path\" }]\n\
     interpolate = [{ open = \"{\", close = \"}\", rule = \"path\" }]\n\
     [rules]\nfile = \"DQ*\"\npath = \"VAR ('.' IDENT)*\"\n",
)?;
let src = "\"hello $user.name\"";
let parse = lang.parse(src);
let injection = &parse.injections()[0];
assert_eq!((injection.id(), injection.language()), ("DQ_VAR", "self"));
assert_eq!(injection.tree().and_then(|t| t.text(src)), Some("$user.name"));
# Ok::<(), lang_forge::Error>(())
```

## `Field` and `Cardinality`

```rust,ignore
#[derive(Clone, Copy, Debug)]
pub struct Field<'a> { /* private */ }

impl<'a> Field<'a> {
    pub fn label(&self) -> u16;
    pub fn name(&self) -> &'a str;
    pub fn cardinality(&self) -> Cardinality;
    pub fn kinds(&self) -> impl Iterator<Item = Kind> + 'a;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Cardinality { One, Optional, Many }
```

A field of a node kind, from [`Language::fields`](#languagefields): its
label (id and name), how many children carry it, and the kinds they can
have, in index order. `Cardinality` is `One` (exactly one child), `Optional`
(none or one), or `Many` (any number, a list field).

```rust
use lang_forge::{Cardinality, Language};

let lang = Language::from_lsf(
    "[sketch]\nformat = 2\n[language]\nname = \"x\"\nversion = \"1.0.0\"\n\
     [rules]\npair = \"key:IDENT '=' value:(NUMBER | IDENT)\"\n",
)?;
let pair = lang.kind("pair").expect("a rule");
let value = lang.fields(pair).find(|f| f.name() == "value").expect("labelled");
assert_eq!(value.cardinality(), Cardinality::One);
assert_eq!(value.kinds().map(|k| lang.kind_name(k)).collect::<Vec<_>>(), ["IDENT", "NUMBER"]);
# Ok::<(), lang_forge::Error>(())
```

## `Sketch`

```rust,ignore
#[derive(Clone, Debug, Default)]
pub struct Sketch { /* private */ }

impl Sketch {
    pub fn new() -> Sketch;
    pub fn add(&mut self, path: &str, text: impl Into<Box<str>>) -> Result<(), Error>;
    pub fn source_map(&self) -> &SourceMap;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

The files of a multi-file sketch (ISSUES M13), forged with
[`Language::from_sketch`](#languagefrom_sketch). Add the entry first, then its
parts, each under its project-relative path. Paths are checked and
normalized as LSF2 §3.4 requires: `/`-separated, no leading `/`, drive
letter, `\`, empty or `.` component, no `..` that leaves the root
(`LSF8001`), every component portable to every OS (`LSF8002`), and no two
paths that differ only by ASCII case (`LSF8003`). Budgets: 8 MiB per file
(`LSF9001`), 64 MiB in total (`LSF9002`), 1024 files (`LSF9003`). A refused
`add` leaves the sketch unchanged. The files go into a `diag_lang::SourceMap`
in the order added, so diagnostics render against the right file.

```rust
use lang_forge::Sketch;

let mut sketch = Sketch::new();
sketch.add("lang/main.lsf", "")?;
sketch.add("lang/parts/../lexer.lsf", "")?; // normalized to lang/lexer.lsf
assert!(sketch.add("../outside.lsf", "").is_err());
assert!(sketch.add("LANG/MAIN.lsf", "").is_err());
assert_eq!(sketch.len(), 2);
let names: Vec<&str> = sketch.source_map().iter().map(|(_, f)| f.name()).collect();
assert_eq!(names, ["lang/main.lsf", "lang/lexer.lsf"]);
# Ok::<(), lang_forge::Error>(())
```

## `ImageError`

```rust,ignore
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageError { NotAnImage, Format(u16), Corrupt, Invalid }

pub const IMAGE_FORMAT: u16 = 1;
```

Why bytes could not be loaded by [`Language::from_image`](#languageto_image-and-languagefrom_image):
`NotAnImage` (no image header), `Format(n)` (an image of another format; forge
the sketch again), `Corrupt` (truncated, or the body does not match its hash),
or `Invalid` (the hash matches but the tables are not ones lang-forge builds).
`IMAGE_FORMAT` is the format this lang-forge writes and reads; it changes
whenever the layout does.

```rust
use lang_forge::{IMAGE_FORMAT, ImageError, Language};

let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nx = \"NUMBER\"\n")?;
let mut image = lang.to_image();
assert_eq!(u16::from_le_bytes([image[4], image[5]]), IMAGE_FORMAT);
image[4] = 9;
assert_eq!(Language::from_image(&image).unwrap_err(), ImageError::Format(9));
let mut image = lang.to_image();
image.truncate(image.len() - 1);
assert_eq!(Language::from_image(&image).unwrap_err(), ImageError::Corrupt);
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

This reference describes `2.0.0-alpha.1`, a **pre-release** of lang-forge
2.0. lang-forge follows [Semantic Versioning](https://semver.org/).

**What 2.0 keeps from 1.x.** A format-1 sketch (no `[sketch]` table, or
`[sketch] format = 1`) forges and parses exactly as under 1.x: the same
language, the same trees for every input (kind names, shapes, spans), and the
same diagnostic messages. The format-1 reader is the 1.x reader, unchanged,
and the whole 1.x test suite runs against it. Every 1.x method and type is
still there with the same signature, and the 1.x guarantees hold for both
formats: parsing never fails and never panics; every tree is lossless;
recursion is bounded at 768 grammar levels, so no input can exhaust the
stack; forging never panics; and `dump` keeps its format (format-2 trees add
`label:` prefixes to labelled lines).

**What 2.0 breaks.** Listed with a migration guide in the
[CHANGELOG](../CHANGELOG.md):

- Every diagnostic now carries a code, which `diag-lang` renders in the
  header: `error[LF1000]: expected `)`, found `;`` where 1.x rendered
  `error: expected `)`, found `;``. Messages and `Error`'s `Display` are
  unchanged; code that matches rendered text must allow for the code.
- `Kind` is four bytes, not two, and carries a field label in format-2 trees.
  Equality, ordering, and hashing ignore the label, so format-1 code is
  unaffected unless it relied on the size.
- `diag-lang` is required at `1.1` or later (diagnostic codes).

**What the alpha does not promise yet.** The format-2 surface — LSF2's
syntax as this release reads it, the new methods and types (`Sketch`,
`Injection`, `Field`, `Cardinality`, `ImageError`, `IMAGE_FORMAT`, and the
`Language` methods added in 2.0), kind numbering in format 2, field
derivation, and the image format — may change before `2.0.0`. The image
format is versioned (`IMAGE_FORMAT`): an image of another format is refused,
never misread, so re-forge after upgrading. Keys LSF2 specifies that this
release refuses with `LSF1007` are listed in the
[coverage table](#lsf2-coverage); they arrive in a later alpha.

**What 2.0.0 will promise** is the 1.x promise extended to format 2: the
surface, the format-1 and format-2 sketch formats, the trees for valid input,
diagnostic codes (a code is never reused for a different problem), and the
guarantees above. As in 1.x, `syntax-lang`, `diag-lang`, and `pass-lang`
are public dependencies, and the MSRV (Rust 1.85) rises only in a minor
release.

What is **not** promised, in any 2.x: the wording of diagnostics and of
`Error`'s `Display`; for input with errors, the exact shape of the recovered
tree and which errors are reported (recovery may get better); the order
problems are found in beyond source order; the `Debug` output of any type;
how much stack the parser needs beyond the bound stated in
[Error recovery](#error-recovery); and performance figures.

See [`../dev/ROADMAP.md`](../dev/ROADMAP.md) and
[`../CHANGELOG.md`](../CHANGELOG.md).
