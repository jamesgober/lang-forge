//! # lang_forge
//!
//! LexerSketch: forge a working language front end — lexer, parser, and
//! lossless syntax tree — from a `.lsf` schematic.
//!
//! A schematic is a short NOML document that describes a language: its name,
//! how its tokens look, its grammar, and the capabilities (passes) it
//! includes. [`Language::from_lsf`] reads it, checks it, and compiles it into
//! tables; [`Language::parse`] then turns source text into a
//! [`syntax_lang::Node`] tree with [`diag_lang::Diagnostic`]s for anything
//! malformed. There is no code generation step and nothing to build: the
//! language is ready the moment the schematic is forged.
//!
//! lang-forge is the capstone of the `-lang` language-construction family. Its
//! trees are `syntax-lang` trees, the family's lossless CST, which the
//! formatter, incremental reparser, language server, and tree-sitter crates
//! are designed to consume; the adapters that connect a forged language to
//! those crates are not part of lang-forge and arrive with LexerSketch. Its
//! diagnostics render with `diag-lang`, and its capabilities run on
//! `pass-lang`.
//!
//! ## A first language
//!
//! ```
//! use lang_forge::Language;
//!
//! let calc = Language::from_lsf(
//!     r##"
//!     [language]
//!     name = "calc"
//!
//!     [lexer]
//!     line_comments = ["#"]
//!
//!     [rules]
//!     program = "stmt*"
//!     stmt    = "'let' IDENT '=' expr ';' | expr ';'"
//!     group   = "'(' expr ')'"
//!
//!     [rules.expr]
//!     operand = "NUMBER | IDENT | group"
//!     levels  = [
//!         { left   = ["+", "-"] },
//!         { left   = ["*", "/"] },
//!         { prefix = ["-"] },
//!         { right  = ["^"] },
//!     ]
//!     "##,
//! )?;
//!
//! let parse = calc.parse("let area = 3 * r ^ 2; # circle-ish\n");
//! assert!(!parse.has_errors());
//!
//! // `^` binds tighter than `*`, so the product's right operand is `r ^ 2`.
//! let binary = calc.kind("binary").expect("the default operator node");
//! let product = parse.tree().descendants().find(|n| *n.kind() == binary).expect("3 * r ^ 2");
//! assert_eq!(product.text(parse.source()), Some("3 * r ^ 2"));
//! assert_eq!(product.child_nodes().last().and_then(|n| n.text(parse.source())), Some("r ^ 2"));
//! # Ok::<(), lang_forge::Error>(())
//! ```
//!
//! ## The rule language
//!
//! Each entry of `[rules]` is a rule. A string rule is a sequence of elements:
//!
//! | Element | Matches |
//! |---|---|
//! | `'text'` or `"text"` | a keyword (if it looks like an identifier) or a symbol |
//! | `IDENT`, `NUMBER`, `STRING`, `NEWLINE` | a token of that built-in class |
//! | `name` | the rule `name`, as a child node |
//! | `a b` | `a` then `b` |
//! | `a \| b` | `a`, or else `b` — the first that matches wins |
//! | `a*`, `a+`, `a?` | zero or more, one or more, zero or one |
//! | `( ... )` | grouping |
//!
//! A rule builds a node named after itself, unless its name starts with `_`,
//! in which case its children are placed directly in the parent. The first
//! rule is the start rule unless `[language] start` names another; its node
//! is the root of every tree.
//!
//! A table rule, `[rules.name]`, is an *expression rule*: an `operand` and
//! operator `levels`, lowest precedence first. Each level is `left`,
//! `right`, or `none` (binary, by associativity), `prefix`, or `postfix`,
//! with an optional `then` (more grammar after the operator, for calls,
//! indexing, or `?:`) and an optional `node` name.
//!
//! ## Errors and recovery
//!
//! Forging reports every problem in a schematic at once, each with a span
//! into the schematic ([`Error`]). Parsing never fails: a missing token is
//! reported and assumed, an unexpected token is reported and wrapped in an
//! `ERROR` node, and the tree always covers the whole source.
//!
//! ## Features
//!
//! - `std` (default) — the standard library. Without it the crate is
//!   `no_std` and needs only `alloc`.
//!
//! ## Re-exports
//!
//! [`syntax_lang`], [`diag_lang`], and [`pass_lang`] are re-exported whole, so
//! code that walks trees, renders diagnostics, or writes capability passes
//! names the same versions lang-forge was built against.
//!
//! ## Stability
//!
//! The public surface is frozen as of `1.0.0` and follows Semantic Versioning:
//! no breaking change before `2.0`, additions arrive in minor releases, and the
//! MSRV (Rust 1.85) only rises in a minor. The promise covers the API, the
//! `.lsf` schematic format, the trees forged languages build from valid input,
//! and the parser's guarantees; it is set out in full in
//! [`docs/API.md`](https://github.com/jamesgober/lang-forge/blob/main/docs/API.md#stability).

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unused_must_use)]
#![deny(unused_results)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::unreachable)]
#![deny(clippy::undocumented_unsafe_blocks)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod error;
mod grammar;
mod kind;
mod language;
mod lexer;
mod noml;
mod parse;
mod parser;
mod rule;
mod schematic;
mod set;
mod tree;

pub use error::Error;
pub use kind::Kind;
pub use language::{Capability, Language};
pub use parse::Parse;

// Re-exported whole: trees are `syntax_lang` trees, problems are `diag_lang`
// diagnostics, and capabilities are `pass_lang` passes.
pub use diag_lang;
pub use pass_lang;
pub use syntax_lang;

/// Compiles and runs the `rust` code blocks in `README.md` and `docs/API.md` as
/// part of `cargo test`, so the published examples cannot drift from the API.
///
/// Present only while collecting doctests (`#[cfg(doctest)]`); it is not part of
/// the public surface and does not appear in the built library or its docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
#[doc = include_str!("../docs/API.md")]
pub struct MarkdownDocTests;
