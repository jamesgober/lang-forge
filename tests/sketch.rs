//! Multi-file sketches (LSF2 §3, ISSUES M13): `Sketch` and
//! `Language::from_sketch`, with diagnostics located in the right file.

use lang_forge::{Language, Sketch};

const ENTRY_HEAD: &str = "[sketch]\nformat = 2\nmodules = [\"parts/lexer.lsf\", \"parts/rules.lsf\"]\n\
                          [language]\nname = \"m\"\nversion = \"1.0.0\"\nstart = \"file\"\n";
const LEXER: &str =
    "[sketch]\nformat = 2\nkind = \"part\"\n[lexer.tokens]\nVAR = { regex = '\\$[a-z]+' }\n";
const RULES: &str = "[sketch]\nformat = 2\nkind = \"part\"\n[rules]\nitem = \"VAR ';'\"\n";

fn sketch(files: &[(&str, &str)]) -> Sketch {
    let mut sketch = Sketch::new();
    for (path, text) in files {
        sketch
            .add(path, *text)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    sketch
}

/// `(code, rendered location + message)` of the first error.
fn first(sketch: &Sketch) -> (String, String) {
    let err = Language::from_sketch(sketch).expect_err("expected an error");
    let d = &err.diagnostics()[0];
    (
        d.code().map(|c| c.to_string()).unwrap_or_default(),
        err.to_string(),
    )
}

#[test]
fn test_parts_merge_into_one_language() {
    let entry = format!("{ENTRY_HEAD}[rules]\nfile = \"item*\"\n");
    let s = sketch(&[
        ("m.lsf", &entry),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", RULES),
    ]);
    assert_eq!(s.len(), 3);
    assert_eq!(s.source_map().iter().count(), 3);
    let lang = Language::from_sketch(&s).expect("forges");
    assert!(!lang.parse("$a; $b;").has_errors());
    assert_eq!(lang.kind_name(lang.root_kind()), "file");
}

#[test]
fn test_one_file_sketch_matches_from_lsf() {
    let text = "[sketch]\nformat = 2\n[language]\nname = \"o\"\nversion = \"1.0.0\"\n[rules]\nfile = \"IDENT*\"\n";
    let lang = Language::from_sketch(&sketch(&[("o.lsf", text)])).expect("forges");
    assert_eq!(
        lang.to_image(),
        Language::from_lsf(text).expect("forges").to_image()
    );
    // A format-1 entry forges as 1.x does.
    let v1 = "[language]\nname = \"o\"\n[rules]\nfile = \"IDENT*\"\n";
    let lang = Language::from_sketch(&sketch(&[("o.lsf", v1)])).expect("forges");
    assert_eq!(lang.format(), 1);
    // Errors carry the file name when they come from a sketch.
    let bad = "[sketch]\nformat = 2\n[language]\nname = \"o\"\nversion = \"1.0.0\"\n[rules]\nfile = \"nope\"\n";
    assert_eq!(
        first(&sketch(&[("o.lsf", bad)])).1,
        "o.lsf:7:9: undefined rule `nope`"
    );
}

#[test]
fn test_errors_point_into_their_part() {
    let entry = format!("{ENTRY_HEAD}[rules]\nfile = \"item*\"\n");
    let rules = "[sketch]\nformat = 2\nkind = \"part\"\n[rules]\nitem = \"VAR ';' extra\"\n";
    let s = sketch(&[
        ("m.lsf", &entry),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", rules),
    ]);
    assert_eq!(
        first(&s),
        (
            "LSF4101".to_owned(),
            "parts/rules.lsf:5:17: undefined rule `extra`".to_owned()
        )
    );
}

#[test]
fn test_key_defined_in_two_files() {
    let entry = format!("{ENTRY_HEAD}[rules]\nfile = \"item*\"\nitem = \"VAR\"\n");
    let s = sketch(&[
        ("m.lsf", &entry),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", RULES),
    ]);
    let err = Language::from_sketch(&s).expect_err("item twice");
    let d = &err.diagnostics()[0];
    assert_eq!(d.code().map(|c| c.to_string()).as_deref(), Some("LSF2002"));
    // The other place is labelled too.
    assert_eq!(d.secondary().len(), 1);
}

#[test]
fn test_module_structure_errors() {
    let entry = format!("{ENTRY_HEAD}[rules]\nfile = \"item*\"\n");
    // A listed module that is not in the sketch.
    let s = sketch(&[("m.lsf", &entry), ("parts/lexer.lsf", LEXER)]);
    assert_eq!(first(&s).0, "LSF2023");
    // A module without `kind = "part"`.
    let not_part = "[sketch]\nformat = 2\n[rules]\nitem = \"VAR ';'\"\n";
    let s = sketch(&[
        ("m.lsf", &entry),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", not_part),
    ]);
    assert_eq!(first(&s).0, "LSF2024");
    // A part with [language].
    let with_language = "[sketch]\nformat = 2\nkind = \"part\"\n[language]\nname = \"x\"\n[rules]\nitem = \"VAR ';'\"\n";
    let s = sketch(&[
        ("m.lsf", &entry),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", with_language),
    ]);
    assert_eq!(first(&s).0, "LSF2022");
    // A part listing parts.
    let nested = "[sketch]\nformat = 2\nkind = \"part\"\nmodules = [\"x.lsf\"]\n[rules]\nitem = \"VAR ';'\"\n";
    let s = sketch(&[
        ("m.lsf", &entry),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", nested),
    ]);
    assert_eq!(first(&s).0, "LSF2003");
    // Rules from two files need `start`.
    let no_start = entry.replace("start = \"file\"\n", "");
    let s = sketch(&[
        ("m.lsf", &no_start),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", RULES),
    ]);
    assert_eq!(first(&s).0, "LSF2006");
    // A module listed twice.
    let twice = entry.replace(
        "\"parts/rules.lsf\"]",
        "\"parts/rules.lsf\", \"parts/rules.lsf\"]",
    );
    let s = sketch(&[
        ("m.lsf", &twice),
        ("parts/lexer.lsf", LEXER),
        ("parts/rules.lsf", RULES),
    ]);
    assert_eq!(first(&s).0, "LSF2005");
    // An empty sketch.
    assert!(Language::from_sketch(&Sketch::new()).is_err());
}

#[test]
fn test_paths_are_portable_and_normalized() {
    let mut s = Sketch::new();
    let code = |r: Result<(), lang_forge::Error>| {
        r.expect_err("refused").diagnostics()[0]
            .code()
            .map(|c| c.to_string())
            .unwrap_or_default()
    };
    assert!(s.is_empty());
    s.add("a/b/../main.lsf", "")
        .expect("normalizes to a/main.lsf");
    assert_eq!(code(s.add("a/./x.lsf", "")), "LSF8002");
    assert_eq!(code(s.add("a/main.lsf", "")), "LSF2005");
    assert_eq!(code(s.add("A/Main.lsf", "")), "LSF8003");
    assert_eq!(code(s.add("../x.lsf", "")), "LSF8001");
    assert_eq!(code(s.add("a/../../x.lsf", "")), "LSF8001");
    assert_eq!(code(s.add("/abs.lsf", "")), "LSF8002");
    assert_eq!(code(s.add("a\\b.lsf", "")), "LSF8002");
    assert_eq!(code(s.add("con.lsf", "")), "LSF8002");
    assert_eq!(code(s.add("a/b:c.lsf", "")), "LSF8002");
    assert_eq!(code(s.add("", "")), "LSF8002");
    assert_eq!(s.len(), 1);
    // Modules are matched by normalized path.
    let entry = "[sketch]\nformat = 2\nmodules = [\"p/../q.lsf\"]\n[language]\nname = \"n\"\nversion = \"1.0.0\"\nstart = \"file\"\n\
                 [rules]\nfile = \"item*\"\n";
    let part = "[sketch]\nformat = 2\nkind = \"part\"\n[rules]\nitem = \"IDENT\"\n";
    let lang =
        Language::from_sketch(&sketch(&[("n.lsf", entry), ("q.lsf", part)])).expect("forges");
    assert!(!lang.parse("a b").has_errors());
}

#[test]
fn test_file_size_budget() {
    let mut s = Sketch::new();
    let huge = "#".repeat((8 << 20) + 1);
    let err = s.add("big.lsf", huge).expect_err("too large");
    assert_eq!(
        err.diagnostics()[0]
            .code()
            .map(|c| c.to_string())
            .as_deref(),
        Some("LSF9001")
    );
    assert!(s.is_empty());
}
