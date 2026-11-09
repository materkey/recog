//! The responses FastAPI and Starlette build: JSON bodies, `{"detail": ...}` errors,
//! the plain 500 of an unhandled exception, redirects.

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use recog_core::pyjson::Value;

use crate::json;
use crate::validation::{self, ErrorItem};

/// A route's result: the response, or the error response Python raises.
pub type Reply = Result<Response, Response>;

fn with_body(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// `JSONResponse(content, status_code)`; a NaN makes it the plain 500.
#[must_use]
pub fn json_status(status: StatusCode, value: &Value) -> Response {
    match json::response(value) {
        Some(body) => with_body(status, "application/json", body.into_bytes()),
        None => internal_error(),
    }
}

/// `JSONResponse(content)`.
#[must_use]
pub fn json(value: &Value) -> Response {
    json_status(StatusCode::OK, value)
}

/// `HTTPException(status_code, detail)`.
#[must_use]
pub fn detail(status: StatusCode, message: &str) -> Response {
    json_status(status, &json::object([("detail", Value::from(message))]))
}

/// `HTTPException(status_code)`: the detail is the status phrase.
#[must_use]
pub fn status_detail(status: StatusCode) -> Response {
    detail(status, status.canonical_reason().unwrap_or_default())
}

/// The 422 of a request that failed validation.
#[must_use]
pub fn invalid(errors: &[ErrorItem]) -> Response {
    json_status(StatusCode::UNPROCESSABLE_ENTITY, &validation::detail(errors))
}

/// What Starlette's `ServerErrorMiddleware` answers to an unhandled exception.
#[must_use]
pub fn internal_error() -> Response {
    plain(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error", &[])
}

/// `PlainTextResponse(content, status_code, headers)`.
#[must_use]
pub fn plain(status: StatusCode, text: &str, headers: &[(HeaderName, String)]) -> Response {
    let mut response = with_body(status, "text/plain; charset=utf-8", text.as_bytes().to_vec());
    for (name, value) in headers {
        if let Ok(value) = HeaderValue::from_str(value) {
            response.headers_mut().insert(name.clone(), value);
        }
    }
    response
}

/// `405 Method Not Allowed` of a route with these methods.
#[must_use]
pub fn method_not_allowed(allow: &str) -> Response {
    let mut response = status_detail(StatusCode::METHOD_NOT_ALLOWED);
    if let Ok(value) = HeaderValue::from_str(allow) {
        response.headers_mut().insert(header::ALLOW, value);
    }
    response
}

/// `RedirectResponse(url)`: 307, an empty body.
#[must_use]
pub fn redirect(location: &str) -> Response {
    let mut response = with_body(StatusCode::TEMPORARY_REDIRECT, "", Vec::new());
    response.headers_mut().remove(header::CONTENT_TYPE);
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}
