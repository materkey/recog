//! The Rust GigaAM backend against the answers `recog/asr_server/gigaam_backend.py`
//! gave to the same steps with the same fake model and fake ffmpeg, recorded into
//! tests/fixtures/backend_python.json by tests/backend_differential.rs
//! (`RECOG_RECORD_ASR_SERVER_FIXTURE=1`) before the Python side went, so the
//! comparison holds without Python.
//! Every step's answer, of a backend with its model and of one still loading, and
//! ffmpeg's command lines must be equal to the recorded ones, and the temporary
//! directory must end empty.

mod backend_support;
mod support;

use backend_support::{
    ask, ffmpeg_lines, fixture_path, loading_steps, prepare, remaining, start_rust, steps, steps_sha256,
};
use serde_json::Value as Json;
use support::Diff;

#[test]
fn recorded_python_answers_when_the_rust_backend_is_asked_the_same_then_the_same_answers_and_ffmpeg_calls() {
    // given
    let fixture = support::read_fixture(
        &fixture_path(),
        &steps_sha256(),
        "RECOG_RECORD_ASR_SERVER_FIXTURE=1 cargo test -p recog-asr-server --test backend_differential",
    );
    let dir = tempfile::tempdir().unwrap();
    prepare(dir.path());
    // when
    let got = ask(start_rust(dir.path(), true), &steps());
    let got_loading = ask(start_rust(dir.path(), false), &loading_steps());
    let rs_ffmpeg = ffmpeg_lines(dir.path(), "rs-tmp");
    // then
    let mut diff = Diff::new("recorded Python");
    diff.compare(fixture["answers"].as_array().unwrap(), &got);
    diff.compare(fixture["loading"].as_array().unwrap(), &got_loading);
    let py_ffmpeg: Vec<String> =
        fixture["ffmpeg"].as_array().unwrap().iter().map(|l| Json::as_str(l).unwrap().to_owned()).collect();
    assert!(py_ffmpeg.len() > 50, "only {} ffmpeg calls recorded", py_ffmpeg.len());
    if py_ffmpeg != rs_ffmpeg {
        diff.fail(format!("ffmpeg command lines differ\n  recorded: {py_ffmpeg:#?}\n  rs: {rs_ffmpeg:#?}"));
    }
    let left = remaining(dir.path(), "rs-tmp");
    if !left.is_empty() {
        diff.fail(format!("rs-tmp: temporary files left: {left:?}"));
    }
    diff.finish();
}
