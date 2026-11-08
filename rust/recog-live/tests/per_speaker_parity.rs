//! The Rust ingest against `recog/per_speaker.py` on the same scripted scenarios.
//!
//! Seeded random scenarios (participants joining at different times, seq gaps,
//! reordering and duplicates, production 3.6 s payloads and other sizes, sample rates
//! 16 kHz, 8 kHz and 0, meeting toggles, generation bumps with and without `drain`,
//! ASR availability flips, callbacks failing with ASR unavailable or another error,
//! the refine callback attached and detached, watchdog-sized time jumps) run through
//! both implementations; every callback call, tick result, refine task, candidate,
//! stats snapshot, the flags after every step, the final per-pid bookkeeping and the
//! log lines must be equal, floats by their repr.

mod common;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use recog_core::py::float_repr;
use recog_live::asr::AsrError;
use recog_live::clock::ManualClock;
use recog_live::log::Logger;
use recog_live::per_speaker::{BatchedChunk, IngestOptions, IngestStats, PcmChunk, PerSpeakerIngest};
use recog_live::refine::RefineTask;
use serde_json::{Value as Json, json};
use sha2::{Digest as _, Sha256};

use common::Rng;

const SEEDS: u64 = 400;

fn num(x: f64) -> Json {
    Json::String(format!("f:{}", float_repr(x)))
}

fn opt_num(x: Option<f64>) -> Json {
    x.map_or(Json::Null, num)
}

fn pcm_bytes(n: usize, seed: i64) -> Vec<u8> {
    (0..n)
        .flat_map(|i| {
            #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
            let v = ((seed * 2_654_435_761 + i as i64 * 40503).rem_euclid(65536) - 32768) as i16;
            v.to_le_bytes()
        })
        .collect()
}

fn digest(samples: &[i16]) -> String {
    let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
    Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn batch_json(batch: &[BatchedChunk]) -> Json {
    batch
        .iter()
        .map(|b| {
            json!([
                b.pid,
                b.speaker_name,
                b.is_local,
                digest(&b.pcm),
                b.pcm.len(),
                num(b.chunk_start_ms),
                num(b.chunk_end_ms),
                num(b.pid_audio_ms_start),
                num(b.pid_audio_ms_end)
            ])
        })
        .collect()
}

fn task_json(task: Option<&RefineTask>) -> Json {
    task.map_or(Json::Null, |t| {
        json!([
            t.pid,
            digest(&t.pcm_samples),
            t.pcm_samples.len(),
            num(t.offset_base_ms),
            num(t.submitted_at),
            t.meeting_generation
        ])
    })
}

fn stats_json(stats: &IngestStats) -> Json {
    let per_pid: serde_json::Map<String, Json> = stats
        .per_pid
        .iter()
        .map(|(pid, s)| {
            (
                pid.clone(),
                json!({
                    "speaker_name": s.speaker_name,
                    "is_local": s.is_local,
                    "chunks_seen": s.chunks_seen,
                    "last_seq": s.last_seq,
                    "gaps_count": s.gaps_count,
                    "out_of_order_count": s.out_of_order_count,
                    "first_seen_ts": num(s.first_seen_ts),
                    "last_seen_ts": num(s.last_seen_ts),
                    "age_s": num(s.age_s),
                    "since_last_chunk_s": num(s.since_last_chunk_s),
                    "chunks_per_sec": num(s.chunks_per_sec),
                }),
            )
        })
        .collect();
    json!({
        "pcm_ever_seen": stats.pcm_ever_seen,
        "degraded": stats.degraded,
        "meeting_active_since": opt_num(stats.meeting_active_since),
        "last_chunk_ts": opt_num(stats.last_chunk_ts),
        "now": num(stats.now),
        "pending_buffer_sizes": stats.pending_buffer_sizes,
        "per_pid": per_pid,
    })
}

fn internals(ingest: &PerSpeakerIngest) -> Json {
    let mut out = serde_json::Map::new();
    for pid in ingest.refine_pids() {
        let s = ingest.refine_state(&pid).unwrap();
        out.insert(
            pid,
            json!({
                "ring": s.ring.iter().map(|(n, end)| json!([n, num(*end)])).collect::<Vec<_>>(),
                "audio_ms": num(s.audio_ms),
                "drained_ms": opt_num(s.drained_ms),
                "watermark_ms": opt_num(s.watermark_ms),
                "silence_ms": opt_num(s.silence_ms),
                "continuous_speech_ms": opt_num(s.continuous_speech_ms),
                "sample_rate": s.sample_rate,
                "touched": s.had_chunk_since_last_tick,
            }),
        );
    }
    Json::Object(out)
}

type Events = Arc<Mutex<Vec<Json>>>;

fn run_rust(scenario: &Json) -> Json {
    let clock = ManualClock::new(0.0);
    let generation = Arc::new(AtomicI64::new(0));
    let active = Arc::new(AtomicBool::new(false));
    let asr = Arc::new(AtomicBool::new(true));
    let fails: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let events: Events = Arc::default();
    let (logger, captured) = Logger::capture(clock.shared());

    let (ev, f) = (events.clone(), fails.clone());
    let callback = Arc::new(move |batch: &[BatchedChunk]| {
        ev.lock().unwrap().push(json!(["call", batch_json(batch)]));
        match f.lock().unwrap().pop_front().as_deref() {
            Some("unavailable") => Err(AsrError::TemporarilyUnavailable),
            Some(_) => Err(AsrError::Failed("boom".into())),
            None => Ok(()),
        }
    });
    let ev = events.clone();
    let submit: recog_live::per_speaker::RefineSubmit = Arc::new(move |task: RefineTask| {
        ev.lock().unwrap().push(json!(["refine", task_json(Some(&task))]));
        Ok(())
    });
    let options = &scenario["options"];
    let (g, a, r) = (generation.clone(), active.clone(), asr.clone());
    #[allow(clippy::cast_possible_truncation)]
    let ingest = PerSpeakerIngest::new(
        callback,
        IngestOptions {
            generation_getter: Some(Arc::new(move || g.load(Ordering::SeqCst))),
            meeting_active_getter: Some(Arc::new(move || a.load(Ordering::SeqCst))),
            max_batch_size: options["max_batch_size"].as_u64().unwrap() as usize,
            max_chunks_per_pid: options["max_chunks_per_pid"].as_u64().unwrap() as usize,
            silence_watchdog_s: options["silence_watchdog_s"].as_f64().unwrap(),
            clock: clock.shared(),
            refine_submit_callback: options["refine"].as_bool().unwrap().then(|| submit.clone()),
            asr_available_getter: Some(Arc::new(move || r.load(Ordering::SeqCst))),
            logger,
            ..IngestOptions::default()
        },
    );
    let push = |e: Json| events.lock().unwrap().push(e);
    for op in scenario["ops"].as_array().unwrap() {
        match op["op"].as_str().unwrap() {
            "time" => clock.set(op["t"].as_f64().unwrap()),
            "enqueue" => {
                #[allow(clippy::cast_possible_truncation)]
                ingest.enqueue(PcmChunk {
                    pid: op["pid"].as_str().unwrap().into(),
                    speaker_name: op["name"].as_str().unwrap().into(),
                    is_local: op["local"].as_bool().unwrap(),
                    seq: op["seq"].as_i64().unwrap(),
                    chunk_start_ts: op["start"].as_f64().unwrap(),
                    chunk_end_ts: op["end"].as_f64().unwrap(),
                    sample_rate: op["sr"].as_u64().unwrap() as u32,
                    pcm: pcm_bytes(op["n"].as_u64().unwrap() as usize, op["seed"].as_i64().unwrap()),
                });
            }
            "tick" => {
                let batch = ingest.tick(op["now"].as_f64());
                push(json!(["tick", batch_json(&batch)]));
            }
            "gen" => generation.store(op["v"].as_i64().unwrap(), Ordering::SeqCst),
            "drain" => ingest.drain(),
            "active" => active.store(op["v"].as_bool().unwrap(), Ordering::SeqCst),
            "asr" => asr.store(op["v"].as_bool().unwrap(), Ordering::SeqCst),
            "fail" => {
                let kind = op["kind"].as_str().unwrap().to_owned();
                fails.lock().unwrap().extend(std::iter::repeat_n(kind, op["count"].as_u64().unwrap() as usize));
            }
            "candidate" => {
                let task = ingest.get_refine_candidate(op["pid"].as_str().unwrap());
                push(json!(["candidate", task_json(task.as_ref())]));
            }
            "refine_cb" => ingest.set_refine_submit_callback(op["v"].as_bool().unwrap().then(|| submit.clone())),
            "stats" => push(json!(["stats", stats_json(&ingest.get_stats())])),
            other => panic!("unknown op {other}"),
        }
        push(json!(["flags", ingest.degraded(), ingest.pcm_ever_seen(), ingest.last_drain_generation()]));
    }
    let events = events.lock().unwrap().clone();
    json!({"events": events, "internals": internals(&ingest), "log": captured.lines()})
}

struct Pid {
    name: &'static str,
    speaker: &'static str,
    local: bool,
    seq: i64,
    start: f64,
}

/// One scenario from `seed`: options and a list of operations.
#[allow(clippy::too_many_lines)]
fn scenario(seed: u64) -> Json {
    let mut rng = Rng::new(seed);
    let names = [("a", "Alice"), ("b", ""), ("c", "Борис"), ("d", "Dana")];
    let mut pids: Vec<Pid> = Vec::new();
    let mut ops = Vec::new();
    let mut t = 1000.0 + rng.uniform() * 10.0;
    let mut generation = 0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pick = |rng: &mut Rng, items: &[u64]| items[rng.range(0, items.len() as i64) as usize];
    let max_batch_size = pick(&mut rng, &[1, 2, 3, 8]);
    let max_chunks_per_pid = pick(&mut rng, &[2, 3, 24]);
    let silence_watchdog_s = if rng.uniform() < 0.3 { 5.0 } else { 30.0 };
    let refine = rng.uniform() < 0.8;
    let options = json!({
        "max_batch_size": max_batch_size,
        "max_chunks_per_pid": max_chunks_per_pid,
        "silence_watchdog_s": silence_watchdog_s,
        "refine": refine,
    });
    let steps = rng.range(20, 90);
    for _ in 0..steps {
        let roll = rng.uniform();
        if roll < 0.45 {
            if pids.len() < names.len() && (pids.is_empty() || rng.uniform() < 0.1) {
                let (name, speaker) = names[pids.len()];
                pids.push(Pid { name, speaker, local: rng.uniform() < 0.2, seq: rng.range(0, 800), start: t - 3.0 });
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let index = rng.range(0, pids.len() as i64) as usize;
            let p = &mut pids[index];
            let (n, sr) = match rng.range(0, 10) {
                0..=3 => (57_600, 16_000),
                4 => (16_000, 16_000),
                5 => (48_000, 16_000),
                6 => (1000, 16_000),
                7 => (0, 16_000),
                8 => (28_800, 8000),
                _ => (57_600, 0),
            };
            let seq = match rng.range(0, 12) {
                0 => p.seq + 2, // a gap
                1 => p.seq - 1, // late or duplicate
                2 => p.seq,     // duplicate of the last
                _ => p.seq + 1,
            };
            p.seq = p.seq.max(seq);
            p.start += 3.0 + (rng.uniform() - 0.5) * 0.01;
            let start = p.start;
            ops.push(json!({
                "op": "enqueue", "pid": p.name, "name": p.speaker, "local": p.local, "seq": seq,
                "start": start, "end": start + 3.6, "sr": sr, "n": n, "seed": rng.range(0, 1_000_000),
            }));
        } else if roll < 0.75 {
            t += 0.2 + rng.uniform() * 0.05;
            ops.push(json!({"op": "time", "t": t}));
            let now = match rng.range(0, 10) {
                0 => Json::Null,
                1 => json!(t + 0.5),
                _ => json!(t),
            };
            ops.push(json!({"op": "tick", "now": now}));
        } else if roll < 0.78 {
            t += 6.0 + rng.uniform() * 30.0;
            ops.push(json!({"op": "time", "t": t}));
        } else if roll < 0.81 {
            generation += 1;
            ops.push(json!({"op": "gen", "v": generation}));
            if rng.uniform() < 0.7 {
                ops.push(json!({"op": "drain"}));
            }
        } else if roll < 0.85 {
            ops.push(json!({"op": "active", "v": rng.uniform() < 0.7}));
        } else if roll < 0.88 {
            ops.push(json!({"op": "asr", "v": rng.uniform() < 0.6}));
        } else if roll < 0.91 {
            let kind = if rng.uniform() < 0.7 { "unavailable" } else { "error" };
            ops.push(json!({"op": "fail", "kind": kind, "count": rng.range(1, 3)}));
        } else if roll < 0.95 {
            let pid = if pids.is_empty() || rng.uniform() < 0.1 {
                "zz"
            } else {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                pids[rng.range(0, pids.len() as i64) as usize].name
            };
            ops.push(json!({"op": "candidate", "pid": pid}));
        } else if roll < 0.97 {
            ops.push(json!({"op": "refine_cb", "v": rng.uniform() < 0.7}));
        } else {
            ops.push(json!({"op": "stats"}));
        }
    }
    // Long silences at the end fire the gap triggers and the watchdog.
    for _ in 0..20 {
        t += 0.2;
        ops.push(json!({"op": "time", "t": t}));
        ops.push(json!({"op": "tick", "now": t}));
    }
    ops.push(json!({"op": "stats"}));
    json!({"options": options, "ops": ops})
}

#[test]
fn seeded_random_scenarios_through_both_ingests_then_everything_they_did_is_the_same() {
    let Some(python) = common::python() else { return };
    let scenarios: Vec<Json> = (0..SEEDS).map(scenario).collect();
    let sandbox = tempfile::tempdir().unwrap();
    let answer = common::run_python(&python, "per_speaker.py", &json!({"scenarios": scenarios}), sandbox.path());
    let answers = answer.as_array().unwrap();
    assert_eq!(answers.len(), scenarios.len());
    let mut counts = [0usize; 4];
    let kinds = [
        "watchdog fired",
        "pcm recovered",
        "holding batch",
        "transcribe callback raised",
        "stale-generation",
        "ASR is behind",
        "suppressed",
    ];
    let mut seen = [false; 7];
    for (seed, (scenario, expected)) in scenarios.iter().zip(answers).enumerate() {
        let ours = run_rust(scenario);
        for (key, (a, b)) in ["events", "internals", "log"].iter().map(|k| (*k, (&ours[*k], &expected[*k]))) {
            if a != b {
                if let (Some(a), Some(b)) = (a.as_array(), b.as_array()) {
                    let at = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
                    panic!(
                        "seed {seed}: {key} differ at {at} (rust {} items, python {}):\nrust:   {}\npython: {}",
                        a.len(),
                        b.len(),
                        a.get(at).map_or(Json::Null, Clone::clone),
                        b.get(at).map_or(Json::Null, Clone::clone)
                    );
                }
                panic!("seed {seed}: {key} differ:\nrust:   {a}\npython: {b}");
            }
        }
        for event in ours["events"].as_array().unwrap() {
            match event[0].as_str().unwrap() {
                "call" => counts[0] += 1,
                "refine" => counts[1] += 1,
                "candidate" if !event[1].is_null() => counts[2] += 1,
                _ => {}
            }
        }
        for line in ours["log"].as_array().unwrap() {
            counts[3] += 1;
            for (k, kind) in kinds.iter().enumerate() {
                seen[k] |= line.as_str().unwrap().contains(kind);
            }
        }
    }
    eprintln!(
        "{SEEDS} scenarios equal: {} callback calls, {} refine tasks, {} candidates, {} log lines",
        counts[0], counts[1], counts[2], counts[3]
    );
    // The scenarios must reach every path they are there for.
    assert!(counts.iter().all(|&c| c > 50), "too little exercised: {counts:?}");
    let missing: Vec<_> = kinds.iter().zip(seen).filter(|(_, s)| !s).map(|(k, _)| k).collect();
    assert!(missing.is_empty(), "no log line of {missing:?}");
}
