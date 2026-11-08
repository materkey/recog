//! The Rust refine worker against `recog/refine.py`, driven step by step.
//!
//! No worker thread runs on either side, so the comparison is deterministic: seeded
//! scripts submit tasks (some of an old generation), pause, resume, bump the
//! generation, stop, and process the queue's head while telling the transcribe
//! callback what to do (answer, find ASR unavailable, give way to live, fail, or pause
//! the worker or switch the meeting while it runs). Every transcribe and apply call,
//! the `should_abort` it saw, what was raised, the queue, the pause flag, the requeue
//! sleeps and the log lines must be equal. Both modes, with and without the callback
//! managing the lock and the generation getter. The threaded paths (the live-priority
//! gate, the post-lock recheck, stop joining) are covered by tests/refine.rs.

mod common;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use recog_live::asr::{AsrError, Transcription};
use recog_live::clock::ManualClock;
use recog_live::log::Logger;
use recog_live::refine::{AbortMode, RefineOptions, RefineResult, RefineTask, RefineWorker, new_model_lock};
use serde_json::{Value as Json, json};

use common::Rng;

const SEEDS: u64 = 300;

type Slot = Arc<Mutex<Option<Arc<RefineWorker>>>>;

fn run_rust(script: &Json) -> Json {
    let generation = Arc::new(AtomicI64::new(0));
    let events: Arc<Mutex<Vec<Json>>> = Arc::default();
    let outcomes: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let clock = ManualClock::new(0.0);
    let (logger, captured) = Logger::capture(clock.shared());
    let slot: Slot = Arc::default();

    let (ev, out, s, g) = (events.clone(), outcomes.clone(), slot.clone(), generation.clone());
    let transcribe = Arc::new(move |pcm: &[i16], should_abort: Option<&dyn Fn() -> bool>| {
        let outcome = out.lock().unwrap().pop_front().unwrap();
        ev.lock().unwrap().push(json!(["transcribe", pcm[0], should_abort.is_some(), should_abort.map(|f| f())]));
        match outcome.as_str() {
            "pause_during" => s.lock().unwrap().as_ref().unwrap().pause(),
            "gen_during" => {
                g.fetch_add(1, Ordering::SeqCst);
            }
            "unavailable" => return Err(AsrError::TemporarilyUnavailable),
            "aborted" => return Err(AsrError::Aborted),
            "error" => return Err(AsrError::Failed("boom".into())),
            _ => {}
        }
        Ok(Transcription { text: format!("text {}", pcm[0]), words: Vec::new() })
    });
    let ev = events.clone();
    let apply = Arc::new(move |r: &RefineResult| {
        ev.lock().unwrap().push(json!(["apply", r.task.pid, r.text]));
        if r.text.ends_with('7') { Err("disk full".to_owned()) } else { Ok(()) }
    });
    let g = generation.clone();
    let worker = Arc::new(RefineWorker::new(
        new_model_lock(),
        transcribe,
        apply,
        RefineOptions {
            abort_mode: AbortMode::parse(script["mode"].as_str().unwrap()).unwrap(),
            transcribe_manages_lock: script["manages_lock"].as_bool().unwrap(),
            generation_getter: script["generation"]
                .as_bool()
                .unwrap()
                .then(|| -> recog_live::refine::GenerationGetter { Arc::new(move || g.load(Ordering::SeqCst)) }),
            clock: clock.shared(),
            logger,
            ..RefineOptions::default()
        },
    ));
    *slot.lock().unwrap() = Some(worker.clone());
    for op in script["ops"].as_array().unwrap() {
        match op["op"].as_str().unwrap() {
            "submit" => {
                #[allow(clippy::cast_possible_truncation)]
                worker.submit(RefineTask {
                    pid: op["id"].as_str().unwrap().into(),
                    pcm_samples: vec![op["n"].as_i64().unwrap() as i16],
                    offset_base_ms: 0.0,
                    submitted_at: 0.0,
                    meeting_generation: op["gen"].as_i64().unwrap(),
                });
            }
            "pause" => worker.pause(),
            "resume" => worker.resume(),
            "gen" => generation.store(op["v"].as_i64().unwrap(), Ordering::SeqCst),
            "stop" => worker.stop(Duration::from_millis(100)),
            "process" => {
                if let Some(task) = worker.take_queued() {
                    outcomes.lock().unwrap().push_back(op["outcome"].as_str().unwrap().to_owned());
                    if let Err(e) = worker.process_task(task) {
                        let (kind, message) = match e {
                            AsrError::Failed(m) => ("RuntimeError", m),
                            other => ("other", other.to_string()),
                        };
                        events.lock().unwrap().push(json!(["raised", kind, message]));
                    }
                    outcomes.lock().unwrap().clear();
                }
            }
            other => panic!("unknown op {other}"),
        }
        let queued: Vec<String> = worker.queued_tasks().iter().map(|t| t.pid.clone()).collect();
        events.lock().unwrap().push(json!(["state", queued, worker.is_paused(), clock.sleeps().len()]));
    }
    *slot.lock().unwrap() = None;
    let events = events.lock().unwrap().clone();
    json!({"events": events, "log": captured.lines()})
}

fn script(seed: u64) -> Json {
    let mut rng = Rng::new(seed);
    let mode = if rng.uniform() < 0.6 { "cooperative" } else { "coarse" };
    let manages_lock = rng.uniform() < 0.5;
    let generation_on = rng.uniform() < 0.7;
    let mut generation = 0;
    let mut ops = Vec::new();
    let mut next_id = 0;
    let outcomes = ["ok", "ok", "ok", "unavailable", "aborted", "error", "pause_during", "gen_during"];
    for _ in 0..rng.range(10, 60) {
        let roll = rng.uniform();
        if roll < 0.4 {
            next_id += 1;
            let task_generation = if rng.uniform() < 0.15 { generation - 1 } else { generation };
            ops.push(json!({"op": "submit", "id": format!("t{next_id}"), "n": next_id, "gen": task_generation}));
        } else if roll < 0.75 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let outcome = outcomes[rng.range(0, outcomes.len() as i64) as usize];
            ops.push(json!({"op": "process", "outcome": outcome}));
        } else if roll < 0.83 {
            ops.push(json!({"op": "pause"}));
        } else if roll < 0.93 {
            ops.push(json!({"op": "resume"}));
        } else if roll < 0.98 {
            generation += 1;
            ops.push(json!({"op": "gen", "v": generation}));
        } else {
            ops.push(json!({"op": "stop"}));
        }
    }
    json!({"mode": mode, "manages_lock": manages_lock, "generation": generation_on, "ops": ops})
}

#[test]
fn seeded_scripts_through_both_workers_then_every_call_queue_and_line_is_the_same() {
    let Some(python) = common::python() else { return };
    let scripts: Vec<Json> = (0..SEEDS).map(script).collect();
    let sandbox = tempfile::tempdir().unwrap();
    let answer = common::run_python(&python, "refine.py", &json!({"scripts": scripts}), sandbox.path());
    let answers = answer.as_array().unwrap();
    let mut counts = [0usize; 3];
    for (seed, (script, expected)) in scripts.iter().zip(answers).enumerate() {
        let ours = run_rust(script);
        for key in ["events", "log"] {
            let (a, b) = (ours[key].as_array().unwrap(), expected[key].as_array().unwrap());
            if a != b {
                let at = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
                panic!(
                    "seed {seed} ({}): {key} differ at {at}:\nrust:   {:?}\npython: {:?}",
                    script["mode"],
                    a.get(at),
                    b.get(at)
                );
            }
        }
        for event in ours["events"].as_array().unwrap() {
            match event[0].as_str().unwrap() {
                "transcribe" => counts[0] += 1,
                "apply" => counts[1] += 1,
                _ => {}
            }
        }
        counts[2] += ours["log"].as_array().unwrap().len();
    }
    eprintln!("{SEEDS} scripts equal: {} transcribe calls, {} applies, {} log lines", counts[0], counts[1], counts[2]);
    // Log lines come only from applies that fail (texts ending in 7).
    assert!(counts[0] > 200 && counts[1] > 100 && counts[2] > 0, "too little exercised: {counts:?}");
}
