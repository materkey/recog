//! A WebSocket client for the tests: received text frames are collected by a task.

use std::sync::{Arc, Mutex};

use futures_util::stream::SplitSink;
use futures_util::{SinkExt as _, StreamExt as _};
use recog_core::pyjson::Value;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderName;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::{obj, until};

type Sink = SplitSink<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, Message>;

pub struct WsClient {
    pub frames: Arc<Mutex<Vec<String>>>,
    sink: tokio::sync::Mutex<Sink>,
    /// The close code, or the error that ended the connection.
    pub closed: Arc<Mutex<Option<String>>>,
}

/// Connects to `path` with these extra headers (each appended, so a header can be
/// sent twice); `Err` when the handshake is refused.
pub async fn connect(port: u16, path: &str, headers: &[(&str, &str)]) -> Result<WsClient, String> {
    let mut request = format!("ws://127.0.0.1:{port}{path}").into_client_request().map_err(|e| e.to_string())?;
    for (k, v) in headers {
        request.headers_mut().append(HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
    }
    let (socket, _) = tokio_tungstenite::connect_async(request).await.map_err(|e| e.to_string())?;
    let (sink, mut stream) = socket.split();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(Mutex::new(None));
    let (f, c) = (Arc::clone(&frames), Arc::clone(&closed));
    tokio::spawn(async move {
        while let Some(message) = stream.next().await {
            match message {
                Ok(Message::Text(text)) => f.lock().unwrap().push(text.as_str().to_owned()),
                Ok(Message::Close(frame)) => {
                    *c.lock().unwrap() =
                        Some(frame.map_or_else(|| "none".to_owned(), |f| u16::from(f.code).to_string()));
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    *c.lock().unwrap() = Some(e.to_string());
                    break;
                }
            }
        }
    });
    Ok(WsClient { frames, sink: tokio::sync::Mutex::new(sink), closed })
}

impl WsClient {
    pub async fn send(&self, text: &str) {
        self.sink.lock().await.send(Message::text(text.to_owned())).await.unwrap();
    }

    pub async fn send_binary(&self, bytes: &[u8]) {
        let _ = self.sink.lock().await.send(Message::binary(bytes.to_vec())).await;
    }

    pub async fn close(&self) {
        let _ = self.sink.lock().await.close().await;
    }

    pub fn frames(&self) -> Vec<String> {
        self.frames.lock().unwrap().clone()
    }

    pub fn closed(&self) -> Option<String> {
        self.closed.lock().unwrap().clone()
    }

    /// Waits until `n` frames arrived and returns them parsed.
    pub async fn wait_frames(&self, n: usize) -> Vec<Value> {
        let frames = Arc::clone(&self.frames);
        until(&format!("{n} frames"), move || frames.lock().unwrap().len() >= n).await;
        self.frames().iter().map(|f| obj(f)).collect()
    }
}
