//! The two conversations the worker has with Node's inspector, which is local: the
//! target list over HTTP (httpx with `trust_env=False, timeout=1`) and one
//! `Runtime.evaluate` over its WebSocket (websockets with `open_timeout=1,
//! close_timeout=1, max_size=131072`, no proxy). Neither follows a redirect: the
//! validated endpoint must not be traded for one the listener chooses.

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use recog_core::pyjson::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::py::{self, PyErr};

/// How an inspector step failed, as the worker tells failures apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// `AttachError(reason)`.
    Attach(String),
    /// The builtin `TimeoutError` (the attachment deadline, the WebSocket opening).
    Timeout,
    /// Any other exception, with what it says (never copied into a result).
    Other(String),
}

impl Failure {
    #[must_use]
    pub fn attach(reason: &str) -> Self {
        Self::Attach(reason.to_owned())
    }
}

impl From<PyErr> for Failure {
    fn from(error: PyErr) -> Self {
        if error.is("TimeoutError") { Self::Timeout } else { Self::Other(error.described()) }
    }
}

const IO_TIMEOUT: Duration = Duration::from_secs(1);
/// Past this the answer is refused anyway (`len(response.content) > 65536`).
const BODY_LIMIT: usize = 65536;

fn other(what: impl std::fmt::Display) -> Failure {
    Failure::Other(what.to_string())
}

async fn with_timeout<T>(future: impl Future<Output = std::io::Result<T>>) -> Result<T, Failure> {
    match tokio::time::timeout(IO_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(other(e)),
        Err(_) => Err(other("httpx.TimeoutException")),
    }
}

/// The body of a chunked response, or None while incomplete.
fn dechunk(raw: &[u8]) -> Result<Option<Vec<u8>>, Failure> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let Some(line_end) = raw[at..].windows(2).position(|w| w == b"\r\n") else { return Ok(None) };
        let line = std::str::from_utf8(&raw[at..at + line_end]).map_err(|_| other("invalid chunk size"))?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| other("invalid chunk size"))?;
        at += line_end + 2;
        if size == 0 {
            return Ok(Some(out));
        }
        if raw.len() < at + size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&raw[at..at + size]);
        at += size + 2;
        if out.len() > BODY_LIMIT {
            return Ok(Some(out));
        }
    }
}

/// The status and body of the response, read until it is whole.
async fn read_response(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), Failure> {
    let mut raw = Vec::new();
    loop {
        let mut buffer = [0u8; 8192];
        let n = with_timeout(stream.read(&mut buffer)).await?;
        raw.extend_from_slice(&buffer[..n]);
        if let Some(response) = parse_response(&raw, n == 0)? {
            return Ok(response);
        }
    }
}

/// The status and body once `raw` holds a whole response, None while more is to
/// come (`closed`: the peer has sent everything it will).
fn parse_response(raw: &[u8], closed: bool) -> Result<Option<(u16, Vec<u8>)>, Failure> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut headers);
    let httparse::Status::Complete(head) = response.parse(raw).map_err(other)? else {
        return if closed { Err(other("server disconnected without sending a response")) } else { Ok(None) };
    };
    let body = response_body(response.headers, &raw[head..], closed)?;
    Ok(body.map(|body| (response.code.unwrap_or(0), body)))
}

fn header_value(headers: &[httparse::Header<'_>], name: &str) -> Option<String> {
    headers.iter().find(|h| h.name.eq_ignore_ascii_case(name)).map(|h| String::from_utf8_lossy(h.value).into_owned())
}

/// The body as the headers frame it: chunked, sized, or until the peer closes.
fn response_body(headers: &[httparse::Header<'_>], body: &[u8], closed: bool) -> Result<Option<Vec<u8>>, Failure> {
    if header_value(headers, "transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        return chunked_body(body, closed);
    }
    match header_value(headers, "content-length") {
        Some(length) => sized_body(body, &length, closed),
        None => Ok((closed || body.len() > BODY_LIMIT).then(|| body.to_vec())),
    }
}

fn chunked_body(body: &[u8], closed: bool) -> Result<Option<Vec<u8>>, Failure> {
    match dechunk(body)? {
        Some(body) => Ok(Some(body)),
        None if closed => Err(other("incomplete chunked read")),
        None => Ok(None),
    }
}

/// The `Content-Length` bytes, or as many as arrived past the size limit.
fn sized_body(body: &[u8], length: &str, closed: bool) -> Result<Option<Vec<u8>>, Failure> {
    let length: usize = length.trim().parse().map_err(|_| other("invalid content-length"))?;
    if body.len() >= length || body.len() > BODY_LIMIT {
        return Ok(Some(body[..length.min(body.len())].to_vec()));
    }
    if closed {
        return Err(other("peer closed connection without sending complete message body"));
    }
    Ok(None)
}

/// `GET http://127.0.0.1:<port>/json/list`, its status checked
/// (`raise_for_status`), its size bounded and its body decoded as JSON.
///
/// # Errors
/// `invalid_inspector_response` for an oversized body; anything else is `Other`.
pub async fn json_list(port: u16) -> Result<Value, Failure> {
    let mut stream = with_timeout(TcpStream::connect(("127.0.0.1", port))).await?;
    let request = format!(
        "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: */*\r\nConnection: close\r\n\
         User-Agent: recog-desktop\r\n\r\n"
    );
    with_timeout(stream.write_all(request.as_bytes())).await?;
    let (status, body) = read_response(&mut stream).await?;
    if !(200..300).contains(&status) {
        return Err(other(format!("HTTPStatusError: {status}")));
    }
    if body.len() > BODY_LIMIT {
        return Err(Failure::attach("invalid_inspector_response"));
    }
    recog_core::pyjson::loads_bytes(&body).map_err(|e| other(e.0))
}

/// The frame `evaluate` sends: `json.dumps` with its default ASCII escaping.
#[must_use]
pub fn evaluate_frame(expression: &str) -> String {
    py::dumps_ascii(&py::obj([
        ("id", Value::Int(1)),
        ("method", Value::from("Runtime.evaluate")),
        (
            "params",
            py::obj([
                ("expression", Value::from(expression)),
                ("awaitPromise", Value::Bool(true)),
                ("returnByValue", Value::Bool(true)),
            ]),
        ),
    ]))
}

/// What one reply says: the value, an evaluation failure, or not ours (`id != 1`).
fn read_reply(reply: &Value) -> Result<Option<Value>, Failure> {
    let id = py::get(reply, "id").map_err(|e| other(e.described()))?;
    if !id.is_some_and(|id| id.py_eq(&Value::Int(1))) {
        return Ok(None);
    }
    let empty = Value::Object(recog_core::pyjson::Map::new());
    let result = py::get(reply, "result").map_err(|e| other(e.described()))?.unwrap_or(&empty);
    let error = py::get(reply, "error").map_err(|e| other(e.described()))?;
    if error.is_some_and(Value::truthy)
        || py::get(result, "exceptionDetails").map_err(|e| other(e.described()))?.is_some_and(Value::truthy)
    {
        // Exception descriptions can contain page data: never copied.
        return Err(Failure::attach("inspector_evaluation_failed"));
    }
    let inner = py::get(result, "result").map_err(|e| other(e.described()))?.unwrap_or(&empty);
    Ok(Some(py::get(inner, "value").map_err(|e| other(e.described()))?.cloned().unwrap_or(Value::Null)))
}

/// `evaluate(url, expression)`: one `Runtime.evaluate` with `awaitPromise` and
/// `returnByValue`; events and replies to other ids are skipped.
///
/// # Errors
/// `inspector_evaluation_failed` for an error reply or a thrown exception, `Timeout`
/// when the WebSocket does not open within a second, `Other` otherwise.
pub async fn evaluate(url: &str, expression: &str) -> Result<Value, Failure> {
    let mut socket = open_socket(url).await?;
    let outcome = converse(&mut socket, expression).await;
    // The closing handshake waits for the peer at most a second (close_timeout=1).
    let _ = tokio::time::timeout(IO_TIMEOUT, async {
        if socket.close(None).await.is_ok() {
            while let Some(Ok(_)) = socket.next().await {}
        }
    })
    .await;
    outcome
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// The WebSocket, opened within a second, messages bounded as websockets bounds
/// them.
async fn open_socket(url: &str) -> Result<Socket, Failure> {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(131_072);
    config.max_frame_size = Some(131_072);
    let opened =
        tokio::time::timeout(IO_TIMEOUT, tokio_tungstenite::connect_async_with_config(url, Some(config), false)).await;
    match opened {
        Err(_) => Err(Failure::Timeout),
        Ok(Err(e)) => Err(other(e)),
        Ok(Ok((socket, _))) => Ok(socket),
    }
}

/// The evaluation sent, then the replies read until ours.
async fn converse(socket: &mut Socket, expression: &str) -> Result<Value, Failure> {
    socket.send(Message::text(evaluate_frame(expression))).await.map_err(other)?;
    loop {
        let message = next_message(socket).await?;
        if let Some(reply) = decode_message(&message)?
            && let Some(value) = read_reply(&reply)?
        {
            return Ok(value);
        }
    }
}

async fn next_message(socket: &mut Socket) -> Result<Message, Failure> {
    match socket.next().await {
        None => Err(other("ConnectionClosed")),
        Some(Err(e)) => Err(other(e)),
        Some(Ok(message)) => Ok(message),
    }
}

/// A data frame's JSON; None for a control frame, which is skipped.
fn decode_message(message: &Message) -> Result<Option<Value>, Failure> {
    let reply = match message {
        Message::Text(text) => py::loads_text(text.as_bytes()),
        Message::Binary(bytes) => {
            recog_core::pyjson::loads_bytes(bytes).map_err(|e| PyErr::new("JSONDecodeError", e.0))
        }
        Message::Close(_) => return Err(other("ConnectionClosed")),
        _ => return Ok(None),
    };
    reply.map(Some).map_err(|e| other(e.described()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_when_read_then_events_skipped_and_errors_redacted() {
        // given
        let event = py::obj([("method", Value::from("Debugger.paused"))]);
        let value = recog_core::pyjson::loads(r#"{"id": 1.0, "result": {"result": {"value": {"pid": 42}}}}"#).unwrap();
        let thrown =
            recog_core::pyjson::loads(r#"{"id": 1, "result": {"exceptionDetails": {"description": "private"}}}"#)
                .unwrap();
        let error = recog_core::pyjson::loads(r#"{"id": 1, "error": {"message": "private"}}"#).unwrap();
        // when / then
        assert_eq!(read_reply(&event).unwrap(), None);
        assert_eq!(read_reply(&value).unwrap(), Some(py::obj([("pid", Value::Int(42))])));
        assert_eq!(read_reply(&thrown).unwrap_err(), Failure::attach("inspector_evaluation_failed"));
        assert_eq!(read_reply(&error).unwrap_err(), Failure::attach("inspector_evaluation_failed"));
        assert!(matches!(read_reply(&Value::Array(vec![])), Err(Failure::Other(_))));
    }

    #[test]
    fn chunked_body_when_complete_then_joined() {
        // given / when / then
        assert_eq!(dechunk(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap(), Some(b"abcde".to_vec()));
        assert_eq!(dechunk(b"3\r\nab").unwrap(), None);
    }
}
