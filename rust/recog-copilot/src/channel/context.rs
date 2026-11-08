//! What the Copilot reads: the literal profile and policy block (`build_copilot_context`)
//! and the `(content, meta)` of a channel push (`_format_channel_event`).
//!
//! The block is the one representation of the user's identity and editable
//! instructions on every surface — channel pushes, long-poll results and transcript
//! snapshots — so that the copilot skill applies the same text whichever way an event
//! reached it. Values are written with `json.dumps(..., ensure_ascii=False)`, so a name
//! or a policy holding braces, quotes or `$` arrives literally.

use recog_core::pyjson::{self, Map, Value};

/// Event keys of the profile, with the labels the block gives them.
pub const USER_CONTEXT_FIELDS: [(&str, &str); 4] = [
    ("local_user_name", "name"),
    ("local_user_aliases", "aliases"),
    ("local_user_profile_revision", "revision"),
    ("local_user_identity_known", "known"),
];

/// Event keys of the editable Copilot instructions.
pub const COPILOT_POLICY_FIELDS: [&str; 2] = ["copilot_instructions", "copilot_policy_revision"];

/// `build_copilot_context(snapshot)`: identity data and editable instructions as
/// separate literal blocks, empty when the snapshot carries neither.
pub fn build_copilot_context(snapshot: &Map) -> String {
    let mut blocks = Vec::new();
    if snapshot.get("privacy_mode").is_some_and(Value::truthy) {
        blocks.push(
            "Privacy mode: use speaker_N labels instead of real names. \
             The user profile is masked; transcript text is unchanged."
                .to_owned(),
        );
    }
    let profile: Map = USER_CONTEXT_FIELDS
        .iter()
        .filter_map(|(key, label)| snapshot.get(key).filter(|v| !v.is_null()).map(|v| ((*label).to_owned(), v.clone())))
        .collect();
    if !profile.is_empty() {
        blocks.push(format!("Current user profile (data):\n{}", pyjson::dumps(&Value::Object(profile), None)));
    }
    if snapshot.contains_key("copilot_instructions") {
        let policy: Map = COPILOT_POLICY_FIELDS
            .iter()
            .filter_map(|key| snapshot.get(key).map(|v| ((*key).to_owned(), v.clone())))
            .collect();
        blocks.push(format!(
            "Current Copilot instructions (user configuration):\n{}",
            pyjson::dumps(&Value::Object(policy), None)
        ));
    }
    blocks.join("\n\n")
}

/// `len()` of a value, or Python's `TypeError` text.
fn py_len(value: &Value) -> Result<usize, String> {
    match value {
        Value::Str(s) => Ok(s.chars().count()),
        Value::Array(items) => Ok(items.len()),
        Value::Object(map) => Ok(map.len()),
        other => Err(format!("TypeError: object of type '{}' has no len()", other.type_name())),
    }
}

/// `event.get(key) or default`, as `str()`.
fn get_or(event: &Map, key: &str, default: &str) -> String {
    event.get(key).filter(|v| v.truthy()).map_or_else(|| default.to_owned(), Value::py_str)
}

/// `_format_channel_event(event)`: the body of the `<channel>` tag Claude sees and its
/// attributes. Meta keys must be identifiers and values strings, per the Claude Code
/// channels reference; a non-string value is written as JSON.
///
/// # Errors
/// The `TypeError` Python raises for a `new_segments` without a length, as
/// `"TypeError: <message>"`; the push is then skipped and logged.
pub fn format_channel_event(event: &Map) -> Result<(String, Map), String> {
    let msg_type = get_or(event, "type", "unknown");
    let mut content = match msg_type.as_str() {
        "copilot_user_message" => format!(
            "User typed in the Copilot tab. Reply with send_copilot_message kind='reply' carrying the original \
             request_id.\n\nText: {}",
            event.get("text").map_or_else(String::new, Value::py_str)
        ),
        "transcript_tick" => {
            let new_segments =
                event.get("new_segments").filter(|v| v.truthy()).cloned().unwrap_or(Value::Array(vec![]));
            format!(
                "Transcript tick: {} new segment(s). Apply the current editable Copilot instructions.\n\nRecent \
                 context:\n{}",
                py_len(&new_segments)?,
                get_or(event, "recent_context", "")
            )
        }
        "meeting_started" => {
            format!(
                "Meeting started: {}. Reset internal proactive state.",
                get_or(event, "meeting_title", "(untitled)")
            )
        }
        "user_profile_updated" => "Local user profile updated. Replace the previous profile with the current user \
                                   context. Do not send a reply for this event."
            .to_owned(),
        "copilot_policy_updated" => "Copilot instructions updated. Replace the previous policy with the current \
                                     user configuration, including cleared instructions. Apply it from this event \
                                     onward. Do not send a reply for this event."
            .to_owned(),
        "meeting_ended" => "Meeting ended. Stop polling and exit the copilot loop.".to_owned(),
        _ => pyjson::dumps(&Value::Object(event.clone()), None),
    };
    let context = build_copilot_context(event);
    if !context.is_empty() {
        content = format!("{context}\n\n{content}");
    }

    let mut meta = Map::new();
    meta.insert("event_type", Value::from(msg_type.as_str()));
    // Transport metadata for clients and sessions started before the rendered context
    // was introduced; new instructions use the context block.
    let keys = ["request_id", "meeting_id", "meeting_title"]
        .into_iter()
        .chain(USER_CONTEXT_FIELDS.iter().map(|(key, _)| *key))
        .chain(["copilot_policy_revision", "privacy_mode"]);
    for key in keys {
        let Some(value) = event.get(key).filter(|v| !v.is_null()) else { continue };
        let text = match value {
            Value::Str(s) => s.clone(),
            other => pyjson::dumps(other, None),
        };
        meta.insert(key, Value::from(text));
    }
    Ok((content, meta))
}
