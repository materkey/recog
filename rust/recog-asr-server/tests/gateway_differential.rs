//! `recog-asr-gateway` against `python -m recog.asr_server.gateway`, both configured
//! by the same environment and pointed at one fake backend (and a Qwen URL where
//! nothing listens): every route, the model aliases, the optional fields, the
//! backend's answers passed on or turned into errors, the health aggregation, timeouts
//! and request validation. Each answer is compared by status, the headers that matter
//! and body; each request the fake received is compared too (boundary masked).
//!
//! The steps and the fake live in tests/gateway_support. With
//! `RECOG_RECORD_ASR_SERVER_FIXTURE=1` the test also writes Python's answers to
//! tests/fixtures/gateway_python.json, which tests/gateway_frozen.rs replays without
//! Python.

mod gateway_support;
mod support;

use std::process::Command;

use gateway_support::{FakeBackend, RECORD, ask, fixture_path, gateway_env, start_rust, steps, steps_sha256};
use serde_json::json;
use support::{Diff, Process};

fn start_python(python: &std::path::Path, dir: &std::path::Path, fake: u16, refused: u16) -> Process {
    let port = support::free_port();
    let mut py = Command::new(python);
    py.arg("-I")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/run_gateway.py"))
        .arg(support::repo_root());
    Process::start(py, &gateway_env(port, fake, refused), port, dir.join("py.log"), "/v1/models")
}

#[test]
fn python_and_rust_gateways_given_one_fake_backend_when_asked_the_same_then_the_same_answers_and_requests() {
    let Some(python) = support::python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeBackend::start();
    let refused = support::free_port();
    let steps = steps();
    let py = start_python(&python, dir.path(), fake.port, refused);
    let rs = start_rust(dir.path(), fake.port, refused);

    let expected = ask(py.port, &fake, refused, &steps);
    if std::env::var_os(RECORD).is_some() {
        support::write_fixture(&fixture_path(), &json!({"steps_sha256": steps_sha256(&steps), "answers": expected}));
    }
    let got = ask(rs.port, &fake, refused, &steps);

    let mut diff = Diff::new("Python");
    diff.compare(&expected, &got);
    diff.finish();
}
