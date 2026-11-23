//! The analysis routes over a meeting or the debug context: `analyze` (one provider),
//! `analyze-multi` (every selected provider) and the Server-Sent Events of
//! `analyze-stream`, with their refusals and the providers' failures.

mod support;

use axum::http::StatusCode;
use recog_core::pyjson::{self, Value};
use support::{Answer, App, get, obj, text, write};

fn meeting() -> App {
    let app = App::new();
    app.factory.answer(Answer::Text("Синяя кнопка.".into()));
    let meeting = app.meetings().join("m1");
    write(
        &meeting.join("meta.json"),
        r#"{"id": "m1", "title": "Обзор", "created_at": "2026-09-01T10:00:00+00:00", "participants": {"alice": {"name": "Алиса", "is_local": true}}}"#,
    );
    write(
        &meeting.join("transcript.jsonl"),
        "{\"id\": 1, \"ts_display\": \"10:00:05\", \"speaker\": \"Алиса\", \"text\": \"Берём синюю кнопку\"}\n",
    );
    write(&app.meetings().join("empty/meta.json"), r#"{"id": "empty", "created_at": "2026-09-01T10:00:00+00:00"}"#);
    write(&app.meetings().join("empty/transcript.jsonl"), "");
    app
}

fn detail(body: &Value) -> String {
    text(body, "detail")
}

/// The events of an SSE body.
fn events(body: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(body).split("\n\n").filter_map(|chunk| chunk.strip_prefix("data: ")).map(obj).collect()
}

fn kinds(events: &[Value]) -> Vec<String> {
    events.iter().map(|e| format!("{}:{}", text(e, "type"), text(e, "provider"))).collect()
}

// --- analyze ---

#[tokio::test]
async fn recorded_meeting_when_analyzed_then_the_answer_of_the_selected_provider() {
    let app = meeting();

    let (status, body) = app.json("POST", "/api/meetings/m1/analyze", Some("{}")).await;

    assert_eq!(status, 200);
    assert_eq!(text(&body, "answer"), "Синяя кнопка.");
    assert!(text(&body, "provider").starts_with("Fake ("), "{body:?}");
    let prompt = &app.factory.prompts()[0];
    assert!(prompt.contains("[10:00:05] Алиса: Берём синюю кнопку"), "{prompt}");
    assert!(prompt.contains("Алиса"));
}

#[tokio::test]
async fn level_off_when_analysis_requested_then_refusal_text_and_no_provider() {
    let app = meeting();
    write(
        &app.data().join("ai-settings.json"),
        r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {}, "access": "off"}"#,
    );

    let (_status, body) = app.json("POST", "/api/meetings/m1/analyze", Some("{}")).await;

    assert_eq!(detail(&body), "AI-функции выключены в настройках recog.");
    assert!(app.factory.prompts().is_empty());
}

#[tokio::test]
async fn invalid_bodies_when_analyzed_then_422_before_any_prompt() {
    let app = meeting();
    for body in [r#"{"question": "custom"}"#, r#"{"debug": "maybe"}"#, r#"{"x": 1}"#] {
        let (status, _) = app.json("POST", "/api/meetings/m1/analyze", Some(body)).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(app.call("POST", "/api/meetings/m1/analyze", None).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(app.factory.prompts().is_empty());
}

#[tokio::test]
async fn absent_or_empty_meeting_when_analyzed_then_404_or_400() {
    let app = meeting();
    for (path, status, message) in [
        ("/api/meetings/absent/analyze", StatusCode::NOT_FOUND, "Meeting not found"),
        ("/api/meetings/empty/analyze", StatusCode::BAD_REQUEST, "Meeting has no transcript"),
        ("/api/meetings/absent/analyze-multi", StatusCode::NOT_FOUND, "Meeting not found"),
    ] {
        let (answer, body) = app.json("POST", path, Some("{}")).await;

        assert_eq!((answer, detail(&body)), (status, message.into()), "{path}");
    }
}

#[tokio::test]
async fn debug_context_when_analyzed_in_debug_then_its_segments_and_user_replace_the_meeting() {
    let app = meeting();
    app.json(
        "POST",
        "/api/debug/ai-context",
        Some(r#"{"segments": [{"ts_display": "1", "speaker": "D", "text": "debug text"}], "user_name": "Debugger"}"#),
    )
    .await;

    let (status, _) = app.json("POST", "/api/meetings/absent/analyze", Some(r#"{"debug": true}"#)).await;

    assert_eq!(status, 200);
    let prompt = &app.factory.prompts()[0];
    assert!(prompt.contains("[1] D: debug text") && prompt.contains("Debugger"), "{prompt}");
}

#[tokio::test]
async fn debug_requested_without_a_context_when_analyzed_then_the_meeting_is_used() {
    let app = meeting();

    let (status, _) = app.json("POST", "/api/meetings/m1/analyze", Some(r#"{"debug": true}"#)).await;

    assert_eq!(status, 200);
    assert!(app.factory.prompts()[0].contains("Берём синюю кнопку"));
}

#[tokio::test]
async fn debug_context_with_a_blank_user_when_analyzed_then_the_user_is_user() {
    let app = meeting();
    *app.state.debug_ai.lock().unwrap() = recog_web::server::DebugAiContext {
        segments: vec![obj(r#"{"ts_display": "1", "speaker": "D", "text": "debug text"}"#)],
        user_name: Some("  ".into()),
        ..Default::default()
    };
    let saved = |revision: &str| {
        format!(r#"{{"revision": "{revision}", "prompt": "[{{user_name}}] {{transcript}}", "providers": ["claude"]}}"#)
    };
    let (_, view) = app.json("GET", "/api/ai-settings", None).await;
    assert_eq!(
        app.call("PUT", "/api/ai-settings/tasks/analysis.increment", Some(&saved(&text(&view, "revision")))).await.0,
        200
    );

    app.json("POST", "/api/meetings/m1/analyze", Some(r#"{"debug": true}"#)).await;

    assert!(app.factory.prompts()[0].starts_with("[User] "), "{:?}", app.factory.prompts());
}

#[tokio::test]
async fn provider_failure_when_analyzed_then_500_with_its_error() {
    let app = meeting();
    app.factory.answer(Answer::Fail("model down".into()));

    let (status, body) = app.json("POST", "/api/meetings/m1/analyze", Some("{}")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(detail(&body).contains("model down"), "{body:?}");
}

#[tokio::test]
async fn metadata_of_the_wrong_shape_when_analyzed_then_500_with_the_python_error() {
    let app = meeting();
    write(
        &app.meetings().join("m1/meta.json"),
        r#"{"id": "m1", "created_at": "2026-09-01T10:00:00+00:00", "participants": []}"#,
    );

    let (status, body) = app.json("POST", "/api/meetings/m1/analyze", Some("{}")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!detail(&body).is_empty(), "{body:?}");
}

// --- analyze-multi ---

#[tokio::test]
async fn several_providers_when_analyzed_together_then_every_answer_by_key() {
    let app = meeting();

    let (status, body) = app.json("POST", "/api/meetings/m1/analyze-multi", Some("{}")).await;

    assert_eq!(status, 200);
    let responses = get(&body, "responses").as_object().unwrap();
    let keys: Vec<&String> = responses.keys().collect();
    assert_eq!(keys, ["claude", "codex"]);
    for answer in responses.values() {
        assert_eq!((text(answer, "answer"), get(answer, "error")), ("Синяя кнопка.".into(), &Value::Null));
    }
}

#[tokio::test]
async fn failing_providers_when_analyzed_together_then_each_carries_its_error() {
    let app = meeting();
    app.factory.answer(Answer::Fail("model down".into()));

    let (_, body) = app.json("POST", "/api/meetings/m1/analyze-multi", Some("{}")).await;

    let claude = get(get(&body, "responses"), "claude");
    assert_eq!(get(claude, "answer"), &Value::Null);
    assert!(text(claude, "error").contains("model down"));
}

#[tokio::test]
async fn provider_not_installed_when_analyzed_together_then_skipped_unless_the_task_is_customized() {
    let app = meeting();
    app.factory.missing.lock().unwrap().push("codex".into());

    let (_, body) = app.json("POST", "/api/meetings/m1/analyze-multi", Some("{}")).await;
    let keys: Vec<String> = get(&body, "responses").as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["claude"]);

    let (_, view) = app.json("GET", "/api/ai-settings", None).await;
    let saved = format!(
        r#"{{"revision": "{}", "prompt": "{{transcript}}", "providers": ["claude", "codex"]}}"#,
        text(&view, "revision")
    );
    assert_eq!(app.call("PUT", "/api/ai-settings/tasks/analysis.increment", Some(&saved)).await.0, 200);
    let (status, body) = app.json("POST", "/api/meetings/m1/analyze-multi", Some("{}")).await;
    assert_eq!((status, detail(&body)), (StatusCode::INTERNAL_SERVER_ERROR, "codex CLI not found".into()));
}

// --- analyze-stream ---

#[tokio::test]
async fn recorded_meeting_when_streamed_then_starts_each_answer_and_complete() {
    let app = meeting();

    let response = app.response("GET", "/api/meetings/m1/analyze-stream", &[]).await;

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream; charset=utf-8");
    assert_eq!(response.headers()["cache-control"], "no-cache");
    let (_, body) = app.call("GET", "/api/meetings/m1/analyze-stream", None).await;
    let events = events(&body);
    let mut middle = kinds(&events[2..events.len() - 1]);
    middle.sort();
    assert_eq!(kinds(&events[..2]), ["start:claude", "start:codex"]);
    assert_eq!(middle, ["done:claude", "done:codex"]);
    assert_eq!(text(&events[2], "text"), "Синяя кнопка.");
    assert_eq!(pyjson::dumps(events.last().unwrap(), None), r#"{"type": "complete"}"#);
}

#[tokio::test]
async fn failing_provider_when_streamed_then_its_error_event_and_complete() {
    let app = meeting();
    app.factory.answer(Answer::Fail("model down".into()));

    let (_, body) = app.call("GET", "/api/meetings/m1/analyze-stream", None).await;

    let events = events(&body);
    let errors: Vec<&Value> = events.iter().filter(|e| text(e, "type") == "error").collect();
    assert_eq!(errors.len(), 2);
    assert!(text(errors[0], "message").contains("model down"));
    assert_eq!(text(events.last().unwrap(), "type"), "complete");
}

#[tokio::test]
async fn refused_meeting_when_streamed_then_one_error_event_with_the_detail() {
    let app = meeting();
    for (path, message) in [
        ("/api/meetings/absent/analyze-stream", "Meeting not found"),
        ("/api/meetings/empty/analyze-stream", "Meeting has no transcript"),
    ] {
        let (status, body) = app.call("GET", path, None).await;

        assert_eq!(status, 200);
        assert_eq!(
            pyjson::dumps(&Value::Array(events(&body)), None),
            format!(r#"[{{"type": "error", "message": "{message}"}}]"#),
            "{path}"
        );
    }
}

#[tokio::test]
async fn metadata_of_the_wrong_shape_or_a_missing_cli_when_streamed_then_one_error_event() {
    let app = meeting();
    write(
        &app.meetings().join("m1/meta.json"),
        r#"{"id": "m1", "created_at": "2026-09-01T10:00:00+00:00", "participants": []}"#,
    );
    let (_, body) = app.call("GET", "/api/meetings/m1/analyze-stream", None).await;
    let events_seen = events(&body);
    assert_eq!(events_seen.len(), 1);
    assert_eq!(text(&events_seen[0], "message"), "'list' object has no attribute 'values'");

    let app = meeting();
    app.factory.missing.lock().unwrap().push("codex".into());
    let (_, view) = app.json("GET", "/api/ai-settings", None).await;
    let saved =
        format!(r#"{{"revision": "{}", "prompt": "{{transcript}}", "providers": ["codex"]}}"#, text(&view, "revision"));
    assert_eq!(app.call("PUT", "/api/ai-settings/tasks/analysis.increment", Some(&saved)).await.0, 200);
    let (_, body) = app.call("GET", "/api/meetings/m1/analyze-stream", None).await;
    assert_eq!(
        pyjson::dumps(&Value::Array(events(&body)), None),
        r#"[{"type": "error", "message": "codex CLI not found"}]"#
    );
}

#[tokio::test]
async fn invalid_query_when_streamed_then_422() {
    let app = meeting();
    for path in ["/api/meetings/m1/analyze-stream?question=custom", "/api/meetings/m1/analyze-stream?debug=xx"] {
        assert_eq!(app.call("GET", path, None).await.0, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
    }
}
