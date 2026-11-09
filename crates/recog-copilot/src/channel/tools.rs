//! The four Copilot tools: their names, descriptions and input schemas as FastMCP
//! publishes them, and what each does with the bridge link.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use recog_core::privacy::PrivacyError;
use recog_core::protocol;
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
    frame.insert("type", Value::from(protocol::COPILOT_CHANNEL_REPLY));
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
    frame.insert("type", Value::from(protocol::COPILOT_CHANNEL_STATUS));
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
    let frame = snapshot_request(&request_id, args);
    let response = match exchange_snapshot(link, &frame, answer, cancel).await {
        Ok(response) => response,
        Err(outcome) => return Ok(outcome),
    };
    Ok(Outcome::Value(snapshot_result(&link.apply_privacy(&response)?)))
}

/// The `get_recent_transcript_request` frame: the call's window arguments, None
/// for those left out.
fn snapshot_request(request_id: &str, args: &Map) -> Map {
    let mut frame = Map::new();
    frame.insert("type", Value::from(protocol::GET_RECENT_TRANSCRIPT_REQUEST));
    frame.insert("request_id", Value::from(request_id));
    for key in ["window_seconds", "since_segment_id", "include_refined"] {
        frame.insert(key, args.get(key).cloned().unwrap_or(Value::Null));
    }
    frame
}

/// Sends the request and waits for the bridge's snapshot; the outcome to answer
/// instead when the send fails, the wait times out or the call is cancelled.
async fn exchange_snapshot<E: std::fmt::Display>(
    link: &BridgeLink,
    frame: &Map,
    answer: tokio::sync::oneshot::Receiver<Result<Map, E>>,
    cancel: &CancellationToken,
) -> Result<Map, Outcome> {
    let sent = tokio::select! {
        sent = link.send(frame) => sent,
        () = cancel.cancelled() => return Err(Outcome::Cancelled),
    };
    if sent.get("ok") != Some(&Value::Bool(true)) {
        return Err(Outcome::Value(sent));
    }
    tokio::select! {
        answer = tokio::time::timeout(SNAPSHOT_TIMEOUT, answer) => snapshot_answer(answer).map_err(Outcome::Value),
        () = cancel.cancelled() => Err(Outcome::Cancelled),
    }
}

/// The snapshot the waiter got, or the failure dict for a timeout or a lost socket.
fn snapshot_answer<E: std::fmt::Display>(
    answer: Result<Result<Result<Map, E>, tokio::sync::oneshot::error::RecvError>, tokio::time::error::Elapsed>,
) -> Result<Map, Map> {
    match answer {
        Err(_) => Err(result(false, Some("timeout waiting for transcript snapshot"))),
        Ok(Ok(Ok(response))) => Ok(response),
        Ok(Ok(Err(dropped))) => Err(result(false, Some(&dropped.to_string()))),
        // The waiter was dropped without an answer: the link lost the socket.
        Ok(Err(_)) => Err(result(false, Some("bridge connection dropped"))),
    }
}

/// The tool's dict for a delivered snapshot: its segments, the identity and policy
/// fields it carries, and the rendered user context when there is one.
fn snapshot_result(response: &Map) -> Map {
    let context = build_copilot_context(response);
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
    out
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

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_that_never_comes_when_the_wait_times_out_then_the_timeout_failure() {
        // given
        let (_tx, rx) = oneshot::channel::<Result<Map, String>>();
        let answer = tokio::time::timeout(SNAPSHOT_TIMEOUT, rx).await;
        // when
        let outcome = snapshot_answer(answer);
        // then
        assert_eq!(outcome, Err(result(false, Some("timeout waiting for transcript snapshot"))));
    }

    #[tokio::test]
    async fn a_waiter_dropped_without_an_answer_when_awaited_then_the_connection_dropped_failure() {
        // given
        let (tx, rx) = oneshot::channel::<Result<Map, String>>();
        drop(tx);
        let answer = tokio::time::timeout(SNAPSHOT_TIMEOUT, rx).await;
        // when
        let outcome = snapshot_answer(answer);
        // then
        assert_eq!(outcome, Err(result(false, Some("bridge connection dropped"))));
    }

    #[test]
    fn window_arguments_left_out_when_the_request_is_built_then_they_are_sent_as_none() {
        // given
        let mut args = Map::new();
        args.insert("window_seconds", Value::Int(30));
        // when
        let frame = snapshot_request("snapshot_1", &args);
        // then
        let keys: Vec<&str> = frame.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["type", "request_id", "window_seconds", "since_segment_id", "include_refined"]);
        assert_eq!(frame.get("type"), Some(&Value::from(protocol::GET_RECENT_TRANSCRIPT_REQUEST)));
        assert_eq!(frame.get("window_seconds"), Some(&Value::Int(30)));
        assert_eq!(frame.get("since_segment_id"), Some(&Value::Null));
    }
}
