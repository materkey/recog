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

/// The words of `text`.
///
/// # Errors
/// `ValueError("No closing quotation")` and `ValueError("No escaped character")`.
pub fn split(text: &str) -> Result<Vec<String>, PyErr> {
    let mut words = Vec::new();
    let mut chars = text.chars();
    loop {
        let mut state = State::Space;
        let mut token = String::new();
        let mut quoted = false;
        loop {
            let next = chars.next();
            match state {
                State::Space => match next {
                    None => break,
                    Some(c) if WHITESPACE.contains(&c) => {
                        if !token.is_empty() || quoted {
                            break;
                        }
                    }
                    Some('\\') => state = State::Escape { back: Back::Word },
                    Some(c @ ('\'' | '"')) => state = State::Quote(c),
                    Some(c) => {
                        token.push(c);
                        state = State::Word;
                    }
                },
                State::Quote(quote) => {
                    quoted = true;
                    match next {
                        None => return Err(PyErr::value("No closing quotation")),
                        Some(c) if c == quote => state = State::Word,
                        Some('\\') if quote == '"' => state = State::Escape { back: Back::Quote(quote) },
                        Some(c) => token.push(c),
                    }
                }
                State::Escape { back } => {
                    let Some(c) = next else { return Err(PyErr::value("No escaped character")) };
                    // Within quotes only the quote itself or the escape may be escaped.
                    if let Back::Quote(quote) = back
                        && c != '\\'
                        && c != quote
                    {
                        token.push('\\');
                    }
                    token.push(c);
                    state = match back {
                        Back::Word => State::Word,
                        Back::Quote(quote) => State::Quote(quote),
                    };
                }
                State::Word => match next {
                    None => break,
                    Some(c) if WHITESPACE.contains(&c) => {
                        if !token.is_empty() || quoted {
                            break;
                        }
                        state = State::Space;
                    }
                    Some(c @ ('\'' | '"')) => state = State::Quote(c),
                    Some('\\') => state = State::Escape { back: Back::Word },
                    Some(c) => token.push(c),
                },
            }
        }
        if token.is_empty() && !quoted {
            return Ok(words);
        }
        words.push(token);
    }
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
