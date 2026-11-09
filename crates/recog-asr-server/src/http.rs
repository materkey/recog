//! What FastAPI and Starlette answer around the handlers: the routing (404, 405 with
//! `Allow`, the 307 of a trailing slash), `{"detail": …}` errors, the 422 of request
//! validation, the 500 of an unhandled exception and the `charset` Starlette adds to a
//! `text/*` media type.
//!
//! Known difference: FastAPI's `/docs`, `/redoc` and `/openapi.json` are not served.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use recog_core::pyjson::{Map, Value};

use crate::json;

/// A status code as Starlette sends it; one hyper cannot carry becomes a 500.
fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Starlette's `Response.init_headers` for a media type: `; charset=utf-8` after a
/// `text/*` type that names no charset.
#[must_use]
pub fn media_type(media: &str) -> String {
    if media.starts_with("text/") && !media.to_lowercase().contains("charset=") {
        format!("{media}; charset=utf-8")
    } else {
        media.to_owned()
    }
}

/// A response with a body and, when given, a content type (taken as it is).
#[must_use]
pub fn response(code: u16, body: Vec<u8>, content_type: Option<&str>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status(code);
    if let Some(value) = content_type.and_then(|t| HeaderValue::from_str(t).ok()) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

/// `PlainTextResponse(text, media_type=…)`.
#[must_use]
pub fn text(code: u16, body: String, media: &str) -> Response {
    response(code, body.into_bytes(), Some(&media_type(media)))
}

/// Starlette's `ServerErrorMiddleware` answer to an unhandled exception.
#[must_use]
pub fn internal_error() -> Response {
    text(500, "Internal Server Error".to_owned(), "text/plain")
}

/// `JSONResponse(value)`; a NaN or infinity in it is the 500 of `allow_nan=False`.
#[must_use]
pub fn json(code: u16, value: &Value) -> Response {
    match json::render(value) {
        Some(body) => response(code, body.into_bytes(), Some("application/json")),
        None => internal_error(),
    }
}

/// `HTTPException(code, detail)` through FastAPI's handler.
#[must_use]
pub fn detail(code: u16, detail: &str) -> Response {
    json(code, &object([("detail", Value::from(detail))]))
}

/// `RequestValidationError` through FastAPI's handler.
#[must_use]
pub fn validation_error(errors: Vec<Value>) -> Response {
    json(422, &object([("detail", Value::Array(errors))]))
}

/// An object of the pairs in order.
pub fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (k, v) in pairs {
        map.insert(k, v);
    }
    Value::Object(map)
}

/// Where Starlette's router sends a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Routed {
    /// The route at this index of the table.
    Found(usize),
    /// The path is a route's with another method: 405 with this `Allow`.
    NotAllowed(&'static str),
    /// Only the path with or without its trailing slash is a route: 307 here.
    Redirect(String),
    NotFound,
}

/// Routes `method` and the decoded `path` against `(path, method)` pairs, as
/// Starlette's `Router` with `redirect_slashes`. `host` and `query` make the
/// redirect's URL.
#[must_use]
pub fn route(
    routes: &[(&'static str, &'static str)],
    method: &str,
    path: &str,
    host: Option<&str>,
    query: Option<&str>,
) -> Routed {
    if let Some(i) = routes.iter().position(|(p, m)| *p == path && *m == method) {
        return Routed::Found(i);
    }
    if let Some((_, m)) = routes.iter().find(|(p, _)| *p == path) {
        return Routed::NotAllowed(m);
    }
    if path != "/" {
        let other = if path.ends_with('/') { path.trim_end_matches('/').to_owned() } else { format!("{path}/") };
        if routes.iter().any(|(p, _)| *p == other) {
            let mut url = match host {
                Some(host) => format!("http://{host}{other}"),
                None => other,
            };
            if let Some(query) = query.filter(|q| !q.is_empty()) {
                url.push('?');
                url.push_str(query);
            }
            return Routed::Redirect(quote_location(&url));
        }
    }
    Routed::NotFound
}

/// The answer to a request that matched no handler.
#[must_use]
pub fn unrouted(routed: Routed) -> Response {
    match routed {
        Routed::Found(_) => internal_error(),
        Routed::NotAllowed(allow) => {
            let mut response = detail(405, "Method Not Allowed");
            response.headers_mut().insert(header::ALLOW, HeaderValue::from_static(allow));
            response
        }
        Routed::Redirect(location) => {
            let mut response = response(307, Vec::new(), None);
            if let Ok(value) = HeaderValue::from_str(&location) {
                response.headers_mut().insert(header::LOCATION, value);
            }
            response
        }
        Routed::NotFound => detail(404, "Not Found"),
    }
}

/// `urllib.parse.quote(url, safe=":/%#?=@[]!$&'()*+,;")` of `RedirectResponse`.
fn quote_location(url: &str) -> String {
    const SAFE: &[u8] = b":/%#?=@[]!$&'()*+,;_.-~";
    let mut out = String::new();
    for &b in url.as_bytes() {
        if b.is_ascii_alphanumeric() || SAFE.contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The path uvicorn puts in the scope: percent-decoded, UTF-8 with replacement.
#[must_use]
pub fn decode_path(raw: &str) -> String {
    crate::form::unquote_utf8(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTES: [(&str, &str); 2] = [("/healthz", "GET"), ("/v1/audio/transcriptions", "POST")];

    #[test]
    fn a_route_with_another_method_when_routed_then_not_allowed_with_its_method() {
        assert_eq!(route(&ROUTES, "GET", "/v1/audio/transcriptions", None, None), Routed::NotAllowed("POST"));
        assert_eq!(route(&ROUTES, "HEAD", "/healthz", None, None), Routed::NotAllowed("GET"));
    }

    #[test]
    fn a_trailing_slash_when_routed_then_redirect_to_the_route() {
        assert_eq!(
            route(&ROUTES, "GET", "/healthz//", Some("h:1"), Some("a=b c")),
            Routed::Redirect("http://h:1/healthz?a=b%20c".to_owned())
        );
        assert_eq!(route(&ROUTES, "GET", "/nope/", Some("h:1"), None), Routed::NotFound);
    }

    #[test]
    fn no_host_header_when_a_trailing_slash_is_routed_then_redirect_to_the_bare_path() {
        assert_eq!(route(&ROUTES, "GET", "/healthz/", None, Some("")), Routed::Redirect("/healthz".to_owned()));
    }

    #[test]
    fn the_root_without_a_route_when_routed_then_not_found_without_a_slash_redirect() {
        assert_eq!(route(&ROUTES, "GET", "/", Some("h:1"), None), Routed::NotFound);
    }

    #[test]
    fn a_text_media_type_when_sent_then_charset_added_once() {
        assert_eq!(media_type("text/vtt"), "text/vtt; charset=utf-8");
        assert_eq!(media_type("text/plain; charset=latin-1"), "text/plain; charset=latin-1");
        assert_eq!(media_type("application/x-subrip"), "application/x-subrip");
    }
}
