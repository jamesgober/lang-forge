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
- **`grammar-lang` not wired.** When this core was built, grammar-lang was a 0.1.0 scaffold with no API, so the grammar engine (rule language, analysis, interpreter) lives in lang-forge as private modules. *(Updated in 1.0.1:)* grammar-lang has since reached 1.0 as a runtime LALR(1) parser generator with a DFA lexer and Bison-compatible conflict semantics. That is a different engine from lang-forge's ordered choice with speculation, recovery, and Pratt levels, so adopting it is a design decision (which engine LexerSketch forges with, or both), not a drop-in swap; it is tracked in LexerSketch's plan (`_lexersketch/DECISIONS.md`), not here. Nothing about the engine is public, so either choice leaves lang-forge's API unchanged.
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

## v1.0.1 - Hardening patch (DONE)
Fixes from the LexerSketch audit (`_lexersketch/ISSUES.md` M01, M03, M06, M07, M08, M09, M10, M11, the strictness part of M14, and F03). No public API change; the `.lsf` format is unchanged, and only schematics that are invalid NOML or hostile are newly refused.

Delivered:
- [x] **M01** The depth-limit flag that limits speculation to one alternative per decision is cleared when an outermost speculation begins and ends, so a construct nested too deeply no longer cripples every later choice in the file; the exponential-time protection inside that speculation is kept.
- [x] **M03** Hostile-schematic memory: schematics are capped at 8 MiB; the tables that grow with expressions times token kinds (token sets, operator tables) are capped at 256 MiB and checked before allocation; FIRST sets are shared wherever equal by construction and FOLLOW sets are built only where read (the 30,000-keyword grammar went from about 480 MiB to 130 MiB); duplicate-key and duplicate-capability checks use ordered maps instead of scans.
- [x] **M06** The crate docs, README, API reference, and manifest no longer say forged trees "plug straight into" the formatter, reparser, LSP, and tree-sitter crates: they are trees those crates are designed to consume, and the adapters arrive with LexerSketch.
- [x] **M07** A leading UTF-8 byte-order mark is skipped in schematics and is trivia in source text (it begins the first `WHITESPACE` token, so trees stay lossless).
- [x] **M08** The memo keeps remembered events where the parser produced them and moves them only when a rewind would discard them — once, with nested replays kept as references — so its memory no longer grows with nesting depth (60-deep nested speculation: about 253 MiB to 24 MiB peak, and 2.5 times faster).
- [x] **M09** Schematic nesting is bounded in total (header and dotted-key parts, arrays, and inline tables together, 64 levels), not per kind; the bound also keeps dropping the document shallow, so no iterative drop is needed.
- [x] **M10** The schematic layout example in `src/schematic.rs` is a forgeable schematic, checked by a unit test; `Language::kind_name` documents (and doctests) what it returns for another language's kind.
- [x] **M11** The `grammar-lang` note above is brought up to date.
- [x] **M14** (strictness) A header redefining a table defined by dotted keys, multi-line string keys, malformed numbers, dates, and times, and non-string items in a block-comment pair are refused.
- [x] **F03** `#![deny(warnings)]` removed from the crate root; CI enforces `-D warnings` through `RUSTFLAGS`.

Dependency wiring: unchanged — `syntax-lang` 1, `diag-lang` 1, `pass-lang` 1, `unicode-lang` 1; no new dependencies (the counting allocator in `tests/memory.rs` is test code over `std`).

Not in this patch (recorded under the anti-deferral rule): M14's `\x` escape (an addition, not a strictness fix) and the move to a shared NOML reader (D6); M02, M04, M05, M12, M13 (minor or major by their SemVer class).

## Later 1.x candidates (additive)
None of these changes the frozen surface; each is a new method, key, or type.
- Emitting a tree-sitter grammar for a forged language through `treesitter-lang`, so one schematic also yields editor highlighting.
- Named token classes beyond the built-ins (several string or number forms with their own kinds, such as `CHAR`), and custom token patterns.
- Contextual keywords (a literal that is a keyword only where the grammar expects it).
- A reusable parse session that keeps its scratch buffers between parses.
- Emitting Rust source for a forged language, for projects that want a compiled-in parser.
