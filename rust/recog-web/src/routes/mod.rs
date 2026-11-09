//! The HTTP route handlers of `recog/web/server.py`.
//!
//! Each handler keeps Python's order of checks and its error mapping: validation
//! (422) before anything else, `HTTPException`s with their detail, `except Exception`
//! blocks as `500 {"detail": str(e)}`, and exceptions Python does not catch as the
//! plain `500 Internal Server Error`.

mod chat;
mod debug;
mod increments;
mod meetings;
mod settings;

use std::sync::Arc;

use axum::http::StatusCode;
use recog_ai::prompt_registry::PromptRegistry;
use recog_core::meeting_storage::MeetingStorage;
use recog_core::pyjson::{Map, Value};

use crate::json;
use crate::request::{BodyError, Req};
use crate::responses::{self, Reply};
use crate::router::Route;
use crate::server::{AppState, root_response};
use crate::validation::{ErrorItem, Field, Loc, validate_model};
use crate::{analysis, audio, desktop};

pub use chat::resolve_ai_context;
use chat::{ask_chat, chat};
use debug::{debug, get_debug_ai_context, live_status, pcm_diagnostics, set_debug_ai_context};
use increments::{generate_increment, generate_title, increments};
use meetings::{delete_meeting, get_meeting, list_meetings, search};
use settings::{
    get_ai_settings, get_privacy, read_profile_error, reset_ai_task, save_ai_models, save_ai_task, save_privacy,
    save_user_profile,
};

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
