//! Format 2 (LSF2): the grammar — labels and fields, predicates, back
//! references, position assertions, Pratt extensions, rule options, the
//! overlap check, layout, injections, `[ast]`, and diagnostic codes.

use lang_forge::syntax_lang::Node;
use lang_forge::{Cardinality, Kind, Language};

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

fn messages(lang: &Language, src: &str) -> Vec<String> {
    lang.parse(src)
        .diagnostics()
        .iter()
        .map(|d| d.message().to_owned())
        .collect()
}

fn codes(lang: &Language, src: &str) -> Vec<String> {
    lang.parse(src)
        .diagnostics()
        .iter()
        .map(|d| d.code().map(|c| c.to_string()).unwrap_or_default())
        .collect()
}

fn ok(lang: &Language, src: &str) -> String {
    let parse = lang.parse(src);
    assert!(
        !parse.has_errors(),
        "{src:?}: {:?}\n{}",
        parse.diagnostics(),
        parse.dump()
    );
    parse.dump()
}

/// `label=kind` for every labelled child of `node`.
fn labelled(lang: &Language, node: &Node<Kind>) -> Vec<String> {
    (0..node.len())
        .filter_map(|i| {
            let label = lang.field_label(node, i)?;
            let child = node.children().nth(i)?;
            Some(format!(
                "{}={}",
                lang.label_name(label)?,
                lang.kind_name(*child.kind())
            ))
        })
        .collect()
}

// ----- labels and fields -----

#[test]
fn test_labels_on_edges_and_kind_equality() {
    let lang = forge(&v2(
        "",
        "file = \"stmt*\"\nstmt = \"'let' name:IDENT ('=' value:NUMBER)? ';'\"\n",
    ));
    let parse = lang.parse("let a = 1; let b;");
    let stmts: Vec<&Node<Kind>> = parse.tree().child_nodes().collect();
    assert_eq!(labelled(&lang, stmts[0]), ["name=IDENT", "value=NUMBER"]);
    assert_eq!(labelled(&lang, stmts[1]), ["name=IDENT"]);
    // Labels ride on the kind but do not change what kind it is.
    let ident = lang.kind("IDENT").expect("builtin");
    let labelled_ident = stmts[0]
        .children()
        .find(|c| c.kind().label().is_some())
        .expect("a labelled child");
    assert_eq!(*labelled_ident.kind(), ident);
    assert_eq!(labelled_ident.kind().unlabelled(), ident);
    assert_eq!(ident.label(), None);
    assert_eq!(
        lang.label_id("name").and_then(|l| lang.label_name(l)),
        Some("name")
    );
    assert_eq!(lang.label_id("nope"), None);
    // Format 1 has no labels.
    let v1 = forge("[language]\nname = \"a\"\n[rules]\na = \"IDENT\"\n");
    assert_eq!(v1.label_name(0), None);
}

#[test]
fn test_labels_on_groups_and_nodes() {
    let lang = forge(&v2(
        "",
        "file = \"call*\"\ncall = \"callee:path args:('(' NUMBER* ')') ';'\"\npath = \"IDENT ('.' IDENT)*\"\n",
    ));
    let parse = lang.parse("a.b(1 2);");
    let call = parse.tree().child_nodes().next().expect("a call");
    let found = labelled(&lang, call);
    assert_eq!(
        found,
        [
            "callee=path",
            "args=(",
            "args=NUMBER",
            "args=NUMBER",
            "args=)"
        ]
    );
}

#[test]
fn test_fields_cardinality_and_kinds() {
    let lang = forge(&v2(
        "",
        "file = \"item*\"\nitem = \"fn_def | alias\"\n\
         fn_def = \"'fn' name:IDENT '(' (params:IDENT (',' params:IDENT)*)? ')' ret:(NUMBER | IDENT)? ';'\"\n\
         alias = \"'type' name:IDENT '=' value:IDENT ';'\"\n",
    ));
    let fn_def = lang.kind("fn_def").expect("a rule");
    let fields: Vec<(String, Cardinality, Vec<String>)> = lang
        .fields(fn_def)
        .map(|f| {
            (
                f.name().to_owned(),
                f.cardinality(),
                f.kinds().map(|k| lang.kind_name(k).to_owned()).collect(),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            (
                "name".to_owned(),
                Cardinality::One,
                vec!["IDENT".to_owned()]
            ),
            (
                "params".to_owned(),
                Cardinality::Many,
                vec!["IDENT".to_owned()]
            ),
            (
                "ret".to_owned(),
                Cardinality::Optional,
                vec!["IDENT".to_owned(), "NUMBER".to_owned()]
            ),
        ]
    );
    // A kind with no labelled children has no fields.
    assert_eq!(lang.fields(lang.kind("item").expect("a rule")).count(), 0);
    for field in lang.fields(fn_def) {
        assert_eq!(lang.label_name(field.label()), Some(field.name()));
    }
}

#[test]
fn test_pratt_operator_nodes_are_labelled() {
    let lang = forge(&v2(
        "",
        "file = \"expr\"\n[rules.expr]\noperand = \"NUMBER | IDENT\"\n\
         levels = [{ left = [\"+\"] }, { prefix = [\"-\"] }, { postfix = [\"!\"] }]\n",
    ));
    let parse = lang.parse("-a + 1!");
    let root = parse.tree();
    let expr = root.child_nodes().next().expect("expr");
    let binary = expr.child_nodes().next().expect("binary");
    assert_eq!(
        labelled(&lang, binary),
        ["lhs=prefix", "op=+", "rhs=postfix"]
    );
    let prefix = binary.child_nodes().next().expect("prefix");
    assert_eq!(labelled(&lang, prefix), ["op=-", "operand=IDENT"]);
}

#[test]
fn test_labels_errors() {
    assert_eq!(refuse(&v2("", "a = \"Name:IDENT\"\n"))[0].0, "LSF4103");
    // The labels of operator nodes are reserved everywhere, so a field name
    // means one thing in every node.
    assert_eq!(refuse(&v2("", "a = \"lhs:IDENT\"\n"))[0].0, "LSF4106");
    let lang = forge(&v2("", "a = \"left:IDENT\"\n"));
    assert_eq!(lang.fields(lang.kind("a").expect("rule")).count(), 1);
}

// ----- predicates, back references, position assertions -----

#[test]
fn test_and_and_not_predicates() {
    let lang = forge(&v2(
        "",
        "file = \"stmt*\"\nstmt = \"!'end' IDENT ';' | 'end' &EOF\"\n",
    ));
    ok(&lang, "a; b; end");
    assert_eq!(
        messages(&lang, "a; end b;"),
        ["expected the end of the input, found identifier `b`"]
    );
}

#[test]
fn test_predicates_do_not_consume_or_build() {
    let lang = forge(&v2(
        "",
        "file = \"item*\"\nitem = \"&(IDENT '(') call | IDENT ';'\"\ncall = \"IDENT '(' ')'\"\n",
    ));
    let dump = ok(&lang, "f() x;");
    assert!(dump.contains("call@0..3"), "{dump}");
    assert!(!dump.contains("call@4"), "{dump}");
}

#[test]
fn test_back_references_match_the_labelled_text() {
    let lang = forge(&v2(
        "",
        "file = \"block*\"\nblock = \"'begin' name:IDENT IDENT* 'end' IDENT=name\"\n",
    ));
    ok(&lang, "begin a x y end a begin b end b");
    assert_eq!(messages(&lang, "begin a end b"), ["`b` does not match `a`"]);
}

#[test]
fn test_eof_line_start_and_newline_before() {
    let lang = forge(&v2(
        "",
        "file = \"stmt* EOF\"\nstmt = \"IDENT args:(!NL_BEFORE IDENT)* (';' | &NL_BEFORE | &EOF)\"\n",
    ));
    let parse = lang.parse("print a b\nprint c; print d");
    assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
    assert_eq!(parse.tree().child_nodes().count(), 3);

    let lang = forge(&v2("", "file = \"(LINE_START '#' IDENT | IDENT)*\"\n"));
    ok(&lang, "#a b\n  #c");
    assert!(lang.parse("b #c").has_errors());
}

#[test]
fn test_predicate_errors() {
    // A label on a zero-width predicate labels nothing.
    assert_eq!(refuse(&v2("", "a = \"x:&IDENT IDENT\"\n"))[0].0, "LSF4105");
    // A back reference names an earlier label on a token.
    assert_eq!(
        refuse(&v2("", "a = \"IDENT=name name:IDENT\"\n"))[0].0,
        "LSF4110"
    );
    assert_eq!(
        refuse(&v2("", "a = \"name:IDENT b=name\"\nb = \"IDENT IDENT\"\n"))[0].0,
        "LSF4109"
    );
}

// ----- Pratt extensions -----

#[test]
fn test_pratt_contextual_operators_and_prec() {
    let lang = forge(&v2(
        "[lexer.keywords]\ncontextual = [\"and\", \"not\"]\n",
        "file = \"stmt*\"\nstmt = \"expr ';'\"\n[rules.expr]\noperand = \"IDENT | NUMBER\"\n\
         levels = [{ left = [\"and\"] }, { prefix = [\"not\"] }, { left = [\"+\"] }]\n",
    ));
    let dump = ok(&lang, "not a and b + 1; and; not;");
    assert!(dump.contains("op:and@6..9"), "{dump}");
    assert!(dump.contains("op:not@0..3"), "{dump}");
    // `and` / `not` alone are identifiers.
    assert!(dump.contains("IDENT@17..20 \"and\""), "{dump}");
    assert!(dump.contains("IDENT@22..25 \"not\""), "{dump}");
}

// ----- rule options and the overlap check -----

#[test]
fn test_overlap_check_and_allow() {
    // Dangling `else` style: the continuations agree, no report.
    forge(&v2(
        "",
        "file = \"stmt*\"\nstmt = \"'if' IDENT block else_part*\"\n\
         else_part = \"'else' 'if' IDENT block | 'else' block\"\nblock = \"'{' '}'\"\n",
    ));

    // `tail*` commits on IDENT, but IDENT can also follow it: `if a b ;`
    // is rejected although the grammar appears to allow it.
    let rules = |allow: &str| {
        format!(
            "file = \"stmt*\"\n[rules.stmt]\nrule = \"'if' IDENT tail* IDENT ';'\"\n{allow}\n[rules.tail]\nrule = \"IDENT ':'\"\n"
        )
    };
    let found = refuse(&v2("", &rules("")));
    assert_eq!(found[0].0, "LSF4301", "{found:?}");
    assert_eq!(
        found[0].1,
        "input `IDENT ;` is rejected: this repetition commits on `IDENT` and then needs `:`"
    );
    // The rule holding the repetition acknowledges it.
    let lang = forge(&v2("", &rules("allow = [\"overlap\"]")));
    assert!(
        lang.warnings()
            .iter()
            .all(|d| d.code().is_none_or(|c| c.to_string() != "LSF4301"))
    );
    // `[sketch.checks] overlap = "warn"` turns it into a warning.
    let warned = v2("", &rules("")).replace(
        "[sketch]\nformat = 2\n",
        "[sketch]\nformat = 2\nchecks = { overlap = \"warn\" }\n",
    );
    let lang = forge(&warned);
    assert!(
        lang.warnings()
            .iter()
            .any(|d| d.code().is_some_and(|c| c.to_string() == "LSF4301"))
    );
    // The fix the spec suggests: a predicate.
    forge(&v2(
        "",
        "file = \"stmt*\"\nstmt = \"'if' IDENT (&(IDENT ':') tail)* IDENT ';'\"\ntail = \"IDENT ':'\"\n",
    ));
}

#[test]
fn test_unused_rules_and_tokens_are_warnings() {
    let lang = forge(&v2(
        "[lexer.tokens]\nUNUSED = { regex = \"@@\" }\n",
        "file = \"IDENT*\"\nlonely = \"NUMBER\"\n",
    ));
    let codes: Vec<String> = lang
        .warnings()
        .iter()
        .filter_map(|d| d.code().map(|c| c.to_string()))
        .collect();
    assert!(codes.contains(&"LSF4302".to_owned()), "{codes:?}");
    assert!(codes.contains(&"LSF3401".to_owned()), "{codes:?}");
    let denied = v2(
        "[lexer.tokens]\nUNUSED = { regex = \"@@\" }\n",
        "file = \"IDENT*\"\n",
    )
    .replace(
        "[sketch]\nformat = 2\n",
        "[sketch]\nformat = 2\nchecks = { unused_token = \"deny\" }\n",
    );
    assert_eq!(refuse(&denied)[0].0, "LSF3401");
}

#[test]
fn test_sync_tokens_recover() {
    let lang = forge(&v2(
        "",
        "file = \"stmt*\"\n[rules.stmt]\nrule = \"'let' IDENT '=' NUMBER ';'\"\nsync = [\"';'\"]\n",
    ));
    let parse = lang.parse("let a = ; let b = 2;");
    assert_eq!(parse.diagnostics().len(), 1, "{:?}", parse.diagnostics());
    assert_eq!(parse.tree().child_nodes().count(), 2);
}

// ----- left recursion -----

#[test]
fn test_left_recursion_is_refused_with_the_cycle() {
    let found = refuse(&v2("", "a = \"b 'x'\"\nb = \"c\"\nc = \"a | 'y'\"\n"));
    assert_eq!(found[0].0, "LSF4305", "{found:?}");
    assert!(found[0].1.contains("a → b → c → a"), "{found:?}");
}

// ----- layout -----

#[test]
fn test_indentation_layout() {
    let lang = forge(&v2(
        "[layout]\nstyle = \"indent\"\nopen_after = [\":\"]\nimplicit_join = [[\"(\", \")\"]]\n\
         newlines = { mode = \"significant\" }\n",
        "file = \"stmt*\"\nstmt = \"simple | compound\"\nsimple = \"(IDENT | '(' | ')')+ NEWLINE\"\n\
         compound = \"'if' IDENT ':' NEWLINE INDENT stmt+ DEDENT\"\n",
    ));
    let src = "if a:\n    x\n    if b:\n        y\n    z (\n w)\nq\n";
    let parse = lang.parse(src);
    assert!(
        !parse.has_errors(),
        "{:?}\n{}",
        parse.diagnostics(),
        parse.dump()
    );
    let kinds: Vec<String> = lang
        .lex(src)
        .iter()
        .filter(|t| matches!(lang.kind_name(*t.kind()), "INDENT" | "DEDENT"))
        .map(|t| lang.kind_name(*t.kind()).to_owned())
        .collect();
    assert_eq!(kinds, ["INDENT", "INDENT", "DEDENT", "DEDENT"]);
    // An inconsistent dedent is reported with a lexical code.
    let bad = lang.parse("if a:\n    x\n  y\n");
    assert!(
        bad.diagnostics()
            .iter()
            .any(|d| d.code().is_some_and(|c| c.to_string().starts_with("LF0")))
    );
}

// ----- injections -----

#[test]
fn test_self_injection_of_embedded_tokens() {
    let lang = forge(&v2(
        "[lexer.tokens]\nVAR = { regex = '\\$[a-z]+' }\n\
         [lexer.strings.DQ]\nopen = '\"'\ninterpolate = [{ open = \"{\", close = \"}\", rule = \"expr\" }]\n\
         embedded = [{ token = \"DQ_VAR\", regex = '\\$[a-z]+(\\.[a-z]+)?', parse = \"var\" }]\n",
        "file = \"expr*\"\nexpr = \"DQ | var\"\nvar = \"VAR ('.' IDENT)?\"\n",
    ));
    let src = "\"hi $a.b\" $c";
    let parse = lang.parse(src);
    assert!(!parse.has_errors(), "{:?}", parse.diagnostics());
    let injections = parse.injections();
    assert_eq!(injections.len(), 1);
    let one = &injections[0];
    assert_eq!(one.language(), "self");
    assert!(!one.is_editor());
    assert_eq!(
        &src[one.span().start().to_usize()..one.span().end().to_usize()],
        "$a.b"
    );
    let tree = one.tree().expect("parsed");
    assert_eq!(lang.kind_name(*tree.kind()), "var");
}

#[test]
fn test_editor_injections_are_ranges_only() {
    let lang = forge(&v2(
        "[lexer]\ninitial_mode = \"page\"\n[lexer.tokens]\nOPEN = { literal = \"<%\", modes = [\"page\"], action = \"switch main\" }\n\
         CLOSE = { literal = \"%>\", action = \"switch page\" }\n[lexer.modes.page]\ntokens = [\"OPEN\"]\ntext = \"HTML\"\n\
         [injections.html]\ntarget = \"HTML\"\nlanguage = \"html\"\nresolve = \"editor\"\n",
        "file = \"(HTML | OPEN IDENT* CLOSE)*\"\n",
    ));
    let parse = lang.parse("<p> <% a %> </p>");
    let ranges: Vec<(&str, bool, bool)> = parse
        .injections()
        .iter()
        .map(|i| (i.language(), i.is_editor(), i.tree().is_some()))
        .collect();
    assert_eq!(ranges, [("html", true, false), ("html", true, false)]);
}

// ----- [ast] supertypes -----

#[test]
fn test_supertypes_expand() {
    let lang = forge(&v2(
        "[ast]\nLiteral = [\"num\", \"text\"]\nExpr = [\"Literal\", \"word\"]\n",
        "file = \"(num | text | word)*\"\nnum = \"NUMBER\"\ntext = \"'!' IDENT\"\nword = \"IDENT\"\n",
    ));
    let expr: Vec<&str> = lang
        .supertype("Expr")
        .expect("declared")
        .map(|k| lang.kind_name(k))
        .collect();
    assert_eq!(expr, ["num", "text", "word"]);
    assert!(lang.supertype("Nope").is_none());
    assert_eq!(
        refuse(&v2(
            "[ast]\nA = [\"B\"]\nB = [\"A\"]\n",
            "file = \"IDENT\"\n"
        ))[0],
        (
            "LSF4502".to_owned(),
            "supertype `A` contains itself: A → B → A".to_owned()
        )
    );
    assert_eq!(
        refuse(&v2("[ast]\nA = [\"missing\"]\n", "file = \"IDENT\"\n"))[0].0,
        "LSF4502"
    );
}

// ----- kinds -----

#[test]
fn test_kind_index_is_public_and_dense() {
    let lang = forge(&v2("", "file = \"stmt*\"\nstmt = \"'let' IDENT ';'\"\n"));
    assert_eq!(
        lang.kind_at(0).map(|k| lang.kind_name(k)),
        Some("WHITESPACE")
    );
    assert_eq!(lang.kind("IDENT").map(|k| k.index()), Some(4));
    assert_eq!(lang.kind("NUMBER").map(|k| k.index()), Some(5));
    assert_eq!(lang.kind("STRING").map(|k| k.index()), Some(6));
    // Dense, except the internal end-of-input marker, which has no kind.
    let mut gaps = 0;
    for i in 0..lang.kind_count() {
        let index = u16::try_from(i).expect("small");
        let Some(kind) = lang.kind_at(index) else {
            gaps += 1;
            continue;
        };
        assert_eq!(kind.index(), index);
        assert_eq!(
            lang.kind(lang.kind_name(kind)).map(Kind::index),
            Some(index),
            "{}",
            lang.kind_name(kind)
        );
    }
    assert_eq!(gaps, 1);
    assert!(
        lang.kind_at(u16::try_from(lang.kind_count()).expect("small"))
            .is_none()
    );
    assert_eq!(lang.kind_name(lang.root_kind()), "file");
    // A literal by its quoted form, too.
    assert_eq!(lang.kind("'let'"), lang.kind("let"));
}

#[test]
fn test_operator_node_may_share_a_keyword_name() {
    let lang = forge(&v2(
        "",
        "file = \"expr\"\n[rules.expr]\noperand = \"IDENT\"\nlevels = [{ left = [\"instanceof\"], node = \"instanceof\" }]\n",
    ));
    let keyword = lang.kind("'instanceof'").expect("the keyword");
    let node = lang.kind("kind:instanceof").expect("the node");
    assert_ne!(keyword, node);
    let dump = ok(&lang, "a instanceof b");
    assert!(dump.contains("instanceof@0..14"), "{dump}");
}

// ----- diagnostic codes -----

#[test]
fn test_lexical_and_parse_codes_are_in_their_ranges() {
    let lang = forge(&v2("", "file = \"stmt*\"\nstmt = \"IDENT ';'\"\n"));
    // An unknown character: lexical (LF0xxx); the stray token: parse (LF1xxx).
    let found = codes(&lang, "a; ` b;");
    assert!(found.iter().any(|c| c.starts_with("LF0")), "{found:?}");
    let found = codes(&lang, "a b;");
    assert_eq!(found, ["LF1000"]);
    let found = codes(&lang, "\"open");
    assert!(found.contains(&"LF0001".to_owned()), "{found:?}");
}

#[test]
fn test_format_1_diagnostics_gain_codes_but_keep_their_text() {
    let v1 = forge("[language]\nname = \"a\"\n[rules]\na = \"IDENT ';'\"\n");
    let parse = v1.parse("x");
    let d = &parse.diagnostics()[0];
    assert_eq!(d.message(), "expected `;`, found the end of the input");
    assert!(d.code().is_some());
    let err = Language::from_lsf("[language]\nname = \"a\"\n[rules]\na = \"b\"\n").unwrap_err();
    assert_eq!(err.diagnostics()[0].message(), "undefined rule `b`");
    assert_eq!(
        err.diagnostics()[0]
            .code()
            .map(|c| c.to_string())
            .as_deref(),
        Some("LSF4101")
    );
}
