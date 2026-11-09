//! Starlette's `JSONResponse.render`: `json.dumps(content, ensure_ascii=False,
//! allow_nan=False, indent=None, separators=(",", ":"))`.

use std::fmt::Write as _;

use recog_core::py;
use recog_core::pyjson::Value;

/// The body, or `None` where `allow_nan=False` makes Python raise (a NaN or an
/// infinite float), which the server answers with a 500.
#[must_use]
pub fn render(value: &Value) -> Option<String> {
    let mut out = String::new();
    write_value(&mut out, value).then_some(out)
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
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
        Value::Float(x) => {
            if !x.is_finite() {
                return false;
            }
            out.push_str(&py::float_repr(*x));
        }
        Value::Str(s) => write_string(out, s),
        Value::Array(items) => {
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
        }
        Value::Object(map) => {
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
        }
    }
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
