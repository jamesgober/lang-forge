//! [`Kind`]: the one kind type every forged language uses for its tokens and
//! nodes.

use core::fmt;

use syntax_lang::TokenKind;

/// The kind of a token or node in a forged language's syntax tree.
///
/// A schematic declares its vocabulary as text — rule names, keywords,
/// punctuation — so a forged language cannot have a Rust `enum` of its own.
/// `Kind` stands in for one: a small `Copy` value, assigned when the language is
/// forged, that compares as cheaply as an enum discriminant. Tokens and nodes
/// share the type, the model [`syntax_lang`] is built on.
///
/// Kinds are looked up by name with [`Language::kind`](crate::Language::kind)
/// and named with [`Language::kind_name`](crate::Language::kind_name). Look a
/// kind up once, keep it, and compare against it while walking trees: the
/// comparison is a single integer compare.
///
/// | Name | Kind of |
/// |---|---|
/// | a rule name, such as `let_stmt` | the node the rule builds |
/// | a literal's text, such as `let` or `+=` | the keyword or symbol token |
/// | a Pratt level's `node`, or `binary`, `prefix`, `postfix` | an operator node |
/// | `IDENT`, `NUMBER`, `STRING`, `NEWLINE` | the built-in token classes |
/// | `WHITESPACE`, `COMMENT` | trivia tokens |
/// | `UNKNOWN` | a character the lexer did not recognize (trivia) |
/// | `ERROR` | a node wrapping tokens the parser skipped |
///
/// A kind belongs to the language that produced it. Comparing kinds from two
/// different languages is meaningless, and naming one language's kind with
/// another language returns the wrong name or `"<unknown>"`.
///
/// # Trivia
///
/// [`TokenKind::is_trivia`] answers without the language: whitespace, comments,
/// unrecognized characters, and — unless the schematic sets `newlines = true` —
/// line breaks are trivia. Trivia is kept in the tree, so it stays lossless,
/// but the parser never sees it.
///
/// # Examples
///
/// ```
/// use lang_forge::Language;
/// use lang_forge::syntax_lang::TokenKind;
///
/// let lang = Language::from_lsf(
///     r#"
///     [language]
///     name = "sum"
///
///     [rules]
///     sum = "NUMBER ('+' NUMBER)*"
///     "#,
/// )?;
///
/// let plus = lang.kind("+").expect("the grammar uses '+'");
/// let space = lang.kind("WHITESPACE").expect("built in");
/// assert_eq!(lang.kind_name(plus), "+");
/// assert!(space.is_trivia());
/// assert!(!plus.is_trivia());
///
/// let parse = lang.parse("1 + 2");
/// let pluses = parse.tree().tokens().filter(|t| *t.kind() == plus).count();
/// assert_eq!(pluses, 1);
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kind(u16);

/// The bit that marks a kind as trivia. Kept inside the value so that
/// [`TokenKind::is_trivia`] needs no access to the language.
const TRIVIA: u16 = 0x8000;

/// The largest number of kinds a language can have: indexes use the 15 bits
/// below the trivia flag.
pub(crate) const MAX_KINDS: usize = TRIVIA as usize;

impl Kind {
    /// A kind with the given index, flagged as trivia or not.
    #[inline]
    pub(crate) const fn new(index: u16, trivia: bool) -> Self {
        Self(if trivia { index | TRIVIA } else { index })
    }

    /// The kind's position in its language's kind table.
    #[inline]
    pub(crate) const fn index(self) -> usize {
        (self.0 & !TRIVIA) as usize
    }
}

impl TokenKind for Kind {
    #[inline]
    fn is_trivia(&self) -> bool {
        self.0 & TRIVIA != 0
    }
}

impl fmt::Debug for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Kind({})", self.index())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_trivia_flag_is_independent_of_index() {
        let plain = Kind::new(7, false);
        let trivia = Kind::new(7, true);
        assert_eq!(plain.index(), 7);
        assert_eq!(trivia.index(), 7);
        assert!(!plain.is_trivia());
        assert!(trivia.is_trivia());
        assert_ne!(plain, trivia);
    }

    #[test]
    fn test_kind_debug_shows_index_only() {
        assert_eq!(alloc::format!("{:?}", Kind::new(3, true)), "Kind(3)");
    }

    #[test]
    fn test_kind_max_index_fits_below_flag() {
        let last = Kind::new((MAX_KINDS - 1) as u16, false);
        assert_eq!(last.index(), MAX_KINDS - 1);
        assert!(!last.is_trivia());
    }
}
