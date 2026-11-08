//! Doubles for the channel tests, the counterparts of the Python test doubles
//! (`FakeBridgeServer`, `_RecordingWS`, the patched `link.send`), plus a small HTTP
//! client for the MCP endpoint.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use recog_copilot::channel::link::{BridgeLink, SendFuture, Socket};
use recog_copilot::channel::transport::McpServer;
use recog_core::privacy::PrivacyStore;
use recog_core::pyjson::{self, Map, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// A link whose privacy setting lives in a fresh temporary data directory.
pub fn link_in(dir: &tempfile::TempDir) -> Arc<BridgeLink> {
    Arc::new(BridgeLink::new(PrivacyStore::new(dir.path())))
}

pub fn object(text: &str) -> Map {
    pyjson::loads(text).unwrap_or_else(|e| panic!("{text}: {e}")).as_object().unwrap().clone()
}

pub fn dumps(map: &Map) -> String {
    pyjson::dumps(&Value::Object(map.clone()), None)
}

type Hook = Box<dyn Fn(&Map) + Send>;

/// `_RecordingWS`: keeps every frame sent; `on_send` sees each one first.
#[derive(Default)]
pub struct RecordingSocket {
    pub sent: Mutex<Vec<Map>>,
    pub on_send: Mutex<Option<Hook>>,
}

impl RecordingSocket {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_hook(hook: impl Fn(&Map) + Send + 'static) -> Arc<Self> {
        let socket = Self::new();
        *socket.on_send.lock().unwrap() = Some(Box::new(hook));
        socket
    }

    pub fn frames(&self) -> Vec<Map> {
        self.sent.lock().unwrap().clone()
    }
}

impl Socket for RecordingSocket {
    fn send(&self, text: String) -> SendFuture {
        let frame = pyjson::loads(&text).unwrap().as_object().unwrap().clone();
        if let Some(hook) = self.on_send.lock().unwrap().as_ref() {
            hook(&frame);
        }
        self.sent.lock().unwrap().push(frame);
        Box::pin(std::future::ready(Ok(())))
    }
}

/// A send that never finishes, as the Python test's `slow_send`; `started` fires.
pub struct BlockingSocket {
    pub started: Arc<tokio::sync::Notify>,
}

impl Socket for BlockingSocket {
    fn send(&self, _text: String) -> SendFuture {
        self.started.notify_one();
        Box::pin(std::future::pending())
    }
}

/// `FakeBridgeServer`: a WebSocket server on a random port that records frames and
/// answers through `responder`.
pub struct FakeBridge {
    pub url: String,
    pub received: mpsc::UnboundedReceiver<Map>,
    pub push: mpsc::UnboundedSender<String>,
}

impl FakeBridge {
    pub async fn start(responder: impl Fn(&Map) -> Option<Map> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (received_tx, received) = mpsc::unbounded_channel();
        let (push, mut push_rx) = mpsc::unbounded_channel::<String>();
        let responder = Arc::new(responder);
        // One connection at a time, as many as the link makes.
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let Ok(socket) = tokio_tungstenite::accept_async(stream).await else { continue };
                serve(socket, &received_tx, &mut push_rx, responder.as_ref()).await;
            }
        });
        FakeBridge { url, received, push }
    }
}

async fn serve(
    socket: tokio_tungstenite::WebSocketStream<TcpStream>,
    received_tx: &mpsc::UnboundedSender<Map>,
    push_rx: &mut mpsc::UnboundedReceiver<String>,
    responder: &(dyn Fn(&Map) -> Option<Map> + Send + Sync),
) {
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            frame = stream.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text,
                    // Pings (the Python client sends one every 20 s) and pongs.
                    Some(Ok(_)) => continue,
                    _ => break,
                };
                let Ok(Value::Object(frame)) = pyjson::loads(&text) else { continue };
                let _ = received_tx.send(frame.clone());
                if let Some(answer) = responder(&frame)
                    && sink.send(Message::text(dumps(&answer))).await.is_err()
                {
                    break;
                }
            }
            text = push_rx.recv() => {
                let Some(text) = text else { break };
                if sink.send(Message::text(text)).await.is_err() {
                    break;
                }
            }
        }
    }
}

impl FakeBridge {
    /// The next frame the bridge received, within two seconds.
    pub async fn next(&mut self) -> Map {
        tokio::time::timeout(Duration::from_secs(2), self.received.recv())
            .await
            .expect("a frame within 2 s")
            .expect("the bridge is up")
    }
}

/// Polls `condition` every 20 ms for up to two seconds.
pub async fn wait_for(condition: impl Fn() -> bool) -> bool {
    for _ in 0..100 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    condition()
}

/// The MCP endpoint serving on a random port.
pub async fn mcp_endpoint(link: Arc<BridgeLink>) -> (Arc<McpServer>, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = Arc::new(McpServer::new(link, "127.0.0.1"));
    tokio::spawn(server.clone().serve(listener));
    (server, address)
}

/// An HTTP response as the tests read it.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Incoming,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The whole body.
    pub async fn text(self) -> String {
        String::from_utf8(self.body.collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    /// The JSON of every `data:` line of an SSE body read to its end.
    pub async fn events(self) -> Vec<serde_json::Value> {
        sse_data(&self.text().await)
    }
}

/// The JSON of every `data:` line.
pub fn sse_data(text: &str) -> Vec<serde_json::Value> {
    text.split("\r\n")
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

/// One request on a fresh connection.
pub async fn request(
    address: SocketAddr,
    method: &str,
    headers: &[(&str, &str)],
    body: impl Into<Bytes>,
) -> HttpResponse {
    let stream = TcpStream::connect(address).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await.unwrap();
    tokio::spawn(connection);
    let mut builder = hyper::Request::builder().method(method).uri("/mcp").header("host", address.to_string());
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = sender.send_request(builder.body(Full::new(body.into())).unwrap()).await.unwrap();
    let status = response.status().as_u16();
    let headers =
        response.headers().iter().map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned())).collect();
    HttpResponse { status, headers, body: response.into_body() }
}

pub const ACCEPT: (&str, &str) = ("accept", "application/json, text/event-stream");
pub const JSON: (&str, &str) = ("content-type", "application/json");

/// POSTs a JSON-RPC message on session `session`.
pub async fn post(address: SocketAddr, session: Option<&str>, body: &serde_json::Value) -> HttpResponse {
    let mut headers = vec![ACCEPT, JSON];
    if let Some(session) = session {
        headers.push(("mcp-session-id", session));
    }
    request(address, "POST", &headers, body.to_string()).await
}

/// Initializes a session (and sends `notifications/initialized`); returns its id.
pub async fn initialize(address: SocketAddr) -> String {
    let init = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}});
    let response = post(address, None, &init).await;
    let session = response.header("mcp-session-id").unwrap().to_owned();
    response.events().await;
    let initialized = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(post(address, Some(&session), &initialized).await.status, 202);
    session
}

/// A standing GET stream of `session`: its data events arrive on the receiver.
pub async fn open_get_stream(address: SocketAddr, session: &str) -> mpsc::UnboundedReceiver<serde_json::Value> {
    let response =
        request(address, "GET", &[("accept", "text/event-stream"), ("mcp-session-id", session)], Bytes::new()).await;
    assert_eq!(response.status, 200);
    let (tx, rx) = mpsc::unbounded_channel();
    let mut body = response.body;
    // Dropping the receiver drops the body, which closes the connection: the client
    // going away.
    tokio::spawn(async move {
        let mut buffer = String::new();
        loop {
            let frame = tokio::select! {
                frame = body.frame() => frame,
                () = tx.closed() => break,
            };
            let Some(Ok(frame)) = frame else { break };
            let Ok(data) = frame.into_data() else { continue };
            buffer.push_str(&String::from_utf8_lossy(&data));
            while let Some(end) = buffer.find("\r\n\r\n") {
                let event: String = buffer.drain(..end + 4).collect();
                for value in sse_data(&event) {
                    let _ = tx.send(value);
                }
            }
        }
    });
    rx
}

/// A tool call's result through the handler: the JSON of its text and `isError`.
pub async fn call_tool(link: &Arc<BridgeLink>, name: &str, arguments: &str) -> (serde_json::Value, bool) {
    let handler = recog_copilot::channel::handler::ChannelHandler { link: link.clone() };
    let result = handler
        .call(name, &object(arguments), &tokio_util::sync::CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let value = serde_json::to_value(&result).unwrap();
    let text = value["content"][0]["text"].as_str().unwrap().to_owned();
    let is_error = value["isError"].as_bool().unwrap();
    (serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)), is_error)
}
