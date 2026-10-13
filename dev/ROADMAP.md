# lang-forge - Roadmap

> Path from scaffold to a stable 1.0, then 2.0 (LSF2). Hard parts are front-loaded; each phase has hard exit criteria.
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

## v2.0.0-alpha.1 - LSF2 syntax forge (DONE, 2026-10-09)
LexerSketch ROADMAP work package 2.1: format-2 sketches (`_lexersketch/specs/LSF2.md` syntax sections §3, §7–§14), the front end for the Mox demo. A major release: format 1 must forge exactly as in 1.x.

Exit criteria:
- [x] Format-1 schematics forge and parse exactly as in 1.x; every 1.x test kept, unchanged, as a regression suite (one assertion in `tests/forge.rs` adjusted for the rendered code in the header, the documented breaking change).
- [x] Mox's syntax sections forge, and representative Mox scripts and templates (`<?mox … ?>` islands, `$vars`, interpolation, heredoc/nowdoc, contextual `async`/`await`/`spawn`) parse with no diagnostic (`tests/mox.rs`).
- [x] Every format-2 feature tested, with its diagnostics and codes; properties (lossless, recovering vs strict, memo on/off) extended to format 2 with labels compared.
- [x] Benches: the 1.x groups unchanged (compared against the 1.0.1 baseline), a `v2` group for Mox.
- [x] Every LSF2 syntax key not done is listed below with the reason; nothing is silently ignored (each is refused with `LSF1007`).

Delivered:
- [x] Format detection (`[sketch] format`), the format-1 path untouched, and a format-2 reader (`src/spec2.rs`) on the crate's own NOML reader, extended (DECISIONS D6): integer values, multi-line text flags, table merging and rebasing for multi-file sketches.
- [x] Custom token classes with a regex engine reimplemented in the crate (`src/regex/`): character-class alphabets, `\p{XID_Start}`/`\p{XID_Continue}`, bounded repetition, subset construction with an automaton budget, one DFA per class, a per-forge compile cache. Conditions (`followed_by`, `when_prev`, `line_start`, `column`), `priority`, `trivia`, and mode actions.
- [x] Lexer modes with a mode stack and a run-time depth budget; `text` fallback classes; `inherit`; `eof`.
- [x] String classes: delimiter parts (`capture`/`backref` for raw strings and heredocs, `regex`, `newline`), interpolation holes parsed by a rule (bracket counting, `when_next`, hole modes), embedded tokens (with `parse` self-injection), escape tokens, `body`, `close_at`, `close_not_followed_by`; generated, labelled kinds for strings that build nodes; zero-width closes on unterminated strings (trees stay well formed).
- [x] Contextual keywords (own kinds in FIRST sets, matched by text), `reserved`, `default = "contextual"`, case-insensitive keywords, contextual Pratt operators.
- [x] Numbers, comment tables, identifier tables (`normalize = "require-nfc"` via unicode-lang), shebang, brackets, `tab_width`.
- [x] `[layout]`: offside rule, joins, tabs, newline modes.
- [x] Rule language v2: labels (P16), `&`/`!` predicates, text back-references, `WORD`, `EOF`, `LINE_START`, `NL_BEFORE`; table rules with `sync`/`allow`/`doc`; `prec`.
- [x] Field labels on tree edges (in `Kind`), `Language::field_label` (lower-lang 0.3 `Labeler` shape), `fields`/`Field`/`Cardinality`, Pratt operator labels.
- [x] The overlap check (M02, LL(2), witness inputs, `allow`, `[sketch.checks]`), unused rule/token warnings.
- [x] Public kind index (M04), LSF2 §5.4 numbering for format 2.
- [x] `.lsl` images (M12): deterministic, versioned, fully validated on load.
- [x] Multi-file sketches with a `SourceMap` (M13): `Sketch`, `Language::from_sketch`, portable paths.
- [x] Diagnostic codes (P19, P04) for every diagnostic, both formats, in diag-lang 1.1's ranges.
- [x] Injections (self-injections parsed, others as ranges), `[hooks]` validated, `[ast]` supertypes.
- [x] P09 (memo replay past the depth limit) and P10 (SCC left-recursion check, budgeted banded edit distance for suggestions) fixed, with regression tests.

Dependency wiring:
- **`diag-lang` 1.1** (was 1): diagnostic codes.
- **`unicode-lang` 1, now with its `alloc` feature**: NFC checks for `normalize = "require-nfc"`; XID tables for `\p{XID_*}` classes.
- `syntax-lang` 1 and `pass-lang` 1 unchanged. No new dependencies; dev-dependencies still `criterion` and `proptest` only.
- **`grammar-lang` still not wired.** Its regex/DFA construction was the reference for `src/regex/`, but it is not a dependency: its lexer DFA runs over bytes for its own token model (one automaton, longest match, its own priorities), while lang-forge needs one DFA per class over character-class alphabets, run-time conditions and modes, its own budgets and error codes, and a stable binary encoding for images. Depending on it would expose none of that and add a second token model to reconcile; reimplementing it (about 2,200 lines) kept the engine under lang-forge's budgets and image format.

Deferred to **v2.0.0-alpha.2** (each refused with `LSF1007` in alpha.1, never ignored), with the reason. *(Moved to v2.0.0-alpha.3: alpha.2 became the parser-limits security fix; see below.)*
- **Sketch composition** — `[language] extends`, `[compose]`, `[sketch] requires`, `kind = "mixin"`, sketch references (§3.5, §4): needs layer merge and conflict semantics (`LSF2010`–`LSF2021`) on top of multi-file sketches; Mox does not use it.
- **`[lexer.split]`** (token splitting, `>>` into `>` `>` for generics): needs parser-driven re-lexing of a token; no alpha.1 consumer.
- **`[lexer.columns] trivia`** (Fortran/COBOL column ranges): needs a column-trivia pass in the scanner; `tab_width` is done.
- **Hooks that run** — scanner (`[lexer.tokens] hook`), reclassify, predicate (`@hook` in rules), and layout (`style = "hook"`) hooks: they need the capability runtime to call into user code from the lexer and parser; `[hooks]` entries are validated now.
- **`%mode(...)`** in rules (re-lexing a range in another mode): needs the parser to drive the scanner; modes switched by the lexer's own actions cover Mox.
- **Rule-scoped `newlines`** and expression-rule `newlines`: needs layout state threaded through the parser.
- **`soft_terminators`** (zero-width `;` for JavaScript ASI): a parser extension.
- **`dynamic` operators** (user-declared fixity): needs a fixity pre-scan (a `fixity` hook) and run-time operator tables.
- **`rest_of_line = "code"`** (shell/Ruby heredocs whose opener line continues with code): needs a pending-heredoc queue in the scanner.
- **Injection `when`** (inject only when the content matches) and **combined self-injections**: `when` needs a regex test on injected content; a combined self-injection needs one parse over discontiguous ranges. Combined editor injections are listed range by range now.
- **Checks not run:** `LSF3113` (DFA ambiguity between token classes), `LSF3125` shadowing analysis beyond format 1's literal check, `LSF4303`; and `confusables` (UAX #39 data is not in unicode-lang yet, so `warn`/`deny` give an `LSF1007` warning).
- **Limits:** kinds are capped at 32,768 (15 index bits, as in 1.x) where LSF2 §1.8 allows 65,535; image warnings are not stored (a loaded language has no `warnings()`).
- **Spec follow-ups for `_lexersketch`:** codes used that LSF2 §27 does not list yet (`LSF1007` not supported, `LSF4112` rule syntax, `LSF4305`–`LSF4310` format-1 grammar checks, `LSF7004`/`LSF7005` pipeline); Mox's `elseif_clause` needs `allow = ["overlap"]` like `if_stmt`.

## v2.0.0-alpha.2 - Parser limits: linear time, a step budget (DONE, 2026-10-13)
A security fix release, ahead of the LSF2 work alpha.1 scheduled here: past the depth limit, parsing time grew exponentially with each level of nesting (Mox: 56 nested array literals in the test copy of the sketch, 60 in the flagship, did not finish in 30 s; 65 `list(` calls took 77 s; LexerSketch hung). Scope set by the coordinator: make reaching the depth limit linear-time, audit every path the memo cannot cover, add an overall work budget (also serving `_lexersketch/ISSUES.md` P46, the Studio's forge timeout, on the parse side).

Confirmed cause: `too_deep` failed the strict attempt that reached the limit and tainted it, so no result around it was memoized, and every retry point above it (each alternative, the item after a failed predicate, the operand after a failed lookahead such as Mox's `&(expr '=>')`) went down the same path again; instrumented, one level past the trip went from 16 to 327,600 limit hits and from 31 k to 7.8 M steps.

Exit criteria:
- [x] Reaching the depth limit is linear-time, with one located `LF1003` diagnostic and a lossless tree; depth-limited results never enter the memo.
- [x] Every path the memo cannot cover audited; none multiplies work with the input.
- [x] A step budget proportional to the input, configurable; hostile grammars and inputs end with `LF9002`.
- [x] Mox nested arrays and `list(` calls at 50, 60, 100, 10,000 levels held to a step count; a property holds random nesting to a linear step bound.
- [x] The existing suite passes unchanged (no test pinned the old soft-failure behaviour; `test_depth_limit_does_not_cripple_later_speculation` still passes as written, though the tail it checks is now unparsed rather than parsed).
- [x] Benches: no regression on normal input against alpha.1.

Delivered:
- [x] **Limits stop the parse.** The depth limit and the budget are a hard stop of the whole parse, strict or not (`Parser::stop`): every retry point (alternatives, optionals, repetitions, predicates, recovering sequences) checks the stop and unwinds without retrying; the attempt in progress is rewound, the tokens not consumed go into one `ERROR` node, and every open node is closed (`close_stopped`). The `hit_limit` flag (M01's "one attempt per decision" workaround) and the depth taint are gone.
- [x] **Progress-guard audit.** The guard cut a cycle and tainted every enclosing strict result (a global counter), which made speculation over unclosed blocks before the end of input exponential in nesting (2.6 s at 22 levels in a `stmts = "NEWLINE stmts | ..."` grammar). Results now record the depth and position of the invocation whose cycle was cut: a result is remembered unless the cycle ran through an invocation outside it, and a result whose cycle was at its own position is marked and replayed only where no rule is active at that position (`chain_start`). Both rules are load-bearing: a property test fails without the second.
- [x] **P09 follow-up.** A replay now counts the depth its attempt went (`max_depth`), so an attempt that holds replays is never replayed where parsing it again would reach the limit (a unit test fails without it).
- [x] **The budget** (`ParseBudget`, `Language::with_parse_budget`/`parse_budget`, `Parse::steps`, `LF9002`). Work is counted in ticks, eight to a step: a grammar level entered, a token matched, an operator is a step; an alternative examined, a memo entry looked at, an event copied is a tick. Counting tokens closed a hole the audit found: a strict attempt through `IDENT*` matched tokens on the fast path without a step, so quadratic speculation (`IDENT* ';' | IDENT` over 100,000 identifiers, 18 s in alpha.1) ran under any budget.
- [x] **Grammar-proportional loops bounded or counted.** Contextual keyword operators are looked up from the owning rule instead of a scan of every rule; a choice's viability test, `try_alternatives`, memo chain walks, the predicate guard's walk, and replays charge ticks; error messages are built only when reported, and an expected set is described from at most seven members (a choice's own FIRST set, which a unit test checks is the union of its alternatives').
- [x] Tests: `tests/limits.rs` (10), and parser unit tests and properties for cycle memoization, cyclic replay, replayed depth, stops agreeing memoized and unmemoized, the committed prefix after a stop, and choice FIRST sets.
- [x] Benches `v2/parse/mox_arrays/50x200` and `v2/parse/mox_arrays/10000`.

Dependency wiring: unchanged — `syntax-lang` 1, `diag-lang` 1.1, `pass-lang` 1, `unicode-lang` 1; no new dependencies. `ParseBudget` is a plain value type; nothing is serialized, and the image format is unchanged.

Not in this release (recorded under the anti-deferral rule):
- **The LSF2 items alpha.1 scheduled for alpha.2** (listed above) move to **v2.0.0-alpha.3**: this release was redefined as the security fix alone, so it ships without waiting on them. Nothing about them changed; each is still refused with `LSF1007`.
- **The memo still lasts one outermost speculation.** Input whose every token starts a speculation that scans far ahead is quadratic (and stopped by the budget); a recovering predicate at every level of nesting rescans the levels below (bounded by the depth limit: at most about 2,000 steps a token in Mox). A memo kept for the whole parse would make both linear at the cost of memory proportional to the input times the rules tried; that is a design decision for 2.0, not a fix.
- **The progress guard can still be exponential in grammar size** at one position (a cycle at the end of the input through `k` rules that all speculate); no result inside such a cycle depends only on its rule and position. The budget bounds it; `tests/limits.rs` holds a 40-rule instance to `LF9002`.
- **The depth limit counts frames, not nesting.** Mox spends about 14 grammar levels per array literal, so 768 levels hold 53 of them; a language that needs more needs a cheaper grammar or a larger limit (the limit is what bounds the stack).

## v2.0.0-alpha.3 - LSF2 completion (planned)
- [ ] The items alpha.1 deferred (listed under alpha.1 above), moved here from alpha.2.

## v2.0.0 - API freeze (planned)
- [ ] The alpha.3 items above.
- [ ] The format-2 surface reviewed and frozen; `docs/API.md` stability section updated to the 2.0 promise.

## Later candidates (additive)
None of these changes a frozen surface; each is a new method, key, or type. (Named token classes, custom token patterns, and contextual keywords, listed here for 1.x, arrived with format 2 in 2.0.0-alpha.1.)
- Emitting a tree-sitter grammar for a forged language through `treesitter-lang`, so one schematic also yields editor highlighting.
- A reusable parse session that keeps its scratch buffers between parses.
- Emitting Rust source for a forged language, for projects that want a compiled-in parser.
