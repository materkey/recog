//! The `recog-mic` command line, run as a process: what the parent relies on before any
//! audio flows (argument errors, the absent-device exit code, a quiet stdout).
#![forbid(unsafe_code)]

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recog-mic")).args(args).output().unwrap()
}

#[test]
fn a_call_without_the_rate_exits_2_like_argparse_and_writes_nothing_to_stdout() {
    // given / when
    let output = run(&["--device", "Mic"]);
    // then
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--sample-rate"));
}

#[test]
fn help_exits_0_with_the_usage_on_stderr() {
    // given / when
    let output = run(&["--help"]);
    // then
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--sample-rate"));
}

#[cfg(target_os = "macos")]
#[test]
fn an_absent_device_exits_4_opening_nothing_and_writing_no_frames() {
    // given: a name no device has
    // when
    let output = run(&["--sample-rate", "16000", "--device", "recog-mic test: no such device"]);
    // then
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not found"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn elsewhere_the_worker_says_it_captures_on_macos_only() {
    // given / when
    let output = run(&["--sample-rate", "16000"]);
    // then
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("macOS only"));
}
