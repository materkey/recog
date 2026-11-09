//! Frames as Python's `json` builds and reads them, on `recog_core::pyjson`.
//!
//! The bridge reads frames with `json.loads` and touches their fields the way Python
//! does (`data.get(key)`, `isinstance(x, int)` where a bool is an int, truthiness), so
//! the port keeps Python's values rather than a typed schema: a frame a typed parser
//! would refuse is handled, or refused, at the same field as in Python.

use std::fmt::Write as _;

use recog_core::pyjson::{self, Map, Value};

/// Builds a `Value::Object` from `key => value` pairs, in order.
#[macro_export]
macro_rules! obj {
    ($($key:expr => $value:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut map = ::recog_core::pyjson::Map::new();
        $(map.insert($key, ::recog_core::pyjson::Value::from($value));)*
        ::recog_core::pyjson::Value::Object(map)
    }};
}

/// `json.dumps(value, ensure_ascii=False)`.
#[must_use]
pub fn dumps(value: &Value) -> String {
    pyjson::dumps(value, None)
}

/// `json.dumps(value)`: non-ASCII characters escaped as `\uXXXX` (UTF-16 surrogate
/// pairs beyond the BMP). Outside strings the JSON text is ASCII, so escaping every
/// non-ASCII character of the output is what Python does.
#[must_use]
pub fn dumps_ascii(value: &Value) -> String {
    let text = dumps(value);
    if text.is_ascii() {
        return text;
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

/// `{**a, **b}`: `b`'s keys update `a` in place, new ones appended.
#[must_use]
pub fn merged(a: &Map, b: &Map) -> Map {
    let mut out = a.clone();
    for (k, v) in b.iter() {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// `data.get(key)` of a dict; None for an absent key.
#[must_use]
pub fn get<'a>(data: &'a Map, key: &str) -> Option<&'a Value> {
    data.get(key)
}

/// `isinstance(v, str) and v` → the string.
#[must_use]
pub fn nonempty_str(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::Str(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// `isinstance(v, int)`: a bool is an int in Python.
#[must_use]
pub fn is_int(value: &Value) -> bool {
    matches!(value, Value::Int(_) | Value::Bool(_))
}

/// `isinstance(v, (int, float))`.
#[must_use]
pub fn is_number(value: &Value) -> bool {
    matches!(value, Value::Int(_) | Value::Float(_) | Value::Bool(_))
}

/// The int value of an int or bool.
#[must_use]
pub fn as_int(value: &Value) -> Option<i128> {
    match value {
        Value::Int(n) => Some(*n),
        Value::Bool(b) => Some(i128::from(*b)),
        _ => None,
    }
}

/// The value of a number, as Python's arithmetic sees it.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(n) => Some(*n as f64),
        Value::Float(x) => Some(*x),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

/// `str(value)`, the `str()` Python applies to frame fields.
#[must_use]
pub fn py_str(value: &Value) -> String {
    value.py_str()
}

/// A float as Python's arithmetic would hand it on: `x * 1000` stays a float.
#[must_use]
pub fn float(x: f64) -> Value {
    Value::Float(x)
}

/// `TypeError` text of an arithmetic operator applied to an unsupported operand.
#[must_use]
pub fn operand_error(op: &str, left: &Value, right: &str) -> String {
    format!("unsupported operand type(s) for {op}: '{}' and '{right}'", left.type_name())
}

/// `'<type>' object has no attribute '<name>'`.
#[must_use]
pub fn attribute_error(value: &Value, name: &str) -> String {
    format!("'{}' object has no attribute '{name}'", value.type_name())
}

/// What `for x in value` yields, or Python's `TypeError` for a non-iterable.
///
/// # Errors
/// The value is not iterable (`None`, a number, a bool).
pub fn iterate(value: &Value) -> Result<Vec<Value>, String> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().map(|k| Value::from(k.as_str())).collect()),
        Value::Str(s) => Ok(s.chars().map(|c| Value::from(c.to_string())).collect()),
        other => Err(format!("'{}' object is not iterable", other.type_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_cyrillic_and_emoji_when_dumped_ascii_then_python_escapes() {
        // given
        let value = obj! {"text" => "Привет 🙂", "n" => 1i64};
        // when
        let text = dumps_ascii(&value);
        // then
        assert_eq!(text, r#"{"text": "\u041f\u0440\u0438\u0432\u0435\u0442 \ud83d\ude42", "n": 1}"#);
    }

    #[test]
    fn given_values_when_iterated_then_python_iteration_rules() {
        assert_eq!(iterate(&Value::from("ab")).unwrap(), vec![Value::from("a"), Value::from("b")]);
        assert_eq!(iterate(&obj! {"k" => 1i64}).unwrap(), vec![Value::from("k")]);
        assert_eq!(iterate(&Value::Null).unwrap_err(), "'NoneType' object is not iterable");
        assert_eq!(iterate(&Value::Int(3)).unwrap_err(), "'int' object is not iterable");
    }

    #[test]
    fn given_bools_when_typed_then_they_count_as_ints() {
        assert!(is_int(&Value::Bool(true)) && is_number(&Value::Bool(false)));
        assert_eq!(as_int(&Value::Bool(true)), Some(1));
        assert!(!is_int(&Value::Float(1.0)));
    }
}
