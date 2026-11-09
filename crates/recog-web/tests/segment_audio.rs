//! `GET /api/meetings/{id}/audio/{segment_index}` beyond the phone-call files of
//! tests/routes.rs: the recorded tracks and their paths in the metadata, the track
//! override, the refusals of the index and the segment, and a failing ffmpeg.

mod support;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use support::{App, text, write};

/// An ffmpeg that records its argv and writes a tiny WAV to its last argument, or
/// fails with `stderr` when given.
fn fake_ffmpeg(dir: &Path, stderr: Option<&str>) -> (PathBuf, PathBuf) {
    let log = dir.join("ffmpeg-args");
    let script = dir.join("fake-ffmpeg");
    let tail = match stderr {
        Some(text) => format!("printf '%s' '{text}' >&2\nexit 1\n"),
        None => "for a in \"$@\"; do last=\"$a\"; done\nprintf WAV > \"$last\"\n".to_owned(),
    };
    std::fs::write(
        &script,
        format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n{tail}", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, log)
}

struct Recorded {
    app: App,
    log: PathBuf,
    _tools: tempfile::TempDir,
}

/// A recorded meeting `m` with these metadata fields, rows and files.
fn recorded(meta_extra: &str, rows: &[&str], files: &[&str], stderr: Option<&str>) -> Recorded {
    let tools = tempfile::tempdir().unwrap();
    let (script, log) = fake_ffmpeg(tools.path(), stderr);
    let app = App::with(|config| config.ffmpeg = script.into_os_string());
    let meeting = app.meetings().join("m");
    write(
        &meeting.join("meta.json"),
        &format!(r#"{{"id": "m", "created_at": "2026-09-01T10:00:00+00:00"{meta_extra}}}"#),
    );
    write(&meeting.join("transcript.jsonl"), &rows.iter().map(|r| format!("{r}\n")).collect::<String>());
    for name in files {
        write(&meeting.join(name), "AUDIO");
    }
    Recorded { app, log, _tools: tools }
}

fn argument(log: &Path, flag: &str) -> String {
    let args: Vec<String> = std::fs::read_to_string(log).unwrap().lines().map(str::to_owned).collect();
    let at = args.iter().position(|a| a == flag).unwrap();
    args[at + 1].clone()
}

const LOCAL: &str = r#"{"text": "me", "audio_offset_ms": 2500, "is_local": true}"#;
const REMOTE: &str = r#"{"text": "them", "audio_offset_ms": 4000, "is_local": false}"#;

#[tokio::test]
async fn live_tracks_when_a_segment_is_played_then_its_side_unless_the_track_is_named() {
    let call = recorded("", &[LOCAL, REMOTE], &["audio_mic.wav", "audio_system.wav"], None);
    let meeting = call.app.meetings().join("m");
    for (path, file, offset) in [
        ("/api/meetings/m/audio/0", "audio_mic.wav", "2.5"),
        ("/api/meetings/m/audio/1", "audio_system.wav", "4.0"),
        ("/api/meetings/m/audio/1?track=mic", "audio_mic.wav", "4.0"),
    ] {
        let (status, body) = call.app.call("GET", path, None).await;

        assert_eq!((status, body.as_slice()), (StatusCode::OK, &b"WAV"[..]), "{path}");
        assert_eq!(argument(&call.log, "-i"), meeting.join(file).to_string_lossy(), "{path}");
        assert_eq!(argument(&call.log, "-ss"), offset, "{path}");
        assert_eq!(argument(&call.log, "-ar"), "16000");
        assert_eq!(argument(&call.log, "-ac"), "1");
    }
}

#[tokio::test]
async fn paths_in_the_metadata_when_a_segment_is_played_then_the_existing_one_is_used() {
    let elsewhere = tempfile::tempdir().unwrap();
    let mic = elsewhere.path().join("mic.wav");
    std::fs::write(&mic, "AUDIO").unwrap();
    for (extra, files, expected) in [
        (format!(r#", "audio_mic_path": "{}""#, mic.display()), &["audio_system.wav"][..], mic.clone()),
        (
            r#", "audio_mic_path": "/nonexistent/mic.wav""#.to_owned(),
            &["audio_mic.wav"][..],
            PathBuf::from("audio_mic.wav"),
        ),
        (r#", "audio_mic_path": """#.to_owned(), &["audio_mic.wav"][..], PathBuf::from("audio_mic.wav")),
        (format!(r#", "audio_system_path": "{}""#, mic.display()), &[][..], mic.clone()),
        (String::new(), &["audio_system.wav"][..], PathBuf::from("audio_system.wav")),
    ] {
        let call = recorded(&extra, &[LOCAL], files, None);

        assert_eq!(call.app.call("GET", "/api/meetings/m/audio/0", None).await.0, 200, "{extra}");

        let expected = if expected.is_absolute() { expected } else { call.app.meetings().join("m").join(expected) };
        assert_eq!(argument(&call.log, "-i"), expected.to_string_lossy(), "{extra}");
    }
}

#[tokio::test]
async fn path_in_the_metadata_that_is_not_text_when_a_segment_is_played_then_plain_500() {
    // A WAV beside the meeting would replace the stored path when the meeting is read.
    let call = recorded(r#", "audio_mic_path": 5"#, &[LOCAL], &["audio_system.wav"], None);

    let (status, body) = call.app.call("GET", "/api/meetings/m/audio/0", None).await;

    assert_eq!((status, body.as_slice()), (StatusCode::INTERNAL_SERVER_ERROR, &b"Internal Server Error"[..]));
}

#[tokio::test]
async fn fractional_offset_when_a_segment_is_played_then_seconds_as_python_prints_them() {
    let call =
        recorded("", &[r#"{"text": "x", "audio_offset_ms": 1234.5, "is_local": true}"#], &["audio_mic.wav"], None);

    call.app.call("GET", "/api/meetings/m/audio/0", None).await;

    assert_eq!(argument(&call.log, "-ss"), "1.2345");
    assert_eq!(argument(&call.log, "-t"), "3.0");
}

#[tokio::test]
async fn index_outside_the_transcript_when_a_segment_is_played_then_400_or_422() {
    let call = recorded("", &[LOCAL], &["audio_mic.wav"], None);
    for path in ["/api/meetings/m/audio/1", "/api/meetings/m/audio/-1"] {
        let (status, body) = call.app.json("GET", path, None).await;

        assert_eq!(
            (status, text(&body, "detail")),
            (StatusCode::BAD_REQUEST, "Invalid segment index".into()),
            "{path}"
        );
    }
    assert_eq!(call.app.call("GET", "/api/meetings/m/audio/abc", None).await.0, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn segment_without_an_offset_when_played_then_400_or_plain_500_for_another_type() {
    let call = recorded(
        "",
        &[
            r#"{"text": "old"}"#,
            r#"{"text": "x", "audio_offset_ms": null}"#,
            r#"{"text": "y", "audio_offset_ms": "5"}"#,
            r#"{"text": "z", "audio_offset_ms": true}"#,
        ],
        &["audio_mic.wav"],
        None,
    );
    for index in [0, 1] {
        let (status, body) = call.app.json("GET", &format!("/api/meetings/m/audio/{index}"), None).await;

        assert_eq!(
            (status, text(&body, "detail")),
            (StatusCode::BAD_REQUEST, "Segment has no audio data (recorded before audio feature)".into())
        );
    }
    assert_eq!(call.app.call("GET", "/api/meetings/m/audio/2", None).await.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(call.app.call("GET", "/api/meetings/m/audio/3", None).await.0, StatusCode::OK);
    assert_eq!(argument(&call.log, "-ss"), "0.001");
}

#[tokio::test]
async fn absent_meeting_when_a_segment_is_played_then_404() {
    let call = recorded("", &[LOCAL], &[], None);

    let (status, body) = call.app.json("GET", "/api/meetings/absent/audio/0", None).await;

    assert_eq!((status, text(&body, "detail")), (StatusCode::NOT_FOUND, "Meeting not found".into()));
}

#[tokio::test]
async fn failing_ffmpeg_when_a_segment_is_played_then_500_with_the_head_of_its_error() {
    let long = "e".repeat(300);
    let call = recorded("", &[LOCAL], &["audio_mic.wav"], Some(&long));

    let (status, body) = call.app.json("GET", "/api/meetings/m/audio/0", None).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(text(&body, "detail"), format!("Audio extraction failed: {}", "e".repeat(200)));
}
