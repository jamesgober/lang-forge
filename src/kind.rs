//! [`Kind`]: the one kind type every forged language uses for its tokens and
//! nodes.

use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};

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
/// A format-2 sketch adds its own token classes, string classes and the kinds
/// generated for them, and the format-2 built-ins (`DOC_COMMENT`, `SHEBANG`,
/// …); `docs/API.md` has the full list.
///
/// A kind belongs to the language that produced it. Comparing kinds from two
/// different languages is meaningless, and naming one language's kind with
/// another language returns the wrong name or `"<unknown>"`.
///
/// # Index
///
/// Every kind has a dense [`index`](Kind::index), its position in the
/// language's kind table, and [`Language::kind_at`](crate::Language::kind_at)
/// turns an index back into the kind. Adapters that need a plain integer (an
/// incremental reparser's constants, a tree-sitter export, a language server)
/// use the pair. For a format-2 sketch the numbering is the deterministic one
/// of LSF2 §5.4; for a format-1 schematic it is an implementation detail that
/// may change between releases.
///
/// # Field labels
///
/// A format-2 grammar can label what a rule matches (`cond:expr`). The label
/// of the edge from a node to one of its children is carried by the child's
/// kind: [`label`](Kind::label) returns it. Labels never take part in
/// comparison, ordering, or hashing — a labelled `expr` is still equal to
/// `lang.kind("expr")` — so code that only matches kinds is unaffected. See
/// [`Language::field_label`](crate::Language::field_label).
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
/// assert_eq!(lang.kind_at(plus.index()), Some(plus));
///
/// let parse = lang.parse("1 + 2");
/// let pluses = parse.tree().tokens().filter(|t| *t.kind() == plus).count();
/// assert_eq!(pluses, 1);
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Copy)]
pub struct Kind(u32);

/// The bit that marks a kind as trivia. Kept inside the value so that
/// [`TokenKind::is_trivia`] needs no access to the language.
const TRIVIA: u32 = 0x8000;

/// The bits that hold the kind's index.
const INDEX: u32 = 0x7FFF;

/// The bits that make up a kind's identity: index and trivia flag. The field
/// label above them is carried along but never compared.
const IDENTITY: u32 = 0xFFFF;

/// The largest number of kinds a language can have: indexes use the 15 bits
/// below the trivia flag.
pub(crate) const MAX_KINDS: usize = TRIVIA as usize;

/// The largest number of field labels a language can have: a label is stored
/// as its id plus one in the top 16 bits, zero meaning "no label".
pub(crate) const MAX_LABELS: usize = 0xFFFF;

impl Kind {
    /// A kind with the given index, flagged as trivia or not.
    #[inline]
    pub(crate) const fn new(index: u16, trivia: bool) -> Self {
        let index = index as u32 & INDEX;
        Self(if trivia { index | TRIVIA } else { index })
    }

    /// The raw bits, label included, for the language image.
    #[inline]
    pub(crate) const fn bits(self) -> u32 {
        self.0
    }

    /// A kind from raw bits read back from a language image.
    #[inline]
    pub(crate) const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The kind's position in its language's kind table.
    ///
    /// Indexes are dense, from 0 to one less than
    /// [`Language::kind_count`](crate::Language::kind_count);
    /// [`Language::kind_at`](crate::Language::kind_at) is the inverse.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"'go' NUMBER\"\n")?;
    /// let go = lang.kind("go").expect("a keyword");
    /// assert!(usize::from(go.index()) < lang.kind_count());
    /// assert_eq!(lang.kind_at(go.index()), Some(go));
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub const fn index(self) -> u16 {
        (self.0 & INDEX) as u16
    }

    /// The kind's index as a `usize`, for indexing tables.
    #[inline]
    pub(crate) const fn slot(self) -> usize {
        (self.0 & INDEX) as usize
    }

    /// The field label of the tree edge this kind sits on, if the grammar
    /// labelled it.
    ///
    /// Only kinds read from a tree carry labels: a token or node a format-2
    /// rule matched under `name:` gets that label's id. Kinds returned by
    /// [`Language::kind`](crate::Language::kind) have none. Name a label with
    /// [`Language::label_name`](crate::Language::label_name).
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf(
    ///     "[sketch]\nformat = 2\n[language]\nname = \"pair\"\nversion = \"1.0.0\"\n\
    ///      [rules]\npair = \"key:IDENT '=' value:NUMBER\"\n",
    /// )?;
    /// let parse = lang.parse("x = 1");
    /// let labels: Vec<Option<&str>> = parse
    ///     .tree()
    ///     .child_tokens()
    ///     .map(|t| t.kind().label().map(|l| lang.label_name(l).unwrap_or("?")))
    ///     .collect();
    /// assert_eq!(labels, [Some("key"), None, None, None, Some("value")]);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub const fn label(self) -> Option<u16> {
        match self.0 >> 16 {
            0 => None,
            n => Some((n - 1) as u16),
        }
    }

    /// The same kind without a field label.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::Language;
    ///
    /// let lang = Language::from_lsf("[language]\nname = \"x\"\n[rules]\nitem = \"NUMBER\"\n")?;
    /// let number = lang.kind("NUMBER").expect("built in");
    /// assert_eq!(number.unlabelled().label(), None);
    /// assert_eq!(number.unlabelled(), number);
    /// # Ok::<(), lang_forge::Error>(())
    /// ```
    #[inline]
    #[must_use]
    pub const fn unlabelled(self) -> Self {
        Self(self.0 & IDENTITY)
    }

    /// The same kind carrying `label`.
    #[inline]
    pub(crate) const fn with_label(self, label: Option<u16>) -> Self {
        match label {
            None => Self(self.0 & IDENTITY),
            Some(l) => Self((self.0 & IDENTITY) | ((l as u32 + 1) << 16)),
        }
    }
}

impl PartialEq for Kind {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0 & IDENTITY == other.0 & IDENTITY
    }
}

impl Eq for Kind {}

impl PartialOrd for Kind {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Kind {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        (self.0 & IDENTITY).cmp(&(other.0 & IDENTITY))
    }
}

impl Hash for Kind {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0 & IDENTITY).hash(state);
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
        match self.label() {
            None => write!(f, "Kind({})", self.index()),
            Some(label) => write!(f, "Kind({}, label {label})", self.index()),
        }
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
        assert_eq!(
            alloc::format!("{:?}", Kind::new(3, false).with_label(Some(2))),
            "Kind(3, label 2)"
        );
    }

    #[test]
    fn test_kind_max_index_fits_below_flag() {
        let last = Kind::new((MAX_KINDS - 1) as u16, false);
        assert_eq!(last.slot(), MAX_KINDS - 1);
        assert!(!last.is_trivia());
    }

    #[test]
    fn test_kind_labels_do_not_affect_identity() {
        use core::hash::BuildHasher;
        let k = Kind::new(9, true);
        let labelled = k.with_label(Some(0));
        let other = k.with_label(Some(MAX_LABELS as u16 - 1));
        assert_eq!(labelled.label(), Some(0));
        assert_eq!(other.label(), Some(MAX_LABELS as u16 - 1));
        assert_eq!(k.label(), None);
        assert_eq!(k, labelled);
        assert_eq!(labelled, other);
        assert_eq!(labelled.cmp(&k), Ordering::Equal);
        assert!(labelled.is_trivia());
        assert_eq!(labelled.index(), 9);
        assert_eq!(labelled.unlabelled().label(), None);
        assert_eq!(labelled.with_label(None).label(), None);
        let state = std::hash::RandomState::new();
        assert_eq!(state.hash_one(k), state.hash_one(other));
        assert_eq!(Kind::from_bits(other.bits()).label(), other.label());
    }
}
