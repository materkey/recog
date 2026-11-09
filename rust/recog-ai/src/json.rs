//! JSON values the way Python's `json` module reads and writes them.
//!
//! The files this crate shares with Python (`ai-settings.json`, increment metadata,
//! `chat.jsonl`) must stay byte-identical, so objects keep their key order (a Python
//! dict), a repeated key keeps its first position and its last value (as `json.loads`
//! does), and [`Json::dumps`] reproduces `json.dumps(value, ensure_ascii=False)` with
//! and without `indent`. Equality follows Python's `==`: object order does not matter,
//! `1 == 1.0` and `True == 1`.

use std::fmt;

use indexmap::IndexMap;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::pytext;

/// A JSON object in insertion order.
pub type Map = IndexMap<String, Json>;

#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    /// Integers up to the range of `u64`/`i64`; serde_json reads bigger ones as floats.
    Int(i128),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Map),
}

impl Json {
    /// `json.loads` of text. Errors carry serde_json's message, not Python's.
    pub fn parse(text: &str) -> Result<Json, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Json::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Json::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The value as a Python int (`bool` is an int in Python).
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Json::Int(value) => Some(*value),
            Json::Bool(value) => Some(i128::from(*value)),
            _ => None,
        }
    }

    /// `dict.get(key)` on an object, `None` for anything else.
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_object().and_then(|map| map.get(key))
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(value) => *value,
            Json::Int(value) => *value != 0,
            Json::Float(value) => *value != 0.0,
            Json::Str(text) => !text.is_empty(),
            Json::Array(items) => !items.is_empty(),
            Json::Object(map) => !map.is_empty(),
        }
    }

    /// The name of the Python type this value loads as, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "NoneType",
            Json::Bool(_) => "bool",
            Json::Int(_) => "int",
            Json::Float(_) => "float",
            Json::Str(_) => "str",
            Json::Array(_) => "list",
            Json::Object(_) => "dict",
        }
    }

    /// `str(value)` of the loaded Python value, as an f-string prints it.
    pub fn py_str(&self) -> String {
        match self {
            Json::Str(text) => text.clone(),
            other => other.py_repr(),
        }
    }

    /// `repr(value)` of the loaded Python value.
    pub fn py_repr(&self) -> String {
        match self {
            Json::Null => "None".to_owned(),
            Json::Bool(true) => "True".to_owned(),
            Json::Bool(false) => "False".to_owned(),
            Json::Int(value) => value.to_string(),
            Json::Float(value) => py_float_repr(*value),
            Json::Str(text) => pytext::py_repr_str(text),
            Json::Array(items) => {
                let parts: Vec<String> = items.iter().map(Json::py_repr).collect();
                format!("[{}]", parts.join(", "))
            }
            Json::Object(map) => {
                let parts: Vec<String> = map
                    .iter()
                    .map(|(key, value)| format!("{}: {}", pytext::py_repr_str(key), value.py_repr()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }

    /// `json.dumps(self, ensure_ascii=False, indent=indent)`.
    pub fn dumps(&self, indent: Option<usize>) -> String {
        let mut out = String::new();
        self.write(&mut out, indent, 0);
        out
    }

    fn write(&self, out: &mut String, indent: Option<usize>, level: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Int(value) => out.push_str(&value.to_string()),
            Json::Float(value) => out.push_str(&py_float_repr(*value)),
            Json::Str(text) => write_string(out, text),
            Json::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    separate(out, indent, level + 1, index == 0);
                    item.write(out, indent, level + 1);
                }
                close(out, indent, level);
                out.push(']');
            }
            Json::Object(map) => {
                if map.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (index, (key, value)) in map.iter().enumerate() {
                    separate(out, indent, level + 1, index == 0);
                    write_string(out, key);
                    out.push_str(": ");
                    value.write(out, indent, level + 1);
                }
                close(out, indent, level);
                out.push('}');
            }
        }
    }
}

/// The separator before an item: `", "` without indent, `",\n" + spaces` with it.
fn separate(out: &mut String, indent: Option<usize>, level: usize, first: bool) {
    match indent {
        None => {
            if !first {
                out.push_str(", ");
            }
        }
        Some(width) => {
            if !first {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&" ".repeat(width * level));
        }
    }
}

fn close(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(width) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(width * level));
    }
}

/// `json.encoder.py_encode_basestring`: only `"`, `\` and control characters escaped.
fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// `float.__repr__`: the shortest digits that round-trip, in exponent notation when the
/// decimal exponent is below -4 or at least 16.
pub fn py_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity".to_owned() } else { "-Infinity".to_owned() };
    }
    // `{:e}` prints the shortest round-trip digits: "1.5e16", "-5e-1", "0e0".
    let formatted = format!("{value:e}");
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|ch| *ch != '.').collect();
    if !(-4..16).contains(&exponent) {
        let (head, tail) = digits.split_at(1);
        let fraction = if tail.is_empty() { String::new() } else { format!(".{tail}") };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{head}{fraction}e{exponent_sign}{:02}", exponent.abs());
    }
    if exponent < 0 {
        let zeros = "0".repeat((-exponent - 1) as usize);
        return format!("{sign}0.{zeros}{digits}");
    }
    let point = exponent as usize + 1;
    if digits.len() <= point {
        let zeros = "0".repeat(point - digits.len());
        format!("{sign}{digits}{zeros}.0")
    } else {
        let (whole, fraction) = digits.split_at(point);
        format!("{sign}{whole}.{fraction}")
    }
}

impl PartialEq for Json {
    fn eq(&self, other: &Json) -> bool {
        match (self, other) {
            (Json::Null, Json::Null) => true,
            (Json::Str(a), Json::Str(b)) => a == b,
            (Json::Array(a), Json::Array(b)) => a == b,
            (Json::Object(a), Json::Object(b)) => a == b,
            (Json::Float(a), Json::Float(b)) => a == b,
            (Json::Float(a), other) | (other, Json::Float(a)) => other.as_int().is_some_and(|b| *a == b as f64),
            (a, b) => match (a.as_int(), b.as_int()) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        }
    }
}

static NULL: Json = Json::Null;

/// `value[key]` of an object; `Null` for a missing key or a value that is no object.
impl std::ops::Index<&str> for Json {
    type Output = Json;

    fn index(&self, key: &str) -> &Json {
        self.get(key).unwrap_or(&NULL)
    }
}

impl From<&str> for Json {
    fn from(text: &str) -> Json {
        Json::Str(text.to_owned())
    }
}

impl From<String> for Json {
    fn from(text: String) -> Json {
        Json::Str(text)
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Json {
        Json::Bool(value)
    }
}

impl From<i64> for Json {
    fn from(value: i64) -> Json {
        Json::Int(i128::from(value))
    }
}

impl From<Option<String>> for Json {
    fn from(value: Option<String>) -> Json {
        value.map_or(Json::Null, Json::Str)
    }
}

impl From<Vec<String>> for Json {
    fn from(values: Vec<String>) -> Json {
        Json::Array(values.into_iter().map(Json::Str).collect())
    }
}

impl From<Map> for Json {
    fn from(map: Map) -> Json {
        Json::Object(map)
    }
}

/// An object from its entries, in order.
pub fn object<K: Into<String>>(entries: impl IntoIterator<Item = (K, Json)>) -> Json {
    Json::Object(entries.into_iter().map(|(key, value)| (key.into(), value)).collect())
}

/// An object of strings, in order.
pub fn string_map(entries: &IndexMap<String, String>) -> Json {
    Json::Object(entries.iter().map(|(key, value)| (key.clone(), Json::Str(value.clone()))).collect())
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Json, E> {
        Ok(Json::Int(i128::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Int(i128::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Json, E> {
        Ok(Json::Float(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Json, E> {
        Ok(Json::Str(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Json, E> {
        Ok(Json::Str(value))
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Json, A::Error> {
        let mut map = Map::new();
        while let Some((key, value)) = access.next_entry::<String, Json>()? {
            // A repeated key keeps its first position and takes the last value: a dict.
            map.insert(key, value);
        }
        Ok(Json::Object(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_given_python_repr_rules_when_printed_then_python_text() {
        let cases = [
            (5.0, "5.0"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (123456789012345678.0, "1.2345678901234568e+17"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (1234.5, "1234.5"),
            (9999999999999998.0, "9999999999999998.0"),
        ];
        for (value, text) in cases {
            assert_eq!(py_float_repr(value), text, "{value}");
        }
    }

    #[test]
    fn nested_object_given_indent_when_dumped_then_python_layout() {
        let value = Json::parse(r#"{"a": [], "b": {}, "c": [1, {"x": 1.5e16}], "d": " \u007f\u001f"}"#).unwrap();
        assert_eq!(
            value.dumps(Some(2)),
            "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    {\n      \"x\": 1.5e+16\n    }\n  ],\n  \"d\": \"\u{2028}\u{7f}\\u001f\"\n}"
        );
        assert_eq!(
            value.dumps(None),
            "{\"a\": [], \"b\": {}, \"c\": [1, {\"x\": 1.5e+16}], \"d\": \"\u{2028}\u{7f}\\u001f\"}"
        );
    }

    #[test]
    fn repeated_key_given_json_text_when_parsed_then_first_position_last_value() {
        let value = Json::parse(r#"{"a": 1, "b": 2, "a": 3}"#).unwrap();
        assert_eq!(value.dumps(None), r#"{"a": 3, "b": 2}"#);
    }

    #[test]
    fn loaded_values_given_repr_when_printed_then_python_text() {
        let value = Json::parse(
            r#"{"n": null, "t": true, "f": false, "i": -7, "x": 0.5, "s": "it's", "a": [1, "b"], "o": {}}"#,
        )
        .unwrap();
        assert_eq!(
            value.py_repr(),
            r#"{'n': None, 't': True, 'f': False, 'i': -7, 'x': 0.5, 's': "it's", 'a': [1, 'b'], 'o': {}}"#
        );
        assert_eq!(Json::Array(Vec::new()).py_repr(), "[]");
        assert_eq!(Json::from("plain").py_str(), "plain");
        assert_eq!(Json::Null.py_str(), "None");
    }

    #[test]
    fn every_variant_given_type_name_when_asked_then_python_type() {
        let value = Json::parse(r#"[null, true, 1, 1.5, "s", [], {}]"#).unwrap();
        let names: Vec<&str> = value.as_array().unwrap().iter().map(Json::type_name).collect();
        assert_eq!(names, ["NoneType", "bool", "int", "float", "str", "list", "dict"]);
    }

    #[test]
    fn escapes_given_every_short_form_when_dumped_then_python_escapes() {
        let text = "q\"b\\n\nr\rt\tb\u{8}f\u{c}c\u{1}é";
        assert_eq!(Json::from(text).dumps(None), r#""q\"b\\n\nr\rt\tb\bf\fc\u0001é""#);
    }

    #[test]
    fn special_and_signed_floats_given_repr_when_printed_then_python_text() {
        let cases = [
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (-0.5, "-0.5"),
            (-1.5e-7, "-1.5e-07"),
            (2.5e300, "2.5e+300"),
            (0.00012, "0.00012"),
        ];
        for (value, text) in cases {
            assert_eq!(py_float_repr(value), text, "{value}");
        }
    }

    #[test]
    fn mixed_kinds_given_python_equality_when_compared_then_only_equal_values_match() {
        assert_eq!(Json::Null, Json::Null);
        assert_eq!(Json::from("a"), Json::from("a"));
        assert_ne!(Json::from("a"), Json::from("b"));
        assert_eq!(Json::parse("[1, 2]").unwrap(), Json::parse("[1.0, 2]").unwrap());
        assert_ne!(Json::parse("[1, 2]").unwrap(), Json::parse("[2, 1]").unwrap());
        assert_eq!(Json::Float(0.5), Json::Float(0.5));
        assert_ne!(Json::Float(0.5), Json::Float(1.5));
        assert_eq!(Json::Bool(true), Json::Float(1.0));
        assert_ne!(Json::Float(1.5), Json::Int(1));
        assert_ne!(Json::Float(0.0), Json::Null);
        assert_ne!(Json::Null, Json::Bool(false));
        assert_ne!(Json::Array(Vec::new()), Json::Object(Map::new()));
        assert_eq!(Json::Bool(false), Json::Int(0));
    }

    #[test]
    fn numbers_given_python_equality_when_compared_then_int_float_and_bool_mix() {
        assert_eq!(Json::Int(1), Json::Float(1.0));
        assert_eq!(Json::Bool(true), Json::Int(1));
        assert_ne!(Json::Str("1".into()), Json::Int(1));
        assert_eq!(Json::parse(r#"{"a": 1, "b": 2}"#).unwrap(), Json::parse(r#"{"b": 2, "a": 1}"#).unwrap());
    }
}
