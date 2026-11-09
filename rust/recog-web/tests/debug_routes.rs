//! The live status and debug routes: `/api/live/status`, `/api/pcm-diagnostics`,
//! `/api/debug` over the bridge's debug file, and the debug AI context
//! (`/api/debug/ai-context`) chosen by meeting, title, IDs, start time or window.

mod support;

use axum::http::StatusCode;
use recog_core::pyjson::{self, Value};
use support::{App, get, text, write};

fn app() -> App {
    App::with(|config| config.ktalk_debug_file = config.data_dir.join("ktalk_debug.json"))
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

fn debug_file(app: &App, text: &str) {
    write(&app.state.config.ktalk_debug_file, text);
}

fn ids(value: &Value) -> Vec<i128> {
    match get(value, "segments") {
        Value::Array(rows) => rows
            .iter()
            .map(|row| match get(row, "id") {
                Value::Int(n) => *n,
                other => panic!("id {other:?}"),
            })
            .collect(),
        other => panic!("segments {other:?}"),
    }
}

/// A recorded meeting with a local participant and four timed rows.
fn review(app: &App) {
    let meeting = app.meetings().join("m1");
    write(
        &meeting.join("meta.json"),
        r#"{"id": "m1", "title": "Обзор дизайна", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "2026-09-01T11:00:00+00:00", "participants": {"alice": {"name": "Алиса", "is_local": true}, "bob": {"name": "Bob"}}}"#,
    );
    let rows: String = [("10:00:00", 1), ("10:00:30", 2), ("10:01:00", 3), ("10:01:20", 4)]
        .iter()
        .map(|(ts, id)| {
            format!(
                "{{\"id\": {id}, \"ts\": \"2026-09-01T{ts}+00:00\", \"ts_display\": \"{ts}\", \"speaker\": \"Bob\", \"text\": \"row {id}\"}}\n"
            )
        })
        .collect();
    write(&meeting.join("transcript.jsonl"), &rows);
}

// --- /api/live/status ---

#[tokio::test]
async fn one_meeting_without_an_end_when_live_status_is_asked_then_it_is_current() {
    let app = app();
    review(&app);
    write(
        &app.meetings().join("live/meta.json"),
        r#"{"id": "live", "created_at": "2026-11-08T09:00:00+00:00", "ended_at": null, "title": "Идёт"}"#,
    );

    let (status, body) = app.json("GET", "/api/live/status", None).await;

    assert_eq!(status, 200);
    assert_eq!(get(&body, "active"), &Value::Bool(true));
    assert_eq!(text(get(&body, "current_meeting"), "id"), "live");
    assert!(matches!(get(&body, "active_meetings"), Value::Array(rows) if rows.len() == 1));
}

#[tokio::test]
async fn only_ended_meetings_when_live_status_is_asked_then_inactive_without_current() {
    let app = app();
    review(&app);

    let (_, body) = app.json("GET", "/api/live/status", None).await;

    assert_eq!(pyjson::dumps(&body, None), r#"{"active": false, "current_meeting": null, "active_meetings": []}"#);
}

// --- /api/pcm-diagnostics ---

#[tokio::test]
async fn no_debug_file_when_pcm_diagnostics_are_asked_then_503_naming_the_live_service() {
    let (status, body) = app().json("GET", "/api/pcm-diagnostics", None).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(text(&body, "detail"), "ktalk_debug.json missing — is com.recog.live running?");
}

#[tokio::test]
async fn fresh_debug_file_when_pcm_diagnostics_are_asked_then_its_sections_and_not_stale() {
    let app = app();
    debug_file(
        &app,
        &format!(
            r#"{{"timestamp": {}, "meeting": {{"id": "m-debug"}}, "conference": {{"participants": [{{"id": "p1"}}]}}, "pcm": {{"transport_enabled": true, "chunks": 3}}, "pcm_transport": {{"clients": 1}}}}"#,
            now()
        ),
    );

    let (status, body) = app.json("GET", "/api/pcm-diagnostics", None).await;

    assert_eq!(status, 200);
    assert_eq!(get(&body, "stale"), &Value::Bool(false));
    assert!(matches!(get(&body, "state_age_s"), Value::Float(age) if (0.0..5.0).contains(age)));
    assert_eq!(text(&body, "meeting_id"), "m-debug");
    assert_eq!(pyjson::dumps(get(&body, "participants"), None), r#"[{"id": "p1"}]"#);
    assert_eq!(pyjson::dumps(get(&body, "pcm"), None), r#"{"transport_enabled": true, "chunks": 3}"#);
    assert_eq!(pyjson::dumps(get(&body, "pcm_transport"), None), r#"{"clients": 1}"#);
}

#[tokio::test]
async fn old_debug_file_without_sections_when_pcm_diagnostics_are_asked_then_stale_with_defaults() {
    let app = app();
    debug_file(&app, r#"{"timestamp": 1000, "pcm_transport": null}"#);

    let (status, body) = app.json("GET", "/api/pcm-diagnostics", None).await;

    assert_eq!(status, 200);
    assert_eq!(get(&body, "stale"), &Value::Bool(true));
    assert_eq!(get(&body, "meeting_id"), &Value::Null);
    assert_eq!(pyjson::dumps(get(&body, "participants"), None), "[]");
    assert_eq!(pyjson::dumps(get(&body, "pcm"), None), r#"{"transport_enabled": false}"#);
    assert_eq!(pyjson::dumps(get(&body, "pcm_transport"), None), "{}");
}

#[tokio::test]
async fn unreadable_debug_file_when_pcm_diagnostics_are_asked_then_500_read_failed() {
    let app = app();
    debug_file(&app, "{broken");

    let (status, body) = app.json("GET", "/api/pcm-diagnostics", None).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(text(&body, "detail").starts_with("read failed: "), "{body:?}");
}

#[tokio::test]
async fn debug_file_of_the_wrong_shape_when_pcm_diagnostics_are_asked_then_plain_500() {
    for content in [r"[1]", r#"{"timestamp": "soon"}"#, r#"{"timestamp": 1, "meeting": 5}"#] {
        let app = app();
        debug_file(&app, content);

        let (status, bytes) = app.call("GET", "/api/pcm-diagnostics", None).await;

        assert_eq!((status, bytes.as_slice()), (StatusCode::INTERNAL_SERVER_ERROR, &b"Internal Server Error"[..]));
    }
}

// --- /api/debug ---

#[tokio::test]
async fn no_debug_file_when_debug_is_asked_then_bridge_state_null_and_stale_with_counts() {
    let app = app();
    review(&app);
    write(&app.meetings().join("live/meta.json"), r#"{"id": "live", "created_at": "2026-11-08T09:00:00+00:00"}"#);
    // Ended meetings are listed from the index.
    write(
        &app.meetings().join("index.json"),
        r#"{"meetings": [{"id": "m1", "created_at": "2026-09-01T10:00:00+00:00"}, {"id": "archived", "created_at": "2026-01-01T10:00:00+00:00"}]}"#,
    );

    let (status, body) = app.json("GET", "/api/debug", None).await;

    assert_eq!(status, 200);
    let bridge = get(&body, "ktalk_bridge");
    assert_eq!(get(bridge, "state"), &Value::Null);
    assert_eq!(get(bridge, "stale"), &Value::Bool(true));
    assert_eq!(text(bridge, "debug_file"), app.state.config.ktalk_debug_file.to_string_lossy());
    let meetings = get(&body, "meetings");
    assert_eq!((get(meetings, "active_count"), get(meetings, "total_count")), (&Value::Int(1), &Value::Int(3)));
    let keys: Vec<&String> = meetings.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["active", "active_count", "total_count"]);
    assert_eq!(pyjson::dumps(get(&body, "server"), None), r#"{"host": "127.0.0.1", "port": 0}"#);
    assert!(matches!(get(&body, "timestamp"), Value::Float(t) if *t > 1.0e9));
}

#[tokio::test]
async fn fresh_debug_file_when_debug_is_asked_then_its_state_with_age_and_not_stale() {
    let app = app();
    debug_file(&app, &format!(r#"{{"timestamp": {}, "meeting": {{"id": "m-debug"}}}}"#, now()));

    let (_, body) = app.json("GET", "/api/debug", None).await;

    let bridge = get(&body, "ktalk_bridge");
    assert_eq!(get(bridge, "stale"), &Value::Bool(false));
    assert_eq!(text(get(get(bridge, "state"), "meeting"), "id"), "m-debug");
    assert!(matches!(get(get(bridge, "state"), "age_seconds"), Value::Float(age) if (0.0..5.0).contains(age)));
}

#[tokio::test]
async fn debug_file_without_a_usable_timestamp_when_debug_is_asked_then_state_shown_and_stale() {
    for (content, state) in [
        (r#"{"meeting": {"id": "x"}}"#, r#"{"meeting": {"id": "x"}}"#),
        (r#"{"timestamp": "soon"}"#, r#"{"error": "unsupported operand type(s) for -: 'float' and 'str'"}"#),
        ("[1, 2]", "[1, 2]"),
    ] {
        let app = app();
        debug_file(&app, content);

        let (_, body) = app.json("GET", "/api/debug", None).await;

        let bridge = get(&body, "ktalk_bridge");
        assert_eq!(pyjson::dumps(get(bridge, "state"), None), state, "{content}");
        assert_eq!(get(bridge, "stale"), &Value::Bool(true), "{content}");
    }
}

#[tokio::test]
async fn unreadable_debug_file_when_debug_is_asked_then_its_error_is_the_state() {
    let app = app();
    debug_file(&app, "{broken");

    let (status, body) = app.json("GET", "/api/debug", None).await;

    assert_eq!(status, 200);
    let error = text(get(get(&body, "ktalk_bridge"), "state"), "error");
    assert!(error.starts_with("Expecting property name enclosed in double quotes"), "{error}");
}

// --- /api/debug/ai-context ---

#[tokio::test]
async fn nothing_set_when_the_debug_context_is_read_then_inactive_and_empty() {
    let (status, body) = app().json("GET", "/api/debug/ai-context?include_segments=true", None).await;

    assert_eq!(status, 200);
    assert_eq!(
        pyjson::dumps(&body, None),
        r#"{"active": false, "meeting_id": null, "meeting_title": null, "segment_count": 0, "user_name": null, "set_at": null}"#
    );
}

#[tokio::test]
async fn unparsable_query_when_the_debug_context_is_read_then_422_for_each_parameter() {
    let (status, body) = app().json("GET", "/api/debug/ai-context?limit=x&include_segments=2", None).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let locs: Vec<String> = match get(&body, "detail") {
        Value::Array(items) => items.iter().map(|i| pyjson::dumps(get(i, "loc"), None)).collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(locs, [r#"["query", "include_segments"]"#, r#"["query", "limit"]"#]);
}

#[tokio::test]
async fn ids_of_a_meeting_when_the_debug_context_is_set_then_those_rows_and_the_local_name() {
    let app = app();
    review(&app);

    let (status, body) =
        app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "m1", "from_id": 2, "to_id": 3}"#)).await;

    assert_eq!(status, 200);
    assert_eq!(get(&body, "active"), &Value::Bool(true));
    assert_eq!(
        (text(&body, "meeting_id"), text(&body, "meeting_title"), text(&body, "user_name")),
        ("m1".into(), "Обзор дизайна".into(), "Алиса".into())
    );
    assert_eq!(get(&body, "segment_count"), &Value::Int(2));
    let (_, read) = app.json("GET", "/api/debug/ai-context?include_segments=yes&limit=1", None).await;
    assert_eq!(ids(&read), [2]);
    assert_eq!(get(&read, "segment_count"), &Value::Int(2));
    let (_, read) = app.json("GET", "/api/debug/ai-context?include_segments=1", None).await;
    assert_eq!(ids(&read), [2, 3]);
}

#[tokio::test]
async fn only_a_lower_id_when_the_debug_context_is_set_then_rows_from_it_on() {
    let app = app();
    review(&app);

    let (_, body) = app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "m1", "from_id": 3}"#)).await;
    assert_eq!(get(&body, "segment_count"), &Value::Int(2));
    let (_, body) = app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "m1", "to_id": 1}"#)).await;
    assert_eq!(get(&body, "segment_count"), &Value::Int(1));
}

#[tokio::test]
async fn start_time_when_the_debug_context_is_set_then_rows_from_it_at_least_five() {
    let app = app();
    review(&app);

    let (status, body) = app
        .json(
            "POST",
            "/api/debug/ai-context",
            Some(r#"{"meeting_id": "m1", "start_ts_display": "10:00:30", "max_segments": 2}"#),
        )
        .await;

    assert_eq!(status, 200);
    let (_, read) = app.json("GET", "/api/debug/ai-context?include_segments=true", None).await;
    assert_eq!((get(&body, "segment_count"), ids(&read)), (&Value::Int(3), vec![2, 3, 4]));
}

#[tokio::test]
async fn unknown_start_time_when_the_debug_context_is_set_then_400_nothing_selected() {
    let app = app();
    review(&app);

    let (status, body) = app
        .json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "m1", "start_ts_display": "99:99:99"}"#))
        .await;

    assert_eq!(
        (status, text(&body, "detail")),
        (StatusCode::BAD_REQUEST, "No segments selected for debug context".into())
    );
}

#[tokio::test]
async fn window_when_the_debug_context_is_set_then_rows_within_at_least_thirty_seconds_of_the_last() {
    let app = app();
    review(&app);

    app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "m1", "window_seconds": 10}"#)).await;

    let (_, read) = app.json("GET", "/api/debug/ai-context?include_segments=true", None).await;
    assert_eq!(ids(&read), [3, 4]);
}

#[tokio::test]
async fn few_max_segments_when_the_debug_context_is_set_by_window_then_the_last_five() {
    let app = app();
    let meeting = app.meetings().join("long");
    write(&meeting.join("meta.json"), r#"{"id": "long", "created_at": "2026-09-01T10:00:00+00:00"}"#);
    let rows: String = (1..=7).map(|id| format!("{{\"id\": {id}, \"text\": \"t\"}}\n\n")).collect();
    write(&meeting.join("transcript.jsonl"), &rows);

    app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "long", "max_segments": 1}"#)).await;

    let (_, read) = app.json("GET", "/api/debug/ai-context?include_segments=true", None).await;
    assert_eq!(ids(&read), [3, 4, 5, 6, 7]);
    assert_eq!(text(&read, "user_name"), "User");
}

#[tokio::test]
async fn meeting_without_a_transcript_file_when_the_debug_context_is_set_by_window_then_400() {
    let app = app();
    write(&app.meetings().join("bare/meta.json"), r#"{"id": "bare", "created_at": "2026-09-01T10:00:00+00:00"}"#);

    let (status, body) = app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "bare"}"#)).await;

    assert_eq!(
        (status, text(&body, "detail")),
        (StatusCode::BAD_REQUEST, "No segments selected for debug context".into())
    );
}

#[tokio::test]
async fn part_of_a_title_when_the_debug_context_is_set_then_that_meeting_is_found() {
    let app = app();
    review(&app);

    let (status, body) = app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_title": "обзор"}"#)).await;

    assert_eq!(status, 200);
    assert_eq!((text(&body, "meeting_id"), text(&body, "meeting_title")), ("m1".into(), "Обзор дизайна".into()));
}

#[tokio::test]
async fn neither_meeting_nor_segments_when_the_debug_context_is_set_then_400() {
    let app = app();
    review(&app);
    for body in ["{}", r#"{"meeting_title": "Nothing like it"}"#, r#"{"meeting_id": "", "meeting_title": ""}"#] {
        let (status, answer) = app.json("POST", "/api/debug/ai-context", Some(body)).await;

        assert_eq!(
            (status, text(&answer, "detail")),
            (StatusCode::BAD_REQUEST, "meeting_id or segments required".into()),
            "{body}"
        );
    }
}

#[tokio::test]
async fn absent_meeting_when_the_debug_context_is_set_then_404() {
    let (status, body) = app().json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "absent"}"#)).await;

    assert_eq!((status, text(&body, "detail")), (StatusCode::NOT_FOUND, "Meeting not found".into()));
}

#[tokio::test]
async fn fields_of_the_wrong_type_when_the_debug_context_is_set_then_422() {
    let (status, body) = app()
        .json("POST", "/api/debug/ai-context", Some(r#"{"max_segments": "x", "segments": [1], "from_id": 1.5}"#))
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(matches!(get(&body, "detail"), Value::Array(items) if items.len() == 3), "{body:?}");
}

#[tokio::test]
async fn given_segments_when_the_debug_context_is_set_then_they_are_kept_with_the_given_names() {
    let app = app();
    let rows: Vec<String> = (1..=7).map(|id| format!(r#"{{"id": {id}, "text": "debug"}}"#)).collect();
    let body = format!(
        r#"{{"segments": [{}], "user_name": "Debugger", "meeting_title": "T", "max_segments": 6}}"#,
        rows.join(", ")
    );

    let (status, answer) = app.json("POST", "/api/debug/ai-context", Some(&body)).await;

    assert_eq!(status, 200);
    assert_eq!(get(&answer, "meeting_id"), &Value::Null);
    assert_eq!((text(&answer, "meeting_title"), text(&answer, "user_name")), ("T".into(), "Debugger".into()));
    assert_eq!(get(&answer, "segment_count"), &Value::Int(6));
    let (_, read) = app.json("GET", "/api/debug/ai-context", None).await;
    assert_eq!(text(&read, "set_at"), text(&answer, "set_at"));
}

#[tokio::test]
async fn earlier_name_when_segments_are_set_without_one_then_it_is_kept_else_user() {
    let app = app();
    let (_, first) = app.json("POST", "/api/debug/ai-context", Some(r#"{"segments": [{"id": 1}]}"#)).await;
    assert_eq!(text(&first, "user_name"), "User");
    app.json("POST", "/api/debug/ai-context", Some(r#"{"segments": [{"id": 1}], "user_name": "Ann"}"#)).await;

    let (_, again) =
        app.json("POST", "/api/debug/ai-context", Some(r#"{"segments": [{"id": 2}], "user_name": ""}"#)).await;

    assert_eq!(text(&again, "user_name"), "Ann");
}

#[tokio::test]
async fn meeting_whose_local_participants_are_unnamed_when_the_debug_context_is_set_then_empty_name() {
    let app = app();
    review(&app);
    write(
        &app.meetings().join("m1/meta.json"),
        r#"{"id": "m1", "title": "Обзор дизайна", "created_at": "2026-09-01T10:00:00+00:00", "participants": {"me": {"is_local": true}}}"#,
    );

    let (_, body) = app.json("POST", "/api/debug/ai-context", Some(r#"{"meeting_id": "m1"}"#)).await;

    assert_eq!(text(&body, "user_name"), "");
}

#[tokio::test]
async fn context_set_when_cleared_then_inactive_again() {
    let app = app();
    app.json("POST", "/api/debug/ai-context", Some(r#"{"segments": [{"id": 1}]}"#)).await;

    let (status, body) = app.json("DELETE", "/api/debug/ai-context", None).await;

    assert_eq!((status, pyjson::dumps(&body, None)), (StatusCode::OK, r#"{"active": false}"#.to_owned()));
    let (_, read) = app.json("GET", "/api/debug/ai-context", None).await;
    assert_eq!(get(&read, "active"), &Value::Bool(false));
}
