<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br><b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>lang-forge</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

---

## [Unreleased]

---

## [2.0.0-alpha.1] - 2026-10-09

The first pre-release of 2.0: LSF2, LexerSketch's format-2 sketch syntax, as
the front end for the Mox demo (ROADMAP work package 2.1). A format-1
schematic — no `[sketch]` table, or `[sketch] format = 1` — forges and parses
exactly as in 1.x: the format-1 reader is unchanged and the whole 1.x test
suite runs against it. The format-2 surface may still change before 2.0.0.

### Breaking

- **Diagnostics carry codes.** Every diagnostic lang-forge produces, in both
  formats, now has a `diag_lang::Code`: `LSF` codes for sketches, `LF0001`–
  `LF0014` for lexical errors and `LF1000`–`LF1004` for parse errors in
  source (diag-lang's phase ranges), `LF9001` for a source over 4 GiB.
  `diag-lang`'s renderer shows the code in the header: `error[LF1000]:
  expected `)`, found `;`` where 1.x rendered `error: expected `)`, found
  `;``. The messages themselves and `Error`'s `Display` are unchanged.
- **`Kind` is four bytes** (it was two) and, in format-2 trees, carries the
  field label of the edge from its parent. `PartialEq`, `Eq`, `PartialOrd`,
  `Ord`, and `Hash` ignore the label, so a labelled `IDENT` equals
  `lang.kind("IDENT")` and format-1 code behaves as before. They are now
  implemented by hand rather than derived.
- **`diag-lang` 1.1** is the minimum (diagnostic codes).

### Migration guide

From 1.x, for a format-1 schematic:

1. Change `lang-forge = "1"` to `lang-forge = "2.0.0-alpha.1"`.
2. If you match **rendered** diagnostics (`Renderer::render` output), expect
   the code in the header: `error[LSF4101]: undefined rule …`,
   `error[LF1000]: expected …`. Matching `Diagnostic::message()` or `Error`'s
   `Display` needs no change. To branch on the kind of problem, use the code:
   `d.code().is_some_and(|c| c.number() < 1000)` is "a lexical error".
3. Nothing else: the schematic, the trees, and the API you use are the same.
   If you kept kinds in maps, [`Kind::index`](docs/API.md#kind) and
   [`Language::kind_count`](docs/API.md#languagekind_count) now allow dense
   tables instead.

To move a schematic to format 2 (LSF2 §2.3):

1. Add `[sketch]` with `format = 2`.
2. `[language] name` must be `[a-z][a-z0-9_]*` (put free text in
   `display_name`), and `version` is required and must be SemVer
   (`"1.0.0"`, not `"1.0"`).
3. Rule names must be `_?[a-z][a-z0-9_]*`: all-uppercase names are token
   classes in format 2.
4. Forge it. If the overlap check reports `LSF4301` with a witness input, a
   greedy repetition or optional was silently rejecting valid input under
   format 1: fix it with a predicate (`(&(',' x) ',' x)*`), or acknowledge an
   intended case with `[rules.r] allow = ["overlap"]`.
5. Then use what format 2 adds: label fields (`cond:expr`), declare token
   classes, modes, and string classes, and make keywords contextual.

### Added

- **Format 2 (LSF2) syntax sections**, read by a new reader beside the
  untouched format-1 one (DECISIONS D6: the crate's own NOML reader,
  extended):
  - `[sketch]`: `format`, `kind`, `modules`, `[sketch.checks]` (`overlap`,
    `unused_rule`, `unused_token`, `unmapped_node`).
  - `[language]`: format-2 tightenings, `display_name`, `description`,
    `edition`, `shebang_names`, `files` (per-extension mode and start rule).
  - `[lexer.tokens]`: custom token classes from regexes or fixed texts, with
    `trivia`, `priority`, `modes`, `action` (`push`/`pop`/`switch`),
    `followed_by`/`not_followed_by`, `when_prev`/`unless_prev`,
    `line_start`/`indented`, and `column`. The regex engine (UTF-8
    character classes, `\p{XID_Start}`/`\p{XID_Continue}`, bounded
    repetition, DFA per class with an automaton budget) is reimplemented in
    the crate from grammar-lang's construction rather than depended on:
    grammar-lang's lexer DFA works on bytes for its own token model, and
    lang-forge needs character-class alphabets, conditions, longest match
    across modes, and its own budgets and image encoding.
  - Lexer **modes** with a mode stack (`initial_mode`, `max_mode_depth`,
    `[lexer.modes.m]` with `inherit`, `literals`, `builtins`, `trivia`,
    `tokens`, `strings`, `text` fallback classes, `actions`, `eof`).
  - **String classes** `[lexer.strings.NAME]`: delimiters of parts (text,
    `regex`, `capture`, `backref`, `newline`) for raw strings and heredocs,
    `interpolate` holes parsed by a rule (with `mode`, `when_next`, and
    bracket counting), `embedded` tokens (with `parse` self-injection),
    `escape_tokens`, `body`, `close_at`, `close_not_followed_by`,
    `multiline`; strings that build nodes get generated, labelled kinds.
    `escapes` and `dedent` are validated for lower-lang.
  - **Keywords**: `[lexer.keywords]` `contextual`, `reserved`,
    `default = "contextual"`, `case = "ascii-insensitive"`; contextual
    keywords work as Pratt operators too.
  - `[lexer.numbers]` (radixes, separators and their rule, floats,
    exponents, leading and trailing dots, hex floats, leading zeros,
    suffixes), comment tables (`doc`, `not_followed_by`, `stop_before`,
    `nested`, `modes`), the identifier table (`extra_start`,
    `extra_continue`, `normalize`), `shebang`, `brackets`, and
    `[lexer.columns] tab_width`.
  - `[layout]`: the offside rule (`INDENT`/`DEDENT`), `open_after`,
    `implicit_join`, `explicit_join`, `tabs`, and `[layout.newlines]` modes
    (`trivia`, `significant`, `terminators` with `terminate_after` and
    `continue_before`, `ignored_inside`).
  - The rule language, version 2: **field labels** (`name:element`, on
    tokens, nodes, and groups), predicates `&e` and `!e`, text
    back-references `IDENT=label`, `WORD`, `EOF`, `LINE_START`,
    `NL_BEFORE`, `INDENT`, `DEDENT`; table rules with `sync`, `allow`, and
    `doc`; `prec` on expression-rule levels.
  - `[injections]` (self-injections parsed, others listed as ranges),
    `[hooks]` (validated), and `[ast]` supertypes.
- **Field labels on tree edges** (ISSUES P16): stored in each child's `Kind`,
  so every format-2 tree carries them. `Language::field_label` has the shape
  of lower-lang 0.3's `Labeler::label`; `Language::fields` (with `Field` and
  `Cardinality`) derives each node kind's fields; `label_name` and
  `label_id`. Pratt operator nodes are labelled `lhs`/`op`/`rhs` and
  `op`/`operand`.
- **The overlap check** (ISSUES M02, `LSF4301`): an LL(2) check that refuses
  a greedy repetition or optional committing on a token after which its
  body and its follower diverge, with a witness input. Format 2 only, so
  format 1 is unchanged.
- **A public kind index** (ISSUES M04): `Kind::index`, `Language::kind_count`,
  `kind_at`, and `root_kind`; format-2 kinds are numbered as LSF2 §5.4
  prescribes.
- **Language images** (ISSUES M12): `Language::to_image` and
  `Language::from_image`, `ImageError`, `IMAGE_FORMAT`. Deterministic bytes;
  untrusted input fully validated before use.
- **Multi-file sketches** (ISSUES M13): `Sketch` and
  `Language::from_sketch`, with diagnostics located through a `SourceMap`
  and portable, normalized paths.
- **Diagnostic codes** (ISSUES P19, P04), in diag-lang 1.1's ranges.
- `Language::format`, `display_name`, `description`, `edition`,
  `shebang_names`, `supertype`, `supertypes`, `warnings`, `parse_file`;
  `Parse::injections` and `Injection`.
- Examples: `template` (a format-2 template renderer that walks the tree by
  field name and renders again from the language's image) with
  `examples/schematics/tmpl.lsf`.
- Tests: format-2 lexer and grammar suites, the Mox sketch with
  representative scripts and templates, multi-file sketches, images
  (including property tests over mutated images), format-2 parser
  properties (strict and memo agreement, labels included), and format-2
  regressions; benches for forging Mox, its image, and lexing and parsing
  Mox scripts and templates.

### Fixed

- **ISSUES P09:** a rule remembered by the memo at a shallow depth was
  replayed where parsing it would pass the depth limit, so memoized and
  unmemoized parsing could disagree near the limit. Memo entries now record
  how deep they went and are replayed only within the limit.
- **ISSUES P10:** the left-recursion check walked from every rule (quadratic
  on long rule chains) and suggestions ran an edit distance per pair of
  names. Left recursion is now found with one strongly-connected-components
  pass, and suggestions use a banded edit distance under one budget shared
  by the whole report. A 5000-rule left-recursive cycle and 3000 undefined
  names against 3000 rules each forge (and are refused) in well under a
  second.

### Notes

- Mox's sketch (`_lexersketch/sketches/mox.lsf`) has one more divergence
  than it acknowledges: `elseif_clause`'s optional `else:` (the same case as
  `if_stmt`, which says `allow = ["overlap"]`). The test copy in
  `tests/sketches/mox.lsf` adds the same allowance; the flagship sketch
  needs it too.
- Not in this alpha, and refused with `LSF1007` rather than ignored: sketch
  composition (`extends`, `[compose]`, mixins), `[lexer.split]`,
  `[lexer.columns] trivia`, scanner/layout/predicate hooks, `%mode(...)`,
  rule-scoped `newlines`, `soft_terminators`, `dynamic` operators,
  `rest_of_line = "code"`, injection `when`, and combined self-injections.
  Each is scheduled for alpha.2 in `dev/ROADMAP.md` with its reason.
- Performance against 1.0.1, run back to back: lexing and parsing valid
  format-1 input are within noise; forging is up to about 1 µs (0–10 %)
  slower on the example schematics, a 100,000-operand operator chain 6–12 %,
  and error-heavy input 10–19 % (every diagnostic carries a code). Parser
  events stay 8 bytes despite the four-byte `Kind` (packed), which keeps
  speculation at 1.x speed. The format-2 scanner is about five times slower
  per byte than format 1's derived lexer. Figures in the release notes.

---

## [1.0.1] - 2026-10-08

A hardening patch from the LexerSketch audit. No public API changes; the
`.lsf` format is unchanged. Some schematics 1.0.0 forged are now refused:
each is invalid NOML or hostile, and each is listed below.

### Fixed

- After one construct was nested too deeply, every later speculative choice
  in the file tried only its first alternative, so valid code after it got
  spurious errors. The depth-limit flag is now cleared when an outermost
  speculation begins and ends; inside a speculation it still limits each
  decision to one attempt, which keeps that speculation linear.
- The memo copied the events of every successful attempt at every level of
  nesting, so its memory grew with the input times its nesting depth. It now
  keeps events where the parser produced them and moves them only when a
  rewind would discard them — once, keeping nested replays as references. A
  60-deep nested speculation went from about 253 MiB to 24 MiB peak heap,
  and from about 96 ms to 38 ms.
- A UTF-8 byte-order mark at the start of a source was an `UNKNOWN`
  character with an error. It is now trivia: it begins the first `WHITESPACE`
  token, together with any whitespace after it, so the tree keeps it. A
  leading byte-order mark in a schematic is skipped (spans still count it).
- The schematic reader followed TOML less strictly than documented. Now
  refused, as NOML and TOML require: a `[header]` defining a table already
  defined by dotted keys (`expr.operand = "..."` then `[rules.expr]`, which
  1.0.0 merged and forged); a multi-line string as a key (1.0.0 accepted
  it); and a malformed number, date, or time (`1abc`, `-`, `0123`,
  `1979-13-01`). Well-formed numbers, dates, and times, including `inf`,
  `nan`, and RFC 3339 date-times, are read and reported as the wrong type,
  since no setting takes one.
- A block-comment pair with a non-string item (`["/*", 1, "*/"]`) had the
  item dropped silently and forged. A pair is now exactly two strings.
- Documentation: the schematic layout example in `src/schematic.rs` used a
  string delimiter (`r"`) the lexer rejects (it is now a forgeable schematic,
  checked by a test); `Language::kind_name` claimed to return `"<unknown>"`
  for another language's kind, when it can return a wrong name; the crate
  docs and README said forged trees "plug straight into" the formatter,
  incremental reparser, language server, and tree-sitter crates — they are
  trees those crates are designed to consume, and the adapters arrive with
  LexerSketch; `dev/ROADMAP.md`'s note on `grammar-lang` was out of date.

### Security

- Hostile schematics could drive forging to gigabytes of memory: the token
  sets are bitsets over every token kind, one or more per grammar
  expression. The tables that grow with expressions times token kinds
  (token sets and expression-rule operator tables) are now capped at
  256 MiB, and a grammar that would exceed the cap is refused before they are
  allocated. FIRST sets are shared wherever equal by construction and FOLLOW
  sets are built only where read, so legitimate grammars use far less: the
  30,000-keyword grammar in the regression suite went from about 480 MiB to
  130 MiB peak. A 4 MB schematic of 32,000 keywords and 180,000 choices
  (an estimated 6 GiB in 1.0.0) is refused in 250 ms at 76 MiB peak.
- Schematics larger than 8 MiB are refused (1.0.0 accepted up to 4 GiB).
- Schematic nesting limits multiplied: 64 levels of inline tables, each under
  a 64-part dotted key, nested about 4,096 tables deep and overflowed a
  256 KiB stack. Nesting is now limited to 64 levels in total, counting
  header and dotted-key parts, arrays, and inline tables together.
- Duplicate-key checks in the reader, and the duplicate-capability check,
  scanned every earlier entry, which was quadratic in the size of a table.
  They are now ordered-map lookups.

### Changed

- `#![deny(warnings)]` is removed from the crate root, so new compiler or
  Clippy warnings no longer break downstream builds; CI keeps enforcing
  `-D warnings` through `RUSTFLAGS`.
- New tests: regressions for each fix, a property that a leading byte-order
  mark changes nothing but offsets (source and schematic), a memo agreement
  test across rescued spans, and `tests/memory.rs`, which measures peak heap
  use with a counting allocator. New benchmark:
  `parse/speculative/nested_60`.

---

## [1.0.0] - 2026-10-07

The API freeze. The 0.2.0 surface, the `.lsf` schematic format, and the
parser's guarantees are now the stable `1.x` contract. The only code change
is a lint fix for newer Clippy.

### Added

- `docs/API.md#stability`: the frozen surface, the schematic-format promise
  (every key and rule form keeps its meaning; a schematic that forges keeps
  forging), the trees for valid input, the guarantees (no failure, no panic,
  lossless trees, bounded recursion), `syntax-lang`, `diag-lang`, and
  `pass-lang` 1 as public dependencies, and MSRV 1.85 are recorded as the
  SemVer promise, together with what is not promised.
- A Stability section in the crate documentation.

### Changed

- Version 1.0.0. `README.md` and `docs/API.md` mark the API stable.

### Fixed

- The lexer's byte-class table is built without a one-byte array that
  Clippy on Rust 1.99 rejects (`clippy::byte_char_slices`), which failed the
  stable CI jobs.

---

## [0.2.0] - 2026-10-07

The core: LexerSketch forges a working language front end from a `.lsf`
schematic — a derived lexer, an interpreted parser with error recovery, and
lossless `syntax-lang` trees — and wires the schematic's capabilities into a
`pass-lang` pipeline.

### Added

- `Language`: a forged language. `from_lsf` (and `FromStr`) reads a schematic,
  checks it, and compiles it; `name`, `version`, `extensions`, and
  `capabilities` expose its identity; `kind` and `kind_name` map between kinds
  and their names; `lex` returns the lossless token stream; `parse` returns a
  `Parse`; `pipeline` assembles the capability passes the schematic includes,
  in schematic order, from a registry. `Send`, `Sync`, `Clone`.
- `Parse`: the result of parsing — `tree`, `source`, `language`,
  `diagnostics`, `has_errors`, `report`, `into_tree`, and `dump`.
- `Kind`: a two-byte `Copy` kind shared by tokens and nodes, implementing
  `syntax_lang::TokenKind` (trivia is known without the language).
- `Capability`: a boxed `pass_lang::Pass` over a `Parse`.
- `Error`: every problem with a schematic or a capability registry, as
  `diag_lang::Diagnostic`s with spans into the schematic; `Display` locates the
  first by line and column.
- The `.lsf` schematic format: `[language]` (name, version, extensions, start
  rule), `[lexer]` (identifier style, significant newlines, line and nested
  block comments, string forms), `[rules]` (a rule language of literals, token
  classes, rule references, sequences, ordered choice, `*`, `+`, `?`, and
  hidden `_` rules), expression rules (`[rules.name]` with an operand and
  operator levels — left, right, non-associative, prefix, postfix — with
  `then` tails and node names), and `[capabilities]`. Schematics are read by a
  zero-copy reader for the static, TOML-compatible core of NOML; NOML's dynamic
  features are refused.
- A derived lexer: keywords and symbols collected from the grammar, longest
  match, Unicode (UAX #31) or ASCII identifiers, number literals with radixes,
  separators, fractions, and exponents, and diagnostics for unexpected
  characters, unterminated strings and comments, and malformed numbers.
- An interpreted parser with ordered-choice semantics: FIRST-set pruning,
  speculation only where alternatives share a first token, left-factoring at
  forge time, Pratt parsing for expression rules, and an event list that makes
  rewinding a truncation. Speculative attempts are memoized by rule and
  position, so shared prefixes are parsed once and nested input stays linear.
  Recovery assumes missing tokens, wraps unwanted ones in `ERROR` nodes, stops
  skipping at any token an enclosing construct is waiting for, never re-enters
  a rule where it is already active without consuming input, and reports at
  most one syntax error per token. Every recursive step counts against a limit
  of 768 grammar levels (at most about 256 KiB of stack in release builds), so
  no input can exhaust the stack. With `newlines = true`, the end of the input
  counts as a line break wherever a `NEWLINE` may come.
- Forge-time checks with suggestions and fixes: undefined rules, literals the
  lexer cannot produce, delimiters used twice, left recursion (named as a
  chain), repetitions of nothing, unreachable alternatives, and more; all
  problems are reported together.
- Example schematics `calc.lsf`, `json.lsf`, `mini.lsf`, and `conf.lsf`, and
  examples `calc`, `check`, `json`, `capabilities`, and `highlight`.
- Unit, integration, property, and regression tests, including a strict
  reference parser the recovering parser must agree with and a check that
  memoization never changes a result; Criterion benchmarks for forging, lexing,
  and parsing (including a grammar that speculates on every statement);
  `README.md` and `docs/API.md` examples run as doctests.
- Forging scales: the analysis visits rules in dependency order, shares the
  FIRST sets of token expressions, and builds FOLLOW and recovery sets only
  where they are read. Dotted keys and table headers in a schematic are
  limited to 64 parts.

### Changed

- `syntax-lang` 1, `diag-lang` 1, `pass-lang` 1, and `unicode-lang` 1 are wired;
  `syntax_lang`, `diag_lang`, and `pass_lang` are re-exported. The `std`
  feature forwards to them.
- The crate description now says what the crate does.

### Removed

- The scaffold's unused `serde` feature and `loom` dev-dependency.

### Fixed

- `Cargo.toml` had unquoted `keywords` and `categories`, so the manifest did not
  parse.
- `clippy.toml` declared MSRV 1.87 against the crate's 1.85.
- `deny.toml` named another project; `docs/API.md` and `dev/ROADMAP.md` carried
  byte-order marks; the README linked a `dev/DIRECTIVES.md` that does not
  exist.
- The crate root now carries the full REPS lint set.

---

## [0.1.0] - 2026-06-18

Initial scaffold and repository bootstrap. No domain logic yet &mdash; this release establishes the structure, tooling, and quality gates the implementation will be built on.

### Added

- `Cargo.toml` with crate metadata, Rust 2024 edition, MSRV 1.85.
- Dual `Apache-2.0 OR MIT` license files.
- `README.md`, `CHANGELOG.md`, and a documentation skeleton.
- `REPS.md` compliance baseline.
- `.github/workflows/ci.yml` CI matrix; `deny.toml`, `clippy.toml`, `rustfmt.toml`.
- `dev/DIRECTIVES.md` and `dev/ROADMAP.md` (committed engineering standards + plan).

[Unreleased]: https://github.com/jamesgober/lang-forge/compare/v2.0.0-alpha.1...HEAD
[2.0.0-alpha.1]: https://github.com/jamesgober/lang-forge/compare/v1.0.1...v2.0.0-alpha.1
[1.0.1]: https://github.com/jamesgober/lang-forge/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/jamesgober/lang-forge/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/jamesgober/lang-forge/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/lang-forge/releases/tag/v0.1.0
