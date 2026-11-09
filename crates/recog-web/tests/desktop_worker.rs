//! `recog-web desktop-worker`: the binary is its own observer's inspector worker, as
//! `python -m recog.desktop.worker` is the Python observer's.

use std::process::Command;

use recog_core::pyjson;

fn worker(home: &std::path::Path, args: &[&str]) -> i32 {
    let output = Command::new(env!("CARGO_BIN_EXE_recog-web"))
        .arg("desktop-worker")
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap();
    output.status.code().unwrap()
}

#[test]
fn nothing_to_recover_when_the_worker_runs_then_waiting_and_exit_zero() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("desktop");
    // when
    let code = worker(dir.path(), &["--root", root.to_str().unwrap(), "--recover"]);
    // then: nothing was open, so nothing is written
    assert_eq!(code, 0);
    assert!(!root.join("status.json").exists());
}

#[test]
fn unconfigured_target_when_the_worker_runs_then_invalid_installation_and_exit_one() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("desktop");
    let target = r#"{"pid": 2147483646, "started": 1.0, "executable": "/gone"}"#;
    // when
    let code = worker(dir.path(), &["--root", root.to_str().unwrap(), "--target", target]);
    // then
    assert_eq!(code, 1);
    let status = pyjson::loads(&std::fs::read_to_string(root.join("status.json")).unwrap()).unwrap();
    let map = status.as_object().unwrap();
    assert_eq!(map.get("reason").and_then(|r| r.as_str()), Some("invalid_installation"));
    assert_eq!(map.get("target"), Some(&pyjson::loads(target).unwrap()));
}

#[test]
fn bad_arguments_when_the_worker_runs_then_exit_two() {
    // given
    let dir = tempfile::tempdir().unwrap();
    // when
    let code = worker(dir.path(), &["--target", "{"]);
    // then
    assert_eq!(code, 2);
}
