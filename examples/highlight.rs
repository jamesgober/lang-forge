//! Syntax highlighting from the token stream alone: `Language::lex` returns
//! every token, trivia included, so painting each one by its kind reproduces
//! the source exactly, in colour.
//!
//! ```text
//! cargo run --example highlight
//! ```

use lang_forge::Language;

const PROGRAM: &str = "\
/* Greatest common divisor, the slow way. */
fn gcd(a, b) {
    while b != 0 {
        let t = b;
        b = a % b;   // remainder
        a = t;
    }
    return a;
}

print(\"gcd:\", gcd(1071, 462));
";

const RESET: &str = "\x1b[0m";

fn main() -> Result<(), lang_forge::Error> {
    let mini = Language::from_lsf(include_str!("schematics/mini.lsf"))?;

    let mut out = String::with_capacity(PROGRAM.len() * 2);
    for token in mini.lex(PROGRAM) {
        let span = token.span();
        let text = &PROGRAM[span.start().to_usize()..span.end().to_usize()];
        match colour(mini.kind_name(*token.kind())) {
            Some(code) => {
                out.push_str(code);
                out.push_str(text);
                out.push_str(RESET);
            }
            None => out.push_str(text),
        }
    }
    print!("{out}");
    Ok(())
}

/// The ANSI colour for a token kind, by name.
fn colour(kind: &str) -> Option<&'static str> {
    match kind {
        "COMMENT" => Some("\x1b[2;3m"),
        "STRING" => Some("\x1b[32m"),
        "NUMBER" => Some("\x1b[33m"),
        "IDENT" => None,
        "WHITESPACE" | "NEWLINE" => None,
        "UNKNOWN" => Some("\x1b[41m"),
        // Every other token is a literal from the grammar: a keyword when it
        // reads like a word, a symbol otherwise.
        word if word.chars().all(|c| c.is_alphanumeric() || c == '_') => Some("\x1b[1;35m"),
        _ => Some("\x1b[36m"),
    }
}
