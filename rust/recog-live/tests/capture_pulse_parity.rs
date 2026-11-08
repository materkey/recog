//! `PulseAudioCapture` against `recog/system_audio.py`: the same scripted answers of
//! `pactl get-default-sink` (sinks, a monitor-like `@` name, failures, timeouts, empty
//! output) and the same sequence of pins, polls and switches must give the same argv,
//! current source, baseline sink, source epoch, spawned commands and log, step by step.

mod common;

use std::io;
use std::sync::{Arc, Mutex};

use common::Rng;
use recog_live::capture::child::{ChildProcess, ReadOutcome, Spawner, StderrTail};
use recog_live::capture::pulse::{PactlOutput, PactlRunner, PulseAudioCapture, pulse_builder};
use recog_live::capture::subprocess::SourceSwitch as _;
use recog_live::clock::ManualClock;
use recog_live::log::Logger;
use serde_json::{Value as Json, json};

const SEEDS: u64 = 400;

struct Scripted(Mutex<Vec<Json>>);

impl PactlRunner for Scripted {
    fn get_default_sink(&self, timeout_s: f64) -> Result<PactlOutput, String> {
        assert!((timeout_s - 2.0).abs() < f64::EPSILON);
        let mut answers = self.0.lock().unwrap();
        let answer = if answers.is_empty() { json!(["sink", ""]) } else { answers.remove(0) };
        match answer[0].as_str().unwrap() {
            "error" => Err(answer[1].as_str().unwrap().to_owned()),
            "exit" => Ok(PactlOutput {
                stdout: answer[1].as_str().unwrap().to_owned(),
                returncode: i32::try_from(answer[2].as_i64().unwrap()).unwrap(),
            }),
            _ => Ok(PactlOutput { stdout: format!("{}\n", answer[1].as_str().unwrap()), returncode: 0 }),
        }
    }
}

struct Child;

impl ChildProcess for Child {
    fn pid(&self) -> u32 {
        7
    }
    fn poll(&self) -> Option<i32> {
        None
    }
    fn read(&self, _max: usize, _timeout: f64) -> ReadOutcome {
        ReadOutcome::Timeout
    }
    fn read_stderr(&self) -> String {
        String::new()
    }
    fn drain_stderr(&self, _tail: StderrTail) {}
    fn reap(&self, _timeout: f64) {}
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Vec<String>>>);

impl Spawner for Recorder {
    fn spawn(&self, argv: &[String]) -> io::Result<Arc<dyn ChildProcess>> {
        self.0.lock().unwrap().push(argv.to_vec());
        Ok(Arc::new(Child))
    }
}

fn script(seed: u64) -> (String, Vec<Json>, Vec<&'static str>) {
    let mut rng = Rng::new(seed);
    let pinned = match rng.range(0, 6) {
        0 => "  bluez.monitor ",
        1 => "   ",
        _ => "",
    };
    let answers = (0..60)
        .map(|_| match rng.range(0, 12) {
            0..=3 => json!(["sink", "sinkA"]),
            4..=5 => json!(["sink", "sinkB"]),
            6 => json!(["sink", "alsa_output.pci-0000_00_1f.3.analog-stereo"]),
            7 => json!(["sink", "@DEFAULT_SINK@"]),
            8 => json!(["exit", "", 1]),
            9 => json!(["exit", "  \n", 0]),
            10 => json!(["error", "Command '['pactl', 'get-default-sink']' timed out after 2.0 seconds"]),
            _ => json!(["error", "[Errno 2] No such file or directory: 'pactl'"]),
        })
        .collect();
    let mut ops = Vec::new();
    if rng.uniform() < 0.7 {
        ops.push("pin");
    }
    for _ in 0..40 {
        ops.push(if rng.uniform() < 0.6 { "poll" } else { "switch" });
    }
    (pinned.to_owned(), answers, ops)
}

fn run_rust(pinned: &str, answers: &[Json], ops: &[&str]) -> Json {
    let (log, captured) = Logger::capture(ManualClock::new(0.0).shared());
    let spawner = Arc::new(Recorder::default());
    let base = pulse_builder().clock(ManualClock::new(0.0).shared()).logger(log).spawner(spawner.clone());
    let cap = PulseAudioCapture::new(base, None, pinned, Arc::new(Scripted(Mutex::new(answers.to_vec()))), 2.0);
    let state = cap.state().clone();
    let mut out = Vec::new();
    for op in ops {
        let mut record = serde_json::Map::new();
        match *op {
            "pin" => cap.pin_initial_source(),
            "poll" => state.poll_default_sink_once(),
            _ => {
                let pending = state.take_pending();
                record.insert("pending".into(), json!(pending));
                if let Some(source) = pending {
                    state.apply(cap.capture(), &source).unwrap();
                }
            }
        }
        record.insert("argv".into(), json!(cap.capture().argv()));
        record.insert("current".into(), json!(state.current_source()));
        record.insert("last_sink".into(), json!(state.last_default_sink()));
        record.insert("epoch".into(), json!(cap.capture().source_epoch()));
        record.insert("follow".into(), json!(cap.follows_default_sink()));
        out.push(Json::Object(record));
    }
    let stderr: String = captured.lines().iter().map(|l| format!("{l}\n")).collect();
    json!({"ops": out, "spawned": *spawner.0.lock().unwrap(), "stderr": stderr})
}

#[test]
fn same_pactl_answers_and_steps_pulse_capture_follows_the_default_sink_as_python_does() {
    let Some(python) = common::python() else { return };
    let scripts: Vec<_> = (0..SEEDS).map(script).collect();
    let request: Vec<Json> = scripts
        .iter()
        .map(|(pinned, answers, ops)| json!({"pinned": pinned, "answers": answers, "ops": ops}))
        .collect();
    let sandbox = tempfile::tempdir().unwrap();
    let python_all = common::run_python(&python, "capture_pulse.py", &Json::Array(request), sandbox.path());
    let mut switches = 0;
    for (seed, ((pinned, answers, ops), python_out)) in scripts.iter().zip(python_all.as_array().unwrap()).enumerate() {
        let rust_out = run_rust(pinned, answers, ops);
        assert_eq!(python_out["stderr"], rust_out["stderr"], "seed {seed} log");
        assert_eq!(python_out["spawned"], rust_out["spawned"], "seed {seed} spawns");
        for (i, (p, r)) in
            python_out["ops"].as_array().unwrap().iter().zip(rust_out["ops"].as_array().unwrap()).enumerate()
        {
            assert_eq!(p, r, "seed {seed} op {i} ({})", ops[i]);
        }
        switches += rust_out["spawned"].as_array().unwrap().len();
    }
    eprintln!("{SEEDS} seeds, {switches} reconnects identical");
    assert!(switches > 100);
}
