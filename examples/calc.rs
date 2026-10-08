//! A calculator in a few dozen lines: forge the language from
//! `schematics/calc.lsf`, parse a program, and evaluate it by walking the
//! syntax tree the forged parser builds.
//!
//! ```text
//! cargo run --example calc
//! ```

use std::collections::HashMap;
use std::process::ExitCode;

use lang_forge::diag_lang::{Renderer, SourceMap};
use lang_forge::syntax_lang::{Element, Node, TokenKind};

use lang_forge::{Kind, Language, Parse};

const PROGRAM: &str = "\
let r = 2;
let area = 3 * r ^ 2;   # three times r squared
(area - 2) / 5;
-r ^ 2;                 # the power binds first: -(r ^ 2)
area % 5;
";

fn main() -> ExitCode {
    let lang = match Language::from_lsf(include_str!("schematics/calc.lsf")) {
        Ok(lang) => lang,
        Err(err) => {
            eprintln!("calc.lsf: {err}");
            return ExitCode::FAILURE;
        }
    };

    let parse = lang.parse(PROGRAM);
    if parse.has_errors() {
        report(&parse);
        return ExitCode::FAILURE;
    }

    let mut calc = Calculator::new(&lang);
    for stmt in parse.tree().child_nodes() {
        match calc.statement(stmt, PROGRAM) {
            Ok(line) => println!("{line}"),
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

/// Renders every diagnostic under the line it points at.
fn report(parse: &Parse<'_>) {
    let mut map = SourceMap::new();
    if map.add("program.calc", parse.source()).is_err() {
        return;
    }
    let renderer = Renderer::new();
    for diagnostic in parse.diagnostics() {
        eprintln!("{}", renderer.render(diagnostic, &map));
    }
}

/// The kinds the evaluator dispatches on, looked up once.
struct Calculator {
    binary: Kind,
    prefix: Kind,
    number: Kind,
    ident: Kind,
    let_: Kind,
    variables: HashMap<String, f64>,
}

impl Calculator {
    fn new(lang: &Language) -> Self {
        let kind = |name: &str| {
            lang.kind(name)
                .unwrap_or_else(|| panic!("calc.lsf defines `{name}`"))
        };
        Self {
            binary: kind("binary"),
            prefix: kind("prefix"),
            number: kind("NUMBER"),
            ident: kind("IDENT"),
            let_: kind("let"),
            variables: HashMap::new(),
        }
    }

    /// Evaluates one `stmt` node; a binding is remembered and echoed.
    fn statement(&mut self, stmt: &Node<Kind>, src: &str) -> Result<String, String> {
        let parts: Vec<&Element<Kind>> = significant(stmt).collect();
        if parts.first().map(|p| *p.kind()) == Some(self.let_) {
            // let IDENT = expr ;
            let name = text(parts[1], src).to_owned();
            let value = self.value(parts[3], src)?;
            let _ = self.variables.insert(name.clone(), value);
            return Ok(format!("{name} = {value}"));
        }
        // expr ;
        let value = self.value(parts[0], src)?;
        Ok(format!("{} = {value}", text(parts[0], src).trim()))
    }

    fn value(&self, element: &Element<Kind>, src: &str) -> Result<f64, String> {
        match element {
            Element::Token(token) if *token.kind() == self.number => text(element, src)
                .replace('_', "")
                .parse()
                .map_err(|e| format!("bad number: {e}")),
            Element::Token(token) if *token.kind() == self.ident => {
                let name = text(element, src);
                self.variables
                    .get(name)
                    .copied()
                    .ok_or_else(|| format!("`{name}` is not defined"))
            }
            Element::Token(_) => Err(format!("unexpected `{}`", text(element, src))),
            Element::Node(node) => self.node(node, src),
        }
    }

    fn node(&self, node: &Node<Kind>, src: &str) -> Result<f64, String> {
        let parts: Vec<&Element<Kind>> = significant(node).collect();
        let kind = *node.kind();
        if kind == self.binary {
            let (lhs, op, rhs) = (
                self.value(parts[0], src)?,
                text(parts[1], src),
                self.value(parts[2], src)?,
            );
            return Ok(match op {
                "+" => lhs + rhs,
                "-" => lhs - rhs,
                "*" => lhs * rhs,
                "/" => lhs / rhs,
                "%" => lhs % rhs,
                _ => lhs.powf(rhs),
            });
        }
        if kind == self.prefix {
            return Ok(-self.value(parts[1], src)?);
        }
        // An `expr` node holds one operand or operator node; a `group`
        // node is `( expr )`.
        match parts.as_slice() {
            [only] => self.value(only, src),
            [_, inner, _] => self.value(inner, src),
            _ => Err(format!(
                "cannot evaluate `{}`",
                node.text(src).unwrap_or("")
            )),
        }
    }
}

/// A node's children without whitespace and comments.
fn significant(node: &Node<Kind>) -> impl Iterator<Item = &Element<Kind>> {
    node.children()
        .filter(|child| !matches!(child, Element::Token(token) if token.kind().is_trivia()))
}

fn text<'s>(element: &Element<Kind>, src: &'s str) -> &'s str {
    let span = element.span();
    &src[span.start().to_usize()..span.end().to_usize()]
}
