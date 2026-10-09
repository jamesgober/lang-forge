//! Format 2 (LSF2): the document, `[sketch]`, `[language]`, and the lexer —
//! token classes, modes, strings, keywords, numbers, comments, identifiers.

use lang_forge::Language;
use lang_forge::syntax_lang::TokenKind;

/// A format-2 sketch with the given extra sections and `[rules]` body.
fn v2(sections: &str, rules: &str) -> String {
    format!(
        "[sketch]\nformat = 2\n[language]\nname = \"t\"\nversion = \"1.0.0\"\n{sections}\n[rules]\n{rules}"
    )
}

fn forge(sketch: &str) -> Language {
    Language::from_lsf(sketch).unwrap_or_else(|e| {
        let all: Vec<String> = e
            .diagnostics()
            .iter()
            .map(|d| d.message().to_owned())
            .collect();
        panic!("{e}\n{all:#?}")
    })
}

/// The codes and messages of a refused sketch.
fn refuse(sketch: &str) -> Vec<(String, String)> {
    match Language::from_lsf(sketch) {
        Ok(_) => panic!("forged, but expected an error"),
        Err(e) => e
            .diagnostics()
            .iter()
            .map(|d| {
                (
                    d.code().map(|c| c.to_string()).unwrap_or_default(),
                    d.message().to_owned(),
                )
            })
            .collect(),
    }
}

/// `kind:text` for every token, trivia included.
fn tokens(lang: &Language, src: &str) -> Vec<String> {
    lang.lex(src)
        .iter()
        .map(|t| {
            let text = &src[t.span().start().to_usize()..t.span().end().to_usize()];
            format!("{}:{text}", lang.kind_name(*t.kind()))
        })
        .collect()
}

/// `kind:text` for the significant tokens.
fn significant(lang: &Language, src: &str) -> Vec<String> {
    lang.lex(src)
        .iter()
        .filter(|t| !t.is_trivia())
        .map(|t| {
            let text = &src[t.span().start().to_usize()..t.span().end().to_usize()];
            format!("{}:{text}", lang.kind_name(*t.kind()))
        })
        .collect()
}

fn messages(lang: &Language, src: &str) -> Vec<String> {
    lang.parse(src)
        .diagnostics()
        .iter()
        .map(|d| d.message().to_owned())
        .collect()
}

// ----- the document -----

#[test]
fn test_format_selection() {
    // No [sketch]: format 1. [sketch] format = 1: format 1, exactly.
    let v1 = forge("[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n");
    assert_eq!(v1.format(), 1);
    let one = forge("[sketch]\nformat = 1\n[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n");
    assert_eq!(one.format(), 1);
    assert_eq!(one.parse("x").dump(), v1.parse("x").dump());
    assert_eq!(forge(&v2("", "a = \"IDENT\"\n")).format(), 2);
    assert_eq!(
        refuse("[sketch]\nformat = 3\n[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n")[0],
        (
            "LSF1101".into(),
            "sketch format 3 is newer than this lang-forge, which reads formats 1–2".into()
        )
    );
    assert_eq!(
        refuse("[sketch]\nkind = \"part\"\n[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n")[0]
            .0,
        "LSF1102"
    );
    assert_eq!(
        refuse("[sketch]\nformat = 1\nkind = \"language\"\n[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n")[0].0,
        "LSF1005"
    );
    assert_eq!(
        refuse("[sketch]\nformat = \"2\"\n[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n")[0].0,
        "LSF1003"
    );
}

#[test]
fn test_unknown_sections_and_keys_suggest() {
    let found = refuse(&v2("[lexer]\nidents = \"xid\"\n", "a = \"IDENT\"\n"));
    assert_eq!(
        found[0],
        ("LSF1002".into(), "unknown key `idents` in [lexer]".into())
    );
    let err = Language::from_lsf(&v2("[lexr]\n", "a = \"IDENT\"\n")).unwrap_err();
    assert_eq!(
        err.diagnostics()[0]
            .code()
            .map(|c| c.to_string())
            .as_deref(),
        Some("LSF1001")
    );
    assert_eq!(
        err.diagnostics()[0].help().collect::<Vec<_>>(),
        ["did you mean `lexer`?"]
    );
    // Sections other crates consume are accepted, not interpreted.
    let lang = forge(&v2(
        "[semantics.policy]\noverflow = \"promote\"\n[tooling.lsp]\nhover = true\n",
        "a = \"IDENT\"\n",
    ));
    assert_eq!(lang.name(), "t");
}

#[test]
fn test_language_identity_v2() {
    let lang = forge(
        "[sketch]\nformat = 2\n[language]\nname = \"mox\"\ndisplay_name = \"Mox\"\ndescription = \"A language.\"\n\
         version = \"0.1.0-alpha.1+b5\"\nedition = \"2026\"\nextensions = [\"mox\"]\nshebang_names = [\"mox\"]\n\
         [rules]\nfile = \"IDENT*\"\n",
    );
    assert_eq!(lang.display_name(), "Mox");
    assert_eq!(lang.description(), Some("A language."));
    assert_eq!(lang.edition(), Some("2026"));
    assert_eq!(lang.version(), Some("0.1.0-alpha.1+b5"));
    assert_eq!(forge(&v2("", "a = \"IDENT\"\n")).display_name(), "t");

    let bad = |language: &str| {
        refuse(&format!(
            "[sketch]\nformat = 2\n[language]\n{language}\n[rules]\na = \"IDENT\"\n"
        ))
    };
    assert_eq!(bad("name = \"Mox!\"\nversion = \"1.0.0\"")[0].0, "LSF8101");
    assert_eq!(
        bad("name = \"m\"\nversion = \"1.2\"")[0],
        (
            "LSF1103".into(),
            "`version` `1.2` is not a SemVer version".into()
        )
    );
    assert_eq!(
        bad("name = \"m\"")[0].1,
        "missing `version` in [language] (format 2 requires it)"
    );
    assert_eq!(
        bad("name = \"m\"\nversion = \"1.0.0\"\nextensions = [\".m\"]")[0].0,
        "LSF1105"
    );
    assert_eq!(
        bad("name = \"m\"\nversion = \"1.0.0\"\nextensions = [\"m\", \"m\"]")[0].0,
        "LSF1106"
    );
    assert_eq!(
        bad("name = \"m\"\nversion = \"1.0.0\"\ndisplay_name = \"a\u{202E}b\"")[0].0,
        "LSF8201"
    );
    assert_eq!(
        bad("name = \"m\"\nversion = \"1.0.0\"\nedition = \"-x\"")[0].0,
        "LSF1104"
    );
    assert_eq!(
        bad("name = \"m\"\nversion = \"1.0.0\"\nextends = [\"base.lsf\"]")[0].0,
        "LSF1007"
    );
    assert_eq!(
        bad("name = \"m\"\nversion = \"1.0.0\"\nfiles = { x = { start = \"a\" } }")[0].0,
        "LSF1107"
    );
}

#[test]
fn test_rule_names_are_lowercase_in_format_2() {
    assert_eq!(refuse(&v2("", "Stmt = \"IDENT\"\n"))[0].0, "LSF4102");
    assert_eq!(refuse(&v2("", "STMT = \"IDENT\"\n"))[0].0, "LSF4102");
    let lang = forge(&v2("", "file = \"_item*\"\n_item = \"IDENT\"\n"));
    assert!(lang.kind("item").is_none());
}

// ----- token classes -----

#[test]
fn test_regex_token_classes_and_longest_match() {
    let lang = forge(&v2(
        "[lexer.tokens]\nVARIABLE = { regex = '\\$[\\p{XID_Start}_]\\p{XID_Continue}*' }\n\
         LIFETIME = { regex = \"'[a-z]+\" }\nCHAR = { regex = \"'[a-z]'\" }\n",
        "file = \"(VARIABLE | LIFETIME | CHAR | '$')*\"\n",
    ));
    assert_eq!(
        significant(&lang, "$name $ü_1 'a 'b' $"),
        [
            "VARIABLE:$name",
            "VARIABLE:$ü_1",
            "LIFETIME:'a",
            "CHAR:'b'",
            "$:$"
        ]
    );
    assert!(!lang.parse("$x 'a 'b'").has_errors());
}

#[test]
fn test_token_class_errors() {
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { regex = \"a*\" }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF3203"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { regex = \"(a\" }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF3205"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { regex = '\\p{Sm}' }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF3206"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { regex = \"a\", literal = \"b\" }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF3204"
    );
    assert_eq!(
        refuse(&v2("[lexer.tokens]\nX = { }\n", "a = \"X\"\n"))[0].0,
        "LSF3204"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nx = { regex = \"a\" }\n",
            "a = \"IDENT\"\n"
        ))[0]
            .0,
        "LSF3101"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nIDENT = { regex = \"a\" }\n",
            "a = \"IDENT\"\n"
        ))[0]
            .0,
        "LSF4104"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { regex = \"(a|b)*a(a|b){12}\" }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF3201"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { regex = \"a\", priority = 101 }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF1006"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { regex = \"a\", not_followed_by = \"ab\" }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF3111"
    );
    // A class literal that is also a grammar literal: one text, one token.
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nARROW = { literal = \"->\" }\n",
            "a = \"ARROW '->'\"\n"
        ))[0]
            .0,
        "LSF3122"
    );
    // An undefined class in a rule suggests the near one.
    let err = Language::from_lsf(&v2(
        "[lexer.tokens]\nVARIABLE = { regex = 'v' }\n",
        "a = \"VARIABEL\"\n",
    ))
    .unwrap_err();
    assert_eq!(
        err.diagnostics()[0].message(),
        "undefined token class `VARIABEL`"
    );
    assert_eq!(
        err.diagnostics()[0].help().collect::<Vec<_>>(),
        ["did you mean `VARIABLE`?"]
    );
}

#[test]
fn test_token_conditions_and_priority() {
    let lang = forge(&v2(
        "[lexer.tokens]\n\
         TAG = { literal = \"<?x\", not_followed_by = '\\p{XID_Continue}' }\n\
         REGEX = { regex = \"/[a-z]+/\", unless_prev = [\"IDENT\", \"NUMBER\", \"')'\"] }\n\
         HASH = { regex = \"#[a-z]+\", line_start = true }\n\
         LABEL = { regex = \"[a-z]+:\", priority = 5 }\n\
         FIRST = { regex = \"[a-z]+\", column = 1, priority = 1 }\n",
        "file = \"(TAG | REGEX | HASH | LABEL | FIRST | IDENT | NUMBER | '/' | '(' | ')' | '<' | '?' | ':')*\"\n",
    ));
    // `<?x` must not be followed by an identifier character.
    assert_eq!(
        significant(&lang, " <?x <?xy"),
        ["TAG:<?x", "<:<", "?:?", "IDENT:xy"]
    );
    // A regex literal only where a value cannot precede it.
    assert_eq!(
        significant(&lang, " /ab/ a /b/ 1"),
        ["REGEX:/ab/", "IDENT:a", "/:/", "IDENT:b", "/:/", "NUMBER:1"]
    );
    // `#` only at the start of a line.
    assert_eq!(
        significant(&lang, "#if\n x #if")
            .first()
            .map(String::as_str),
        Some("HASH:#if")
    );
    // Equal lengths: priority decides; column conditions count from 1.
    assert_eq!(significant(&lang, "ab cd:"), ["FIRST:ab", "LABEL:cd:"]);
}

#[test]
fn test_trivia_classes_stay_out_of_the_parse() {
    let lang = forge(&v2(
        "[lexer.tokens]\nPRAGMA = { regex = \"@[a-z]+\", trivia = true }\n",
        "file = \"IDENT*\"\n",
    ));
    let parse = lang.parse("a @pragma b");
    assert!(!parse.has_errors());
    let pragma = lang.kind("PRAGMA").expect("a class");
    assert!(pragma.is_trivia());
    assert_eq!(
        parse
            .tree()
            .tokens()
            .filter(|t| *t.kind() == pragma)
            .count(),
        1
    );
}

// ----- modes -----

#[test]
fn test_template_mode_with_text_and_switches() {
    let lang = forge(&v2(
        "[lexer]\ninitial_mode = \"template\"\n\
         [lexer.tokens]\nOPEN = { literal = \"{%\", modes = [\"template\"], action = \"switch main\" }\n\
         CLOSE = { literal = \"%}\", action = \"switch template\" }\n\
         [lexer.modes.template]\ntokens = [\"OPEN\"]\ntext = \"TEXT\"\n",
        "file = \"(TEXT | OPEN IDENT* CLOSE)*\"\n",
    ));
    assert_eq!(
        tokens(&lang, "Hi {% a b %} there %}"),
        [
            "TEXT:Hi ",
            "OPEN:{%",
            "WHITESPACE: ",
            "IDENT:a",
            "WHITESPACE: ",
            "IDENT:b",
            "WHITESPACE: ",
            "CLOSE:%}",
            "TEXT: there %}"
        ]
    );
    assert!(!lang.parse("Hi {% a b %} there").has_errors());
    assert_eq!(
        lang.parse("{% a").diagnostics()[0].message(),
        "expected CLOSE, found the end of the input"
    );
}

#[test]
fn test_push_and_pop_frames() {
    let lang = forge(&v2(
        "[lexer.tokens]\nLB = { literal = \"[[\", action = \"push raw\" }\n\
         RB = { literal = \"]]\", modes = [\"raw\"], action = \"pop\" }\n\
         [lexer.modes.raw]\ntokens = [\"RB\"]\ntext = \"RAW\"\n",
        "file = \"(IDENT | LB RAW? RB)*\"\n",
    ));
    assert_eq!(
        significant(&lang, "a [[ b c ]] d"),
        ["IDENT:a", "LB:[[", "RAW: b c ", "RB:]]", "IDENT:d"]
    );
    // A pushed frame left open at the end of the input is an error by
    // default (`eof = "error"` for pushed frames).
    let parse = lang.parse("a [[ b");
    assert!(
        parse
            .diagnostics()
            .iter()
            .any(|d| d.message() == "the input ends inside `raw`")
    );
    assert!(
        parse
            .diagnostics()
            .iter()
            .any(|d| d.code().is_some_and(|c| c.to_string() == "LF0010"))
    );
}

#[test]
fn test_mode_budget_stops_pushing() {
    let lang = forge(&v2(
        "[lexer]\nmax_mode_depth = 3\n[lexer.tokens]\nOPEN = { literal = \"(\", action = \"push main\" }\n\
         CLOSE = { literal = \")\", action = \"pop\" }\n[lexer.modes.main]\neof = \"ok\"\n",
        "file = \"(OPEN | CLOSE | IDENT)*\"\n",
    ));
    let parse = lang.parse("((((((x");
    let deep = parse
        .diagnostics()
        .iter()
        .filter(|d| d.message() == "lexer modes are nested too deeply")
        .count();
    assert!(deep >= 1);
    assert_eq!(parse.tree().text("((((((x"), Some("((((((x"));
}

#[test]
fn test_mode_errors() {
    assert_eq!(
        refuse(&v2("[lexer]\ninitial_mode = \"nope\"\n", "a = \"IDENT\"\n"))[0].0,
        "LSF3302"
    );
    assert_eq!(
        refuse(&v2("[lexer.modes.Bad]\ntext = \"T\"\n", "a = \"IDENT\"\n"))[0].0,
        "LSF3301"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.modes.a]\ninherit = \"b\"\n[lexer.modes.b]\ninherit = \"a\"\n",
            "a = \"IDENT\"\n"
        ))[0]
            .0,
        "LSF3303"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.tokens]\nX = { literal = \"x\", action = \"jump main\" }\n",
            "a = \"X\"\n"
        ))[0]
            .0,
        "LSF1006"
    );
}

// ----- strings -----

#[test]
fn test_string_classes_as_tokens() {
    let lang = forge(&v2(
        "[lexer.strings.SQ]\nopen = \"'\"\nescape = \"\\\\\"\n\
         [lexer.strings.RAW]\nopen = [\"r\", { capture = \"#*\" }, '\"']\nclose = ['\"', { backref = true }]\nescape = \"\"\n",
        "file = \"STRING*\"\n",
    ));
    assert_eq!(
        significant(&lang, r###"'a\'b' r"x" r#"has "quotes""# r##"x"#y"##"###),
        [
            "SQ:'a\\'b'",
            "RAW:r\"x\"",
            "RAW:r#\"has \"quotes\"\"#",
            "RAW:r##\"x\"#y\"##"
        ]
    );
    // `STRING` means any string class.
    assert!(!lang.parse(r##"'a' r#"b"#"##).has_errors());
    // `rust` is an identifier, not a raw string.
    assert_eq!(significant(&lang, "rust"), ["IDENT:rust"]);
}

#[test]
fn test_interpolation_and_embedded_tokens_build_nodes() {
    let lang = forge(&v2(
        "[lexer.tokens]\nVAR = { regex = '\\$[a-z]+' }\n\
         [lexer.strings.DQ]\nopen = '\"'\nescape = \"\\\\\"\nescape_tokens = true\n\
         interpolate = [{ open = \"{\", when_next = '\\$', close = \"}\", rule = \"expr\" }]\n\
         embedded = [{ token = \"DQ_VAR\", regex = '\\$[a-z]+' }]\n",
        "file = \"items:expr*\"\n[rules.expr]\noperand = \"VAR | NUMBER | DQ | '{' expr '}'\"\nlevels = [{ left = [\"+\"] }]\n",
    ));
    let src = r#""a $b {$c + {1}} \n { x}""#;
    let parse = lang.parse(src);
    assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
    let dump = parse.dump();
    for line in [
        "open:DQ_OPEN@0..1",
        "parts:DQ_TEXT@1..3 \"a \"",
        "parts:DQ_VAR@3..5 \"$b\"",
        "parts:DQ_INTERP@6..16",
        "open:DQ_INTERP_OPEN@6..7",
        "value:expr@7..15",
        "close:DQ_INTERP_CLOSE@15..16",
        "parts:DQ_ESCAPE@17..19 \"\\\\n\"",
        // `{` not followed by `$` is text.
        "parts:DQ_TEXT@19..24 \" { x}\"",
        "close:DQ_CLOSE@24..25",
    ] {
        assert!(dump.contains(line), "missing {line} in\n{dump}");
    }
}

#[test]
fn test_unterminated_node_strings_close_at_the_end() {
    let lang = forge(&v2(
        "[lexer.strings.DQ]\nopen = '\"'\ninterpolate = [{ open = \"{\", close = \"}\", rule = \"value\" }]\n",
        "file = \"DQ*\"\nvalue = \"NUMBER\"\n",
    ));
    let src = "\"abc {1";
    let parse = lang.parse(src);
    let messages: Vec<&str> = parse.diagnostics().iter().map(|d| d.message()).collect();
    assert_eq!(messages, ["unterminated string"]);
    assert_eq!(parse.tree().text(src), Some(src));
    let dump = parse.dump();
    assert!(dump.contains("close:DQ_INTERP_CLOSE@7..7"), "{dump}");
    assert!(dump.contains("close:DQ_CLOSE@7..7"), "{dump}");
    // Without `multiline`, the line end ends the string.
    let parse = lang.parse("\"ab\ncd\"");
    assert_eq!(parse.diagnostics()[0].message(), "unterminated string");
}

#[test]
fn test_heredocs_close_at_an_indented_line_start() {
    let lang = forge(&v2(
        "[lexer.strings.HEREDOC]\nopen = [\"<<<\", { regex = '[ \\t]*' }, { capture = '[A-Za-z_][A-Za-z_0-9]*' }, { newline = true }]\n\
         close = [{ backref = true }]\nclose_at = \"line-start-indented\"\nclose_not_followed_by = '[A-Za-z_0-9]'\n\
         multiline = true\nescape = \"\"\n",
        "file = \"(HEREDOC ';')*\"\n",
    ));
    let src = "<<<EOT\n  line EOT\n  EOTX\n  EOT;\n<<< X\nX;";
    assert_eq!(
        significant(&lang, src),
        [
            "HEREDOC:<<<EOT\n  line EOT\n  EOTX\n  EOT",
            ";:;",
            "HEREDOC:<<< X\nX",
            ";:;"
        ]
    );
    assert!(!lang.parse(src).has_errors());
}

#[test]
fn test_string_class_errors() {
    // The array form and named classes cannot meet: the document itself
    // refuses the second `strings` (a duplicate key).
    assert_eq!(
        refuse(&v2(
            "[lexer]\nstrings = ['\"']\n[lexer.strings.SQ]\nopen = \"'\"\n",
            "a = \"STRING\"\n"
        ))[0]
            .0,
        "LSF0001"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.strings.H]\nopen = [\"<<\", { capture = \"[a-z]+\" }]\n",
            "a = \"H\"\n"
        ))[0]
            .0,
        "LSF3114"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.strings.S]\nopen = \"'\"\nescape = \"ab\"\n",
            "a = \"S\"\n"
        ))[0]
            .0,
        "LSF3115"
    );
    assert!(
        refuse(&v2(
            "[lexer.strings.S]\nopen = '\"'\ninterpolate = [{ open = \"{\", close = \"}\", rule = \"nope\" }]\n",
            "a = \"S\"\n"
        ))
        .iter()
        .any(|(c, _)| c == "LSF4101")
    );
    // A greedy part that eats the next part's first character can never
    // match: a warning.
    let lang = forge(&v2(
        "[lexer.strings.S]\nopen = [{ regex = \"[a-z]*\" }, \"x\"]\nclose = \"x\"\n",
        "a = \"S?\"\n",
    ));
    assert!(
        lang.warnings()
            .iter()
            .any(|d| d.code().is_some_and(|c| c.to_string() == "LSF3116"))
    );
    assert_eq!(
        // A node-form string generates `DQ_OPEN`; a class of that name clashes.
        refuse(&v2(
            "[lexer.strings.DQ]\nopen = '\"'\ninterpolate = [{ open = \"{\", close = \"}\", rule = \"a\" }]\n\
             [lexer.tokens]\nDQ_OPEN = { regex = \"q\" }\n",
            "a = \"DQ\"\n"
        ))[0]
            .0,
        "LSF3118"
    );
}

// ----- keywords -----

#[test]
fn test_contextual_keywords() {
    let lang = forge(&v2(
        "[lexer.keywords]\ncontextual = [\"async\", \"await\"]\n",
        "file = \"stmt*\"\nstmt = \"'async'? 'fn' name:IDENT ';' | expr ';'\"\n\
         [rules.expr]\noperand = \"IDENT | NUMBER\"\nlevels = [{ prefix = [\"await\"] }]\n",
    ));
    // Lexed as identifiers.
    assert_eq!(significant(&lang, "async fn")[0], "IDENT:async");
    // Matched as keywords where the grammar asks; the tree records the
    // keyword's kind.
    let parse = lang.parse("async fn async; await x; async; await;");
    assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
    let dump = parse.dump();
    assert!(dump.contains("async@0..5"), "{dump}");
    assert!(dump.contains("name:IDENT@9..14 \"async\""), "{dump}");
    assert!(dump.contains("op:await@16..21"), "{dump}");
    // `async;` and `await;` are plain identifiers there.
    assert!(dump.contains("IDENT@25..30 \"async\""), "{dump}");
    assert!(dump.contains("IDENT@32..37 \"await\""), "{dump}");
}

#[test]
fn test_contextual_keywords_do_not_commit_repetitions() {
    // A repetition of a contextual keyword commits only on that word, not on
    // every identifier (the overlap check and the parser agree on this).
    let lang = forge(&v2(
        "[lexer.keywords]\ncontextual = [\"readonly\"]\n",
        "file = \"param*\"\nparam = \"mods:'readonly'* ty:IDENT name:NUMBER\"\n",
    ));
    assert!(!lang.parse("Foo 1 readonly Bar 2").has_errors());
}

#[test]
fn test_reserved_and_default_contextual_keywords() {
    let lang = forge(&v2(
        "[lexer.keywords]\nreserved = [\"goto\"]\n",
        "file = \"IDENT*\"\n",
    ));
    // `goto` is a keyword although no rule uses it: not an identifier.
    assert_eq!(significant(&lang, "goto x"), ["goto:goto", "IDENT:x"]);
    assert_eq!(
        messages(&lang, "goto")[0],
        "expected an identifier, found `goto`"
    );

    // SQL style: every keyword contextual except the reserved ones.
    let lang = forge(&v2(
        "[lexer.keywords]\ndefault = \"contextual\"\nreserved = [\"select\"]\n",
        "file = \"'select' cols:IDENT ('from' table:IDENT)?\"\n",
    ));
    assert_eq!(
        significant(&lang, "select from from x"),
        ["select:select", "IDENT:from", "IDENT:from", "IDENT:x"]
    );
    assert!(!lang.parse("select from from x").has_errors());
}

#[test]
fn test_case_insensitive_keywords() {
    let lang = forge(&v2(
        "[lexer.keywords]\ncase = \"ascii-insensitive\"\n",
        "file = \"stmt*\"\nstmt = \"'select' IDENT ';'\"\n",
    ));
    assert_eq!(
        significant(&lang, "SELECT a; Select b; select c;"),
        [
            "select:SELECT",
            "IDENT:a",
            ";:;",
            "select:Select",
            "IDENT:b",
            ";:;",
            "select:select",
            "IDENT:c",
            ";:;"
        ]
    );
    assert!(!lang.parse("SELECT a; Select b;").has_errors());
}

#[test]
fn test_keyword_errors() {
    assert_eq!(
        refuse(&v2(
            "[lexer.keywords]\ncontextual = [\"+=\"]\n",
            "a = \"IDENT '+='\"\n"
        ))[0]
            .0,
        "LSF3108"
    );
    let lang = forge(&v2(
        "[lexer.keywords]\ndefault = \"contextual\"\ncontextual = [\"x\"]\n",
        "a = \"'x' IDENT\"\n",
    ));
    assert!(
        lang.warnings()
            .iter()
            .any(|d| d.code().is_some_and(|c| c.to_string() == "LSF3109"))
    );
}

// ----- numbers -----

#[test]
fn test_number_options() {
    let lang = forge(&v2(
        "[lexer.numbers]\nradix = [\"0x\", \"0b\"]\nradix_case = \"any\"\nseparators = \"_\"\nleading_dot = true\n\
         trailing_dot = true\nleading_zeros = \"error\"\nsuffixes = [\"u8\", \"i64\"]\nhex_floats = true\n",
        "file = \"(NUMBER | IDENT | '.')*\"\n",
    ));
    assert_eq!(
        significant(&lang, "0XFF 0b1_0 .5 1. 2u8 0x1.8p3 1e5"),
        [
            "NUMBER:0XFF",
            "NUMBER:0b1_0",
            "NUMBER:.5",
            "NUMBER:1.",
            "NUMBER:2u8",
            "NUMBER:0x1.8p3",
            "NUMBER:1e5"
        ]
    );
    // `0o` is not an allowed radix here: `0` then an identifier run-on.
    assert_eq!(
        messages(&lang, "0o7"),
        ["invalid suffix `o7` on number literal"]
    );
    assert_eq!(
        messages(&lang, "0755"),
        ["decimal literal `0755` has a leading zero; write `0o755` for octal"]
    );
    assert_eq!(
        messages(&lang, "1__0"),
        ["a digit separator must sit between two digits"]
    );
    assert_eq!(
        messages(&lang, "2px"),
        ["invalid suffix `px` on number literal"]
    );
    assert!(
        lang.warnings()
            .iter()
            .any(|d| d.code().is_some_and(|c| c.to_string() == "LSF3120"))
    );
}

#[test]
fn test_number_option_errors() {
    assert_eq!(
        refuse(&v2(
            "[lexer.numbers]\nseparators = \"__\"\n",
            "a = \"NUMBER\"\n"
        ))[0]
            .0,
        "LSF3119"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.numbers]\nsuffixes = [\"U8\"]\n",
            "a = \"NUMBER\"\n"
        ))[0]
            .0,
        "LSF3121"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer.numbers]\nradix = [\"0z\"]\n",
            "a = \"NUMBER\"\n"
        ))[0]
            .0,
        "LSF1006"
    );
}

// ----- comments, identifiers, shebang -----

#[test]
fn test_comment_tables() {
    let lang = forge(&v2(
        "[lexer]\nline_comments = [{ open = \"//\", stop_before = [\"?>\"] }, { open = \"#\", not_followed_by = '\\[' }]\n\
         block_comments = [{ open = \"/**\", close = \"*/\", doc = true, not_followed_by = \"/\" }, [\"/*\", \"*/\"]]\n",
        "file = \"(IDENT | '#[' | '?>')*\"\n",
    ));
    assert_eq!(
        tokens(&lang, "a // x ?> #c\n#[ /** d */ /**/"),
        [
            "IDENT:a",
            "WHITESPACE: ",
            "COMMENT:// x ",
            "?>:?>",
            "WHITESPACE: ",
            "COMMENT:#c",
            "WHITESPACE:\n",
            "#[:#[",
            "WHITESPACE: ",
            "DOC_COMMENT:/** d */",
            "WHITESPACE: ",
            "COMMENT:/**/"
        ]
    );
    assert_eq!(
        refuse(&v2(
            "[lexer]\nline_comments = [{ open = \"rem\" }]\n",
            "a = \"IDENT\"\n"
        ))[0]
            .0,
        "LSF3110"
    );
    assert_eq!(
        refuse(&v2(
            "[lexer]\nline_comments = [{ open = \"//\", stop_before = [\"%>\"] }]\n",
            "a = \"IDENT\"\n"
        ))[0]
            .0,
        "LSF3112"
    );
}

#[test]
fn test_identifier_table_and_shebang() {
    let lang = forge(&v2(
        "[lexer]\nshebang = true\nidentifiers = { style = \"ascii\", extra_start = \"$\", extra_continue = \"-\" }\n",
        "file = \"IDENT*\"\n",
    ));
    assert_eq!(
        tokens(&lang, "#!/usr/bin/env t\n$a-b c")[0],
        "SHEBANG:#!/usr/bin/env t"
    );
    assert_eq!(significant(&lang, "$a-b c"), ["IDENT:$a-b", "IDENT:c"]);
    let nfc = forge(&v2(
        "[lexer]\nidentifiers = { normalize = \"require-nfc\" }\n",
        "file = \"IDENT*\"\n",
    ));
    // `e` + COMBINING ACUTE ACCENT is not in NFC; `é` (U+00E9) is.
    assert_eq!(
        messages(&nfc, "e\u{301}x"),
        ["identifier `e\u{301}x` is not in Unicode normal form C"]
    );
    assert!(messages(&nfc, "\u{e9}x").is_empty());
    let confusables = forge(&v2(
        "[lexer]\nidentifiers = { confusables = \"warn\" }\n",
        "file = \"IDENT*\"\n",
    ));
    assert_eq!(
        confusables.warnings()[0]
            .code()
            .map(|c| c.to_string())
            .as_deref(),
        Some("LSF1007")
    );
}

#[test]
fn test_brackets_count_inside_holes() {
    let lang = forge(&v2(
        "[lexer]\nbrackets = [[\"{\", \"}\"]]\n\
         [lexer.strings.T]\nopen = \"`\"\ninterpolate = [{ open = \"${\", close = \"}\", rule = \"value\" }]\n",
        "file = \"T*\"\nvalue = \"'{' IDENT ':' NUMBER '}' | IDENT\"\n",
    ));
    let parse = lang.parse("`a ${ {x: 1} } b`");
    assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
    assert_eq!(
        refuse(&v2(
            "[lexer]\nbrackets = [[\"<\", \">\"]]\n",
            "a = \"IDENT\"\n"
        ))[0]
            .0,
        "LSF3105"
    );
}

#[test]
fn test_unsupported_keys_are_refused_not_ignored() {
    for (section, what) in [
        ("[lexer.split]\n\">>\" = [\">\", \">\"]\n", "split"),
        ("[lexer.columns]\ntrivia = [[73, 80]]\n", "columns"),
        ("[layout]\nstyle = \"hook\"\n", "layout hook"),
        ("[lexer.tokens]\nX = { hook = \"scan\" }\n", "scanner hook"),
    ] {
        let found = refuse(&v2(section, "a = \"IDENT\"\n"));
        assert!(
            found.iter().any(|(c, _)| c == "LSF1007"),
            "{what}: {found:?}"
        );
    }
}
