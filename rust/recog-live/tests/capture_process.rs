//! The captures on real child processes and real pipes (the end-to-end cases of
//! tests/test_mic_child.py and the real-pipe behaviour of `SubprocessPCMCapture`):
//! stub children written in Python, never an audio device.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use recog_live::capture::framed::child_mic_capture;
use recog_live::capture::subprocess::{RestartPolicy, SubprocessCapture};
use recog_live::clock::SystemClock;
use recog_live::log::Logger;

const SR: u32 = 16_000;

const FRAMES_THEN_SLEEP: &str = r#"
import struct, sys, time
out = sys.stdout.buffer
for i in range(50):
    out.write(struct.pack("<ddI", 1.0 + i, time.time(), 160) + struct.pack("<160f", *([0.25] * 160)))
out.flush()
time.sleep(30)
"#;

const ONE_FRAME_THEN_EXIT: &str = r#"
import struct, sys, time
sys.stdout.buffer.write(struct.pack("<ddI", 1.0, time.time(), 800) + struct.pack("<800f", *([0.5] * 800)))
sys.stdout.buffer.flush()
time.sleep(1)
"#;

fn argv(python: &Path, code: &str) -> Vec<String> {
    vec![python.display().to_string(), "-I".into(), "-S".into(), "-c".into(), code.into()]
}

fn wait_for(mut done: impl FnMut() -> bool, seconds: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !done() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

#[test]
fn child_writing_frames_to_a_real_pipe_get_chunk_gives_its_audio_and_stop_leaves_no_process() {
    let Some(python) = common::python() else { return };
    let (log, captured) = Logger::capture(SystemClock::shared());
    let capture = child_mic_capture(argv(&python, FRAMES_THEN_SLEEP), None).logger(log).build();
    capture.start().unwrap();
    let mut got = None;
    assert!(wait_for(
        || {
            got = capture.get_chunk(0.25);
            got.is_some()
        },
        5
    ));
    let (chunk, start_ts) = got.unwrap();
    assert_eq!(chunk.len(), SR as usize / 4);
    assert!(chunk.iter().all(|&s| s == 0.25));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    assert!((start_ts - now).abs() < 5.0);
    let process = capture.process().unwrap();
    capture.stop();
    assert!(process.poll().is_some());
    assert!(capture.process().is_none());
    // The stub stamps frames a second apart, so holes are logged too.
    assert!(captured.lines().iter().any(|l| l.starts_with("[mic] Started (PID: ")));
    assert!(captured.text().contains("[mic] 990 ms hole in the captured audio"));
    assert_eq!(captured.lines().last().unwrap(), "[mic] Stopped");
}

#[test]
fn child_delivering_a_little_then_exiting_the_respawned_child_s_audio_arrives_too() {
    let Some(python) = common::python() else { return };
    let (log, captured) = Logger::capture(SystemClock::shared());
    let policy = RestartPolicy { backoff_s: vec![0.05], ..RestartPolicy::default() };
    let capture = child_mic_capture(argv(&python, ONE_FRAME_THEN_EXIT), None).logger(log).policy(policy).build();
    capture.start().unwrap();
    let arrived = wait_for(|| capture.buffer().buffered_samples() >= 1600, 10);
    capture.stop();
    assert!(arrived, "{:?}", captured.lines());
    assert!(captured.text().contains("[mic] Process died, restarting... (attempt 1)"));
    assert!(captured.text().contains("[mic] Restarted (PID: "));
}

#[test]
fn child_dying_on_its_first_spawn_start_quotes_its_stderr_and_keeps_running() {
    let Some(python) = common::python() else { return };
    let (log, captured) = Logger::capture(SystemClock::shared());
    let code = "import sys; sys.stderr.write('parec: Connection failure: Connection refused\\n'); sys.exit(1)";
    let capture = SubprocessCapture::builder(argv(&python, code), "pulse").logger(log).build();
    capture.start().unwrap();
    assert!(capture.running());
    assert!(!capture.dead());
    capture.stop();
    assert!(
        captured.text().contains(
            "[pulse] Failed to start, retrying in background (stderr: parec: Connection failure: Connection refused)"
        ),
        "{:?}",
        captured.lines()
    );
}

#[test]
fn child_ignoring_sigterm_stop_kills_it_after_the_timeout() {
    let Some(python) = common::python() else { return };
    let (log, _) = Logger::capture(SystemClock::shared());
    let code = "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)";
    let capture = SubprocessCapture::builder(argv(&python, code), "test").logger(log).build();
    capture.start().unwrap();
    let process = capture.process().unwrap();
    let started = Instant::now();
    capture.stop();
    assert!(process.poll().is_some());
    assert!(started.elapsed() < Duration::from_secs(5));
}
