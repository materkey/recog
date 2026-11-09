//! Increments and titles.

use std::sync::Arc;

use recog_ai::increment::IncrementGenerator;
use recog_ai::prompt_registry::PromptRegistry;
use recog_ai::template::variables;
use recog_ai::title;
use recog_core::meeting_storage::{UnnamedLocal, local_participant_names};
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

pub(super) async fn generate_increment(state: &AppState, req: &Req) -> Reply {
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

pub(super) async fn generate_title(state: &AppState, req: &Req) -> Reply {
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
        transcript::join_names(&local_participant_names(&meta_of(&found), UnnamedLocal::Empty).map_err(fail)?)
            .map_err(fail)?;
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
    storage.store_title(&meeting_id, &update).map_err(fail)?;
    if let Some(catalog) = state.catalog() {
        catalog.refresh(None).await;
    }
    Ok(responses::json(&json::object([
        ("meeting_id", Value::from(meeting_id.as_str())),
        ("title", Value::from(title)),
        ("title_source", Value::from(provider)),
    ])))
}
