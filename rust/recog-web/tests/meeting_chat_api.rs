//! The cases of tests/test_meeting_chat.py that go through the HTTP API: questions
//! about a recorded meeting answered by a fake provider, the saved dialog, its limits
//! and failures.

mod support;

use recog_ai::json::Json;
use recog_core::pyjson::Value;
use support::{Answer, App, get, text, write};

const ANSWER: &str = "Пётр за синюю кнопку [11:30:14].";

fn chat() -> App {
    chat_with(|_| {})
}

fn chat_with(change: impl FnOnce(&mut recog_web::config::Config)) -> App {
    let app = App::with(change);
    app.factory.answer(Answer::Text(ANSWER.into()));
    let meeting = app.meetings().join("m1");
    write(
        &meeting.join("meta.json"),
        r#"{"id": "m1", "title": "Обзор дизайна", "created_at": "2026-11-02T08:30:08+00:00", "participants": {"a": {"name": "Peter"}, "b": {"name": "Мария", "is_local": true}}}"#,
    );
    write(
        &meeting.join("transcript.jsonl"),
        concat!(
            r#"{"id": 1, "ts": "2026-11-02T08:30:14+00:00", "ts_display": "11:30:14", "speaker": "Peter", "speaker_id": "a", "text": "Я за синюю кнопку."}"#,
            "\n",
            r#"{"id": 2, "ts": "2026-11-02T08:31:00+00:00", "ts_display": "11:31:00", "speaker": "Мария", "speaker_id": "b", "text": "Понял."}"#,
            "\n"
        ),
    );
    write(&app.meetings().join("empty/meta.json"), r#"{"id": "empty", "created_at": "2026-11-02T08:30:08+00:00"}"#);
    write(&app.meetings().join("empty/transcript.jsonl"), "");
    app
}

async fn ask(app: &App, question: &str, meeting: &str) -> (axum::http::StatusCode, Value) {
    let body = recog_core::pyjson::dumps(
        &support::obj(&format!("{{\"question\": {}}}", recog_core::pyjson::dumps(&Value::from(question), None))),
        None,
    );
    app.json("POST", &format!("/api/meetings/{meeting}/chat"), Some(&body)).await
}

fn questions(body: &Value) -> Vec<String> {
    match body {
        Value::Array(turns) => turns.iter().map(|t| text(t, "question")).collect(),
        _ => panic!("{body:?}"),
    }
}

#[tokio::test]
async fn question_about_a_meeting_when_asked_then_the_model_sees_the_transcript_and_the_question() {
    let app = chat();

    let (status, turn) = ask(&app, "Что сказал Пётр?", "m1").await;

    assert_eq!(status, 200);
    assert_eq!(text(&turn, "question"), "Что сказал Пётр?");
    assert_eq!(text(&turn, "answer"), ANSWER);
    assert_eq!(text(&turn, "provider"), "Fake (model-x)");
    assert_eq!(get(&turn, "transcript_truncated"), &Value::Bool(false));
    let prompt = &app.factory.prompts()[0];
    assert!(prompt.contains("[11:30:14] Peter: Я за синюю кнопку."));
    assert!(prompt.contains("Обзор дизайна") && prompt.contains("Мария"));
    assert!(prompt.trim_end().ends_with("Что сказал Пётр?"));
    assert!(prompt.find("Я за синюю кнопку.").unwrap() < prompt.find("Что сказал Пётр?").unwrap());
}

#[tokio::test]
async fn answered_question_when_the_meeting_is_reopened_then_the_dialog_is_still_there() {
    let app = chat();
    ask(&app, "Что сказал Пётр?", "m1").await;

    let (status, body) = app.json("GET", "/api/meetings/m1/chat", None).await;

    assert_eq!(status, 200);
    assert_eq!(questions(&body), ["Что сказал Пётр?"]);
}

#[tokio::test]
async fn follow_up_question_when_asked_then_the_model_sees_the_earlier_dialog_before_it() {
    let app = chat();
    ask(&app, "Что сказал Пётр?", "m1").await;
    app.factory.answer(Answer::Text("Да.".into()));

    ask(&app, "А он уверен?", "m1").await;

    let prompt = &app.factory.prompts()[1];
    assert!(prompt.find(ANSWER).unwrap() < prompt.find("А он уверен?").unwrap());
    assert!(prompt.contains("Что сказал Пётр?"));
}

#[tokio::test]
async fn long_dialog_when_asked_then_only_recent_turns_reach_the_model() {
    let app = chat();
    for number in 0..10 {
        ask(&app, &format!("Вопрос номер {number}."), "m1").await;
    }

    ask(&app, "Последний", "m1").await;

    let prompt = app.factory.prompts().last().cloned().unwrap();
    assert!(!prompt.contains("Вопрос номер 1."));
    assert!(prompt.contains("Вопрос номер 2.") && prompt.contains("Вопрос номер 9."));
}

#[tokio::test]
async fn failed_model_call_when_asked_then_502_and_nothing_stored() {
    let app = chat();
    app.factory.answer(Answer::Fail("Claude CLI error: limit".into()));

    let (status, body) = ask(&app, "Что сказал Пётр?", "m1").await;

    assert_eq!(status, 502);
    assert!(text(&body, "detail").contains("limit"));
    assert_eq!(app.json("GET", "/api/meetings/m1/chat", None).await.1, Value::Array(Vec::new()));
}

#[tokio::test]
async fn empty_model_answer_when_asked_then_it_counts_as_a_failure() {
    let app = chat();
    app.factory.answer(Answer::Text("  \n".into()));

    let (status, _) = ask(&app, "Что сказал Пётр?", "m1").await;

    assert_eq!(status, 502);
    assert_eq!(app.json("GET", "/api/meetings/m1/chat", None).await.1, Value::Array(Vec::new()));
}

#[tokio::test]
async fn meeting_deleted_during_the_answer_when_finished_then_404_and_it_is_not_recreated() {
    let app = chat();
    let dir = app.meetings().join("m1");
    let doomed = dir.clone();
    *app.factory.before_answer.lock().unwrap() = Some(Box::new(move || {
        let _ = std::fs::remove_dir_all(&doomed);
    }));

    let (status, _) = ask(&app, "Что сказал Пётр?", "m1").await;

    assert_eq!(status, 404);
    assert!(!dir.exists());
}

#[tokio::test]
async fn transcript_over_the_limit_when_asked_then_the_turn_says_it_was_cut() {
    let app = chat_with(|config| config.chat_transcript_max_chars = 20);

    let (_, turn) = ask(&app, "Что сказал Пётр?", "m1").await;

    assert_eq!(get(&turn, "transcript_truncated"), &Value::Bool(true));
    assert!(app.factory.prompts()[0].contains("Понял."));
}

#[tokio::test]
async fn new_conversation_when_requested_then_the_dialog_is_gone() {
    let app = chat();
    ask(&app, "Что сказал Пётр?", "m1").await;

    assert_eq!(app.call("DELETE", "/api/meetings/m1/chat", None).await.0, 200);
    assert_eq!(app.call("DELETE", "/api/meetings/m1/chat", None).await.0, 200);

    assert_eq!(app.json("GET", "/api/meetings/m1/chat", None).await.1, Value::Array(Vec::new()));
    ask(&app, "Снова", "m1").await;
    assert!(!app.factory.prompts().last().unwrap().contains("Что сказал Пётр?"));
}

#[tokio::test]
async fn damaged_lines_when_reading_the_dialog_then_the_rest_survives() {
    let app = chat();
    ask(&app, "Что сказал Пётр?", "m1").await;
    let path = app.meetings().join("m1/chat.jsonl");
    let fields = r#""asked_at": "t", "provider": "p", "settings_revision": "r", "transcript_truncated": false"#;
    let mut text_ = std::fs::read_to_string(&path).unwrap();
    text_.push_str("{\"question\": \"оборв\n[1, 2]\n{\"question\": \"нет ответа\"}\n");
    text_.push_str(&format!("{{{fields}, \"question\": \"без ответа\", \"answer\": null}}\n"));
    text_.push_str(&format!("{{{fields}, \"question\": null, \"answer\": \"без вопроса\"}}\n"));
    std::fs::write(&path, text_).unwrap();
    ask(&app, "Второй", "m1").await;

    let turns = app.json("GET", "/api/meetings/m1/chat", None).await.1;

    assert_eq!(questions(&turns), ["Что сказал Пётр?", "Второй"]);
}

#[tokio::test]
async fn unusable_question_when_asked_then_rejected_before_the_model() {
    for question in ["", "   \n", &"x".repeat(4001)] {
        let app = chat();

        let (status, _) = ask(&app, question, "m1").await;

        assert_eq!(status, 422);
        assert!(app.factory.prompts().is_empty());
    }
}

#[tokio::test]
async fn unknown_meeting_when_the_chat_is_opened_then_not_found() {
    for meeting in ["absent", "..%5Cm1"] {
        for method in ["GET", "DELETE"] {
            let app = chat();

            let (status, _) = app.call(method, &format!("/api/meetings/{meeting}/chat"), None).await;

            assert!(status == 400 || status == 404, "{method} {meeting}: {status}");
        }
    }
}

#[tokio::test]
async fn meeting_without_speech_when_asked_then_400_without_the_model() {
    let app = chat();

    let (status, _) = ask(&app, "Есть кто?", "empty").await;

    assert_eq!(status, 400);
    assert!(app.factory.prompts().is_empty());
}

#[tokio::test]
async fn edited_prompt_and_model_when_asked_then_the_chat_uses_them() {
    let app = chat();
    let store = recog_ai::settings::AiSettingsStore::new(app.data(), Some(app.state.config.prompts_dir.clone()));
    store
        .write_task(
            "meeting_chat",
            &Json::parse(r#"{"prompt": "Мой промпт\n{transcript}", "models": {"claude": "other"}}"#).unwrap(),
            None,
        )
        .unwrap();

    let (_, turn) = ask(&app, "Что сказал Пётр?", "m1").await;

    assert!(app.factory.prompts()[0].starts_with("Мой промпт"));
    let seen = app.factory.settings.lock().unwrap().last().cloned().unwrap();
    assert_eq!(seen.providers, ["claude"]);
    assert_eq!(seen.models.get("claude").map(String::as_str), Some("other"));
    assert_eq!(text(&turn, "settings_revision"), seen.revision);
}

#[tokio::test]
async fn saved_dialog_when_searched_on_disk_then_the_russian_text_is_readable() {
    let app = chat();

    ask(&app, "Что сказал Пётр?", "m1").await;

    assert!(std::fs::read_to_string(app.meetings().join("m1/chat.jsonl")).unwrap().contains("Что сказал Пётр?"));
}
