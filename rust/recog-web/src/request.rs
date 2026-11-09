//! An HTTP request as the route handlers see it, and FastAPI's reading of its query
//! and body.

use axum::body::Bytes;
use axum::http::HeaderMap;
use recog_core::pyjson::{self, Value};

use crate::router::Params;
use crate::validation::{ErrorItem, Loc};

pub struct Req {
    pub method: String,
    /// The percent-decoded path.
    pub path: String,
    pub params: Params,
    pub query: Vec<(String, String)>,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Why a body could not be read.
pub enum BodyError {
    /// The 422 of `json.JSONDecodeError`.
    Invalid(ErrorItem),
    /// Any other failure: FastAPI's 400 "There was an error parsing the body".
    Unparsable,
}

/// Python 3.12's JSON decoder gives up (`RecursionError`) beyond this nesting.
pub const MAX_JSON_DEPTH: usize = 9997;

fn nesting(text: &str) -> usize {
    let (mut depth, mut deepest, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for c in text.chars() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' | '{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// Why untrusted JSON was refused.
pub enum JsonError {
    Decode(pyjson::DecodeError),
    /// Nested deeper than Python's decoder recurses.
    Recursion,
}

/// `json.loads(text)` of text a client sent: nesting deeper than Python's decoder
/// accepts is refused before parsing, as Python raises `RecursionError`.
///
/// # Errors
/// What `json.loads` raises.
pub fn loads_untrusted(text: &str) -> Result<Value, JsonError> {
    if nesting(text) > MAX_JSON_DEPTH {
        return Err(JsonError::Recursion);
    }
    pyjson::loads(text).map_err(JsonError::Decode)
}

/// The position and the bare message of a `JSONDecodeError` from its text
/// (`"<msg>: line L column C (char N)"`).
fn decode_parts(message: &str) -> (i128, String) {
    if let Some((msg, rest)) = message.rsplit_once(": line ")
        && let Some(pos) = rest.rsplit_once("(char ").and_then(|(_, p)| p.strip_suffix(')'))
        && let Ok(pos) = pos.parse()
    {
        return (pos, msg.to_owned());
    }
    (0, message.to_owned())
}

impl Req {
    /// `request.query_params.get(name)`: the last value given.
    #[must_use]
    pub fn query(&self, name: &str) -> Option<&str> {
        self.query.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    #[must_use]
    pub fn param(&self, name: &str) -> &str {
        self.params.get(name)
    }

    fn json_content_type(&self) -> bool {
        let Some(value) = self.headers.get("content-type").and_then(|v| v.to_str().ok()) else { return false };
        let media = value.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
        let Some((main, sub)) = media.split_once('/') else { return false };
        main == "application" && (sub == "json" || sub.ends_with("+json"))
    }

    /// The body FastAPI hands to validation: `None` for an empty body or JSON `null`,
    /// the JSON value for a JSON content type, else the raw bytes as text.
    ///
    /// # Errors
    /// JSON that does not parse, or a body that cannot be decoded.
    pub fn body(&self) -> Result<Option<Value>, BodyError> {
        if self.body.is_empty() {
            return Ok(None);
        }
        if !self.json_content_type() {
            return Ok(Some(Value::from(String::from_utf8_lossy(&self.body).into_owned())));
        }
        let raw = self.body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&self.body);
        let Ok(text) = std::str::from_utf8(raw) else { return Err(BodyError::Unparsable) };
        match loads_untrusted(text) {
            Ok(Value::Null) => Ok(None),
            Ok(value) => Ok(Some(value)),
            Err(JsonError::Recursion) => Err(BodyError::Unparsable),
            Err(JsonError::Decode(e)) => {
                let (pos, msg) = decode_parts(&e.0);
                Err(BodyError::Invalid(ErrorItem {
                    kind: "json_invalid",
                    loc: vec![Loc::from("body"), Loc::Index(usize::try_from(pos).unwrap_or(0))],
                    msg: "JSON decode error".to_owned(),
                    input: Value::Object(pyjson::Map::new()),
                    ctx: Some(crate::json::object([("error", Value::from(msg))])),
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_json_given_untrusted_parser_when_loaded_then_refused_like_recursion_error() {
        // given
        let deep = format!("{}0{}", "[".repeat(MAX_JSON_DEPTH + 1), "]".repeat(MAX_JSON_DEPTH + 1));
        let fine = format!("{}0{}", "[".repeat(2000), "]".repeat(2000));
        // when / then
        assert!(matches!(loads_untrusted(&deep), Err(JsonError::Recursion)));
        assert!(matches!(loads_untrusted(&fine), Ok(Value::Array(_))));
        assert!(matches!(loads_untrusted(r#"{"a": "[[[["}"#), Ok(Value::Object(_))));
    }

    fn request(content_type: Option<&str>, body: &[u8]) -> Req {
        let mut headers = HeaderMap::new();
        if let Some(value) = content_type {
            headers.insert("content-type", value.parse().unwrap());
        }
        Req {
            method: "PUT".into(),
            path: "/".into(),
            params: Params::default(),
            query: vec![("a".into(), "1".into()), ("a".into(), "2".into())],
            headers,
            body: Bytes::copy_from_slice(body),
        }
    }

    fn read(content_type: Option<&str>, body: &[u8]) -> Result<Option<String>, String> {
        match request(content_type, body).body() {
            Ok(value) => Ok(value.map(|v| pyjson::dumps(&v, None))),
            Err(BodyError::Unparsable) => Err("unparsable".into()),
            Err(BodyError::Invalid(item)) => Err(pyjson::dumps(&item.to_json(), None)),
        }
    }

    #[test]
    fn bodies_of_each_content_type_when_read_then_json_text_or_nothing() {
        for (content_type, body, expected) in [
            (Some("application/json"), &b"{\"a\": 1}"[..], Ok(Some(r#"{"a": 1}"#.to_owned()))),
            (Some("Application/JSON; charset=utf-8"), b"[1]", Ok(Some("[1]".to_owned()))),
            (Some("application/merge-patch+json"), b"2", Ok(Some("2".to_owned()))),
            (Some("application/json"), b"\xef\xbb\xbf{}", Ok(Some("{}".to_owned()))),
            (Some("application/json"), b"null", Ok(None)),
            (Some("application/json"), b"", Ok(None)),
            (Some("text/plain"), b"{\"a\": 1}", Ok(Some(r#""{\"a\": 1}""#.to_owned()))),
            (Some("json"), b"x", Ok(Some(r#""x""#.to_owned()))),
            (Some("application/xml"), b"x", Ok(Some(r#""x""#.to_owned()))),
            (None, b"x\xff", Ok(Some("\"x\u{fffd}\"".to_owned()))),
            (Some("application/json"), b"{\xff}", Err("unparsable".to_owned())),
        ] {
            assert_eq!(read(content_type, body), expected, "{content_type:?} {body:?}");
        }
    }

    #[test]
    fn json_that_does_not_parse_or_nests_too_deep_when_read_then_422_or_400() {
        let deep = format!("{}{}", "[".repeat(MAX_JSON_DEPTH + 1), "]".repeat(MAX_JSON_DEPTH + 1));

        assert_eq!(
            read(Some("application/json"), b"{\"a\": 1,}"),
            Err(r#"{"type": "json_invalid", "loc": ["body", 8], "msg": "JSON decode error", "input": {}, "ctx": {"error": "Expecting property name enclosed in double quotes"}}"#.to_owned())
        );
        assert_eq!(read(Some("application/json"), deep.as_bytes()), Err("unparsable".to_owned()));
    }

    #[test]
    fn repeated_query_parameter_when_read_then_the_last_value() {
        let req = request(None, b"");

        assert_eq!((req.query("a"), req.query("b"), req.param("x")), (Some("2"), None, ""));
    }

    #[test]
    fn strings_and_escapes_when_nesting_is_measured_then_brackets_inside_strings_do_not_count() {
        assert_eq!(nesting(r#"[{"a": "[[\"{{"}, [1]]"#), 2);
        assert_eq!(nesting(r#""\\" [ ]"#), 1);
        assert_eq!(nesting("]]][["), 2);
        assert_eq!(nesting("plain"), 0);
    }

    #[test]
    fn decode_error_text_given_message_when_split_then_position_and_bare_message() {
        // given / when / then
        assert_eq!(decode_parts("Expecting value: line 1 column 1 (char 0)"), (0, "Expecting value".into()));
        assert_eq!(
            decode_parts("Expecting ',' delimiter: line 2 column 2 (char 9)"),
            (9, "Expecting ',' delimiter".into())
        );
    }
}
