//! [`ParseBudget`]: how much work one parse may do.

/// How much work one parse may do before it stops: a cap on the parser's
/// steps, proportional to the input.
///
/// A parse of `n` significant tokens (comments and whitespace do not count)
/// may take `base + per_token × n` steps. A step is one grammar level entered
/// (a rule, a sequence, a choice, a repetition, an optional, a predicate, a
/// label, or an operator's operand), one token matched, or one operator; the
/// cheap units of work whose number the grammar or the memo sets — an
/// alternative examined, a remembered attempt looked at, a tree event copied
/// from one — are an eighth of a step each. Every loop the parser runs is
/// counted, so the budget bounds the parser's time whatever the grammar. A
/// parse that would take more stops where it is, with a diagnostic (code
/// `LF9002`) at the token it reached; the tree stays lossless, the tokens not
/// yet parsed kept in one `ERROR` node, like the depth limit's stop.
///
/// The parser's work is linear in the input for a given grammar, and the
/// default leaves a margin of about two over the most any example language
/// or the Mox sketch needs, on ordinary input and on nesting up to the depth
/// limit, so the budget is not reached in practice. It exists for what is
/// not ordinary: a grammar whose cost per token grows with the grammar (a
/// cycle the progress guard cuts at the end of the input, exponential in the
/// number of rules in it) and inputs that make speculation scan far ahead
/// from every token (an alternative that consumes a long run of tokens before
/// it fails, then a short one that matches, at every position: quadratic).
/// On those, a parse ends with a budget error in bounded time instead of
/// running on. A tool that runs sketches it did not write, such as an editor
/// preview, can set a tighter budget, or an absolute one with `per_token`
/// zero.
///
/// The budget bounds the parser, not the lexer or the tree, which take time
/// linear in the input regardless.
///
/// The budget is set per language with
/// [`Language::with_parse_budget`](crate::Language::with_parse_budget) and
/// covers every parse of it, [`parse`](crate::Language::parse),
/// [`parse_file`](crate::Language::parse_file), and each self-injection
/// (which has its own budget, from its own tokens).
/// [`Parse::steps`](crate::Parse::steps) tells how many steps a parse took.
///
/// # Examples
///
/// ```
/// use lang_forge::{Language, ParseBudget};
///
/// let lang = Language::from_lsf("[language]\nname = \"n\"\n[rules]\nfile = \"item*\"\nitem = \"NUMBER\"\n")?
///     .with_parse_budget(ParseBudget::new(16, 0));
///
/// // Sixteen steps cover a few items, not a hundred.
/// assert!(!lang.parse("1 2 3").has_errors());
/// let source = "7 ".repeat(100);
/// let parse = lang.parse(&source);
/// assert_eq!(parse.diagnostics()[0].code().map(|c| c.to_string()).as_deref(), Some("LF9002"));
/// // Still lossless: the tokens not parsed are in an `ERROR` node.
/// assert_eq!(parse.tree().text(parse.source()), Some(parse.source()));
/// # Ok::<(), lang_forge::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParseBudget {
    base: u64,
    per_token: u64,
}

impl ParseBudget {
    /// The budget every language starts with: 1,048,576 steps, plus 4,096
    /// per significant token.
    ///
    /// Ordinary input takes 3 to 15 steps per token on the example languages
    /// and the Mox corpus. The most the Mox sketch takes is about 2,000 per
    /// token, on wide array literals nested just under the depth limit (each
    /// entry's predicate parses the entry once more at every level). So this
    /// is reached only by the grammars and inputs the budget exists for. At 4
    /// to 15 ns a step on a current desktop CPU, a parse that uses all of it
    /// takes about 5 to 15 ms for the base and 15 to 60 ms per thousand
    /// tokens.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::ParseBudget;
    ///
    /// assert_eq!(ParseBudget::DEFAULT, ParseBudget::default());
    /// assert_eq!(ParseBudget::DEFAULT.steps(0), 1 << 20);
    /// assert_eq!(ParseBudget::DEFAULT.steps(10), (1 << 20) + 40_960);
    /// ```
    pub const DEFAULT: Self = Self::new(1 << 20, 4096);

    /// A budget of `base` steps plus `per_token` steps for each significant
    /// token. `ParseBudget::new(u64::MAX, 0)` is no budget at all; `per_token`
    /// zero is an absolute cap, whatever the input's size.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::ParseBudget;
    ///
    /// let budget = ParseBudget::new(1000, 50);
    /// assert_eq!((budget.base(), budget.per_token()), (1000, 50));
    /// assert_eq!(budget.steps(4), 1200);
    /// ```
    #[must_use]
    pub const fn new(base: u64, per_token: u64) -> Self {
        Self { base, per_token }
    }

    /// The steps allowed whatever the input's size.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::ParseBudget;
    ///
    /// assert_eq!(ParseBudget::new(7, 3).base(), 7);
    /// ```
    #[must_use]
    pub const fn base(self) -> u64 {
        self.base
    }

    /// The steps allowed for each significant token.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::ParseBudget;
    ///
    /// assert_eq!(ParseBudget::new(7, 3).per_token(), 3);
    /// ```
    #[must_use]
    pub const fn per_token(self) -> u64 {
        self.per_token
    }

    /// The steps allowed for a parse of `tokens` significant tokens:
    /// `base + per_token × tokens`, saturating at `u64::MAX`.
    ///
    /// # Examples
    ///
    /// ```
    /// use lang_forge::ParseBudget;
    ///
    /// assert_eq!(ParseBudget::new(10, 2).steps(5), 20);
    /// assert_eq!(ParseBudget::new(u64::MAX, 1).steps(5), u64::MAX);
    /// ```
    #[must_use]
    pub const fn steps(self, tokens: usize) -> u64 {
        self.per_token
            .saturating_mul(tokens as u64)
            .saturating_add(self.base)
    }
}

impl Default for ParseBudget {
    /// [`ParseBudget::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_steps_saturate() {
        assert_eq!(ParseBudget::new(0, 0).steps(usize::MAX), 0);
        assert_eq!(ParseBudget::new(1, u64::MAX).steps(2), u64::MAX);
        assert_eq!(ParseBudget::new(u64::MAX, u64::MAX).steps(1), u64::MAX);
        assert_eq!(ParseBudget::DEFAULT.steps(1), (1 << 20) + 4096);
    }
}
