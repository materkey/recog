//! The Rust remote backend against the answers `recog/remote_asr.py` gave to the same
//! scenarios over the same fake gateway, recorded into
//! tests/fixtures/remote_asr_python.json by tests/asr_remote_parity.rs
//! (`RECOG_RECORD_REMOTE_ASR_FIXTURE=1`), so the comparison holds without Python.
//! Every step's answer, `available` after it, the `[FallbackASR]` log lines and the
//! requests the gateway saw must be equal to the recorded ones.

mod common;
mod remote_asr_support;

use remote_asr_support::{Gateway, assert_every_state_reached, fixture_path, run_rust, scenarios, scenarios_sha256};
use serde_json::Value as Json;

#[test]
fn recorded_python_answers_when_the_rust_backend_replays_the_scenarios_then_steps_logs_and_requests_are_the_same() {
    // given
    let fixture: Json = serde_json::from_slice(&std::fs::read(fixture_path()).unwrap()).unwrap();
    let scenarios = scenarios();
    assert_eq!(
        fixture["scenarios_sha256"].as_str().unwrap(),
        scenarios_sha256(&scenarios),
        "the scenarios changed since the answers were recorded: record them again on a checkout with the Python side \
         (RECOG_RECORD_REMOTE_ASR_FIXTURE=1 cargo test -p recog-live --test asr_remote_parity)"
    );
    let expected = fixture["answers"].as_array().unwrap();
    let python_requests: Vec<String> =
        fixture["requests"].as_array().unwrap().iter().map(|r| r.as_str().unwrap().to_owned()).collect();
    let gateway = Gateway::start();
    gateway.reset(&scenarios);
    // when
    let got: Vec<Json> = scenarios.iter().enumerate().map(|(i, s)| run_rust(&gateway.url, i, s)).collect();
    let rust_requests = gateway.records();
    // then
    for (i, (want, have)) in expected.iter().zip(&got).enumerate() {
        assert_eq!(have, want, "scenario {i}: Rust (left) and the recorded Python answer (right) differ");
    }
    assert_eq!(got.len(), expected.len());
    assert!(python_requests.len() > 500, "only {} requests", python_requests.len());
    assert_every_state_reached(&Json::from(expected.clone()));
    if rust_requests != python_requests {
        let only_rust: Vec<_> = rust_requests.iter().filter(|r| !python_requests.contains(r)).take(3).collect();
        let only_python: Vec<_> = python_requests.iter().filter(|r| !rust_requests.contains(r)).take(3).collect();
        panic!("requests differ:\nonly Rust: {only_rust:#?}\nonly the recording: {only_python:#?}");
    }
}
