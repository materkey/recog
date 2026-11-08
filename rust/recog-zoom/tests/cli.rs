//! The `recog-zoom-app` command line, up to the point where it would start the helper.
#![forbid(unsafe_code)]

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recog-zoom-app")).args(args).output().unwrap()
}

#[test]
fn given_help_when_run_then_the_usage_is_printed_and_it_exits_0() {
    let output = run(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage: recog-zoom-app [--repo-root DIR]"));
}

#[test]
fn given_an_unknown_argument_when_run_then_it_exits_2_and_names_it() {
    let output = run(&["--bridge"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown argument: --bridge"));
}

#[test]
fn given_repo_root_without_a_directory_when_run_then_it_exits_2() {
    let output = run(&["--repo-root"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--repo-root needs a directory"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn given_another_os_when_run_then_it_says_macos_only_and_fails() {
    let output = run(&[]);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("supports macOS only"));
}
