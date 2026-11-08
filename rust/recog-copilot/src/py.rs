//! The few Python semantics the supervisor's inputs and outputs depend on.
//!
//! The Python supervisor reads frames with truthiness and `str()` (`if meeting_id:`
//! then `str(meeting_id)`), splits the tmux pane with `str.splitlines()`, quotes the
//! Claude command with `shlex.join` and writes frames with `json.dumps`. A port that
//! used Rust's own notions would spawn under another meeting id, classify another set
//! of lines or send other bytes; the differential test compares these against Python.

use serde_json::Value;
use unicode_general_category::{GeneralCategory, get_general_category};

/// `bool(value)` of a JSON value as Python's `json.loads` would build it.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `str(value)`.
pub fn str_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `repr(value)`.
pub fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (None, None, Some(f)) => float_repr(f),
            (None, None, None) => n.to_string(),
        },
        Value::String(s) => str_repr(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", str_repr(k), repr(v))).collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// `repr(None)` or `repr(text)`: how the Python log lines name an optional id.
pub fn repr_opt(text: Option<&str>) -> String {
    text.map_or_else(|| "None".to_owned(), str_repr)
}

/// `float.__repr__`: the shortest round-trip digits, fixed notation for decimal
/// exponents from -4 to 15 and `1e+16` style otherwise.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    let sci = format!("{:e}", f.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let n = digits.len() as i32;
    let decpt = exponent + 1;
    let body = if (-4..16).contains(&exponent) {
        if decpt <= 0 {
            format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
        } else if decpt >= n {
            format!("{}{}.0", digits, "0".repeat((decpt - n) as usize))
        } else {
            format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
        }
    } else {
        let rest = if n > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{}{}e{}{:02}", &digits[..1], rest, exp_sign, exponent.abs())
    };
    format!("{sign}{body}")
}

/// `repr(text)` of a `str`.
pub fn str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` of one character.
pub fn is_printable(c: char) -> bool {
    c == ' ' || is_printable_but_space(c)
}

fn is_printable_but_space(c: char) -> bool {
    !(is_other_category(c)
        || matches!(
            get_general_category(c),
            GeneralCategory::LineSeparator | GeneralCategory::ParagraphSeparator | GeneralCategory::SpaceSeparator
        ))
}

/// `unicodedata.category(c).startswith("C")`: control, format, surrogate, private use
/// or unassigned.
pub fn is_other_category(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    )
}

/// A character of Python's Unicode `\w`: `str.isalnum()` or the underscore, i.e. a
/// letter or a number of any kind.
pub fn is_word(c: char) -> bool {
    c == '_'
        || matches!(
            get_general_category(c),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::DecimalNumber
                | GeneralCategory::LetterNumber
                | GeneralCategory::OtherNumber
        )
}

/// `str.isspace()` of one character: Rust's White_Space plus the information
/// separators U+001C..U+001F, which Python counts as whitespace too.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `text.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `text.rstrip()`.
pub fn rstrip(text: &str) -> &str {
    text.trim_end_matches(is_space)
}

/// `text.splitlines()`: Python also breaks lines at `\r`, `\v`, `\f`, the information
/// separators, NEL and the Unicode line and paragraph separators.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let breaks = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !breaks {
            continue;
        }
        lines.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r'
            && let Some(&(j, '\n')) = chars.peek()
        {
            chars.next();
            end = j + 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `shlex.quote(arg)`.
pub fn shlex_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_owned();
    }
    let safe = arg.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe { arg.to_owned() } else { format!("'{}'", arg.replace('\'', "'\"'\"'")) }
}

/// `shlex.join(args)`.
pub fn shlex_join<S: AsRef<str>>(args: &[S]) -> String {
    args.iter().map(|a| shlex_quote(a.as_ref())).collect::<Vec<_>>().join(" ")
}

/// `json.dumps(value)` with its defaults: `", "` and `": "` separators, every
/// character outside printable ASCII escaped, keys in insertion order.
pub fn json_dumps(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, &mut out);
    out
}

fn write_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => out.push_str(&i.to_string()),
            (None, Some(u), _) => out.push_str(&u.to_string()),
            (None, None, Some(f)) => out.push_str(&float_repr(f)),
            (None, None, None) => out.push_str(&n.to_string()),
        },
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json_string(key, out);
                out.push_str(": ");
                write_json(item, out);
            }
            out.push('}');
        }
    }
}

fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_like_python_repr() {
        // given / when / then
        assert_eq!(float_repr(1.0), "1.0");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(float_repr(1.5e16), "1.5e+16");
        assert_eq!(float_repr(1e15), "1000000000000000.0");
        assert_eq!(float_repr(-2.5), "-2.5");
        assert_eq!(float_repr(123.456), "123.456");
    }

    #[test]
    fn splitlines_breaks_at_every_python_boundary_and_drops_the_last_empty_line() {
        // given / when / then
        assert_eq!(splitlines("a\r\nb\rc\u{2028}d\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n\n"), vec!["", ""]);
    }

    #[test]
    fn json_dumps_uses_python_separators_and_ascii_escapes() {
        // given
        let value = json!({"type": "x", "id": "é😀", "n": null, "l": [1, true]});
        // when / then
        let expected = "{\"type\": \"x\", \"id\": \"\\u00e9\\ud83d\\ude00\", \"n\": null, \"l\": [1, true]}";
        assert_eq!(json_dumps(&value), expected);
    }

    #[test]
    fn shlex_quotes_only_unsafe_arguments() {
        // given / when / then
        assert_eq!(shlex_join(&["claude", "", "a b", "it's", "x=1,2"]), "claude '' 'a b' 'it'\"'\"'s' x=1,2");
    }
}
