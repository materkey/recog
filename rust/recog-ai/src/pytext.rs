//! Python's text semantics where the ported code depends on them: `str.isspace` and
//! `strip`, `splitlines`, universal newlines of `Path.read_text`, the `\w` class of a
//! Unicode `re` pattern, `str.isalnum` and `repr(str)`. Character classes come from the
//! general category in Unicode 15.0, Python 3.12's `unicodedata` version.

use std::io;
use std::path::Path;

use unicode_general_category::{GeneralCategory, get_general_category};

/// `str.isspace`: Rust's White_Space plus the separators U+001C..U+001F.
pub fn is_space(ch: char) -> bool {
    ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch)
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.lstrip()`.
pub fn lstrip(text: &str) -> &str {
    text.trim_start_matches(is_space)
}

/// `str.rstrip()`.
pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// `str.splitlines()`: every line boundary Python knows, `\r\n` counted once, no empty
/// line after a final boundary. U+2028, U+2029 and U+0085 are boundaries too, which
/// `json.dumps(ensure_ascii=False)` leaves unescaped inside strings.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let boundary = matches!(
            ch,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + ch.len_utf8();
        if ch == '\r'
            && let Some((next, '\n')) = chars.peek().copied()
        {
            chars.next();
            end = next + 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// The newline translation of text mode reading: `\r\n` and a lone `\r` become `\n`.
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `Path.read_text(encoding="utf-8")`: strict UTF-8, universal newlines.
pub fn read_text(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(universal_newlines(&text))
}

/// `Path.read_text(encoding="utf-8", errors="replace")`.
pub fn read_text_lossy(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(universal_newlines(&String::from_utf8_lossy(&bytes)))
}

fn is_letter(category: GeneralCategory) -> bool {
    use GeneralCategory::*;
    matches!(category, UppercaseLetter | LowercaseLetter | TitlecaseLetter | ModifierLetter | OtherLetter)
}

fn is_number(category: GeneralCategory) -> bool {
    use GeneralCategory::*;
    matches!(category, DecimalNumber | LetterNumber | OtherNumber)
}

/// `str.isalnum()` of one character.
pub fn is_alnum(ch: char) -> bool {
    let category = get_general_category(ch);
    is_letter(category) || is_number(category)
}

/// `\w` of a Unicode `re` pattern.
pub fn is_word(ch: char) -> bool {
    ch == '_' || is_alnum(ch)
}

/// `unicodedata.category(ch).startswith("C")`.
pub fn is_other(ch: char) -> bool {
    use GeneralCategory::*;
    matches!(get_general_category(ch), Control | Format | Surrogate | PrivateUse | Unassigned)
}

/// `str.isprintable()` of one character.
pub fn is_printable(ch: char) -> bool {
    use GeneralCategory::*;
    ch == ' '
        || !matches!(
            get_general_category(ch),
            Control
                | Format
                | Surrogate
                | PrivateUse
                | Unassigned
                | LineSeparator
                | ParagraphSeparator
                | SpaceSeparator
        )
}

/// `repr(text)`.
pub fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            ch if ch == quote || ch == '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ch if (ch as u32) < 0x20 || ch as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", ch as u32)),
            ch if ch.is_ascii() || is_printable(ch) => out.push(ch),
            ch if (ch as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", ch as u32)),
            ch if (ch as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push_str(&format!("\\U{:08x}", ch as u32)),
        }
    }
    out.push(quote);
    out
}

/// Code point count, Python's `len(str)`.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// `text[:count]` in code points.
pub fn prefix_chars(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_boundaries_given_text_when_split_then_python_lines() {
        assert_eq!(splitlines("a\r\nb\rc\nd\u{2028}e\u{85}f\n"), ["a", "b", "c", "d", "e", "f"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n\n"), ["", ""]);
        assert_eq!(splitlines("tail"), ["tail"]);
    }

    #[test]
    fn separators_given_strip_when_called_then_python_whitespace_removed() {
        assert_eq!(strip("\u{1c} x \u{a0}\u{3000}"), "x");
        assert_eq!(strip("\u{200b}x"), "\u{200b}x");
    }

    #[test]
    fn quotes_and_controls_given_repr_when_called_then_python_text() {
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_repr_str("a\"b'c"), "'a\"b\\'c'");
        assert_eq!(py_repr_str("\u{7}\u{a0}é\u{2028}😀"), "'\\x07\\xa0é\\u2028😀'");
    }
}
