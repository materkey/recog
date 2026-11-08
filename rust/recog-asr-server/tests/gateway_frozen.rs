//! `recog-asr-gateway` against the answers `python -m recog.asr_server.gateway` gave
//! to the same steps over the same fake backend, recorded into
//! tests/fixtures/gateway_python.json by tests/gateway_differential.rs
//! (`RECOG_RECORD_ASR_SERVER_FIXTURE=1`), so the comparison holds without Python.
//! Every step's answer and the requests the fake received during it must be equal to
//! the recorded ones.

mod gateway_support;
mod support;

use std::process::Command;

use gateway_support::{FakeBackend, ask, fixture_path, start_rust, steps, steps_sha256};
use support::Diff;

#[test]
fn recorded_python_answers_when_the_rust_gateway_is_asked_the_same_then_the_same_answers_and_requests() {
    // given
    let steps = steps();
    let fixture = support::read_fixture(
        &fixture_path(),
        &steps_sha256(&steps),
        "RECOG_RECORD_ASR_SERVER_FIXTURE=1 cargo test -p recog-asr-server --test gateway_differential",
    );
    let expected = fixture["answers"].as_array().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeBackend::start();
    let refused = support::free_port();
    let rs = start_rust(dir.path(), fake.port, refused);
    // when
    let got = ask(rs.port, &fake, refused, &steps);
    // then
    let mut diff = Diff::new("recorded Python");
    diff.compare(expected, &got);
    diff.finish();
}

#[test]
fn a_port_python_refuses_when_the_gateway_starts_then_it_exits_with_the_message() {
    let output =
        Command::new(env!("CARGO_BIN_EXE_recog-asr-gateway")).env("ASR_GATEWAY_PORT", "eighty").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid literal for int() with base 10: 'eighty'"));
}
