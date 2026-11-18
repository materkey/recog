//! Live status and the debug routes.

use std::path::Path;

use axum::http::StatusCode;
use recog_core::meeting_storage::{MeetingStorage, UnnamedLocal, local_participant_names};
use recog_core::pyjson::{self, Map, Value};
use recog_core::time::PyDateTime;

use super::{bad_request, body_model, check, fail, meta_of, not_found, segments_of, storage};
use crate::json;
use crate::meeting_catalog::MeetingCatalog;
use crate::request::Req;
use crate::responses::{self, Reply};
use crate::server::AppState;
use crate::transcript::{self, PyError, get};
use crate::util::{local_now_isoformat, py_round, unix_time};
use crate::validation::{self, Field, Ty, field};

fn read_json(path: &Path) -> Result<Value, PyError> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&raw).map_err(|e| pyjson::utf8_error_message(&raw, &e))?;
    pyjson::loads(&text.replace("\r\n", "\n").replace('\r', "\n")).map_err(|e| e.0)
}

pub(super) async fn live_status(state: &AppState) -> Reply {
    let storage = storage(state)?;
    let active = tokio::task::spawn_blocking(move || storage.list_active_meetings())
        .await
        .map_err(|_| responses::internal_error())?
        .map_err(fail)?;
    Ok(responses::json(&json::object([
        ("active", Value::Bool(!active.is_empty())),
        ("current_meeting", active.first().cloned().unwrap_or(Value::Null)),
        ("active_meetings", Value::Array(active)),
    ])))
}

fn number(value: Option<&Value>) -> Result<f64, ()> {
    match value {
        None => Ok(0.0),
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(x)) => Ok(*x),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(_) => Err(()),
    }
}

pub(super) fn pcm_diagnostics(state: &AppState) -> Reply {
    let path = &state.config.ktalk_debug_file;
    if !path.exists() {
        return Err(responses::detail(
            StatusCode::SERVICE_UNAVAILABLE,
            "ktalk_debug.json missing — is com.recog.live running?",
        ));
    }
    let debug = read_json(path).map_err(|e| fail(format!("read failed: {e}")))?;
    let Value::Object(fields) = &debug else { return Err(responses::internal_error()) };
    let age = unix_time() - number(fields.get("timestamp")).map_err(|()| responses::internal_error())?;
    let empty = Value::Object(Map::new());
    let field = |name: &str, key: &str, default: Value| -> Result<Value, axum::response::Response> {
        let section = fields.get(name).unwrap_or(&empty);
        Ok(get(section, key).map_err(|_| responses::internal_error())?.cloned().unwrap_or(default))
    };
    let pcm = fields
        .get("pcm")
        .filter(|v| v.truthy())
        .cloned()
        .unwrap_or_else(|| json::object([("transport_enabled", Value::Bool(false))]));
    let transport =
        fields.get("pcm_transport").filter(|v| v.truthy()).cloned().unwrap_or_else(|| Value::Object(Map::new()));
    Ok(responses::json(&json::object([
        ("state_age_s", Value::Float(py_round(age, 2))),
        ("stale", Value::Bool(age > 5.0)),
        ("meeting_id", field("meeting", "id", Value::Null)?),
        ("participants", field("conference", "participants", Value::Array(Vec::new()))?),
        ("pcm", pcm),
        ("pcm_transport", transport),
    ])))
}

/// The bridge's debug file as `/api/debug` shows it, and whether it is stale.
fn bridge_state(path: &Path) -> (Value, bool) {
    if !path.exists() {
        return (Value::Null, true);
    }
    match read_json(path) {
        Ok(value) => with_age(value),
        Err(e) => (json::object([("error", Value::from(e))]), true),
    }
}

/// The debug file's contents with their `age_seconds` when they carry a timestamp,
/// and whether that is more than five seconds ago.
fn with_age(mut value: Value) -> (Value, bool) {
    let timestamp = match value.as_object() {
        Some(map) if map.contains_key("timestamp") => number(map.get("timestamp")),
        _ => return (value, true),
    };
    let Ok(timestamp) = timestamp else {
        return (json::object([("error", Value::from("unsupported operand type(s) for -: 'float' and 'str'"))]), true);
    };
    let age = unix_time() - timestamp;
    if let Some(map) = value.as_object_mut() {
        map.insert("age_seconds", Value::Float(py_round(age, 1)));
    }
    (value, age > 5.0)
}

/// The active meetings and the number of all meetings; none when the storage fails.
async fn meeting_counts(state: &AppState) -> (Vec<Value>, usize) {
    let Ok(storage) = storage(state) else { return (Vec::new(), 0) };
    let catalog = state.catalog();
    tokio::task::spawn_blocking(move || active_and_total(&storage, catalog.as_ref()))
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn active_and_total(storage: &MeetingStorage, catalog: Option<&MeetingCatalog>) -> Option<(Vec<Value>, usize)> {
    let active = storage.list_active_meetings().ok()?;
    let all = match catalog {
        Some(catalog) => catalog.list_meetings().len(),
        None => storage.list_meetings(true, false).ok()?.len(),
    };
    Some((active, all))
}

pub(super) async fn debug(state: &AppState) -> Reply {
    let path = state.config.ktalk_debug_file.clone();
    let (ktalk_state, stale) = bridge_state(&path);
    let (active, all_count) = meeting_counts(state).await;
    let active_count = Value::Int(active.len() as i128);
    let meetings = json::object([
        ("active", Value::Array(active)),
        ("active_count", active_count),
        ("total_count", Value::Int(all_count as i128)),
    ]);
    Ok(responses::json(&json::object([
        ("timestamp", Value::Float(unix_time())),
        ("timestamp_iso", Value::from(local_now_isoformat())),
        (
            "ktalk_bridge",
            json::object([
                ("state", ktalk_state),
                ("stale", Value::Bool(stale)),
                ("debug_file", Value::from(path.to_string_lossy().into_owned())),
            ]),
        ),
        ("meetings", meetings),
        (
            "server",
            json::object([
                ("host", Value::from(state.config.host.as_str())),
                ("port", Value::Int(i128::from(state.config.port))),
            ]),
        ),
    ])))
}

pub(super) fn get_debug_ai_context(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let include = validation::validate_param(
        "query",
        "include_segments",
        &Ty::Bool,
        req.query("include_segments"),
        false,
        &mut errors,
    );
    let limit = validation::validate_param("query", "limit", &Ty::Int, req.query("limit"), false, &mut errors);
    check(&errors)?;
    let include = matches!(include, Some(Value::Bool(true)));
    let limit = match limit {
        Some(Value::Int(n)) => n,
        _ => 50,
    };
    let context = state.debug_ai.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let mut payload = Map::new();
    payload.insert("active", Value::Bool(!context.segments.is_empty()));
    payload.insert("meeting_id", json::opt_str(context.meeting_id.as_deref()));
    payload.insert("meeting_title", context.meeting_title.clone().unwrap_or(Value::Null));
    payload.insert("segment_count", Value::Int(context.segments.len() as i128));
    payload.insert("user_name", json::opt_str(context.user_name.as_deref()));
    payload.insert("set_at", json::opt_str(context.set_at.as_deref()));
    if include && !context.segments.is_empty() {
        let cap = usize::try_from(limit.clamp(1, 200)).unwrap_or(1);
        payload.insert("segments", Value::Array(context.segments.iter().take(cap).cloned().collect()));
    }
    Ok(responses::json(&Value::Object(payload)))
}

/// `_find_meeting_by_title`: the newest exact (case-insensitive) title match, else the
/// newest containing one.
fn find_meeting_by_title(meetings_dir: &Path, title: &str) -> Result<Option<Value>, PyError> {
    if title.is_empty() {
        return Ok(None);
    }
    let wanted = title.to_lowercase();
    let (mut best, mut fallback) = (Pick::default(), Pick::default());
    for entry in std::fs::read_dir(meetings_dir).map_err(|e| e.to_string())?.flatten() {
        let Some(meta) = meeting_meta(&entry.path()) else { continue };
        let Some((meeting_title, candidate)) = titled(&meta)? else { continue };
        if meeting_title == wanted {
            best.offer(candidate)?;
        } else if meeting_title.contains(&wanted) {
            fallback.offer(candidate)?;
        }
    }
    Ok(best.id.filter(Value::truthy).or(fallback.id))
}

/// The metadata of a meeting directory; `None` for anything else or unreadable.
fn meeting_meta(dir: &Path) -> Option<Value> {
    let meta_path = dir.join("meta.json");
    if !dir.is_dir() || !meta_path.exists() {
        return None;
    }
    read_json(&meta_path).ok()
}

/// A titled meeting's lower-cased, stripped title with its ID and creation time;
/// `None` without a title.
fn titled(meta: &Value) -> Result<Option<(String, Pick)>, PyError> {
    let title = get(meta, "title")?.filter(|v| v.truthy()).cloned().unwrap_or_else(|| Value::from(""));
    let Value::Str(title) = title else { return Err("'int' object has no attribute 'strip'".into()) };
    let title = recog_core::py::strip(&title).to_lowercase();
    if title.is_empty() {
        return Ok(None);
    }
    let created = transcript::parse_iso_ts(get(meta, "created_at")?);
    Ok(Some((title, Pick { id: get(meta, "id")?.cloned(), created })))
}

/// A meeting of a title search: the one picked so far, or a candidate.
#[derive(Default)]
struct Pick {
    id: Option<Value>,
    created: Option<PyDateTime>,
}

impl Pick {
    /// Takes the candidate while nothing dated is picked, or when it is newer than the
    /// picked one.
    fn offer(&mut self, candidate: Pick) -> Result<(), PyError> {
        let newer = match (candidate.created, self.created) {
            (_, None) => true,
            (Some(c), Some(t)) => c.minus(&t).map_err(|e| e.to_string())? > 0,
            (None, Some(_)) => false,
        };
        if newer {
            *self = candidate;
        }
        Ok(())
    }
}

fn debug_context_fields() -> [Field; 9] {
    let opt = |ty: Ty| Ty::Optional(Box::new(ty));
    [
        field("meeting_id", opt(Ty::Str), false),
        field("meeting_title", opt(Ty::Str), false),
        field("from_id", opt(Ty::Int), false),
        field("to_id", opt(Ty::Int), false),
        field("start_ts_display", opt(Ty::Str), false),
        field("window_seconds", Ty::Int, false),
        field("max_segments", Ty::Int, false),
        field("segments", opt(Ty::List(Box::new(Ty::AnyDict))), false),
        field("user_name", opt(Ty::Str), false),
    ]
}

/// A text field of the body, `None` when absent or null.
fn body_text(body: &Map, name: &str) -> Option<String> {
    body.get(name).filter(|v| !v.is_null()).map(Value::py_str)
}

/// An integer field of the body.
fn body_int(body: &Map, name: &str) -> Option<i128> {
    match body.get(name) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    }
}

/// What a debug context is set from: its segments, the meeting's title and local
/// participants' names.
struct Selected {
    segments: Vec<Value>,
    meeting_title: Value,
    local_names: Vec<Value>,
}

/// The given segments, or those chosen from a stored meeting (by ID or title).
fn selected_segments(
    state: &AppState,
    body: &Map,
    meeting_id: &mut Option<String>,
    max_segments: usize,
) -> Result<Selected, axum::response::Response> {
    match body.get("segments") {
        Some(Value::Array(rows)) if !rows.is_empty() => Ok(Selected {
            segments: rows.iter().take(max_segments).cloned().collect(),
            meeting_title: body.get("meeting_title").cloned().unwrap_or(Value::Null),
            local_names: Vec::new(),
        }),
        _ => stored_segments(state, body, meeting_id, max_segments),
    }
}

/// The segments of a stored meeting, found by its ID or else by its title.
fn stored_segments(
    state: &AppState,
    body: &Map,
    meeting_id: &mut Option<String>,
    max_segments: usize,
) -> Result<Selected, axum::response::Response> {
    if meeting_id.as_deref().is_none_or(str::is_empty)
        && let Some(title) = body_text(body, "meeting_title").filter(|t| !t.is_empty())
    {
        *meeting_id = find_meeting_by_title(&state.config.meetings_dir, &title)
            .map_err(|_| responses::internal_error())?
            .map(|v| v.py_str());
    }
    let Some(id) = meeting_id.clone().filter(|id| !id.is_empty()) else {
        return Err(bad_request("meeting_id or segments required"));
    };
    let found = stored_meeting(state, &id)?;
    let meta = meta_of(&found);
    let meeting_title = get(&meta, "title").map_err(|_| responses::internal_error())?.cloned().unwrap_or(Value::Null);
    let local_names = local_participant_names(&meta, UnnamedLocal::Empty).map_err(|_| responses::internal_error())?;
    let segments = select_segments(&state.config.meetings_dir, &id, &segments_of(&found), body, max_segments)
        .map_err(|_| responses::internal_error())?;
    Ok(Selected { segments, meeting_title, local_names })
}

/// `storage.get_meeting(id)`, the 404 without it.
fn stored_meeting(state: &AppState, id: &str) -> Result<Value, axum::response::Response> {
    let storage = storage(state)?;
    match storage.get_meeting(id) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err(not_found("Meeting not found")),
        Err(_) => Err(responses::internal_error()),
    }
}

/// The rows of a meeting between two IDs, from a start time, or within a window of
/// the last one.
fn select_segments(
    meetings_dir: &Path,
    id: &str,
    rows: &[Value],
    body: &Map,
    max_segments: usize,
) -> Result<Vec<Value>, PyError> {
    let (from_id, to_id) = (body_int(body, "from_id"), body_int(body, "to_id"));
    if from_id.is_some() || to_id.is_some() {
        return transcript::slice_segments_by_id(rows, from_id, to_id, max_segments);
    }
    if let Some(start) = body_text(body, "start_ts_display").filter(|s| !s.is_empty()) {
        return transcript::slice_segments_by_ts_display(rows, &start, max_segments);
    }
    let window_seconds = body_int(body, "window_seconds").unwrap_or(240).clamp(30, 900);
    transcript::select_recent_segments(meetings_dir, id, window_seconds, max_segments)
}

/// The user of a debug context: the given name, the meeting's named local
/// participants, the previous context's name, else `User`.
fn debug_user_name(given: Option<String>, local_names: Vec<Value>, previous: Option<&str>) -> Result<String, PyError> {
    if let Some(name) = given.filter(|n| !n.is_empty()) {
        return Ok(name);
    }
    if !local_names.is_empty() {
        let named: Vec<Value> = local_names.into_iter().filter(Value::truthy).collect();
        return transcript::join_names(&named);
    }
    Ok(previous.filter(|n| !n.is_empty()).unwrap_or("User").to_owned())
}

pub(super) fn set_debug_ai_context(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let body = body_model(req, &debug_context_fields(), false, &mut errors)?;
    check(&errors)?;
    let body = body.unwrap_or_default();
    let max_segments = usize::try_from(body_int(&body, "max_segments").unwrap_or(35).clamp(5, 200)).unwrap_or(5);
    let mut meeting_id = body_text(&body, "meeting_id");
    let Selected { segments, meeting_title, local_names } =
        selected_segments(state, &body, &mut meeting_id, max_segments)?;
    if segments.is_empty() {
        return Err(bad_request("No segments selected for debug context"));
    }
    let mut context = state.debug_ai.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let user_name = debug_user_name(body_text(&body, "user_name"), local_names, context.user_name.as_deref())
        .map_err(|_| responses::internal_error())?;
    *context = crate::server::DebugAiContext {
        segments: segments.clone(),
        user_name: Some(user_name.clone()),
        meeting_id: meeting_id.clone(),
        meeting_title: Some(meeting_title.clone()),
        set_at: Some(local_now_isoformat()),
    };
    Ok(responses::json(&json::object([
        ("active", Value::Bool(true)),
        ("meeting_id", json::opt_str(meeting_id.as_deref())),
        ("meeting_title", meeting_title),
        ("segment_count", Value::Int(segments.len() as i128)),
        ("user_name", Value::from(user_name)),
        ("set_at", json::opt_str(context.set_at.as_deref())),
    ])))
}

/// Coalesce checks across open tabs. Permission checks never ask for a grant.
pub(super) async fn readiness(state: &AppState) -> Reply {
    let mut cached = state.readiness.lock().await;
    if let Some((time, value)) = cached.as_ref()
        && time.elapsed() < std::time::Duration::from_secs(10)
    {
        return Ok(responses::json(value));
    }
    let root = state.config.repo_root.clone();
    let zoom = state.config.install.live.zoom_app;
    let rows = tokio::task::spawn_blocking(move || {
        use recog_install::{
            readiness,
            services::{Manager, NAMES, State},
            units::Backend,
        };
        let backend = if cfg!(target_os = "macos") { Backend::Launchd } else { Backend::Systemd };
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut rows = readiness::checks(&root, &path, backend);
        rows.extend(readiness::permission_checks(&root, backend, zoom));
        let check = std::process::Command::new(root.join("bin/recog")).arg("check").output();
        rows.push(readiness::Check {
            name: "Настройки".into(),
            required: true,
            ready: check.is_ok_and(|o| o.status.success()),
            hint: "recog doctor".into(),
        });
        if let Some(home) = std::env::home_dir() {
            let manager = Manager { backend, dir: backend.unit_dir(&home) };
            for name in NAMES {
                rows.push(readiness::Check {
                    name: name.into(),
                    required: matches!(name, "live" | "frontend"),
                    ready: matches!(manager.state(name), Ok(State::Running(_))),
                    hint: "recog service status".into(),
                });
            }
        }
        Value::Array(
            rows.into_iter()
                .map(|row| {
                    json::object([
                        (
                            "name",
                            Value::from(row.name.strip_prefix(&format!("{}/", root.display())).unwrap_or(&row.name)),
                        ),
                        ("ready", Value::Bool(row.ready)),
                        ("required", Value::Bool(row.required)),
                        ("hint", Value::from(if row.ready { String::new() } else { row.hint })),
                    ])
                })
                .collect(),
        )
    })
    .await
    .map_err(|_| responses::internal_error())?;
    let response = responses::json(&rows);
    *cached = Some((std::time::Instant::now(), rows));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meeting(dir: &Path, name: &str, meta: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("meta.json"), meta).unwrap();
    }

    fn found(dir: &Path, title: &str) -> Option<String> {
        find_meeting_by_title(dir, title).unwrap().map(|v| v.py_str())
    }

    #[test]
    fn exact_and_partial_titles_when_searched_then_the_exact_one_wins_whatever_its_age() {
        let dir = tempfile::tempdir().unwrap();
        meeting(dir.path(), "a", r#"{"id": "a", "title": " Обзор ", "created_at": "2026-01-01T10:00:00+00:00"}"#);
        meeting(dir.path(), "b", r#"{"id": "b", "title": "Обзор дизайна", "created_at": "2026-05-01T10:00:00+00:00"}"#);

        assert_eq!(found(dir.path(), "обзор").as_deref(), Some("a"));
        assert_eq!(found(dir.path(), "дизайн").as_deref(), Some("b"));
        assert_eq!(found(dir.path(), "бюджет"), None);
        assert_eq!(found(dir.path(), ""), None);
    }

    #[test]
    fn several_matches_when_searched_then_the_newest_with_a_time_wins() {
        let dir = tempfile::tempdir().unwrap();
        meeting(dir.path(), "old", r#"{"id": "old", "title": "Sync", "created_at": "2026-01-01T10:00:00+00:00"}"#);
        meeting(dir.path(), "new", r#"{"id": "new", "title": "sync", "created_at": "2026-03-01T10:00:00+00:00"}"#);
        meeting(dir.path(), "undated", r#"{"id": "undated", "title": "SYNC", "created_at": "soon"}"#);
        meeting(
            dir.path(),
            "p-old",
            r#"{"id": "p-old", "title": "Sync up", "created_at": "2026-01-01T10:00:00+00:00"}"#,
        );
        meeting(
            dir.path(),
            "p-new",
            r#"{"id": "p-new", "title": "Re sync", "created_at": "2026-02-01T10:00:00+00:00"}"#,
        );

        assert_eq!(found(dir.path(), "SYNC").as_deref(), Some("new"));
        assert_eq!(found(dir.path(), "sync u").as_deref(), Some("p-old"));
    }

    #[test]
    fn only_undated_matches_when_searched_then_one_of_them_is_found() {
        let dir = tempfile::tempdir().unwrap();
        meeting(dir.path(), "x", r#"{"id": "x", "title": "Sync"}"#);

        assert_eq!(found(dir.path(), "sync").as_deref(), Some("x"));
    }

    #[test]
    fn exact_match_without_an_id_when_searched_then_the_partial_one_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        meeting(dir.path(), "a", r#"{"title": "Sync", "created_at": "2026-01-01T10:00:00+00:00"}"#);
        meeting(dir.path(), "b", r#"{"id": "b", "title": "Sync two", "created_at": "2026-01-01T10:00:00+00:00"}"#);

        assert_eq!(found(dir.path(), "sync").as_deref(), Some("b"));
    }

    #[test]
    fn entries_that_are_not_meetings_when_searched_then_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.path().join("no-meta")).unwrap();
        meeting(dir.path(), "broken", "{broken");
        meeting(dir.path(), "untitled", r#"{"id": "untitled", "title": null}"#);
        meeting(dir.path(), "blank", r#"{"id": "blank", "title": "   "}"#);
        meeting(dir.path(), "ok", r#"{"id": "ok", "title": "Sync"}"#);

        assert_eq!(found(dir.path(), "sync").as_deref(), Some("ok"));
        assert_eq!(found(dir.path(), "yn").as_deref(), Some("ok"));
    }

    #[test]
    fn title_that_is_not_text_or_times_that_do_not_compare_when_searched_then_the_error() {
        let dir = tempfile::tempdir().unwrap();
        meeting(dir.path(), "n", r#"{"id": "n", "title": 5}"#);
        assert_eq!(find_meeting_by_title(dir.path(), "x").unwrap_err(), "'int' object has no attribute 'strip'");

        let dir = tempfile::tempdir().unwrap();
        meeting(dir.path(), "a", r#"{"id": "a", "title": "Sync", "created_at": "2026-01-01T10:00:00"}"#);
        meeting(dir.path(), "b", r#"{"id": "b", "title": "Sync", "created_at": "2026-01-01T10:00:00+00:00"}"#);
        assert!(find_meeting_by_title(dir.path(), "sync").is_err());
        assert!(find_meeting_by_title(&dir.path().join("absent"), "sync").is_err());
    }
}
