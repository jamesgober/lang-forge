//! Injections (LSF2 §12, and §9.5.4 `embedded` tokens with `parse`): ranges
//! of a parsed source that another parse covers.
//!
//! A self-injection — `language = "self"` resolved by the sketch, or an
//! embedded string token with `parse` — is parsed here with the same
//! language, starting at its rule, and its tree is attached to the
//! [`Parse`](crate::Parse) by span; it never changes the host tree's tokens.
//! An injection of another language, or one resolved by the editor, is listed
//! with its range only, for the caller (or the editor) to handle. Injected
//! trees are searched for further injections, breadth first, to a depth of
//! 16; deeper ones are left unparsed with a warning.

use alloc::vec::Vec;

use diag_lang::{Diagnostic, Label, Severity};
use syntax_lang::{Element, Node, Span, TokenKind};

use crate::{
    Language, Parse, codes,
    grammar::{InjectionDef, Lex},
    kind::Kind,
    parse::Injection,
    parser,
    scan::{MAIN, PartRt, StringKinds},
};

/// The deepest chain of injections parsed (LSF2 §1.8, §12).
const MAX_DEPTH: u32 = 16;

/// Builds the [`Parse`], parsing the self-injections the tree contains.
pub(crate) fn finish<'a>(
    language: &'a Language,
    source: &'a str,
    tree: Node<Kind>,
    mut diagnostics: Vec<Diagnostic>,
) -> Parse<'a> {
    let grammar = language.tables();
    let Some(extra) = grammar.extra.as_deref() else {
        return Parse::new(language, source, tree, diagnostics, Vec::new());
    };
    if extra.injections.is_empty() && extra.embedded_parse.is_empty() {
        return Parse::new(language, source, tree, diagnostics, Vec::new());
    }
    let Lex::V2(scanner) = &grammar.lexer else {
        return Parse::new(language, source, tree, diagnostics, Vec::new());
    };
    let mut injections: Vec<Injection<'a>> = Vec::new();
    // Targets found in the host tree, then in each injected tree in turn.
    let mut found: Vec<Target<'a>> = Vec::new();
    collect(&tree, extra, &grammar.kinds, scanner, source, 1, &mut found);
    let mut next = 0;
    while next < found.len() {
        let target = found[next].clone();
        next += 1;
        let parsed = match target.rule {
            Some(rule) if target.depth > MAX_DEPTH => {
                let _ = rule;
                diagnostics.push(
                    Diagnostic::new(
                        Severity::Warning,
                        "this injection is nested more than 16 levels deep and was not parsed",
                        Label::unlabelled(target.span),
                    )
                    .with_code(codes::PARSE_TOO_DEEP),
                );
                None
            }
            Some(rule) => {
                let (start, end) = (target.span.start().to_usize(), target.span.end().to_usize());
                let mut tokens = Vec::new();
                let mut diags = Vec::new();
                scanner.run_range(source, start, end, MAIN, &mut tokens, &mut diags);
                let (tree, diags) = parser::parse_tokens(grammar, source, &tokens, rule, diags);
                diagnostics.extend(diags);
                collect(
                    &tree,
                    extra,
                    &grammar.kinds,
                    scanner,
                    source,
                    target.depth + 1,
                    &mut found,
                );
                Some(tree)
            }
            None => None,
        };
        injections.push(Injection::new(
            target.id,
            target.language,
            target.span,
            parsed,
            target.editor,
        ));
    }
    diagnostics.sort_by_key(|d| d.primary().span().start().to_u32());
    injections.sort_by_key(|i| (i.span().start().to_u32(), i.span().end().to_u32()));
    Parse::new(language, source, tree, diagnostics, injections)
}

/// One range to inject.
#[derive(Clone)]
struct Target<'a> {
    id: &'a str,
    language: &'a str,
    span: Span,
    /// The rule to parse it with (a self-injection), or `None`.
    rule: Option<u32>,
    editor: bool,
    depth: u32,
}

/// Collects the targets in `tree` (iteratively; trees can be deep).
fn collect<'a>(
    tree: &Node<Kind>,
    extra: &'a crate::grammar::Extra,
    kinds: &'a crate::grammar::Kinds,
    scanner: &crate::scan::Scanner,
    source: &str,
    depth: u32,
    out: &mut Vec<Target<'a>>,
) {
    let mut stack: Vec<&Node<Kind>> = Vec::from([tree]);
    while let Some(node) = stack.pop() {
        for child in node.children() {
            let kind = *child.kind();
            if kind.is_trivia() {
                continue;
            }
            let index = kind.index();
            if let Ok(at) = extra
                .embedded_parse
                .binary_search_by_key(&index, |(k, _)| *k)
            {
                let (_, rule) = extra.embedded_parse[at];
                out.push(Target {
                    id: kinds.name_at(usize::from(index)),
                    language: "self",
                    span: child.span(),
                    rule: Some(rule),
                    editor: false,
                    depth,
                });
            }
            for def in extra.injections.iter() {
                let hit = match def.label {
                    None => def.kind == index,
                    Some(label) => node.kind().index() == def.kind && kind.label() == Some(label),
                };
                if hit {
                    out.push(Target {
                        id: &def.id,
                        language: &def.language,
                        span: content(def, child, scanner, source),
                        rule: (&*def.language == "self" && !def.editor).then_some(def.start),
                        editor: def.editor,
                        depth,
                    });
                }
            }
            if let Element::Node(n) = child {
                stack.push(n);
            }
        }
    }
}

/// The range an injection covers: the element, or (`content = "inner"`) a
/// string token without its delimiters.
fn content(
    def: &InjectionDef,
    element: &Element<Kind>,
    scanner: &crate::scan::Scanner,
    source: &str,
) -> Span {
    let span = element.span();
    if !def.inner {
        return span;
    }
    let Element::Token(token) = element else {
        return span;
    };
    let text = &source[span.start().to_usize()..span.end().to_usize()];
    for class in scanner.strings.iter() {
        let StringKinds::Token(kind) = class.kinds else {
            continue;
        };
        if kind != *token.kind() {
            continue;
        }
        if let ([PartRt::Text(open)], [PartRt::Text(close)]) = (&*class.open, &*class.close) {
            if text.len() >= open.len() + close.len()
                && text.as_bytes().starts_with(open)
                && text.as_bytes().ends_with(close)
            {
                return Span::new(
                    span.start().to_u32() + open.len() as u32,
                    span.end().to_u32() - close.len() as u32,
                );
            }
        }
    }
    span
}
