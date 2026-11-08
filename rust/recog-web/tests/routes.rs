//! The route cases of tests/test_audio_retention.py (which file a segment is cut from
//! and where the cut starts and ends), tests/test_frontend_static_cache.py,
//! tests/test_desktop_integration.py (the read-only status) and
//! tests/test_data_directory.py (the meetings directory under the data root).

mod support;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use http_body_util::BodyExt as _;
use recog_web::config::Config;
use support::{App, text, write};

/// An ffmpeg that records its argv and writes a tiny WAV to its last argument.
fn fake_ffmpeg(dir: &Path) -> (PathBuf, PathBuf) {
    let log = dir.join("ffmpeg-args");
    let script = dir.join("fake-ffmpeg");
    std::fs::write(&script, format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nfor a in \"$@\"; do last=\"$a\"; done\nprintf WAV > \"$last\"\n", log.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, log)
}

struct Call {
    app: App,
    log: PathBuf,
    _tools: tempfile::TempDir,
}

fn call_meeting(files: &[&str], rows: &[(i64, bool)]) -> Call {
    let tools = tempfile::tempdir().unwrap();
    let (script, log) = fake_ffmpeg(tools.path());
    let app = App::with(|config| config.ffmpeg = script.into_os_string());
    let meeting = app.meetings().join("call");
    write(&meeting.join("meta.json"), r#"{"id": "call", "created_at": "2026-09-01T10:00:00+00:00"}"#);
    let transcript: String = rows
        .iter()
        .map(|(offset, local)| {
            format!("{{\"text\": \"line\", \"audio_offset_ms\": {offset}, \"is_local\": {local}}}\n")
        })
        .collect();
    write(&meeting.join("transcript.jsonl"), &transcript);
    for name in files {
        write(&meeting.join(name), "AUDIO");
    }
    Call { app, log, _tools: tools }
}

fn argument(log: &Path, flag: &str) -> String {
    let args: Vec<String> = std::fs::read_to_string(log).unwrap().lines().map(str::to_owned).collect();
    let at = args.iter().position(|a| a == flag).unwrap();
    args[at + 1].clone()
}

#[tokio::test]
async fn call_with_only_a_mix_when_a_segment_is_played_then_the_mix_is_sliced_from_its_offset() {
    for local in [true, false] {
        let call = call_meeting(&["audio_mix.m4a"], &[(2000, local)]);

        let response = call.app.response("GET", "/api/meetings/call/audio/0", &[]).await;

        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "audio/wav");
        assert_eq!(response.headers()["content-disposition"], "inline; filename=\"segment_0.wav\"");
        assert_eq!(response.into_body().collect().await.unwrap().to_bytes().as_ref(), b"WAV");
        assert_eq!(argument(&call.log, "-i"), call.app.meetings().join("call/audio_mix.m4a").to_string_lossy());
        assert_eq!(argument(&call.log, "-ss"), "2.0");
    }
}

#[tokio::test]
async fn call_with_a_file_per_side_when_a_segment_is_played_then_its_side_or_the_other_is_sliced() {
    for (files, local, played) in [
        (&["audio_me.m4a", "audio_them.m4a"][..], true, "audio_me.m4a"),
        (&["audio_me.m4a", "audio_them.m4a"][..], false, "audio_them.m4a"),
        (&["audio_me.m4a"][..], false, "audio_me.m4a"),
        (&["audio_them.m4a"][..], true, "audio_them.m4a"),
    ] {
        let call = call_meeting(files, &[(2000, local)]);

        assert_eq!(call.app.call("GET", "/api/meetings/call/audio/0", None).await.0, 200);

        assert_eq!(
            argument(&call.log, "-i"),
            call.app.meetings().join("call").join(played).to_string_lossy(),
            "{files:?} {local}"
        );
    }
}

#[tokio::test]
async fn path_outside_the_meetings_when_a_segment_is_requested_then_refused_before_ffmpeg() {
    let call = call_meeting(&["audio_mix.m4a"], &[(0, false)]);
    write(&call.app.data().join("meta.json"), r#"{"id": "outside", "created_at": "2026-09-01T10:00:00+00:00"}"#);

    for path in ["/api/meetings/../audio/0", "/api/meetings/%2E%2E/audio/0"] {
        let (status, body) = call.app.json("GET", path, None).await;

        assert_eq!((status, text(&body, "detail")), (StatusCode::BAD_REQUEST, "Invalid meeting ID".into()), "{path}");
    }
    assert!(!call.log.exists());
}

#[tokio::test]
async fn reply_after_a_line_when_the_line_is_played_then_the_slice_stops_at_the_reply_only_on_a_mix() {
    for (files, seconds) in [(&["audio_mix.m4a"][..], "2.0"), (&["audio_me.m4a", "audio_them.m4a"][..], "12.0")] {
        let call = call_meeting(files, &[(0, true), (2000, false), (12000, true)]);

        call.app.call("GET", "/api/meetings/call/audio/0", None).await;

        assert_eq!(argument(&call.log, "-t"), seconds, "{files:?}");
    }
}

#[tokio::test]
async fn meeting_without_any_audio_when_a_segment_is_played_then_400_with_the_reason() {
    let call = call_meeting(&[], &[(0, true)]);
    let (status, body) = call.app.json("GET", "/api/meetings/call/audio/0", None).await;
    assert_eq!(
        (status, text(&body, "detail")),
        (StatusCode::BAD_REQUEST, "No audio file available for this meeting".into())
    );

    write(
        &call.app.meetings().join("call/meta.json"),
        r#"{"id": "call", "created_at": "2026-09-01T10:00:00+00:00", "audio_deleted_at": "2026-09-02T10:00:00"}"#,
    );
    let (_, body) = call.app.json("GET", "/api/meetings/call/audio/0", None).await;
    assert_eq!(text(&body, "detail"), "Audio expired (older than 24h retention)");
}

#[tokio::test]
async fn web_ui_files_when_served_then_the_browser_must_revalidate() {
    let app = App::new();
    for path in ["/", "/static/app.js", "/static/style.css"] {
        let response = app.response("GET", path, &[]).await;

        assert_eq!(response.status(), 200, "{path}");
        assert_eq!(response.headers()["cache-control"], "no-cache", "{path}");
    }
}

#[tokio::test]
async fn unchanged_script_when_revalidated_then_not_modified() {
    let app = App::new();
    let etag = app.response("GET", "/static/app.js", &[]).await.headers()["etag"].to_str().unwrap().to_owned();

    let response = app.response("GET", "/static/app.js", &[("If-None-Match", &etag)]).await;

    assert_eq!(response.status(), 304);
    assert_eq!(response.headers()["cache-control"], "no-cache");
}

#[tokio::test]
async fn desktop_status_when_requested_then_disabled_and_nothing_is_written() {
    let app = App::new();

    let (status, body) = app.json("GET", "/api/desktop/status", None).await;

    assert_eq!(status, 200);
    assert_eq!(text(&body, "state"), "disabled");
    assert!(!app.data().join("desktop").exists());
}

#[test]
fn data_root_override_when_the_server_is_configured_then_meetings_live_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let expected = dir.path().join("custom-root");
    let environ: HashMap<String, String> = [
        ("HOME".to_owned(), dir.path().to_string_lossy().into_owned()),
        ("RECOG_DATA_DIR".to_owned(), expected.to_string_lossy().into_owned()),
        ("RECOG_CONFIG_FILE".to_owned(), dir.path().join("none.toml").to_string_lossy().into_owned()),
    ]
    .into();

    let config = Config::from_environ(dir.path(), &environ, "127.0.0.1", 0).unwrap();

    assert_eq!(config.meetings_dir, expected.join("meetings"));
    assert_eq!(config.storage().unwrap().base_path(), expected.join("meetings"));
}
