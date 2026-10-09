//! Turning the parser's events into a lossless `syntax_lang` tree.
//!
//! The parser only sees significant tokens; this pass threads the trivia back
//! in. Trivia between two significant tokens is placed before the next node
//! or token, in the innermost node open at that point, so a node's span starts
//! at its first significant token and ends at its last: comments and
//! whitespace between statements belong to the enclosing block, not to the
//! statement they happen to precede. Trivia after the last token goes into
//! the root, which therefore covers the whole source.
//!
//! Field labels (format 2) ride on the events as scopes: `Label(l)` opens a
//! scope at the current depth and `Unlabel` closes it; a node or token added
//! at the depth of the innermost scope carries that scope's label in its
//! kind, unless its `Start` event already carries one of its own (an operand
//! an operator node wrapped after the fact). Trivia and `ERROR` nodes are
//! never labelled.

use alloc::vec::Vec;

use syntax_lang::{Builder, Element, Node, Token};

use crate::kind::Kind;

/// One step of the parse, replayed to build the tree, packed into 8 bytes.
///
/// The parser records an event per token and node, and the memo copies them
/// when it rescues or replays an attempt, so their size is memory traffic on
/// the hottest paths. With a four-byte [`Kind`] a plain enum would take 12
/// bytes; this packs the variant into `b`, which a `Start` uses for its
/// forward link: values from [`TAG`] up are the other variants, and a link is
/// an index into the event list, which stays far below them (it would take
/// tens of gigabytes of events to reach). Read one with [`Event::step`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Event {
    /// The `Start`'s or `TokenAs`'s kind bits, or the `Label`'s label.
    a: u32,
    /// A `Start`'s forward link, or `TAG + variant`.
    b: u32,
}

/// The first `b` value that is a variant tag rather than a forward link.
const TAG: u32 = 0xFFFF_FFF0;

impl Event {
    pub(crate) const TOMBSTONE: Self = Self { a: 0, b: TAG };
    pub(crate) const TOKEN: Self = Self { a: 0, b: TAG + 1 };
    pub(crate) const UNLABEL: Self = Self { a: 0, b: TAG + 2 };
    pub(crate) const FINISH: Self = Self { a: 0, b: TAG + 3 };

    /// Opens a node of `kind`, wrapped later by the `Start` at `forward` (0:
    /// none).
    #[inline]
    pub(crate) const fn start(kind: Kind, forward: u32) -> Self {
        Self {
            a: kind.bits(),
            b: forward,
        }
    }

    #[inline]
    pub(crate) const fn token_as(kind: Kind) -> Self {
        Self {
            a: kind.bits(),
            b: TAG + 4,
        }
    }

    #[inline]
    pub(crate) const fn label(label: u16) -> Self {
        Self {
            a: label as u32,
            b: TAG + 5,
        }
    }

    /// The event as a [`Step`], for matching.
    #[inline]
    pub(crate) const fn step(self) -> Step {
        match self.b {
            b if b < TAG => Step::Start {
                kind: Kind::from_bits(self.a),
                forward: b,
            },
            b if b == TAG + 1 => Step::Token,
            b if b == TAG + 2 => Step::Unlabel,
            b if b == TAG + 3 => Step::Finish,
            b if b == TAG + 4 => Step::TokenAs(Kind::from_bits(self.a)),
            b if b == TAG + 5 => Step::Label(self.a as u16),
            _ => Step::Tombstone,
        }
    }
}

impl core::fmt::Debug for Event {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.step().fmt(f)
    }
}

/// An [`Event`], unpacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Open a node. `forward` (when non-zero) is the index of a later `Start`
    /// whose node must wrap this one — an operator node discovered after its
    /// left operand was already parsed.
    Start { kind: Kind, forward: u32 },
    /// A placeholder that turned out not to be needed, or a `Start` already
    /// replayed through a forward link.
    Tombstone,
    /// The next significant token.
    Token,
    /// The next significant token, with another kind: a contextual keyword
    /// matched where the grammar asks for it (format 2).
    TokenAs(Kind),
    /// Open a label scope at the current depth (format 2).
    Label(u16),
    /// Close the innermost label scope.
    Unlabel,
    /// Close the innermost open node.
    Finish,
}

/// Replays `events` over `tokens` (all tokens, trivia included).
///
/// `fallback` names the root should the events ever be unbalanced; the parser
/// never produces such events, and the fallback keeps the tree lossless
/// regardless.
pub(crate) fn build(
    tokens: &[Token<Kind>],
    events: &mut [Event],
    fallback: Kind,
    error: Kind,
) -> Node<Kind> {
    let mut builder = Builder::new();
    let mut next = 0;
    let mut open = 0usize;
    let mut chain: Vec<Kind> = Vec::new();
    let mut scopes: Vec<(usize, u16)> = Vec::new();
    // The label of an element added at depth `open`.
    let label_at = |scopes: &[(usize, u16)], open: usize| match scopes.last() {
        Some(&(depth, label)) if depth == open => Some(label),
        _ => None,
    };
    for i in 0..events.len() {
        match events[i].step() {
            Step::Start { kind, forward } => {
                chain.clear();
                chain.push(kind);
                let mut link = forward as usize;
                while link != 0 {
                    let Step::Start { kind, forward } = events[link].step() else {
                        break;
                    };
                    chain.push(kind);
                    events[link] = Event::TOMBSTONE;
                    link = forward as usize;
                }
                if open > 0 {
                    next = trivia(&mut builder, tokens, next);
                }
                for &kind in chain.iter().rev() {
                    let kind = if kind.label().is_some() || kind == error {
                        kind
                    } else {
                        kind.with_label(label_at(&scopes, open))
                    };
                    builder.start_node(kind);
                    open += 1;
                }
            }
            Step::Token => {
                next = trivia(&mut builder, tokens, next);
                if let Some(&token) = tokens.get(next) {
                    let label = label_at(&scopes, open);
                    builder.token(match label {
                        Some(_) => Token::new(token.kind().with_label(label), token.span()),
                        None => token,
                    });
                    next += 1;
                }
            }
            Step::TokenAs(kind) => {
                next = trivia(&mut builder, tokens, next);
                if let Some(&token) = tokens.get(next) {
                    let kind = kind.with_label(label_at(&scopes, open));
                    builder.token(Token::new(kind, token.span()));
                    next += 1;
                }
            }
            Step::Label(label) => scopes.push((open, label)),
            Step::Unlabel => {
                let _ = scopes.pop();
            }
            Step::Finish => {
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
            Step::Tombstone => {}
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
            node.kind().slot(),
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
            Event::start(ROOT, 0),
            Event::start(EXPR, 0),
            Event::start(BIN, 7),
            Event::TOKEN,
            Event::TOKEN,
            Event::TOKEN,
            Event::FINISH,
            Event::start(BIN, 0),
            Event::TOKEN,
            Event::TOKEN,
            Event::FINISH,
            Event::FINISH,
            Event::FINISH,
        ];
        let root = build(&tokens(), &mut events, ROOT, ROOT);
        let mut out = Vec::new();
        outline(&root, &mut out);
        assert_eq!(out, [(20, 0, 9), (21, 1, 8), (22, 1, 8), (22, 1, 6)]);
        assert_eq!(root.tokens().count(), 9);
    }

    #[test]
    fn test_build_places_trivia_outside_nodes() {
        let mut events = vec![
            Event::start(ROOT, 0),
            Event::TOMBSTONE,
            Event::start(EXPR, 0),
            Event::TOKEN,
            Event::TOKEN,
            Event::TOKEN,
            Event::TOKEN,
            Event::TOKEN,
            Event::FINISH,
            Event::FINISH,
        ];
        let root = build(&tokens(), &mut events, ROOT, ROOT);
        let mut out = Vec::new();
        outline(&root, &mut out);
        // The leading and trailing spaces stay in the root.
        assert_eq!(out, [(20, 0, 9), (21, 1, 8)]);
    }

    #[test]
    fn test_build_unbalanced_events_fall_back_losslessly() {
        let mut events = vec![Event::start(ROOT, 0), Event::TOKEN];
        let result = std::panic::catch_unwind(move || build(&tokens(), &mut events, ROOT, ROOT));
        // Debug builds flag the bug; release builds return the fallback.
        if let Ok(root) = result {
            assert_eq!(root.tokens().count(), 9);
        }
    }
}
