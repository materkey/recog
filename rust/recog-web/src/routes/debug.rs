//! Live status and the debug routes.

use std::path::Path;

use axum::http::StatusCode;
use recog_core::meeting_storage::{UnnamedLocal, local_participant_names};
use recog_core::pyjson::{self, Map, Value};

use super::{bad_request, body_model, check, fail, meta_of, not_found, segments_of, storage};
use crate::json;
use crate::request::Req;
use crate::responses::{self, Reply};
use crate::server::AppState;
use crate::transcript::{self, PyError, get};
use crate::util::{local_now_isoformat, py_round, unix_time};
use crate::validation::{self, Ty, field};

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

pub(super) async fn debug(state: &AppState) -> Reply {
    let path = state.config.ktalk_debug_file.clone();
    let mut ktalk_state = Value::Null;
    let mut stale = true;
    if path.exists() {
        match read_json(&path) {
            Ok(mut value) => {
                if let Value::Object(map) = &mut value
                    && map.contains_key("timestamp")
                {
                    match number(map.get("timestamp")) {
                        Ok(timestamp) => {
                            let age = unix_time() - timestamp;
                            stale = age > 5.0;
                            map.insert("age_seconds", Value::Float(py_round(age, 1)));
                        }
                        Err(()) => {
                            value = json::object([(
                                "error",
                                Value::from("unsupported operand type(s) for -: 'float' and 'str'"),
                            )]);
                        }
                    }
                }
                ktalk_state = value;
            }
            Err(e) => ktalk_state = json::object([("error", Value::from(e))]),
        }
    }
    let (active, all_count) = match storage(state) {
        Ok(storage) => {
            let catalog = state.catalog();
            tokio::task::spawn_blocking(move || {
                let active = storage.list_active_meetings().ok()?;
                let all = match catalog {
                    Some(catalog) => catalog.list_meetings().len(),
                    None => storage.list_meetings(true, false).ok()?.len(),
                };
                Some((active, all))
            })
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
        }
        Err(_) => (Vec::new(), 0),
    };
    let counts = json::object([
        ("active_count", Value::Int(active.len() as i128)),
        ("total_count", Value::Int(all_count as i128)),
    ]);
    let mut meetings = json::object([("active", Value::Array(active))]);
    if let (Value::Object(meetings), Value::Object(counts)) = (&mut meetings, counts) {
        for (k, v) in counts.iter() {
            meetings.insert(k.clone(), v.clone());
        }
    }
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
    let mut best: (Option<Value>, Option<recog_core::time::PyDateTime>) = (None, None);
    let mut fallback: (Option<Value>, Option<recog_core::time::PyDateTime>) = (None, None);
    let entries = std::fs::read_dir(meetings_dir).map_err(|e| e.to_string())?;
    let newer = |created: Option<recog_core::time::PyDateTime>,
                 than: Option<recog_core::time::PyDateTime>|
     -> Result<bool, PyError> {
        match (created, than) {
            (_, None) => Ok(true),
            (Some(c), Some(t)) => Ok(c.minus(&t).map_err(|e| e.to_string())? > 0),
            (None, Some(_)) => Ok(false),
        }
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let meta_path = dir.join("meta.json");
        if !meta_path.exists() {
            continue;
        }
        let Ok(meta) = read_json(&meta_path) else { continue };
        let title_value = get(&meta, "title")?.filter(|v| v.truthy()).cloned().unwrap_or_else(|| Value::from(""));
        let Value::Str(meeting_title) = title_value else { return Err("'int' object has no attribute 'strip'".into()) };
        let meeting_title = recog_core::py::strip(&meeting_title).to_lowercase();
        if meeting_title.is_empty() {
            continue;
        }
        let created = transcript::parse_iso_ts(get(&meta, "created_at")?);
        let id = get(&meta, "id")?.cloned();
        if meeting_title == wanted {
            if best.1.is_none() || newer(created, best.1)? && created.is_some() {
                best = (id, created);
            }
        } else if meeting_title.contains(&wanted)
            && (fallback.1.is_none() || newer(created, fallback.1)? && created.is_some())
        {
            fallback = (id, created);
        }
    }
    Ok(best.0.filter(Value::truthy).or(fallback.0))
}

pub(super) fn set_debug_ai_context(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let opt = |ty: Ty| Ty::Optional(Box::new(ty));
    let fields = [
        field("meeting_id", opt(Ty::Str), false),
        field("meeting_title", opt(Ty::Str), false),
        field("from_id", opt(Ty::Int), false),
        field("to_id", opt(Ty::Int), false),
        field("start_ts_display", opt(Ty::Str), false),
        field("window_seconds", Ty::Int, false),
        field("max_segments", Ty::Int, false),
        field("segments", opt(Ty::List(Box::new(Ty::AnyDict))), false),
        field("user_name", opt(Ty::Str), false),
    ];
    let body = body_model(req, &fields, false, &mut errors)?;
    check(&errors)?;
    let body = body.unwrap_or_default();
    let text = |name: &str| body.get(name).filter(|v| !v.is_null()).map(Value::py_str);
    let int = |name: &str| match body.get(name) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    };
    let max_segments = usize::try_from(int("max_segments").unwrap_or(35).clamp(5, 200)).unwrap_or(5);
    let window_seconds = int("window_seconds").unwrap_or(240).clamp(30, 900);
    let mut meeting_id = text("meeting_id");
    let mut meeting_title = body.get("meeting_title").cloned().unwrap_or(Value::Null);
    let mut local_names: Vec<Value> = Vec::new();
    let given: Vec<Value> = match body.get("segments") {
        Some(Value::Array(rows)) => rows.clone(),
        _ => Vec::new(),
    };
    let segments = if given.is_empty() {
        if meeting_id.as_deref().is_none_or(str::is_empty)
            && let Some(title) = text("meeting_title").filter(|t| !t.is_empty())
        {
            meeting_id = find_meeting_by_title(&state.config.meetings_dir, &title)
                .map_err(|_| responses::internal_error())?
                .map(|v| v.py_str());
        }
        let Some(id) = meeting_id.clone().filter(|id| !id.is_empty()) else {
            return Err(bad_request("meeting_id or segments required"));
        };
        let storage = storage(state)?;
        let found = match storage.get_meeting(&id) {
            Ok(Some(found)) => found,
            Ok(None) => return Err(not_found("Meeting not found")),
            Err(_) => return Err(responses::internal_error()),
        };
        let meta = meta_of(&found);
        meeting_title = get(&meta, "title").map_err(|_| responses::internal_error())?.cloned().unwrap_or(Value::Null);
        local_names = local_participant_names(&meta, UnnamedLocal::Empty).map_err(|_| responses::internal_error())?;
        let rows = segments_of(&found);
        if int("from_id").is_some() || int("to_id").is_some() {
            transcript::slice_segments_by_id(&rows, int("from_id"), int("to_id"), max_segments)
                .map_err(|_| responses::internal_error())?
        } else if let Some(start) = text("start_ts_display").filter(|s| !s.is_empty()) {
            transcript::slice_segments_by_ts_display(&rows, &start, max_segments)
                .map_err(|_| responses::internal_error())?
        } else {
            transcript::select_recent_segments(&state.config.meetings_dir, &id, window_seconds, max_segments)
                .map_err(|_| responses::internal_error())?
        }
    } else {
        given.into_iter().take(max_segments).collect()
    };
    if segments.is_empty() {
        return Err(bad_request("No segments selected for debug context"));
    }
    let mut context = state.debug_ai.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let user_name = match text("user_name").filter(|n| !n.is_empty()) {
        Some(name) => name,
        None if !local_names.is_empty() => {
            let named: Vec<Value> = local_names.into_iter().filter(Value::truthy).collect();
            transcript::join_names(&named).map_err(|_| responses::internal_error())?
        }
        None => context.user_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| "User".to_owned()),
    };
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
