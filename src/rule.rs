//! The rule language: the text of a grammar rule, parsed into an [`Ast`].
//!
//! ```text
//! choice  = seq ('|' seq)*
//! seq     = postfix+
//! postfix = atom ('*' | '+' | '?')?
//! atom    = NAME | 'literal' | "literal" | '(' choice ')'
//! ```
//!
//! A `NAME` is a rule or one of the built-in token classes (`IDENT`, `NUMBER`,
//! `STRING`, `NEWLINE`); a quoted literal is a keyword or symbol. Whitespace,
//! including line breaks, separates elements and is otherwise ignored, so long
//! rules can be written over several lines in a multi-line string.

use alloc::{boxed::Box, format, vec::Vec};

use syntax_lang::Span;

use crate::{error::Report, noml::Text};

/// How deeply parentheses may nest inside one rule. Generous for any real
/// grammar, and it bounds the recursion of every later pass over the rule.
const MAX_NESTING: u32 = 64;

/// A parsed rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ast<'t> {
    /// A quoted keyword or symbol.
    Literal(&'t str, Span),
    /// A rule or token class.
    Name(&'t str, Span),
    /// Elements matched one after another.
    Seq(Vec<Ast<'t>>, Span),
    /// Alternatives tried in order.
    Choice(Vec<Ast<'t>>, Span),
    /// `body*` (`min_one == false`) or `body+`.
    Repeat {
        body: Box<Ast<'t>>,
        min_one: bool,
        span: Span,
    },
    /// `body?`.
    Optional(Box<Ast<'t>>, Span),
}

impl Ast<'_> {
    /// The schematic span the element was read from.
    pub(crate) fn span(&self) -> Span {
        match self {
            Ast::Literal(_, span)
            | Ast::Name(_, span)
            | Ast::Seq(_, span)
            | Ast::Choice(_, span)
            | Ast::Optional(_, span)
            | Ast::Repeat { span, .. } => *span,
        }
    }

    /// Calls `visit` on every node, parents before children.
    pub(crate) fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Self)) {
        visit(self);
        match self {
            Ast::Literal(..) | Ast::Name(..) => {}
            Ast::Seq(items, _) | Ast::Choice(items, _) => {
                for item in items {
                    item.walk(visit);
                }
            }
            Ast::Repeat { body, .. } | Ast::Optional(body, _) => body.walk(visit),
        }
    }
}

/// Parses the text of one rule. `whole` is the span of the string value in the
/// schematic, used when escapes keep finer positions from being exact.
///
/// Problems are added to `report`; the rule is then `None`.
pub(crate) fn parse<'t>(text: &'t Text<'_>, whole: Span, report: &mut Report) -> Option<Ast<'t>> {
    let mut parser = RuleParser {
        text: &text.text,
        source: text,
        whole,
        pos: 0,
        depth: 0,
    };
    parser.skip_space();
    if parser.at_end() {
        report.error(whole, "the rule is empty");
        return None;
    }
    let result = parser.choice().and_then(|ast| {
        parser.skip_space();
        if parser.at_end() {
            Ok(ast)
        } else {
            Err((parser.point(), alloc::string::String::from("unmatched `)`")))
        }
    });
    match result {
        Ok(ast) => Some(ast),
        Err((span, message)) => {
            report.error(span, message);
            None
        }
    }
}

type Problem = (Span, alloc::string::String);

struct RuleParser<'t, 'r> {
    text: &'t str,
    source: &'r Text<'r>,
    whole: Span,
    pos: usize,
    depth: u32,
}

impl<'t> RuleParser<'t, '_> {
    fn choice(&mut self) -> Result<Ast<'t>, Problem> {
        let start = self.pos;
        let mut alternatives = Vec::new();
        loop {
            alternatives.push(self.seq()?);
            self.skip_space();
            if self.peek() == Some(b'|') {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(if alternatives.len() == 1 {
            alternatives.remove(0)
        } else {
            Ast::Choice(alternatives, self.span(start, self.pos))
        })
    }

    fn seq(&mut self) -> Result<Ast<'t>, Problem> {
        self.skip_space();
        let start = self.pos;
        let mut items = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                None | Some(b'|' | b')') => break,
                _ => items.push(self.postfix()?),
            }
        }
        match items.len() {
            0 => Err((
                self.point(),
                format!(
                    "expected an element, found {}",
                    self.describe_here("the end of the rule")
                ),
            )),
            1 => Ok(items.remove(0)),
            _ => Ok(Ast::Seq(items, self.span(start, self.pos))),
        }
    }

    fn postfix(&mut self) -> Result<Ast<'t>, Problem> {
        let start = self.pos;
        let atom = self.atom()?;
        let ast = match self.peek() {
            Some(op @ (b'*' | b'+')) => {
                self.pos += 1;
                Ast::Repeat {
                    body: Box::new(atom),
                    min_one: op == b'+',
                    span: self.span(start, self.pos),
                }
            }
            Some(b'?') => {
                self.pos += 1;
                Ast::Optional(Box::new(atom), self.span(start, self.pos))
            }
            _ => return Ok(atom),
        };
        if matches!(self.peek(), Some(b'*' | b'+' | b'?')) {
            return Err((
                self.span(self.pos, self.pos + 1),
                format!(
                    "`{}` cannot follow another `*`, `+`, or `?`; group with parentheses first",
                    char::from(self.text.as_bytes()[self.pos])
                ),
            ));
        }
        Ok(ast)
    }

    fn atom(&mut self) -> Result<Ast<'t>, Problem> {
        let start = self.pos;
        match self.peek() {
            Some(b'(') => {
                if self.depth >= MAX_NESTING {
                    return Err((
                        self.span(start, start + 1),
                        format!("parentheses nest more than {MAX_NESTING} levels deep"),
                    ));
                }
                self.pos += 1;
                self.depth += 1;
                let inner = self.choice()?;
                self.depth -= 1;
                self.skip_space();
                if self.peek() != Some(b')') {
                    return Err((
                        self.span(start, start + 1),
                        alloc::string::String::from("unclosed `(`"),
                    ));
                }
                self.pos += 1;
                Ok(inner)
            }
            Some(quote @ (b'\'' | b'"')) => {
                self.pos += 1;
                let content = self.pos;
                let Some(len) = self.text[content..].find(char::from(quote)) else {
                    return Err((
                        self.span(start, start + 1),
                        alloc::string::String::from("unterminated literal"),
                    ));
                };
                let literal = &self.text[content..content + len];
                self.pos = content + len + 1;
                if literal.contains(['\n', '\r']) {
                    return Err((
                        self.span(start, self.pos),
                        alloc::string::String::from("a literal cannot span lines"),
                    ));
                }
                Ok(Ast::Literal(literal, self.span(start, self.pos)))
            }
            Some(b) if b.is_ascii_alphabetic() || b == b'_' => {
                while self
                    .peek()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    self.pos += 1;
                }
                Ok(Ast::Name(
                    &self.text[start..self.pos],
                    self.span(start, self.pos),
                ))
            }
            Some(b'*' | b'+' | b'?') => Err((
                self.span(start, start + 1),
                format!(
                    "`{}` must follow the element it applies to",
                    char::from(self.text.as_bytes()[start])
                ),
            )),
            _ => Err(self.unexpected("an element")),
        }
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn span(&self, from: usize, to: usize) -> Span {
        self.source.span(from, to, self.whole)
    }

    /// The span of the character at the current position (empty at the end).
    fn point(&self) -> Span {
        let width = self.text[self.pos..]
            .chars()
            .next()
            .map_or(0, char::len_utf8);
        self.span(self.pos, self.pos + width)
    }

    fn describe_here(&self, at_end: &str) -> alloc::string::String {
        match self.text[self.pos..].chars().next() {
            None => alloc::string::String::from(at_end),
            Some(c) if c.is_control() => format!("`{}`", c.escape_debug()),
            Some(c) => format!("`{c}`"),
        }
    }

    fn unexpected(&self, expected: &str) -> Problem {
        (
            self.point(),
            format!(
                "expected {expected}, found {}",
                self.describe_here("the end of the rule")
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use alloc::{borrow::Cow, string::String};

    use super::*;

    fn text(s: &str) -> Text<'_> {
        Text {
            text: Cow::Borrowed(s),
            start: 100,
            exact: true,
        }
    }

    fn parse_ok(s: &str) -> String {
        let t = text(s);
        let mut report = Report::default();
        let ast = parse(&t, Span::new(99, 99 + s.len() as u32 + 2), &mut report).unwrap();
        render(&ast)
    }

    fn parse_err(s: &str) -> (String, Span) {
        let t = text(s);
        let mut report = Report::default();
        assert!(parse(&t, Span::new(0, 1), &mut report).is_none());
        let err = report.into_error("");
        let d = &err.diagnostics()[0];
        (String::from(d.message()), d.primary().span())
    }

    /// An S-expression rendering, for compact assertions.
    fn render(ast: &Ast<'_>) -> String {
        match ast {
            Ast::Literal(t, _) => format!("'{t}'"),
            Ast::Name(n, _) => String::from(*n),
            Ast::Seq(items, _) => format!(
                "(seq {})",
                items.iter().map(render).collect::<Vec<_>>().join(" ")
            ),
            Ast::Choice(items, _) => format!(
                "(alt {})",
                items.iter().map(render).collect::<Vec<_>>().join(" ")
            ),
            Ast::Repeat { body, min_one, .. } => {
                format!("({} {})", if *min_one { "+" } else { "*" }, render(body))
            }
            Ast::Optional(body, _) => format!("(? {})", render(body)),
        }
    }

    #[test]
    fn test_parse_sequence_choice_and_postfix() {
        assert_eq!(
            parse_ok("'let' IDENT '=' expr ';'"),
            "(seq 'let' IDENT '=' expr ';')"
        );
        assert_eq!(parse_ok("a | b c | d"), "(alt a (seq b c) d)");
        assert_eq!(parse_ok("a* b+ c?"), "(seq (* a) (+ b) (? c))");
        assert_eq!(parse_ok("(a | b)* \"x\""), "(seq (* (alt a b)) 'x')");
        assert_eq!(parse_ok("  a\n  | b\n"), "(alt a b)");
        assert_eq!(parse_ok("'\"'"), "'\"'");
    }

    #[test]
    fn test_parse_spans_map_into_schematic() {
        let t = text("a 'b'");
        let mut report = Report::default();
        let ast = parse(&t, Span::new(0, 1), &mut report).unwrap();
        let Ast::Seq(items, span) = ast else { panic!() };
        assert_eq!(span, Span::new(100, 105));
        assert_eq!(items[0].span(), Span::new(100, 101));
        assert_eq!(items[1].span(), Span::new(102, 105));
    }

    #[test]
    fn test_parse_inexact_text_falls_back_to_whole_span() {
        let t = Text {
            text: Cow::Borrowed("a b"),
            start: 7,
            exact: false,
        };
        let mut report = Report::default();
        let ast = parse(&t, Span::new(5, 12), &mut report).unwrap();
        assert_eq!(ast.span(), Span::new(5, 12));
    }

    #[test]
    fn test_parse_errors() {
        assert_eq!(parse_err("   ").0, "the rule is empty");
        assert_eq!(
            parse_err("a |").0,
            "expected an element, found the end of the rule"
        );
        assert_eq!(parse_err("a | | b").0, "expected an element, found `|`");
        assert_eq!(parse_err("(a").0, "unclosed `(`");
        assert_eq!(parse_err("a )").0, "unmatched `)`");
        assert_eq!(parse_err("()").0, "expected an element, found `)`");
        assert_eq!(parse_err("'abc").0, "unterminated literal");
        assert_eq!(
            parse_err("* a").0,
            "`*` must follow the element it applies to"
        );
        assert_eq!(
            parse_err("a*?").0,
            "`?` cannot follow another `*`, `+`, or `?`; group with parentheses first"
        );
        assert_eq!(parse_err("a ; b").0, "expected an element, found `;`");
        assert_eq!(parse_err("'a\nb'").0, "a literal cannot span lines");
    }

    #[test]
    fn test_parse_error_span_points_at_problem() {
        assert_eq!(parse_err("a ; b").1, Span::new(102, 103));
    }

    #[test]
    fn test_parse_limits_nesting() {
        let deep = format!("{}a{}", "(".repeat(70), ")".repeat(70));
        assert!(parse_err(&deep).0.contains("nest more than 64 levels"));
        let ok = format!("{}a{}", "(".repeat(64), ")".repeat(64));
        assert_eq!(parse_ok(&ok), "a");
    }

    #[test]
    fn test_walk_visits_parents_first() {
        let t = text("(a | 'b')*");
        let mut report = Report::default();
        let ast = parse(&t, Span::new(0, 1), &mut report).unwrap();
        let mut seen = Vec::new();
        ast.walk(&mut |n| seen.push(render(n)));
        assert_eq!(seen, ["(* (alt a 'b'))", "(alt a 'b')", "a", "'b'"]);
    }
}
