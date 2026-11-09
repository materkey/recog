//! Questions about a meeting.

use axum::http::StatusCode;
use recog_ai::AiError;
use recog_ai::meeting_chat::{self, ChatTurn};
use recog_core::meeting_storage::{MeetingStorage, UnnamedLocal, local_participant_names};
use recog_core::pyjson::Value;

use super::{bad_request, body_model, check, meta_of, not_found, segments_of};
use crate::ai_provider::build_prompt;
use crate::json;
use crate::request::Req;
use crate::responses::{self, Reply};
use crate::server::AppState;
use crate::transcript::{self, PyError};
use crate::util::{invalid_id, utc_now_isoformat};
use crate::validation::{Ty, field};

/// `_meeting_chat_path`.
fn chat_path(storage: &MeetingStorage, meeting_id: &str) -> Result<std::path::PathBuf, axum::response::Response> {
    if invalid_id(meeting_id) {
        return Err(bad_request("Invalid meeting ID"));
    }
    storage.find_meeting_path(meeting_id).ok_or_else(|| not_found("Meeting not found"))
}

pub(super) fn chat(state: &AppState, req: &Req, clear: bool) -> Reply {
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
    let names = local_participant_names(&meta_of(&found), UnnamedLocal::Empty).map_err(|e| e.to_string())?;
    let local = if names.is_empty() { "User".to_owned() } else { transcript::join_names(&names)? };
    let segments = segments_of(&found);
    if segments.is_empty() {
        return Ok(Err(bad_request("Meeting has no transcript")));
    }
    Ok(Ok((segments, local)))
}

pub(super) async fn ask_chat(state: &AppState, req: &Req) -> Reply {
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
    let transcript = transcript::build_ai_transcript(&segments, state.config.chat_transcript_max_chars)
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
