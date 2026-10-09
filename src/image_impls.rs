//! The language image encoding of the forged tables, and the check that a
//! decoded image is one lang-forge could have built (see `image.rs`).

use alloc::boxed::Box;

use crate::{
    grammar::{
        CAT_EOF, CAT_NODE, CapabilityRef, Expr, Extra, FieldDef, Grammar, InjectionDef, Kinds,
        Level, Lex, Pratt, Program, ProgramV2, Rule, RuleBody,
    },
    image::{Image, ImageError, Reader, Res, Writer, check, image_enum, image_struct},
    kind::Kind,
    lexer::Lexer,
    scan::{
        Act, Builtins, Class, Comment, Fixed, Hole, LayoutRt, Matcher, Mode, Numbers, PartRt,
        Scanner, StrClass, StringKinds,
    },
    schematic::{Fixity, IdentMode},
    set::NO_SET,
    spec2::{CloseAt, LeadingZeros},
};

image_enum!(IdentMode { Xid = 0, Ascii = 1 });
image_enum!(Fixity {
    Left = 0,
    Right = 1,
    NonAssoc = 2,
    Prefix = 3,
    Postfix = 4,
});
image_enum!(CloseAt {
    Anywhere = 0,
    LineStart = 1,
    LineStartIndented = 2,
});
image_enum!(LeadingZeros {
    Decimal = 0,
    Error = 1,
    Octal = 2,
});

image_struct!(CapabilityRef {
    name,
    span,
    line,
    column
});
image_struct!(Kinds {
    names,
    values,
    by_name,
    cats
});
image_struct!(Rule {
    name,
    node,
    body,
    first,
    nullable,
    sync
});
image_struct!(Pratt {
    operand,
    prefix,
    after,
    levels,
    contextual
});
image_struct!(Level {
    node,
    fixity,
    lbp,
    rbp,
    then
});
image_struct!(ProgramV2 {
    contextual,
    case_insensitive,
    op_labels,
    lines,
    backrefs,
    predicates,
});
image_struct!(Program {
    exprs,
    items,
    first,
    nullable,
    rules,
    pratts,
    sets,
    sync,
    start,
    eof,
    error,
    newline,
    ident,
    number,
    strings,
    v2,
});
image_struct!(FieldDef {
    label,
    cardinality,
    kinds
});
image_struct!(InjectionDef {
    id,
    kind,
    label,
    language,
    editor,
    start,
    inner,
    combined,
    scope,
});
image_struct!(Grammar {
    name,
    version,
    extensions,
    capabilities,
    kinds,
    lexer,
    program,
    extra,
});
image_struct!(Builtins {
    whitespace,
    comment,
    doc_comment,
    unknown,
    ident,
    number,
    newline,
    indent,
    dedent,
    shebang,
});
image_struct!(Class {
    kind,
    matcher,
    priority,
    action,
    followed_by,
    not_followed_by,
    when_prev,
    unless_prev,
    line_start,
    indented,
    column,
});
image_struct!(Comment {
    open,
    close,
    nested,
    doc,
    not_followed_by,
    stop_before,
});
image_struct!(Hole {
    open,
    close,
    mode,
    when_next
});
image_struct!(StrClass {
    open,
    close,
    escape,
    multiline,
    next_line,
    close_at,
    close_not_followed_by,
    kinds,
    holes,
    embedded,
});
image_struct!(Mode {
    fixed,
    heads,
    regexes,
    part_strings,
    literals,
    builtins,
    trivia,
    text,
    eof_error,
    actions,
    may_start,
    name,
});
image_struct!(Numbers {
    radix,
    radix_any_case,
    separator,
    separator_anywhere,
    floats,
    exponent,
    leading_dot,
    trailing_dot,
    hex_floats,
    leading_zeros,
    suffixes,
});
image_struct!(LayoutRt {
    indent,
    open_after,
    joins,
    newline_joins,
    explicit_join,
    tab_width,
    mixed_error,
    newlines,
    terminate_after,
    continue_before,
});
image_struct!(Scanner {
    ident_mode,
    extra_start,
    extra_continue,
    require_nfc,
    numbers,
    keywords,
    case_insensitive,
    comments,
    classes,
    strings,
    modes,
    initial,
    max_depth,
    shebang,
    brackets,
    k,
    newlines,
    tab_width,
    layout,
});

impl Image for Extra {
    fn put(&self, w: &mut Writer) {
        self.display_name.put(w);
        self.description.put(w);
        self.edition.put(w);
        self.shebang_names.put(w);
        self.labels.put(w);
        self.fields.put(w);
        self.supertypes.put(w);
        self.injections.put(w);
        self.files.put(w);
        self.embedded_parse.put(w);
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok(Self {
            display_name: Image::get(r)?,
            description: Image::get(r)?,
            edition: Image::get(r)?,
            shebang_names: Image::get(r)?,
            labels: Image::get(r)?,
            fields: Image::get(r)?,
            supertypes: Image::get(r)?,
            injections: Image::get(r)?,
            files: Image::get(r)?,
            embedded_parse: Image::get(r)?,
            warnings: Box::new([]),
        })
    }
}

impl Image for Lex {
    fn put(&self, w: &mut Writer) {
        match self {
            Lex::V1(l) => {
                0u8.put(w);
                l.put(w);
            }
            Lex::V2(s) => {
                1u8.put(w);
                s.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        match u8::get(r)? {
            0 => Ok(Lex::V1(Box::new(Lexer::get(r)?))),
            1 => Ok(Lex::V2(Box::new(Scanner::get(r)?))),
            _ => Err(ImageError::Invalid),
        }
    }
}

impl Image for RuleBody {
    fn put(&self, w: &mut Writer) {
        match *self {
            RuleBody::Expr(e) => {
                0u8.put(w);
                e.put(w);
            }
            RuleBody::Pratt(p) => {
                1u8.put(w);
                p.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        match u8::get(r)? {
            0 => Ok(RuleBody::Expr(u32::get(r)?)),
            1 => Ok(RuleBody::Pratt(u32::get(r)?)),
            _ => Err(ImageError::Invalid),
        }
    }
}

impl Image for Expr {
    fn put(&self, w: &mut Writer) {
        match *self {
            Expr::Token(k) => (0u8, k).put(w),
            Expr::Rule(r) => (1u8, r).put(w),
            Expr::Seq { start, len } => (2u8, start, len).put(w),
            Expr::Choice { start, len } => (3u8, start, len).put(w),
            Expr::Repeat {
                body,
                min_one,
                stop,
            } => {
                (4u8, body).put(w);
                (min_one, stop).put(w);
            }
            Expr::Optional(body) => (5u8, body).put(w),
            Expr::Keyword(k) => (6u8, k).put(w),
            Expr::Word => 7u8.put(w),
            Expr::Label { label, body } => (8u8, label, body).put(w),
            Expr::And(body) => (9u8, body).put(w),
            Expr::Not(body) => (10u8, body).put(w),
            Expr::BackRef { label, body } => (11u8, label, body).put(w),
            Expr::Eof => 12u8.put(w),
            Expr::LineStart => 13u8.put(w),
            Expr::NlBefore => 14u8.put(w),
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok(match u8::get(r)? {
            0 => Expr::Token(u16::get(r)?),
            1 => Expr::Rule(u32::get(r)?),
            2 => Expr::Seq {
                start: u32::get(r)?,
                len: u32::get(r)?,
            },
            3 => Expr::Choice {
                start: u32::get(r)?,
                len: u32::get(r)?,
            },
            4 => Expr::Repeat {
                body: u32::get(r)?,
                min_one: bool::get(r)?,
                stop: u32::get(r)?,
            },
            5 => Expr::Optional(u32::get(r)?),
            6 => Expr::Keyword(u16::get(r)?),
            7 => Expr::Word,
            8 => Expr::Label {
                label: u16::get(r)?,
                body: u32::get(r)?,
            },
            9 => Expr::And(u32::get(r)?),
            10 => Expr::Not(u32::get(r)?),
            11 => Expr::BackRef {
                label: u16::get(r)?,
                body: u32::get(r)?,
            },
            12 => Expr::Eof,
            13 => Expr::LineStart,
            14 => Expr::NlBefore,
            _ => return Err(ImageError::Invalid),
        })
    }
}

impl Image for Act {
    fn put(&self, w: &mut Writer) {
        match *self {
            Act::Push(m) => (0u8, m).put(w),
            Act::Pop => (1u8, 0u16).put(w),
            Act::Switch(m) => (2u8, m).put(w),
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        let (tag, mode) = <(u8, u16)>::get(r)?;
        match tag {
            0 => Ok(Act::Push(mode)),
            1 => Ok(Act::Pop),
            2 => Ok(Act::Switch(mode)),
            _ => Err(ImageError::Invalid),
        }
    }
}

impl Image for Matcher {
    fn put(&self, w: &mut Writer) {
        match self {
            Matcher::Regex(r) => {
                0u8.put(w);
                r.put(w);
            }
            Matcher::Literal(l) => {
                1u8.put(w);
                l.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        match u8::get(r)? {
            0 => Ok(Matcher::Regex(Image::get(r)?)),
            1 => Ok(Matcher::Literal(Image::get(r)?)),
            _ => Err(ImageError::Invalid),
        }
    }
}

impl Image for PartRt {
    fn put(&self, w: &mut Writer) {
        match self {
            PartRt::Text(t) => {
                0u8.put(w);
                t.put(w);
            }
            PartRt::Regex(r) => {
                1u8.put(w);
                r.put(w);
            }
            PartRt::Capture(r) => {
                2u8.put(w);
                r.put(w);
            }
            PartRt::Backref => 3u8.put(w),
            PartRt::Newline => 4u8.put(w),
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok(match u8::get(r)? {
            0 => PartRt::Text(Image::get(r)?),
            1 => PartRt::Regex(Image::get(r)?),
            2 => PartRt::Capture(Image::get(r)?),
            3 => PartRt::Backref,
            4 => PartRt::Newline,
            _ => return Err(ImageError::Invalid),
        })
    }
}

impl Image for Fixed {
    fn put(&self, w: &mut Writer) {
        match *self {
            Fixed::Literal(k) => {
                0u8.put(w);
                k.put(w);
            }
            Fixed::Class(i) => (1u8, u32::from(i)).put(w),
            Fixed::Comment(i) => (2u8, u32::from(i)).put(w),
            Fixed::String(i) => (3u8, u32::from(i)).put(w),
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        let tag = u8::get(r)?;
        if tag == 0 {
            return Ok(Fixed::Literal(Kind::get(r)?));
        }
        let index = u16::try_from(u32::get(r)?).map_err(|_| ImageError::Invalid)?;
        match tag {
            1 => Ok(Fixed::Class(index)),
            2 => Ok(Fixed::Comment(index)),
            3 => Ok(Fixed::String(index)),
            _ => Err(ImageError::Invalid),
        }
    }
}

impl Image for StringKinds {
    fn put(&self, w: &mut Writer) {
        match self {
            StringKinds::Token(k) => {
                0u8.put(w);
                k.put(w);
            }
            StringKinds::Node {
                open,
                text,
                escape,
                embedded,
                interp_open,
                interp_close,
                close,
            } => {
                1u8.put(w);
                open.put(w);
                text.put(w);
                escape.put(w);
                embedded.put(w);
                interp_open.put(w);
                interp_close.put(w);
                close.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Res<Self> {
        Ok(match u8::get(r)? {
            0 => StringKinds::Token(Kind::get(r)?),
            1 => StringKinds::Node {
                open: Image::get(r)?,
                text: Image::get(r)?,
                escape: Image::get(r)?,
                embedded: Image::get(r)?,
                interp_open: Image::get(r)?,
                interp_close: Image::get(r)?,
                close: Image::get(r)?,
            },
            _ => return Err(ImageError::Invalid),
        })
    }
}

impl Grammar {
    /// Checks that every index the lexer and parser follow at run time is in
    /// range, and every kind they produce is one of the language's, so a
    /// decoded image can parse any input without panicking.
    pub(crate) fn validate(&self) -> Res<()> {
        let kinds = &self.kinds;
        let n_kinds = kinds.names.len();
        check(n_kinds > 0 && kinds.values.len() == n_kinds && kinds.cats.len() == n_kinds)?;
        check(
            kinds
                .values
                .iter()
                .enumerate()
                .all(|(i, k)| k.slot() == i && k.label().is_none()),
        )?;
        check(kinds.by_name.iter().all(|&i| usize::from(i) < n_kinds))?;
        check(kinds.by_name.windows(2).all(|w| {
            let (a, b) = (usize::from(w[0]), usize::from(w[1]));
            (&kinds.names[a], a) < (&kinds.names[b], b)
        }))?;
        let p = &self.program;
        let eof = usize::from(p.eof);
        check(eof < n_kinds && kinds.cats[eof] == CAT_EOF)?;
        // A token kind the lexer produces: below the end-of-input bit, and
        // one of the language's kinds.
        let token_ok = |k: Kind| k.slot() < eof && kinds.values[k.slot()] == k;
        let kind_ok = |k: Kind| k.slot() < n_kinds && kinds.values[k.slot()] == k;
        match &self.lexer {
            Lex::V1(l) => check(l.valid(&token_ok))?,
            Lex::V2(s) => check(scanner_ok(s, &token_ok))?,
        }
        program_ok(p, n_kinds, &kind_ok)?;
        if let Some(extra) = &self.extra {
            let rules = p.rules.len();
            check(
                extra
                    .supertypes
                    .iter()
                    .all(|(_, m)| m.iter().all(|&k| usize::from(k) < n_kinds)),
            )?;
            check(extra.injections.iter().all(|i| (i.start as usize) < rules))?;
            check(
                extra
                    .files
                    .iter()
                    .all(|&(_, _, start)| start == u32::MAX || (start as usize) < rules),
            )?;
            check(
                extra
                    .embedded_parse
                    .iter()
                    .all(|&(_, rule)| (rule as usize) < rules),
            )?;
            check(extra.embedded_parse.windows(2).all(|w| w[0].0 < w[1].0))?;
            check(extra.fields.windows(2).all(|w| w[0].0 < w[1].0))?;
            check(matches!(self.lexer, Lex::V2(_)))?;
        }
        Ok(())
    }
}

fn program_ok(p: &Program, n_kinds: usize, kind_ok: &dyn Fn(Kind) -> bool) -> Res<()> {
    let n_exprs = p.exprs.len();
    let n_rules = p.rules.len();
    check(p.sets.valid())?;
    let n_sets = p.sets.count() as u32;
    let set_ok = |s: u32| s < n_sets;
    let opt_set_ok = |s: u32| s == NO_SET || s < n_sets;
    // Every token kind the parser compares must fit the sets' bits.
    check(usize::from(p.eof) < p.sets.bits())?;
    // The built-in token kinds the parser tests sets for are tokens.
    check(
        p.newline < p.eof
            && p.ident < p.eof
            && p.number < p.eof
            && p.strings.iter().all(|&s| s < p.eof),
    )?;
    check(
        p.first.len() == n_exprs && p.nullable.len() == n_exprs && p.sync.len() == p.items.len(),
    )?;
    check(p.first.iter().all(|&s| set_ok(s)))?;
    check(p.sync.iter().all(|&s| opt_set_ok(s)))?;
    check(p.items.iter().all(|&e| (e as usize) < n_exprs))?;
    let expr_ok = |e: u32| (e as usize) < n_exprs;
    for expr in p.exprs.iter() {
        let ok = match *expr {
            Expr::Token(_) | Expr::Word | Expr::Eof | Expr::LineStart | Expr::NlBefore => true,
            Expr::Keyword(k) => k < p.eof,
            Expr::Rule(r) => (r as usize) < n_rules,
            Expr::Seq { start, len } | Expr::Choice { start, len } => (start as usize)
                .checked_add(len as usize)
                .is_some_and(|end| end <= p.items.len()),
            Expr::Repeat { body, stop, .. } => expr_ok(body) && opt_set_ok(stop),
            Expr::Optional(body)
            | Expr::Label { body, .. }
            | Expr::And(body)
            | Expr::Not(body)
            | Expr::BackRef { body, .. } => expr_ok(body),
        };
        check(ok)?;
    }
    for rule in p.rules.iter() {
        check(rule.node.is_none_or(kind_ok) && set_ok(rule.first) && opt_set_ok(rule.sync))?;
        check(match rule.body {
            RuleBody::Expr(e) => expr_ok(e),
            RuleBody::Pratt(q) => (q as usize) < p.pratts.len(),
        })?;
    }
    for pratt in p.pratts.iter() {
        let levels = pratt.levels.len();
        check(expr_ok(pratt.operand))?;
        check(
            pratt
                .prefix
                .iter()
                .chain(pratt.after.iter())
                .all(|&l| usize::from(l) <= levels),
        )?;
        check(pratt.contextual.iter().all(|&(k, a, b)| {
            usize::from(k) < n_kinds && usize::from(a) <= levels && usize::from(b) <= levels
        }))?;
        check(
            pratt
                .levels
                .iter()
                .all(|l| kind_ok(l.node) && l.then.is_none_or(expr_ok)),
        )?;
    }
    check((p.start as usize) < n_rules && kind_ok(p.error))?;
    if let Some(v2) = &p.v2 {
        // Contextual keywords: sorted by text, kinds below the end bit.
        check(v2.contextual.windows(2).all(|w| w[0].0 < w[1].0))?;
        check(v2.contextual.iter().all(|&(_, k)| k < p.eof))?;
    }
    let _ = CAT_NODE;
    Ok(())
}

fn scanner_ok(s: &Scanner, token_ok: &dyn Fn(Kind) -> bool) -> bool {
    // Texts are whole UTF-8 sequences, so every token ends on a character
    // boundary.
    let text_ok = |t: &[u8]| !t.is_empty() && core::str::from_utf8(t).is_ok();
    let n_modes = s.modes.len();
    let n_classes = s.classes.len();
    let n_comments = s.comments.len();
    let n_strings = s.strings.len();
    let act_ok = |a: &Act| match *a {
        Act::Push(m) | Act::Switch(m) => usize::from(m) < n_modes,
        Act::Pop => true,
    };
    let modes_ok = s.modes.iter().all(|m| {
        m.heads.len() == 257
            && m.heads[0] == 0
            && m.heads[256] as usize == m.fixed.len()
            && m.heads.windows(2).all(|w| w[0] <= w[1])
            && (0..256).all(|b| {
                m.fixed[m.heads[b] as usize..m.heads[b + 1] as usize]
                    .iter()
                    .all(|(t, _)| t.first() == Some(&(b as u8)))
            })
            && m.fixed.iter().all(|(_, f)| match *f {
                Fixed::Literal(k) => token_ok(k),
                Fixed::Class(i) => usize::from(i) < n_classes,
                Fixed::Comment(i) => usize::from(i) < n_comments,
                Fixed::String(i) => usize::from(i) < n_strings,
            })
            && m.regexes.iter().all(|&i| usize::from(i) < n_classes)
            && m.part_strings.iter().all(|&i| usize::from(i) < n_strings)
            && m.text.is_none_or(token_ok)
            && m.actions.iter().all(|(_, a)| act_ok(a))
            && m.actions.windows(2).all(|w| w[0].0 < w[1].0)
    });
    let classes_ok = s.classes.iter().all(|c| {
        token_ok(c.kind)
            && c.action.as_ref().is_none_or(act_ok)
            && match &c.matcher {
                Matcher::Literal(l) => text_ok(l),
                Matcher::Regex(_) => true,
            }
            && c.when_prev
                .as_ref()
                .is_none_or(|v| v.windows(2).all(|w| w[0] < w[1]))
            && c.unless_prev
                .as_ref()
                .is_none_or(|v| v.windows(2).all(|w| w[0] < w[1]))
    });
    let comments_ok = s.comments.iter().all(|c| {
        text_ok(&c.open)
            && c.close.as_deref().is_none_or(text_ok)
            && c.stop_before.iter().all(|t| text_ok(t))
    });
    let strings_ok = s.strings.iter().all(|c| {
        let kinds_ok = match &c.kinds {
            StringKinds::Token(k) => token_ok(*k),
            StringKinds::Node {
                open,
                text,
                escape,
                embedded,
                interp_open,
                interp_close,
                close,
            } => {
                token_ok(*open)
                    && token_ok(*text)
                    && escape.is_none_or(token_ok)
                    && embedded.len() == c.embedded.len()
                    && embedded.iter().all(|k| token_ok(*k))
                    && interp_open.is_none_or(token_ok)
                    && interp_close.is_none_or(token_ok)
                    && token_ok(*close)
            }
        };
        let parts_ok = |parts: &[PartRt]| {
            !parts.is_empty()
                && parts.iter().all(|p| match p {
                    PartRt::Text(t) => !t.is_empty() && core::str::from_utf8(t).is_ok(),
                    _ => true,
                })
        };
        kinds_ok
            && parts_ok(&c.open)
            && parts_ok(&c.close)
            && c.holes
                .iter()
                .all(|h| text_ok(&h.open) && text_ok(&h.close) && usize::from(h.mode) < n_modes)
            && (c.holes.is_empty()
                || matches!(
                    c.kinds,
                    StringKinds::Node {
                        interp_open: Some(_),
                        interp_close: Some(_),
                        ..
                    }
                ))
    });
    let fixed_utf8 = s
        .modes
        .iter()
        .all(|m| m.fixed.iter().all(|(t, _)| core::str::from_utf8(t).is_ok()));
    n_modes > 0
        && usize::from(s.initial) < n_modes
        && modes_ok
        && fixed_utf8
        && classes_ok
        && comments_ok
        && strings_ok
        && s.keywords.valid(token_ok)
        && [
            s.k.whitespace,
            s.k.comment,
            s.k.doc_comment,
            s.k.unknown,
            s.k.ident,
            s.k.number,
            s.k.newline,
            s.k.indent,
            s.k.dedent,
            s.k.shebang,
        ]
        .into_iter()
        .all(token_ok)
        && s.layout
            .as_ref()
            .is_none_or(|l| l.tab_width >= 1 && l.explicit_join.as_deref().is_none_or(text_ok))
        && s.tab_width >= 1
}

/// Every forged language's image round-trips; the property tests and the
/// integration tests check the languages they forge.
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use crate::Language;

    #[test]
    fn test_image_round_trip_format_1_and_2() {
        for sketch in [
            "[language]\nname = \"a\"\n[lexer]\nstrings = ['\"']\nline_comments = [\"#\"]\nblock_comments = [[\"/*\", \"*/\"]]\n[rules]\nfile = \"(IDENT | STRING)*\"\n",
            "[sketch]\nformat = 2\n[language]\nname = \"b\"\nversion = \"1.0.0\"\n[lexer.tokens]\nVAR = { regex = '\\$[a-z]+' }\n[lexer.strings.DQ]\nopen = '\"'\ninterpolate = [{ open = \"{\", close = \"}\", rule = \"expr\" }]\n[rules]\nfile = \"items:expr*\"\n[rules.expr]\noperand = \"VAR | NUMBER | DQ | '{' expr '}'\"\nlevels = [{ left = [\"+\"] }]\n",
        ] {
            let lang = Language::from_lsf(sketch).unwrap_or_else(|e| panic!("{e}"));
            let image = lang.to_image();
            let back = Language::from_image(&image).expect("loads");
            assert_eq!(back.to_image(), image);
            let src = "$a + \"x{$b + 1}y\" + 2";
            assert_eq!(back.parse(src).dump(), lang.parse(src).dump());
        }
    }
}
