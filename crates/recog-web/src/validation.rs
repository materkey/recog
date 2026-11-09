//! Request validation the way FastAPI and pydantic 2 do it, with their 422 bodies.
//!
//! Only the types the Python request models use are covered: `str` (with
//! `StringConstraints`), lax and strict `bool`, `int`, `Literal`, `Optional`, `list`,
//! `dict` and `dict[str, str]`. Values are coerced as pydantic's lax Python mode
//! coerces what `json.loads` produced (`"12"` and `1.0` are the integer 12 and 1,
//! `"yes"` is true), and every error carries pydantic's `type`, `loc`, `msg`, `input`
//! and `ctx`. Errors come in FastAPI's order: path, query, then body; inside a model in
//! field declaration order, then the forbidden extra keys in input order.

use recog_core::py;
use recog_core::pyjson::{Map, Value};

use crate::json;

/// A position in the request: `"body"`, a field name or a list index.
#[derive(Clone, Debug, PartialEq)]
pub enum Loc {
    Key(String),
    Index(usize),
}

impl From<&str> for Loc {
    fn from(key: &str) -> Self {
        Loc::Key(key.to_owned())
    }
}

/// One pydantic error.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorItem {
    pub kind: &'static str,
    pub loc: Vec<Loc>,
    pub msg: String,
    pub input: Value,
    pub ctx: Option<Value>,
}

impl ErrorItem {
    fn new(kind: &'static str, loc: &[Loc], msg: impl Into<String>, input: &Value) -> Self {
        Self { kind, loc: loc.to_vec(), msg: msg.into(), input: input.clone(), ctx: None }
    }

    /// The error as FastAPI serializes it in `{"detail": [...]}`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let loc = self
            .loc
            .iter()
            .map(|part| match part {
                Loc::Key(key) => Value::from(key.as_str()),
                Loc::Index(i) => Value::Int(i128::try_from(*i).unwrap_or(i128::MAX)),
            })
            .collect();
        let mut map = Map::new();
        map.insert("type", Value::from(self.kind));
        map.insert("loc", Value::Array(loc));
        map.insert("msg", Value::from(self.msg.as_str()));
        map.insert("input", self.input.clone());
        if let Some(ctx) = &self.ctx {
            map.insert("ctx", ctx.clone());
        }
        Value::Object(map)
    }
}

/// The 422 body of a list of errors.
#[must_use]
pub fn detail(errors: &[ErrorItem]) -> Value {
    json::object([("detail", Value::Array(errors.iter().map(ErrorItem::to_json).collect()))])
}

/// A field type.
#[derive(Clone, Debug)]
pub enum Ty {
    Str,
    /// `Annotated[str, StringConstraints(strip_whitespace, min_length, max_length)]`.
    ConStr {
        strip: bool,
        min: usize,
        max: usize,
    },
    Bool,
    /// `Field(strict=True)`: only `true` and `false`.
    StrictBool,
    Int,
    Literal(&'static [&'static str]),
    Optional(Box<Ty>),
    List(Box<Ty>),
    /// `dict` with any values.
    AnyDict,
    /// `dict[str, T]`.
    Dict(Box<Ty>),
}

/// One model field: name, type and whether it may be absent.
#[derive(Clone, Debug)]
pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
}

#[must_use]
pub fn field(name: &'static str, ty: Ty, required: bool) -> Field {
    Field { name, ty, required }
}

const BOOL_TRUE: [&str; 6] = ["1", "on", "t", "true", "y", "yes"];
const BOOL_FALSE: [&str; 6] = ["0", "off", "f", "false", "n", "no"];

/// pydantic's string-to-integer rule: surrounding whitespace, a sign, digits with
/// single underscores between them, and an optional all-zero fraction.
fn parse_int_str(raw: &str) -> Option<i128> {
    let (sign, rest) = split_sign(py::strip(raw));
    let (digits, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    if !fraction.bytes().all(|b| b == b'0') || !well_formed_digits(digits) {
        return None;
    }
    let n: i128 = digits.replace('_', "").parse().ok()?;
    Some(sign * n)
}

/// The sign of an integer's text and the rest.
fn split_sign(s: &str) -> (i128, &str) {
    match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => (1, s),
    }
}

/// Digits with single underscores between them.
fn well_formed_digits(digits: &str) -> bool {
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
}

fn parse_bool_str(raw: &str) -> Option<bool> {
    let lower = raw.to_lowercase();
    if BOOL_TRUE.contains(&lower.as_str()) {
        Some(true)
    } else if BOOL_FALSE.contains(&lower.as_str()) {
        Some(false)
    } else {
        None
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") }
}

/// Validates one value; the coerced value, or the errors pushed onto `errors`.
pub fn validate(ty: &Ty, input: &Value, loc: &[Loc], errors: &mut Vec<ErrorItem>) -> Option<Value> {
    let checked = match ty {
        Ty::Optional(inner) if !input.is_null() => return validate(inner, input, loc, errors),
        Ty::List(inner) => return validate_list(inner, input, loc, errors),
        Ty::Dict(inner) => return validate_dict(inner, input, loc, errors),
        scalar => validate_scalar(scalar, input),
    };
    match checked {
        Ok(value) => Some(value),
        Err(failure) => {
            errors.push(failure.at(loc, input));
            None
        }
    }
}

/// An error before its position and input are known.
struct Failure {
    kind: &'static str,
    msg: String,
    ctx: Option<Value>,
}

impl Failure {
    fn new(kind: &'static str, msg: impl Into<String>) -> Self {
        Self { kind, msg: msg.into(), ctx: None }
    }

    fn with_ctx(mut self, ctx: Value) -> Self {
        self.ctx = Some(ctx);
        self
    }

    fn at(self, loc: &[Loc], input: &Value) -> ErrorItem {
        ErrorItem { kind: self.kind, loc: loc.to_vec(), msg: self.msg, input: input.clone(), ctx: self.ctx }
    }
}

/// A value of a type without items: the coerced value or its error.
fn validate_scalar(ty: &Ty, input: &Value) -> Result<Value, Failure> {
    match ty {
        Ty::Str => string(input).map(|_| input.clone()),
        Ty::ConStr { strip, min, max } => constrained(input, *strip, *min, *max),
        Ty::StrictBool => strict_bool(input),
        Ty::Bool => lax_bool(input).map(Value::Bool),
        Ty::Int => lax_int(input).map(Value::Int),
        Ty::Literal(options) => literal(options, input),
        // A null `Optional`; lists and dictionaries of a type are validated by item.
        Ty::Optional(_) | Ty::List(_) | Ty::Dict(_) => Ok(Value::Null),
        Ty::AnyDict => any_dict(input),
    }
}

fn strict_bool(input: &Value) -> Result<Value, Failure> {
    match input {
        Value::Bool(_) => Ok(input.clone()),
        _ => Err(Failure::new("bool_type", "Input should be a valid boolean")),
    }
}

fn any_dict(input: &Value) -> Result<Value, Failure> {
    match input {
        Value::Object(_) => Ok(input.clone()),
        _ => Err(Failure::new("dict_type", "Input should be a valid dictionary")),
    }
}

fn string(input: &Value) -> Result<&str, Failure> {
    match input {
        Value::Str(s) => Ok(s),
        _ => Err(Failure::new("string_type", "Input should be a valid string")),
    }
}

/// `StringConstraints`: the (stripped) text within its length bounds.
fn constrained(input: &Value, strip: bool, min: usize, max: usize) -> Result<Value, Failure> {
    let s = string(input)?;
    let value = if strip { py::strip(s) } else { s };
    let length = value.chars().count();
    if length < min {
        return Err(Failure::new(
            "string_too_short",
            format!("String should have at least {}", plural(min, "character")),
        )
        .with_ctx(json::object([("min_length", Value::Int(min as i128))])));
    }
    if length > max {
        return Err(Failure::new(
            "string_too_long",
            format!("String should have at most {}", plural(max, "character")),
        )
        .with_ctx(json::object([("max_length", Value::Int(max as i128))])));
    }
    Ok(Value::from(value))
}

/// pydantic's lax boolean: `0`/`1` as numbers, the words of `BOOL_TRUE` and
/// `BOOL_FALSE`.
fn lax_bool(input: &Value) -> Result<bool, Failure> {
    let unparsable = || Failure::new("bool_parsing", "Input should be a valid boolean, unable to interpret input");
    match input {
        Value::Bool(b) => Ok(*b),
        Value::Int(n @ (0 | 1)) => Ok(*n == 1),
        Value::Int(_) => Err(unparsable()),
        Value::Float(x) if *x == 0.0 || *x == 1.0 => Ok(*x == 1.0),
        Value::Str(s) => parse_bool_str(s).ok_or_else(unparsable),
        _ => Err(Failure::new("bool_type", "Input should be a valid boolean")),
    }
}

/// pydantic's lax integer: booleans, integral finite floats and integer text.
fn lax_int(input: &Value) -> Result<i128, Failure> {
    match input {
        Value::Int(n) => Ok(*n),
        Value::Bool(b) => Ok(i128::from(*b)),
        Value::Float(x) if !x.is_finite() => Err(Failure::new("finite_number", "Input should be a finite number")),
        Value::Float(x) if x.fract() == 0.0 => Ok(*x as i128),
        Value::Float(_) => {
            Err(Failure::new("int_from_float", "Input should be a valid integer, got a number with a fractional part"))
        }
        Value::Str(s) => parse_int_str(s).ok_or_else(|| {
            Failure::new("int_parsing", "Input should be a valid integer, unable to parse string as an integer")
        }),
        _ => Err(Failure::new("int_type", "Input should be a valid integer")),
    }
}

/// One of the literal's options.
fn literal(options: &[&str], input: &Value) -> Result<Value, Failure> {
    match input {
        Value::Str(s) if options.contains(&s.as_str()) => Ok(input.clone()),
        _ => {
            let expected = options.iter().map(|o| format!("'{o}'")).collect::<Vec<_>>();
            let expected = match expected.len() {
                0 | 1 => expected.join(""),
                n => format!("{} or {}", expected[..n - 1].join(", "), expected[n - 1]),
            };
            Err(Failure::new("literal_error", format!("Input should be {expected}"))
                .with_ctx(json::object([("expected", Value::from(expected))])))
        }
    }
}

fn validate_list(inner: &Ty, input: &Value, loc: &[Loc], errors: &mut Vec<ErrorItem>) -> Option<Value> {
    let Value::Array(items) = input else {
        errors.push(ErrorItem::new("list_type", loc, "Input should be a valid list", input));
        return None;
    };
    let before = errors.len();
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let mut item_loc = loc.to_vec();
        item_loc.push(Loc::Index(i));
        if let Some(v) = validate(inner, item, &item_loc, errors) {
            out.push(v);
        }
    }
    (errors.len() == before).then_some(Value::Array(out))
}

fn validate_dict(inner: &Ty, input: &Value, loc: &[Loc], errors: &mut Vec<ErrorItem>) -> Option<Value> {
    let Value::Object(map) = input else {
        errors.push(ErrorItem::new("dict_type", loc, "Input should be a valid dictionary", input));
        return None;
    };
    let before = errors.len();
    let mut out = Map::new();
    for (k, v) in map.iter() {
        let mut item_loc = loc.to_vec();
        item_loc.push(Loc::Key(k.clone()));
        if let Some(v) = validate(inner, v, &item_loc, errors) {
            out.insert(k.clone(), v);
        }
    }
    (errors.len() == before).then_some(Value::Object(out))
}

/// Validates a model: the given fields (coerced) in declaration order, or the errors.
/// `forbid_extra` is `model_config = {"extra": "forbid"}`.
pub fn validate_model(
    input: &Value,
    fields: &[Field],
    forbid_extra: bool,
    loc: &[Loc],
    errors: &mut Vec<ErrorItem>,
) -> Option<Map> {
    let Value::Object(map) = input else {
        errors.push(ErrorItem::new(
            "model_attributes_type",
            loc,
            "Input should be a valid dictionary or object to extract fields from",
            input,
        ));
        return None;
    };
    let before = errors.len();
    let mut out = Map::new();
    for f in fields {
        let mut field_loc = loc.to_vec();
        field_loc.push(Loc::Key(f.name.to_owned()));
        match map.get(f.name) {
            Some(value) => {
                if let Some(v) = validate(&f.ty, value, &field_loc, errors) {
                    out.insert(f.name, v);
                }
            }
            None if f.required => errors.push(ErrorItem::new("missing", &field_loc, "Field required", input)),
            None => {}
        }
    }
    if forbid_extra {
        for (k, v) in map.iter() {
            if !fields.iter().any(|f| f.name == k) {
                let mut field_loc = loc.to_vec();
                field_loc.push(Loc::Key(k.clone()));
                errors.push(ErrorItem::new("extra_forbidden", &field_loc, "Extra inputs are not permitted", v));
            }
        }
    }
    (errors.len() == before).then_some(out)
}

/// A query or path parameter given as text (`None` when absent).
pub fn validate_param(
    location: &'static str,
    name: &'static str,
    ty: &Ty,
    raw: Option<&str>,
    required: bool,
    errors: &mut Vec<ErrorItem>,
) -> Option<Value> {
    let loc = [Loc::from(location), Loc::from(name)];
    match raw {
        None if required => {
            errors.push(ErrorItem::new("missing", &loc, "Field required", &Value::Null));
            None
        }
        None => None,
        Some(raw) => validate(ty, &Value::from(raw), &loc, errors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recog_core::pyjson;

    fn check(ty: &Ty, input: &str) -> Result<Value, Vec<Value>> {
        let mut errors = Vec::new();
        let value = pyjson::loads(input).unwrap();
        validate(ty, &value, &[], &mut errors).ok_or_else(|| errors.iter().map(ErrorItem::to_json).collect())
    }

    fn kind(result: Result<Value, Vec<Value>>) -> String {
        match result {
            Ok(v) => pyjson::dumps(&v, None),
            Err(e) => e[0].as_object().unwrap().get("type").unwrap().py_str(),
        }
    }

    #[test]
    fn integers_given_lax_python_mode_when_validated_then_pydantic_coercions() {
        // given / when / then
        for (input, expected) in [
            ("1", "1"),
            ("1.0", "1"),
            ("1.5", "int_from_float"),
            ("true", "1"),
            ("\"12\"", "12"),
            ("\" 12 \"", "12"),
            ("\"1_000\"", "1000"),
            ("\"+3\"", "3"),
            ("\"-3\"", "-3"),
            ("\"1e3\"", "int_parsing"),
            ("\"\"", "int_parsing"),
            ("\"12.0\"", "12"),
            ("\"12.5\"", "int_parsing"),
            ("null", "int_type"),
            ("[1]", "int_type"),
            ("Infinity", "finite_number"),
        ] {
            assert_eq!(kind(check(&Ty::Int, input)), expected, "{input}");
        }
    }

    #[test]
    fn booleans_given_lax_and_strict_modes_when_validated_then_pydantic_coercions() {
        // given / when / then
        for (input, expected) in [
            ("true", "true"),
            ("1", "true"),
            ("0", "false"),
            ("2", "bool_parsing"),
            ("1.0", "true"),
            ("0.5", "bool_type"),
            ("\"TRUE\"", "true"),
            ("\"off\"", "false"),
            ("\" true\"", "bool_parsing"),
            ("null", "bool_type"),
        ] {
            assert_eq!(kind(check(&Ty::Bool, input)), expected, "{input}");
        }
        assert_eq!(kind(check(&Ty::StrictBool, "\"false\"")), "bool_type");
    }

    #[test]
    fn missing_and_extra_fields_given_model_when_validated_then_errors_in_fastapi_order() {
        // given
        let input = pyjson::loads(r#"{"z": 2, "models": {"a": 1}, "revision": null}"#).unwrap();
        let fields = [field("models", Ty::Dict(Box::new(Ty::Str)), true), field("revision", Ty::Str, true)];
        let mut errors = Vec::new();
        // when
        let out = validate_model(&input, &fields, true, &[Loc::from("body")], &mut errors);
        // then
        assert!(out.is_none());
        let body = json::response(&detail(&errors)).unwrap();
        assert_eq!(
            body,
            r#"{"detail":[{"type":"string_type","loc":["body","models","a"],"msg":"Input should be a valid string","input":1},{"type":"string_type","loc":["body","revision"],"msg":"Input should be a valid string","input":null},{"type":"extra_forbidden","loc":["body","z"],"msg":"Extra inputs are not permitted","input":2}]}"#
        );
    }

    #[test]
    fn constrained_text_given_blank_or_long_input_when_validated_then_length_errors_with_ctx() {
        // given
        let ty = Ty::ConStr { strip: true, min: 1, max: 3 };
        // when / then
        assert_eq!(
            check(&ty, "\"  \"").unwrap_err()[0],
            pyjson::loads(r#"{"type": "string_too_short", "loc": [], "msg": "String should have at least 1 character", "input": "  ", "ctx": {"min_length": 1}}"#).unwrap()
        );
        assert_eq!(kind(check(&ty, "\"abcd\"")), "string_too_long");
        assert_eq!(check(&ty, "\" ab \"").unwrap(), Value::from("ab"));
    }

    #[test]
    fn constrained_text_without_strip_or_of_another_type_when_validated_then_counted_as_given() {
        let ty = Ty::ConStr { strip: false, min: 2, max: 4 };

        assert_eq!(check(&ty, "\" ab \"").unwrap(), Value::from(" ab "));
        let short = check(&ty, "\"a\"").unwrap_err();
        assert_eq!(text(&short[0], "msg"), "String should have at least 2 characters");
        let long = check(&ty, "\"abcde\"").unwrap_err();
        assert_eq!(text(&long[0], "msg"), "String should have at most 4 characters");
        assert_eq!(pyjson::dumps(long[0].as_object().unwrap().get("ctx").unwrap(), None), r#"{"max_length": 4}"#);
        assert_eq!(kind(check(&ty, "5")), "string_type");
        assert_eq!(kind(check(&Ty::Str, "5")), "string_type");
        assert_eq!(kind(check(&Ty::Str, "\"x\"")), "\"x\"");
    }

    fn text(error: &Value, key: &str) -> String {
        error.as_object().unwrap().get(key).unwrap().py_str()
    }

    #[test]
    fn literal_with_one_or_several_options_when_validated_then_pydantic_expected_text() {
        for (options, expected) in [
            (&["a"][..], "'a'"),
            (&["a", "b"][..], "'a' or 'b'"),
            (&["a", "b", "c"][..], "'a', 'b' or 'c'"),
            (&[][..], ""),
        ] {
            let errors = check(&Ty::Literal(options), "\"z\"").unwrap_err();

            assert_eq!(text(&errors[0], "msg"), format!("Input should be {expected}"), "{options:?}");
            assert_eq!(text(&errors[0], "type"), "literal_error");
        }
        assert_eq!(check(&Ty::Literal(&["a", "b"]), "\"b\"").unwrap(), Value::from("b"));
        assert_eq!(kind(check(&Ty::Literal(&["1"]), "1")), "literal_error");
    }

    #[test]
    fn containers_with_bad_items_when_validated_then_an_error_per_item_at_its_position() {
        let list = Ty::List(Box::new(Ty::Int));
        let errors = check(&list, r#"[1, "x", 3, null]"#).unwrap_err();
        let locs: Vec<String> =
            errors.iter().map(|e| pyjson::dumps(e.as_object().unwrap().get("loc").unwrap(), None)).collect();
        assert_eq!(locs, ["[1]", "[3]"]);
        assert_eq!(check(&list, r#"[1, "2"]"#).unwrap(), pyjson::loads("[1, 2]").unwrap());
        assert_eq!(kind(check(&list, "{}")), "list_type");

        assert_eq!(kind(check(&Ty::AnyDict, "[]")), "dict_type");
        assert_eq!(kind(check(&Ty::AnyDict, r#"{"a": [1]}"#)), r#"{"a": [1]}"#);
        assert_eq!(kind(check(&Ty::Dict(Box::new(Ty::Str)), "5")), "dict_type");
        assert_eq!(kind(check(&Ty::Dict(Box::new(Ty::Str)), r#"{"a": "b"}"#)), r#"{"a": "b"}"#);
        assert_eq!(kind(check(&Ty::Optional(Box::new(Ty::Int)), "null")), "null");
        assert_eq!(kind(check(&Ty::Optional(Box::new(Ty::Int)), "\"x\"")), "int_parsing");
    }

    #[test]
    fn integer_text_with_misplaced_underscores_or_signs_when_validated_then_int_parsing() {
        for input in ["\"_1\"", "\"1_\"", "\"1__0\"", "\"-\"", "\"+\"", "\"1-2\"", "\"0x10\"", "\".0\""] {
            assert_eq!(kind(check(&Ty::Int, input)), "int_parsing", "{input}");
        }
        assert_eq!(kind(check(&Ty::Int, "\"-1_0.00\"")), "-10");
        assert_eq!(kind(check(&Ty::Int, "\"7.\"")), "7");
    }

    #[test]
    fn query_parameter_when_absent_or_given_then_missing_only_when_required() {
        let mut errors = Vec::new();

        assert_eq!(validate_param("query", "q", &Ty::Int, None, false, &mut errors), None);
        assert!(errors.is_empty());
        assert_eq!(validate_param("query", "q", &Ty::Int, Some("4"), true, &mut errors), Some(Value::Int(4)));
        assert_eq!(validate_param("query", "q", &Ty::Int, None, true, &mut errors), None);
        assert_eq!(
            pyjson::dumps(&errors[0].to_json(), None),
            r#"{"type": "missing", "loc": ["query", "q"], "msg": "Field required", "input": null}"#
        );
    }

    #[test]
    fn model_that_is_not_an_object_or_lacks_optional_fields_when_validated_then_its_error_or_the_given_fields() {
        let fields = [field("a", Ty::Int, false), field("b", Ty::Str, true)];
        let mut errors = Vec::new();

        assert!(validate_model(&pyjson::loads("[1]").unwrap(), &fields, false, &[], &mut errors).is_none());
        assert_eq!(errors[0].kind, "model_attributes_type");
        let mut errors = Vec::new();
        let out = validate_model(&pyjson::loads(r#"{"b": "x", "c": 1}"#).unwrap(), &fields, false, &[], &mut errors);
        assert_eq!(out.map(|m| pyjson::dumps(&Value::Object(m), None)).as_deref(), Some(r#"{"b": "x"}"#));
    }
}
