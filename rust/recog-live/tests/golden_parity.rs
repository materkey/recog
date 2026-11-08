//! The golden recorder against `recog/golden.py` on the same scripted sessions.
//!
//! Each seed scripts a session (connects, frames, captures of every dtype, ASR answers
//! of every shape, readings of both clocks chosen to stress `round` and `repr`). Rust
//! records it, Python records it, and the two directories must be identical byte for
//! byte. Python's `read_session` and the player's `ReplayASR` then read the Rust
//! recording back: the same events, samples and answers as Rust's `read_session`.

mod common;

use std::sync::Mutex;

use common::Rng;
use recog_core::pyjson::{self, Value};
use recog_live::asr::{Transcription, Word};
use recog_live::clock::ManualClock;
use recog_live::golden::{AUDIO, AsrOutput, Audio, AudioData, EVENTS, GoldenRecorder, audio_key, read_session};
use recog_live::log::Logger;
use serde_json::{Value as Json, json};

const FRAMES: &[&str] = &[
    r#"{"type": "speaker_update", "speaker": {"id": "a", "name": "Алиса"}}"#,
    r#"{"type": "participants_update", "participants": [{"id": "x:1", "name": "Bob 😀", "micMuted": false}], "localMicMuted": null}"#,
    r#"{"type": "clock_sync", "client_ts": 1731000000123.5, "offset": -0.000001, "big": 123456789012345678901234567890}"#,
    r#"{"type": "x", "values": [NaN, Infinity, -Infinity, -0.0, 1e16, 1e-5, 0.1, 5e-324, 1.7976931348623157e308]}"#,
    r#"{"a": 1, "b": 2, "a": 3}"#,
    r#"{"text": "line\nbreak\ttab \u0001   \"quoted\" \\ slash /", "empty": {}, "list": []}"#,
    r#"{"nested": {"deep": {"deeper": [1, [2, [3, {"k": true}]]]}}, "n": null}"#,
    r#"{"type": "pcm_chunk", "pid": "p1", "seq": 17, "pcm": "AAEC", "chunk_start_ts": 1731000000.25}"#,
    r"[]",
    r#""just a string""#,
];

const TEXTS: &[&str] =
    &["", " ", "привет", "Привет мир", "a\"b\\c", "emoji 😀 ✓", "\u{3000}пробел\u{a0}", "tab\tnew\nline"];

#[derive(Clone)]
enum Arr {
    F32(Vec<f32>),
    I16(Vec<i16>),
    F64(Vec<f64>),
}

impl Arr {
    fn audio(&self) -> Audio<'_> {
        match self {
            Arr::F32(s) => Audio::F32(s),
            Arr::I16(s) => Audio::I16(s),
            Arr::F64(s) => Audio::F64(s),
        }
    }

    fn spec(&self) -> Json {
        let audio = self.audio();
        json!([audio.dtype(), common::b64(&audio.bytes())])
    }
}

enum Out {
    None,
    Text(String),
    Texts(Vec<String>),
    Timed(Transcription),
}

enum Op {
    Connect(i64),
    Disconnect(i64),
    Frame(i64, &'static str),
    Capture(String, Arr, f64),
    Asr(String, bool, Vec<Arr>, Out),
    Close,
}

fn random_array(rng: &mut Rng) -> Arr {
    let n = usize::try_from(rng.range(0, 300)).unwrap();
    #[allow(clippy::cast_possible_truncation)]
    match rng.range(0, 3) {
        0 => Arr::F32(
            (0..n)
                .map(|i| match i % 97 {
                    13 => f32::NAN,
                    29 => -0.0,
                    31 => f32::INFINITY,
                    _ => (rng.uniform() * 2.0 - 1.0) as f32,
                })
                .collect(),
        ),
        1 => Arr::I16((0..n).map(|_| i16::try_from(rng.range(-32768, 32768)).unwrap()).collect()),
        _ => Arr::F64((0..n).map(|_| rng.uniform() * 2.0 - 1.0).collect()),
    }
}

fn random_text(rng: &mut Rng) -> String {
    TEXTS[usize::try_from(rng.range(0, TEXTS.len() as i64)).unwrap()].to_owned()
}

/// An answer of the shape `call` gives, as the live process records it.
fn random_output(rng: &mut Rng, call: &str, clips: usize) -> Out {
    match call {
        "transcribe_chunk" if rng.range(0, 3) == 0 => Out::None,
        "transcribe_chunk" => Out::Text(random_text(rng)),
        "transcribe_batch" => Out::Texts((0..clips).map(|_| random_text(rng)).collect()),
        _ => Out::Timed(Transcription {
            text: random_text(rng),
            words: (0..rng.range(0, 5))
                .map(|_| Word { text: random_text(rng), start: rng.range(-40, 40_000), end: rng.range(0, 40_000) })
                .collect(),
        }),
    }
}

fn random_ops(rng: &mut Rng, n: usize) -> Vec<Op> {
    let mut ops: Vec<Op> = (0..n)
        .map(|_| {
            let client = rng.range(1, 4);
            match rng.range(0, 12) {
                0 => Op::Connect(client),
                1 => Op::Disconnect(client),
                2..=4 => Op::Frame(client, FRAMES[usize::try_from(rng.range(0, FRAMES.len() as i64)).unwrap()]),
                5..=7 => {
                    let source = if rng.range(0, 2) == 0 { "mic" } else { "system" };
                    Op::Capture(source.into(), random_array(rng), 1.7e9 + rng.uniform() * 1e6)
                }
                _ => {
                    let call = ["transcribe_chunk", "transcribe_batch", "transcribe_with_timestamps"]
                        [usize::try_from(rng.range(0, 3)).unwrap()];
                    // A batch is a list of clips (possibly empty), the others one array.
                    let single = call != "transcribe_batch";
                    let count = if single { 1 } else { usize::try_from(rng.range(0, 4)).unwrap() };
                    let clips: Vec<Arr> = (0..count).map(|_| random_array(rng)).collect();
                    let out = random_output(rng, call, count);
                    Op::Asr(call.into(), single, clips, out)
                }
            }
        })
        .collect();
    if rng.range(0, 3) == 0 {
        // A close in the middle: later events are dropped by both.
        let at = usize::try_from(rng.range(0, n as i64)).unwrap();
        ops.insert(at, Op::Close);
    }
    ops
}

/// Readings that stress `round(x, 6)` (ties at the sixth decimal, carries) and `repr`.
fn random_clock(rng: &mut Rng, n: usize) -> (Vec<f64>, Vec<f64>) {
    let start = [100.0, 0.0, 12_345.678_901_234, 1e6 + 0.1][usize::try_from(rng.range(0, 4)).unwrap()];
    let mut now = start;
    let mut clock = vec![start];
    let mut wall = Vec::new();
    for i in 0..n {
        now += match rng.range(0, 5) {
            0 => 0.5,
            #[allow(clippy::cast_precision_loss)]
            1 => (2 * rng.range(0, 64) + 1) as f64 / 128.0,
            2 => 1e-7,
            3 => 0.000_000_5,
            _ => rng.uniform() * 3.0,
        };
        clock.push(now);
        #[allow(clippy::cast_precision_loss)]
        wall.push(match i % 6 {
            0 => 1_700_000_000.0,
            1 => 1.7e9 + rng.uniform() * 1e6,
            2 => 0.1,
            3 => 1e16,
            4 => 1e-5,
            _ => rng.range(0, 1_000_000_000) as f64 / 1e3,
        });
    }
    (clock, wall)
}

fn out_json(out: &Out) -> Json {
    match out {
        Out::None => Json::Null,
        Out::Text(t) => json!(t),
        Out::Texts(ts) => json!(ts),
        Out::Timed(r) => {
            json!({"text": r.text, "words": r.words.iter().map(|w| json!([w.text, w.start, w.end])).collect::<Vec<_>>()})
        }
    }
}

fn op_json(op: &Op) -> Json {
    match op {
        Op::Connect(c) => json!(["connect", c]),
        Op::Disconnect(c) => json!(["disconnect", c]),
        Op::Frame(c, text) => json!(["frame", c, text]),
        Op::Capture(source, arr, ts) => json!(["capture", source, arr.spec(), ts]),
        Op::Asr(call, single, clips, out) => {
            json!(["asr", call, single, clips.iter().map(Arr::spec).collect::<Vec<_>>(), out_json(out)])
        }
        Op::Close => json!(["close"]),
    }
}

fn record_rust(dir: &std::path::Path, ops: &[Op], clock: &[f64], wall: &[f64]) {
    let clock = Mutex::new(Vec::from(clock).into_iter());
    let wall = Mutex::new(Vec::from(wall).into_iter());
    let (logger, _) = Logger::capture(ManualClock::new(0.0).shared());
    let recorder = GoldenRecorder::with_clocks(
        dir,
        Box::new(move || clock.lock().unwrap().next().unwrap()),
        Box::new(move || wall.lock().unwrap().next().unwrap()),
        logger,
    )
    .unwrap();
    for op in ops {
        match op {
            Op::Connect(c) => recorder.client_connected(*c),
            Op::Disconnect(c) => recorder.client_disconnected(*c),
            Op::Frame(c, text) => recorder.frame(*c, &pyjson::loads(text).unwrap()),
            Op::Capture(source, arr, ts) => recorder.capture(source, arr.audio(), *ts),
            Op::Asr(call, _, clips, out) => {
                let audio: Vec<Audio<'_>> = clips.iter().map(Arr::audio).collect();
                let output = match out {
                    Out::None => AsrOutput::None,
                    Out::Text(t) => AsrOutput::Text(t),
                    Out::Texts(ts) => AsrOutput::Texts(ts),
                    Out::Timed(r) => AsrOutput::Timed(r),
                };
                recorder.asr(call, &audio, output);
            }
            Op::Close => recorder.close(),
        }
    }
    recorder.close();
}

fn audio_spec(audio: &AudioData) -> Json {
    let audio = audio.as_audio();
    json!([audio.dtype(), common::b64(&audio.bytes())])
}

/// What `ReplayASR` keeps of a session's ASR events, by key, as JSON text.
fn replay_answers(events: &[recog_live::golden::Event]) -> serde_json::Map<String, Json> {
    let mut answers = serde_json::Map::new();
    for event in events {
        if event.get("kind").and_then(Value::as_str) != Some("asr") {
            continue;
        }
        let Some(Value::Array(inputs)) = event.get("inputs") else { panic!("asr without inputs") };
        let output = event.get("output").unwrap();
        if event.get("call").and_then(Value::as_str) == Some("transcribe_batch") {
            let Value::Array(texts) = output else { panic!("a batch answer that is not a list") };
            for (key, text) in inputs.iter().zip(texts) {
                answers.insert(key.as_str().unwrap().to_owned(), json!(pyjson::dumps(text, None)));
            }
        } else if let Some(key) = inputs.first() {
            answers.insert(key.as_str().unwrap().to_owned(), json!(pyjson::dumps(output, None)));
        }
    }
    answers
}

#[test]
fn scripted_sessions_recorded_by_both_then_files_are_identical_and_python_reads_rust_back() {
    let Some(python) = common::python() else { return };
    let (mut sessions, mut events_total, mut bytes_total) = (0, 0, 0);
    for seed in 0..30 {
        let mut rng = Rng::new(seed);
        let ops = random_ops(&mut rng, 80);
        let (clock, wall) = random_clock(&mut rng, ops.len() + 1);
        let arrays: Vec<Arr> = (0..5).map(|_| random_array(&mut rng)).collect();
        let root = tempfile::tempdir().unwrap();
        let (rust_dir, python_dir) = (root.path().join("rust"), root.path().join("python"));
        record_rust(&rust_dir, &ops, &clock, &wall);

        let request = json!({
            "record_dir": python_dir,
            "read_dir": rust_dir,
            "clock": clock,
            "wall": wall,
            "ops": ops.iter().map(op_json).collect::<Vec<_>>(),
            "arrays": arrays.iter().map(Arr::spec).collect::<Vec<_>>(),
        });
        let answer = common::run_python(&python, "golden_record.py", &request, &root.path().join("sandbox"));

        for name in [EVENTS, AUDIO] {
            let ours = std::fs::read(rust_dir.join(name)).unwrap();
            let theirs = std::fs::read(python_dir.join(name)).unwrap();
            if ours != theirs {
                let first = ours.iter().zip(&theirs).position(|(a, b)| a != b).unwrap_or(ours.len().min(theirs.len()));
                panic!(
                    "seed {seed}: {name} differs at byte {first}:\nrust   {}\npython {}",
                    String::from_utf8_lossy(&ours[first.saturating_sub(80)..(first + 80).min(ours.len())]),
                    String::from_utf8_lossy(&theirs[first.saturating_sub(80)..(first + 80).min(theirs.len())]),
                );
            }
            bytes_total += ours.len();
        }

        let ours = read_session(&rust_dir).unwrap();
        let theirs = answer["read"].as_array().unwrap();
        assert_eq!(ours.len(), theirs.len(), "seed {seed}");
        for (event, expected) in ours.iter().zip(theirs) {
            assert_eq!(json!(pyjson::dumps(&Value::Object(event.fields.clone()), None)), expected[0], "seed {seed}");
            assert_eq!(event.audio.as_ref().map_or(Json::Null, audio_spec), expected[1], "seed {seed}");
        }
        assert_eq!(Json::Object(replay_answers(&ours)), answer["replay"], "seed {seed}");
        let keys: Vec<Json> = arrays.iter().map(|a| json!(audio_key(a.audio()))).collect();
        assert_eq!(Json::Array(keys), answer["keys"], "seed {seed}");
        sessions += 1;
        events_total += ours.len();
    }
    eprintln!("golden parity: {sessions} sessions, {events_total} events, {bytes_total} bytes identical");
}

/// `float.hex()`: `[-]0x<0|1>.<13 hex digits>p<exp>`, zero as `0x0.0p+0`.
fn hex_float(x: f64) -> String {
    let bits = x.to_bits();
    let sign = if bits >> 63 == 1 { "-" } else { "" };
    let exponent = i64::try_from((bits >> 52) & 0x7ff).unwrap();
    let mantissa = bits & ((1 << 52) - 1);
    match exponent {
        0 if mantissa == 0 => format!("{sign}0x0.0p+0"),
        0 => format!("{sign}0x0.{mantissa:013x}p-1022"),
        _ => format!("{sign}0x1.{mantissa:013x}p{:+}", exponent - 1023),
    }
}

#[test]
fn many_floats_round6_compared_then_python_round_gives_the_same_float() {
    let Some(python) = common::python() else { return };
    let mut rng = Rng::new(42);
    let mut values: Vec<f64> = Vec::new();
    #[allow(clippy::cast_precision_loss)]
    for k in 0..2000 {
        values.push((2 * k + 1) as f64 / 128.0);
        values.push((2 * k + 1) as f64 / 2f64.powi(21));
        values.push(rng.uniform() * 10f64.powi(i32::try_from(rng.range(-9, 18)).unwrap()));
        values.push(-rng.uniform() * 1000.0);
        let x = f64::from_bits(rng.next_u64());
        if x.is_finite() {
            values.push(x);
        }
    }
    values.extend([0.0, -0.0, 1e-7, 5e-7, 0.999_999_5, 9.999_999_5, 1e300, 5e-324, f64::MAX]);
    let sandbox = tempfile::tempdir().unwrap();
    let request = json!({ "values": values.iter().map(|&v| hex_float(v)).collect::<Vec<_>>() });
    let answer = common::run_python(&python, "golden_round.py", &request, sandbox.path());
    let answer = answer.as_array().unwrap();
    for (x, expected) in values.iter().zip(answer) {
        let ours = hex_float(recog_live::golden::round6(*x));
        assert_eq!(json!(ours), *expected, "round({x:e}, 6)");
    }
    eprintln!("round6 parity: {} values", values.len());
}
