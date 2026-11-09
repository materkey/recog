//! What `recog/zoom_app.py` does with strings and JSON values, where Rust differs.
//!
//! The helper's snapshot is read as Python reads a `json.loads` result: `str.strip()`
//! removes four separator controls that Rust's `trim()` keeps, a falsy value counts as
//! absent, `True == 1`, and iterating a string yields its characters. Matching that
//! keeps the Rust session's frames equal to the Python one's on any line the helper
//! could write, which the differential test checks.

use std::fmt::Write as _;

use serde_json::Value;

/// `str.isspace()`: Unicode `White_Space` plus the separators U+001C to U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// Python truthiness of a JSON value: null, false, 0, "", [] and {} are false.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

/// Python `==` of two JSON values: numbers and booleans compare by value.
pub fn eq(first: &Value, second: &Value) -> bool {
    if let (Some(a), Some(b)) = (numeric(first), numeric(second)) {
        return a == b;
    }
    match (first, second) {
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| eq(x, y)),
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(key, x)| b.get(key).is_some_and(|y| eq(x, y)))
        }
        _ => first == second,
    }
}

/// What a value fails with where Python raises (`TypeError`, `AttributeError`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyError(pub String);

impl std::fmt::Display for PyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PyError {}

pub type PyResult<T> = Result<T, PyError>;

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `for item in value`: a list's items, a string's characters, a dict's keys.
pub fn items(value: &Value) -> PyResult<Vec<Value>> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::String(text) => Ok(text.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(map) => Ok(map.keys().map(|key| Value::String(key.clone())).collect()),
        other => Err(PyError(format!("'{}' object is not iterable", kind(other)))),
    }
}

/// `for item in value or []`.
pub fn items_or_empty(value: Option<&Value>) -> PyResult<Vec<Value>> {
    match value {
        Some(value) if truthy(value) => items(value),
        _ => Ok(Vec::new()),
    }
}

/// `len(value)`.
pub fn len(value: &Value) -> PyResult<usize> {
    match value {
        Value::Array(items) => Ok(items.len()),
        Value::String(text) => Ok(text.chars().count()),
        Value::Object(map) => Ok(map.len()),
        other => Err(PyError(format!("object of type '{}' has no len()", kind(other)))),
    }
}

/// `value[index]` for an index already checked against `len(value)`.
pub fn index(value: &Value, index: usize) -> PyResult<Value> {
    match value {
        Value::Array(items) => Ok(items[index].clone()),
        Value::String(text) => Ok(Value::String(text.chars().nth(index).map(String::from).unwrap_or_default())),
        Value::Object(_) => Err(PyError(format!("KeyError: {index}"))),
        other => Err(PyError(format!("'{}' object is not subscriptable", kind(other)))),
    }
}

/// The string a value must be for a `str` method, else Python's `AttributeError`.
pub fn text<'a>(value: &'a Value, method: &str) -> PyResult<&'a str> {
    match value {
        Value::String(text) => Ok(text),
        other => Err(PyError(format!("'{}' object has no attribute '{method}'", kind(other)))),
    }
}

/// `repr(float)`: the shortest round-trip digits, with an exponent outside 1e-4..1e16.
pub fn float_repr(number: f64) -> String {
    if number.is_nan() {
        return "nan".into();
    }
    if number.is_infinite() {
        return if number > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let scientific = format!("{number:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if (-4..16).contains(&exponent) {
        let plain = format!("{number}");
        return if plain.contains('.') { plain } else { plain + ".0" };
    }
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exponent.abs())
}

fn number_str(number: &serde_json::Number) -> String {
    match number.as_f64() {
        Some(float) if number.is_f64() => float_repr(float),
        _ => number.to_string(),
    }
}

/// `repr(str)`, for strings inside a container; non-printable characters other than
/// the ASCII controls are left as they are (an approximation of `str.isprintable`).
fn str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in text.chars() {
        push_escaped(&mut out, c, quote);
    }
    out.push(quote);
    out
}

/// One character of `repr(str)` quoted with `quote`.
fn push_escaped(out: &mut String, c: char, quote: char) {
    match c {
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c if c == quote => {
            out.push('\\');
            out.push(c);
        }
        c if (c as u32) < 0x20 || c as u32 == 0x7f => {
            let _ = write!(out, "\\x{:02x}", c as u32);
        }
        c => out.push(c),
    }
}

fn repr(value: &Value) -> String {
    match value {
        Value::String(text) => str_repr(text),
        Value::Array(items) => format!("[{}]", items.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => {
            let pairs: Vec<String> =
                map.iter().map(|(key, item)| format!("{}: {}", str_repr(key), repr(item))).collect();
            format!("{{{}}}", pairs.join(", "))
        }
        other => display(other),
    }
}

/// `str(value)` as an f-string prints it.
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => number_str(number),
        Value::String(text) => text.clone(),
        container => repr(container),
    }
}

/// `str(value)` of an optional value: `None` when absent.
pub fn display_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".into(), display)
}

/// `json.dumps(value, ensure_ascii=False)`: `", "` and `": "` separators, Python floats.
pub fn json_dumps(value: &Value) -> String {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PythonFormatter);
    serde::Serialize::serialize(value, &mut serializer).expect("a JSON value always serializes");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

struct PythonFormatter;

impl serde_json::ser::Formatter for PythonFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()> {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()> {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(b": ")
    }

    fn write_f64<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, value: f64) -> std::io::Result<()> {
        writer.write_all(float_repr(value).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn given_separator_controls_when_stripped_then_removed_like_python() {
        assert_eq!(strip("\u{1c} a \u{1f}\u{85}"), "a");
        assert_eq!(strip("\u{200b}a"), "\u{200b}a");
    }

    #[test]
    fn given_floats_when_formatted_then_python_repr() {
        assert_eq!(float_repr(1.0), "1.0");
        assert_eq!(float_repr(101.5), "101.5");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e-5), "1.5e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(-0.0), "-0.0");
    }

    #[test]
    fn given_values_when_compared_then_python_equality() {
        assert!(eq(&json!(true), &json!(1)));
        assert!(eq(&json!(1), &json!(1.0)));
        assert!(eq(&json!({"a": [1, false]}), &json!({"a": [1.0, 0]})));
        assert!(!eq(&json!("1"), &json!(1)));
    }

    #[test]
    fn given_values_when_displayed_then_python_str() {
        assert_eq!(display(&json!(null)), "None");
        assert_eq!(display(&json!([])), "[]");
        assert_eq!(display(&json!({"a": "it's"})), "{'a': \"it's\"}");
        assert_eq!(display(&json!(2.5)), "2.5");
    }

    #[test]
    fn given_strings_with_escapes_when_displayed_in_a_container_then_python_repr() {
        assert_eq!(
            display(&json!(["a\\b", "x\ny\rz\tw", "\u{1}\u{7f}", "é"])),
            r"['a\\b', 'x\ny\rz\tw', '\x01\x7f', 'é']"
        );
        assert_eq!(display(&json!(["it's"])), "[\"it's\"]");
        assert_eq!(display(&json!(["it's \"q\""])), r#"['it\'s "q"']"#);
        assert_eq!(display(&json!(["say \"hi\""])), r#"['say "hi"']"#);
        assert_eq!(display(&json!({"k": [null, true, 1.5, 2]})), "{'k': [None, True, 1.5, 2]}");
    }

    #[test]
    fn given_a_value_when_dumped_then_python_separators_and_raw_unicode() {
        assert_eq!(json_dumps(&json!({"b": [1, "Иван\n"], "a": 0.5})), "{\"b\": [1, \"Иван\\n\"], \"a\": 0.5}");
    }
}
