//! What the ported modules take from Python's `str`, `int()` and `float()`: the
//! character classes of Python's Unicode database (`unicode_tables`, generated from
//! the interpreter), `repr()` of a string, and the number parsing environment values
//! go through. Error texts are Python's, because the ported modules put them into
//! their own messages.

use crate::unicode_tables::{CASEFOLD, DECIMAL_ZEROS, NONPRINTABLE, OTHER, SPACE};

fn in_ranges(table: &[(u32, u32)], c: char) -> bool {
    let code = u32::from(c);
    table
        .binary_search_by(|&(start, end)| {
            if end < code {
                std::cmp::Ordering::Less
            } else if start > code {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `str.isspace()` of one character.
#[must_use]
pub fn is_space(c: char) -> bool {
    in_ranges(SPACE, c)
}

/// `unicodedata.category(c).startswith("C")`: control, format, private use,
/// unassigned.
#[must_use]
pub fn is_other(c: char) -> bool {
    in_ranges(OTHER, c)
}

/// `str.isprintable()` of one character.
#[must_use]
pub fn is_printable(c: char) -> bool {
    !in_ranges(NONPRINTABLE, c)
}

/// `str.strip()` without arguments.
#[must_use]
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.split()` without arguments: runs of whitespace separate, none at the ends.
pub fn split_whitespace(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_space).filter(|part| !part.is_empty())
}

/// `str.casefold()`.
#[must_use]
pub fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match CASEFOLD.binary_search_by_key(&u32::from(c), |&(code, _)| code) {
            Ok(at) => out.push_str(CASEFOLD[at].1),
            Err(_) => out.push(c),
        }
    }
    out
}

/// `str.splitlines()`: the line boundaries Python knows, `\r\n` as one.
#[must_use]
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            lines.push(&s[start..at]);
            let mut end = at + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, next)| next == '\n') {
                chars.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// `repr()` of a string, quotes included.
#[must_use]
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => {
                let code = u32::from(c);
                if code < 0x100 {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code < 0x10000 {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// The digits of a Python numeric literal with its single underscores between digits
/// removed, or None when an underscore is misplaced.
fn without_underscores(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            let between_digits =
                i > 0 && bytes[i - 1].is_ascii_digit() && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !between_digits {
                return None;
            }
        } else {
            out.push(char::from(b));
        }
    }
    Some(out)
}

/// Every Unicode decimal digit as its ASCII digit, as `int()` and `float()` read
/// them.
fn ascii_digits(s: &str) -> String {
    s.chars()
        .map(|c| {
            let code = u32::from(c);
            let zero = match DECIMAL_ZEROS.binary_search(&code) {
                Ok(at) => Some(DECIMAL_ZEROS[at]),
                Err(0) => None,
                Err(at) => Some(DECIMAL_ZEROS[at - 1]).filter(|z| code - z < 10),
            };
            zero.and_then(|z| char::from_digit(code - z, 10)).unwrap_or(c)
        })
        .collect()
}

/// `int(raw)` in base 10, within i64.
///
/// # Errors
/// Python's `ValueError` text for what `int()` rejects. A value beyond i64 also fails
/// here, where Python's unbounded `int` would accept it.
pub fn parse_int(raw: &str) -> Result<i64, String> {
    let invalid = || format!("invalid literal for int() with base 10: {}", repr(raw));
    let text = ascii_digits(strip(raw));
    let text = text.as_str();
    if !text.is_ascii() {
        return Err(invalid());
    }
    let (sign, digits) = match text.as_bytes().first() {
        Some(b'+') => ("", &text[1..]),
        Some(b'-') => ("-", &text[1..]),
        _ => ("", text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_') {
        return Err(invalid());
    }
    let digits = without_underscores(digits).ok_or_else(invalid)?;
    format!("{sign}{digits}").parse::<i64>().map_err(|_| invalid())
}

/// `float(raw)`.
///
/// # Errors
/// Python's `ValueError` text for what `float()` rejects.
pub fn parse_float(raw: &str) -> Result<f64, String> {
    let invalid = || format!("could not convert string to float: {}", repr(raw));
    let text = ascii_digits(strip(raw));
    let text = text.as_str();
    if !text.is_ascii() {
        return Err(invalid());
    }
    let negative = text.starts_with('-');
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let value = match unsigned.to_ascii_lowercase().as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ if is_decimal_literal(unsigned) => {
            without_underscores(unsigned).ok_or_else(invalid)?.parse::<f64>().map_err(|_| invalid())?
        }
        _ => return Err(invalid()),
    };
    Ok(if negative { -value } else { value })
}

/// Python's grammar of an unsigned float literal: `digits [. [digits]] [exponent]` or
/// `. digits [exponent]`, digits with single underscores between them.
fn is_decimal_literal(s: &str) -> bool {
    fn digits(s: &[u8]) -> usize {
        let mut i = 0;
        while i < s.len()
            && (s[i].is_ascii_digit() || s[i] == b'_' && i > 0 && s.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            i += 1;
        }
        i
    }
    let s = s.as_bytes();
    let whole = digits(s);
    let mut i = whole;
    let mut fraction = 0;
    if s.get(i) == Some(&b'.') {
        i += 1;
        fraction = digits(&s[i..]);
        i += fraction;
    }
    if whole == 0 && fraction == 0 {
        return false;
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(s.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exponent = digits(&s[i..]);
        if exponent == 0 {
            return false;
        }
        i += exponent;
    }
    i == s.len()
}

/// `repr(x)` of a float: the shortest digits that round-trip, in fixed notation
/// for exponents from -4 to 15 and in Python's exponent notation otherwise.
#[must_use]
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // `{:e}` gives the shortest round-trip digits: d[.ddd]e<exp>.
    let formatted = format!("{:e}", x.abs());
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exponent) {
        let point = exponent + 1;
        let text = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(usize::try_from(-point).unwrap_or(0)))
        } else {
            let point = usize::try_from(point).unwrap_or(0);
            if digits.len() <= point {
                format!("{digits}{}.0", "0".repeat(point - digits.len()))
            } else {
                format!("{}.{}", &digits[..point], &digits[point..])
            }
        };
        format!("{sign}{text}")
    } else {
        let mantissa = if digits.len() == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{mantissa}e{exp_sign}{:02}", exponent.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_whitespace_includes_the_information_separators_rust_leaves_out() {
        // given / when / then
        assert!(is_space('\x1c') && is_space('\u{a0}') && is_space(' '));
        assert!(!is_space('\u{200b}'));
        assert_eq!(strip("\x1f a \u{3000}"), "a");
    }

    #[test]
    fn repr_of_strings_picks_quotes_and_escapes_like_python() {
        // given / when / then
        assert_eq!(repr("abc"), "'abc'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr("tab\there\n\x01\u{7f}"), "'tab\\there\\n\\x01\\x7f'");
        assert_eq!(repr("é\u{200b}\u{e000}"), "'é\\u200b\\ue000'");
    }

    #[test]
    fn int_parsing_follows_python_int_with_its_error_text() {
        // given / when / then
        assert_eq!(parse_int("+1_000"), Ok(1000));
        assert_eq!(parse_int("-007"), Ok(-7));
        assert_eq!(parse_int("1__0"), Err("invalid literal for int() with base 10: '1__0'".into()));
        assert_eq!(parse_int("3.5"), Err("invalid literal for int() with base 10: '3.5'".into()));
        assert_eq!(parse_int(" \u{661}_\u{662}\u{a0}"), Ok(12));
        assert_eq!(parse_float("\u{661}e\u{662}"), Ok(100.0));
        assert_eq!(parse_float("\u{ff11}.5"), Ok(1.5));
    }

    #[test]
    fn float_parsing_accepts_what_python_float_accepts() {
        // given / when / then
        assert_eq!(parse_float("1_5.2_5"), Ok(15.25));
        assert_eq!(parse_float(".5"), Ok(0.5));
        assert_eq!(parse_float("5."), Ok(5.0));
        assert_eq!(parse_float("1e3"), Ok(1000.0));
        assert!(parse_float("-Infinity").unwrap().is_infinite());
        assert!(parse_float("nan").unwrap().is_nan());
        assert_eq!(parse_float("soon"), Err("could not convert string to float: 'soon'".into()));
        assert!(parse_float("1_").is_err() && parse_float("e5").is_err() && parse_float("0x10").is_err());
    }

    #[test]
    fn float_repr_matches_python_repr() {
        // given / when / then
        for (x, text) in [
            (1.0, "1.0"),
            (2.5, "2.5"),
            (0.1, "0.1"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123_456_789_012_345.0, "123456789012345.0"),
            (-0.0, "-0.0"),
            (1e300, "1e+300"),
            (-2.5e-7, "-2.5e-07"),
        ] {
            assert_eq!(float_repr(x), text);
        }
    }

    #[test]
    fn casefold_and_splitlines_follow_python() {
        // given / when / then
        assert_eq!(casefold("Straße ME"), "strasse me");
        assert_eq!(splitlines("a\r\nb\rc\u{2028}d\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
    }
}
