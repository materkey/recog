//! The increment and title routes: listing and reading the saved increments,
//! generating one (forced, for a participant, skipped, failed) and the title of a
//! meeting, with every refusal of their checks.

mod support;

use axum::http::StatusCode;
use recog_core::pyjson::{self, Value};
use support::{Answer, App, get, text, write};

const META: &str = r#"{"id": "m1", "title": "Обзор дизайна", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "2026-09-01T11:00:00+00:00", "participants": {"alice": {"name": "Алиса", "is_local": true}, "bob": {"name": "Bob"}}}"#;

fn recorded(app: &App, meta: &str) {
    let meeting = app.meetings().join("m1");
    write(&meeting.join("meta.json"), meta);
    write(
        &meeting.join("transcript.jsonl"),
        concat!(
            r#"{"id": 1, "ts_display": "10:00:05", "speaker": "Алиса", "speaker_id": "alice", "text": "Привет, начнём обзор синей кнопки и круглых углов"}"#,
            "\n",
            r#"{"id": 2, "ts_display": "10:00:09", "speaker": "Bob", "speaker_id": "bob", "text": "I prefer the blue button with round corners"}"#,
            "\n",
        ),
    );
}

fn meeting() -> App {
    let app = App::new();
    app.factory.answer(Answer::Text("Инкремент: синяя кнопка.".into()));
    recorded(&app, META);
    app
}

const GENERATE: &str = "/api/meetings/m1/increments/my-increment/generate";

fn detail(body: &Value) -> String {
    text(body, "detail")
}

// --- Generation ---

#[tokio::test]
async fn recorded_meeting_when_an_increment_is_generated_then_version_one_with_its_text() {
    let app = meeting();

    let (status, body) = app.json("POST", GENERATE, None).await;

    assert_eq!(status, 200);
    assert_eq!((text(&body, "status"), get(&body, "version")), ("generated".into(), &Value::Int(1)));
    let data = get(&body, "data");
    assert!(text(data, "content").ends_with("Инкремент: синяя кнопка."), "{data:?}");
    assert_eq!(text(get(data, "meta"), "status"), "completed");
    assert_eq!(text(get(data, "meta"), "participant_name"), "Алиса");
    assert!(app.factory.prompts()[0].contains("Участник: Алиса"));
    assert!(app.meetings().join("m1/increments/my-increment/v1.md").exists());
}

#[tokio::test]
async fn nothing_new_when_generated_again_then_skipped_unless_forced() {
    let app = meeting();
    app.json("POST", GENERATE, None).await;

    let (status, body) = app.json("POST", GENERATE, None).await;
    assert_eq!(
        (status, pyjson::dumps(&body, None)),
        (StatusCode::OK, r#"{"status": "skipped", "message": "No new segments to process"}"#.to_owned())
    );

    let (_, body) = app.json("POST", &format!("{GENERATE}?force=true"), None).await;
    assert_eq!(get(&body, "version"), &Value::Int(2));
}

#[tokio::test]
async fn known_participant_when_generated_then_the_increment_is_theirs() {
    let app = meeting();

    let (status, body) = app.json("POST", &format!("{GENERATE}?participant_id=bob"), None).await;

    assert_eq!(status, 200);
    let meta = get(get(&body, "data"), "meta");
    assert_eq!((text(meta, "participant_id"), text(meta, "participant_name")), ("bob".into(), "Bob".into()));
}

#[tokio::test]
async fn unknown_participant_when_generated_then_400_before_any_prompt() {
    let app = meeting();

    let (status, body) = app.json("POST", &format!("{GENERATE}?participant_id=nobody"), None).await;

    assert_eq!((status, detail(&body)), (StatusCode::BAD_REQUEST, "Participant not found".into()));
    assert!(app.factory.prompts().is_empty());
}

#[tokio::test]
async fn participants_as_a_list_or_a_text_when_a_participant_is_asked_then_looked_up_in_them() {
    for (participants, known) in [(r#"["bob"]"#, true), (r#"["al"]"#, false), (r#""bobby""#, true), (r#""al""#, false)]
    {
        let app = App::new();
        recorded(
            &app,
            &format!(r#"{{"id": "m1", "created_at": "2026-09-01T10:00:00+00:00", "participants": {participants}}}"#),
        );

        let (status, body) = app.json("POST", &format!("{GENERATE}?participant_id=bob"), None).await;

        if known {
            assert_ne!(detail(&body), "Participant not found", "{participants}");
        } else {
            assert_eq!(
                (status, detail(&body)),
                (StatusCode::BAD_REQUEST, "Participant not found".into()),
                "{participants}"
            );
        }
    }
}

#[tokio::test]
async fn participants_of_another_type_or_meta_not_a_dict_when_a_participant_is_asked_then_plain_500() {
    let app = App::new();
    recorded(&app, r#"{"id": "m1", "created_at": "2026-09-01T10:00:00+00:00", "participants": 5}"#);

    let (status, bytes) = app.call("POST", &format!("{GENERATE}?participant_id=bob"), None).await;

    assert_eq!((status, bytes.as_slice()), (StatusCode::INTERNAL_SERVER_ERROR, &b"Internal Server Error"[..]));
}

#[tokio::test]
async fn provider_failure_when_generated_then_500_with_its_error_and_a_failed_version() {
    let app = meeting();
    app.factory.answer(Answer::Fail("the model is down".into()));

    let (status, body) = app.json("POST", GENERATE, None).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(detail(&body).contains("the model is down"), "{body:?}");
    let (_, list) = app.json("GET", "/api/meetings/m1/increments/my-increment", None).await;
    let Value::Array(metas) = &list else { panic!("{list:?}") };
    assert_eq!(text(&metas[0], "status"), "failed");
}

#[tokio::test]
async fn unparsable_force_when_generated_then_422() {
    let (status, body) = meeting().json("POST", &format!("{GENERATE}?force=maybe"), None).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let Value::Array(items) = get(&body, "detail") else { panic!("{body:?}") };
    assert_eq!(items.len(), 1);
    assert_eq!(pyjson::dumps(get(&items[0], "loc"), None), r#"["query", "force"]"#);
}

#[tokio::test]
async fn path_like_ids_when_generated_then_400_naming_which() {
    let app = meeting();
    for (path, message) in [
        ("/api/meetings/..%5Cx/increments/my-increment/generate", "Invalid meeting ID"),
        ("/api/meetings/m1/increments/..%5Cp/generate", "Invalid prompt ID"),
    ] {
        let (status, body) = app.json("POST", path, None).await;

        assert_eq!((status, detail(&body)), (StatusCode::BAD_REQUEST, message.into()), "{path}");
    }
}

#[tokio::test]
async fn unknown_meeting_or_prompt_when_generated_then_404() {
    let app = meeting();
    for (path, message) in [
        ("/api/meetings/absent/increments/my-increment/generate", "Meeting not found"),
        ("/api/meetings/m1/increments/no-such-prompt/generate", "Prompt 'no-such-prompt' not found"),
    ] {
        let (status, body) = app.json("POST", path, None).await;

        assert_eq!((status, detail(&body)), (StatusCode::NOT_FOUND, message.into()), "{path}");
    }
}

#[tokio::test]
async fn meeting_of_an_external_directory_when_generated_then_404_no_directory() {
    let app = App::with(|config| config.external_paths = vec![(config.data_dir.with_file_name("external"), None)]);
    let external = app.data().with_file_name("external").join("ext");
    write(&external.join("meta.json"), r#"{"id": "ext", "created_at": "2026-07-01T12:00:00+00:00"}"#);
    write(&external.join("transcript.jsonl"), "{\"id\": 1, \"text\": \"imported\"}\n");

    let (status, body) = app.json("POST", "/api/meetings/ext/increments/my-increment/generate", None).await;

    assert_eq!((status, detail(&body)), (StatusCode::NOT_FOUND, "Meeting directory not found".into()));
}

// --- Listing and reading ---

#[tokio::test]
async fn generated_increment_when_listed_and_read_then_its_metadata_and_content() {
    let app = meeting();
    let (_, empty) = app.json("GET", "/api/meetings/m1/increments", None).await;
    assert_eq!(pyjson::dumps(&empty, None), "{}");
    app.json("POST", GENERATE, None).await;

    let (status, all) = app.json("GET", "/api/meetings/m1/increments", None).await;
    assert_eq!(status, 200);
    assert!(matches!(get(&all, "my-increment"), Value::Array(metas) if metas.len() == 1), "{all:?}");

    let (_, one) = app.json("GET", "/api/meetings/m1/increments/my-increment", None).await;
    assert!(matches!(&one, Value::Array(metas) if text(&metas[0], "status") == "completed"), "{one:?}");

    let (status, version) = app.json("GET", "/api/meetings/m1/increments/my-increment/1", None).await;
    assert_eq!(status, 200);
    assert_eq!(get(get(&version, "meta"), "version"), &Value::Int(1));
    assert!(text(&version, "content").ends_with("Инкремент: синяя кнопка."));
}

#[tokio::test]
async fn missing_or_unparsable_version_when_read_then_404_or_422() {
    let app = meeting();

    let (status, body) = app.json("GET", "/api/meetings/m1/increments/my-increment/9", None).await;
    assert_eq!((status, detail(&body)), (StatusCode::NOT_FOUND, "Increment my-increment/v9 not found".into()));
    let (status, _) = app.json("GET", "/api/meetings/m1/increments/my-increment/abc", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn path_like_or_unknown_meeting_when_increments_are_listed_then_400_or_404() {
    let app = meeting();
    for (path, status, message) in [
        ("/api/meetings/..%5Cx/increments", StatusCode::BAD_REQUEST, "Invalid meeting ID"),
        ("/api/meetings/m1/increments/..%5Cp", StatusCode::BAD_REQUEST, "Invalid prompt ID"),
        ("/api/meetings/m1/increments/..%5Cp/1", StatusCode::BAD_REQUEST, "Invalid prompt ID"),
        ("/api/meetings/absent/increments", StatusCode::NOT_FOUND, "Meeting not found"),
        ("/api/meetings/absent/increments/my-increment/1", StatusCode::NOT_FOUND, "Meeting not found"),
    ] {
        let (answer, body) = app.json("GET", path, None).await;

        assert_eq!((answer, detail(&body)), (status, message.into()), "{path}");
    }
}

// --- Titles ---

#[tokio::test]
async fn recorded_meeting_when_a_title_is_generated_then_stored_with_its_provider() {
    let app = meeting();
    app.factory.answer(Answer::Text("Синяя кнопка".into()));

    let (status, body) = app.json("POST", "/api/meetings/m1/generate-title", None).await;

    assert_eq!(status, 200);
    assert_eq!(pyjson::dumps(&body, None), r#"{"meeting_id": "m1", "title": "Синяя кнопка", "title_source": "codex"}"#);
    let (_, meta) = app.json("GET", "/api/meetings/m1", None).await;
    assert_eq!((text(&meta, "title"), text(&meta, "title_source")), ("Синяя кнопка".into(), "codex".into()));
    let prompt = &app.factory.prompts()[0];
    assert!(prompt.contains("[10:00:05] Алиса: Привет"), "{prompt}");
}

#[tokio::test]
async fn meeting_without_rows_or_text_when_a_title_is_generated_then_400() {
    for (rows, message) in
        [("", "Meeting has no transcript"), ("{\"id\": 1, \"text\": \"\"}\n", "Meeting has no transcript text")]
    {
        let app = meeting();
        write(&app.meetings().join("m1/transcript.jsonl"), rows);

        let (status, body) = app.json("POST", "/api/meetings/m1/generate-title", None).await;

        assert_eq!((status, detail(&body)), (StatusCode::BAD_REQUEST, message.into()), "{rows:?}");
    }
}

#[tokio::test]
async fn provider_without_an_answer_when_a_title_is_generated_then_500_and_title_unchanged() {
    let app = meeting();
    app.factory.answer(Answer::Fail("down".into()));

    let (status, body) = app.json("POST", "/api/meetings/m1/generate-title", None).await;

    assert_eq!((status, detail(&body)), (StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate title".into()));
    let (_, meta) = app.json("GET", "/api/meetings/m1", None).await;
    assert_eq!(text(&meta, "title"), "Обзор дизайна");
}

#[tokio::test]
async fn absent_meeting_when_a_title_is_generated_then_404() {
    let (status, body) = meeting().json("POST", "/api/meetings/absent/generate-title", None).await;

    assert_eq!((status, detail(&body)), (StatusCode::NOT_FOUND, "Meeting not found".into()));
}

#[tokio::test]
async fn unnamed_local_participant_when_a_title_is_generated_then_the_user_is_user() {
    let app = App::new();
    app.factory.answer(Answer::Text("T".into()));
    recorded(
        &app,
        r#"{"id": "m1", "created_at": "2026-09-01T10:00:00+00:00", "participants": {"x": {"is_local": true}}}"#,
    );
    let saved = r#"{"revision": "R", "prompt": "{user_name}: {transcript}", "providers": ["claude"]}"#;
    let (_, view) = app.json("GET", "/api/ai-settings", None).await;
    let saved = saved.replace("\"R\"", &format!("\"{}\"", text(&view, "revision")));
    assert_eq!(app.call("PUT", "/api/ai-settings/tasks/title", Some(&saved)).await.0, 200);

    let (status, _) = app.json("POST", "/api/meetings/m1/generate-title", None).await;

    assert_eq!(status, 200);
    assert!(app.factory.prompts()[0].starts_with("User: "), "{:?}", app.factory.prompts());
}
