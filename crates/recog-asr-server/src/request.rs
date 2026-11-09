//! The form fields of `POST /v1/audio/transcriptions`, validated as FastAPI validates
//! the handler's parameters (`File(...)`, `Form(...)`): in declaration order, every
//! error collected, an empty string counting as absent.
//!
//! Known difference: the `input` of an error about a file sent where a string or a
//! number is expected holds only the file's name and size; Python serialises the
//! whole `UploadFile` object there.

use axum::body::Body;
use axum::extract::Request;
use axum::http::header;
use axum::response::Response;
use recog_core::py;
use recog_core::pyjson::Value;

use crate::form::{self, FormData, FormValue, UploadFile};
use crate::http::{self, object};

/// The parameters from a request's body, or FastAPI's answer before the handler runs:
/// the 400 of a body that cannot be read or parsed, the 422 of the validation.
///
/// # Errors
/// That answer (boxed: a response is large).
pub async fn read(request: Request, default_model: &str) -> Result<Transcription, Box<Response>> {
    let content_type = request.headers().get(header::CONTENT_TYPE).map(|v| v.as_bytes().to_vec());
    let Ok(body) = axum::body::to_bytes(Body::new(request.into_body()), usize::MAX).await else {
        return Err(Box::new(http::detail(400, form::PARSE_ERROR)));
    };
    let form = form::parse(content_type.as_deref(), &body).map_err(|detail| Box::new(http::detail(400, &detail)))?;
    transcription(&form, default_model).map_err(|errors| Box::new(http::validation_error(errors)))
}

/// The parameters both handlers declare.
#[derive(Debug, Clone)]
pub struct Transcription {
    pub file: UploadFile,
    pub model: String,
    pub language: Option<String>,
    pub response_format: String,
    pub prompt: Option<String>,
    pub temperature: Option<f64>,
    pub timestamp_granularities: Option<String>,
}

fn loc(name: &str) -> Value {
    Value::Array(vec![Value::from("body"), Value::from(name)])
}

fn missing(name: &str) -> Value {
    object([
        ("type", Value::from("missing")),
        ("loc", loc(name)),
        ("msg", Value::from("Field required")),
        ("input", Value::Null),
    ])
}

fn upload_input(file: &UploadFile) -> Value {
    object([
        ("filename", Value::from(file.filename.as_str())),
        ("size", Value::Int(i128::try_from(file.data.len()).unwrap_or(i128::MAX))),
    ])
}

/// `_get_multidict_value`: absent and `""` are both no value.
fn value<'a>(form: &'a FormData, name: &str) -> Option<&'a FormValue> {
    match form.get(name) {
        Some(FormValue::Text(t)) if t.is_empty() => None,
        other => other,
    }
}

fn string(form: &FormData, name: &str, errors: &mut Vec<Value>) -> Option<String> {
    match value(form, name) {
        None => None,
        Some(FormValue::Text(t)) => Some(t.clone()),
        Some(FormValue::File(file)) => {
            errors.push(object([
                ("type", Value::from("string_type")),
                ("loc", loc(name)),
                ("msg", Value::from("Input should be a valid string")),
                ("input", upload_input(file)),
            ]));
            None
        }
    }
}

/// The parameters, or the `detail` list of the 422.
///
/// # Errors
/// Every validation error, in FastAPI's order.
pub fn transcription(form: &FormData, default_model: &str) -> Result<Transcription, Vec<Value>> {
    let mut errors = Vec::new();
    let file = file(form, &mut errors);
    let model = string(form, "model", &mut errors);
    let language = string(form, "language", &mut errors);
    let response_format = string(form, "response_format", &mut errors);
    let prompt = string(form, "prompt", &mut errors);
    let temperature = number(form, "temperature", &mut errors);
    let timestamp_granularities = string(form, "timestamp_granularities", &mut errors);
    match file {
        Some(file) if errors.is_empty() => Ok(Transcription {
            file,
            model: model.unwrap_or_else(|| default_model.to_owned()),
            language,
            response_format: response_format.unwrap_or_else(|| "json".to_owned()),
            prompt,
            temperature,
            timestamp_granularities,
        }),
        _ => Err(errors),
    }
}

/// `file: UploadFile = File(...)`.
fn file(form: &FormData, errors: &mut Vec<Value>) -> Option<UploadFile> {
    match value(form, "file") {
        None => {
            errors.push(missing("file"));
            None
        }
        Some(FormValue::Text(t)) => {
            errors.push(object([
                ("type", Value::from("value_error")),
                ("loc", loc("file")),
                ("msg", Value::from("Value error, Expected UploadFile, received: <class 'str'>")),
                ("input", Value::from(t.as_str())),
                ("ctx", object([("error", object([]))])),
            ]));
            None
        }
        Some(FormValue::File(file)) => Some(file.clone()),
    }
}

/// An optional `float` form field.
fn number(form: &FormData, name: &str, errors: &mut Vec<Value>) -> Option<f64> {
    match value(form, name) {
        None => None,
        Some(FormValue::Text(t)) => {
            let parsed = py::parse_float(t).ok();
            if parsed.is_none() {
                errors.push(object([
                    ("type", Value::from("float_parsing")),
                    ("loc", loc(name)),
                    ("msg", Value::from("Input should be a valid number, unable to parse string as a number")),
                    ("input", Value::from(t.as_str())),
                ]));
            }
            parsed
        }
        Some(FormValue::File(file)) => {
            errors.push(object([
                ("type", Value::from("float_type")),
                ("loc", loc(name)),
                ("msg", Value::from("Input should be a valid number")),
                ("input", upload_input(file)),
            ]));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form;
    use crate::json;

    fn urlencoded(body: &str) -> FormData {
        form::parse(Some(b"application/x-www-form-urlencoded"), body.as_bytes()).unwrap()
    }

    #[test]
    fn no_file_and_a_bad_temperature_when_validated_then_both_errors_in_order() {
        let errors = transcription(&urlencoded("temperature=abc"), "gigaam").unwrap_err();
        assert_eq!(
            json::render(&Value::Array(errors)).unwrap(),
            r#"[{"type":"missing","loc":["body","file"],"msg":"Field required","input":null},{"type":"float_parsing","loc":["body","temperature"],"msg":"Input should be a valid number, unable to parse string as a number","input":"abc"}]"#
        );
    }

    fn multipart(parts: &[(&str, Option<&str>, &str)]) -> FormData {
        let mut body = String::new();
        for (name, filename, value) in parts {
            let filename = filename.map(|f| format!("; filename=\"{f}\"")).unwrap_or_default();
            body.push_str(&format!(
                "--b\r\nContent-Disposition: form-data; name=\"{name}\"{filename}\r\n\r\n{value}\r\n"
            ));
        }
        body.push_str("--b--\r\n");
        form::parse(Some(b"multipart/form-data; boundary=b"), body.as_bytes()).unwrap()
    }

    #[test]
    fn only_a_file_when_validated_then_the_defaults() {
        let t = transcription(&multipart(&[("file", Some("a.wav"), "RIFF")]), "whisper-1").unwrap();
        assert_eq!((t.file.filename.as_str(), t.file.data.as_slice()), ("a.wav", b"RIFF".as_slice()));
        assert_eq!((t.model.as_str(), t.response_format.as_str()), ("whisper-1", "json"));
        assert_eq!((t.language, t.prompt, t.temperature, t.timestamp_granularities), (None, None, None, None));
    }

    #[test]
    fn every_field_when_validated_then_each_taken_and_empty_ones_absent() {
        let form = multipart(&[
            ("file", Some("a.wav"), "x"),
            ("model", None, "qwen"),
            ("language", None, "en"),
            ("response_format", None, ""),
            ("prompt", None, "hi"),
            ("temperature", None, " 1_0 "),
            ("timestamp_granularities", None, "word"),
        ]);
        let t = transcription(&form, "gigaam").unwrap();
        assert_eq!((t.model.as_str(), t.response_format.as_str()), ("qwen", "json"));
        assert_eq!((t.language.as_deref(), t.prompt.as_deref()), (Some("en"), Some("hi")));
        assert_eq!((t.temperature, t.timestamp_granularities.as_deref()), (Some(10.0), Some("word")));
    }

    #[test]
    fn files_where_text_is_expected_when_validated_then_type_errors_with_name_and_size() {
        let form = multipart(&[
            ("model", Some("m.txt"), "abc"),
            ("temperature", Some("t.txt"), "0"),
            ("file", Some("a.wav"), "x"),
        ]);
        let errors = transcription(&form, "gigaam").unwrap_err();
        assert_eq!(
            json::render(&Value::Array(errors)).unwrap(),
            r#"[{"type":"string_type","loc":["body","model"],"msg":"Input should be a valid string","input":{"filename":"m.txt","size":3}},{"type":"float_type","loc":["body","temperature"],"msg":"Input should be a valid number","input":{"filename":"t.txt","size":1}}]"#
        );
    }

    #[test]
    fn a_file_sent_as_text_when_validated_then_pydantics_value_error() {
        let errors = transcription(&urlencoded("file=abc"), "gigaam").unwrap_err();
        assert_eq!(
            json::render(&errors[0]).unwrap(),
            r#"{"type":"value_error","loc":["body","file"],"msg":"Value error, Expected UploadFile, received: <class 'str'>","input":"abc","ctx":{"error":{}}}"#
        );
    }
}
