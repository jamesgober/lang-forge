//! A command-line checker for any forged language: forge a schematic, parse a
//! source file with it, and report every problem with source context.
//!
//! ```text
//! cargo run --example check                                   # a built-in sample
//! cargo run --example check -- schematic.lsf file.src          # your own files
//! cargo run --example check -- --tree schematic.lsf file.src   # also print the tree
//! ```
//!
//! With no files, it checks a short `mini` program that contains two
//! deliberate mistakes, using `schematics/mini.lsf`.

use std::process::ExitCode;

use lang_forge::Language;
use lang_forge::diag_lang::{Diagnostic, Renderer, SourceMap};

const SAMPLE: &str = "\
fn area(w, h) {
    return w * h;
}

let size = area(3, 4;
if size > 10 {
    print(\"big\");
} else {
    print(\"small\")
}
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let show_tree = args.iter().any(|a| a == "--tree");
    args.retain(|a| a != "--tree");

    let (schematic_name, schematic, source_name, source) = match args.as_slice() {
        [] => (
            String::from("mini.lsf"),
            String::from(include_str!("schematics/mini.lsf")),
            String::from("sample.mini"),
            String::from(SAMPLE),
        ),
        [schematic, source] => match (
            std::fs::read_to_string(schematic),
            std::fs::read_to_string(source),
        ) {
            (Ok(s), Ok(t)) => (schematic.clone(), s, source.clone(), t),
            (Err(e), _) => return fail(&format!("cannot read {schematic}: {e}")),
            (_, Err(e)) => return fail(&format!("cannot read {source}: {e}")),
        },
        _ => return fail("usage: check [--tree] [<schematic.lsf> <source>]"),
    };

    let lang = match Language::from_lsf(&schematic) {
        Ok(lang) => lang,
        Err(err) => {
            render(&schematic_name, &schematic, err.diagnostics());
            return fail(&format!(
                "{schematic_name}: the schematic has {} problem(s)",
                err.diagnostics().len()
            ));
        }
    };

    let parse = lang.parse(&source);
    if show_tree {
        print!("{}", parse.dump());
    }
    render(&source_name, &source, parse.diagnostics());
    if parse.has_errors() {
        let count = parse.diagnostics().len();
        return fail(&format!(
            "{source_name}: {count} problem(s) found by `{}`",
            lang.name()
        ));
    }
    println!(
        "{source_name}: ok ({} v{})",
        lang.name(),
        lang.version().unwrap_or("?")
    );
    ExitCode::SUCCESS
}

/// Draws each diagnostic under the line it points at.
fn render(name: &str, text: &str, diagnostics: &[Diagnostic]) {
    let mut map = SourceMap::new();
    if map.add(name, text).is_err() {
        return;
    }
    let renderer = Renderer::new();
    for diagnostic in diagnostics {
        eprintln!("{}", renderer.render(diagnostic, &map));
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}
