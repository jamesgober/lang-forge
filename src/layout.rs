//! Layout (LSF2 §10): the offside rule and newline policies, applied to the
//! scanner's token stream.
//!
//! With a layout policy in force the scanner lexes every line break outside
//! strings and comments as a `NEWLINE` token. This pass then decides, line by
//! line, which of them end a logical line (they stay `NEWLINE`) and which are
//! only whitespace (inside brackets, on blank or comment-only lines, on
//! continuation lines, or where the terminator rules say so), and — for
//! `style = "indent"` — inserts zero-width `INDENT` and `DEDENT` tokens before
//! the first token of a line whose indentation differs from the enclosing
//! block's. The stream stays lossless: tokens are only re-kinded or inserted
//! with empty spans. The pass is linear in the number of tokens.

use alloc::vec::Vec;

use diag_lang::{Diagnostic, Label, Severity};
use syntax_lang::{Span, Token, TokenKind};

use crate::{
    codes,
    kind::Kind,
    scan::{LayoutRt, Scanner, StringKinds},
};

/// Applies `layout` to `tokens[first..]`.
pub(crate) fn apply(
    sc: &Scanner,
    layout: &LayoutRt,
    src: &str,
    first: usize,
    tokens: &mut Vec<Token<Kind>>,
    diags: &mut Vec<Diagnostic>,
) {
    let k = &sc.k;
    // Interpolation holes count as implicit joins.
    let mut joins: Vec<(u16, u16)> = layout.joins.to_vec();
    let mut newline_joins: Vec<(u16, u16)> = layout.newline_joins.to_vec();
    for s in sc.strings.iter() {
        if let StringKinds::Node {
            interp_open: Some(o),
            interp_close: Some(c),
            ..
        } = &s.kinds
        {
            joins.push((o.index(), c.index()));
            newline_joins.push((o.index(), c.index()));
        }
    }
    let depth_change = |pairs: &[(u16, u16)], kind: u16| -> i32 {
        if pairs.iter().any(|(o, _)| *o == kind) {
            1
        } else if pairs.iter().any(|(_, c)| *c == kind) {
            -1
        } else {
            0
        }
    };

    let input: Vec<Token<Kind>> = tokens.drain(first..).collect();
    let out = tokens;
    out.reserve(input.len() + 8);

    // The indent stack: (column with tabs at `tab_width`, column with tabs at
    // 1), the second only to detect inconsistent tabs and spaces.
    let mut stack: Vec<(u32, u32)> = Vec::from([(0, 0)]);
    let mut join_depth: i32 = 0;
    let mut newline_depth: i32 = 0;
    let mut at_line_start = true;
    let mut line_has_tokens = false;
    // The output index of the line break that ended the last logical line,
    // while it may still be revoked by a continuation line.
    let mut pending: Option<usize> = None;
    let mut prev_significant: Option<u16> = None;

    let whitespace = |t: &Token<Kind>| Token::new(k.whitespace, t.span());
    for t in input {
        let kind = *t.kind();
        if kind == k.newline {
            let inside = if layout.indent {
                join_depth > 0
            } else {
                newline_depth > 0
            };
            if inside || !line_has_tokens {
                out.push(whitespace(&t));
                continue;
            }
            let terminates = match layout.newlines {
                // `terminators`: only after a terminating token.
                2 if !layout.indent => prev_significant
                    .is_some_and(|p| layout.terminate_after.binary_search(&p).is_ok()),
                0 if !layout.indent => false,
                _ => true,
            };
            if terminates {
                pending = Some(out.len());
                out.push(t);
                line_has_tokens = false;
                at_line_start = true;
            } else {
                out.push(whitespace(&t));
            }
            continue;
        }
        if kind.is_trivia() {
            out.push(t);
            continue;
        }
        // A significant token.
        let index = kind.index();
        if at_line_start {
            // `continue_before`: a line starting with one of these continues
            // the previous one.
            if layout.newlines == 2
                && !layout.indent
                && layout.continue_before.binary_search(&index).is_ok()
            {
                if let Some(at) = pending.take() {
                    out[at] = whitespace(&out[at]);
                }
            }
            if layout.indent && join_depth == 0 {
                let (column, alt) = indentation(src, t.span().start().to_usize(), layout.tab_width);
                let &(top, top_alt) = stack.last().unwrap_or(&(0, 0));
                let deeper = column > top;
                let continuation = deeper
                    && !layout.open_after.is_empty()
                    && prev_significant
                        .is_none_or(|p| layout.open_after.binary_search(&p).is_err());
                if continuation {
                    // A continuation line: the line break before it joins.
                    if let Some(at) = pending.take() {
                        out[at] = whitespace(&out[at]);
                    }
                } else {
                    let at = t.span().start().to_u32();
                    if layout.mixed_error && (column > top) != (alt > top_alt) {
                        diags.push(error(
                            codes::LEX_MIXED_INDENT,
                            at,
                            "inconsistent use of tabs and spaces in indentation",
                        ));
                    }
                    if deeper {
                        stack.push((column, alt));
                        out.push(Token::new(k.indent, Span::empty(at)));
                    } else if column < top {
                        while stack.len() > 1 && stack.last().is_some_and(|s| s.0 > column) {
                            let _ = stack.pop();
                            out.push(Token::new(k.dedent, Span::empty(at)));
                        }
                        if stack.last().is_some_and(|s| s.0 != column) {
                            diags.push(error(
                                codes::LEX_BAD_DEDENT,
                                at,
                                "unindent does not match any outer indentation level",
                            ));
                        }
                    }
                    pending = None;
                }
            } else {
                pending = None;
            }
            at_line_start = false;
        }
        join_depth = (join_depth + depth_change(&joins, index)).max(0);
        newline_depth = (newline_depth + depth_change(&newline_joins, index)).max(0);
        line_has_tokens = true;
        prev_significant = Some(index);
        out.push(t);
    }
    if layout.indent {
        let end = src.len() as u32;
        if line_has_tokens {
            out.push(Token::new(k.newline, Span::empty(end)));
        }
        while stack.len() > 1 {
            let _ = stack.pop();
            out.push(Token::new(k.dedent, Span::empty(end)));
        }
    }
}

/// The indentation column (0-based) of the line containing `at`, measured
/// with tabs at `tab` and at 1.
fn indentation(src: &str, at: usize, tab: u32) -> (u32, u32) {
    let bytes = src.as_bytes();
    let mut start = at;
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    let tab = tab.max(1);
    let (mut column, mut alt) = (0u32, 0u32);
    for &b in &bytes[start..at] {
        match b {
            b'\t' => {
                column = (column / tab + 1) * tab;
                alt += 1;
            }
            b' ' => {
                column += 1;
                alt += 1;
            }
            _ => {
                // Something else before the first token (a comment run, a
                // BOM): the token's own column counts.
                column += 1;
                alt += 1;
            }
        }
    }
    (column, alt)
}

fn error(code: diag_lang::Code, at: u32, message: &str) -> Diagnostic {
    Diagnostic::new(
        Severity::Error,
        alloc::string::String::from(message),
        Label::unlabelled(Span::empty(at)),
    )
    .with_code(code)
}
