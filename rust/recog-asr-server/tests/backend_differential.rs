//! The Rust GigaAM backend against `recog/asr_server/gigaam_backend.py`, the model
//! replaced on both sides by a fake that answers from one scenarios file by the number
//! of frames of the WAV ffmpeg wrote, and ffmpeg by a script that copies its input
//! (or fails on input starting with `BAD`, or is killed on `SIG`). Every response
//! format over segments with awkward times and texts, words, diarization requests,
//! decoding and model failures, request validation and routing, and a backend whose
//! model is still loading. Answers are compared by status, the headers that matter
//! and body; ffmpeg's command lines are compared, and both temporary directories must
//! end empty.
//!
//! The steps, the fake model and the fake ffmpeg live in tests/backend_support. With
//! `RECOG_RECORD_ASR_SERVER_FIXTURE=1` the test also writes Python's answers to
//! tests/fixtures/backend_python.json, which tests/backend_frozen.rs replays without
//! Python.

mod backend_support;
mod support;

use std::path::Path;
use std::process::Command;

use backend_support::{
    RECORD, ask, ffmpeg_lines, fixture_path, loading_steps, prepare, remaining, start_rust, steps, steps_sha256,
};
use serde_json::json;
use support::{Diff, Process};

fn start_python(python: &Path, dir: &Path, with_model: bool) -> Process {
    let port = support::free_port();
    let mut command = Command::new(python);
    command
        .arg("-I")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/run_backend.py"))
        .arg(support::repo_root())
        .arg(port.to_string())
        .arg(dir.join("scenarios.json"));
    if !with_model {
        command.arg("--no-model");
    }
    let env = [
        ("PATH", format!("{}:/usr/bin:/bin", dir.join("bin").display())),
        ("TMPDIR", dir.join("py-tmp").display().to_string()),
    ];
    let log = dir.join(if with_model { "py.log" } else { "py-loading.log" });
    Process::start(command, &env, port, log, "/healthz")
}

#[test]
fn python_and_rust_backends_given_one_fake_model_when_asked_the_same_then_the_same_answers() {
    let Some(python) = support::python() else { return };
    let dir = tempfile::tempdir().unwrap();
    prepare(dir.path());
    let (steps, loading_steps) = (steps(), loading_steps());

    let py = start_python(&python, dir.path(), true);
    let py_loading = start_python(&python, dir.path(), false);
    let expected = ask(py.port, &steps);
    let expected_loading = ask(py_loading.port, &loading_steps);
    let py_ffmpeg = ffmpeg_lines(dir.path(), "py-tmp");
    if std::env::var_os(RECORD).is_some() {
        support::write_fixture(
            &fixture_path(),
            &json!({
                "steps_sha256": steps_sha256(),
                "answers": expected,
                "loading": expected_loading,
                "ffmpeg": py_ffmpeg,
            }),
        );
    }
    let got = ask(start_rust(dir.path(), true), &steps);
    let got_loading = ask(start_rust(dir.path(), false), &loading_steps);
    let rs_ffmpeg = ffmpeg_lines(dir.path(), "rs-tmp");

    let mut diff = Diff::new("Python");
    diff.compare(&expected, &got);
    diff.compare(&expected_loading, &got_loading);
    if py_ffmpeg != rs_ffmpeg {
        diff.fail(format!("ffmpeg command lines differ\n  py: {py_ffmpeg:#?}\n  rs: {rs_ffmpeg:#?}"));
    }
    for side in ["py-tmp", "rs-tmp"] {
        let left = remaining(dir.path(), side);
        if !left.is_empty() {
            diff.fail(format!("{side}: temporary files left: {left:?}"));
        }
    }
    diff.finish();
}
