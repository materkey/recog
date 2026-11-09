//! The OpenAI-compatible gateway, in place of `recog/asr_server/gateway.py`: routes
//! `POST /v1/audio/transcriptions` by its `model` field to the GigaAM or Qwen backend,
//! lists the aliases at `/v1/models` and gathers both backends' `/healthz`. No
//! authentication.
//!
//! The request to a backend is the multipart body httpx builds (the same fields, in
//! the same order, with httpx's escaping of the file name), sent with ureq from a
//! blocking thread; `ASR_TIMEOUT_S` bounds connecting and each read and write, as
//! httpx's timeout does, and a health check has 5 s.
//!
//! Known differences: the text of a failed request is httpx's for what the tests
//! reproduce (a refused connection, a timeout, a connection closed before the answer,
//! a URL without an http(s) scheme) and ureq's otherwise (a host that does not
//! resolve, for example); a backend's error body is read as UTF-8 whatever charset it
//! names; requests run concurrently, where Python's handler shares one event loop.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::ErrorKind;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header;
use axum::response::Response;
use recog_core::py;
use recog_core::pyjson::{self, Map, Value};

use crate::config::GatewayConfig;
use crate::form::latin1;
use crate::http::{self, Routed, object};
use crate::log;
use crate::request::{self, Transcription};

/// Which backend an alias goes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Gigaam,
    Qwen,
}

/// `MODEL_MAP`: model name → backend.
const MODEL_MAP: [(&str, Target); 9] = [
    ("gigaam", Target::Gigaam),
    ("gigaam-v3", Target::Gigaam),
    ("gigaam-v3-e2e-rnnt", Target::Gigaam),
    ("gigaam-diarized", Target::Gigaam),
    ("gigaam-diarized-v3", Target::Gigaam),
    ("qwen3-asr", Target::Qwen),
    ("qwen3-asr-1.7b", Target::Qwen),
    ("qwen", Target::Qwen),
    ("whisper-1", Target::Gigaam),
];

const ROUTES: [(&str, &str); 3] = [("/healthz", "GET"), ("/v1/models", "GET"), ("/v1/audio/transcriptions", "POST")];

/// The health check's timeout, `client.get(..., timeout=5)`.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

/// The gateway: its settings and its HTTP clients.
pub struct Gateway {
    config: GatewayConfig,
    agent: ureq::Agent,
    health: ureq::Agent,
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_connect(timeout)
        .timeout_per_read(timeout)
        .timeout_per_write(timeout)
        .build()
        .new_agent()
}

impl Gateway {
    #[must_use]
    pub fn new(config: GatewayConfig) -> Self {
        let agent = agent(config.timeout);
        Self { config, agent, health: agent_for_health() }
    }

    #[must_use]
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    fn url(&self, target: Target) -> &str {
        match target {
            Target::Gigaam => &self.config.gigaam_url,
            Target::Qwen => &self.config.qwen_url,
        }
    }
}

fn agent_for_health() -> ureq::Agent {
    agent(Some(HEALTH_TIMEOUT))
}

/// `sorted(set(MODEL_MAP.keys()))`.
fn aliases() -> Vec<&'static str> {
    let mut names: Vec<&str> = MODEL_MAP.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The axum application.
pub fn app(gateway: Arc<Gateway>) -> Router {
    Router::new().fallback(dispatch).with_state(gateway)
}

/// Serves on a bound listener until `shutdown` resolves.
///
/// # Errors
/// The listener fails.
pub async fn serve(
    listener: tokio::net::TcpListener,
    gateway: Arc<Gateway>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, app(gateway)).with_graceful_shutdown(shutdown).await
}

async fn dispatch(State(gateway): State<Arc<Gateway>>, request: Request) -> Response {
    let path = http::decode_path(request.uri().path());
    let host = request.headers().get(header::HOST).map(|v| latin1(v.as_bytes()));
    let query = request.uri().query().map(str::to_owned);
    match http::route(&ROUTES, request.method().as_str(), &path, host.as_deref(), query.as_deref()) {
        Routed::Found(0) => healthz(gateway).await,
        Routed::Found(1) => models(),
        Routed::Found(2) => transcribe(gateway, request).await,
        other => http::unrouted(other),
    }
}

/// What a backend answered.
struct Upstream {
    status: u16,
    content_type: Option<String>,
    body: Vec<u8>,
}

/// httpx's check of the URL's scheme, with its messages.
fn check_scheme(url: &str) -> Result<(), String> {
    let scheme = url.split_once(':').map(|(s, _)| s).filter(|s| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    match scheme.map(str::to_ascii_lowercase) {
        None => Err("Request URL is missing an 'http://' or 'https://' protocol.".to_owned()),
        Some(s) if s == "http" || s == "https" => Ok(()),
        Some(s) => Err(format!("Request URL has an unsupported protocol '{s}://'.")),
    }
}

/// `str(exception)` of the httpx error that stands for a ureq failure.
fn httpx_message(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Timeout(_) => String::new(),
        ureq::Error::ConnectionFailed => "All connection attempts failed".to_owned(),
        ureq::Error::Io(io) => match io.kind() {
            ErrorKind::ConnectionRefused => "All connection attempts failed".to_owned(),
            ErrorKind::UnexpectedEof => "Server disconnected without sending a response.".to_owned(),
            // httpx's ReadError and WriteError carry no text.
            ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe => {
                String::new()
            }
            _ => io.to_string(),
        },
        other => other.to_string(),
    }
}

fn read(mut response: ureq::http::Response<ureq::Body>) -> Result<Upstream, String> {
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| std::str::from_utf8(v.as_bytes()).map_or_else(|_| latin1(v.as_bytes()), str::to_owned));
    let body = response.body_mut().with_config().limit(u64::MAX).read_to_vec().map_err(|e| httpx_message(&e))?;
    Ok(Upstream { status, content_type, body })
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Upstream, String> {
    check_scheme(url)?;
    read(agent.get(url).call().map_err(|e| httpx_message(&e))?)
}

fn post(agent: &ureq::Agent, url: &str, content_type: &str, body: &[u8]) -> Result<Upstream, String> {
    check_scheme(url)?;
    read(agent.post(url).header("Content-Type", content_type).send(body).map_err(|e| httpx_message(&e))?)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(e.to_string()))
}

async fn healthz(gateway: Arc<Gateway>) -> Response {
    let mut out = Map::new();
    for (name, target) in [("qwen", Target::Qwen), ("gigaam", Target::Gigaam)] {
        let url = format!("{}/healthz", gateway.url(target));
        let agent = gateway.health.clone();
        let answer = blocking(move || {
            let upstream = get(&agent, &url)?;
            pyjson::loads_bytes(&upstream.body).map_err(|e| e.0)
        })
        .await;
        let value = answer.unwrap_or_else(|message| object([("error", Value::from(message))]));
        out.insert(name, value);
    }
    http::json(200, &Value::Object(out))
}

fn models() -> Response {
    let data = aliases()
        .into_iter()
        .map(|id| {
            object([("id", Value::from(id)), ("object", Value::from("model")), ("owned_by", Value::from("local"))])
        })
        .collect();
    http::json(200, &object([("object", Value::from("list")), ("data", Value::Array(data))]))
}

/// A multipart boundary: 32 random hex digits, as httpx's `os.urandom(16).hex()`.
fn boundary() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let state = RandomState::new();
    let mut text = String::with_capacity(32);
    for _ in 0..2 {
        let mut hasher = state.build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        text.push_str(&format!("{:016x}", hasher.finish()));
    }
    text
}

/// httpx's `_format_form_param`: `"` and `\` escaped, control characters but ESC
/// percent-encoded.
fn form_param(name: &str, value: &str) -> String {
    let mut out = format!("{name}=\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("%22"),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) <= 0x1f && c != '\u{1b}' => out.push_str(&format!("%{:02X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The body httpx's `MultipartStream` writes for `data=` then `files=`.
fn multipart(boundary: &str, fields: &[(&str, String)], filename: &str, content_type: &str, file: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(file.len() + 1024);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; {}\r\n\r\n{value}\r\n", form_param("name", name))
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; {}; {}\r\nContent-Type: {content_type}\r\n\r\n",
            form_param("name", "file"),
            form_param("filename", filename)
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// The data fields `transcribe` sends, in its order.
fn fields(t: &Transcription) -> Vec<(&'static str, String)> {
    let mut data = vec![("model", t.model.clone()), ("response_format", t.response_format.clone())];
    if let Some(language) = &t.language {
        data.push(("language", language.clone()));
    }
    if let Some(prompt) = &t.prompt {
        data.push(("prompt", prompt.clone()));
    }
    if let Some(temperature) = t.temperature {
        data.push(("temperature", py::float_repr(temperature)));
    }
    if let Some(granularities) = &t.timestamp_granularities {
        data.push(("timestamp_granularities", granularities.clone()));
    }
    data
}

async fn transcribe(gateway: Arc<Gateway>, request: Request) -> Response {
    let t = match request::read(request, "whisper-1").await {
        Ok(t) => t,
        Err(answer) => return *answer,
    };
    let lowered = t.model.to_lowercase();
    let Some(target) = MODEL_MAP.iter().find(|(name, _)| *name == lowered).map(|(_, target)| *target) else {
        let available: Vec<String> = aliases().into_iter().map(py::repr).collect();
        return http::detail(400, &format!("unknown model '{}'. Available: [{}]", t.model, available.join(", ")));
    };
    let backend = gateway.url(target).to_owned();
    let filename = if t.file.filename.is_empty() { "audio.wav" } else { t.file.filename.as_str() };
    let file_type = t.file.content_type.as_deref().filter(|c| !c.is_empty()).unwrap_or("application/octet-stream");
    let boundary = boundary();
    let payload = multipart(&boundary, &fields(&t), filename, file_type, &t.file.data);
    log::info(&format!(
        "→ {backend}  model={}  bytes={}  fmt={}  lang={}  ts_gran={}",
        t.model,
        t.file.data.len(),
        t.response_format,
        log::or_none(t.language.as_deref()),
        log::or_none(t.timestamp_granularities.as_deref())
    ));
    let agent = gateway.agent.clone();
    let url = format!("{backend}/v1/audio/transcriptions");
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let answer = blocking(move || post(&agent, &url, &content_type, &payload)).await;
    match answer {
        Err(message) => {
            log::error(&format!("backend error: {message}"));
            http::detail(502, &format!("backend {backend} unreachable: {message}"))
        }
        Ok(upstream) if upstream.status >= 400 => {
            http::detail(upstream.status, &String::from_utf8_lossy(&upstream.body))
        }
        Ok(upstream) => {
            let media = upstream.content_type.as_deref().map(http::media_type);
            http::response(upstream.status, upstream.body, media.as_deref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_with_quotes_and_controls_when_formatted_then_httpx_escaping() {
        assert_eq!(form_param("filename", "a\"b\\c\nd\u{1b}.wav"), "filename=\"a%22b\\\\c%0Ad\u{1b}.wav\"");
    }

    #[test]
    fn a_transcription_when_forwarded_then_httpxs_multipart_body() {
        let body = multipart("B", &[("model", "gigaam".to_owned())], "a.wav", "audio/wav", b"RIFF");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "--B\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngigaam\r\n\
             --B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\n\
             Content-Type: audio/wav\r\n\r\nRIFF\r\n--B--\r\n"
        );
    }

    #[test]
    fn urls_without_an_http_scheme_when_checked_then_httpxs_messages() {
        assert_eq!(check_scheme("http://x").map_err(|_| ()), Ok(()));
        assert_eq!(
            check_scheme("127.0.0.1:9002"),
            Err("Request URL is missing an 'http://' or 'https://' protocol.".to_owned())
        );
        assert_eq!(check_scheme("ftp://x"), Err("Request URL has an unsupported protocol 'ftp://'.".to_owned()));
    }

    #[test]
    fn ureq_failures_when_told_as_httpx_errors_then_httpxs_texts() {
        assert_eq!(httpx_message(&ureq::Error::Timeout(ureq::Timeout::PerRead)), "");
        assert_eq!(httpx_message(&ureq::Error::ConnectionFailed), "All connection attempts failed");
        for (kind, text) in [
            (ErrorKind::ConnectionRefused, "All connection attempts failed"),
            (ErrorKind::UnexpectedEof, "Server disconnected without sending a response."),
            (ErrorKind::TimedOut, ""),
            (ErrorKind::WouldBlock, ""),
            (ErrorKind::ConnectionReset, ""),
            (ErrorKind::BrokenPipe, ""),
        ] {
            let error = ureq::Error::Io(std::io::Error::new(kind, "the OS's text"));
            assert_eq!(httpx_message(&error), text, "{kind:?}");
        }
        assert_eq!(httpx_message(&ureq::Error::Io(std::io::Error::other("disk full"))), "disk full");
        assert_eq!(httpx_message(&ureq::Error::HostNotFound), ureq::Error::HostNotFound.to_string());
    }

    #[test]
    fn the_alias_table_when_listed_then_sorted_once_each() {
        assert_eq!(
            aliases(),
            [
                "gigaam",
                "gigaam-diarized",
                "gigaam-diarized-v3",
                "gigaam-v3",
                "gigaam-v3-e2e-rnnt",
                "qwen",
                "qwen3-asr",
                "qwen3-asr-1.7b",
                "whisper-1"
            ]
        );
    }
}
