//! The MCP Streamable HTTP endpoint at `/mcp`, with the wire behaviour of the Python
//! channel's transport (mcp 1.27 `StreamableHTTPSessionManager` in stateful mode,
//! behind Starlette and uvicorn): its sessions, status codes, error bodies, SSE framing
//! and pings. The protocol itself — the handshake, the request dispatch, the result
//! types — is rmcp's: each session runs an rmcp server over a channel transport.
//!
//! rmcp's own Streamable HTTP server is not used because it answers differently on the
//! wire (plain-text errors, a priming event on every stream, other status codes) and
//! does not expose what the channel needs to know about a session: whether it holds a
//! standing GET stream, the only route a channel push can travel. A session without
//! one is a zombie left by a killed client; it is neither counted nor pushed to.
//!
//! What mcp 1.27 validates before a message reaches the server — the JSON-RPC shape,
//! the request methods it knows, "request before initialization", "method not found" —
//! is checked here with its error codes and messages, so rmcp only sees what the
//! Python server would dispatch.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt as _, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::HeaderValue;
use hyper::{Method, Request, Response, StatusCode};
use recog_core::pyjson::{self, Map, Value};
use rmcp::model::{
    ClientJsonRpcMessage, CustomNotification, ErrorCode, ErrorData, JsonRpcMessage, NumberOrString, RequestId,
    ServerJsonRpcMessage, ServerNotification,
};
use rmcp::transport::Transport;
use rmcp::{RoleServer, service::serve_server_with_ct};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::handler::{CHANNEL_NOTIFICATION_METHOD, ChannelHandler, SUPPORTED_PROTOCOL_VERSIONS};
use super::link::BridgeLink;
use super::log;

pub type Body = UnsyncBoxBody<Bytes, Infallible>;

/// sse-starlette's default keep-alive comment interval.
pub const PING_INTERVAL: Duration = Duration::from_secs(15);

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
/// What a request without the protocol header is taken to speak.
const DEFAULT_NEGOTIATED_VERSION: &str = "2025-03-26";

/// Every request method of mcp 1.27's `ClientRequest` union.
const CLIENT_REQUEST_METHODS: [&str; 17] = [
    "ping",
    "initialize",
    "completion/complete",
    "logging/setLevel",
    "prompts/get",
    "prompts/list",
    "resources/list",
    "resources/templates/list",
    "resources/read",
    "resources/subscribe",
    "resources/unsubscribe",
    "tools/call",
    "tools/list",
    "tasks/get",
    "tasks/result",
    "tasks/list",
    "tasks/cancel",
];

/// The methods FastMCP registers handlers for; the rest are "Method not found".
const HANDLED_METHODS: [&str; 9] = [
    "ping",
    "initialize",
    "prompts/get",
    "prompts/list",
    "resources/list",
    "resources/templates/list",
    "resources/read",
    "tools/call",
    "tools/list",
];

/// The notifications passed on to the server; any other is dropped, as the Python
/// session drops what fails `ClientNotification` validation.
const FORWARDED_NOTIFICATIONS: [&str; 3] =
    ["notifications/initialized", "notifications/progress", "notifications/roots/list_changed"];

/// FastMCP's DNS-rebinding protection, on when it binds a loopback host.
#[derive(Clone, Debug)]
pub struct Security {
    pub allowed_hosts: Vec<&'static str>,
    pub allowed_origins: Vec<&'static str>,
}

impl Security {
    /// The settings FastMCP picks for `host`, `None` when protection is off.
    pub fn for_host(host: &str) -> Option<Self> {
        ["127.0.0.1", "localhost", "::1"].contains(&host).then(|| Security {
            allowed_hosts: vec!["127.0.0.1:*", "localhost:*", "[::1]:*"],
            allowed_origins: vec!["http://127.0.0.1:*", "http://localhost:*", "http://[::1]:*"],
        })
    }

    fn matches(value: &str, allowed: &[&str]) -> bool {
        allowed.contains(&value)
            || allowed.iter().any(|a| a.strip_suffix(":*").is_some_and(|base| value.starts_with(&format!("{base}:"))))
    }
}

/// One server-to-client message on an SSE stream.
struct Event {
    json: String,
    /// The response that ends a POST stream.
    last: bool,
}

type Stream = mpsc::UnboundedSender<Event>;

/// A request id as Python keys `_in_flight`: an int and a string never collide.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum IdKey {
    Int(i128),
    Str(String),
}

impl IdKey {
    fn of(id: &RequestId) -> Self {
        match id {
            NumberOrString::Number(n) => IdKey::Int(i128::from(*n)),
            NumberOrString::String(s) => IdKey::Str(s.to_string()),
        }
    }

    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Int(n) => Some(IdKey::Int(*n)),
            Value::Str(s) => Some(IdKey::Str(s.clone())),
            _ => None,
        }
    }

    /// `str(id)`: the key of the request's SSE stream.
    fn stream_key(&self) -> String {
        match self {
            IdKey::Int(n) => n.to_string(),
            IdKey::Str(s) => s.clone(),
        }
    }
}

#[derive(Default)]
struct SessionState {
    terminated: bool,
    /// Where client messages go once an `initialize` started the rmcp server.
    to_server: Option<mpsc::UnboundedSender<ClientJsonRpcMessage>>,
    server_ct: Option<CancellationToken>,
    initialized: bool,
    request_streams: HashMap<String, Stream>,
    /// The standing GET stream and the token of the response that holds it.
    get_stream: Option<(u64, Stream)>,
    next_token: u64,
    in_flight: HashSet<IdKey>,
    cancelled: HashSet<IdKey>,
}

/// One MCP session (`StreamableHTTPServerTransport`).
pub struct Session {
    pub id: String,
    state: Mutex<SessionState>,
}

impl Session {
    fn new(id: String) -> Self {
        Session { id, state: Mutex::new(SessionState::default()) }
    }

    fn state(&self) -> MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds a standing GET stream: the only route a channel push can travel.
    pub fn channel_reachable(&self) -> bool {
        let state = self.state();
        !state.terminated && state.get_stream.is_some()
    }

    pub fn is_terminated(&self) -> bool {
        self.state().terminated
    }

    /// `terminate()`: closes every stream (ending their responses) and stops the
    /// server, whose in-flight calls are cancelled.
    pub fn terminate(&self) {
        let mut state = self.state();
        if state.terminated {
            return;
        }
        state.terminated = true;
        state.request_streams.clear();
        state.get_stream = None;
        state.to_server = None;
        if let Some(ct) = state.server_ct.take() {
            ct.cancel();
        }
    }

    /// Where a message from the server goes: a response to its request's stream, a
    /// server-initiated message to the GET stream; without one it is dropped.
    fn route(&self, message: &ServerJsonRpcMessage) {
        let serialised = serde_json::to_string(message)
            .map_err(|e| log::line(&format!("{} cannot serialise a message: {e}", log::PREFIX)));
        let Ok(json) = serialised else { return };
        match answered_request(message) {
            Some(Some(id)) => self.answer(&IdKey::of(id), json),
            Some(None) => {}
            None => self.send_on_get_stream(json),
        }
    }

    /// A message of the server's own: on the GET stream, dropped without one.
    fn send_on_get_stream(&self, json: String) {
        if let Some((_, stream)) = &self.state().get_stream {
            let _ = stream.send(Event { json, last: false });
        }
    }

    /// Ends request `key`'s stream with its answer, unless the request was cancelled.
    fn answer(&self, key: &IdKey, json: String) {
        let mut state = self.state();
        if state.cancelled.remove(key) {
            return;
        }
        state.in_flight.remove(key);
        if let Some(stream) = state.request_streams.remove(&key.stream_key()) {
            let _ = stream.send(Event { json, last: true });
        }
    }

    /// Sends a serialised notification on the GET stream; false when there is none.
    pub fn push(&self, json: &str) -> bool {
        let state = self.state();
        match (&state.get_stream, state.terminated) {
            (Some((_, stream)), false) => stream.send(Event { json: json.to_owned(), last: false }).is_ok(),
            _ => false,
        }
    }

    /// Answers request `key` directly, without the server.
    fn reply(&self, key: &IdKey, message: &ServerJsonRpcMessage) {
        let Ok(json) = serde_json::to_string(message) else { return };
        if let Some(stream) = self.state().request_streams.remove(&key.stream_key()) {
            let _ = stream.send(Event { json, last: true });
        }
    }
}

/// The request a server message answers: `None` for a request or a notification of
/// the server's own, `Some(None)` for an error that names no request.
fn answered_request(message: &ServerJsonRpcMessage) -> Option<Option<&RequestId>> {
    match message {
        JsonRpcMessage::Response(response) => Some(Some(&response.id)),
        JsonRpcMessage::Error(error) => Some(error.id.as_ref()),
        JsonRpcMessage::Request(_) | JsonRpcMessage::Notification(_) => None,
    }
}

/// The rmcp transport of one session: client messages from the HTTP side in, server
/// messages routed onto the session's SSE streams.
struct SessionTransport {
    session: Arc<Session>,
    incoming: mpsc::UnboundedReceiver<ClientJsonRpcMessage>,
}

impl Transport<RoleServer> for SessionTransport {
    type Error = std::io::Error;

    fn send(&mut self, item: ServerJsonRpcMessage) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.session.route(&item);
        std::future::ready(Ok(()))
    }

    fn receive(&mut self) -> impl Future<Output = Option<ClientJsonRpcMessage>> + Send {
        self.incoming.recv()
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.incoming.close();
        std::future::ready(Ok(()))
    }
}

/// A JSON-RPC message as mcp 1.27 tells them apart.
enum Message {
    Request { id: IdKey, method: String, params: Value },
    Notification { method: String, params: Value },
    Other,
}

/// `JSONRPCMessage.model_validate`: a request needs an integer (not a bool) or string
/// id; anything else with a method is a notification. `params` must be an object.
fn classify(value: &Value) -> Result<Message, String> {
    let Some(object) = value.as_object() else {
        return Err(format!(
            "4 validation errors for JSONRPCMessage (input_value={}, input_type={})",
            super::pydantic::input_repr(value),
            value.type_name()
        ));
    };
    let version_ok = object.get("jsonrpc") == Some(&Value::from("2.0"));
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    let params_ok = matches!(params, Value::Null | Value::Object(_));
    let method = object.get("method").and_then(Value::as_str);
    let id = object.get("id").and_then(IdKey::from_value);
    if let (true, true, Some(method), Some(id)) = (version_ok, params_ok, method, id.clone()) {
        return Ok(Message::Request { id, method: method.to_owned(), params });
    }
    if let (true, true, Some(method)) = (version_ok, params_ok, method) {
        return Ok(Message::Notification { method: method.to_owned(), params });
    }
    let has_id = object.get("id").is_some_and(|v| matches!(v, Value::Int(_) | Value::Str(_)));
    let response_ok = version_ok && has_id && matches!(object.get("result"), Some(Value::Object(_)));
    let error_ok = version_ok
        && object.get("error").and_then(Value::as_object).is_some_and(|e| {
            matches!(e.get("code"), Some(Value::Int(_))) && matches!(e.get("message"), Some(Value::Str(_)))
        });
    if response_ok || error_ok {
        return Ok(Message::Other);
    }
    Err(format!("validation errors for JSONRPCMessage (input_value={})", super::pydantic::input_repr(value)))
}

/// What `ClientRequest` validation requires of the methods FastMCP handles.
fn params_valid(method: &str, params: &Value) -> bool {
    let object = params.as_object();
    let str_field = |key: &str| object.and_then(|p| p.get(key)).is_some_and(|v| matches!(v, Value::Str(_)));
    match method {
        "initialize" => {
            let client = object.and_then(|p| p.get("clientInfo")).and_then(Value::as_object);
            str_field("protocolVersion")
                && object.and_then(|p| p.get("capabilities")).is_some_and(|v| matches!(v, Value::Object(_)))
                && client.is_some_and(|c| {
                    matches!(c.get("name"), Some(Value::Str(_))) && matches!(c.get("version"), Some(Value::Str(_)))
                })
        }
        "tools/call" => {
            str_field("name")
                && object.and_then(|p| p.get("arguments")).is_none_or(|v| matches!(v, Value::Null | Value::Object(_)))
        }
        "prompts/get" => str_field("name"),
        "resources/read" => object
            .and_then(|p| p.get("uri"))
            .and_then(Value::as_str)
            .is_some_and(|uri| uri.split_once(':').is_some_and(|(scheme, _)| !scheme.is_empty())),
        _ => true,
    }
}

/// What `ServerSession` does with a request before (and instead of) its handler.
enum Verdict {
    Invalid,
    MethodNotFound,
    /// A ping before the server started, answered by the transport.
    Pong,
    Forward(Box<ClientJsonRpcMessage>),
}

/// `ClientRequest` validation, the ping before initialization, "request before
/// initialization", "method not found", in mcp 1.27's order. `parsed` is the
/// message as rmcp reads it, `None` when it cannot (or its id does not fit).
fn request_verdict(
    method: &str,
    params: &Value,
    started: bool,
    initialized: bool,
    parsed: Option<ClientJsonRpcMessage>,
) -> Verdict {
    if !known_request(method, params) {
        return Verdict::Invalid;
    }
    if method == "ping" && !started {
        return Verdict::Pong;
    }
    if method != "initialize" && !initialized {
        return Verdict::Invalid;
    }
    if !HANDLED_METHODS.contains(&method) {
        return Verdict::MethodNotFound;
    }
    parsed.map_or(Verdict::Invalid, |parsed| Verdict::Forward(Box::new(parsed)))
}

fn known_request(method: &str, params: &Value) -> bool {
    CLIENT_REQUEST_METHODS.contains(&method) && params_valid(method, params)
}

/// An error answer to the request, when its id can be written back.
fn reply_error(
    request_id: Option<RequestId>,
    reply: impl Fn(ServerJsonRpcMessage),
    code: i32,
    message: &str,
    data: Option<serde_json::Value>,
) {
    if let Some(request_id) = request_id {
        reply(JsonRpcMessage::error(ErrorData::new(ErrorCode(code), message.to_owned(), data), Some(request_id)));
    }
}

fn header<'a>(request: &'a Request<Incoming>, name: &str) -> Option<&'a str> {
    request.headers().get(name).and_then(|v| v.to_str().ok())
}

fn full(status: StatusCode, body: impl Into<Bytes>) -> Response<Body> {
    let mut response = Response::new(Full::new(body.into()).boxed_unsync());
    *response.status_mut() = status;
    response
}

fn with_header(mut response: Response<Body>, name: &'static str, value: &str) -> Response<Body> {
    if let Ok(value) = HeaderValue::from_str(value) {
        response.headers_mut().insert(name, value);
    }
    response
}

/// `_create_error_response`: a JSON-RPC error with the id `"server-error"`.
fn json_error(status: StatusCode, code: i32, message: &str, session: Option<&str>) -> Response<Body> {
    let error: ServerJsonRpcMessage = JsonRpcMessage::error(
        ErrorData::new(ErrorCode(code), message.to_owned(), None),
        Some(NumberOrString::String("server-error".into())),
    );
    let body = serde_json::to_string(&error).unwrap_or_default();
    let response = with_header(full(status, body), "content-type", "application/json");
    match session {
        Some(id) => with_header(response, SESSION_HEADER, id),
        None => response,
    }
}

/// An empty JSON response (`_create_json_response(None, status)`).
fn json_empty(status: StatusCode, session: &str) -> Response<Body> {
    with_header(with_header(full(status, Bytes::new()), "content-type", "application/json"), SESSION_HEADER, session)
}

/// `str(datetime.now(timezone.utc))`.
fn python_utc_now() -> String {
    let now = chrono::Utc::now();
    let micros = now.timestamp_subsec_micros();
    if micros == 0 {
        now.format("%Y-%m-%d %H:%M:%S+00:00").to_string()
    } else {
        format!("{}.{micros:06}+00:00", now.format("%Y-%m-%d %H:%M:%S"))
    }
}

/// A POST body as its text and JSON-RPC message, or the error answer: a body that
/// cannot be read or decoded, JSON that does not parse, a message that fails
/// `JSONRPCMessage` validation.
async fn read_message(session: &Session, request: Request<Incoming>) -> Result<(String, Message), Box<Response<Body>>> {
    let fail = |status, code, message: &str| Box::new(json_error(status, code, message, Some(&session.id)));
    let raw =
        request.into_body().collect().await.map_err(|e| {
            fail(StatusCode::INTERNAL_SERVER_ERROR, -32603, &format!("Error handling POST request: {e}"))
        })?;
    let text = decode_body(&raw.to_bytes()).map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, -32603, &e))?;
    let value =
        pyjson::loads(&text).map_err(|e| fail(StatusCode::BAD_REQUEST, -32700, &format!("Parse error: {e}")))?;
    let message =
        classify(&value).map_err(|e| fail(StatusCode::BAD_REQUEST, -32602, &format!("Validation error: {e}")))?;
    Ok((text, message))
}

/// The body as UTF-8 text without a leading BOM.
fn decode_body(raw: &[u8]) -> Result<String, String> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    std::str::from_utf8(raw)
        .map(str::to_owned)
        .map_err(|e| format!("Error handling POST request: {}", pyjson::utf8_error_message(raw, &e)))
}

/// The answer to a path other than `/mcp`: Starlette's redirect from `/mcp/`, else
/// its plain 404.
fn outside_endpoint(request: &Request<Incoming>) -> Option<Response<Body>> {
    let path = request.uri().path();
    if path == "/mcp" {
        return None;
    }
    if path == "/mcp/" {
        let host = header(request, "host").unwrap_or("").to_owned();
        let query = request.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        let location = format!("http://{host}/mcp{query}");
        return Some(with_header(full(StatusCode::TEMPORARY_REDIRECT, Bytes::new()), "location", &location));
    }
    Some(with_header(full(StatusCode::NOT_FOUND, "Not Found"), "content-type", "text/plain; charset=utf-8"))
}

/// Drops the GET stream registration when its response ends.
struct GetStreamGuard {
    session: Arc<Session>,
    token: u64,
}

impl Drop for GetStreamGuard {
    fn drop(&mut self) {
        let mut state = self.session.state();
        if state.get_stream.as_ref().is_some_and(|(token, _)| *token == self.token) {
            state.get_stream = None;
        }
    }
}

/// An sse-starlette response: `event: message` events, a ping comment every
/// `ping` while open, ended by the last event or by the stream closing.
fn sse_response(
    receiver: mpsc::UnboundedReceiver<Event>,
    ping: Duration,
    session: &str,
    guard: Option<GetStreamGuard>,
) -> Response<Body> {
    let interval = tokio::time::interval_at(tokio::time::Instant::now() + ping, ping);
    let state = (receiver, interval, false, guard);
    let stream = futures_util::stream::unfold(state, |(mut receiver, mut interval, done, guard)| async move {
        if done {
            return None;
        }
        let (text, done) = tokio::select! {
            event = receiver.recv() => {
                let event = event?;
                (format!("event: message\r\ndata: {}\r\n\r\n", event.json), event.last)
            }
            _ = interval.tick() => (format!(": ping - {}\r\n\r\n", python_utc_now()), false),
        };
        Some((Ok::<_, Infallible>(Frame::data(Bytes::from(text))), (receiver, interval, done, guard)))
    });
    let mut response = Response::new(StreamBody::new(stream).boxed_unsync());
    let headers = response.headers_mut();
    headers.insert("cache-control", HeaderValue::from_static("no-cache, no-transform"));
    headers.insert("connection", HeaderValue::from_static("keep-alive"));
    headers.insert("content-type", HeaderValue::from_static("text/event-stream"));
    if let Ok(value) = HeaderValue::from_str(session) {
        headers.insert(SESSION_HEADER, value);
    }
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// The channel's MCP endpoint and its sessions.
pub struct McpServer {
    pub link: Arc<BridgeLink>,
    /// `_server_instances`, in creation order.
    sessions: Mutex<Vec<Arc<Session>>>,
    security: Option<Security>,
    pub ping_interval: Duration,
}

impl McpServer {
    pub fn new(link: Arc<BridgeLink>, host: &str) -> Self {
        McpServer {
            link,
            sessions: Mutex::new(Vec::new()),
            security: Security::for_host(host),
            ping_interval: PING_INTERVAL,
        }
    }

    fn sessions(&self) -> MutexGuard<'_, Vec<Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sessions in creation order (`_server_instances.values()`).
    pub fn session_list(&self) -> Vec<Arc<Session>> {
        self.sessions().clone()
    }

    /// Forgets a session (the reaper's `instances.pop`).
    pub fn remove_session(&self, id: &str) -> Option<Arc<Session>> {
        let mut sessions = self.sessions();
        let at = sessions.iter().position(|s| s.id == id)?;
        Some(sessions.remove(at))
    }

    /// Sessions that can receive a channel push.
    pub fn reachable_count(&self) -> usize {
        self.sessions().iter().filter(|s| s.channel_reachable()).count()
    }

    /// `_broadcast_channel_notification`: the push written to every session holding a
    /// standing GET stream; returns how many.
    pub fn broadcast(&self, content: &str, meta: &Map) -> usize {
        let params = serde_json::json!({"content": content, "meta": json_from_map(meta)});
        let message: ServerJsonRpcMessage = JsonRpcMessage::notification(ServerNotification::CustomNotification(
            CustomNotification::new(CHANNEL_NOTIFICATION_METHOD, Some(params)),
        ));
        // pydantic writes a notification's fields as `method`, `params`, `jsonrpc`.
        let Ok(serde_json::Value::Object(mut fields)) = serde_json::to_value(&message) else { return 0 };
        let mut ordered: serde_json::Map<String, serde_json::Value> = ["method", "params", "jsonrpc"]
            .into_iter()
            .filter_map(|key| fields.remove(key).map(|value| (key.to_owned(), value)))
            .collect();
        ordered.extend(fields);
        let json = serde_json::Value::Object(ordered).to_string();
        self.session_list().iter().filter(|session| session.push(&json)).count()
    }

    /// One HTTP request.
    pub async fn handle(self: Arc<Self>, request: Request<Incoming>) -> Response<Body> {
        if let Some(response) = outside_endpoint(&request) {
            return response;
        }
        let session = match self.session_for(&request) {
            Ok(session) => session,
            Err(response) => return *response,
        };
        if let Some(response) = self.security_check(&request) {
            return response;
        }
        if session.is_terminated() {
            return json_error(
                StatusCode::NOT_FOUND,
                -32600,
                "Not Found: Session has been terminated",
                Some(&session.id),
            );
        }
        self.by_method(session, request).await
    }

    /// The session a request names, or a new one when it names none, whatever the
    /// method.
    fn session_for(&self, request: &Request<Incoming>) -> Result<Arc<Session>, Box<Response<Body>>> {
        let Some(value) = request.headers().get(SESSION_HEADER) else {
            let session = Arc::new(Session::new(uuid::Uuid::new_v4().simple().to_string()));
            self.sessions().push(session.clone());
            return Ok(session);
        };
        let found = value.to_str().ok().and_then(|id| self.sessions().iter().find(|s| s.id == id).cloned());
        found.ok_or_else(|| Box::new(json_error(StatusCode::NOT_FOUND, -32600, "Session not found", None)))
    }

    async fn by_method(&self, session: Arc<Session>, request: Request<Incoming>) -> Response<Body> {
        match *request.method() {
            Method::POST => self.post(session, request).await,
            Method::GET => self.get(session, &request),
            Method::DELETE => self.delete(&session, &request),
            _ => with_header(
                json_error(StatusCode::METHOD_NOT_ALLOWED, -32600, "Method Not Allowed", Some(&session.id)),
                "allow",
                "GET, POST, DELETE",
            ),
        }
    }

    /// `TransportSecurityMiddleware.validate_request`.
    fn security_check(&self, request: &Request<Incoming>) -> Option<Response<Body>> {
        if request.method() == Method::POST {
            let content_type = header(request, "content-type").unwrap_or("");
            if !content_type.to_lowercase().starts_with("application/json") {
                return Some(full(StatusCode::BAD_REQUEST, "Invalid Content-Type header"));
            }
        }
        let security = self.security.as_ref()?;
        let host = header(request, "host").unwrap_or("");
        if host.is_empty() || !Security::matches(host, &security.allowed_hosts) {
            return Some(full(StatusCode::MISDIRECTED_REQUEST, "Invalid Host header"));
        }
        if let Some(origin) = header(request, "origin").filter(|o| !o.is_empty())
            && !Security::matches(origin, &security.allowed_origins)
        {
            return Some(full(StatusCode::FORBIDDEN, "Invalid Origin header"));
        }
        None
    }

    /// `_validate_request_headers`: the session header, then the protocol version.
    fn validate_headers(session: &Session, request: &Request<Incoming>) -> Option<Response<Body>> {
        if header(request, SESSION_HEADER).is_none() {
            return Some(json_error(
                StatusCode::BAD_REQUEST,
                -32600,
                "Bad Request: Missing session ID",
                Some(&session.id),
            ));
        }
        let version = header(request, PROTOCOL_HEADER).unwrap_or(DEFAULT_NEGOTIATED_VERSION);
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
            let message = format!(
                "Bad Request: Unsupported protocol version: {version}. Supported versions: {}",
                SUPPORTED_PROTOCOL_VERSIONS.join(", ")
            );
            return Some(json_error(StatusCode::BAD_REQUEST, -32600, &message, Some(&session.id)));
        }
        None
    }

    async fn post(&self, session: Arc<Session>, request: Request<Incoming>) -> Response<Body> {
        if let Some(response) = Self::post_headers_error(&session, &request) {
            return response;
        }
        let headers_error = Self::validate_headers(&session, &request);
        let (text, message) = match read_message(&session, request).await {
            Ok(read) => read,
            Err(response) => return *response,
        };
        let is_initialize = matches!(&message, Message::Request { method, .. } if method == "initialize");
        if !is_initialize && let Some(response) = headers_error {
            return response;
        }
        self.accept_message(&session, message, &text)
    }

    /// What a POST must accept and send: both JSON and SSE answers, a JSON body.
    fn post_headers_error(session: &Session, request: &Request<Incoming>) -> Option<Response<Body>> {
        let accept = header(request, "accept").unwrap_or("");
        let types: Vec<&str> = accept.split(',').map(str::trim).collect();
        let has_json = types.iter().any(|t| t.starts_with("application/json"));
        let has_sse = types.iter().any(|t| t.starts_with("text/event-stream"));
        if !(has_json && has_sse) {
            return Some(json_error(
                StatusCode::NOT_ACCEPTABLE,
                -32600,
                "Not Acceptable: Client must accept both application/json and text/event-stream",
                Some(&session.id),
            ));
        }
        let content_type = header(request, "content-type").unwrap_or("");
        let main = content_type.split(';').next().unwrap_or("");
        if !main.split(',').any(|part| part.trim() == "application/json") {
            return Some(json_error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                -32600,
                "Unsupported Media Type: Content-Type must be application/json",
                Some(&session.id),
            ));
        }
        None
    }

    /// A request opens its SSE stream and goes to dispatch; anything else is accepted
    /// with an empty 202.
    fn accept_message(&self, session: &Arc<Session>, message: Message, text: &str) -> Response<Body> {
        match message {
            Message::Request { id, method, params } => {
                let (sender, receiver) = mpsc::unbounded_channel();
                session.state().request_streams.insert(id.stream_key(), sender);
                let response = sse_response(receiver, self.ping_interval, &session.id, None);
                self.dispatch_request(session, &id, &method, &params, text);
                response
            }
            Message::Notification { method, params } => {
                self.dispatch_notification(session, &method, &params, text);
                json_empty(StatusCode::ACCEPTED, &session.id)
            }
            Message::Other => json_empty(StatusCode::ACCEPTED, &session.id),
        }
    }

    /// What `ServerSession` does with a request before (and instead of) its handler.
    fn dispatch_request(&self, session: &Arc<Session>, id: &IdKey, method: &str, params: &Value, raw: &str) {
        let request_id = match id {
            IdKey::Int(n) => i64::try_from(*n).map(NumberOrString::Number).ok(),
            IdKey::Str(s) => Some(NumberOrString::String(s.as_str().into())),
        };
        let parsed = serde_json::from_str::<ClientJsonRpcMessage>(raw).ok().filter(|_| request_id.is_some());
        let (started, initialized) = {
            let state = session.state();
            (state.to_server.is_some(), state.initialized)
        };
        let reply = |message: ServerJsonRpcMessage| session.reply(id, &message);
        match request_verdict(method, params, started, initialized, parsed) {
            Verdict::Invalid => reply_error(request_id, reply, -32602, "Invalid request parameters", Some("".into())),
            Verdict::MethodNotFound => reply_error(request_id, reply, -32601, "Method not found", None),
            Verdict::Pong => {
                if let Some(request_id) = request_id {
                    reply(JsonRpcMessage::response(rmcp::model::ServerResult::EmptyResult(().into()), request_id));
                }
            }
            Verdict::Forward(parsed) => self.forward_request(session, id, method, *parsed, started),
        }
    }

    /// Hands a request to the session's rmcp server, starting it on the first
    /// `initialize`.
    fn forward_request(
        &self,
        session: &Arc<Session>,
        id: &IdKey,
        method: &str,
        parsed: ClientJsonRpcMessage,
        started: bool,
    ) {
        if method == "initialize" && !started {
            self.start_server(session);
        }
        let mut state = session.state();
        if method == "initialize" {
            state.initialized = true;
        }
        state.in_flight.insert(id.clone());
        if let Some(to_server) = &state.to_server {
            let _ = to_server.send(parsed);
        }
    }

    /// Starts the rmcp server of a session on its first `initialize`.
    fn start_server(&self, session: &Arc<Session>) {
        let (sender, incoming) = mpsc::unbounded_channel();
        let ct = CancellationToken::new();
        {
            let mut state = session.state();
            state.to_server = Some(sender);
            state.server_ct = Some(ct.clone());
        }
        let transport = SessionTransport { session: session.clone(), incoming };
        let handler = ChannelHandler { link: self.link.clone() };
        let id = session.id.clone();
        tokio::spawn(async move {
            match serve_server_with_ct(handler, transport, ct).await {
                Ok(running) => {
                    let _ = running.waiting().await;
                }
                Err(e) => log::line(&format!("{} session {id} failed to initialize: {e}", log::PREFIX)),
            }
        });
    }

    fn dispatch_notification(&self, session: &Arc<Session>, method: &str, params: &Value, raw: &str) {
        if method == "notifications/cancelled" {
            let Some(key) = params.as_object().and_then(|p| p.get("requestId")).and_then(IdKey::from_value) else {
                return;
            };
            let cancelled = {
                let mut state = session.state();
                let in_flight = state.in_flight.remove(&key);
                if in_flight {
                    state.cancelled.insert(key.clone());
                }
                in_flight
            };
            if !cancelled {
                return;
            }
            let request_id = match &key {
                IdKey::Int(n) => i64::try_from(*n).map(NumberOrString::Number).ok(),
                IdKey::Str(s) => Some(NumberOrString::String(s.as_str().into())),
            };
            if let Some(request_id) = request_id {
                session.reply(&key, &JsonRpcMessage::error(super::handler::request_cancelled(), Some(request_id)));
            }
        } else if !FORWARDED_NOTIFICATIONS.contains(&method) {
            return;
        }
        let Ok(parsed) = serde_json::from_str::<ClientJsonRpcMessage>(raw) else { return };
        if let Some(to_server) = &session.state().to_server {
            let _ = to_server.send(parsed);
        }
    }

    fn get(&self, session: Arc<Session>, request: &Request<Incoming>) -> Response<Body> {
        let accept = header(request, "accept").unwrap_or("");
        if !accept.split(',').map(str::trim).any(|t| t.starts_with("text/event-stream")) {
            return json_error(
                StatusCode::NOT_ACCEPTABLE,
                -32600,
                "Not Acceptable: Client must accept text/event-stream",
                Some(&session.id),
            );
        }
        if let Some(response) = Self::validate_headers(&session, request) {
            return response;
        }
        if header(request, "last-event-id").is_some_and(|v| !v.is_empty()) {
            // Without an event store the Python transport fails here.
            return with_header(
                full(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
                "content-type",
                "text/plain; charset=utf-8",
            );
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        let token = {
            let mut state = session.state();
            if state.get_stream.is_some() {
                drop(state);
                return json_error(
                    StatusCode::CONFLICT,
                    -32600,
                    "Conflict: Only one SSE stream is allowed per session",
                    Some(&session.id),
                );
            }
            state.next_token += 1;
            let token = state.next_token;
            state.get_stream = Some((token, sender));
            token
        };
        let id = session.id.clone();
        let guard = GetStreamGuard { session, token };
        sse_response(receiver, self.ping_interval, &id, Some(guard))
    }

    fn delete(&self, session: &Session, request: &Request<Incoming>) -> Response<Body> {
        if let Some(response) = Self::validate_headers(session, request) {
            return response;
        }
        session.terminate();
        json_empty(StatusCode::OK, &session.id)
    }

    /// Serves `/mcp` on `listener` until it fails.
    ///
    /// # Errors
    /// Accepting a connection failed.
    pub async fn serve(self: Arc<Self>, listener: tokio::net::TcpListener) -> std::io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request| {
                    let server = server.clone();
                    async move { Ok::<_, Infallible>(server.handle(request).await) }
                });
                let io = hyper_util::rt::TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await;
            });
        }
    }
}

/// serde's JSON of a pyjson dict whose values are strings (the push's meta).
fn json_from_map(map: &Map) -> serde_json::Value {
    serde_json::Value::Object(map.iter().map(|(k, v)| (k.clone(), json_from_value(v))).collect())
}

fn json_from_value(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => i64::try_from(*n)
            .map(serde_json::Value::from)
            .or_else(|_| u64::try_from(*n).map(serde_json::Value::from))
            .unwrap_or_else(|_| {
                #[allow(clippy::cast_precision_loss)]
                serde_json::Value::from(*n as f64)
            }),
        Value::Float(x) => serde_json::Value::from(*x),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(json_from_value).collect()),
        Value::Object(map) => json_from_map(map),
    }
}
