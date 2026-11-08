//! The two WebSocket endpoints: `/api/live/stream` (the live meeting, the Copilot
//! relay) and `/api/meetings/{id}/watch`.
//!
//! The live stream is gated by the Origin allowlist before the handshake completes
//! (WebSockets bypass CORS), sends `init` with the current meeting, then the cached
//! agent-state frame and the shared dialog (`copilot_history`), and relays the
//! browser's three Copilot frame types to the bridge verbatim, answering
//! `bridge_disconnected` when the link is down. Closing a tab sends nothing upstream.

use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::http::HeaderMap;
use futures_util::{SinkExt as _, StreamExt as _};
use recog_core::pyjson::Value;
use recog_core::ws_origin::origin_allowed;
use tokio::sync::{mpsc, oneshot};

use crate::json;
use crate::request::loads_untrusted;
use crate::server::{AppState, INBOUND_COPILOT_TYPES};
use crate::util::Client;

/// Why a live-stream handshake is refused: duplicate Origin headers or an origin
/// outside the allowlist.
#[must_use]
pub fn origin_rejection(headers: &HeaderMap, extra: &[String]) -> Option<String> {
    let origins: Vec<String> =
        headers.get_all("origin").iter().map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned()).collect();
    if origins.len() > 1 {
        return Some("duplicate Origin headers".to_owned());
    }
    let origin = origins.first().map(String::as_str);
    if origin_allowed(origin, extra) {
        return None;
    }
    Some(format!("disallowed origin {}", origin.map_or_else(|| "None".to_owned(), recog_core::py::repr)))
}

/// `handle_client_frame`: a whitelisted Copilot frame relayed verbatim; on failure
/// the originating client gets `bridge_disconnected` with its `request_id`.
pub async fn handle_client_frame(state: &AppState, raw: &str, reply: &(dyn Fn(String) -> bool + Sync)) {
    let data = match loads_untrusted(raw) {
        Ok(Value::Object(map)) => map,
        Ok(_) => {
            eprintln!("[live-stream] dropped non-object client frame");
            return;
        }
        Err(_) => {
            eprintln!("[live-stream] dropped malformed client frame");
            return;
        }
    };
    let kind = data.get("type");
    if !kind.and_then(Value::as_str).is_some_and(|k| INBOUND_COPILOT_TYPES.contains(&k)) {
        let shown = kind.map_or_else(|| "None".to_owned(), Value::py_repr);
        eprintln!("[live-stream] dropped non-copilot client frame type={shown}");
        return;
    }
    let sent = match state.subscriber() {
        Some(subscriber) => subscriber.send(raw.to_owned()).await,
        None => false,
    };
    if sent {
        return;
    }
    let frame = json::object([
        ("type", Value::from("copilot_status")),
        ("status", Value::from("bridge_disconnected")),
        ("request_id", data.get("request_id").cloned().unwrap_or(Value::Null)),
    ]);
    if !reply(json::ascii(&frame)) {
        eprintln!("[live-stream] bridge_disconnected reply failed: connection closed");
    }
}

/// How an endpoint's receive loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Closed,
    Crashed,
}

/// Drains a client's queue into its socket until the endpoint says how it ended.
fn spawn_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut rx: mpsc::UnboundedReceiver<String>,
    close: oneshot::Receiver<Ended>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut close = close;
        let ended = loop {
            tokio::select! {
                biased;
                ended = &mut close => break ended.unwrap_or(Ended::Closed),
                message = rx.recv() => match message {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            return;
                        }
                    }
                    None => break close.await.unwrap_or(Ended::Closed),
                },
            }
        };
        while let Ok(text) = rx.try_recv() {
            let _ = sink.send(Message::Text(text.into())).await;
        }
        // An endpoint that died (Starlette's receive_text on a binary frame) drops the
        // connection without a closing handshake.
        if ended == Ended::Closed {
            let _ = sink.send(Message::Close(Some(CloseFrame { code: 1000, reason: "".into() }))).await;
            let _ = sink.close().await;
        }
    })
}

/// What the live stream sends right after the handshake.
async fn send_init(state: &Arc<AppState>, client: &Client) -> Result<(), String> {
    let storage = state.config.storage().map_err(|e| e.to_string())?;
    let (guard, active) =
        state.live_watcher.read_active_meetings(state.config.storage().map_err(|e| e.to_string())?).await;
    let active = active.map_err(|e| e.to_string())?;
    let init = match active.first() {
        Some(current) => {
            let fields = current.as_object().cloned().unwrap_or_default();
            let id = fields.get("id").map(Value::py_str).unwrap_or_default();
            let source = fields.get("source").filter(|v| !v.is_null()).cloned();
            let refine = source.as_ref().and_then(Value::as_str) == Some("ktalk");
            state.live_watcher.set_current_meeting(Some(&id), source);
            let meeting = storage.get_meeting(&id).map_err(|e| e.to_string())?;
            let segments = meeting
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|m| m.get("segments"))
                .cloned()
                .unwrap_or(Value::Array(Vec::new()));
            if let Value::Array(rows) = &segments {
                state.live_watcher.seed_post_init_watermark(rows.iter().filter_map(|s| s.as_object()?.get("id")));
            }
            drop(guard);
            json::object([
                ("type", Value::from("init")),
                ("meeting", current.clone()),
                ("segments", segments),
                ("active_meetings", Value::Array(active.clone())),
                ("refine_enabled", Value::Bool(refine)),
            ])
        }
        None => {
            drop(guard);
            json::object([
                ("type", Value::from("init")),
                ("meeting", Value::Null),
                ("segments", Value::Array(Vec::new())),
                ("active_meetings", Value::Array(Vec::new())),
                ("refine_enabled", Value::Bool(false)),
            ])
        }
    };
    client.send_text(&json::ascii(&init));
    if let Some(primer) = state.agent_state_primer() {
        client.send_text(&primer);
    }
    if let Some(history) = state.dialog_history() {
        client.send_text(&history);
    }
    Ok(())
}

/// `/api/live/stream` after an accepted handshake.
pub async fn live_stream(state: Arc<AppState>, socket: WebSocket) {
    let (sink, mut stream) = socket.split();
    let (client, rx) = Client::new();
    let (close_tx, close_rx) = oneshot::channel();
    let writer = spawn_writer(sink, rx, close_rx);
    state.live_watcher.register(client.clone());
    if let Err(message) = send_init(&state, &client).await {
        client.send_text(&json::ascii(&json::object([
            ("type", Value::from("error")),
            ("message", Value::from(message)),
        ])));
    }
    let reply_client = client.clone();
    let reply = move |raw: String| reply_client.send_text(&raw);
    let mut close_code = None;
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Text(text)) => handle_client_frame(&state, text.as_str(), &reply).await,
            Ok(Message::Binary(_)) => {
                // Starlette's receive_text fails on a binary frame; the handler dies.
                close_code = Some(Ended::Crashed);
                break;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    state.live_watcher.unregister(&client);
    let _ = close_tx.send(close_code.unwrap_or(Ended::Closed));
    drop(client);
    drop(reply);
    let _ = writer.await;
}

/// `/api/meetings/{meeting_id}/watch`: new transcript rows of one meeting.
pub async fn watch_meeting(state: Arc<AppState>, meeting_id: String, socket: WebSocket) {
    let (sink, mut stream) = socket.split();
    let (client, rx) = Client::new();
    let (close_tx, close_rx) = oneshot::channel();
    let writer = spawn_writer(sink, rx, close_rx);
    state.transcript_watcher.register(&meeting_id, client.clone());
    let mut close_code = None;
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Binary(_)) => {
                close_code = Some(Ended::Crashed);
                break;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    state.transcript_watcher.unregister(&meeting_id, &client);
    let _ = close_tx.send(close_code.unwrap_or(Ended::Closed));
    drop(client);
    let _ = writer.await;
}
