//! What the Python channel's tool results and argument errors get from pydantic.
//!
//! FastMCP returns a tool's dict as `pydantic_core.to_json(result, indent=2)` text, and
//! validates the arguments against a pydantic model built from the function signature
//! (in lax mode, after `pre_parse_json`); a failure becomes the text of the
//! `ValidationError`. The channel's four tools take only `str`, `int`, `bool` and their
//! `Optional`s, so this covers exactly those, with pydantic 2.13's wording.
//!
//! Known differences: an integer outside i128 (from a string such as `"9" * 40`) fails
//! as too large here, while pydantic keeps it.

use std::fmt::Write as _;

use recog_core::py;
use recog_core::pyjson::{self, Map, Value};

/// The pydantic release whose error links the messages carry.
pub const PYDANTIC_ERRORS_VERSION: &str = "2.13";

/// `pydantic_core.to_json(value, indent=2).decode()`.
pub fn to_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out
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

/// A float as pydantic writes it: Python's `repr`, with the exponent unpadded
/// (`1.5e-7`, not `1.5e-07`), and `NaN`/`Infinity` for the non-finite values.
pub fn float_text(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let repr = py::float_repr(x);
    match repr.split_once('e') {
        Some((mantissa, exponent)) => {
            let (sign, digits) = exponent.split_at(1);
            format!("{mantissa}e{sign}{}", digits.trim_start_matches('0'))
        }
        None => repr,
    }
}

fn newline(out: &mut String, level: usize) {
    out.push('\n');
    out.push_str(&"  ".repeat(level));
}

fn write_value(out: &mut String, value: &Value, level: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(x) => out.push_str(&float_text(*x)),
        Value::Str(s) => write_string(out, s),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                write_value(out, item, level + 1);
            }
            newline(out, level);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                write_string(out, key);
                out.push_str(": ");
                write_value(out, item, level + 1);
            }
            newline(out, level);
            out.push('}');
        }
    }
}

/// The type a tool parameter declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Str,
    Int,
    Bool,
}

/// One parameter of a tool, in signature order.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub name: &'static str,
    pub kind: Kind,
    /// `Optional[...]`: `None` is a value.
    pub optional: bool,
    /// The default, as JSON text; `None` for a required parameter.
    pub default: Option<&'static str>,
}

struct Failure {
    field: &'static str,
    kind: &'static str,
    message: &'static str,
    input: Value,
}

/// `repr()` of an input as pydantic shows it: over 50 bytes, the first 25 and the last
/// 24 around `...`, cut at character boundaries.
pub fn input_repr(value: &Value) -> String {
    let repr = value.py_repr();
    if repr.len() <= 50 {
        return repr;
    }
    let mut head = 25;
    while !repr.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = repr.len() - 24;
    while !repr.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}...{}", &repr[..head], &repr[tail..])
}

/// pydantic's lax `str` → `int`: surrounding whitespace, a sign, digits with single
/// underscores between them, and an all-zero fraction.
fn parse_int(text: &str) -> Option<i128> {
    let text = text.trim();
    let (sign, rest) = match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (rest, None),
    };
    if let Some(fraction) = fraction
        && (fraction.is_empty() || !fraction.bytes().all(|b| b == b'0'))
    {
        return None;
    }
    let bytes = whole.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() || !bytes[bytes.len() - 1].is_ascii_digit() {
        return None;
    }
    let mut digits = String::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'0'..=b'9' => digits.push(char::from(b)),
            b'_' if bytes[i - 1].is_ascii_digit() && bytes[i + 1].is_ascii_digit() => {}
            _ => return None,
        }
    }
    digits.parse::<i128>().ok().map(|n| sign * n)
}

fn validate(field: &Field, value: &Value) -> Result<Value, Failure> {
    let fail = |kind, message| Failure { field: field.name, kind, message, input: value.clone() };
    if field.optional && value.is_null() {
        return Ok(Value::Null);
    }
    match field.kind {
        Kind::Str => match value {
            Value::Str(_) => Ok(value.clone()),
            _ => Err(fail("string_type", "Input should be a valid string")),
        },
        Kind::Int => match value {
            Value::Int(n) => Ok(Value::Int(*n)),
            Value::Bool(b) => Ok(Value::Int(i128::from(*b))),
            Value::Float(x) if !x.is_finite() => Err(fail("finite_number", "Input should be a finite number")),
            Value::Float(x) if x.fract() != 0.0 => {
                Err(fail("int_from_float", "Input should be a valid integer, got a number with a fractional part"))
            }
            #[allow(clippy::cast_possible_truncation)]
            Value::Float(x) if x.abs() < 9_223_372_036_854_775_808.0 => Ok(Value::Int(*x as i128)),
            Value::Float(_) => {
                Err(fail("int_parsing_size", "Unable to parse input string as an integer, exceeded maximum size"))
            }
            Value::Str(s) => parse_int(s).map(Value::Int).ok_or_else(|| {
                fail("int_parsing", "Input should be a valid integer, unable to parse string as an integer")
            }),
            _ => Err(fail("int_type", "Input should be a valid integer")),
        },
        Kind::Bool => {
            let parsing = || fail("bool_parsing", "Input should be a valid boolean, unable to interpret input");
            match value {
                Value::Bool(b) => Ok(Value::Bool(*b)),
                Value::Int(0) => Ok(Value::Bool(false)),
                Value::Int(1) => Ok(Value::Bool(true)),
                Value::Int(n) if i64::try_from(*n).is_ok() => Err(parsing()),
                Value::Float(x) if *x == 0.0 => Ok(Value::Bool(false)),
                Value::Float(x) if *x == 1.0 => Ok(Value::Bool(true)),
                // A whole float reads as an integer other than 0 and 1; a fraction is
                // not a boolean at all, nor is a float beyond i64.
                Value::Float(x) if x.fract() == 0.0 && x.abs() < 9_223_372_036_854_775_808.0 => Err(parsing()),
                Value::Str(s) => match s.to_lowercase().as_str() {
                    "0" | "off" | "f" | "false" | "n" | "no" => Ok(Value::Bool(false)),
                    "1" | "on" | "t" | "true" | "y" | "yes" => Ok(Value::Bool(true)),
                    _ => Err(parsing()),
                },
                _ => Err(fail("bool_type", "Input should be a valid boolean")),
            }
        }
    }
}

/// FastMCP's `pre_parse_json`: a string argument for a parameter not declared `str`
/// that parses as JSON other than a string or a number becomes that value.
fn pre_parse_json(fields: &[Field], arguments: &Map) -> Map {
    let mut data = arguments.clone();
    for (key, value) in arguments.iter() {
        let Some(field) = fields.iter().find(|f| f.name == key) else { continue };
        let declared_str = field.kind == Kind::Str && !field.optional;
        let Value::Str(text) = value else { continue };
        if declared_str {
            continue;
        }
        match pyjson::loads(text) {
            Ok(Value::Str(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_)) | Err(_) => {}
            Ok(parsed) => data.insert(key.clone(), parsed),
        }
    }
    data
}

/// The arguments of `model` validated as FastMCP validates them: each parameter's
/// value (or default), in signature order, or the `ValidationError` text.
pub fn validate_arguments(model: &str, fields: &[Field], arguments: &Map) -> Result<Map, String> {
    let data = pre_parse_json(fields, arguments);
    let mut out = Map::new();
    let mut failures = Vec::new();
    for field in fields {
        match data.get(field.name) {
            Some(value) => match validate(field, value) {
                Ok(value) => out.insert(field.name, value),
                Err(failure) => failures.push(failure),
            },
            None => match field.default {
                Some(default) => out.insert(field.name, pyjson::loads(default).unwrap_or(Value::Null)),
                None => failures.push(Failure {
                    field: field.name,
                    kind: "missing",
                    message: "Field required",
                    input: Value::Object(data.clone()),
                }),
            },
        }
    }
    if failures.is_empty() {
        return Ok(out);
    }
    let count = failures.len();
    let mut text = format!("{count} validation error{} for {model}", if count == 1 { "" } else { "s" });
    for failure in failures {
        let _ = write!(
            text,
            "\n{}\n  {} [type={}, input_value={}, input_type={}]\n    For further information visit \
             https://errors.pydantic.dev/{PYDANTIC_ERRORS_VERSION}/v/{}",
            failure.field,
            failure.message,
            failure.kind,
            input_repr(&failure.input),
            failure.input.type_name(),
            failure.kind
        );
    }
    Err(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(text: &str) -> Map {
        pyjson::loads(text).unwrap().as_object().unwrap().clone()
    }

    #[test]
    fn nested_dict_when_serialised_then_indented_like_pydantic() {
        // given
        let value = pyjson::loads(r#"{"ok": true, "event": {"a": [1, 2.5, {}], "b": []}, "x": null}"#).unwrap();
        // when
        let text = to_json(&value);
        // then
        assert_eq!(
            text,
            "{\n  \"ok\": true,\n  \"event\": {\n    \"a\": [\n      1,\n      2.5,\n      {}\n    ],\n    \"b\": []\n  },\n  \"x\": null\n}"
        );
    }

    #[test]
    fn floats_when_serialised_then_exponent_is_unpadded() {
        // given / when / then
        assert_eq!(float_text(1.5e-7), "1.5e-7");
        assert_eq!(float_text(1e20), "1e+20");
        assert_eq!(float_text(1e15), "1000000000000000.0");
        assert_eq!(float_text(f64::NAN), "NaN");
    }

    #[test]
    fn long_input_when_shown_then_cut_around_dots_at_character_boundaries() {
        // given
        let ascii = Value::from("y".repeat(49).as_str());
        let wide = Value::from("é".repeat(60).as_str());
        // when / then
        assert_eq!(input_repr(&Value::from("y".repeat(48).as_str())).len(), 50);
        assert_eq!(input_repr(&ascii), format!("'{}...{}'", "y".repeat(24), "y".repeat(23)));
        assert_eq!(input_repr(&wide), format!("'{}...{}'", "é".repeat(12), "é".repeat(11)));
    }

    #[test]
    fn wrong_and_missing_arguments_when_validated_then_pydantic_message() {
        // given
        let fields = [
            Field { name: "kind", kind: Kind::Str, optional: false, default: None },
            Field { name: "text", kind: Kind::Str, optional: false, default: None },
        ];
        // when
        let error = validate_arguments("send_copilot_messageArguments", &fields, &object(r#"{"kind": 5}"#));
        // then
        assert_eq!(
            error.unwrap_err(),
            "2 validation errors for send_copilot_messageArguments\nkind\n  Input should be a valid string \
             [type=string_type, input_value=5, input_type=int]\n    For further information visit \
             https://errors.pydantic.dev/2.13/v/string_type\ntext\n  Field required [type=missing, \
             input_value={'kind': 5}, input_type=dict]\n    For further information visit \
             https://errors.pydantic.dev/2.13/v/missing"
        );
    }

    const STRING_TYPE: (&str, &str) = ("string_type", "Input should be a valid string");
    const INT_TYPE: (&str, &str) = ("int_type", "Input should be a valid integer");
    const INT_PARSING: (&str, &str) =
        ("int_parsing", "Input should be a valid integer, unable to parse string as an integer");
    const INT_FROM_FLOAT: (&str, &str) =
        ("int_from_float", "Input should be a valid integer, got a number with a fractional part");
    const FINITE: (&str, &str) = ("finite_number", "Input should be a finite number");
    const INT_SIZE: (&str, &str) =
        ("int_parsing_size", "Unable to parse input string as an integer, exceeded maximum size");
    const BOOL_TYPE: (&str, &str) = ("bool_type", "Input should be a valid boolean");
    const BOOL_PARSING: (&str, &str) = ("bool_parsing", "Input should be a valid boolean, unable to interpret input");

    fn checked(kind: Kind, optional: bool, value: Value) -> Result<String, (&'static str, &'static str)> {
        let field = Field { name: "f", kind, optional, default: None };
        validate(&field, &value).map(|v| pyjson::dumps(&v, None)).map_err(|failure| {
            assert_eq!(failure.field, "f");
            assert_eq!(pyjson::dumps(&failure.input, None), pyjson::dumps(&value, None));
            (failure.kind, failure.message)
        })
    }

    fn text(s: &str) -> Value {
        Value::from(s)
    }

    #[test]
    fn string_field_when_validated_then_only_strings_pass() {
        // given / when / then
        assert_eq!(checked(Kind::Str, false, text("x")), Ok("\"x\"".to_owned()));
        assert_eq!(checked(Kind::Str, false, Value::Int(5)), Err(STRING_TYPE));
        assert_eq!(checked(Kind::Str, false, Value::Null), Err(STRING_TYPE));
        assert_eq!(checked(Kind::Str, true, Value::Null), Ok("null".to_owned()));
        assert_eq!(checked(Kind::Str, true, Value::Bool(true)), Err(STRING_TYPE));
    }

    #[test]
    fn int_field_when_validated_then_numbers_and_strings_coerced_as_pydantic_lax_mode() {
        // given / when / then
        assert_eq!(checked(Kind::Int, false, Value::Int(5)), Ok("5".to_owned()));
        assert_eq!(checked(Kind::Int, false, Value::Bool(true)), Ok("1".to_owned()));
        assert_eq!(checked(Kind::Int, false, Value::Bool(false)), Ok("0".to_owned()));
        assert_eq!(checked(Kind::Int, false, Value::Float(5.0)), Ok("5".to_owned()));
        assert_eq!(checked(Kind::Int, false, Value::Float(-0.0)), Ok("0".to_owned()));
        assert_eq!(checked(Kind::Int, false, Value::Float(-9.0e18)), Ok("-9000000000000000000".to_owned()));
        assert_eq!(checked(Kind::Int, false, Value::Float(1.5)), Err(INT_FROM_FLOAT));
        assert_eq!(checked(Kind::Int, false, Value::Float(f64::NAN)), Err(FINITE));
        assert_eq!(checked(Kind::Int, false, Value::Float(f64::NEG_INFINITY)), Err(FINITE));
        assert_eq!(checked(Kind::Int, false, Value::Float(9.3e18)), Err(INT_SIZE));
        assert_eq!(checked(Kind::Int, false, Value::Float(1e300)), Err(INT_SIZE));
        assert_eq!(checked(Kind::Int, false, text("12.000")), Ok("12".to_owned()));
        assert_eq!(checked(Kind::Int, false, text("abc")), Err(INT_PARSING));
        assert_eq!(checked(Kind::Int, false, Value::Null), Err(INT_TYPE));
        assert_eq!(checked(Kind::Int, true, Value::Null), Ok("null".to_owned()));
        assert_eq!(checked(Kind::Int, false, Value::Array(vec![Value::Int(1)])), Err(INT_TYPE));
    }

    #[test]
    fn integer_strings_when_parsed_then_only_pydantic_integer_syntax_accepted() {
        // given
        let accepted = [
            ("0", 0),
            ("-5", -5),
            ("+4", 4),
            (" 7 ", 7),
            ("1_0", 10),
            ("1_000.00", 1000),
            ("12.0", 12),
            ("99999999999999999999", 99_999_999_999_999_999_999),
        ];
        let refused = ["", "-", "1.5", "1.", ".0", "1e3", "abc", "_1", "1_", "1__0", "1_.0", "٣", "1 0", "--1"];
        // when / then
        for (input, expected) in accepted {
            assert_eq!(parse_int(input), Some(expected), "{input:?}");
        }
        for input in refused {
            assert_eq!(parse_int(input), None, "{input:?}");
        }
    }

    #[test]
    fn bool_field_when_validated_then_pydantic_truth_words_and_numbers_accepted() {
        // given / when / then
        assert_eq!(checked(Kind::Bool, false, Value::Bool(false)), Ok("false".to_owned()));
        assert_eq!(checked(Kind::Bool, false, Value::Int(0)), Ok("false".to_owned()));
        assert_eq!(checked(Kind::Bool, false, Value::Int(1)), Ok("true".to_owned()));
        assert_eq!(checked(Kind::Bool, false, Value::Int(2)), Err(BOOL_PARSING));
        assert_eq!(checked(Kind::Bool, false, Value::Int(-1)), Err(BOOL_PARSING));
        assert_eq!(checked(Kind::Bool, false, Value::Int(123_456_789_012_345_678_901)), Err(BOOL_TYPE));
        assert_eq!(checked(Kind::Bool, false, Value::Float(0.0)), Ok("false".to_owned()));
        assert_eq!(checked(Kind::Bool, false, Value::Float(1.0)), Ok("true".to_owned()));
        assert_eq!(checked(Kind::Bool, false, Value::Float(5.0)), Err(BOOL_PARSING));
        assert_eq!(checked(Kind::Bool, false, Value::Float(0.5)), Err(BOOL_TYPE));
        assert_eq!(checked(Kind::Bool, false, Value::Float(1e300)), Err(BOOL_TYPE));
        assert_eq!(checked(Kind::Bool, false, Value::Float(f64::NAN)), Err(BOOL_TYPE));
        assert_eq!(checked(Kind::Bool, false, Value::Float(f64::INFINITY)), Err(BOOL_TYPE));
        for word in ["0", "off", "f", "false", "n", "no", "OFF", "No"] {
            assert_eq!(checked(Kind::Bool, false, text(word)), Ok("false".to_owned()), "{word}");
        }
        for word in ["1", "on", "t", "true", "y", "yes", "T", "Yes"] {
            assert_eq!(checked(Kind::Bool, false, text(word)), Ok("true".to_owned()), "{word}");
        }
        assert_eq!(checked(Kind::Bool, false, text("maybe")), Err(BOOL_PARSING));
        assert_eq!(checked(Kind::Bool, false, text("")), Err(BOOL_PARSING));
        assert_eq!(checked(Kind::Bool, false, Value::Null), Err(BOOL_TYPE));
        assert_eq!(checked(Kind::Bool, true, Value::Null), Ok("null".to_owned()));
        assert_eq!(checked(Kind::Bool, false, Value::Object(Map::new())), Err(BOOL_TYPE));
    }

    #[test]
    fn strings_with_escapes_when_serialised_then_only_controls_escaped() {
        // given
        let value = Value::from("q\"b\\n\nr\rt\tb\u{8}f\u{c}c\u{1}d\u{1f}é😀\u{7f}");
        // when
        let text = to_json(&value);
        // then
        assert_eq!(text, "\"q\\\"b\\\\n\\nr\\rt\\tb\\bf\\fc\\u0001d\\u001fé😀\u{7f}\"");
    }

    #[test]
    fn scalars_when_serialised_then_pydantic_text() {
        // given / when / then
        assert_eq!(to_json(&Value::Bool(false)), "false");
        assert_eq!(to_json(&Value::Int(-123_456_789_012_345_678_901)), "-123456789012345678901");
        assert_eq!(to_json(&Value::Float(f64::INFINITY)), "Infinity");
        assert_eq!(to_json(&Value::Float(f64::NEG_INFINITY)), "-Infinity");
        assert_eq!(to_json(&Value::Float(-2.5)), "-2.5");
        assert_eq!(to_json(&Value::Float(5e-324)), "5e-324");
    }

    #[test]
    fn lax_inputs_when_validated_then_coerced_as_pydantic_does() {
        // given
        let fields = [
            Field { name: "a", kind: Kind::Int, optional: false, default: Some("180") },
            Field { name: "b", kind: Kind::Int, optional: true, default: Some("null") },
            Field { name: "c", kind: Kind::Bool, optional: false, default: Some("true") },
            Field { name: "d", kind: Kind::Str, optional: true, default: Some("null") },
        ];
        // when
        let out =
            validate_arguments("M", &fields, &object(r#"{"a": " 1_000.00 ", "b": "null", "c": "No", "d": "null"}"#));
        // then
        assert_eq!(
            pyjson::dumps(&Value::Object(out.unwrap()), None),
            r#"{"a": 1000, "b": null, "c": false, "d": null}"#
        );
    }
}
