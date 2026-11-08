//! The four Copilot tools: their names, descriptions and input schemas as FastMCP
//! publishes them, and what each does with the bridge link.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use recog_core::privacy::PrivacyError;
use recog_core::pyjson::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::context::{COPILOT_POLICY_FIELDS, USER_CONTEXT_FIELDS, build_copilot_context};
use super::link::{BridgeLink, result};
use super::pydantic::{Field, Kind, validate_arguments};

/// How long `get_recent_transcript` waits for the bridge's snapshot.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest a single long-poll may hold its request open.
pub const MAX_AWAIT_SECONDS: i128 = 300;

/// A tool as `tools/list` shows it.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// The `inputSchema` FastMCP derives from the signature, as JSON text.
    pub input_schema: &'static str,
    pub fields: &'static [Field],
}

const fn field(name: &'static str, kind: Kind, optional: bool, default: Option<&'static str>) -> Field {
    Field { name, kind, optional, default }
}

pub const TOOLS: [ToolSpec; 4] = [
    ToolSpec {
        name: "send_copilot_message",
        description: "Send a Copilot message into the active KTalk meeting Copilot tab. Use kind='reply' for \
                      answers to a user request, kind='proactive' for unsolicited notes (the extension UI filters \
                      proactive messages through a 60 s cooldown and the user's mute toggle — only \
                      kind='proactive' triggers that path), or kind='manual' for ad-hoc messages.",
        input_schema: r#"{"properties":{"kind":{"title":"Kind","type":"string"},"text":{"title":"Text","type":"string"},"request_id":{"anyOf":[{"type":"string"},{"type":"null"}],"default":null,"title":"Request Id"},"meeting_id":{"anyOf":[{"type":"string"},{"type":"null"}],"default":null,"title":"Meeting Id"}},"required":["kind","text"],"title":"send_copilot_messageArguments","type":"object"}"#,
        fields: &[
            field("kind", Kind::Str, false, None),
            field("text", Kind::Str, false, None),
            field("request_id", Kind::Str, true, Some("null")),
            field("meeting_id", Kind::Str, true, Some("null")),
        ],
    },
    ToolSpec {
        name: "set_copilot_status",
        description: "Update the status indicator for a previously-received user request. Common statuses: \
                      'queued', 'working', 'done', 'error'.",
        input_schema: r#"{"properties":{"request_id":{"title":"Request Id","type":"string"},"status":{"title":"Status","type":"string"},"message":{"default":"","title":"Message","type":"string"}},"required":["request_id","status"],"title":"set_copilot_statusArguments","type":"object"}"#,
        fields: &[
            field("request_id", Kind::Str, false, None),
            field("status", Kind::Str, false, None),
            field("message", Kind::Str, false, Some("\"\"")),
        ],
    },
    ToolSpec {
        name: "get_recent_transcript",
        description: "Snapshot the recent transcript buffer for the active KTalk meeting. Returns segments newer \
                      than `window_seconds` (default 180) and, if `since_segment_id` is provided, only segments \
                      with a strictly larger id. Set `include_refined=False` to skip tier-2 refined segments. The \
                      bridge filters by the current meeting_id, so segments from a meeting that already ended are \
                      never returned.",
        input_schema: r#"{"properties":{"window_seconds":{"default":180,"title":"Window Seconds","type":"integer"},"since_segment_id":{"anyOf":[{"type":"integer"},{"type":"null"}],"default":null,"title":"Since Segment Id"},"include_refined":{"default":true,"title":"Include Refined","type":"boolean"}},"title":"get_recent_transcriptArguments","type":"object"}"#,
        fields: &[
            field("window_seconds", Kind::Int, false, Some("180")),
            field("since_segment_id", Kind::Int, true, Some("null")),
            field("include_refined", Kind::Bool, false, Some("true")),
        ],
    },
    ToolSpec {
        name: "await_copilot_event",
        description: "Block until the next bridge event arrives or the timeout elapses. Returns {'ok': True, \
                      'event': <dict>} on success or {'ok': True, 'event': null} on timeout. Event types: \
                      'copilot_user_message' (the user typed in the Copilot tab — reply with send_copilot_message \
                      kind='reply' carrying the original request_id), 'user_profile_updated' (replace local \
                      identity and aliases; no reply), 'copilot_policy_updated' (replace user behavior policy; no \
                      reply), 'transcript_tick' (new transcript segments — apply current editable Copilot \
                      instructions), 'meeting_started' / 'meeting_ended' (lifecycle markers; reset internal \
                      proactive state and exit the loop after meeting_ended). Call this in a loop while a meeting \
                      is active. The queue is bounded at 50 events; oldest events are dropped on overflow.",
        input_schema: r#"{"properties":{"timeout_seconds":{"default":60,"title":"Timeout Seconds","type":"integer"}},"title":"await_copilot_eventArguments","type":"object"}"#,
        fields: &[field("timeout_seconds", Kind::Int, false, Some("60"))],
    },
];

/// How a call ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The tool's dict, shown as pydantic JSON with `isError: false`.
    Value(Map),
    /// FastMCP's `Error executing tool <name>: <message>` with `isError: true`.
    Error(String),
    /// `notifications/cancelled` (or the session's end) stopped the call.
    Cancelled,
}

/// A short random id: `uuid.uuid4().hex[:12]`.
pub fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_owned()
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Runs tool `name` with the raw `arguments` of the call.
pub async fn call(link: &BridgeLink, name: &str, arguments: &Map, cancel: &CancellationToken) -> Outcome {
    let Some(spec) = TOOLS.iter().find(|t| t.name == name) else {
        return Outcome::Error(format!("Unknown tool: {name}"));
    };
    let args = match validate_arguments(&format!("{name}Arguments"), spec.fields, arguments) {
        Ok(args) => args,
        Err(message) => return Outcome::Error(format!("Error executing tool {name}: {message}")),
    };
    let privacy_error = |e: PrivacyError| Outcome::Error(format!("Error executing tool {name}: {e}"));
    match name {
        "send_copilot_message" => Outcome::Value(send_copilot_message(link, &args).await),
        "set_copilot_status" => Outcome::Value(set_copilot_status(link, &args).await),
        "get_recent_transcript" => get_recent_transcript(link, &args, cancel).await.unwrap_or_else(privacy_error),
        _ => await_copilot_event(link, &args, cancel).await.unwrap_or_else(privacy_error),
    }
}

async fn send_copilot_message(link: &BridgeLink, args: &Map) -> Map {
    let (Some(kind), Some(text)) = (args.get("kind"), args.get("text")) else { unreachable!("validated") };
    if !kind.truthy() {
        return result(false, Some("kind must be a non-empty string"));
    }
    if !text.truthy() {
        return result(false, Some("text must be a non-empty string"));
    }
    // The bridge requires a non-empty request id on every reply frame; proactive and
    // manual sends have no user request to correlate, so they get a synthetic one.
    let request_id =
        non_empty_str(args.get("request_id")).map_or_else(|| format!("proactive_{}", short_id()), str::to_owned);
    let mut frame = Map::new();
    frame.insert("type", Value::from("copilot_channel_reply"));
    frame.insert("request_id", Value::from(request_id));
    frame.insert("kind", kind.clone());
    frame.insert("text", text.clone());
    frame.insert("meeting_id", args.get("meeting_id").cloned().unwrap_or(Value::Null));
    frame.insert("created_at", Value::Float(now_ms()));
    link.send(&frame).await
}

async fn set_copilot_status(link: &BridgeLink, args: &Map) -> Map {
    let (Some(request_id), Some(status)) = (args.get("request_id"), args.get("status")) else {
        unreachable!("validated")
    };
    if !request_id.truthy() {
        return result(false, Some("request_id must be a non-empty string"));
    }
    if !status.truthy() {
        return result(false, Some("status must be a non-empty string"));
    }
    let mut frame = Map::new();
    frame.insert("type", Value::from("copilot_channel_status"));
    frame.insert("request_id", request_id.clone());
    frame.insert("status", status.clone());
    frame.insert("message", args.get("message").cloned().unwrap_or(Value::from("")));
    link.send(&frame).await
}

/// Unregisters the pending request however the call ends — a cancelled call must
/// not leave its entry behind while the socket stays up.
struct PendingGuard<'a> {
    link: &'a BridgeLink,
    request_id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.link.unregister_pending(&self.request_id);
    }
}

async fn get_recent_transcript(
    link: &BridgeLink,
    args: &Map,
    cancel: &CancellationToken,
) -> Result<Outcome, PrivacyError> {
    if !link.is_connected() {
        return Ok(Outcome::Value(result(false, Some("bridge not connected"))));
    }
    let request_id = format!("snapshot_{}", short_id());
    let answer = link.register_pending(&request_id);
    let _guard = PendingGuard { link, request_id: request_id.clone() };
    let mut frame = Map::new();
    frame.insert("type", Value::from("get_recent_transcript_request"));
    frame.insert("request_id", Value::from(request_id.as_str()));
    for key in ["window_seconds", "since_segment_id", "include_refined"] {
        frame.insert(key, args.get(key).cloned().unwrap_or(Value::Null));
    }
    let sent = tokio::select! {
        sent = link.send(&frame) => sent,
        () = cancel.cancelled() => return Ok(Outcome::Cancelled),
    };
    if sent.get("ok") != Some(&Value::Bool(true)) {
        return Ok(Outcome::Value(sent));
    }
    let response = tokio::select! {
        answer = tokio::time::timeout(SNAPSHOT_TIMEOUT, answer) => match answer {
            Err(_) => return Ok(Outcome::Value(result(false, Some("timeout waiting for transcript snapshot")))),
            Ok(Ok(Ok(response))) => response,
            Ok(Ok(Err(dropped))) => return Ok(Outcome::Value(result(false, Some(&dropped.to_string())))),
            // The waiter was dropped without an answer: the link lost the socket.
            Ok(Err(_)) => return Ok(Outcome::Value(result(false, Some("bridge connection dropped")))),
        },
        () = cancel.cancelled() => return Ok(Outcome::Cancelled),
    };
    let response = link.apply_privacy(&response)?;
    let context = build_copilot_context(&response);
    let mut out = result(true, None);
    out.insert("segments", response.get("segments").cloned().unwrap_or(Value::Array(Vec::new())));
    let keys = USER_CONTEXT_FIELDS.iter().map(|(key, _)| *key).chain(COPILOT_POLICY_FIELDS).chain(["privacy_mode"]);
    for key in keys {
        if let Some(value) = response.get(key) {
            out.insert(key, value.clone());
        }
    }
    if !context.is_empty() {
        out.insert("user_context", Value::from(context));
    }
    Ok(Outcome::Value(out))
}

async fn await_copilot_event(
    link: &BridgeLink,
    args: &Map,
    cancel: &CancellationToken,
) -> Result<Outcome, PrivacyError> {
    let timeout = match args.get("timeout_seconds") {
        Some(Value::Int(n)) if *n > 0 => (*n).min(MAX_AWAIT_SECONDS),
        _ => return Ok(Outcome::Value(result(false, Some("timeout_seconds must be a positive int")))),
    };
    let wait = Duration::from_secs(u64::try_from(timeout).unwrap_or(1));
    let event = tokio::select! {
        event = tokio::time::timeout(wait, link.events.get()) => event,
        () = cancel.cancelled() => return Ok(Outcome::Cancelled),
    };
    let Ok(event) = event else {
        let mut out = result(true, None);
        out.insert("event", Value::Null);
        return Ok(Outcome::Value(out));
    };
    let event = link.apply_privacy(&event)?;
    let context = build_copilot_context(&event);
    let mut out = result(true, None);
    out.insert("event", Value::Object(event));
    if !context.is_empty() {
        out.insert("user_context", Value::from(context));
    }
    Ok(Outcome::Value(out))
}
