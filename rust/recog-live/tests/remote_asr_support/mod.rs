//! The remote backend's scenarios over one fake gateway, which
//! tests/asr_remote_frozen.rs replays against the answers `recog/remote_asr.py` gave
//! to them, recorded into tests/fixtures/remote_asr_python.json by the parity test
//! the Python side went with.
//!
//! The gateway is an HTTP server on an ephemeral port that answers every request from
//! a plan: the clip's first sample names it, and each name has a queue of answers
//! (text with words, null text, 503/404, a body that is not JSON, a list, words whose
//! times cannot be read, string times, a reply slower than the live timeout, health
//! answers of both gateway forms). Seeded random scenarios and hand-written ones
//! (timeouts, the health probe, a recovery probe) drive `FallbackAsr` through chunks,
//! per-speaker batches and refine windows while the clock moves past the failure
//! window, the open window and the local-load retry, and the fake local model's
//! background load ends in success or failure when the scenario says. A run's answer
//! is every step's result (text, unavailable, error), `available` after it and the
//! `[FallbackASR]` log lines (the gateway's error text cut off); the gateway records
//! the requests it saw (method, path, multipart part headers and values, the WAV's
//! format chunk, sample count and samples' SHA-256).
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use recog_live::asr::wavo::{EngineAnswer, Loader};
use recog_live::asr::{AsrError, Engine, FallbackAsr, RemoteAsrClient, WavoAsr};
use recog_live::clock::ManualClock;
use recog_live::live::LiveAsr;
use recog_live::log::Logger;
use serde_json::{Value as Json, json};
use sha2::{Digest as _, Sha256};

use crate::common;
use crate::common::Rng;

pub const SEEDS: u64 = 40;

/// How long a "slow" answer waits: longer than the live timeout, shorter than refine's.
const SLOW_S: f64 = 2.0;

// ---- the fake gateway ----

#[derive(Default)]
struct GatewayState {
    plan: HashMap<String, VecDeque<String>>,
    records: Vec<Json>,
}

pub struct Gateway {
    pub url: String,
    state: Arc<Mutex<GatewayState>>,
}

impl Gateway {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state: Arc<Mutex<GatewayState>> = Arc::default();
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                std::thread::spawn(move || serve(stream, &state));
            }
        });
        Self { url, state }
    }

    /// Loads the plans of all scenarios (`s<index>/<id>` -> answers) and forgets the
    /// requests seen.
    pub fn reset(&self, scenarios: &[Json]) {
        let mut state = self.state.lock().unwrap();
        state.records.clear();
        state.plan.clear();
        for (i, scenario) in scenarios.iter().enumerate() {
            for (id, answers) in scenario["gateway"].as_object().unwrap() {
                let answers = answers.as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_owned()).collect();
                state.plan.insert(format!("s{i}/{id}"), answers);
            }
        }
    }

    /// The requests seen, in a fixed order (batch clips arrive in parallel).
    pub fn records(&self) -> Vec<String> {
        let mut records: Vec<String> = self.state.lock().unwrap().records.iter().map(Json::to_string).collect();
        records.sort();
        records
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length: usize = headers.iter().find(|(n, _)| n == "content-length").map_or(0, |(_, v)| v.parse().unwrap_or(0));
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { method, path, headers, body })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The parts of a multipart body: their header lines and content.
fn multipart_parts(body: &[u8], boundary: &str) -> Vec<(Vec<String>, Vec<u8>)> {
    let delimiter = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut rest = body;
    let Some(start) = find(rest, delimiter.as_bytes()) else { return parts };
    rest = &rest[start + delimiter.len()..];
    while let Some(after) = rest.strip_prefix(b"\r\n") {
        let end = find(after, format!("\r\n{delimiter}").as_bytes()).unwrap();
        let part = &after[..end];
        let split = find(part, b"\r\n\r\n").unwrap();
        let headers = String::from_utf8_lossy(&part[..split]).split("\r\n").map(str::to_owned).collect();
        parts.push((headers, part[split + 4..].to_vec()));
        rest = &after[end + 2 + delimiter.len()..];
    }
    parts
}

/// A WAV file's chunk ids (PEAK left out: libsndfile's alone), format chunk and
/// float32 samples.
fn read_wav(bytes: &[u8]) -> (Vec<String>, String, Vec<f32>) {
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let (mut ids, mut fmt, mut samples) = (Vec::new(), String::new(), Vec::new());
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = String::from_utf8_lossy(&bytes[at..at + 4]).into_owned();
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let content = &bytes[at + 8..at + 8 + size];
        match id.as_str() {
            "fmt " => fmt = content.iter().map(|b| format!("{b:02x}")).collect(),
            "data" => samples = content.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
            _ => {}
        }
        if id != "PEAK" {
            ids.push(id);
        }
        at += 8 + size + size % 2;
    }
    (ids, fmt, samples)
}

/// The answer `kind` to a transcription request; `refine` for a `verbose_json` one.
fn transcription(kind: &str, id: &str, refine: bool) -> (u16, String) {
    let words =
        json!([{"word": format!("w{id}"), "start": 0.0015, "end": 0.5}, {"word": "б", "start": 1, "end": 1.2345}]);
    let body = match kind {
        "ok" => json!({"text": format!(" text {id} "), "words": words}),
        // A null text in a refine answer would be None in Python's result, which a
        // Transcription cannot hold: null words instead (`words or []`).
        "null" if refine => json!({"text": format!("t {id}"), "words": null}),
        "null" => json!({"text": null}),
        "nowords" => json!({"text": format!("only {id}")}),
        "badword" => json!({"text": "t", "words": [{"word": "x", "start": "abc", "end": 1}]}),
        "strtimes" => json!({"text": "s", "words": [{"word": "y", "start": "0.25", "end": " 1.5 "}]}),
        "list" => json!([]),
        "garbage" => return (200, "not json".into()),
        status if status.starts_with("status:") => return (status[7..].parse().unwrap(), "{}".into()),
        other => panic!("unknown answer {other}"),
    };
    (200, body.to_string())
}

fn health(kind: &str) -> (u16, String) {
    let body = match kind {
        "ok" | "flat_true" => json!({"ok": true}),
        "flat_false" => json!({"ok": false}),
        "truthy" => json!({"ok": "yes"}),
        "engine_true" => json!({"gigaam": {"ok": true}}),
        "other_engine" => json!({"qwen": {"ok": true}}),
        "engine_false_other_true" => json!({"gigaam": {"ok": false}, "qwen": {"ok": true}}),
        "none_ok" => json!({"qwen": {"ok": false}, "x": 1}),
        "list" => json!([1]),
        "garbage" => return (200, "{".into()),
        status if status.starts_with("status:") => return (status[7..].parse().unwrap(), "{}".into()),
        other => panic!("unknown health answer {other}"),
    };
    (200, body.to_string())
}

fn serve(mut stream: TcpStream, state: &Mutex<GatewayState>) {
    let Some(request) = read_request(&stream) else { return };
    let (scenario, rest) = request.path.trim_start_matches('/').split_once('/').unwrap();
    let mut record = json!({"method": request.method, "path": format!("/{rest}"), "scenario": scenario});
    let mut refine = false;
    let key = if rest == "healthz" {
        "healthz".to_owned()
    } else {
        let content_type = request.headers.iter().find(|(n, _)| n == "content-type").unwrap().1.clone();
        let boundary = content_type.strip_prefix("multipart/form-data; boundary=").expect(&content_type).to_owned();
        let mut parts = Vec::new();
        let mut id = String::new();
        for (headers, content) in multipart_parts(&request.body, &boundary) {
            if headers.iter().any(|h| h.contains("filename=")) {
                let (ids, fmt, samples) = read_wav(&content);
                id = format!("{}", (samples[0] * 32768.0).round());
                let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
                let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
                parts.push(json!({"headers": headers, "chunks": ids, "fmt": fmt, "n": samples.len(), "sha": sha}));
            } else {
                parts.push(json!({"headers": headers, "value": String::from_utf8_lossy(&content)}));
            }
        }
        refine = parts.iter().any(|p| p["value"] == "verbose_json");
        record["parts"] = Json::from(parts);
        record["id"] = Json::from(id.clone());
        id
    };
    let answer = {
        let mut state = state.lock().unwrap();
        state.records.push(record);
        state.plan.get_mut(&format!("{scenario}/{key}")).and_then(VecDeque::pop_front).unwrap_or_else(|| "ok".into())
    };
    let (status, body) = if key == "healthz" {
        health(&answer)
    } else if answer == "slow" {
        std::thread::sleep(Duration::from_secs_f64(SLOW_S));
        transcription("ok", &key, refine)
    } else {
        transcription(&answer, &key, refine)
    };
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

// ---- the Rust side ----

/// The fake local model: `local <id> ` for every clip.
struct FakeEngine;

impl Engine for FakeEngine {
    fn transcribe(&self, pcm: &[f32]) -> Result<EngineAnswer, AsrError> {
        Ok((format!("local {} ", (pcm[0] * 32768.0).round()), Vec::new()))
    }

    fn device(&self) -> String {
        "fake".into()
    }
}

/// `RemoteASRError: <call> failed: <ureq's text>` with the text cut, as the Python
/// script does before the rate limit adds its suffix.
fn normalize(line: &str) -> String {
    let Some((head, tail)) = line.split_once("RemoteASRError: ") else { return line.to_owned() };
    let Some((call, detail)) = tail.split_once(" failed: ") else { return line.to_owned() };
    let suffix = detail.rfind(" (+").filter(|_| detail.ends_with(" suppressed)")).map_or("", |at| &detail[at..]);
    format!("{head}RemoteASRError: {call} failed: <detail>{suffix}")
}

fn outcome<T>(result: Result<T, AsrError>, ok: impl FnOnce(T) -> Json) -> Json {
    match result {
        Ok(v) => json!({"ok": ok(v)}),
        Err(AsrError::TemporarilyUnavailable) => json!("unavailable"),
        Err(_) => json!("error"),
    }
}

pub fn run_rust(url: &str, index: usize, scenario: &Json) -> Json {
    let config = &scenario["config"];
    let clock = ManualClock::new(0.0);
    clock.advance(1000.0);
    let (logger, captured) = Logger::capture(clock.shared());
    let (gate, outcomes) = mpsc::channel::<String>();
    let outcomes = Mutex::new(outcomes);
    let loader: Loader = Box::new(move || match outcomes.lock().unwrap().recv() {
        Ok(outcome) if outcome == "ok" => Ok(Arc::new(FakeEngine) as Arc<dyn Engine>),
        _ => Err(AsrError::Failed("weights missing".into())),
    });
    let local = Arc::new(WavoAsr::new(loader, logger.clone()));
    let client = RemoteAsrClient::new(
        &format!("{url}/s{index}/"),
        config["model"].as_str().unwrap(),
        config["timeout_s"].as_f64().unwrap(),
        Some(config["live_timeout_s"].as_f64().unwrap()),
    );
    let fb = FallbackAsr::new(
        client,
        local,
        config["fails"].as_i64().unwrap(),
        config["open_s"].as_f64().unwrap(),
        clock.shared(),
        logger,
    );
    let asr: &dyn LiveAsr = &fb;
    let mut steps = Vec::new();
    for op in scenario["ops"].as_array().unwrap() {
        let kind = op["op"].as_str().unwrap();
        let result = match kind {
            "load" => outcome(asr.load_model(), |()| Json::Null),
            "healthz" => json!(fb.remote().healthz()),
            "chunk" => {
                let pcm = common::f32_from_b64(op["pcm"].as_str().unwrap());
                outcome(asr.transcribe_chunk(&pcm), Json::from)
            }
            "batch" => {
                let clips: Vec<Vec<i16>> =
                    op["clips"].as_array().unwrap().iter().map(|c| common::i16_from_b64(c.as_str().unwrap())).collect();
                let refs: Vec<&[i16]> = clips.iter().map(Vec::as_slice).collect();
                outcome(asr.transcribe_batch(&refs), Json::from)
            }
            "refine" => {
                let pcm = common::i16_from_b64(op["pcm"].as_str().unwrap());
                outcome(asr.transcribe_with_timestamps(&pcm, None), |r| {
                    let words: Vec<Json> = r.words.iter().map(|w| json!([w.text, w.start, w.end])).collect();
                    json!({"text": r.text, "words": words})
                })
            }
            "advance" => {
                clock.advance(op["s"].as_f64().unwrap());
                Json::Null
            }
            "local" => {
                let loading = fb.local_loading();
                if loading {
                    gate.send(op["outcome"].as_str().unwrap().to_owned()).unwrap();
                    fb.wait_local_ready(Duration::from_secs(10));
                }
                json!(loading)
            }
            other => panic!("unknown op {other}"),
        };
        steps.push(json!({"op": kind, "result": result, "available": asr.available()}));
    }
    let log: Vec<String> =
        captured.lines().iter().filter(|l| l.starts_with("[FallbackASR]")).map(|l| normalize(l)).collect();
    json!({"steps": steps, "log": log})
}

// ---- scenarios ----

/// A clip whose first sample is `id` (the gateway's name for it).
fn clip(id: i64, len: usize) -> Vec<i16> {
    let mut rng = Rng::new(id as u64);
    let mut pcm: Vec<i16> = (0..len).map(|_| rng.range(-12000, 12000) as i16).collect();
    pcm[0] = id as i16;
    pcm
}

fn chunk_op(id: i64) -> Json {
    let pcm: Vec<f32> = clip(id, 1600).iter().map(|&v| f32::from(v) / 32768.0).collect();
    json!({"op": "chunk", "pcm": common::f32_b64(&pcm)})
}

fn batch_op(ids: &[i64]) -> Json {
    let clips: Vec<String> = ids.iter().map(|&id| common::i16_b64(&clip(id, 800 + (id as usize % 5) * 160))).collect();
    json!({"op": "batch", "clips": clips})
}

fn refine_op(id: i64) -> Json {
    json!({"op": "refine", "pcm": common::i16_b64(&clip(id, 3200))})
}

fn config(fails: i64, open_s: f64) -> Json {
    json!({"model": "gigaam", "timeout_s": 10.0, "live_timeout_s": 0.8, "fails": fails, "open_s": open_s})
}

const ANSWERS: [&str; 12] =
    ["ok", "ok", "ok", "ok", "status:503", "status:404", "garbage", "list", "null", "nowords", "badword", "strtimes"];

pub fn scenario(seed: u64) -> Json {
    let mut rng = Rng::new(seed);
    let mut next_id = 1i64;
    let mut chunk_ids: Vec<i64> = Vec::new();
    let mut gateway = serde_json::Map::new();
    let mut fresh = |rng: &mut Rng, gateway: &mut serde_json::Map<String, Json>| {
        let id = next_id;
        next_id += 1;
        let n = rng.range(1, 3);
        let answers: Vec<Json> = (0..n).map(|_| json!(ANSWERS[rng.range(0, ANSWERS.len() as i64) as usize])).collect();
        gateway.insert(id.to_string(), Json::from(answers));
        id
    };
    let mut ops = vec![json!({"op": "load"})];
    for _ in 0..rng.range(15, 35) {
        let roll = rng.uniform();
        let op = if roll < 0.3 {
            let id = if !chunk_ids.is_empty() && rng.uniform() < 0.25 {
                chunk_ids[rng.range(0, chunk_ids.len() as i64) as usize]
            } else {
                let id = fresh(&mut rng, &mut gateway);
                chunk_ids.push(id);
                id
            };
            chunk_op(id)
        } else if roll < 0.5 {
            let ids: Vec<i64> = (0..rng.range(1, 5)).map(|_| fresh(&mut rng, &mut gateway)).collect();
            batch_op(&ids)
        } else if roll < 0.7 {
            refine_op(fresh(&mut rng, &mut gateway))
        } else if roll < 0.85 {
            let s = [1.0, 20.0, 31.0, 59.0, 61.0, 301.0][rng.range(0, 6) as usize];
            json!({"op": "advance", "s": s})
        } else if roll < 0.95 {
            json!({"op": "local", "outcome": if rng.uniform() < 0.5 { "ok" } else { "fail" }})
        } else {
            json!({"op": "healthz"})
        };
        ops.push(op);
    }
    let fails = rng.range(1, 5);
    let open_s = [30.0, 60.0][rng.range(0, 2) as usize];
    json!({"config": config(fails, open_s), "gateway": gateway, "ops": ops})
}

/// Hand-written: timeouts on the live path only, the health probe's answers, a
/// recovery probe closing the circuit, the failures' window sliding.
pub fn handwritten() -> Vec<Json> {
    let healthz: Vec<Json> = [
        "flat_true",
        "flat_false",
        "truthy",
        "engine_true",
        "other_engine",
        "engine_false_other_true",
        "none_ok",
        "list",
        "garbage",
        "status:503",
    ]
    .iter()
    .map(|&k| json!(k))
    .collect();
    let mut health_ops = vec![json!({"op": "load"})];
    health_ops.extend((1..healthz.len()).map(|_| json!({"op": "healthz"})));
    let timeouts = json!({
        "config": config(2, 60.0),
        "gateway": {"1": ["slow"], "2": ["slow"], "3": ["slow", "ok"], "4": ["status:503"]},
        "ops": [chunk_op(1), refine_op(2), batch_op(&[3, 4]), batch_op(&[3]), json!({"op": "local", "outcome": "ok"}),
                chunk_op(1), json!({"op": "advance", "s": 61.0}), chunk_op(1)],
    });
    let recovery = json!({
        "config": config(3, 30.0),
        "gateway": {"1": ["status:503", "status:503", "status:503", "status:503", "ok"],
                    "2": ["status:503", "status:503", "status:503", "status:503", "status:503"]},
        "ops": [chunk_op(1), json!({"op": "advance", "s": 59.0}), chunk_op(1), chunk_op(1),
                json!({"op": "advance", "s": 2.0}), chunk_op(1), chunk_op(1), chunk_op(1),
                json!({"op": "advance", "s": 30.0}), chunk_op(1), chunk_op(1), json!({"op": "advance", "s": 31.0}),
                chunk_op(1), chunk_op(1), refine_op(2), refine_op(2),
                json!({"op": "local", "outcome": "fail"}), json!({"op": "advance", "s": 100.0}), refine_op(2),
                json!({"op": "advance", "s": 201.0}), refine_op(2), json!({"op": "local", "outcome": "ok"}),
                refine_op(2)],
    });
    vec![json!({"config": config(3, 60.0), "gateway": {"healthz": healthz}, "ops": health_ops}), timeouts, recovery]
}

/// Every scenario, the hand-written ones first, in the order their answers are kept.
pub fn scenarios() -> Vec<Json> {
    let mut scenarios = handwritten();
    scenarios.extend((0..SEEDS).map(scenario));
    scenarios
}

/// The SHA-256 of the scenarios as JSON: the recorded answers hold only for these.
pub fn scenarios_sha256(scenarios: &[Json]) -> String {
    Sha256::digest(Json::from(scenarios.to_vec()).to_string().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The Python answers, recorded by `RECOG_RECORD_REMOTE_ASR_FIXTURE=1 cargo test -p
/// recog-live --test asr_remote_parity` on a checkout that still has the Python side
/// (the commit "Freeze the Python answers of the remote ASR parity test").
pub fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/remote_asr_python.json")
}

/// The answers reach every state worth comparing.
pub fn assert_every_state_reached(answers: &Json) {
    let text = answers.to_string();
    for wanted in [
        "circuit OPEN",
        "circuit CLOSED",
        "(+",
        "local GigaAM ready",
        "local GigaAM load FAILED",
        "\"unavailable\"",
        "\"error\"",
        "\"local ",
        "\"words\":[[",
    ] {
        assert!(text.contains(wanted), "no scenario reached {wanted}");
    }
}
