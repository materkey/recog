//! Starlette's `JSONResponse.render`: `json.dumps(content, ensure_ascii=False,
//! allow_nan=False, indent=None, separators=(",", ":"))`.

use std::fmt::Write as _;

use recog_core::py;
use recog_core::pyjson::{Map, Value};

/// The body, or `None` where `allow_nan=False` makes Python raise (a NaN or an
/// infinite float), which the server answers with a 500.
#[must_use]
pub fn render(value: &Value) -> Option<String> {
    let mut out = String::new();
    write_value(&mut out, value).then_some(out)
}

/// The two-character escape `json.dumps` writes for a character, if it has one.
fn short_escape(c: char) -> Option<&'static str> {
    Some(match c {
        '"' => "\\\"",
        '\\' => "\\\\",
        '\n' => "\\n",
        '\r' => "\\r",
        '\t' => "\\t",
        '\u{8}' => "\\b",
        '\u{c}' => "\\f",
        _ => return None,
    })
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        if let Some(escape) = short_escape(c) {
            out.push_str(escape);
        } else if u32::from(c) < 0x20 {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, value: &Value) -> bool {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(x) => return write_float(out, *x),
        Value::Str(s) => write_string(out, s),
        Value::Array(items) => return write_array(out, items),
        Value::Object(map) => return write_object(out, map),
    }
    true
}

fn write_float(out: &mut String, x: f64) -> bool {
    if !x.is_finite() {
        return false;
    }
    out.push_str(&py::float_repr(x));
    true
}

fn write_array(out: &mut String, items: &[Value]) -> bool {
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if !write_value(out, item) {
            return false;
        }
    }
    out.push(']');
    true
}

fn write_object(out: &mut String, map: &Map) -> bool {
    out.push('{');
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_string(out, k);
        out.push(':');
        if !write_value(out, v) {
            return false;
        }
    }
    out.push('}');
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use recog_core::pyjson;

    #[test]
    fn a_document_when_rendered_then_compact_with_python_floats() {
        let value = pyjson::loads(r#"{"a": [1, 2.0, 1e-05, "ё\n"], "b": null}"#).unwrap();
        assert_eq!(render(&value).unwrap(), r#"{"a":[1,2.0,1e-05,"ё\n"],"b":null}"#);
    }

    #[test]
    fn a_nan_when_rendered_then_refused() {
        assert_eq!(render(&Value::Array(vec![Value::Float(f64::NAN)])), None);
    }

    #[test]
    fn an_infinity_inside_an_object_when_rendered_then_refused() {
        let value = pyjson::loads(r#"{"a": {"b": [1, 2]}}"#).unwrap();
        assert_eq!(render(&value).as_deref(), Some(r#"{"a":{"b":[1,2]}}"#));
        let Value::Object(mut map) = value else { panic!() };
        map.insert("c", Value::Float(f64::NEG_INFINITY));
        assert_eq!(render(&Value::Object(map)), None);
    }

    #[test]
    fn control_characters_and_quotes_when_rendered_then_jsons_escapes_and_the_rest_as_is() {
        let text = "\"\\\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f} ё/";
        assert_eq!(render(&Value::from(text)).unwrap(), "\"\\\"\\\\\\n\\r\\t\\b\\f\\u0001\\u001f\u{7f} ё/\"");
    }

    #[test]
    fn scalars_when_rendered_then_pythons_literals() {
        let value = Value::Array(vec![Value::Bool(true), Value::Bool(false), Value::Int(-7), Value::Float(-0.0)]);
        assert_eq!(render(&value).unwrap(), "[true,false,-7,-0.0]");
        assert_eq!(render(&Value::Array(Vec::new())).unwrap(), "[]");
    }
}
