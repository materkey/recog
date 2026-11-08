//! The HTTP route handlers of `recog/web/server.py`.
//!
//! Each handler keeps Python's order of checks and its error mapping: validation
//! (422) before anything else, `HTTPException`s with their detail, `except Exception`
//! blocks as `500 {"detail": str(e)}`, and exceptions Python does not catch as the
//! plain `500 Internal Server Error`.

use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use recog_ai::AiError;
use recog_ai::increment::IncrementGenerator;
use recog_ai::meeting_chat::{self, ChatTurn};
use recog_ai::prompt_registry::PromptRegistry;
use recog_ai::template::variables;
use recog_ai::title;
use recog_core::meeting_storage::MeetingStorage;
use recog_core::privacy::PrivacySource as _;
use recog_core::pyjson::{self, Map, Value};
use recog_core::user_profile::ProfileError;

use crate::ai_provider::build_prompt;
use crate::json;
use crate::request::{BodyError, Req};
use crate::responses::{self, Reply};
use crate::router::Route;
use crate::server::{AppState, root_response};
use crate::transcript::{self, PyError, get};
use crate::util::{invalid_id, local_now_isoformat, py_round, unix_time, utc_now_isoformat};
use crate::validation::{self, ErrorItem, Field, Loc, Ty, field, validate_model};
use crate::{analysis, audio, desktop};

/// One route.
pub async fn handle(state: &Arc<AppState>, route: Route, req: Req) -> Reply {
    match route {
        Route::DesktopStatus => {
            let root = state.config.data_dir.join("desktop");
            let value = tokio::task::spawn_blocking(move || desktop::status(&root))
                .await
                .map_err(|_| responses::internal_error())?;
            Ok(responses::json(&value))
        }
        Route::GetAiSettings => get_ai_settings(state).await,
        Route::SaveAiModels => save_ai_models(state, &req).await,
        Route::SaveAiTask => save_ai_task(state, &req).await,
        Route::ResetAiTask => reset_ai_task(state, &req).await,
        Route::GetPrivacy => get_privacy(state),
        Route::SavePrivacy => save_privacy(state, &req),
        Route::GetUserProfile => {
            let snapshot = state.profile.snapshot().await.map_err(|_| read_profile_error())?;
            Ok(responses::json(&snapshot))
        }
        Route::SaveUserProfile => save_user_profile(state, &req).await,
        Route::ClearUserProfile => {
            let snapshot = state.profile.clear().await.map_err(|_| {
                responses::detail(StatusCode::INTERNAL_SERVER_ERROR, "Не удалось очистить профиль пользователя.")
            })?;
            Ok(responses::json(&snapshot))
        }
        Route::ListMeetings => list_meetings(state).await,
        Route::Search => search(state, &req).await,
        Route::GetMeeting | Route::GetTranscript => get_meeting(state, &req, route == Route::GetMeeting),
        Route::DeleteMeeting => delete_meeting(state, &req).await,
        Route::ListPrompts => {
            let prompts = PromptRegistry::new(&state.config.prompts_dir).load_all();
            let rows = prompts
                .iter()
                .map(|p| {
                    json::object([
                        ("id", Value::from(p.id.as_str())),
                        ("name", Value::from(p.name.as_str())),
                        ("filter", json::opt_str(p.filter.as_deref())),
                    ])
                })
                .collect();
            Ok(responses::json(&Value::Array(rows)))
        }
        Route::ListIncrements | Route::ListPromptIncrements | Route::GetIncrement => increments(state, route, &req),
        Route::GenerateIncrement => generate_increment(state, &req).await,
        Route::GenerateTitle => generate_title(state, &req).await,
        Route::LiveStatus => live_status(state).await,
        Route::PcmDiagnostics => pcm_diagnostics(state),
        Route::Debug => debug(state).await,
        Route::GetDebugAiContext => get_debug_ai_context(state, &req),
        Route::SetDebugAiContext => set_debug_ai_context(state, &req),
        Route::ClearDebugAiContext => {
            *state.debug_ai.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Default::default();
            Ok(responses::json(&json::object([("active", Value::Bool(false))])))
        }
        Route::Analyze | Route::AnalyzeMulti => analysis::analyze(state, &req, route == Route::AnalyzeMulti).await,
        Route::AnalyzeStream => analysis::analyze_stream(state, &req),
        Route::GetChat | Route::ClearChat => chat(state, &req, route == Route::ClearChat),
        Route::AskChat => ask_chat(state, &req).await,
        Route::SegmentAudio => audio::segment_audio(state, &req).await,
        Route::Root => Ok(root_response(&state.config.static_dir, &req.headers)),
    }
}

/// `HTTPException(500, detail=str(e))`.
pub fn fail(message: impl std::fmt::Display) -> axum::response::Response {
    responses::detail(StatusCode::INTERNAL_SERVER_ERROR, &message.to_string())
}

fn not_found(message: &str) -> axum::response::Response {
    responses::detail(StatusCode::NOT_FOUND, message)
}

fn bad_request(message: &str) -> axum::response::Response {
    responses::detail(StatusCode::BAD_REQUEST, message)
}

/// The body validated against a model, or the 422/400 FastAPI answers.
pub fn body_model(
    req: &Req,
    fields: &[Field],
    forbid_extra: bool,
    errors: &mut Vec<ErrorItem>,
) -> Result<Option<Map>, axum::response::Response> {
    let body = match req.body() {
        Ok(body) => body,
        Err(BodyError::Invalid(item)) => return Err(responses::invalid(&[item])),
        Err(BodyError::Unparsable) => return Err(bad_request("There was an error parsing the body")),
    };
    let Some(body) = body else {
        errors.push(ErrorItem {
            kind: "missing",
            loc: vec![Loc::from("body")],
            msg: "Field required".into(),
            input: Value::Null,
            ctx: None,
        });
        return Ok(None);
    };
    Ok(validate_model(&body, fields, forbid_extra, &[Loc::from("body")], errors))
}

fn check(errors: &[ErrorItem]) -> Result<(), axum::response::Response> {
    if errors.is_empty() { Ok(()) } else { Err(responses::invalid(errors)) }
}

fn storage(state: &AppState) -> Result<MeetingStorage, axum::response::Response> {
    state.config.storage().map_err(fail)
}

/// `storage.get_meeting(meeting_id)` or the 404.
fn meeting(storage: &MeetingStorage, meeting_id: &str) -> Result<Value, axum::response::Response> {
    match storage.get_meeting(meeting_id) {
        Ok(Some(meeting)) => Ok(meeting),
        Ok(None) => Err(not_found("Meeting not found")),
        Err(e) => Err(fail(e)),
    }
}

fn meta_of(meeting: &Value) -> Value {
    meeting.as_object().and_then(|m| m.get("meta")).cloned().unwrap_or_else(|| Value::Object(Map::new()))
}

fn segments_of(meeting: &Value) -> Vec<Value> {
    match meeting.as_object().and_then(|m| m.get("segments")) {
        Some(Value::Array(rows)) => rows.clone(),
        _ => Vec::new(),
    }
}

// --- AI settings, privacy, user profile ---

fn ai_write_error(error: AiError) -> axum::response::Response {
    match error {
        AiError::Conflict(message) => responses::detail(StatusCode::CONFLICT, &message),
        AiError::SettingsRead(_) => fail("Не удалось прочитать настройки AI."),
        AiError::Value(message) => responses::detail(StatusCode::UNPROCESSABLE_ENTITY, &message),
        AiError::Io(_) => fail("Не удалось сохранить настройки AI."),
        _ => responses::internal_error(),
    }
}

async fn get_ai_settings(state: &AppState) -> Reply {
    match state.ai_settings.view().await {
        Ok(view) => Ok(responses::json(&view)),
        Err(e) if e.is_value_error() || matches!(e, AiError::Io(_)) => Err(fail("Не удалось прочитать настройки AI.")),
        Err(_) => Err(responses::internal_error()),
    }
}

fn dict_of_str() -> Ty {
    Ty::Dict(Box::new(Ty::Str))
}

async fn save_ai_models(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let fields = [field("models", dict_of_str(), true), field("revision", Ty::Str, true)];
    let body = body_model(req, &fields, true, &mut errors)?;
    check(&errors)?;
    let body = body.unwrap_or_default();
    let models = body.get("models").cloned().unwrap_or(Value::Null);
    let revision = body.get("revision").map(Value::py_str);
    let result = state
        .ai_settings
        .write(move |store| store.write_models(&models, revision.as_deref()))
        .await
        .map_err(ai_write_error)?;
    Ok(responses::json(&result))
}

async fn save_ai_task(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let optional_str = || Ty::Optional(Box::new(Ty::Str));
    let fields = [
        field("enabled", Ty::Optional(Box::new(Ty::StrictBool)), false),
        field("prompt", Ty::Str, true),
        field("providers", Ty::List(Box::new(Ty::Str)), true),
        field("models", dict_of_str(), false),
        field("reasoning_effort", optional_str(), false),
        field("claude_effort", optional_str(), false),
        field("revision", Ty::Str, true),
    ];
    let body = body_model(req, &fields, true, &mut errors)?;
    check(&errors)?;
    let body = body.unwrap_or_default();
    // model_dump(exclude={"revision"}, exclude_none=True): models defaults to {}.
    let mut values = Map::new();
    for name in ["enabled", "prompt", "providers", "models", "reasoning_effort", "claude_effort"] {
        match body.get(name) {
            Some(Value::Null) => {}
            Some(value) => values.insert(name, value.clone()),
            None if name == "models" => values.insert(name, Value::Object(Map::new())),
            None => {}
        }
    }
    let revision = body.get("revision").map(Value::py_str);
    let task_id = req.param("task_id").to_owned();
    let values = Value::Object(values);
    let result = state
        .ai_settings
        .write(move |store| store.write_task(&task_id, &values, revision.as_deref()))
        .await
        .map_err(ai_write_error)?;
    Ok(responses::json(&result))
}

async fn reset_ai_task(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let revision = validation::validate_param("query", "revision", &Ty::Str, req.query("revision"), true, &mut errors);
    check(&errors)?;
    let revision = revision.map(|v| v.py_str());
    let task_id = req.param("task_id").to_owned();
    let result = state
        .ai_settings
        .write(move |store| store.reset_task(&task_id, revision.as_deref()))
        .await
        .map_err(ai_write_error)?;
    Ok(responses::json(&result))
}

fn privacy_json(privacy: recog_core::privacy::Privacy) -> Value {
    json::object([("enabled", Value::Bool(privacy.enabled)), ("configured", Value::Bool(privacy.configured))])
}

fn get_privacy(state: &AppState) -> Reply {
    let privacy = state.privacy.read().map_err(|_| fail("Не удалось прочитать настройку Privacy."))?;
    Ok(responses::json(&privacy_json(privacy)))
}

fn save_privacy(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let body = body_model(req, &[field("enabled", Ty::StrictBool, true)], true, &mut errors)?;
    check(&errors)?;
    let enabled = matches!(body.unwrap_or_default().get("enabled"), Some(Value::Bool(true)));
    let privacy = state.privacy.write(enabled).map_err(|_| fail("Не удалось сохранить настройку Privacy."))?;
    let state_json = privacy_json(privacy);
    let mut frame = Map::new();
    frame.insert("type", Value::from("privacy_updated"));
    for (k, v) in state_json.as_object().into_iter().flat_map(Map::iter) {
        frame.insert(k.clone(), v.clone());
    }
    state.forward_bridge_message(&json::ascii(&Value::Object(frame)));
    Ok(responses::json(&state_json))
}

fn read_profile_error() -> axum::response::Response {
    fail("Не удалось прочитать профиль пользователя.")
}

async fn save_user_profile(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let fields = [field("name", Ty::Str, true), field("aliases", Ty::List(Box::new(Ty::Str)), false)];
    let body = body_model(req, &fields, false, &mut errors)?;
    check(&errors)?;
    let body = body.unwrap_or_default();
    let name = body.get("name").map(Value::py_str).unwrap_or_default();
    let aliases: Vec<String> = match body.get("aliases") {
        Some(Value::Array(items)) => items.iter().map(Value::py_str).collect(),
        _ => Vec::new(),
    };
    match state.profile.save(&name, &aliases).await {
        Ok(snapshot) => Ok(responses::json(&snapshot)),
        Err(ProfileError::Invalid(message)) => Err(responses::detail(StatusCode::UNPROCESSABLE_ENTITY, &message)),
        Err(ProfileError::Io(_)) => Err(fail("Не удалось сохранить профиль пользователя.")),
    }
}

// --- Meetings ---

async fn list_meetings(state: &AppState) -> Reply {
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

async fn search(state: &AppState, req: &Req) -> Reply {
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

fn get_meeting(state: &AppState, req: &Req, meta: bool) -> Reply {
    let storage = storage(state)?;
    let meeting = meeting(&storage, req.param("meeting_id"))?;
    let value = if meta { meta_of(&meeting) } else { Value::Array(segments_of(&meeting)) };
    Ok(responses::json(&value))
}

async fn delete_meeting(state: &AppState, req: &Req) -> Reply {
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

// --- Increments and titles ---

fn generator(state: &AppState, with_prompts: bool) -> IncrementGenerator {
    let prompts = with_prompts.then(|| state.config.prompts_dir.clone());
    IncrementGenerator::new(
        &state.config.meetings_dir,
        Some(prompts.unwrap_or_else(|| state.config.prompts_dir.clone())),
    )
    .with_factory(Arc::clone(&state.ai.factory))
    .with_legacy_env(state.config.legacy_env.clone())
}

fn increment_json((meta, content): (recog_ai::json::Json, Option<String>)) -> Value {
    json::object([("meta", json::to_core(&meta)), ("content", json::opt_str(content.as_deref()))])
}

fn version_param(req: &Req, errors: &mut Vec<ErrorItem>) -> Option<i64> {
    match validation::validate_param("path", "version", &Ty::Int, Some(req.param("version")), true, errors) {
        Some(Value::Int(n)) => Some(i64::try_from(n).unwrap_or(i64::MAX)),
        _ => None,
    }
}

fn check_ids(req: &Req, with_prompt: bool) -> Result<(), axum::response::Response> {
    if invalid_id(req.param("meeting_id")) {
        return Err(bad_request("Invalid meeting ID"));
    }
    if with_prompt && invalid_id(req.param("prompt_id")) {
        return Err(bad_request("Invalid prompt ID"));
    }
    Ok(())
}

fn increments(state: &AppState, route: Route, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let version = if route == Route::GetIncrement { version_param(req, &mut errors) } else { None };
    check(&errors)?;
    check_ids(req, route != Route::ListIncrements)?;
    let meeting_id = req.param("meeting_id");
    let prompt_id = req.param("prompt_id");
    let storage = storage(state)?;
    meeting(&storage, meeting_id)?;
    let generator = generator(state, false);
    match route {
        Route::ListIncrements => {
            let all = generator.list_all_increments(meeting_id).map_err(fail)?;
            let mut map = Map::new();
            for (prompt, metas) in all {
                map.insert(prompt, Value::Array(metas.iter().map(json::to_core).collect()));
            }
            Ok(responses::json(&Value::Object(map)))
        }
        Route::ListPromptIncrements => {
            let metas = generator.list_increments(meeting_id, prompt_id).map_err(fail)?;
            Ok(responses::json(&Value::Array(metas.iter().map(json::to_core).collect())))
        }
        _ => {
            let version = version.unwrap_or(0);
            match generator.get_increment(meeting_id, prompt_id, version).map_err(fail)? {
                Some(found) => Ok(responses::json(&increment_json(found))),
                None => Err(not_found(&format!("Increment {prompt_id}/v{version} not found"))),
            }
        }
    }
}

async fn generate_increment(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let force = validation::validate_param("query", "force", &Ty::Bool, req.query("force"), false, &mut errors);
    let participant = validation::validate_param(
        "query",
        "participant_id",
        &Ty::Str,
        req.query("participant_id"),
        false,
        &mut errors,
    );
    check(&errors)?;
    check_ids(req, true)?;
    let force = matches!(force, Some(Value::Bool(true)));
    let participant_id = participant.map(|v| v.py_str());
    let meeting_id = req.param("meeting_id");
    let prompt_id = req.param("prompt_id");
    let storage = storage(state)?;
    let found = meeting(&storage, meeting_id)?;
    if let Some(participant) = &participant_id {
        let meta = meta_of(&found);
        let participants = get(&meta, "participants").map_err(fail)?.cloned();
        let known = match participants {
            None => false,
            Some(Value::Object(map)) => map.contains_key(participant),
            Some(Value::Array(items)) => items.contains(&Value::from(participant.as_str())),
            Some(Value::Str(s)) => s.contains(participant.as_str()),
            Some(_) => return Err(responses::internal_error()),
        };
        if !known {
            return Err(bad_request("Participant not found"));
        }
    }
    let meeting_path = state.config.meetings_dir.join(meeting_id);
    if !meeting_path.exists() {
        return Err(not_found("Meeting directory not found"));
    }
    let registry = PromptRegistry::new(&state.config.prompts_dir);
    let Some(prompt) = registry.get(prompt_id) else {
        return Err(not_found(&format!("Prompt '{prompt_id}' not found")));
    };
    let generator = generator(state, true);
    generator.migrate_flat_increments(&meeting_path, "my-increment").map_err(fail)?;
    let version =
        generator.generate(meeting_id, &meeting_path, &prompt, force, participant_id.as_deref()).await.map_err(fail)?;
    let Some(version) = version else {
        let metas = generator.list_increments(meeting_id, prompt_id).map_err(fail)?;
        let wanted = json::to_ai(&json::opt_str(participant_id.as_deref()));
        let latest =
            metas.iter().rfind(|m| m.get("participant_id").cloned().unwrap_or(recog_ai::json::Json::Null) == wanted);
        if let Some(latest) = latest
            && latest.get("status").and_then(recog_ai::json::Json::as_str) == Some("failed")
        {
            let message =
                latest.get("error").map_or_else(|| "Generation failed".to_owned(), recog_ai::json::Json::py_str);
            return Err(fail(message));
        }
        return Ok(responses::json(&json::object([
            ("status", Value::from("skipped")),
            ("message", Value::from("No new segments to process")),
        ])));
    };
    let data =
        generator.get_increment(meeting_id, prompt_id, version).map_err(fail)?.map_or(Value::Null, increment_json);
    Ok(responses::json(&json::object([
        ("status", Value::from("generated")),
        ("version", Value::Int(i128::from(version))),
        ("data", data),
    ])))
}

/// `json.dumps(value, ensure_ascii=False, indent=2)` written as Python writes it.
fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    std::fs::write(path, pyjson::dumps(value, Some(2)))
}

fn read_json(path: &Path) -> Result<Value, PyError> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&raw).map_err(|e| pyjson::utf8_error_message(&raw, &e))?;
    pyjson::loads(&text.replace("\r\n", "\n").replace('\r', "\n")).map_err(|e| e.0)
}

fn store_title(state: &AppState, meeting_id: &str, update: &Map) -> Result<(), PyError> {
    let meta_path = state.config.meetings_dir.join(meeting_id).join("meta.json");
    if !meta_path.exists() {
        return Ok(());
    }
    let mut meta = read_json(&meta_path)?;
    let Value::Object(map) = &mut meta else { return Err("'list' object does not support item assignment".into()) };
    for (k, v) in update.iter() {
        map.insert(k.clone(), v.clone());
    }
    map.insert("updated_at", Value::from(local_now_isoformat()));
    let ended = map.get("ended_at").is_some_and(Value::truthy);
    write_json(&meta_path, &meta).map_err(|e| e.to_string())?;
    if !ended {
        return Ok(());
    }
    let index_path = state.config.meetings_dir.join("index.json");
    if !index_path.exists() {
        return Ok(());
    }
    let mut index = read_json(&index_path)?;
    let empty = Value::Array(Vec::new());
    let rows = get(&index, "meetings")?.unwrap_or(&empty).clone();
    let Value::Array(mut rows) = rows else { return Err("'meetings' is not a list".into()) };
    for row in &mut rows {
        if get(row, "id")? == Some(&Value::from(meeting_id)) {
            if let Value::Object(m) = row {
                m.insert("title", update.get("title").cloned().unwrap_or(Value::Null));
            }
            break;
        }
    }
    if let Value::Object(map) = &mut index {
        map.insert("meetings", Value::Array(rows));
    }
    write_json(&index_path, &index).map_err(|e| e.to_string())
}

async fn generate_title(state: &AppState, req: &Req) -> Reply {
    let meeting_id = req.param("meeting_id").to_owned();
    let storage = storage(state)?;
    let found = meeting(&storage, &meeting_id)?;
    let segments = segments_of(&found);
    if segments.is_empty() {
        return Err(bad_request("Meeting has no transcript"));
    }
    let transcript = transcript::segments_to_transcript(&segments).map_err(fail)?;
    if transcript.is_empty() {
        return Err(bad_request("Meeting has no transcript text"));
    }
    let settings = state.ai.task_settings("title").map_err(fail)?;
    let again = storage.get_meeting(&meeting_id).map_err(fail)?;
    let (meeting_title, participants) = transcript::analysis_prompt_metadata(again.as_ref()).map_err(fail)?;
    let user_name =
        transcript::join_names(&transcript::local_participant_names(&meta_of(&found)).map_err(fail)?).map_err(fail)?;
    let user_name = if user_name.is_empty() { "User".to_owned() } else { user_name };
    let vars = variables([
        ("meeting_title", meeting_title.as_str()),
        ("participants", participants.as_str()),
        ("user_name", user_name.as_str()),
    ]);
    let title = title::generate_title(&transcript, None, Some(&settings), &vars, state.ai.factory.as_ref()).await;
    let provider = settings.provider().to_owned();
    let Some(title) = title else { return Err(fail("Failed to generate title")) };
    let mut update = Map::new();
    update.insert("title", Value::from(title.as_str()));
    update.insert("title_source", Value::from(provider.as_str()));
    let model = settings.models.get(&provider).ok_or_else(|| fail(format!("'{provider}'")))?;
    update.insert("title_model", Value::from(model.as_str()));
    update.insert("title_settings_revision", Value::from(settings.revision.as_str()));
    store_title(state, &meeting_id, &update).map_err(fail)?;
    if let Some(catalog) = state.catalog() {
        catalog.refresh(None).await;
    }
    Ok(responses::json(&json::object([
        ("meeting_id", Value::from(meeting_id.as_str())),
        ("title", Value::from(title)),
        ("title_source", Value::from(provider)),
    ])))
}

// --- Live status and debug ---

async fn live_status(state: &AppState) -> Reply {
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

fn pcm_diagnostics(state: &AppState) -> Reply {
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

async fn debug(state: &AppState) -> Reply {
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

fn get_debug_ai_context(state: &AppState, req: &Req) -> Reply {
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

fn set_debug_ai_context(state: &AppState, req: &Req) -> Reply {
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
        local_names = transcript::local_participant_names(&meta).map_err(|_| responses::internal_error())?;
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

// --- Questions about a meeting ---

/// `_meeting_chat_path`.
fn chat_path(storage: &MeetingStorage, meeting_id: &str) -> Result<std::path::PathBuf, axum::response::Response> {
    if invalid_id(meeting_id) {
        return Err(bad_request("Invalid meeting ID"));
    }
    storage.find_meeting_path(meeting_id).ok_or_else(|| not_found("Meeting not found"))
}

fn chat(state: &AppState, req: &Req, clear: bool) -> Reply {
    let storage = state.config.storage().map_err(|_| responses::internal_error())?;
    let path = chat_path(&storage, req.param("meeting_id"))?;
    if clear {
        meeting_chat::clear_turns(&path).map_err(|_| responses::internal_error())?;
        return Ok(responses::json(&json::object([("success", Value::Bool(true))])));
    }
    let turns = meeting_chat::read_turns(&path).map_err(|_| responses::internal_error())?;
    Ok(responses::json(&Value::Array(turns.iter().map(|t| json::to_core(&t.to_json())).collect())))
}

/// `_resolve_ai_context(meeting_id, storage, debug)`: the segments and the local
/// participant, or the 404/400 Python raises.
pub fn resolve_ai_context(
    state: &AppState,
    meeting_id: &str,
    storage: &MeetingStorage,
    debug: bool,
) -> Result<Result<(Vec<Value>, String), axum::response::Response>, PyError> {
    if debug {
        let context = state.debug_ai.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if !context.segments.is_empty() {
            let user =
                context.user_name.filter(|n| !recog_core::py::strip(n).is_empty()).unwrap_or_else(|| "User".to_owned());
            return Ok(Ok((context.segments, user)));
        }
    }
    if meeting_id.is_empty() {
        return Ok(Err(not_found("Meeting not found")));
    }
    let Some(found) = storage.get_meeting(meeting_id).map_err(|e| e.to_string())? else {
        return Ok(Err(not_found("Meeting not found")));
    };
    let names = transcript::local_participant_names(&meta_of(&found))?;
    let local = if names.is_empty() { "User".to_owned() } else { transcript::join_names(&names)? };
    let segments = segments_of(&found);
    if segments.is_empty() {
        return Ok(Err(bad_request("Meeting has no transcript")));
    }
    Ok(Ok((segments, local)))
}

async fn ask_chat(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let fields = [field("question", Ty::ConStr { strip: true, min: 1, max: 4000 }, true)];
    let body = body_model(req, &fields, false, &mut errors)?;
    check(&errors)?;
    let question = body.unwrap_or_default().get("question").map(Value::py_str).unwrap_or_default();
    let meeting_id = req.param("meeting_id").to_owned();
    let storage = state.config.storage().map_err(|_| responses::internal_error())?;
    let path = chat_path(&storage, &meeting_id)?;
    let (segments, local) =
        resolve_ai_context(state, &meeting_id, &storage, false).map_err(|_| responses::internal_error())??;
    let transcript = transcript::build_ai_transcript(&segments, state.chat_transcript_max_chars)
        .map_err(|_| responses::internal_error())?;
    if transcript.is_empty() {
        return Err(bad_request("Meeting has no transcript"));
    }
    let settings = state.ai.task_settings("meeting_chat").map_err(|_| responses::internal_error())?;
    let again = storage.get_meeting(&meeting_id).map_err(|_| responses::internal_error())?;
    let (title, participants) =
        transcript::analysis_prompt_metadata(again.as_ref()).map_err(|_| responses::internal_error())?;
    let instructions =
        build_prompt(&settings, &transcript, &local, &participants, &title).map_err(|_| responses::internal_error())?;
    let history = meeting_chat::read_turns(&path).map_err(|_| responses::internal_error())?;
    let prompt = meeting_chat::build_chat_prompt(&instructions, &history, &question);
    let provider = state
        .ai
        .provider("meeting_chat", &settings)
        .map_err(|e| responses::detail(StatusCode::BAD_GATEWAY, &e.to_string()))?;
    let answer =
        provider.complete(&prompt).await.map_err(|e| responses::detail(StatusCode::BAD_GATEWAY, &e.to_string()))?;
    let answer = recog_core::py::strip(&answer).to_owned();
    if answer.is_empty() {
        return Err(responses::detail(StatusCode::BAD_GATEWAY, "Модель вернула пустой ответ"));
    }
    let turn = ChatTurn::new(
        &utc_now_isoformat(),
        &question,
        &answer,
        &provider.name(),
        &settings.revision,
        transcript.starts_with("...(truncated)..."),
    );
    match meeting_chat::append_turn(&path, &turn) {
        Ok(()) => Ok(responses::json(&json::to_core(&turn.to_json()))),
        Err(AiError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(not_found("Meeting not found")),
        Err(_) => Err(responses::internal_error()),
    }
}
