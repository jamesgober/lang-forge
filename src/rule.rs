//! The rule language: the text of a grammar rule, parsed into an [`Ast`].
//!
//! Format 1:
//!
//! ```text
//! choice  = seq ('|' seq)*
//! seq     = postfix+
//! postfix = atom ('*' | '+' | '?')?
//! atom    = NAME | 'literal' | "literal" | '(' choice ')'
//! ```
//!
//! Format 2 (LSF2 §11.2) is a strict superset:
//!
//! ```text
//! choice   = seq ('|' seq)*
//! seq      = element+
//! element  = label? ( '&' unary | '!' unary | unary )
//! label    = LABEL ':'                   -- no space between LABEL and ':'
//! unary    = primary quant? textref?
//! textref  = '=' LABEL                   -- no space before '='
//! primary  = '(' choice ')' | LITERAL | NAME | '@' HOOK | '%' MODE '(' choice ')'
//! ```
//!
//! A `NAME` is a rule or a token class (`IDENT`, `NUMBER`, `STRING`,
//! `NEWLINE`, and in format 2 any declared class); a quoted literal is a
//! keyword or symbol. Whitespace, including line breaks, separates elements
//! and is otherwise ignored, so long rules can be written over several lines
//! in a multi-line string. A format-1 rule means exactly what it meant in
//! lang-forge 1.x, errors included: the format-2 syntax is recognized only
//! when the sketch says `format = 2`.

use alloc::{boxed::Box, format, vec::Vec};

use syntax_lang::Span;

use crate::{codes, error::Report, noml::Text};

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
    /// `label:body` (format 2): the field label of what `body` adds.
    Label {
        label: &'t str,
        label_span: Span,
        body: Box<Ast<'t>>,
        span: Span,
    },
    /// `&body` (format 2): succeeds iff `body` would match here.
    And(Box<Ast<'t>>, Span),
    /// `!body` (format 2): succeeds iff `body` would not match here.
    Not(Box<Ast<'t>>, Span),
    /// `body=label` (format 2): `body`'s text must equal the text of the token
    /// last labelled `label` in this rule invocation.
    BackRef {
        body: Box<Ast<'t>>,
        label: &'t str,
        label_span: Span,
        span: Span,
    },
    /// `@hook` (format 2): a predicate hook.
    Hook(&'t str, Span),
    /// `%mode(body)` (format 2): `body` lexed in `mode`.
    Mode {
        mode: &'t str,
        body: Box<Ast<'t>>,
        span: Span,
    },
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
            | Ast::Repeat { span, .. }
            | Ast::Label { span, .. }
            | Ast::And(_, span)
            | Ast::Not(_, span)
            | Ast::BackRef { span, .. }
            | Ast::Hook(_, span)
            | Ast::Mode { span, .. } => *span,
        }
    }

    /// Calls `visit` on every node, parents before children.
    pub(crate) fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Self)) {
        visit(self);
        match self {
            Ast::Literal(..) | Ast::Name(..) | Ast::Hook(..) => {}
            Ast::Seq(items, _) | Ast::Choice(items, _) => {
                for item in items {
                    item.walk(visit);
                }
            }
            Ast::Repeat { body, .. }
            | Ast::Optional(body, _)
            | Ast::Label { body, .. }
            | Ast::And(body, _)
            | Ast::Not(body, _)
            | Ast::BackRef { body, .. }
            | Ast::Mode { body, .. } => body.walk(visit),
        }
    }
}

/// Parses the text of one rule. `whole` is the span of the string value in the
/// schematic, used when escapes keep finer positions from being exact.
///
/// `v2` turns on the format-2 syntax. Problems are added to `report`; the rule
/// is then `None`.
pub(crate) fn parse<'t>(
    text: &'t Text<'_>,
    whole: Span,
    v2: bool,
    report: &mut Report,
) -> Option<Ast<'t>> {
    let mut parser = RuleParser {
        text: &text.text,
        source: text,
        whole,
        pos: 0,
        depth: 0,
        v2,
    };
    parser.skip_space();
    if parser.at_end() {
        report.error(codes::RULE_SYNTAX, whole, "the rule is empty");
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
            report.error(codes::RULE_SYNTAX, span, message);
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
    /// Format-2 syntax.
    v2: bool,
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
                _ if self.v2 => items.push(self.element()?),
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

    /// A format-2 element: an optional label, then a predicate or a unary.
    fn element(&mut self) -> Result<Ast<'t>, Problem> {
        let start = self.pos;
        let label = self.label();
        if label.is_some() {
            self.skip_space();
        }
        let inner = match self.peek() {
            Some(op @ (b'&' | b'!')) => {
                self.pos += 1;
                if self.peek().is_none_or(|b| b.is_ascii_whitespace()) {
                    return Err(self.unexpected("an element after the predicate"));
                }
                let body = Box::new(self.postfix()?);
                let span = self.span(start, self.pos);
                if op == b'&' {
                    Ast::And(body, span)
                } else {
                    Ast::Not(body, span)
                }
            }
            _ => self.postfix()?,
        };
        Ok(match label {
            None => inner,
            Some((label, label_span)) => Ast::Label {
                label,
                label_span,
                body: Box::new(inner),
                span: self.span(start, self.pos),
            },
        })
    }

    /// `name:` directly before an element (format 2), consumed if present.
    fn label(&mut self) -> Option<(&'t str, Span)> {
        let bytes = self.text.as_bytes();
        let start = self.pos;
        if !bytes
            .get(start)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return None;
        }
        let mut end = start + 1;
        while bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            end += 1;
        }
        if bytes.get(end) != Some(&b':') || bytes.get(end + 1) == Some(&b':') {
            return None;
        }
        self.pos = end + 1;
        Some((&self.text[start..end], self.span(start, end)))
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
            _ => return Ok(self.textref(atom, start)),
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
        Ok(self.textref(ast, start))
    }

    /// `=label` directly after a unary (format 2).
    fn textref(&mut self, ast: Ast<'t>, start: usize) -> Ast<'t> {
        if !self.v2 || self.peek() != Some(b'=') {
            return ast;
        }
        let bytes = self.text.as_bytes();
        let name = self.pos + 1;
        if !bytes
            .get(name)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return ast;
        }
        let mut end = name + 1;
        while bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            end += 1;
        }
        self.pos = end;
        Ast::BackRef {
            body: Box::new(ast),
            label: &self.text[name..end],
            label_span: self.span(name, end),
            span: self.span(start, end),
        }
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
            Some(sigil @ (b'@' | b'%')) if self.v2 => {
                self.pos += 1;
                let name = self.pos;
                while self
                    .peek()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    self.pos += 1;
                }
                if self.pos == name {
                    return Err(self.unexpected(if sigil == b'@' {
                        "a hook name after `@`"
                    } else {
                        "a mode name after `%`"
                    }));
                }
                let word = &self.text[name..self.pos];
                if sigil == b'@' {
                    return Ok(Ast::Hook(word, self.span(start, self.pos)));
                }
                if self.peek() != Some(b'(') {
                    return Err(self.unexpected("`(` after the mode name"));
                }
                let open = self.pos;
                let inner = self.atom()?;
                let _ = open;
                Ok(Ast::Mode {
                    mode: word,
                    body: Box::new(inner),
                    span: self.span(start, self.pos),
                })
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
            multiline: false,
        }
    }

    fn parse_ok(s: &str) -> String {
        let t = text(s);
        let mut report = Report::default();
        let ast = parse(
            &t,
            Span::new(99, 99 + s.len() as u32 + 2),
            false,
            &mut report,
        )
        .unwrap();
        render(&ast)
    }

    fn parse_v2(s: &str) -> String {
        let t = text(s);
        let mut report = Report::default();
        let ast = parse(
            &t,
            Span::new(99, 99 + s.len() as u32 + 2),
            true,
            &mut report,
        );
        match ast {
            Some(ast) => render(&ast),
            None => {
                let err = report.into_error("");
                format!("error: {}", err.diagnostics()[0].message())
            }
        }
    }

    fn parse_err(s: &str) -> (String, Span) {
        let t = text(s);
        let mut report = Report::default();
        assert!(parse(&t, Span::new(0, 1), false, &mut report).is_none());
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
            Ast::Label { label, body, .. } => format!("({label}: {})", render(body)),
            Ast::And(body, _) => format!("(& {})", render(body)),
            Ast::Not(body, _) => format!("(! {})", render(body)),
            Ast::BackRef { body, label, .. } => format!("(= {} {label})", render(body)),
            Ast::Hook(name, _) => format!("@{name}"),
            Ast::Mode { mode, body, .. } => format!("(%{mode} {})", render(body)),
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
    fn test_parse_v2_labels_predicates_and_textrefs() {
        assert_eq!(
            parse_v2("cond:expr then:(a | b)* x"),
            "(seq (cond: expr) (then: (* (alt a b))) x)"
        );
        assert_eq!(
            parse_v2("x (&(',' !')') ',' x)* ','?"),
            "(seq x (* (seq (& (seq ',' (! ')'))) ',' x)) (? ','))"
        );
        assert_eq!(
            parse_v2("end:NAME=tag '>'"),
            "(seq (end: (= NAME tag)) '>')"
        );
        assert_eq!(parse_v2("a::b"), "error: expected an element, found `:`");
        assert_eq!(parse_v2("@typedef x"), "(seq @typedef x)");
        assert_eq!(parse_v2("%jsx(a b)"), "(%jsx (seq a b))");
        assert_eq!(
            parse_v2("& a"),
            "error: expected an element after the predicate, found ` `"
        );
        assert_eq!(
            parse_v2("global:'\\'? parts:IDENT"),
            "(seq (global: (? '\\')) (parts: IDENT))"
        );
        assert_eq!(parse_v2("x:&y"), "(x: (& y))");
        // Format 1 keeps its errors.
        assert_eq!(parse_err("a:b").0, "expected an element, found `:`");
        assert_eq!(parse_err("&a").0, "expected an element, found `&`");
    }

    #[test]
    fn test_parse_spans_map_into_schematic() {
        let t = text("a 'b'");
        let mut report = Report::default();
        let ast = parse(&t, Span::new(0, 1), false, &mut report).unwrap();
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
            multiline: false,
        };
        let mut report = Report::default();
        let ast = parse(&t, Span::new(5, 12), false, &mut report).unwrap();
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
        let ast = parse(&t, Span::new(0, 1), false, &mut report).unwrap();
        let mut seen = Vec::new();
        ast.walk(&mut |n| seen.push(render(n)));
        assert_eq!(seen, ["(* (alt a 'b'))", "(alt a 'b')", "a", "'b'"]);
    }
}
