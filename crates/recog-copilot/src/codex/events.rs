//! The Codex harness's source of Copilot events: an MCP client of the channel that
//! long-polls `await_copilot_event` (plan D3). Each event is rendered by the channel's
//! own `format_channel_event` into the `<channel>` block a Claude session receives, so
//! the Copilot playbook reads the same on both providers; privacy masking already
//! happened in the channel.
//!
//! Plain HTTP/1.1 to the local channel, one connection per request: the channel's
//! streamable HTTP transport answers a POST with an SSE body or JSON.

use http_body_util::{BodyExt as _, Full};
use recog_core::pyjson::{self, Map, Value};
use serde_json::json;
use tokio::net::TcpStream;

use crate::channel::context::format_channel_event;

/// One Copilot event as a turn's input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotEvent {
    pub event_type: String,
    pub text: String,
}

pub struct ChannelClient {
    authority: String,
    path: String,
    session: Option<String>,
    next_id: u64,
}

impl ChannelClient {
    /// A client of the channel at `url` (`http://host:port/path`).
    ///
    /// # Errors
    /// A URL that is not plain `http://`.
    pub fn new(url: &str) -> Result<Self, String> {
        let rest = url.strip_prefix("http://").ok_or_else(|| format!("not an http:// URL: {url}"))?;
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_owned()),
        };
        Ok(ChannelClient { authority: authority.to_owned(), path, session: None, next_id: 0 })
    }

    /// A new MCP session: `initialize`, then `notifications/initialized`.
    ///
    /// # Errors
    /// The channel unreachable or refusing.
    pub async fn connect(&mut self) -> Result<(), String> {
        self.session = None;
        let client = json!({"name": "recog-copilot-codex", "version": env!("CARGO_PKG_VERSION")});
        let params = json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": client});
        let initialize = self.call("initialize", params);
        let (session, body) = self.post(&initialize).await?;
        result_of(&body)?;
        self.session = Some(session.ok_or("the channel gave no MCP session")?);
        self.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await?;
        Ok(())
    }

    /// The next event, or `None` when none arrived within `timeout_seconds`.
    ///
    /// # Errors
    /// The channel unreachable, its session gone, or an answer of the wrong shape.
    pub async fn next_event(&mut self, timeout_seconds: u64) -> Result<Option<CopilotEvent>, String> {
        let arguments = json!({"timeout_seconds": timeout_seconds});
        let call = self.call("tools/call", json!({"name": "await_copilot_event", "arguments": arguments}));
        let (_, body) = self.post(&call).await?;
        let result = result_of(&body)?;
        let text = result.pointer("/content/0/text").and_then(serde_json::Value::as_str).unwrap_or_default();
        if result.get("isError").and_then(serde_json::Value::as_bool) == Some(true) {
            return Err(text.to_owned());
        }
        let Ok(Value::Object(answer)) = pyjson::loads(text) else { return Err(format!("unreadable event: {text}")) };
        match answer.get("event") {
            Some(Value::Object(event)) => render(event).map(Some),
            Some(Value::Null) => Ok(None),
            _ => Err(format!("no event in {text}")),
        }
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
    }

    /// The session id the channel named, and the body.
    async fn post(&self, message: &serde_json::Value) -> Result<(Option<String>, String), String> {
        let stream = TcpStream::connect(&self.authority).await.map_err(|e| format!("channel unreachable: {e}"))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(connection);
        let mut request = hyper::Request::post(&self.path)
            .header("host", &self.authority)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if let Some(session) = &self.session {
            request = request.header("mcp-session-id", session);
        }
        let request = request.body(Full::new(bytes::Bytes::from(message.to_string()))).map_err(|e| e.to_string())?;
        let response = sender.send_request(request).await.map_err(|e| e.to_string())?;
        let status = response.status();
        let session = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()).map(str::to_owned);
        let body = response.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
        let body = String::from_utf8_lossy(&body).into_owned();
        if !status.is_success() {
            return Err(format!("channel answered {status}: {body}"));
        }
        Ok((session, body))
    }
}

/// The JSON-RPC result in a JSON or SSE body.
fn result_of(body: &str) -> Result<serde_json::Value, String> {
    let messages = body.lines().filter_map(|line| line.strip_prefix("data:")).map(str::trim).chain([body.trim()]);
    for message in messages {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(message) else { continue };
        if let Some(error) = message.get("error") {
            return Err(format!("channel error: {error}"));
        }
        if let Some(result) = message.get("result") {
            return Ok(result.clone());
        }
    }
    Err(format!("no result in the channel's answer: {body}"))
}

/// `<channel source="copilot" key="value"…>` with the push's body.
fn render(event: &Map) -> Result<CopilotEvent, String> {
    let (content, meta) = format_channel_event(event)?;
    let mut tag = String::from(r#"<channel source="copilot""#);
    for (key, value) in meta.iter() {
        tag.push_str(&format!(r#" {key}="{}""#, escape(&value.py_str())));
    }
    let event_type = meta.get("event_type").map(Value::py_str).unwrap_or_default();
    Ok(CopilotEvent { event_type, text: format!("{tag}>\n{content}\n</channel>") })
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}
