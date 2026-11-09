//! Increments and titles.

use std::path::PathBuf;
use std::sync::Arc;

use recog_ai::increment::IncrementGenerator;
use recog_ai::json::Json;
use recog_ai::prompt_registry::{PromptConfig, PromptRegistry};
use recog_ai::settings::ResolvedTask;
use recog_ai::template::{Variables, variables};
use recog_ai::title;
use recog_core::meeting_storage::{MeetingStorage, UnnamedLocal, local_participant_names};
use recog_core::pyjson::{Map, Value};

use super::{bad_request, check, fail, meeting, meta_of, not_found, segments_of, storage};
use crate::json;
use crate::request::Req;
use crate::responses::{self, Reply};
use crate::router::Route;
use crate::server::AppState;
use crate::transcript::{self, get};
use crate::util::invalid_id;
use crate::validation::{self, ErrorItem, Ty};

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

pub(super) fn increments(state: &AppState, route: Route, req: &Req) -> Reply {
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
        Route::ListIncrements => list_all(&generator, meeting_id),
        Route::ListPromptIncrements => {
            let metas = generator.list_increments(meeting_id, prompt_id).map_err(fail)?;
            Ok(responses::json(&Value::Array(metas.iter().map(json::to_core).collect())))
        }
        _ => one_version(&generator, meeting_id, prompt_id, version.unwrap_or(0)),
    }
}

/// Every prompt's increments of a meeting.
fn list_all(generator: &IncrementGenerator, meeting_id: &str) -> Reply {
    let all = generator.list_all_increments(meeting_id).map_err(fail)?;
    let mut map = Map::new();
    for (prompt, metas) in all {
        map.insert(prompt, Value::Array(metas.iter().map(json::to_core).collect()));
    }
    Ok(responses::json(&Value::Object(map)))
}

/// One saved version of a prompt's increment, or the 404.
fn one_version(generator: &IncrementGenerator, meeting_id: &str, prompt_id: &str, version: i64) -> Reply {
    match generator.get_increment(meeting_id, prompt_id, version).map_err(fail)? {
        Some(found) => Ok(responses::json(&increment_json(found))),
        None => Err(not_found(&format!("Increment {prompt_id}/v{version} not found"))),
    }
}

/// The `force` and `participant_id` query parameters, or the 422.
fn generate_params(req: &Req) -> Result<(bool, Option<String>), axum::response::Response> {
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
    Ok((matches!(force, Some(Value::Bool(true))), participant.map(|v| v.py_str())))
}

/// The 400 of a participant the meeting does not list (by key, item or substring).
fn check_participant(found: &Value, participant: Option<&str>) -> Result<(), axum::response::Response> {
    let Some(participant) = participant else { return Ok(()) };
    let meta = meta_of(found);
    let known = match get(&meta, "participants").map_err(fail)? {
        None => false,
        Some(Value::Object(map)) => map.contains_key(participant),
        Some(Value::Array(items)) => items.contains(&Value::from(participant)),
        Some(Value::Str(s)) => s.contains(participant),
        Some(_) => return Err(responses::internal_error()),
    };
    if known { Ok(()) } else { Err(bad_request("Participant not found")) }
}

/// The meeting's own directory and the prompt, or their 404.
fn directory_and_prompt(
    state: &AppState,
    meeting_id: &str,
    prompt_id: &str,
) -> Result<(PathBuf, PromptConfig), axum::response::Response> {
    let meeting_path = state.config.meetings_dir.join(meeting_id);
    if !meeting_path.exists() {
        return Err(not_found("Meeting directory not found"));
    }
    let registry = PromptRegistry::new(&state.config.prompts_dir);
    let Some(prompt) = registry.get(prompt_id) else {
        return Err(not_found(&format!("Prompt '{prompt_id}' not found")));
    };
    Ok((meeting_path, prompt))
}

pub(super) async fn generate_increment(state: &AppState, req: &Req) -> Reply {
    let (force, participant_id) = generate_params(req)?;
    check_ids(req, true)?;
    let meeting_id = req.param("meeting_id");
    let prompt_id = req.param("prompt_id");
    let storage = storage(state)?;
    let found = meeting(&storage, meeting_id)?;
    check_participant(&found, participant_id.as_deref())?;
    let (meeting_path, prompt) = directory_and_prompt(state, meeting_id, prompt_id)?;
    let generator = generator(state, true);
    generator.migrate_flat_increments(&meeting_path, "my-increment").map_err(fail)?;
    let version =
        generator.generate(meeting_id, &meeting_path, &prompt, force, participant_id.as_deref()).await.map_err(fail)?;
    let Some(version) = version else {
        return not_generated(&generator, meeting_id, prompt_id, participant_id.as_deref());
    };
    let data =
        generator.get_increment(meeting_id, prompt_id, version).map_err(fail)?.map_or(Value::Null, increment_json);
    Ok(responses::json(&json::object([
        ("status", Value::from("generated")),
        ("version", Value::Int(i128::from(version))),
        ("data", data),
    ])))
}

/// No new version: the 500 of the latest version for this participant when it
/// failed, else `skipped`.
fn not_generated(
    generator: &IncrementGenerator,
    meeting_id: &str,
    prompt_id: &str,
    participant_id: Option<&str>,
) -> Reply {
    let metas = generator.list_increments(meeting_id, prompt_id).map_err(fail)?;
    let wanted = json::to_ai(&json::opt_str(participant_id));
    let latest = metas.iter().rfind(|m| m.get("participant_id").cloned().unwrap_or(Json::Null) == wanted);
    if let Some(latest) = latest
        && latest.get("status").and_then(Json::as_str) == Some("failed")
    {
        let message = latest.get("error").map_or_else(|| "Generation failed".to_owned(), Json::py_str);
        return Err(fail(message));
    }
    Ok(responses::json(&json::object([
        ("status", Value::from("skipped")),
        ("message", Value::from("No new segments to process")),
    ])))
}

/// The transcript a title is generated from, or the 400 of a meeting without text.
fn title_transcript(found: &Value) -> Result<String, axum::response::Response> {
    let segments = segments_of(found);
    if segments.is_empty() {
        return Err(bad_request("Meeting has no transcript"));
    }
    let transcript = transcript::segments_to_transcript(&segments).map_err(fail)?;
    if transcript.is_empty() {
        return Err(bad_request("Meeting has no transcript text"));
    }
    Ok(transcript)
}

/// The title prompt's variables: the meeting's title, its participants and the user.
fn title_variables(
    storage: &MeetingStorage,
    meeting_id: &str,
    found: &Value,
) -> Result<Variables, axum::response::Response> {
    let again = storage.get_meeting(meeting_id).map_err(fail)?;
    let (meeting_title, participants) = transcript::analysis_prompt_metadata(again.as_ref()).map_err(fail)?;
    let user_name =
        transcript::join_names(&local_participant_names(&meta_of(found), UnnamedLocal::Empty).map_err(fail)?)
            .map_err(fail)?;
    let user_name = if user_name.is_empty() { "User".to_owned() } else { user_name };
    Ok(variables([
        ("meeting_title", meeting_title.as_str()),
        ("participants", participants.as_str()),
        ("user_name", user_name.as_str()),
    ]))
}

/// What a generated title writes to `meta.json`.
fn title_update(title: &str, settings: &ResolvedTask) -> Result<Map, axum::response::Response> {
    let provider = settings.provider();
    let mut update = Map::new();
    update.insert("title", Value::from(title));
    update.insert("title_source", Value::from(provider));
    let model = settings.models.get(provider).ok_or_else(|| fail(format!("'{provider}'")))?;
    update.insert("title_model", Value::from(model.as_str()));
    update.insert("title_settings_revision", Value::from(settings.revision.as_str()));
    Ok(update)
}

pub(super) async fn generate_title(state: &AppState, req: &Req) -> Reply {
    let meeting_id = req.param("meeting_id").to_owned();
    let storage = storage(state)?;
    let found = meeting(&storage, &meeting_id)?;
    let transcript = title_transcript(&found)?;
    let settings = state.ai.task_settings("title").map_err(fail)?;
    let vars = title_variables(&storage, &meeting_id, &found)?;
    let title = title::generate_title(&transcript, None, Some(&settings), &vars, state.ai.factory.as_ref()).await;
    let provider = settings.provider().to_owned();
    let Some(title) = title else { return Err(fail("Failed to generate title")) };
    storage.store_title(&meeting_id, &title_update(&title, &settings)?).map_err(fail)?;
    if let Some(catalog) = state.catalog() {
        catalog.refresh(None).await;
    }
    Ok(responses::json(&json::object([
        ("meeting_id", Value::from(meeting_id.as_str())),
        ("title", Value::from(title)),
        ("title_source", Value::from(provider)),
    ])))
}
