//! The AI settings, privacy and user profile routes.

use axum::http::StatusCode;
use recog_ai::AiError;
use recog_core::privacy::PrivacySource as _;
use recog_core::pyjson::{Map, Value};
use recog_core::user_profile::ProfileError;

use super::{body_model, check, fail};
use crate::json;
use crate::request::Req;
use crate::responses::{self, Reply};
use crate::server::AppState;
use crate::validation::{self, Ty, field};

fn ai_write_error(error: AiError) -> axum::response::Response {
    match error {
        AiError::Conflict(message) => responses::detail(StatusCode::CONFLICT, &message),
        AiError::SettingsRead(_) => fail("Не удалось прочитать настройки AI."),
        AiError::Value(message) => responses::detail(StatusCode::UNPROCESSABLE_ENTITY, &message),
        AiError::Io(_) => fail("Не удалось сохранить настройки AI."),
        _ => responses::internal_error(),
    }
}

pub(super) async fn get_ai_settings(state: &AppState) -> Reply {
    match state.ai_settings.view().await {
        Ok(view) => Ok(responses::json(&view)),
        Err(e) if e.is_value_error() || matches!(e, AiError::Io(_)) => Err(fail("Не удалось прочитать настройки AI.")),
        Err(_) => Err(responses::internal_error()),
    }
}

fn dict_of_str() -> Ty {
    Ty::Dict(Box::new(Ty::Str))
}

pub(super) async fn save_ai_models(state: &AppState, req: &Req) -> Reply {
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

pub(super) async fn save_ai_access(state: &AppState, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let fields = [field("access", Ty::Str, true), field("revision", Ty::Str, true)];
    let body = body_model(req, &fields, true, &mut errors)?;
    check(&errors)?;
    let body = body.unwrap_or_default();
    let access = body.get("access").map(Value::py_str).unwrap_or_default();
    let revision = body.get("revision").map(Value::py_str);
    let result = state
        .ai_settings
        .write(move |store| store.write_access(&access, revision.as_deref()))
        .await
        .map_err(ai_write_error)?;
    Ok(responses::json(&result))
}

pub(super) async fn save_ai_task(state: &AppState, req: &Req) -> Reply {
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
    let revision = body.get("revision").map(Value::py_str);
    let task_id = req.param("task_id").to_owned();
    let values = Value::Object(task_values(&body));
    let result = state
        .ai_settings
        .write(move |store| store.write_task(&task_id, &values, revision.as_deref()))
        .await
        .map_err(ai_write_error)?;
    Ok(responses::json(&result))
}

/// The task's settings as `model_dump(exclude={"revision"}, exclude_none=True)`
/// gives them: None values left out, models defaulting to {}.
fn task_values(body: &Map) -> Map {
    let mut values = Map::new();
    for name in ["enabled", "prompt", "providers", "models", "reasoning_effort", "claude_effort"] {
        match body.get(name) {
            Some(Value::Null) => {}
            Some(value) => values.insert(name, value.clone()),
            None if name == "models" => values.insert(name, Value::Object(Map::new())),
            None => {}
        }
    }
    values
}

pub(super) async fn reset_ai_task(state: &AppState, req: &Req) -> Reply {
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

pub(super) fn get_privacy(state: &AppState) -> Reply {
    let privacy = state.privacy.read().map_err(|_| fail("Не удалось прочитать настройку Privacy."))?;
    Ok(responses::json(&privacy_json(privacy)))
}

pub(super) fn save_privacy(state: &AppState, req: &Req) -> Reply {
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

pub(super) fn read_profile_error() -> axum::response::Response {
    fail("Не удалось прочитать профиль пользователя.")
}

pub(super) async fn save_user_profile(state: &AppState, req: &Req) -> Reply {
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
