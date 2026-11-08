//! The restart policy against `SubprocessPCMCapture`: both run the same stub child
//! (tests/parity/capture_stubs/child.py) with the same scaled policy, side by side,
//! and must print the same lines and spawn it at the same moments.
//!
//! The policy is scaled down from 1/2/5/10 s but its first pause is kept above the
//! 0.5 s startup wait, as the real one is: with a shorter one the reader replaces the
//! child while `start()` is still waiting, and which child `start()` then looks at
//! is a race in both implementations. Spawn times come from the stub itself (its
//! start time in a log file). The tolerance, 250 ms per interval, covers the stub's
//! own interpreter start and exit (20-60 ms each, varying by spawn, seen up to 110 ms
//! apart between the two sides on a loaded machine) and the 5-50 ms polls of the reap
//! and the idle read; a missed or extra backoff step or startup wait is 500 ms
//! or more, so it cannot hide in it. A requested restart is taken when the read wait
//! (1 s) ends, so the request comes mid-wait, at 1.5 s, where both see it at 2 s.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use recog_live::capture::subprocess::{RestartPolicy, SubprocessCapture};
use recog_live::clock::SystemClock;
use recog_live::log::Logger;
use serde_json::{Value as Json, json};

const TOLERANCE_S: f64 = 0.25;

fn policy() -> RestartPolicy {
    RestartPolicy { backoff_s: vec![0.6, 0.7, 0.8], max_restarts: 3, lost_retry_s: 0.9, ..RestartPolicy::default() }
}

struct Scenario {
    mode: &'static str,
    spawns: usize,
    settle_s: f64,
    restart_after_s: Option<f64>,
}

fn stub_argv(python: &Path, mode: &str, log: &Path) -> Vec<String> {
    let child = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/capture_stubs/child.py");
    vec![
        python.display().to_string(),
        "-I".into(),
        "-S".into(),
        child.display().to_string(),
        mode.into(),
        log.display().to_string(),
    ]
}

fn spawned(log: &Path) -> usize {
    fs::read_to_string(log).map_or(0, |t| t.lines().count())
}

fn run_rust(argv: Vec<String>, scenario: &Scenario, log: &Path) -> (String, Option<String>) {
    let (logger, captured) = Logger::capture(SystemClock::shared());
    let capture = SubprocessCapture::builder(argv, "stub").max_buffer_s(10.0).logger(logger).policy(policy()).build();
    let started = Instant::now();
    if let Err(e) = capture.start() {
        return (String::new(), Some(e.to_string()));
    }
    let mut restart_after = scenario.restart_after_s;
    let deadline = started + Duration::from_secs(20);
    while spawned(log) < scenario.spawns && Instant::now() < deadline {
        if let Some(after) = restart_after
            && started.elapsed().as_secs_f64() >= after
        {
            capture.request_restart("silence");
            restart_after = None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_secs_f64(scenario.settle_s));
    capture.stop();
    (captured.lines().iter().map(|l| format!("{l}\n")).collect(), None)
}

/// `PID: 1234` -> `PID: <pid>`.
fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("PID: ") {
        out.push_str(&rest[..at + 5]);
        rest = rest[at + 5..].trim_start_matches(|c: char| c.is_ascii_digit());
        out.push_str("<pid>");
    }
    out.push_str(rest);
    out
}

fn intervals(log: &Path) -> Vec<f64> {
    let times: Vec<f64> = fs::read_to_string(log).unwrap().lines().map(|l| l.parse().unwrap()).collect();
    times.windows(2).map(|w| w[1] - w[0]).collect()
}

fn compare(scenario: &Scenario) {
    let Some(python) = common::python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (py_log, rs_log) = (dir.path().join("python.log"), dir.path().join("rust.log"));
    let request = json!({
        "argv": stub_argv(&python, scenario.mode, &py_log),
        "policy": {"backoff_s": policy().backoff_s, "max_restarts": policy().max_restarts, "lost_retry_s": policy().lost_retry_s},
        "spawn_log": py_log,
        "spawns": scenario.spawns,
        "settle_s": scenario.settle_s,
        "restart_after_s": scenario.restart_after_s,
    });
    let sandbox = dir.path().join("sandbox");
    let python_side = {
        let python = python.clone();
        std::thread::spawn(move || common::run_python(&python, "capture_restart.py", &request, &sandbox))
    };
    let (rust_text, rust_error) = run_rust(stub_argv(&python, scenario.mode, &rs_log), scenario, &rs_log);
    let answer: Json = python_side.join().unwrap();

    assert_eq!(answer["error"], Json::Null);
    assert_eq!(rust_error, None);
    let python_text = normalize(answer["stderr"].as_str().unwrap());
    assert_eq!(normalize(&rust_text), python_text, "{} log", scenario.mode);
    let (py, rs) = (intervals(&py_log), intervals(&rs_log));
    assert_eq!(py.len(), rs.len(), "{} spawns: python {py:?}, rust {rs:?}", scenario.mode);
    assert_eq!(py.len() + 1, scenario.spawns);
    for (p, r) in py.iter().zip(&rs) {
        assert!((p - r).abs() <= TOLERANCE_S, "{} intervals: python {py:?}, rust {rs:?}", scenario.mode);
    }
    eprintln!("{}: python {py:.3?}, rust {rs:.3?}", scenario.mode);
}

#[test]
fn child_dying_at_once_both_back_off_mark_it_lost_and_retry_slowly_alike() {
    compare(&Scenario { mode: "die", spawns: 5, settle_s: 0.25, restart_after_s: None });
}

#[test]
fn child_streaming_then_dying_both_respawn_it_on_the_first_backoff_step_alike() {
    compare(&Scenario { mode: "pcm_then_die", spawns: 3, settle_s: 0.25, restart_after_s: None });
}

#[test]
fn silent_child_and_a_requested_restart_both_restart_it_once_alike() {
    compare(&Scenario { mode: "silent", spawns: 2, settle_s: 0.75, restart_after_s: Some(1.5) });
}

#[test]
fn missing_executable_both_refuse_to_start() {
    let Some(python) = common::python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let request = json!({
        "argv": ["/nonexistent/recog-stub"],
        "policy": {"backoff_s": [0.6], "max_restarts": 1, "lost_retry_s": 0.9},
        "spawn_log": dir.path().join("none.log"),
        "spawns": 1,
        "settle_s": 0.0,
    });
    let answer = common::run_python(&python, "capture_restart.py", &request, &dir.path().join("sandbox"));
    assert_eq!(answer["error"], "FileNotFoundError");
    let scenario = Scenario { mode: "die", spawns: 1, settle_s: 0.0, restart_after_s: None };
    let (text, error) = run_rust(vec!["/nonexistent/recog-stub".into()], &scenario, &dir.path().join("none.log"));
    assert!(error.unwrap().contains("No such file or directory"));
    assert_eq!(text, "");
}
