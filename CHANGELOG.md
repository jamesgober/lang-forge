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

[Unreleased]: https://github.com/jamesgober/lang-forge/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/jamesgober/lang-forge/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/jamesgober/lang-forge/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/lang-forge/releases/tag/v0.1.0
