# lang-forge - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
> Master plan: ../../_strategy/LANG_COLLECTION.md
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

## v0.1.0 - Scaffold (DONE)
Compiles, CI green, structure correct, no domain logic.
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt.
- [x] Scaffold defects fixed at the start of v0.2.0: unquoted `keywords`/`categories` (the manifest did not parse), clippy MSRV 1.87 vs 1.85, `deny.toml` naming another project, byte-order marks in two docs, a README link to a missing `dev/DIRECTIVES.md`, and the REPS lint set missing from the crate root.

## v0.2.0 - Core (DONE)
The capstone generator (LexerSketch): consumes a .lsf schematic and emits a language.
Dependencies (wires grammar + emitted pipeline) are wired here, when first used.
Exit criteria:
- [x] Every public item has rustdoc + a runnable example.
- [x] Core invariants property-tested (full DIRECTIVES + API authored at this stage).

Delivered:
- [x] The `.lsf` schematic: `[language]`, `[lexer]`, `[rules]` (rule language + expression rules), `[capabilities]`; read by a zero-copy reader for the static core of NOML with spans on every key and value.
- [x] A derived lexer (keywords and symbols from the grammar, longest match, XID/ASCII identifiers, numbers, strings, comments, significant newlines).
- [x] An interpreted parser: ordered choice pruned by FIRST sets, speculation only on genuine one-token ambiguity with memoized attempts (linear time), forge-time left-factoring, Pratt expression rules, event-list tree building, structured error recovery with a no-progress guard, and a 768-level recursion bound (about 256 KiB of stack in release builds).
- [x] Forge-time analysis: FIRST/nullable/FOLLOW fixpoints, left recursion, repetitions of nothing, unreachable alternatives, literal and delimiter conflicts, all problems reported at once.
- [x] Capabilities: `Language::pipeline` assembles the schematic's passes from a registry into a `pass-lang` `PassManager`.
- [x] Properties: lossless trees for any input; recovering parse equals a strict reference parse whenever no error is reported; memoization never changes a result; generated programs parse cleanly; forging never panics on arbitrary or mutated schematics.
- [x] Adversarial review: seven defects found (stack exhaustion, exponential and quadratic speculation, a forging overflow, recursion without progress, end-of-input `NEWLINE`, a split-character span) and fixed, each kept in `tests/regressions.rs`; forge-time scaling on extreme schematics fixed.

Dependency decisions (recorded under the anti-deferral rule):
- **Wired:** `syntax-lang` 1 (the output tree), `diag-lang` 1 (all problems), `pass-lang` 1 (capabilities), `unicode-lang` 1 (XID identifiers).
- **`grammar-lang` not wired.** It is still a 0.1.0 scaffold with no API and is not published, so the grammar engine (rule language, analysis, interpreter) lives in lang-forge as private modules. Nothing about it is public, so lang-forge can adopt `grammar-lang` internally once that crate has a stable core, with no change to lang-forge's API.
- **`lexer-lang` not wired.** Its `Cursor` steps a `char` at a time; the derived lexer is table-driven over bytes with ASCII fast paths and only decodes UTF-8 on non-ASCII input. Routing the hot loop through `Cursor` would slow it with nothing gained.
- **`parser-lang` not wired.** Its `Parser` skips trivia and keeps errors on the side; lang-forge builds lossless trees from an event list that supports speculation and forward-linked operator nodes. The same reasoning kept it out of `incremental-lang`.
- **`ast-lang` not wired.** A typed AST needs Rust types per language; a language forged at run time has none. The lossless CST is the output, and typed views are the consumer's to build.
- **SEMA/CODE/LXRT crates** are reached through capabilities, not wired directly: a schematic names the passes it includes, and those passes depend on whatever they need.
- **The `noml` crate is not used** to read schematics: it pulls `tempfile`, `serde`, `indexmap`, and `thiserror` 1 in as runtime dependencies, requires `std`, targets edition 2021, and is being rebuilt. A schematic needs only NOML's static core, read with spans for diagnostics.
- The scaffold's `serde` feature and `loom` dev-dependency were removed: nothing serializes, and a `Language` is immutable (`Send + Sync`, no interior mutability).

## v1.0.0 - API freeze (DONE)
Public surface stable and frozen until 2.0.
- [x] docs/API.md marked stable; SemVer promise recorded (surface, schematic format, trees for valid input, guarantees, public dependencies, MSRV, and what is not promised).
- [x] Full test + benchmark suite green: Windows and Linux (WSL2) locally, stable and 1.85; macOS through the CI matrix.

## Later 1.x candidates (additive)
None of these changes the frozen surface; each is a new method, key, or type.
- Emitting a tree-sitter grammar for a forged language through `treesitter-lang`, so one schematic also yields editor highlighting.
- Named token classes beyond the built-ins (several string or number forms with their own kinds, such as `CHAR`), and custom token patterns.
- Contextual keywords (a literal that is a keyword only where the grammar expects it).
- A reusable parse session that keeps its scratch buffers between parses.
- Emitting Rust source for a forged language, for projects that want a compiled-in parser.
