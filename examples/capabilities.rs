//! Capabilities: the schematic names the passes a language includes, and the
//! language assembles them, in order, from a registry of available passes.
//!
//! `schematics/mini.lsf` includes `unused-variables`; this example registers a
//! pass by that name (and one the language does not ask for), builds the
//! pipeline, and runs it over a program.
//!
//! ```text
//! cargo run --example capabilities
//! ```

use std::collections::HashMap;

use lang_forge::diag_lang::{Diagnostic, Label, Renderer, Severity, SourceMap};
use lang_forge::pass_lang::{Outcome, Pass, PassError};
use lang_forge::syntax_lang::Span;
use lang_forge::{Capability, Language, Parse};

const PROGRAM: &str = "\
fn main() {
    let width = 4;
    let height = 3;
    let unused = 99;
    print(width * height);
}
";

/// Warns about `let` bindings whose name is never mentioned again.
struct UnusedVariables;

impl<'a> Pass<Parse<'a>> for UnusedVariables {
    fn name(&self) -> &'static str {
        "unused-variables"
    }

    fn run(&mut self, parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
        let lang = parse.language();
        let (let_, ident) = match (lang.kind("let"), lang.kind("IDENT")) {
            (Some(l), Some(i)) => (l, i),
            _ => return Err(PassError::new("the language has no `let` bindings")),
        };
        let src = parse.source();
        let text = |span: Span| &src[span.start().to_usize()..span.end().to_usize()];

        // Every identifier use, counted by name; the name after `let` is a
        // declaration, not a use.
        let tokens: Vec<_> = parse.tree().tokens().filter(|t| !t.is_trivia()).collect();
        let mut uses: HashMap<&str, usize> = HashMap::new();
        let mut declared: Vec<Span> = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            if *token.kind() != ident {
                continue;
            }
            if i > 0 && *tokens[i - 1].kind() == let_ {
                declared.push(token.span());
            } else {
                *uses.entry(text(token.span())).or_default() += 1;
            }
        }

        let unused: Vec<Span> = declared
            .into_iter()
            .filter(|span| !uses.contains_key(text(*span)))
            .collect();
        for span in &unused {
            let message = format!("variable `{}` is never used", text(*span));
            parse.report(
                Diagnostic::new(
                    Severity::Warning,
                    message,
                    Label::new(*span, "declared here"),
                )
                .with_help("remove it, or use it"),
            );
        }
        Ok(Outcome::Unchanged)
    }
}

/// A pass for a different language; the pipeline leaves it out.
struct TabsNotSpaces;

impl<'a> Pass<Parse<'a>> for TabsNotSpaces {
    fn name(&self) -> &'static str {
        "tabs-not-spaces"
    }

    fn run(&mut self, _parse: &mut Parse<'a>) -> Result<Outcome, PassError> {
        Ok(Outcome::Unchanged)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mini = Language::from_lsf(include_str!("schematics/mini.lsf"))?;
    println!(
        "{} includes: {}",
        mini.name(),
        mini.capabilities().collect::<Vec<_>>().join(", ")
    );

    // A registry may hold passes for many languages.
    let registry: Vec<Capability> = vec![Box::new(TabsNotSpaces), Box::new(UnusedVariables)];
    let mut pipeline = mini.pipeline(registry)?;
    println!("pipeline: {} pass(es)", pipeline.len());

    let mut parse = mini.parse(PROGRAM);
    let report = pipeline.run(&mut parse)?;
    println!("ran {} pass(es)", report.runs().len());

    let mut map = SourceMap::new();
    let _ = map.add("main.mini", PROGRAM)?;
    for diagnostic in parse.diagnostics() {
        println!("{}", Renderer::new().render(diagnostic, &map));
    }
    Ok(())
}
