//! The diagnostic code registry.
//!
//! Every diagnostic lang-forge produces carries a [`Code`]. Problems with a
//! sketch are `LSF` codes, numbered by the area of the sketch format they
//! concern (LSF2 §27.1); problems with a program written in a forged language
//! are `LF` codes, split by phase as diag-lang recommends: `LF0001`–`LF0999`
//! lexical, `LF1000`–`LF1999` parse, `LF9000`–`LF9999` limits. A code is never
//! reused for a different problem; messages may be reworded, codes may not.
//!
//! The full table, with what each code means, is in `docs/API.md`
//! ("Diagnostic codes").

use diag_lang::Code;

/// Builds a code at compile time. Every call below is a constant, so a
/// malformed code fails the build rather than any run.
const fn code(prefix: &str, number: u16) -> Code {
    match Code::new(prefix, number) {
        Some(code) => code,
        None => panic!("malformed diagnostic code"),
    }
}

/// Defines `pub(crate) const NAME: Code = code(prefix, number);` for each line.
macro_rules! codes {
    ($($(#[$doc:meta])* $name:ident = $prefix:literal $number:literal;)*) => {
        $( $(#[$doc])* pub(crate) const $name: Code = code($prefix, $number); )*
    };
}

codes! {
    // ----- sketch problems: reading -----
    /// The document is not valid NOML.
    NOML_INVALID = "LSF" 1;
    /// A NOML feature a sketch may not use (functions, `@native`, `[[...]]`).
    NOML_FEATURE = "LSF" 2;

    // ----- sections, keys, types -----
    /// An unknown section.
    UNKNOWN_SECTION = "LSF" 1001;
    /// An unknown key.
    UNKNOWN_KEY = "LSF" 1002;
    /// A value of the wrong type or shape.
    WRONG_TYPE = "LSF" 1003;
    /// A required section, key, or value is missing.
    MISSING = "LSF" 1004;
    /// A format-2 section or key in a format-1 document.
    NEEDS_FORMAT_2 = "LSF" 1005;
    /// An integer out of its key's range.
    OUT_OF_RANGE = "LSF" 1006;
    /// Something LSF2 specifies that this release of lang-forge does not
    /// implement yet (the message names the roadmap item).
    NOT_SUPPORTED = "LSF" 1007;
    /// `[sketch] format` is newer than this reader.
    FORMAT_TOO_NEW = "LSF" 1101;
    /// `[sketch]` without `format`.
    FORMAT_MISSING = "LSF" 1102;
    /// `[language] version` is not SemVer.
    VERSION = "LSF" 1103;
    /// `[language] edition` is malformed.
    EDITION = "LSF" 1104;
    /// A malformed file extension.
    EXTENSION = "LSF" 1105;
    /// A file extension listed twice.
    EXTENSION_TWICE = "LSF" 1106;
    /// A `[language] files` entry that names an unknown extension, mode, or rule.
    FILES_ENTRY = "LSF" 1107;
    /// A reference that could mean a rule, a class, or a literal.
    AMBIGUOUS_REF = "LSF" 1201;
    /// A literal reference to a text that is not a token of the language.
    UNKNOWN_LITERAL = "LSF" 1202;

    // ----- files and composition -----
    /// A key or table defined in two files of one sketch.
    DEFINED_TWICE = "LSF" 2002;
    /// `modules` outside the entry file.
    MODULES_OUTSIDE_ENTRY = "LSF" 2003;
    /// A file listed or added twice.
    FILE_TWICE = "LSF" 2005;
    /// `[language] start` is required because rules span several files.
    START_REQUIRED = "LSF" 2006;
    /// One text used for two lexical purposes.
    TEXT_TWICE = "LSF" 2014;
    /// A part or mixin with `[language]`, or an entry without it.
    FILE_ROLE = "LSF" 2022;
    /// A module that was not added to the sketch, or a file no module lists.
    MODULE_NOT_FOUND = "LSF" 2023;
    /// A module whose `[sketch] kind` is not `"part"`.
    NOT_A_PART = "LSF" 2024;

    // ----- lexer -----
    /// A malformed token-class name, or a class declared twice.
    CLASS_NAME = "LSF" 3101;
    /// An unknown identifier style.
    IDENT_STYLE = "LSF" 3102;
    /// `newlines` together with `[layout.newlines]`.
    NEWLINES_TWICE = "LSF" 3103;
    /// `strings = [...]` together with named string classes.
    STRINGS_MIXED = "LSF" 3104;
    /// A bracket text that is not a grammar literal.
    BRACKET_NOT_LITERAL = "LSF" 3105;
    /// A bracket text in two pairs.
    BRACKET_TWICE = "LSF" 3106;
    /// An unusable `extra_start` / `extra_continue` character.
    EXTRA_CHARS = "LSF" 3107;
    /// A contextual keyword that does not read like an identifier.
    CONTEXTUAL_SHAPE = "LSF" 3108;
    /// A redundant contextual-keyword list (warning).
    CONTEXTUAL_REDUNDANT = "LSF" 3109;
    /// A comment or string delimiter the lexer cannot use.
    DELIMITER = "LSF" 3110;
    /// A `not_followed_by` / `when_next` value that is not one character class.
    ONE_CLASS = "LSF" 3111;
    /// A `stop_before` text that is not a token of the comment's modes.
    STOP_BEFORE = "LSF" 3112;
    /// A string close delimiter that is missing or empty.
    STRING_CLOSE = "LSF" 3114;
    /// An escape that is not a single character.
    ESCAPE = "LSF" 3115;
    /// A delimiter part that can never match (warning).
    DEAD_PART = "LSF" 3116;
    /// An interpolation hole whose close text is unusable.
    HOLE_CLOSE = "LSF" 3117;
    /// A generated kind name that collides with a declared one.
    GENERATED_NAME = "LSF" 3118;
    /// A `separators` value that is not one character.
    SEPARATORS = "LSF" 3119;
    /// A literal that `trailing_dot` numbers can swallow (warning).
    TRAILING_DOT = "LSF" 3120;
    /// A malformed number suffix.
    SUFFIX = "LSF" 3121;
    /// A grammar literal that equals a declared class's literal.
    LITERAL_IS_CLASS = "LSF" 3122;
    /// A literal the lexer can never produce as one token.
    LITERAL_SHAPE = "LSF" 3125;
    /// A token regex whose automaton is too large.
    REGEX_TOO_LARGE = "LSF" 3201;
    /// All token regexes of one mode together are too large.
    MODE_TOO_LARGE = "LSF" 3202;
    /// A token regex that matches the empty string.
    REGEX_EMPTY = "LSF" 3203;
    /// A token class with none or several of `regex`, `literal`, `hook`.
    CLASS_SOURCE = "LSF" 3204;
    /// Malformed regex syntax.
    REGEX_SYNTAX = "LSF" 3205;
    /// A Unicode property the regex dialect does not support.
    REGEX_PROPERTY = "LSF" 3206;
    /// A malformed mode name.
    MODE_NAME = "LSF" 3301;
    /// A mode that is not declared.
    MODE_UNDECLARED = "LSF" 3302;
    /// A cycle of `inherit`.
    MODE_CYCLE = "LSF" 3303;
    /// A declared token class nothing uses (warning).
    UNUSED_TOKEN = "LSF" 3401;

    // ----- rules -----
    /// An undefined rule, class, hook, or mode.
    UNDEFINED = "LSF" 4101;
    /// A malformed or forbidden rule name.
    RULE_NAME = "LSF" 4102;
    /// A malformed label.
    LABEL_NAME = "LSF" 4103;
    /// A reserved name used for a rule, node, or keyword.
    RESERVED = "LSF" 4104;
    /// A label on an element that adds nothing to the tree.
    LABEL_ZERO_WIDTH = "LSF" 4105;
    /// A label reserved for operator nodes, or `kind`.
    LABEL_RESERVED = "LSF" 4106;
    /// A missing or out-of-order `prec`.
    PREC = "LSF" 4108;
    /// A text back-reference on something other than a token.
    BACKREF_NOT_TOKEN = "LSF" 4109;
    /// A text back-reference to a label that does not occur earlier.
    BACKREF_LATER = "LSF" 4110;
    /// Malformed rule text.
    RULE_SYNTAX = "LSF" 4112;
    /// A greedy repetition or optional that diverges from its follower (M02).
    OVERLAP = "LSF" 4301;
    /// A rule nothing reaches (warning).
    UNUSED_RULE = "LSF" 4302;
    /// Left recursion.
    LEFT_RECURSION = "LSF" 4305;
    /// A repetition of something that can match nothing.
    EMPTY_REPETITION = "LSF" 4306;
    /// An alternative that can never match.
    DEAD_ALTERNATIVE = "LSF" 4307;
    /// An expression rule whose operand can match nothing.
    EMPTY_OPERAND = "LSF" 4308;
    /// An operator at two levels of one expression rule.
    OPERATOR_TWICE = "LSF" 4309;
    /// An expression rule with too many levels.
    TOO_MANY_LEVELS = "LSF" 4310;
    /// A malformed injection id or entry.
    INJECTION = "LSF" 4401;
    /// A hook key missing or out of place.
    HOOK_REQUIRED = "LSF" 4402;
    /// An injection target that names nothing.
    INJECTION_TARGET = "LSF" 4403;
    /// An injected language that cannot be found.
    INJECTION_LANGUAGE = "LSF" 4404;
    /// An unknown hook kind.
    HOOK_KIND = "LSF" 4405;
    /// A malformed or colliding supertype name.
    SUPERTYPE_NAME = "LSF" 4501;
    /// A cycle of supertypes, or a member that is not a node kind.
    SUPERTYPE_MEMBER = "LSF" 4502;

    // ----- capabilities -----
    /// A malformed or repeated capability name.
    CAPABILITY_NAME = "LSF" 7001;
    /// An included capability with no pass in the registry.
    CAPABILITY_MISSING = "LSF" 7004;
    /// An included capability with several passes in the registry.
    CAPABILITY_AMBIGUOUS = "LSF" 7005;

    // ----- security validation -----
    /// A path that leaves the project root.
    PATH_ESCAPES = "LSF" 8001;
    /// A path that is not portable.
    PATH_PORTABLE = "LSF" 8002;
    /// Two paths that differ only by ASCII case.
    PATH_CASE = "LSF" 8003;
    /// A malformed language name.
    LANGUAGE_NAME = "LSF" 8101;
    /// Free text with a forbidden character.
    FREE_TEXT = "LSF" 8201;
    /// Literal token text with a forbidden character or length.
    LITERAL_TEXT = "LSF" 8202;

    // ----- limits -----
    /// A document larger than 8 MiB.
    DOCUMENT_TOO_LARGE = "LSF" 9001;
    /// A sketch larger than 64 MiB in total.
    SKETCH_TOO_LARGE = "LSF" 9002;
    /// More than 1024 files.
    TOO_MANY_FILES = "LSF" 9003;
    /// Nesting deeper than 64 levels.
    TOO_DEEP = "LSF" 9004;
    /// More kinds than a language can have.
    TOO_MANY_KINDS = "LSF" 9005;
    /// More field labels than a language can have.
    TOO_MANY_LABELS = "LSF" 9006;
    /// More than 256 lexer modes.
    TOO_MANY_MODES = "LSF" 9007;
    /// Parser tables larger than the budget.
    TABLES_TOO_LARGE = "LSF" 9008;

    // ----- program problems: lexical -----
    /// Characters that begin no token.
    LEX_UNEXPECTED = "LF" 1;
    /// A string with no closing delimiter.
    LEX_UNTERMINATED_STRING = "LF" 2;
    /// A block comment with no closing delimiter.
    LEX_UNTERMINATED_COMMENT = "LF" 3;
    /// A digit outside a number literal's radix.
    LEX_BAD_DIGIT = "LF" 4;
    /// A radix prefix with no digits.
    LEX_NO_DIGITS = "LF" 5;
    /// Letters or digits running on after a number.
    LEX_NUMBER_SUFFIX = "LF" 6;
    /// A decimal literal with a leading zero where they are refused.
    LEX_LEADING_ZERO = "LF" 7;
    /// A digit separator in a place the separator rule forbids.
    LEX_SEPARATOR = "LF" 8;
    /// An identifier that is not in NFC.
    LEX_NOT_NFC = "LF" 9;
    /// End of input inside a lexer mode that must be closed.
    LEX_UNTERMINATED_MODE = "LF" 10;
    /// Lexer modes nested past `max_mode_depth`.
    LEX_TOO_DEEP = "LF" 11;
    /// Tabs and spaces mixed inconsistently in indentation.
    LEX_MIXED_INDENT = "LF" 12;
    /// A dedent to a column no enclosing block uses.
    LEX_BAD_DEDENT = "LF" 13;
    /// A malformed exponent or hexadecimal float.
    LEX_EXPONENT = "LF" 14;

    // ----- program problems: parse -----
    /// A token the grammar does not allow here.
    PARSE_EXPECTED = "LF" 1000;
    /// Input left over after the start rule.
    PARSE_LEFTOVER = "LF" 1001;
    /// A non-associative operator chained.
    PARSE_CHAINED = "LF" 1002;
    /// Input nested more deeply than the parser's limit.
    PARSE_TOO_DEEP = "LF" 1003;
    /// A text back-reference whose text differs.
    PARSE_MISMATCH = "LF" 1004;

    // ----- program problems: limits -----
    /// A source too large to address.
    SOURCE_TOO_LARGE = "LF" 9001;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codes_render_and_split_by_phase() {
        assert_eq!(alloc::format!("{OVERLAP}"), "LSF4301");
        assert_eq!(alloc::format!("{LEX_UNEXPECTED}"), "LF0001");
        assert!(LEX_TOO_DEEP.number() < 1000);
        assert!((1000..2000).contains(&PARSE_EXPECTED.number()));
        assert_eq!(SOURCE_TOO_LARGE.number(), 9001);
    }
}
