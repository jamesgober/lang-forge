//! Turning the parser's events into a lossless `syntax_lang` tree.
//!
//! The parser only sees significant tokens; this pass threads the trivia back
//! in. Trivia between two significant tokens is placed before the next node
//! or token, in the innermost node open at that point, so a node's span starts
//! at its first significant token and ends at its last: comments and
//! whitespace between statements belong to the enclosing block, not to the
//! statement they happen to precede. Trivia after the last token goes into
//! the root, which therefore covers the whole source.

use alloc::vec::Vec;

use syntax_lang::{Builder, Element, Node, Token};

use crate::kind::Kind;

/// One step of the parse, replayed to build the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// Open a node. `forward` (when non-zero) is the index of a later `Start`
    /// whose node must wrap this one — an operator node discovered after its
    /// left operand was already parsed.
    Start { kind: Kind, forward: u32 },
    /// A placeholder that turned out not to be needed, or a `Start` already
    /// replayed through a forward link.
    Tombstone,
    /// The next significant token.
    Token,
    /// Close the innermost open node.
    Finish,
}

/// Replays `events` over `tokens` (all tokens, trivia included).
///
/// `fallback` names the root should the events ever be unbalanced; the parser
/// never produces such events, and the fallback keeps the tree lossless
/// regardless.
pub(crate) fn build(tokens: &[Token<Kind>], events: &mut [Event], fallback: Kind) -> Node<Kind> {
    let mut builder = Builder::new();
    let mut next = 0;
    let mut open = 0usize;
    let mut chain: Vec<Kind> = Vec::new();
    for i in 0..events.len() {
        match events[i] {
            Event::Start { kind, forward } => {
                chain.clear();
                chain.push(kind);
                let mut link = forward as usize;
                while link != 0 {
                    let Event::Start { kind, forward } = events[link] else {
                        break;
                    };
                    chain.push(kind);
                    events[link] = Event::Tombstone;
                    link = forward as usize;
                }
                if open > 0 {
                    next = trivia(&mut builder, tokens, next);
                }
                for &kind in chain.iter().rev() {
                    builder.start_node(kind);
                    open += 1;
                }
            }
            Event::Token => {
                next = trivia(&mut builder, tokens, next);
                if let Some(&token) = tokens.get(next) {
                    builder.token(token);
                    next += 1;
                }
            }
            Event::Finish => {
                if open == 1 {
                    // Closing the root: everything left is trailing trivia.
                    for &token in &tokens[next.min(tokens.len())..] {
                        builder.token(token);
                    }
                    next = tokens.len();
                }
                open = open.saturating_sub(1);
                builder.finish_node();
            }
            Event::Tombstone => {}
        }
    }
    match builder.finish() {
        Ok(root) if next == tokens.len() => root,
        _ => {
            debug_assert!(false, "unbalanced parse events");
            Node::new(
                fallback,
                tokens.iter().map(|t| Element::Token(*t)).collect(),
            )
        }
    }
}

/// Adds the trivia tokens starting at `next`; returns the index after them.
#[inline]
fn trivia(builder: &mut Builder<Kind>, tokens: &[Token<Kind>], mut next: usize) -> usize {
    while let Some(&token) = tokens.get(next) {
        if !token.is_trivia() {
            break;
        }
        builder.token(token);
        next += 1;
    }
    next
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use syntax_lang::Span;

    use super::*;

    const ROOT: Kind = Kind::new(20, false);
    const EXPR: Kind = Kind::new(21, false);
    const BIN: Kind = Kind::new(22, false);
    const NUM: Kind = Kind::new(5, false);
    const PLUS: Kind = Kind::new(7, false);
    const WS: Kind = Kind::new(1, true);

    fn tok(kind: Kind, start: u32, end: u32) -> Token<Kind> {
        Token::new(kind, Span::new(start, end))
    }

    /// `1 + 2 + 3`, with spaces.
    fn tokens() -> Vec<Token<Kind>> {
        vec![
            tok(WS, 0, 1),
            tok(NUM, 1, 2),
            tok(WS, 2, 3),
            tok(PLUS, 3, 4),
            tok(WS, 4, 5),
            tok(NUM, 5, 6),
            tok(PLUS, 6, 7),
            tok(NUM, 7, 8),
            tok(WS, 8, 9),
        ]
    }

    fn outline(node: &Node<Kind>, out: &mut Vec<(usize, u32, u32)>) {
        out.push((
            node.kind().index(),
            node.span().start().to_u32(),
            node.span().end().to_u32(),
        ));
        for child in node.child_nodes() {
            outline(child, out);
        }
    }

    #[test]
    fn test_build_forward_links_wrap_left_operands() {
        // root( expr( bin( bin(1 + 2) + 3 ) ) ), the outer `bin` spliced in by
        // a forward link from the inner one's start.
        let mut events = vec![
            Event::Start {
                kind: ROOT,
                forward: 0,
            },
            Event::Start {
                kind: EXPR,
                forward: 0,
            },
            Event::Start {
                kind: BIN,
                forward: 7,
            },
            Event::Token,
            Event::Token,
            Event::Token,
            Event::Finish,
            Event::Start {
                kind: BIN,
                forward: 0,
            },
            Event::Token,
            Event::Token,
            Event::Finish,
            Event::Finish,
            Event::Finish,
        ];
        let root = build(&tokens(), &mut events, ROOT);
        let mut out = Vec::new();
        outline(&root, &mut out);
        assert_eq!(out, [(20, 0, 9), (21, 1, 8), (22, 1, 8), (22, 1, 6)]);
        assert_eq!(root.tokens().count(), 9);
    }

    #[test]
    fn test_build_places_trivia_outside_nodes() {
        let mut events = vec![
            Event::Start {
                kind: ROOT,
                forward: 0,
            },
            Event::Tombstone,
            Event::Start {
                kind: EXPR,
                forward: 0,
            },
            Event::Token,
            Event::Token,
            Event::Token,
            Event::Token,
            Event::Token,
            Event::Finish,
            Event::Finish,
        ];
        let root = build(&tokens(), &mut events, ROOT);
        let mut out = Vec::new();
        outline(&root, &mut out);
        // The leading and trailing spaces stay in the root.
        assert_eq!(out, [(20, 0, 9), (21, 1, 8)]);
    }

    #[test]
    fn test_build_unbalanced_events_fall_back_losslessly() {
        let mut events = vec![
            Event::Start {
                kind: ROOT,
                forward: 0,
            },
            Event::Token,
        ];
        let result = std::panic::catch_unwind(move || build(&tokens(), &mut events, ROOT));
        // Debug builds flag the bug; release builds return the fallback.
        if let Ok(root) = result {
            assert_eq!(root.tokens().count(), 9);
        }
    }
}
