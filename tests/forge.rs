//! Forging: what a schematic may say, and every way it can be refused.

use lang_forge::diag_lang::{Renderer, SourceMap};
use lang_forge::{Error, Language};

/// Forges, expecting failure, and returns the messages in order.
fn refuse(schematic: &str) -> Vec<String> {
    match Language::from_lsf(schematic) {
        Ok(lang) => panic!("forged `{}` but expected an error", lang.name()),
        Err(err) => err
            .diagnostics()
            .iter()
            .map(|d| d.message().to_owned())
            .collect(),
    }
}

/// The first message only.
fn refuse_one(schematic: &str) -> String {
    refuse(schematic).remove(0)
}

/// A schematic with the given `[rules]` body and a valid header.
fn rules(body: &str) -> String {
    format!("[language]\nname = \"t\"\n[lexer]\nstrings = ['\"']\n[rules]\n{body}")
}

fn help_of(schematic: &str) -> Vec<String> {
    let err = Language::from_lsf(schematic).expect_err("refused");
    err.diagnostics()[0].help().map(str::to_owned).collect()
}

#[test]
fn test_forge_identity_settings_are_exposed() {
    let lang = Language::from_lsf(
        "[language]\nname = \"iron\"\nversion = \"0.3.0-alpha\"\nextensions = [\"fe\", \"iron\"]\n\
         [rules]\nfile = \"IDENT*\"\n[capabilities]\ninclude = [\"borrow\", \"thermal\"]\n",
    )
    .expect("forges");
    assert_eq!(lang.name(), "iron");
    assert_eq!(lang.version(), Some("0.3.0-alpha"));
    assert_eq!(lang.extensions().len(), 2);
    assert_eq!(lang.extensions().collect::<Vec<_>>(), ["fe", "iron"]);
    assert_eq!(
        lang.capabilities().collect::<Vec<_>>(),
        ["borrow", "thermal"]
    );
}

#[test]
fn test_forge_optional_settings_default() {
    let lang = Language::from_lsf("[language]\nname = \"min\"\n[rules]\nfile = \"NUMBER*\"\n")
        .expect("forges");
    assert_eq!(lang.version(), None);
    assert_eq!(lang.extensions().len(), 0);
    assert_eq!(lang.capabilities().len(), 0);
}

#[test]
fn test_forge_from_str_matches_from_lsf() {
    let text = "[language]\nname = \"x\"\n[rules]\nx = \"IDENT\"\n";
    let parsed: Language = text.parse().expect("forges");
    assert_eq!(
        parsed.name(),
        Language::from_lsf(text).expect("forges").name()
    );
}

#[test]
fn test_language_is_send_sync_and_clone() {
    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync::<Language>();
    assert_send_sync::<Error>();
}

#[test]
fn test_forge_kinds_cover_every_category() {
    let lang = Language::from_lsf(&rules(
        "file = \"_item*\"\n_item = \"decl | NUMBER\"\ndecl = \"'let' IDENT '=' expr ';'\"\n\
         [rules.expr]\noperand = \"NUMBER\"\nlevels = [{ left = [\"+\"] }, { prefix = [\"-\"], node = \"neg\" }]\n",
    ))
    .expect("forges");
    for name in [
        "file",
        "decl",
        "expr",
        "let",
        "=",
        ";",
        "+",
        "-",
        "binary",
        "neg",
        "IDENT",
        "NUMBER",
        "STRING",
        "NEWLINE",
        "WHITESPACE",
        "COMMENT",
        "UNKNOWN",
        "ERROR",
    ] {
        let kind = lang
            .kind(name)
            .unwrap_or_else(|| panic!("no kind `{name}`"));
        assert_eq!(lang.kind_name(kind), name);
    }
    // Hidden rules build no node; EOF never appears in a tree.
    assert!(lang.kind("_item").is_none());
    assert!(lang.kind("item").is_none());
    assert!(lang.kind("EOF").is_none());
    assert!(lang.kind("prefix").is_none());
}

#[test]
fn test_forge_kind_from_other_language_is_unknown() {
    let small =
        Language::from_lsf("[language]\nname = \"a\"\n[rules]\na = \"NUMBER\"\n").expect("forges");
    let big = Language::from_lsf(&rules(
        "r1 = \"r2\"\nr2 = \"r3\"\nr3 = \"r4\"\nr4 = \"r5\"\nr5 = \"r6\"\nr6 = \"'x' 'y' 'z' 'w'\"\n",
    ))
    .expect("forges");
    let r6 = big.kind("r6").expect("rule");
    assert_eq!(small.kind_name(r6), "<unknown>");
}

#[test]
fn test_forge_start_rule_can_be_named() {
    let lang = Language::from_lsf(
        "[language]\nname = \"s\"\nstart = \"file\"\n[rules]\nitem = \"NUMBER\"\nfile = \"item*\"\n",
    )
    .expect("forges");
    let parse = lang.parse("1 2");
    assert_eq!(lang.kind_name(*parse.tree().kind()), "file");
}

#[test]
fn test_forge_reports_every_problem_in_source_order() {
    let messages =
        refuse("[language]\nname = \"t\"\nflavour = \"x\"\n[rules]\na = \"b c\"\nd = \"'1x'\"\n");
    assert_eq!(
        messages,
        [
            "unknown key `flavour` in [language]",
            "undefined rule `b`",
            "undefined rule `c`",
            "literal `1x` starts with a digit, which the lexer reads as a number",
        ]
    );
}

#[test]
fn test_forge_error_display_locates_first_problem() {
    let err = Language::from_lsf("[language]\nname = \"t\"\n[rules]\nfile = \"stmt*\"\n")
        .expect_err("refused");
    assert_eq!(err.to_string(), "4:9: undefined rule `stmt`");
    let err = Language::from_lsf("[language]\nname = \"t\"\n[rules]\nfile = \"a b\"\n")
        .expect_err("refused");
    assert_eq!(
        err.to_string(),
        "4:9: undefined rule `a` (and 1 more error)"
    );
}

#[test]
fn test_forge_errors_render_with_diag_lang() {
    let schematic = "[language]\nname = \"t\"\n[rules]\nprogram = \"stmt*\"\nstmts = \"NUMBER\"\n";
    let err = Language::from_lsf(schematic).expect_err("refused");
    let mut map = SourceMap::new();
    map.add("t.lsf", schematic).expect("fits");
    let text = Renderer::new().render(&err.diagnostics()[0], &map);
    assert!(text.contains("error: undefined rule `stmt`"), "{text}");
    assert!(text.contains("t.lsf:4:12"), "{text}");
    assert!(text.contains("help: did you mean `stmts`?"), "{text}");
}

// ----- the [language] table -----

#[test]
fn test_forge_requires_language_and_rules_tables() {
    assert_eq!(
        refuse("[rules]\na = \"NUMBER\"\n"),
        ["missing [language] table"]
    );
    assert_eq!(
        refuse("[language]\nname = \"t\"\n"),
        ["missing [rules] table"]
    );
    assert_eq!(
        refuse(""),
        ["missing [language] table", "missing [rules] table"]
    );
}

#[test]
fn test_forge_requires_a_name() {
    assert_eq!(
        refuse_one("[language]\n[rules]\na = \"NUMBER\"\n"),
        "missing `name` in [language]"
    );
    assert_eq!(
        refuse_one("[language]\nname = \"  \"\n[rules]\na = \"NUMBER\"\n"),
        "the language name is empty"
    );
}

#[test]
fn test_forge_rejects_mistyped_settings() {
    assert_eq!(
        refuse_one("[language]\nname = 7\n[rules]\na = \"NUMBER\"\n"),
        "`name` must be a string, found a number"
    );
    assert_eq!(
        refuse_one("[language]\nname = \"t\"\nextensions = \"t\"\n[rules]\na = \"NUMBER\"\n"),
        "`extensions` must be an array, found a string"
    );
    assert_eq!(
        refuse_one("[language]\nname = \"t\"\nextensions = [1]\n[rules]\na = \"NUMBER\"\n"),
        "`extensions` must hold strings, found a number"
    );
    assert_eq!(
        refuse_one("language = \"t\"\n[rules]\na = \"NUMBER\"\n"),
        "`language` must be a table, found a string"
    );
    assert_eq!(
        refuse_one(
            "[language]\nname = \"t\"\n[lexer]\nnewlines = \"yes\"\n[rules]\na = \"NUMBER\"\n"
        ),
        "`newlines` must be a boolean, found a string"
    );
}

#[test]
fn test_forge_rejects_unknown_sections_and_keys() {
    assert_eq!(
        refuse_one("[language]\nname = \"t\"\n[grammar]\n[rules]\na = \"NUMBER\"\n"),
        "unknown section `grammar`"
    );
    let schematic =
        "[language]\nname = \"t\"\n[lexer]\ncomments = [\"#\"]\n[rules]\na = \"NUMBER\"\n";
    assert_eq!(refuse_one(schematic), "unknown key `comments` in [lexer]");
    assert!(help_of(schematic)[0].contains("`line_comments`"));
}

#[test]
fn test_forge_rejects_dotted_extensions() {
    let schematic = "[language]\nname = \"t\"\nextensions = [\".t\"]\n[rules]\na = \"NUMBER\"\n";
    assert_eq!(refuse_one(schematic), "extension `.t` starts with a dot");
    assert_eq!(help_of(schematic), ["write `t`"]);
}

#[test]
fn test_forge_rejects_undefined_or_hidden_start() {
    assert_eq!(
        refuse_one("[language]\nname = \"t\"\nstart = \"fil\"\n[rules]\nfile = \"NUMBER\"\n"),
        "start rule `fil` is not defined"
    );
    assert_eq!(
        refuse_one("[language]\nname = \"t\"\n[rules]\n_file = \"NUMBER\"\n"),
        "the start rule `_file` is hidden"
    );
}

// ----- the [lexer] table -----

#[test]
fn test_forge_rejects_bad_lexer_settings() {
    let lexer = |body: &str| {
        format!("[language]\nname = \"t\"\n[lexer]\n{body}\n[rules]\na = \"NUMBER\"\n")
    };
    assert_eq!(
        refuse_one(&lexer("identifiers = \"unicode\"")),
        "unknown identifier style `unicode`"
    );
    assert_eq!(
        refuse_one(&lexer("line_comments = [\"\"]")),
        "a comment delimiter is empty"
    );
    assert_eq!(
        refuse_one(&lexer("block_comments = [\"/*\"]")),
        "a block comment is a pair of delimiters"
    );
    assert_eq!(
        refuse_one(&lexer("block_comments = [[\"/*\", \"*/\", \"x\"]]")),
        "a block comment is a pair of delimiters"
    );
    assert_eq!(
        refuse_one(&lexer("strings = [\"\"]")),
        "a string delimiter is empty"
    );
    assert_eq!(
        refuse_one(&lexer("strings = [{ close = \"'\" }]")),
        "a string needs an `open` delimiter"
    );
    assert_eq!(
        refuse_one(&lexer("strings = [{ open = \"'\", escape = \"ab\" }]")),
        "escape `ab` is not a single character"
    );
    assert_eq!(
        refuse_one(&lexer("strings = [{ open = \"'\", quote = \"x\" }]")),
        "unknown key `quote` in a string"
    );
    assert_eq!(
        refuse_one(&lexer("strings = [true]")),
        "expected a string delimiter, found a boolean"
    );
}

#[test]
fn test_forge_rejects_unreachable_delimiters() {
    let lexer = |body: &str| {
        format!("[language]\nname = \"t\"\n[lexer]\n{body}\n[rules]\na = \"NUMBER\"\n")
    };
    assert_eq!(
        refuse_one(&lexer("line_comments = [\"rem\"]")),
        "comment delimiter `rem` starts like an identifier, which the lexer reads first"
    );
    assert_eq!(
        refuse_one(&lexer("strings = [\"1\"]")),
        "string delimiter `1` starts with a digit, which begins a number"
    );
    assert_eq!(
        refuse_one(&lexer("line_comments = [\"/ /\"]")),
        "comment delimiter `/ /` contains whitespace"
    );
}

#[test]
fn test_forge_rejects_delimiters_used_twice() {
    assert_eq!(
        refuse_one(
            "[language]\nname = \"t\"\n[lexer]\nline_comments = [\"#\"]\n[rules]\na = \"'#' NUMBER\"\n"
        ),
        "`#` is used for two different things"
    );
    assert_eq!(
        refuse_one(
            "[language]\nname = \"t\"\n[lexer]\nline_comments = [\"--\"]\nstrings = [\"--\"]\n[rules]\na = \"NUMBER\"\n"
        ),
        "`--` is used for two different things"
    );
}

// ----- the [rules] table -----

#[test]
fn test_forge_requires_at_least_one_rule() {
    assert_eq!(
        refuse("[language]\nname = \"t\"\n[rules]\n"),
        ["[rules] declares no rules"]
    );
}

#[test]
fn test_forge_rejects_malformed_rule_text() {
    assert_eq!(refuse_one(&rules("a = \"\"")), "the rule is empty");
    assert_eq!(
        refuse_one(&rules("a = \"NUMBER |\"")),
        "expected an element, found the end of the rule"
    );
    assert_eq!(refuse_one(&rules("a = \"(NUMBER\"")), "unclosed `(`");
    assert_eq!(refuse_one(&rules("a = \"NUMBER)\"")), "unmatched `)`");
    assert_eq!(refuse_one(&rules("a = \"'x\"")), "unterminated literal");
    assert_eq!(
        refuse_one(&rules("a = \"NUMBER ; NUMBER\"")),
        "expected an element, found `;`"
    );
    assert_eq!(
        refuse_one(&rules("a = [\"NUMBER\"]")),
        "rule `a` must be a string or a table, found an array"
    );
}

#[test]
fn test_forge_rejects_bad_rule_names() {
    assert_eq!(
        refuse_one(&rules("\"my rule\" = \"NUMBER\"")),
        "`my rule` is not a valid rule name"
    );
    assert_eq!(
        refuse_one(&rules("_ = \"NUMBER\"")),
        "`_` is not a valid rule name"
    );
    assert_eq!(
        refuse_one(&rules("IDENT = \"NUMBER\"")),
        "`IDENT` is the name of a built-in kind and cannot name a rule"
    );
}

#[test]
fn test_forge_suggests_close_rule_names() {
    let schematic = rules("program = \"statment*\"\nstatement = \"NUMBER ';'\"");
    assert_eq!(refuse_one(&schematic), "undefined rule `statment`");
    assert_eq!(help_of(&schematic), ["did you mean `statement`?"]);
}

#[test]
fn test_forge_rejects_bad_literals() {
    assert_eq!(
        refuse_one(&rules("a = \"''\"")),
        "a literal cannot be empty"
    );
    assert_eq!(
        refuse_one(&rules("a = \"'a b'\"")),
        "literal `a b` contains whitespace"
    );
    assert_eq!(
        refuse_one(&rules("a = \"'x+'\"")),
        "literal `x+` mixes identifier and symbol characters"
    );
    assert_eq!(
        refuse_one(&rules("a = \"'IDENT'\"")),
        "keyword `IDENT` has the name of a built-in kind"
    );
    assert_eq!(
        refuse_one(&rules("a = \"'b'\"\nb = \"NUMBER\"")),
        "`b` is both a keyword and a rule"
    );
}

#[test]
fn test_forge_literal_shape_follows_identifier_style() {
    // `ü` begins an identifier in XID mode, so `über` is a keyword; in ASCII
    // mode it is not an identifier at all and `ü` cannot begin a symbol.
    assert!(Language::from_lsf(&rules("a = \"'über' NUMBER\"")).is_ok());
    let ascii =
        "[language]\nname = \"t\"\n[lexer]\nidentifiers = \"ascii\"\n[rules]\na = \"'λ' NUMBER\"\n";
    let lang = Language::from_lsf(ascii).expect("`λ` is a symbol in ASCII mode");
    assert!(!lang.parse("λ 1").has_errors());
}

#[test]
fn test_forge_rejects_misused_token_classes() {
    let no_strings = "[language]\nname = \"t\"\n[rules]\na = \"STRING\"\n";
    assert_eq!(
        refuse_one(no_strings),
        "the grammar uses STRING, but [lexer] declares no strings"
    );
    assert_eq!(
        refuse_one(&rules("a = \"NEWLINE\"")),
        "the grammar uses NEWLINE, but line breaks are whitespace"
    );
    assert_eq!(
        refuse_one(&rules("a = \"COMMENT\"")),
        "`COMMENT` is trivia, which the parser never sees"
    );
    assert_eq!(
        refuse_one(&rules("a = \"ERROR\"")),
        "`ERROR` is not a token a rule can match"
    );
}

#[test]
fn test_forge_rejects_left_recursion() {
    assert_eq!(
        refuse_one(&rules("sum = \"sum '+' NUMBER | NUMBER\"")),
        "rule `sum` is left-recursive: sum → sum"
    );
    assert_eq!(
        refuse_one(&rules("a = \"b 'x'\"\nb = \"c? a\"\nc = \"'y'\"")),
        "rule `a` is left-recursive: a → b → a"
    );
    let schematic = rules("[rules.e]\noperand = \"e '.' IDENT | IDENT\"\nlevels = []");
    assert_eq!(refuse_one(&schematic), "rule `e` is left-recursive: e → e");
}

#[test]
fn test_forge_rejects_repetition_of_nothing() {
    assert_eq!(
        refuse_one(&rules("a = \"NUMBER? *\"")),
        "`*` must follow the element it applies to"
    );
    assert_eq!(
        refuse_one(&rules("a = \"(NUMBER?)*\"")),
        "`*` repeats something that can match nothing, so it would never stop"
    );
    assert_eq!(
        refuse_one(&rules("a = \"b+\"\nb = \"IDENT*\"")),
        "`+` repeats something that can match nothing, so it would never stop"
    );
}

#[test]
fn test_forge_rejects_unreachable_alternatives() {
    assert_eq!(
        refuse(&rules("a = \"IDENT | IDENT '(' ')'\"")),
        ["this alternative can never match"]
    );
    assert_eq!(
        refuse(&rules("a = \"NUMBER | NUMBER\"")),
        ["this alternative can never match"]
    );
    // Longer alternatives first is fine.
    assert!(Language::from_lsf(&rules("a = \"IDENT '(' ')' | IDENT\"")).is_ok());
}

// ----- expression rules -----

#[test]
fn test_forge_rejects_bad_expression_rules() {
    assert_eq!(
        refuse_one(&rules("[rules.e]\nlevels = []")),
        "an expression rule needs an `operand`"
    );
    assert_eq!(
        refuse_one(&rules("[rules.e]\noperand = \"NUMBER\"\nflavour = 1")),
        "unknown key `flavour` in an expression rule"
    );
    assert_eq!(
        refuse_one(&rules("[rules.e]\noperand = \"NUMBER?\"\nlevels = []")),
        "the operand of expression rule `e` can match nothing"
    );
    assert_eq!(
        refuse_one(&rules("[rules.e]\noperand = \"NUMBER\"\nlevels = [\"+\"]")),
        "an operator level must be a table, found a string"
    );
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ node = \"x\" }]"
        )),
        "an operator level names no operators"
    );
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ left = [] }]"
        )),
        "an operator level names no operators"
    );
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ left = [\"+\"], right = [\"^\"] }]"
        )),
        "an operator level has exactly one of `left`, `right`, `none`, `prefix`, or `postfix`"
    );
}

#[test]
fn test_forge_rejects_conflicting_operators() {
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ left = [\"+\"] }, { postfix = [\"+\"] }]"
        )),
        "`+` is already an infix or postfix operator at an earlier level"
    );
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ prefix = [\"-\"] }, { prefix = [\"-\"] }]"
        )),
        "`-` is already a prefix operator at an earlier level"
    );
    // The same token may be prefix in one position and infix in the other.
    assert!(
        Language::from_lsf(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ left = [\"-\"] }, { prefix = [\"-\"] }]"
        ))
        .is_ok()
    );
}

#[test]
fn test_forge_rejects_bad_node_names() {
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ left = [\"+\"], node = \"_sum\" }]"
        )),
        "a node name cannot start with `_`"
    );
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ left = [\"+\"], node = \"ERROR\" }]"
        )),
        "`ERROR` is the name of a built-in kind and cannot name a node"
    );
    assert_eq!(
        refuse_one(&rules(
            "[rules.e]\noperand = \"NUMBER | 'binary'\"\nlevels = [{ left = [\"+\"] }]"
        )),
        "node name `binary` is also a keyword"
    );
}

#[test]
fn test_forge_node_names_may_be_shared() {
    let lang = Language::from_lsf(&rules(
        "[rules.e]\noperand = \"NUMBER\"\nlevels = [{ left = [\"+\"], node = \"op\" }, { left = [\"*\"], node = \"op\" }]",
    ))
    .expect("forges");
    let op = lang.kind("op").expect("node");
    let parse = lang.parse("1 + 2 * 3");
    assert_eq!(
        parse
            .tree()
            .descendants()
            .filter(|n| *n.kind() == op)
            .count(),
        2
    );
}

// ----- NOML and capabilities -----

#[test]
fn test_forge_rejects_dynamic_noml() {
    assert_eq!(
        refuse_one("[language]\nname = env(\"LANG\")\n[rules]\na = \"NUMBER\"\n"),
        "NOML function calls such as `env(...)` are not allowed in a schematic"
    );
    assert_eq!(
        refuse_one("[language]\nname = @string(\"x\")\n[rules]\na = \"NUMBER\"\n"),
        "NOML native types (`@...`) are not allowed in a schematic"
    );
}

#[test]
fn test_forge_passes_noml_syntax_errors_through() {
    assert_eq!(
        refuse_one("[language\nname = \"t\"\n"),
        "expected `]` to close the table header, found the end of the line"
    );
    assert_eq!(
        refuse_one("[language]\nname = \"t\nx\"\n"),
        "unterminated string"
    );
}

#[test]
fn test_forge_rejects_bad_capabilities() {
    let caps = |body: &str| {
        format!("[language]\nname = \"t\"\n[rules]\na = \"NUMBER\"\n[capabilities]\n{body}\n")
    };
    assert_eq!(
        refuse_one(&caps("include = [\"x\", \"x\"]")),
        "capability `x` is included twice"
    );
    assert_eq!(
        refuse_one(&caps("include = [\"\"]")),
        "a capability name is empty"
    );
    assert_eq!(
        refuse_one(&caps("include = [\"a \", \"a\"]")),
        "capability name `a ` has surrounding whitespace"
    );
    assert_eq!(
        refuse_one(&caps("require = [\"x\"]")),
        "unknown key `require` in [capabilities]"
    );
}

#[test]
fn test_forge_multiline_rules_and_comments_in_schematic() {
    let lang = Language::from_lsf(
        "# A comment.\n[language]\nname = \"m\" # trailing comment\n\n[rules]\nstmt = \"\"\"\n    'a' NUMBER\n  | 'b' IDENT\n\"\"\"\n",
    )
    .expect("forges");
    assert!(!lang.parse("b x").has_errors());
}

#[test]
fn test_forge_escaped_rule_text_still_reports_errors() {
    // An escape inside the rule string breaks the one-to-one mapping between
    // rule text and schematic offsets; the error then points at the whole
    // string rather than a wrong spot.
    let schematic = "[language]\nname = \"t\"\n[rules]\na = \"\\u0062 NUMBER\"\n";
    let err = Language::from_lsf(schematic).expect_err("`b` is undefined");
    assert_eq!(err.diagnostics()[0].message(), "undefined rule `b`");
    let span = err.diagnostics()[0].primary().span();
    assert_eq!(
        &schematic[span.start().to_usize()..span.end().to_usize()],
        "\"\\u0062 NUMBER\""
    );
}

#[test]
fn test_forge_readme_schematic_tour() {
    // The annotated schematic in README.md must forge as written.
    let readme = include_str!("../README.md");
    let section = readme
        .find("## A schematic, table by table")
        .expect("section");
    let start = readme[section..].find("```toml\n").expect("block") + section + "```toml\n".len();
    let end = readme[start..].find("```").expect("end") + start;
    let lang = Language::from_lsf(&readme[start..end]).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(lang.name(), "mini");
    let parse = lang.parse("fn f(a, b) { let c = a + b * 2; return c; }\nf(1, #\"raw\nstring\"#);");
    assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
}
