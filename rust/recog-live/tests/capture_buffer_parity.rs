//! `BufferedAudioCapture` against `recog/audio_capture.py`: the same blocks with the
//! same capture times and the same `get_chunk` calls must give the same chunks (by
//! sha256 of their float32 bytes), the same capture times as `f64`s, the same drops
//! and the same epochs. The scripts mix realtime feeding, device gaps, a backlog past
//! `CAPTURE_RESYNC_MAX_S`, overflow of the cap, clears, source boundaries and stops.

mod common;

use common::Rng;
use recog_live::capture::buffer::BufferedAudioCapture;
use recog_live::clock::ManualClock;
use serde_json::{Value as Json, json};
use sha2::{Digest as _, Sha256};

const SEEDS: u64 = 40;
const OPS: usize = 300;

fn samples(k: i64, n: i64) -> Vec<f32> {
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    (0..n).map(|i| ((k * 7919 + i) % 2001 - 1000) as f32 / 1024.0).collect()
}

fn script(seed: u64, sample_rate: u32) -> Vec<Json> {
    let mut rng = Rng::new(seed);
    let mut wall = 1.762e9 + rng.uniform() * 1000.0;
    let mut ops = Vec::new();
    for k in 0..OPS {
        let roll = rng.uniform();
        let op = if roll < 0.6 {
            let n = rng.range(1, i64::from(sample_rate) * 2);
            #[allow(clippy::cast_precision_loss)]
            let at = wall;
            #[allow(clippy::cast_precision_loss)]
            {
                wall += n as f64 / f64::from(sample_rate);
            }
            // Device gaps, jitter, and now and then a clock going back a little.
            let r = rng.uniform();
            if r < 0.1 {
                wall += rng.uniform() * 8.0;
            } else if r < 0.15 {
                wall -= rng.uniform() * 0.05;
            } else {
                wall += (rng.uniform() - 0.5) * 0.003;
            }
            json!({"op": "append", "k": k, "n": n, "at": at})
        } else if roll < 0.92 {
            let d = [0.5, 1.0, 2.0, 3.0, 3.3, 7.25][usize::try_from(rng.range(0, 6)).unwrap()];
            json!({"op": "get", "d": d})
        } else if roll < 0.95 {
            json!({"op": "clear"})
        } else if roll < 0.98 {
            json!({"op": "boundary"})
        } else {
            json!({"op": "running", "on": rng.uniform() < 0.5})
        };
        ops.push(op);
    }
    // Leave it running for the last gets.
    ops.push(json!({"op": "running", "on": true}));
    ops.push(json!({"op": "get", "d": 0.5}));
    ops
}

fn run_rust(sample_rate: u32, max_buffer_s: f64, ops: &[Json]) -> Vec<Json> {
    let cap = BufferedAudioCapture::new(sample_rate, "parity", max_buffer_s, ManualClock::new(0.0).shared());
    cap.set_running(true);
    ops.iter()
        .map(|op| {
            let result = match op["op"].as_str().unwrap() {
                "append" => {
                    let data = samples(op["k"].as_i64().unwrap(), op["n"].as_i64().unwrap());
                    json!({"dropped": cap.append(&data, op["at"].as_f64().unwrap())})
                }
                "get" => match cap.get_chunk(op["d"].as_f64().unwrap()) {
                    None => Json::Null,
                    Some((chunk, ts)) => {
                        let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
                        let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
                        json!({"len": chunk.len(), "sha256": sha, "ts": ts, "chunk_epoch": cap.chunk_epoch()})
                    }
                },
                "clear" => {
                    cap.clear_buffer();
                    Json::Null
                }
                "boundary" => {
                    cap.mark_source_boundary();
                    Json::Null
                }
                "running" => {
                    cap.set_running(op["on"].as_bool().unwrap());
                    Json::Null
                }
                other => panic!("{other}"),
            };
            json!({
                "result": result,
                "buffered": cap.buffered_samples(),
                "start": cap.buffer_start_ts(),
                "source_epoch": cap.source_epoch(),
            })
        })
        .collect()
}

#[test]
fn same_blocks_and_calls_buffered_capture_gives_python_s_chunks_times_and_epochs_exactly() {
    let Some(python) = common::python() else { return };
    let mut chunks = 0;
    for seed in 0..SEEDS {
        let sample_rate = if seed % 5 == 4 { 48_000 } else { 16_000 };
        let max_buffer_s = [10.0, 4.0, 600.0, 7.5][usize::try_from(seed % 4).unwrap()];
        let ops = script(seed, sample_rate);
        let sandbox = tempfile::tempdir().unwrap();
        let request = json!({"sample_rate": sample_rate, "max_buffer_s": max_buffer_s, "ops": ops});
        let python_out = common::run_python(&python, "capture_buffer.py", &request, sandbox.path());
        let rust_out = run_rust(sample_rate, max_buffer_s, &ops);
        let python_out = python_out.as_array().unwrap();
        assert_eq!(python_out.len(), rust_out.len());
        for (i, (p, r)) in python_out.iter().zip(&rust_out).enumerate() {
            assert_eq!(p, r, "seed {seed} op {i} ({})", ops[i]);
        }
        chunks += rust_out.iter().filter(|r| r["result"].get("sha256").is_some()).count();
    }
    eprintln!("{SEEDS} seeds, {chunks} chunks identical");
    assert!(chunks > 1000);
}
