//! `analyze`, `analyze-multi` and the Server-Sent Events of `analyze-stream`: the
//! increment scenario run over a meeting's transcript (or the debug context).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use recog_ai::AiError;
use recog_ai::provider::AiProvider;
use recog_core::pyjson::{Map, Value};
use tokio::sync::mpsc;

use crate::ai_provider::build_prompt;
use crate::json;
use crate::request::Req;
use crate::responses::{self, Reply};
use crate::routes::{body_model, fail, resolve_ai_context};
use crate::server::AppState;
use crate::transcript::{self, PyError};
use crate::validation::{self, Ty, field};

const ANALYSIS_MAX_CHARS: usize = 100_000;
const QUESTIONS: &[&str] = &["increment"];

/// The prompt of the increment scenario over a meeting, or the HTTP error Python
/// raises before calling a provider.
fn prepare(
    state: &AppState,
    meeting_id: &str,
    question: &str,
    debug: bool,
) -> Result<Result<(recog_ai::settings::ResolvedTask, String), Response>, PyError> {
    let storage = state.config.storage().map_err(|e| e.to_string())?;
    let (segments, local) = match resolve_ai_context(state, meeting_id, &storage, debug)? {
        Ok(found) => found,
        Err(response) => return Ok(Err(response)),
    };
    let transcript = transcript::build_ai_transcript(&segments, ANALYSIS_MAX_CHARS)?;
    let settings = state.ai.task_settings(question).map_err(|e| e.to_string())?;
    let meeting =
        if meeting_id.is_empty() { None } else { storage.get_meeting(meeting_id).map_err(|e| e.to_string())? };
    let (title, participants) = transcript::analysis_prompt_metadata(meeting.as_ref())?;
    let prompt = build_prompt(&settings, &transcript, &local, &participants, &title).map_err(|e| e.to_string())?;
    Ok(Ok((settings, prompt)))
}

/// `POST /api/meetings/{id}/analyze` and `/analyze-multi`.
pub async fn analyze(state: &Arc<AppState>, req: &Req, multi: bool) -> Reply {
    let mut errors = Vec::new();
    let fields = [field("question", Ty::Literal(QUESTIONS), false), field("debug", Ty::Bool, false)];
    let body = body_model(req, &fields, true, &mut errors)?;
    if !errors.is_empty() {
        return Err(responses::invalid(&errors));
    }
    let body = body.unwrap_or_default();
    let question = body.get("question").map_or_else(|| "increment".to_owned(), Value::py_str);
    let debug = matches!(body.get("debug"), Some(Value::Bool(true)));
    let (settings, prompt) = prepare(state, req.param("meeting_id"), &question, debug).map_err(fail)??;
    if !multi {
        let provider = state.ai.provider(&question, &settings).map_err(fail)?;
        let answer = provider.complete(&prompt).await.map_err(fail)?;
        return Ok(responses::json(&json::object([
            ("answer", Value::from(answer)),
            ("provider", Value::from(provider.name())),
        ])));
    }
    let providers = state.ai.all_providers(&question, &settings).map_err(fail)?;
    let calls = providers.iter().map(|provider| {
        let prompt = prompt.as_str();
        async move {
            let result = provider.complete(prompt).await;
            let (answer, error) = match result {
                Ok(answer) => (Value::from(answer), Value::Null),
                Err(e) => (Value::Null, Value::from(e.to_string())),
            };
            (
                provider.key().to_owned(),
                json::object([("answer", answer), ("name", Value::from(provider.name())), ("error", error)]),
            )
        }
    });
    let mut responses_map = Map::new();
    for (key, value) in futures_util::future::join_all(calls).await {
        responses_map.insert(key, value);
    }
    Ok(responses::json(&json::object([("responses", Value::Object(responses_map))])))
}

fn event(value: &Value) -> String {
    format!("data: {}\n\n", json::ascii(value))
}

async fn produce(provider: Box<dyn AiProvider>, prompt: Arc<String>, tx: mpsc::UnboundedSender<Option<Value>>) {
    let key = provider.key().to_owned();
    let chunk_tx = tx.clone();
    let chunk_key = key.clone();
    let mut sink = move |text: String, done: bool| {
        let _ = chunk_tx.send(Some(json::object([
            ("type", Value::from(if done { "done" } else { "chunk" })),
            ("provider", Value::from(chunk_key.as_str())),
            ("text", Value::from(text)),
        ])));
    };
    let result: Result<(), AiError> = provider.stream(&prompt, &mut sink).await;
    if let Err(e) = result {
        let _ = tx.send(Some(json::object([
            ("type", Value::from("error")),
            ("provider", Value::from(key.as_str())),
            ("message", Value::from(e.to_string())),
        ])));
    }
    let _ = tx.send(None);
}

async fn stream_events(
    state: Arc<AppState>,
    meeting_id: String,
    question: String,
    debug: bool,
    out: mpsc::Sender<String>,
) {
    let error =
        |message: String| event(&json::object([("type", Value::from("error")), ("message", Value::from(message))]));
    let prepared = tokio::task::spawn_blocking({
        let state = Arc::clone(&state);
        let (meeting_id, question) = (meeting_id.clone(), question.clone());
        move || prepare(&state, &meeting_id, &question, debug)
    })
    .await;
    let (settings, prompt) = match prepared {
        Ok(Ok(Ok(ready))) => ready,
        Ok(Ok(Err(response))) => {
            let detail = response_detail(response).await;
            let _ = out.send(event(&json::object([("type", Value::from("error")), ("message", detail)]))).await;
            return;
        }
        Ok(Err(message)) => {
            let _ = out.send(error(message)).await;
            return;
        }
        Err(e) => {
            let _ = out.send(error(e.to_string())).await;
            return;
        }
    };
    let providers = match state.ai.all_providers(&question, &settings) {
        Ok(providers) => providers,
        Err(e) => {
            let _ = out.send(error(e.to_string())).await;
            return;
        }
    };
    for provider in &providers {
        let start = json::object([
            ("type", Value::from("start")),
            ("provider", Value::from(provider.key())),
            ("name", Value::from(provider.name())),
        ]);
        if out.send(event(&start)).await.is_err() {
            return;
        }
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    let prompt = Arc::new(prompt);
    let mut pending = providers.len();
    let tasks: Vec<_> = providers
        .into_iter()
        .map(|provider| tokio::spawn(produce(provider, Arc::clone(&prompt), tx.clone())))
        .collect();
    drop(tx);
    let mut client_gone = false;
    while pending > 0 {
        match rx.recv().await {
            Some(Some(value)) => {
                if out.send(event(&value)).await.is_err() {
                    client_gone = true;
                    break;
                }
            }
            Some(None) => pending -= 1,
            None => break,
        }
    }
    for task in &tasks {
        task.abort();
    }
    if !client_gone {
        let _ = out.send(event(&json::object([("type", Value::from("complete"))]))).await;
    }
}

/// The `detail` of an `HTTPException` response, for the SSE error event.
async fn response_detail(response: Response) -> Value {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap_or_default();
    match recog_core::pyjson::loads(&String::from_utf8_lossy(&body)) {
        Ok(Value::Object(map)) => map.get("detail").cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// `GET /api/meetings/{id}/analyze-stream`.
pub fn analyze_stream(state: &Arc<AppState>, req: &Req) -> Reply {
    let mut errors = Vec::new();
    let question = validation::validate_param(
        "query",
        "question",
        &Ty::Literal(QUESTIONS),
        req.query("question"),
        false,
        &mut errors,
    );
    let debug = validation::validate_param("query", "debug", &Ty::Bool, req.query("debug"), false, &mut errors);
    if !errors.is_empty() {
        return Err(responses::invalid(&errors));
    }
    let question = question.map_or_else(|| "increment".to_owned(), |v| v.py_str());
    let debug = matches!(debug, Some(Value::Bool(true)));
    let (out, rx) = mpsc::channel::<String>(64);
    tokio::spawn(stream_events(Arc::clone(state), req.param("meeting_id").to_owned(), question, debug, out));
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (Ok::<_, std::convert::Infallible>(chunk), rx))
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream; charset=utf-8"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    Ok(response)
}
