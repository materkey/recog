//! `shlex.split(s)`: Python's POSIX-mode lexer with whitespace splitting and no
//! comments, which is how `ready()` reads `NODE_OPTIONS`. Ported from `read_token`
//! state by state, since the Rust `shlex` crate quotes and escapes differently.

use crate::py::PyErr;

const WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Space,
    Word,
    Quote(char),
    Escape { back: Back },
}

/// Where an escape returns: a word, or inside double quotes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Back {
    Word,
    Quote(char),
}

/// The token being read: its text, and whether a quote was part of it (so that
/// `''` is a word).
#[derive(Default)]
struct Token {
    text: String,
    quoted: bool,
}

impl Token {
    fn is_word(&self) -> bool {
        !self.text.is_empty() || self.quoted
    }
}

/// What one character does: the next state, or the token is complete.
enum Step {
    Next(State),
    Done,
}

/// Whitespace ends a word, or keeps the lexer between words.
fn whitespace(token: &Token) -> Step {
    if token.is_word() { Step::Done } else { Step::Next(State::Space) }
}

/// Between words and in a word alike (this lexer has no comments or word
/// characters to tell them apart).
fn unquoted(next: Option<char>, token: &mut Token) -> Step {
    match next {
        None => Step::Done,
        Some(c) if WHITESPACE.contains(&c) => whitespace(token),
        Some(c @ ('\'' | '"')) => Step::Next(State::Quote(c)),
        Some('\\') => Step::Next(State::Escape { back: Back::Word }),
        Some(c) => {
            token.text.push(c);
            Step::Next(State::Word)
        }
    }
}

fn in_quotes(quote: char, next: Option<char>, token: &mut Token) -> Result<Step, PyErr> {
    token.quoted = true;
    match next {
        None => Err(PyErr::value("No closing quotation")),
        Some(c) if c == quote => Ok(Step::Next(State::Word)),
        Some('\\') if quote == '"' => Ok(Step::Next(State::Escape { back: Back::Quote(quote) })),
        Some(c) => {
            token.text.push(c);
            Ok(Step::Next(State::Quote(quote)))
        }
    }
}

fn escaped(back: Back, next: Option<char>, token: &mut Token) -> Result<Step, PyErr> {
    let Some(c) = next else { return Err(PyErr::value("No escaped character")) };
    // Within quotes only the quote itself or the escape may be escaped.
    if let Back::Quote(quote) = back
        && c != '\\'
        && c != quote
    {
        token.text.push('\\');
    }
    token.text.push(c);
    Ok(Step::Next(match back {
        Back::Word => State::Word,
        Back::Quote(quote) => State::Quote(quote),
    }))
}

/// `read_token()`: the next word, None at the end of the text.
fn read_token(chars: &mut std::str::Chars<'_>) -> Result<Option<String>, PyErr> {
    let mut state = State::Space;
    let mut token = Token::default();
    loop {
        let next = chars.next();
        let step = match state {
            State::Space | State::Word => unquoted(next, &mut token),
            State::Quote(quote) => in_quotes(quote, next, &mut token)?,
            State::Escape { back } => escaped(back, next, &mut token)?,
        };
        match step {
            Step::Next(next) => state = next,
            Step::Done => return Ok(token.is_word().then_some(token.text)),
        }
    }
}

/// The words of `text`.
///
/// # Errors
/// `ValueError("No closing quotation")` and `ValueError("No escaped character")`.
pub fn split(text: &str) -> Result<Vec<String>, PyErr> {
    let mut words = Vec::new();
    let mut chars = text.chars();
    while let Some(word) = read_token(&mut chars)? {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_when_split_then_quotes_and_escapes_resolved_like_python() {
        // given / when / then
        assert_eq!(
            split("--max-old-space-size=512 --inspect-port=9333").unwrap(),
            ["--max-old-space-size=512", "--inspect-port=9333"]
        );
        assert_eq!(split(r#"a"b c"d 'e\f' "g\"h\i" \j '' """#).unwrap(), ["ab cd", r"e\f", r#"g"h\i"#, "j", "", ""]);
        assert_eq!(split("  \t\n").unwrap(), Vec::<String>::new());
        assert_eq!(split("\"unterminated").unwrap_err().message, "No closing quotation");
        assert_eq!(split("trailing\\").unwrap_err().message, "No escaped character");
    }

    #[test]
    fn blanks_escapes_and_joined_quotes_when_split_then_like_python() {
        // given / when / then
        assert_eq!(split(" a  b\t").unwrap(), ["a", "b"]);
        assert_eq!(split("'' x \"\"").unwrap(), ["", "x", ""]);
        assert_eq!(split("a\\ b").unwrap(), ["a b"]);
        assert_eq!(split(r#""a\\b" "a\$b""#).unwrap(), [r"a\b", r"a\$b"]);
        assert_eq!(split("'it''s'").unwrap(), ["its"]);
        assert_eq!(split("x=\"1 2\"y").unwrap(), ["x=1 2y"]);
        assert_eq!(split("\"a\\").unwrap_err().message, "No escaped character");
        assert_eq!(split("'a\\").unwrap_err().message, "No closing quotation");
    }
}
