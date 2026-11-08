//! The Rust remote backend against `recog/remote_asr.py`, both over one fake gateway.
//!
//! The test runs an HTTP server on an ephemeral port that answers every request from
//! a plan: the clip's first sample names it, and each name has a queue of answers
//! (text with words, null text, 503/404, a body that is not JSON, a list, words whose
//! times cannot be read, string times, a reply slower than the live timeout, health
//! answers of both gateway forms). Seeded random scenarios and hand-written ones
//! (timeouts, the health probe, a recovery probe) drive `FallbackASR` and
//! `FallbackAsr` through chunks, per-speaker batches and refine windows while the
//! clock moves past the failure window, the open window and the local-load retry, and
//! the fake local model's background load ends in success or failure when the
//! scenario says. Every step's answer (text, unavailable, error), `available` after
//! it, the `[FallbackASR]` log lines (the gateway's error text cut off) and the
//! requests the gateway saw (method, path, multipart part headers and values, the
//! WAV's format chunk, sample count and samples' SHA-256) must be equal.
//!
//! The scenarios and the gateway live in tests/remote_asr_support. With
//! `RECOG_RECORD_REMOTE_ASR_FIXTURE=1` the test also writes Python's answers and
//! requests to tests/fixtures/remote_asr_python.json, which tests/asr_remote_frozen.rs
//! replays without Python.

mod common;
mod remote_asr_support;

use std::time::Duration;

use remote_asr_support::{Gateway, assert_every_state_reached, fixture_path, run_rust, scenarios, scenarios_sha256};
use serde_json::{Value as Json, json};

#[test]
fn scripted_gateway_driving_both_fallback_backends_then_steps_logs_and_requests_are_the_same() {
    let Some(python) = common::python() else { return };
    let scenarios = scenarios();
    let gateway = Gateway::start();
    let sandbox = tempfile::tempdir().unwrap();

    gateway.reset(&scenarios);
    let expected = common::run_python(
        &python,
        "remote_asr.py",
        &json!({"url": gateway.url, "scenarios": scenarios}),
        sandbox.path(),
    );
    // The batch clips Python stopped waiting for after a failure have arrived by now.
    std::thread::sleep(Duration::from_millis(300));
    let python_requests = gateway.records();
    if std::env::var_os("RECOG_RECORD_REMOTE_ASR_FIXTURE").is_some() {
        let fixture = json!({
            "scenarios_sha256": scenarios_sha256(&scenarios),
            "answers": expected,
            "requests": python_requests,
        });
        std::fs::write(fixture_path(), format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap())).unwrap();
    }

    gateway.reset(&scenarios);
    let got: Vec<Json> = scenarios.iter().enumerate().map(|(i, s)| run_rust(&gateway.url, i, s)).collect();
    let rust_requests = gateway.records();

    for (i, (want, have)) in expected.as_array().unwrap().iter().zip(&got).enumerate() {
        assert_eq!(have, want, "scenario {i}: Rust (left) and Python (right) differ");
    }
    assert_eq!(got.len(), expected.as_array().unwrap().len());
    assert!(python_requests.len() > 500, "only {} requests", python_requests.len());
    assert_every_state_reached(&expected);
    if rust_requests != python_requests {
        let only_rust: Vec<_> = rust_requests.iter().filter(|r| !python_requests.contains(r)).take(3).collect();
        let only_python: Vec<_> = python_requests.iter().filter(|r| !rust_requests.contains(r)).take(3).collect();
        panic!("requests differ:\nonly Rust: {only_rust:#?}\nonly Python: {only_python:#?}");
    }
}
