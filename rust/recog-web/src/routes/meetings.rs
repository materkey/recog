//! The meeting list, search, one meeting and its deletion.

use axum::http::StatusCode;
use recog_core::meeting_storage::MeetingStorage;
use recog_core::pyjson::Value;

use super::{bad_request, fail, meeting, meta_of, not_found, segments_of, storage};
use crate::json;
use crate::request::Req;
use crate::responses::{self, Reply};
use crate::server::AppState;
use crate::transcript::{PyError, get};
use crate::util::invalid_id;

pub(super) async fn list_meetings(state: &AppState) -> Reply {
    let meetings = match state.catalog() {
        Some(catalog) => catalog.list_meetings(),
        None => {
            let storage = storage(state)?;
            tokio::task::spawn_blocking(move || storage.list_meetings(true, false))
                .await
                .map_err(|_| responses::internal_error())?
                .map_err(fail)?
        }
    };
    let rows = meetings
        .into_iter()
        .map(|meeting| {
            let label = match meeting.as_object().and_then(|m| m.get("source")) {
                Some(Value::Str(source)) => state.config.source_label(source).map(str::to_owned),
                _ => None,
            };
            match (label, meeting) {
                (Some(label), Value::Object(mut map)) => {
                    map.insert("source_label", Value::from(label));
                    Value::Object(map)
                }
                (_, meeting) => meeting,
            }
        })
        .collect();
    Ok(responses::json(&Value::Array(rows)))
}

/// `str.lower()` of a Python text value; others raise `AttributeError`.
fn lower(value: &Value) -> Result<String, PyError> {
    match value {
        Value::Str(s) => Ok(s.to_lowercase()),
        other => Err(format!("'{}' object has no attribute 'lower'", other.type_name())),
    }
}

fn search_meetings(storage: &MeetingStorage, q: &str) -> Result<Vec<Value>, PyError> {
    let meetings = storage.list_meetings(true, false).map_err(|e| e.to_string())?;
    let query = q.to_lowercase();
    let mut results = Vec::new();
    for info in &meetings {
        let id = get(info, "id")?.ok_or("'id'")?.py_str();
        let Some(meeting) = storage.get_meeting(&id).map_err(|e| e.to_string())? else { continue };
        let mut matches = Vec::new();
        for segment in segments_of(&meeting) {
            let text = get(&segment, "text")?.cloned().unwrap_or_else(|| Value::from(""));
            if lower(&text)?.contains(&query) {
                matches.push(json::object([
                    ("ts_display", get(&segment, "ts_display")?.cloned().unwrap_or(Value::Null)),
                    ("speaker", get(&segment, "speaker")?.cloned().unwrap_or(Value::Null)),
                    ("text", text),
                ]));
            }
        }
        if !matches.is_empty() {
            matches.truncate(5);
            let title = get(info, "title")?.filter(|v| v.truthy()).cloned().unwrap_or_else(|| Value::from(id.as_str()));
            results.push(json::object([
                ("meeting_id", get(info, "id")?.cloned().unwrap_or(Value::Null)),
                ("title", title),
                ("created_at", get(info, "created_at")?.cloned().unwrap_or(Value::Null)),
                ("source", get(info, "source")?.cloned().unwrap_or(Value::Null)),
                ("matches", Value::Array(matches)),
            ]));
        }
    }
    Ok(results)
}

pub(super) async fn search(state: &AppState, req: &Req) -> Reply {
    let q = req.query("q").unwrap_or_default().to_owned();
    if q.chars().count() < 2 {
        return Ok(responses::json(&Value::Array(Vec::new())));
    }
    let storage = storage(state)?;
    let results = tokio::task::spawn_blocking(move || search_meetings(&storage, &q))
        .await
        .map_err(|_| responses::internal_error())?
        .map_err(fail)?;
    Ok(responses::json(&Value::Array(results)))
}

pub(super) fn get_meeting(state: &AppState, req: &Req, meta: bool) -> Reply {
    let storage = storage(state)?;
    let meeting = meeting(&storage, req.param("meeting_id"))?;
    let value = if meta { meta_of(&meeting) } else { Value::Array(segments_of(&meeting)) };
    Ok(responses::json(&value))
}

pub(super) async fn delete_meeting(state: &AppState, req: &Req) -> Reply {
    let meeting_id = req.param("meeting_id").to_owned();
    if invalid_id(&meeting_id) {
        return Err(bad_request("Invalid meeting ID"));
    }
    let storage = storage(state)?;
    let deleted = match storage.delete_meeting(&meeting_id) {
        Ok(deleted) => deleted,
        Err(e) if e.is_value_error() => return Err(responses::detail(StatusCode::CONFLICT, &e.to_string())),
        Err(e) => return Err(fail(e)),
    };
    if !deleted {
        return Err(not_found("Meeting not found"));
    }
    if state.live_watcher.current_meeting_id().as_deref() == Some(meeting_id.as_str()) {
        state.live_watcher.set_current_meeting(None, None);
    }
    if let Some(catalog) = state.catalog() {
        catalog.refresh(None).await;
    }
    Ok(responses::json(&json::object([("success", Value::Bool(true))])))
}
