//! The three ways the Python server writes JSON, and the bridge between the two JSON
//! types of the crates below.
//!
//! - HTTP bodies are Starlette's `JSONResponse.render`: `ensure_ascii=False`,
//!   `separators=(",", ":")`, `allow_nan=False` ([`response`]).
//! - Live-stream and bridge frames built with a bare `json.dumps(...)` escape every
//!   non-ASCII character (`ensure_ascii=True`) and keep the default separators
//!   ([`ascii`]).
//! - Frames built with `json.dumps(..., ensure_ascii=False)` ([`unicode`]).
//!
//! `recog-core` and `recog-ai` each keep their own ordered JSON value; values cross
//! between them through their Python-compatible text form ([`to_core`], [`to_ai`]).

use std::fmt::Write as _;

use recog_ai::json::Json;
use recog_core::pyjson::{self, Map, Value};

/// `json.dumps(value, ensure_ascii=False)`.
#[must_use]
pub fn unicode(value: &Value) -> String {
    pyjson::dumps(value, None)
}

/// `json.dumps(value)`: non-ASCII as `\uXXXX`, default separators.
#[must_use]
pub fn ascii(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, true, (", ", ": "));
    out
}

/// The body Starlette's `JSONResponse` sends, or `None` when Python's `json.dumps`
/// would refuse it (`allow_nan=False` and a NaN or infinite float).
#[must_use]
pub fn response(value: &Value) -> Option<String> {
    if has_non_finite(value) {
        return None;
    }
    let mut out = String::new();
    write_value(&mut out, value, false, (",", ":"));
    Some(out)
}

fn has_non_finite(value: &Value) -> bool {
    match value {
        Value::Float(x) => !x.is_finite(),
        Value::Array(items) => items.iter().any(has_non_finite),
        Value::Object(map) => map.values().any(has_non_finite),
        _ => false,
    }
}

fn write_string(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        if let Some(escape) = named_escape(c) {
            out.push_str(escape);
        } else if u32::from(c) < 0x20 {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        } else if ensure_ascii && u32::from(c) > 0x7e {
            // ESCAPE_ASCII is `[^\ -~]`: DEL and everything above ASCII.
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        } else {
            out.push(c);
        }
    }
    out.push('"');
}

/// The two-character escape of a character that has one.
fn named_escape(c: char) -> Option<&'static str> {
    match c {
        '"' => Some("\\\""),
        '\\' => Some("\\\\"),
        '\n' => Some("\\n"),
        '\r' => Some("\\r"),
        '\t' => Some("\\t"),
        '\u{8}' => Some("\\b"),
        '\u{c}' => Some("\\f"),
        _ => None,
    }
}

fn write_value(out: &mut String, value: &Value, ensure_ascii: bool, separators: (&str, &str)) {
    match value {
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(separators.0);
                }
                write_value(out, item, ensure_ascii, separators);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(separators.0);
                }
                write_string(out, k, ensure_ascii);
                out.push_str(separators.1);
                write_value(out, v, ensure_ascii, separators);
            }
            out.push('}');
        }
        Value::Str(s) => write_string(out, s, ensure_ascii),
        scalar => out.push_str(&pyjson::dumps(scalar, None)),
    }
}

/// A `recog-ai` value as a `recog-core` one.
#[must_use]
pub fn to_core(value: &Json) -> Value {
    pyjson::loads(&value.dumps(None)).unwrap_or(Value::Null)
}

/// A `recog-core` value as a `recog-ai` one.
#[must_use]
pub fn to_ai(value: &Value) -> Json {
    Json::parse(&pyjson::dumps(value, None)).unwrap_or(Json::Null)
}

/// An object from key/value pairs, in their order.
pub fn object<K: Into<String>>(entries: impl IntoIterator<Item = (K, Value)>) -> Value {
    let mut map = Map::new();
    for (k, v) in entries {
        map.insert(k, v);
    }
    Value::Object(map)
}

/// `Optional[str]` as JSON.
#[must_use]
pub fn opt_str(value: Option<&str>) -> Value {
    value.map_or(Value::Null, Value::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_text_given_each_writer_when_dumped_then_python_escaping_and_separators() {
        // given
        let value = pyjson::loads(r#"{"a": "Привет 😀\u007f", "b": [1, 2.5, null]}"#).unwrap();
        // when / then
        assert_eq!(
            ascii(&value),
            r#"{"a": "\u041f\u0440\u0438\u0432\u0435\u0442 \ud83d\ude00\u007f", "b": [1, 2.5, null]}"#
        );
        assert_eq!(unicode(&value), "{\"a\": \"Привет 😀\u{7f}\", \"b\": [1, 2.5, null]}");
        assert_eq!(response(&value).unwrap(), "{\"a\":\"Привет 😀\u{7f}\",\"b\":[1,2.5,null]}");
    }

    #[test]
    fn control_characters_and_quotes_given_each_writer_when_dumped_then_python_escapes() {
        let value = Value::from("q\"b\\n\nr\rt\tb\u{8}f\u{c}x\u{1}\u{1f} ~");
        let escaped = r#""q\"b\\n\nr\rt\tb\bf\fx\u0001\u001f ~""#;

        assert_eq!(ascii(&value), escaped);
        assert_eq!(response(&value).unwrap(), escaped);
        let keyed = object([("k\n", Value::from("é"))]);
        // The escape is spelled in two pieces so that no editor turns it into the letter.
        assert_eq!(ascii(&keyed), ["{\"k\\n\": \"", "\\", "u00e9\"}"].concat());
        assert_eq!(response(&keyed).unwrap(), "{\"k\\n\":\"é\"}");
    }

    #[test]
    fn nan_given_response_writer_when_dumped_then_refused_like_allow_nan_false() {
        // given
        let value = pyjson::loads(r#"{"a": [NaN]}"#).unwrap();
        // when / then
        assert_eq!(response(&value), None);
        assert_eq!(ascii(&value), r#"{"a": [NaN]}"#);
    }
}
