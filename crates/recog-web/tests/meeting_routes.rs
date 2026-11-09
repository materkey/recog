//! The meeting routes: the list with its source labels, the search over the
//! transcripts, one meeting's metadata and transcript, and the deletion with its
//! refusals.

mod support;

use axum::http::StatusCode;
use recog_core::pyjson::{self, Value};
use support::{App, get, text, write};

/// An ended meeting in the index, with a transcript; `rows` are `(speaker, text)`.
fn ended(app: &App, id: &str, title: &str, created: &str, rows: &[(&str, &str)]) {
    let meeting = app.meetings().join(id);
    write(
        &meeting.join("meta.json"),
        &format!(
            r#"{{"id": "{id}", "title": "{title}", "created_at": "{created}", "ended_at": "{created}", "source": "importer"}}"#
        ),
    );
    let lines: String = rows
        .iter()
        .enumerate()
        .map(|(i, (speaker, text))| {
            format!(
                "{{\"id\": {}, \"ts_display\": \"10:00:0{i}\", \"speaker\": \"{speaker}\", \"text\": \"{text}\"}}\n",
                i + 1
            )
        })
        .collect();
    write(&meeting.join("transcript.jsonl"), &lines);
}

fn index(app: &App, entries: &[&str]) {
    write(&app.meetings().join("index.json"), &format!(r#"{{"meetings": [{}]}}"#, entries.join(", ")));
}

fn labelled() -> App {
    App::with(|config| config.install.source_labels = vec![("importer".to_owned(), "Phone Importer".to_owned())])
}

fn ids(body: &Value, key: &str) -> Vec<String> {
    match body {
        Value::Array(rows) => rows.iter().map(|row| text(row, key)).collect(),
        other => panic!("{other:?}"),
    }
}

// --- /api/meetings ---

#[tokio::test]
async fn meetings_of_a_labelled_source_when_listed_then_newest_first_with_the_label() {
    let app = labelled();
    index(
        &app,
        &[
            r#"{"id": "old", "created_at": "2026-01-01T10:00:00+00:00", "source": "importer"}"#,
            r#"{"id": "new", "created_at": "2026-02-01T10:00:00+00:00", "source": "ktalk"}"#,
            r#"{"id": "nosource", "created_at": "2026-01-15T10:00:00+00:00"}"#,
        ],
    );

    let (status, body) = app.json("GET", "/api/meetings", None).await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body, "id"), ["new", "nosource", "old"]);
    let Value::Array(rows) = &body else { unreachable!() };
    assert_eq!(get(&rows[2], "source_label"), &Value::from("Phone Importer"));
    assert_eq!(get(&rows[0], "source_label"), &Value::Null);
    assert_eq!(get(&rows[1], "source_label"), &Value::Null);
}

#[tokio::test]
async fn index_of_the_wrong_shape_when_listed_then_500_with_the_error() {
    let app = App::new();
    write(&app.meetings().join("index.json"), r#"{"meetings": 5}"#);

    let (status, body) = app.json("GET", "/api/meetings", None).await;

    assert_eq!(
        (status, text(&body, "detail")),
        (StatusCode::INTERNAL_SERVER_ERROR, "'int' object is not iterable".into())
    );
}

// --- /api/search ---

fn searchable() -> App {
    let app = App::new();
    ended(
        &app,
        "m1",
        "Обзор дизайна",
        "2026-09-01T10:00:00+00:00",
        &[("Алиса", "Привет, начнём"), ("Bob", "Hello there")],
    );
    ended(
        &app,
        "m2",
        "",
        "2026-09-02T10:00:00+00:00",
        &[
            ("A", "привет 1"),
            ("A", "привет 2"),
            ("A", "привет 3"),
            ("A", "ПРИВЕТ 4"),
            ("A", "привет 5"),
            ("A", "привет 6"),
        ],
    );
    index(
        &app,
        &[
            r#"{"id": "m1", "title": "Обзор дизайна", "created_at": "2026-09-01T10:00:00+00:00", "source": "ktalk"}"#,
            r#"{"id": "m2", "title": null, "created_at": "2026-09-02T10:00:00+00:00"}"#,
            r#"{"id": "archived-only", "created_at": "2026-01-01T00:00:00+00:00", "title": "Archive"}"#,
        ],
    );
    app
}

#[tokio::test]
async fn query_in_any_case_when_searched_then_meetings_with_their_first_five_matches() {
    let app = searchable();

    let (status, body) = app.json("GET", "/api/search?q=%D0%9F%D0%A0%D0%98%D0%92%D0%95%D0%A2", None).await;

    assert_eq!(status, 200);
    assert_eq!(ids(&body, "meeting_id"), ["m2", "m1"]);
    let Value::Array(results) = &body else { unreachable!() };
    assert_eq!(text(&results[0], "title"), "m2");
    assert!(matches!(get(&results[0], "matches"), Value::Array(m) if m.len() == 5));
    assert_eq!(
        pyjson::dumps(&results[1], None),
        r#"{"meeting_id": "m1", "title": "Обзор дизайна", "created_at": "2026-09-01T10:00:00+00:00", "source": "ktalk", "matches": [{"ts_display": "10:00:00", "speaker": "Алиса", "text": "Привет, начнём"}]}"#
    );
}

#[tokio::test]
async fn short_or_missing_query_when_searched_then_nothing() {
    let app = searchable();
    for path in ["/api/search", "/api/search?q=h", "/api/search?q=zzz"] {
        let (status, body) = app.json("GET", path, None).await;

        assert_eq!((status, pyjson::dumps(&body, None)), (StatusCode::OK, "[]".to_owned()), "{path}");
    }
}

#[tokio::test]
async fn row_without_text_or_with_other_fields_missing_when_searched_then_it_does_not_match() {
    let app = App::new();
    write(
        &app.meetings().join("m/meta.json"),
        r#"{"id": "m", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "x"}"#,
    );
    write(&app.meetings().join("m/transcript.jsonl"), "{\"id\": 1}\n{\"id\": 2, \"text\": \"hello\"}\n");
    index(&app, &[r#"{"id": "m", "created_at": "2026-09-01T10:00:00+00:00"}"#]);

    let (_, body) = app.json("GET", "/api/search?q=hello", None).await;

    assert_eq!(
        pyjson::dumps(&body, None),
        r#"[{"meeting_id": "m", "title": "m", "created_at": "2026-09-01T10:00:00+00:00", "source": null, "matches": [{"ts_display": null, "speaker": null, "text": "hello"}]}]"#
    );
}

#[tokio::test]
async fn text_that_is_not_a_string_when_searched_then_500_with_the_python_error() {
    let app = App::new();
    write(
        &app.meetings().join("m/meta.json"),
        r#"{"id": "m", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "x"}"#,
    );
    write(&app.meetings().join("m/transcript.jsonl"), "{\"id\": 1, \"text\": 5}\n");
    index(&app, &[r#"{"id": "m", "created_at": "2026-09-01T10:00:00+00:00"}"#]);

    let (status, body) = app.json("GET", "/api/search?q=hello", None).await;

    assert_eq!(
        (status, text(&body, "detail")),
        (StatusCode::INTERNAL_SERVER_ERROR, "'int' object has no attribute 'lower'".into())
    );
}

#[tokio::test]
async fn index_entry_without_an_id_when_searched_then_500_key_error() {
    let app = App::new();
    index(&app, &[r#"{"created_at": "2026-09-01T10:00:00+00:00"}"#]);

    let (status, body) = app.json("GET", "/api/search?q=hello", None).await;

    assert_eq!((status, text(&body, "detail")), (StatusCode::INTERNAL_SERVER_ERROR, "'id'".into()));
}

// --- /api/meetings/{id} and its transcript ---

#[tokio::test]
async fn stored_meeting_when_read_then_its_metadata_and_its_rows() {
    let app = searchable();

    let (status, meta) = app.json("GET", "/api/meetings/m1", None).await;
    assert_eq!((status, text(&meta, "title")), (StatusCode::OK, "Обзор дизайна".into()));
    let (status, rows) = app.json("GET", "/api/meetings/m1/transcript", None).await;
    assert_eq!(
        (status, ids(&rows, "text")),
        (StatusCode::OK, vec!["Привет, начнём".to_owned(), "Hello there".to_owned()])
    );
}

#[tokio::test]
async fn absent_meeting_when_read_then_404() {
    let app = App::new();
    for path in ["/api/meetings/absent", "/api/meetings/absent/transcript"] {
        let (status, body) = app.json("GET", path, None).await;

        assert_eq!((status, text(&body, "detail")), (StatusCode::NOT_FOUND, "Meeting not found".into()), "{path}");
    }
}

// --- DELETE /api/meetings/{id} ---

#[tokio::test]
async fn ended_meeting_when_deleted_then_gone_from_disk_index_and_live_watcher() {
    let app = searchable();
    app.state.live_watcher.set_current_meeting(Some("m1"), None);

    let (status, body) = app.json("DELETE", "/api/meetings/m1", None).await;

    assert_eq!((status, pyjson::dumps(&body, None)), (StatusCode::OK, r#"{"success": true}"#.to_owned()));
    assert!(!app.meetings().join("m1").exists());
    assert_eq!(app.state.live_watcher.current_meeting_id(), None);
    let (_, list) = app.json("GET", "/api/meetings", None).await;
    assert_eq!(ids(&list, "id"), ["m2", "archived-only"]);
    let (status, _) = app.json("DELETE", "/api/meetings/m1", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn meeting_other_than_the_live_one_when_deleted_then_the_live_one_stays_current() {
    let app = searchable();
    app.state.live_watcher.set_current_meeting(Some("m2"), None);

    app.json("DELETE", "/api/meetings/m1", None).await;

    assert_eq!(app.state.live_watcher.current_meeting_id().as_deref(), Some("m2"));
}

#[tokio::test]
async fn active_meeting_when_deleted_then_409_and_kept() {
    let app = App::new();
    write(&app.meetings().join("live/meta.json"), r#"{"id": "live", "created_at": "2026-11-08T09:00:00+00:00"}"#);

    let (status, body) = app.json("DELETE", "/api/meetings/live", None).await;

    assert_eq!((status, text(&body, "detail")), (StatusCode::CONFLICT, "Cannot delete an active meeting".into()));
    assert!(app.meetings().join("live/meta.json").exists());
}

#[tokio::test]
async fn path_like_or_unreadable_meeting_when_deleted_then_400_409_or_500() {
    let app = App::new();
    let (status, body) = app.json("DELETE", "/api/meetings/..%5Cx", None).await;
    assert_eq!((status, text(&body, "detail")), (StatusCode::BAD_REQUEST, "Invalid meeting ID".into()));

    // A JSON decode error is a ValueError in Python, answered like the refusal.
    write(&app.meetings().join("broken/meta.json"), "{broken");
    let (status, _) = app.json("DELETE", "/api/meetings/broken", None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    ended(&app, "m", "T", "2026-09-01T10:00:00+00:00", &[]);
    write(&app.meetings().join("index.json"), r#"{"meetings": 5}"#);
    let (status, body) = app.json("DELETE", "/api/meetings/m", None).await;
    assert_eq!(
        (status, text(&body, "detail")),
        (StatusCode::INTERNAL_SERVER_ERROR, "'int' object is not iterable".into())
    );
}
