//! [`Sketch`]: a sketch whose parts are several files (LSF2 §3, ISSUES M13).

use alloc::{
    boxed::Box,
    collections::BTreeSet,
    format,
    string::{String, ToString},
    vec::Vec,
};

use diag_lang::{Diagnostic, Label, SourceMap};
use syntax_lang::Span;

use crate::{Error, Language, codes, error::Report, noml};

/// The most files one sketch may have (LSF2 §1.8).
const MAX_FILES: usize = 1024;

/// The most bytes all files of a sketch may have together (LSF2 §1.8).
const MAX_TOTAL: usize = 64 << 20;

/// A sketch made of several files: the entry and the parts its
/// `[sketch] modules` lists (LSF2 §3.2).
///
/// Add the entry first, then its parts, each under its project-relative path
/// (`/`-separated, no `..` that leaves the root, portable to every OS). The
/// files go into a [`SourceMap`] in the order they are added, so every
/// diagnostic forging reports — and every span in the forged language's
/// warnings — points into the right file: render them with
/// [`source_map`](Sketch::source_map). A one-file sketch forged from a
/// `Sketch` gets the same spans as from [`Language::from_lsf`], since the
/// first file starts at position 0.
///
/// The parts merge as if the files were one document: a table may be
/// continued in another file (`[rules]` in two files adds rules to one
/// table), and a key or table defined twice, in one file or two, is an error
/// naming both places. When rules come from more than one file,
/// `[language] start` is required.
///
/// # Examples
///
/// ```
/// use lang_forge::{Language, Sketch};
///
/// let mut sketch = Sketch::new();
/// sketch.add("calc.lsf", r#"
/// [sketch]
/// format = 2
/// modules = ["rules/expr.lsf"]
///
/// [language]
/// name = "calc"
/// version = "1.0.0"
/// start = "program"
///
/// [rules]
/// program = "stmts:stmt*"
/// stmt = "value:expr ';'"
/// "#)?;
/// sketch.add("rules/expr.lsf", r#"
/// [sketch]
/// format = 2
/// kind = "part"
///
/// [rules.expr]
/// operand = "NUMBER"
/// levels = [{ left = ["+"] }]
/// "#)?;
///
/// let calc = Language::from_sketch(&sketch)?;
/// assert!(!calc.parse("1 + 2; 3;").has_errors());
/// # Ok::<(), lang_forge::Error>(())
/// ```
///
/// A problem in a part is reported in that part:
///
/// ```
/// use lang_forge::{Language, Sketch};
///
/// let mut sketch = Sketch::new();
/// sketch.add("a.lsf", "[sketch]\nformat = 2\nmodules = [\"b.lsf\"]\n[language]\nname = \"a\"\nversion = \"1.0.0\"\nstart = \"x\"\n[rules]\nx = \"y\"\n")?;
/// sketch.add("b.lsf", "[sketch]\nformat = 2\nkind = \"part\"\n[rules]\ny = \"undefined_rule\"\n")?;
/// let err = Language::from_sketch(&sketch).unwrap_err();
/// assert_eq!(err.to_string(), "b.lsf:5:6: undefined rule `undefined_rule`");
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct Sketch {
    map: SourceMap,
    /// The normalized path of each file, in the order added.
    paths: Vec<String>,
    total: usize,
}

impl Sketch {
    /// An empty sketch.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Sketch;
    ///
    /// let mut sketch = Sketch::new();
    /// assert!(sketch.is_empty());
    /// sketch.add("main.lsf", "[language]\nname = \"m\"\n[rules]\nm = \"IDENT\"\n")?;
    /// assert_eq!(sketch.len(), 1);
    /// assert_eq!(sketch.source_map().iter().next().map(|(_, f)| f.name()), Some("main.lsf"));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file under its project-relative `path`. The first file added
    /// is the entry.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the path is not a portable project-relative
    /// path (`LSF8001`, `LSF8002`), is already in the sketch, or differs
    /// from one only by ASCII case (`LSF2005`, `LSF8003`), or if the file is
    /// larger than 8 MiB (`LSF9001`), the sketch would pass 64 MiB in total
    /// (`LSF9002`), or it would have more than 1024 files (`LSF9003`). The
    /// sketch is unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Sketch;
    ///
    /// let mut sketch = Sketch::new();
    /// assert!(sketch.add("lang/main.lsf", "").is_ok());
    /// assert!(sketch.add("../outside.lsf", "").is_err());
    /// assert!(sketch.add("C:/abs.lsf", "").is_err());
    /// assert!(sketch.add("lang/main.lsf", "").is_err());
    /// assert_eq!(sketch.len(), 1);
    /// ```
    pub fn add(&mut self, path: &str, text: impl Into<Box<str>>) -> Result<(), Error> {
        let text: Box<str> = text.into();
        let fail = |code, message: String| {
            Err(Error::located(
                Vec::from([crate::error::coded(code, Span::empty(0), message)]),
                1,
                1,
            ))
        };
        let normalized = match normalize(path) {
            Ok(p) => p,
            Err((code, message)) => return fail(code, message),
        };
        if self.paths.contains(&normalized) {
            return fail(
                codes::FILE_TWICE,
                format!("`{normalized}` is already in the sketch"),
            );
        }
        if let Some(other) = self
            .paths
            .iter()
            .find(|p| p.eq_ignore_ascii_case(&normalized))
        {
            return fail(
                codes::PATH_CASE,
                format!(
                    "`{normalized}` and `{other}` differ only by case, which is one file on Windows and macOS"
                ),
            );
        }
        if text.len() > noml::MAX_SCHEMATIC {
            return fail(
                codes::DOCUMENT_TOO_LARGE,
                format!(
                    "`{normalized}` is larger than {} MiB; a sketch file is at most that",
                    noml::MAX_SCHEMATIC >> 20
                ),
            );
        }
        if self.total + text.len() > MAX_TOTAL {
            return fail(
                codes::SKETCH_TOO_LARGE,
                format!("a sketch is at most {} MiB in total", MAX_TOTAL >> 20),
            );
        }
        if self.paths.len() >= MAX_FILES {
            return fail(
                codes::TOO_MANY_FILES,
                format!("a sketch has at most {MAX_FILES} files"),
            );
        }
        let len = text.len();
        if self.map.add(normalized.as_str(), text).is_err() {
            return fail(
                codes::SKETCH_TOO_LARGE,
                String::from("the sketch does not fit in the source map"),
            );
        }
        self.total += len;
        self.paths.push(normalized);
        Ok(())
    }

    /// The sketch's files, for rendering diagnostics with
    /// [`Renderer`](diag_lang::Renderer).
    #[must_use]
    pub fn source_map(&self) -> &SourceMap {
        &self.map
    }

    /// The number of files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether no file has been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The text and global base offset of file `i`.
    fn file(&self, i: usize) -> Option<(&str, u32)> {
        let (_, file) = self.map.iter().nth(i)?;
        Some((file.text(), file.span().start().to_u32()))
    }
}

/// Normalizes and validates a project-relative path (LSF2 §3.4).
fn normalize(path: &str) -> Result<String, (diag_lang::Code, String)> {
    let bad = |code, why: &str| Err((code, format!("path `{path}` {why}")));
    if path.is_empty() {
        return bad(codes::PATH_PORTABLE, "is empty");
    }
    if path.starts_with('/') || path.contains('\\') || path.as_bytes().get(1) == Some(&b':') {
        return bad(
            codes::PATH_PORTABLE,
            "must be project-relative and `/`-separated",
        );
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => return bad(codes::PATH_PORTABLE, "has an empty or `.` component"),
            ".." => {
                if parts.pop().is_none() {
                    return bad(codes::PATH_ESCAPES, "leaves the project root");
                }
            }
            _ => {
                if part.len() > 255
                    || part.chars().any(|c| {
                        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')
                    })
                    || part.ends_with('.')
                    || part.ends_with(' ')
                    || is_reserved_name(part)
                {
                    return bad(
                        codes::PATH_PORTABLE,
                        "is not portable to every operating system",
                    );
                }
                parts.push(part);
            }
        }
    }
    if parts.is_empty() {
        return bad(codes::PATH_PORTABLE, "names no file");
    }
    Ok(parts.join("/"))
}

/// Windows reserved device names, with any extension.
fn is_reserved_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

/// A diagnostic with every span moved by `base`.
fn shift(d: &Diagnostic, base: u32) -> Diagnostic {
    let move_span = |s: Span| Span::new(s.start().to_u32() + base, s.end().to_u32() + base);
    let primary = d.primary();
    let label = if primary.message().is_empty() {
        Label::unlabelled(move_span(primary.span()))
    } else {
        Label::new(move_span(primary.span()), primary.message())
    };
    let mut out = Diagnostic::new(d.severity(), d.message(), label);
    for s in d.secondary() {
        out = out.with_secondary(Label::new(move_span(s.span()), s.message()));
    }
    for n in d.notes() {
        out = out.with_note(n);
    }
    for h in d.help() {
        out = out.with_help(h);
    }
    if let Some(code) = d.code() {
        out = out.with_code(code);
    }
    out
}

impl Language {
    /// Forges a language from a [`Sketch`] of one or more files (LSF2 §3).
    ///
    /// The first file is the entry. A format-2 entry lists its parts in
    /// `[sketch] modules`, and every part must be in the sketch with
    /// `[sketch] kind = "part"`. A one-file format-1 sketch is forged exactly
    /// as [`from_lsf`](Language::from_lsf) forges it.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] whose diagnostics point into the sketch's
    /// [`source_map`](Sketch::source_map): everything `from_lsf` reports,
    /// plus a module that is not in the sketch or listed twice (`LSF2023`,
    /// `LSF2005`), a file no module lists (`LSF2023`), a part that is not
    /// marked `kind = "part"` or has `[language]` or `modules` (`LSF2024`,
    /// `LSF2022`, `LSF2003`), a key defined in two files (`LSF2002`), and a
    /// missing `[language] start` when rules span files (`LSF2006`).
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::{Language, Sketch};
    ///
    /// let mut sketch = Sketch::new();
    /// sketch.add("words.lsf", "[sketch]\nformat = 2\nmodules = [\"lexer.lsf\"]\n\
    ///     [language]\nname = \"words\"\nversion = \"1.0.0\"\n[rules]\nfile = \"TAG*\"\n")?;
    /// sketch.add("lexer.lsf", "[sketch]\nformat = 2\nkind = \"part\"\n\
    ///     [lexer.tokens]\nTAG = { regex = \"#[a-z]+\" }\n")?;
    /// assert_eq!(sketch.len(), 2);
    ///
    /// let words = Language::from_sketch(&sketch)?;
    /// assert!(!words.parse("#a #bc").has_errors());
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    pub fn from_sketch(sketch: &Sketch) -> Result<Self, Error> {
        let map = &sketch.map;
        let mut report = Report::default();
        let n = sketch.len();
        let rank_of = |span: Span| -> u32 {
            let at = span.start();
            map.locate(at).map_or(0, |(id, _)| {
                map.iter().position(|(i, _)| i == id).unwrap_or(0) as u32
            })
        };
        let Some((entry_text, entry_base)) = sketch.file(0) else {
            report.error(codes::MISSING, Span::empty(0), "the sketch has no files");
            return Err(report.into_sketch_error(map, &rank_of));
        };
        // Read every file; spans move into the source map's positions.
        let mut roots = Vec::with_capacity(n);
        for i in 0..n {
            let Some((text, base)) = sketch.file(i) else {
                continue;
            };
            match noml::read(text) {
                Ok(root) => roots.push(Some(root)),
                Err(d) => {
                    report.diagnostic(shift(&d, base));
                    roots.push(None);
                }
            }
        }
        if !report.is_clean() {
            return Err(report.into_sketch_error(map, &rank_of));
        }
        let mut roots: Vec<noml::Table<'_>> = roots.into_iter().flatten().collect();
        let format2 = roots[0].entries.iter().any(|e| e.key == "sketch");
        if !format2 {
            if n > 1 {
                report.error(
                    codes::MODULES_OUTSIDE_ENTRY,
                    Span::new(entry_base, entry_base),
                    "a format-1 sketch is one file; give the entry `[sketch] format = 2` and `modules` to split it",
                );
                return Err(report.into_sketch_error(map, &rank_of));
            }
            return Language::from_lsf(entry_text);
        }
        for (i, root) in roots.iter_mut().enumerate() {
            if let Some((_, base)) = sketch.file(i) {
                root.rebase(base);
            }
        }

        // The modules the entry lists.
        let listed: Vec<(String, Span)> = crate::spec2::modules(&roots[0])
            .into_iter()
            .map(|(p, s)| (p.to_string(), s))
            .collect();
        let mut order: Vec<usize> = Vec::new();
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for (path, span) in &listed {
            match normalize(path) {
                Ok(p) => match sketch.paths.iter().position(|q| *q == p) {
                    Some(0) => report.error(
                        codes::FILE_TWICE,
                        *span,
                        "the entry cannot list itself as a module",
                    ),
                    Some(i) if !seen.insert(i) => {
                        report.error(
                            codes::FILE_TWICE,
                            *span,
                            format!("module `{p}` is listed twice"),
                        );
                    }
                    Some(i) => order.push(i),
                    None => report.error(
                        codes::MODULE_NOT_FOUND,
                        *span,
                        format!("module `{p}` is not in the sketch"),
                    ),
                },
                Err((code, message)) => report.error(code, *span, message),
            }
        }
        for (i, path) in sketch.paths.iter().enumerate().skip(1) {
            if !seen.contains(&i) {
                let base = sketch.file(i).map_or(0, |(_, b)| b);
                report.error(
                    codes::MODULE_NOT_FOUND,
                    Span::new(base, base),
                    format!(
                        "`{path}` is in the sketch, but the entry's `modules` does not list it"
                    ),
                );
            }
        }
        // Parts: `[sketch] kind = "part"`, no `[language]`, no `modules`.
        let mut rules_files = 0;
        for (i, root) in roots.iter().enumerate() {
            if root.entries.iter().any(|e| e.key == "rules") {
                rules_files += 1;
            }
            if i == 0 {
                continue;
            }
            let base = sketch.file(i).map_or(0, |(_, b)| b);
            let at = Span::new(base, base);
            let sketch_table = root.entries.iter().find(|e| e.key == "sketch");
            let kind = sketch_table.and_then(|e| match &e.value.kind {
                noml::ValueKind::Table(t) => {
                    t.entries
                        .iter()
                        .find(|k| k.key == "kind")
                        .and_then(|k| match &k.value.kind {
                            noml::ValueKind::Str(s) => Some(s.text.to_string()),
                            _ => None,
                        })
                }
                _ => None,
            });
            if kind.as_deref() != Some("part") {
                report.error(
                    codes::NOT_A_PART,
                    sketch_table.map_or(at, |e| e.key_span),
                    format!(
                        "module `{}` must say `[sketch] kind = \"part\"`",
                        sketch.paths[i]
                    ),
                );
            }
            if let Some(language) = root.entries.iter().find(|e| e.key == "language") {
                report.error(
                    codes::FILE_ROLE,
                    language.key_span,
                    "a part cannot have [language]; it belongs in the entry",
                );
            }
            if !crate::spec2::modules(root).is_empty() {
                report.error(
                    codes::MODULES_OUTSIDE_ENTRY,
                    at,
                    "`modules` is only allowed in the entry file",
                );
            }
        }
        if rules_files > 1 {
            let start_given = roots[0].entries.iter().any(|e| {
                e.key == "language"
                    && matches!(&e.value.kind, noml::ValueKind::Table(t) if t.entries.iter().any(|k| k.key == "start"))
            });
            if !start_given {
                report.error(
                    codes::START_REQUIRED,
                    Span::new(entry_base, entry_base),
                    "[rules] spans several files, so `[language] start` is required",
                );
            }
        }
        if !report.is_clean() {
            return Err(report.into_sketch_error(map, &rank_of));
        }

        // Merge the parts into the entry, in `modules` order.
        let mut taken: Vec<Option<noml::Table<'_>>> = roots.into_iter().map(Some).collect();
        let Some(mut merged) = taken[0].take() else {
            return Err(report.into_sketch_error(map, &rank_of));
        };
        for &i in &order {
            let Some(mut part) = taken[i].take() else {
                continue;
            };
            // Each file has its own `[sketch]`.
            part.entries.retain(|e| e.key != "sketch");
            let mut twice = |key: &str, first: Span, second: Span| {
                report.diagnostic(
                    Diagnostic::new(
                        diag_lang::Severity::Error,
                        format!("`{key}` is defined twice in this sketch"),
                        Label::new(second, "defined again here"),
                    )
                    .with_secondary(Label::new(first, "first defined here"))
                    .with_code(codes::DEFINED_TWICE),
                );
            };
            merged.merge(part, &mut twice);
        }
        let spec = crate::schematic::interpret(merged, &mut report);
        let locate = |span: Span| -> (u32, u32) {
            map.line_col(span.start())
                .map_or((1, 1), |(_, lc)| (lc.line, lc.col))
        };
        let grammar = spec.and_then(|spec| crate::grammar2::compile(&spec, &locate, &mut report));
        match grammar {
            Some(mut grammar) if report.is_clean() => {
                if let Some(extra) = grammar.extra.as_mut() {
                    extra.warnings = report.into_sketch_warnings(&rank_of).into();
                }
                Ok(Language::from_grammar(grammar))
            }
            _ => {
                if report.is_clean() {
                    report.error(
                        codes::MISSING,
                        Span::empty(0),
                        "the sketch could not be forged",
                    );
                }
                Err(report.into_sketch_error(map, &rank_of))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_paths() {
        assert_eq!(normalize("a/b/../c.lsf").as_deref().ok(), Some("a/c.lsf"));
        assert!(normalize("../x.lsf").is_err());
        assert!(normalize("a//b").is_err());
        assert!(normalize("./a").is_err());
        assert!(normalize("/a").is_err());
        assert!(normalize("a\\b").is_err());
        assert!(normalize("CON.lsf").is_err());
        assert!(normalize("com1").is_err());
        assert!(normalize("com0").is_ok());
        assert!(normalize("a?.lsf").is_err());
        assert!(normalize("a.").is_err());
        assert!(normalize("x/..").is_err());
    }
}
