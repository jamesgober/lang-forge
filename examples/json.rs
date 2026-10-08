//! A JSON validator forged from `schematics/json.lsf`: each document is parsed
//! and either accepted or shown with its first error in context.
//!
//! ```text
//! cargo run --example json
//! ```

use lang_forge::Language;
use lang_forge::diag_lang::{Renderer, SourceMap};

const DOCUMENTS: [&str; 5] = [
    r#"{"name": "lang-forge", "tags": ["parser", "generator"], "stable": true}"#,
    r#"[1, -2.5, 3e10, {"nested": {"deep": [null]}}]"#,
    r#"{"missing": }"#,
    r#"{"trailing": [1, 2,]}"#,
    r#"{"a": 1} {"b": 2}"#,
];

fn main() -> Result<(), lang_forge::Error> {
    let json = Language::from_lsf(include_str!("schematics/json.lsf"))?;
    let renderer = Renderer::new();

    for (i, document) in DOCUMENTS.iter().enumerate() {
        let parse = json.parse(document);
        if !parse.has_errors() {
            let values = parse
                .tree()
                .descendants()
                .filter(|n| json.kind_name(*n.kind()) == "value")
                .count();
            println!("document {i}: valid ({values} values)");
            continue;
        }
        println!("document {i}: {} error(s)", parse.diagnostics().len());
        let mut map = SourceMap::new();
        if map.add(format!("document-{i}.json"), *document).is_ok() {
            println!("{}", renderer.render(&parse.diagnostics()[0], &map));
        }
    }
    Ok(())
}
