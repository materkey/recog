//! `pcm_chunk` frames the extension's worklet could send malformed: each field
//! missing or of a type Python's `int()`/`float()`/`b64decode` refuses, with the exact
//! log line Python printed, the values Python coerced, and the one error (an
//! `OverflowError`) that escapes the handler (the steps of the former differential
//! scenario `pcm_chunks.json`).

mod common;

use std::sync::Arc;

use common::{Harness, field};
use recog_bridge::testing::{FakeStore, RecordingLive};
use recog_core::pyjson::Value;

fn rig() -> (Harness, Arc<FakeStore>, Arc<RecordingLive>) {
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    h.bridge.set_meeting_storage(storage.clone());
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    (h, storage, live)
}

/// A `pcm_chunk` frame with these fields (JSON text, Python's NaN allowed).
fn pcm(fields: &str) -> String {
    format!(r#"{{"type": "pcm_chunk", {fields}}}"#)
}

const VALID: &str = r#""pid": "p1", "seq": 5, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000"#;

fn chunks(live: &RecordingLive) -> Vec<Value> {
    live.calls()
        .into_iter()
        .filter(|c| field(c, "call").as_str() == Some("pcm_chunk"))
        .map(|c| field(&c, "chunk").clone())
        .collect()
}

fn malformed(reason: &str) -> String {
    format!("[KTalk Bridge] Malformed pcm_chunk frame, skipped: {reason}")
}

#[tokio::test]
async fn given_frames_missing_a_field_when_they_arrive_then_logged_with_the_key_and_skipped() {
    let cases = [
        (r#""seq": 1, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000, "pcm_base64": "AAAA""#, "'pid'"),
        (r#""pid": "p1", "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000, "pcm_base64": "AAAA""#, "'seq'"),
        (r#""pid": "p1", "seq": 1, "chunk_end_ms": 2, "sample_rate": 16000, "pcm_base64": "AAAA""#, "'chunk_start_ms'"),
        (r#""pid": "p1", "seq": 1, "chunk_start_ms": 1, "sample_rate": 16000, "pcm_base64": "AAAA""#, "'chunk_end_ms'"),
        (r#""pid": "p1", "seq": 1, "chunk_start_ms": 1, "chunk_end_ms": 2, "pcm_base64": "AAAA""#, "'sample_rate'"),
        (r#""pid": "p1", "seq": 1, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000"#, "'pcm_base64'"),
    ];
    for (fields, key) in cases {
        // given
        let (h, _storage, live) = rig();
        // when
        let result = h.process(&pcm(fields), None).await;
        // then
        assert_eq!(result, Ok(()), "{key}");
        assert!(h.logged(&malformed(key)), "{key}: {:?}", h.logs());
        assert!(chunks(&live).is_empty(), "{key}");
    }
}

#[tokio::test]
async fn given_fields_python_cannot_convert_when_they_arrive_then_logged_with_python_text_and_skipped() {
    let rest = r#""pcm_base64": "AAAA""#;
    let cases = [
        (r#""pid": "", "seq": 1, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000"#, "pid is empty"),
        (
            r#""pid": "p1", "seq": NaN, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000"#,
            "cannot convert float NaN to integer",
        ),
        (
            r#""pid": "p1", "seq": null, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000"#,
            "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'",
        ),
        (
            r#""pid": "p1", "seq": "x", "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000"#,
            "invalid literal for int() with base 10: 'x'",
        ),
        (
            r#""pid": "p1", "seq": 1, "chunk_start_ms": "soon", "chunk_end_ms": 2, "sample_rate": 16000"#,
            "could not convert string to float: 'soon'",
        ),
        (
            r#""pid": "p1", "seq": 1, "chunk_start_ms": [1], "chunk_end_ms": 2, "sample_rate": 16000"#,
            "float() argument must be a string or a real number, not 'list'",
        ),
        (
            r#""pid": "p1", "seq": 1, "chunk_start_ms": 1, "chunk_end_ms": {}, "sample_rate": 16000"#,
            "float() argument must be a string or a real number, not 'dict'",
        ),
        (
            r#""pid": "p1", "seq": 1, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": [16000]"#,
            "int() argument must be a string, a bytes-like object or a real number, not 'list'",
        ),
    ];
    for (fields, reason) in cases {
        // given
        let (h, _storage, live) = rig();
        // when
        let result = h.process(&pcm(&format!("{fields}, {rest}")), None).await;
        // then
        assert_eq!(result, Ok(()), "{reason}");
        assert!(h.logged(&malformed(reason)), "{reason}: {:?}", h.logs());
        assert!(chunks(&live).is_empty(), "{reason}");
    }
}

#[tokio::test]
async fn given_infinite_seq_when_chunk_arrives_then_overflow_error_escapes_the_handler() {
    // given
    let (h, _storage, live) = rig();
    // when
    let result = h
        .process(
            &pcm(r#""pid": "p1", "seq": Infinity, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000, "pcm_base64": "AAAA""#),
            None,
        )
        .await;
    // then
    assert_eq!(result, Err("cannot convert float infinity to integer".to_owned()));
    assert!(chunks(&live).is_empty());
    assert!(!h.logged("Malformed pcm_chunk"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_payload_that_is_not_a_string_when_chunk_arrives_then_type_error_escapes_the_handler() {
    // given
    let (h, _storage, live) = rig();
    // when
    let result = h.process(&pcm(&format!(r#"{VALID}, "pcm_base64": 12345"#)), None).await;
    // then
    assert_eq!(result, Err("argument should be a bytes-like object or ASCII string, not 'int'".to_owned()));
    assert!(chunks(&live).is_empty());
}

#[tokio::test]
async fn given_bad_base64_when_chunk_arrives_then_decode_failure_logged_with_pid_and_seq() {
    let cases = [
        ("AA=A", "Discontinuous padding not allowed"),
        ("ЖЖЖЖ", "string argument should contain only ASCII characters"),
        ("A", "Invalid base64-encoded string: number of data characters (1) cannot be 1 more than a multiple of 4"),
    ];
    for (payload, reason) in cases {
        // given
        let (h, _storage, live) = rig();
        // when
        h.process(&pcm(&format!(r#"{VALID}, "pcm_base64": "{payload}""#)), None).await.unwrap();
        // then
        let line = format!("[KTalk Bridge] pcm_chunk base64 decode failed (pid=p1, seq=5): {reason}");
        assert!(h.logged(&line), "{payload}: {:?}", h.logs());
        assert!(chunks(&live).is_empty());
    }
}

#[tokio::test]
async fn given_values_python_coerces_when_chunk_arrives_then_dispatched_as_ints() {
    // given
    let (h, _storage, live) = rig();
    // when
    let fields = r#""pid": 42, "speaker_name": null, "seq": "3", "chunk_start_ms": "1500", "chunk_end_ms": 2500,
        "sample_rate": 16000.9, "pcm_base64": "//8AAA==""#;
    h.process(&pcm(fields), None).await.unwrap();
    h.process(
        &pcm(r#""pid": "p1", "seq": true, "chunk_start_ms": 1, "chunk_end_ms": 2, "sample_rate": 16000, "pcm_base64": "AAA=""#),
        None,
    )
    .await
    .unwrap();
    // then
    let received = chunks(&live);
    assert_eq!(received.len(), 2, "{:?}", h.logs());
    assert_eq!(field(&received[0], "pid").as_str(), Some("42"));
    assert_eq!(field(&received[0], "speaker_name").as_str(), Some(""));
    assert_eq!(field(&received[0], "seq"), &Value::Int(3));
    assert_eq!(field(&received[0], "sample_rate"), &Value::Int(16000));
    assert_eq!(field(&received[0], "chunk_start_ts"), &Value::Float(1.5));
    assert_eq!(field(&received[0], "pcm").as_str(), Some("ffff0000"));
    assert_eq!(field(&received[1], "seq"), &Value::Int(1));
    assert_eq!(field(&received[1], "pcm").as_str(), Some("0000"));
}

#[tokio::test]
async fn given_sibling_meeting_chunks_when_they_keep_coming_then_drop_logged_once_per_10_s_per_pid() {
    // given
    let (h, storage, live) = rig();
    *storage.current_id.lock().unwrap() = Some("current".into());
    let sibling = pcm(&format!(r#"{VALID}, "pcm_base64": "AAAAAA==", "meeting_id": "sibling""#));
    let line = "[KTalk Bridge] pcm_chunk cross-meeting dropped (pid=p1, seq=5, chunk=sibling, current=current)";
    let count = |h: &Harness| h.logs().iter().filter(|l| l.as_str() == line).count();
    // when
    h.process(&sibling, None).await.unwrap();
    h.process(&sibling, None).await.unwrap();
    // then
    assert_eq!(count(&h), 1, "{:?}", h.logs());
    // when: past the 10 s throttle
    h.clock.advance(11.0);
    h.process(&sibling, None).await.unwrap();
    // then
    assert_eq!(count(&h), 2, "{:?}", h.logs());
    assert!(chunks(&live).is_empty());
}

#[tokio::test]
async fn given_live_side_not_wanting_pcm_when_valid_chunk_arrives_then_not_handed_over() {
    // given
    let (h, _storage, live) = rig();
    live.wants_pcm.store(false, std::sync::atomic::Ordering::SeqCst);
    // when
    h.process(&pcm(&format!(r#"{VALID}, "pcm_base64": "AAAAAA==""#)), None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert!(h.logs().iter().all(|l| !l.contains("pcm_chunk")), "{:?}", h.logs());
}
