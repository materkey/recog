//! A connected client as the bridge sees it: an id, a way to send text and to close,
//! the address it came from and the Origin headers of its handshake.
//!
//! [`crate::server`] makes clients from WebSocket connections; tests and the
//! differential probe make them from fakes, the way the Python tests hand fake
//! websockets to `_handle_client`.

use std::fmt;
use std::sync::Arc;

use futures_util::stream::BoxStream;

use crate::hooks::BoxFuture;

/// Identifies a client for the bridge's sets; ids grow with each connection, so sets
/// iterate in connection order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u64);

/// A failed send: the text of the exception Python's `send` would raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendError(pub String);

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The sending half of a connection (`websocket.send` / `websocket.close`).
pub trait ClientSink: Send + Sync {
    /// Resolves once the text was written, as `await websocket.send(text)`; may never
    /// resolve for a peer that stopped reading (the bridge bounds its broadcasts).
    fn send(&self, text: String) -> BoxFuture<Result<(), SendError>>;
    /// `await websocket.close(code, reason)`.
    fn close(&self, code: u16, reason: String) -> BoxFuture<Result<(), SendError>>;
}

/// One received message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Text(String),
    /// `json.loads` takes bytes too.
    Binary(Vec<u8>),
}

/// What a connection hands the bridge: its sink, its messages until it closes (an
/// error ends it as Python's `ConnectionClosedError` does), its peer address as Python
/// prints it, and the Origin header values of its handshake (None when the transport
/// has no handshake, like the Python tests' fakes).
pub struct Connection {
    pub sink: Arc<dyn ClientSink>,
    pub inbound: BoxStream<'static, Result<Inbound, String>>,
    pub remote_address: String,
    pub origins: Option<Vec<String>>,
}

/// A registered client.
pub struct Client {
    pub id: ClientId,
    pub sink: Arc<dyn ClientSink>,
    pub remote_address: String,
}

impl Client {
    #[must_use]
    pub fn new(id: ClientId, sink: Arc<dyn ClientSink>, remote_address: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { id, sink, remote_address: remote_address.into() })
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("id", &self.id).field("remote_address", &self.remote_address).finish()
    }
}

/// Python's `str()` of a socket address: `('127.0.0.1', 50000)`, or the 4-tuple of
/// IPv6.
#[must_use]
pub fn python_address(addr: &std::net::SocketAddr) -> String {
    match addr {
        std::net::SocketAddr::V4(a) => format!("('{}', {})", a.ip(), a.port()),
        std::net::SocketAddr::V6(a) => format!("('{}', {}, {}, {})", a.ip(), a.port(), a.flowinfo(), a.scope_id()),
    }
}
