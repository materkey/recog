//! `WavoAsr` against `recog/wavo_backend.py` on the same input.
//!
//! - `words_from_tokens` on seeded random token sequences, always.
//! - The real model, opt-in with `RECOG_LIVE_ASR_PARITY=1`: it needs the GPU and the
//!   cached `gigaam-v3` (never downloaded here), and `wavo_asr` in the interpreter. The
//!   same clips go through Python's `WavoASR` on the `wavo_asr` module and through Rust's
//!   `WavoAsr` on the crate: synthetic clips, and windows of the local example recordings
//!   under `.local/examples` when they exist (read in place, never copied).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use common::Rng;
use recog_live::asr::wavo::Loader;
use recog_live::asr::{Clip, Engine, WavoAsr, WavoEngine, words_from_tokens};
use recog_live::log::Logger;
use serde_json::{Value as Json, json};

/// Pieces that exercise the word starts, Python's whitespace and non-ASCII text.
const PIECES: &[&str] = &[
    "▁",
    "▁▁",
    "▁При",
    "вет",
    "▁мир",
    "a",
    "▁b",
    " ",
    "\u{3000}",
    "▁\u{1c}",
    "\u{200b}",
    "▁😀",
    "ё",
    "",
    "▁ x ",
    "\t",
    "▁,",
    ".",
    "▁ ",
    "\u{a0}",
];

fn random_case(rng: &mut Rng) -> Vec<(String, u32)> {
    let n = rng.range(0, 25);
    let mut start = 0u32;
    (0..n)
        .map(|_| {
            start += u32::try_from(rng.range(0, 4)).unwrap() * 40;
            (PIECES[usize::try_from(rng.range(0, PIECES.len() as i64)).unwrap()].to_owned(), start)
        })
        .collect()
}

#[test]
fn random_token_sequences_words_from_tokens_compared_then_python_and_rust_give_the_same_words() {
    let Some(python) = common::python() else { return };
    let sandbox = tempfile::tempdir().unwrap();
    let mut cases = Vec::new();
    for seed in 0..50 {
        let mut rng = Rng::new(seed);
        for _ in 0..40 {
            cases.push(random_case(&mut rng));
        }
    }
    let request = json!({ "cases": cases });
    let answer = common::run_python(&python, "asr_words.py", &request, sandbox.path());
    let answer = answer.as_array().unwrap();
    assert_eq!(answer.len(), cases.len());
    for (case, expected) in cases.iter().zip(answer) {
        let ours: Vec<Json> = words_from_tokens(case).into_iter().map(|w| json!([w.text, w.start, w.end])).collect();
        assert_eq!(&Json::Array(ours), expected, "tokens {case:?}");
    }
}

/// The samples of a 16 kHz mono Int16 WAV from `start_s` for `len_s`, as float32.
fn wav_window(path: &Path, start_s: f64, len_s: f64) -> Option<Vec<f32>> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let (mut format_ok, mut data) = (false, None);
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = bytes.get(at + 8..(at + 8 + size).min(bytes.len()))?;
        if id == b"fmt " {
            let channels = u16::from_le_bytes(body.get(2..4)?.try_into().ok()?);
            let rate = u32::from_le_bytes(body.get(4..8)?.try_into().ok()?);
            let bits = u16::from_le_bytes(body.get(14..16)?.try_into().ok()?);
            format_ok = channels == 1 && rate == 16000 && bits == 16;
        } else if id == b"data" {
            data = Some(body);
        }
        at += 8 + size + size % 2;
    }
    let data = data.filter(|_| format_ok)?;
    let samples: Vec<i16> = data.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (from, to) = ((start_s * 16000.0) as usize, ((start_s + len_s) * 16000.0) as usize);
    let window = samples.get(from..to.min(samples.len()))?;
    Some(window.iter().map(|&v| f32::from(v) / 32768.0).collect())
}

fn synthetic_clips() -> Vec<(String, Vec<f32>)> {
    let n = 3 * 16000;
    let silence = vec![0.0f32; n];
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let tone: Vec<f32> =
        (0..n).map(|i| (0.3 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16000.0).sin()) as f32).collect();
    let mut rng = Rng::new(7);
    #[allow(clippy::cast_possible_truncation)]
    let noise: Vec<f32> = (0..n).map(|_| ((rng.uniform() - 0.5) * 0.1) as f32).collect();
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let chirp: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 / 16000.0;
            (0.2 * (2.0 * std::f64::consts::PI * (200.0 * t + 300.0 * t * t)).sin()) as f32
        })
        .collect();
    vec![("silence".into(), silence), ("tone".into(), tone), ("noise".into(), noise), ("chirp".into(), chirp)]
}

fn example_clips() -> Vec<(String, Vec<f32>)> {
    let examples = PathBuf::from("/Users/vkkovalev/projects/recog/.local/examples");
    let windows =
        [("example_5min.wav", [0.0, 60.0, 125.0, 200.0, 270.0]), ("vlad_sample.wav", [0.0, 15.0, 30.0, 40.0, 45.0])];
    let mut clips = Vec::new();
    for (name, starts) in windows {
        for start in starts {
            let len = if start < 50.0 { 15.0 } else { 25.0 };
            if let Some(samples) = wav_window(&examples.join(name), start, len) {
                clips.push((format!("{name}@{start}s+{len}s"), samples));
            }
        }
    }
    clips
}

fn python_has_wavo(python: &Path) -> bool {
    Command::new(python)
        .args(["-c", "import wavo_asr"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn same_clips_real_model_compared_then_python_and_rust_give_the_same_text_and_words() {
    if std::env::var_os("RECOG_LIVE_ASR_PARITY").is_none_or(|v| v != "1") {
        eprintln!("real-model ASR parity skipped: set RECOG_LIVE_ASR_PARITY=1 (needs the GPU and the cached model)");
        return;
    }
    let Some(python) = common::python() else { return };
    let Some(model_path) = WavoEngine::cached_path() else {
        eprintln!("real-model ASR parity skipped: gigaam-v3 is not in the Hugging Face cache");
        return;
    };
    if !python_has_wavo(&python) {
        eprintln!("real-model ASR parity skipped: {} cannot import wavo_asr", python.display());
        return;
    }
    let clips: Vec<(String, Vec<f32>)> = synthetic_clips().into_iter().chain(example_clips()).collect();

    // Python finds the same cached file: HF_HUB_CACHE is the cache Rust found it in, so
    // its wavo_asr.Model() loads without downloading.
    let hub_cache = wavo::hub::Hub::new().root().to_path_buf();
    let request = json!({ "clips": clips.iter().map(|(_, s)| common::f32_b64(s)).collect::<Vec<_>>() });
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/asr_model.py");
    let mut child = Command::new(&python)
        .arg(&script)
        .arg(common::repo_root())
        .env("HF_HUB_CACHE", &hub_cache)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), request.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "asr_model.py failed");
    let answers: Vec<Json> = serde_json::from_slice(&output.stdout).unwrap();

    let path = model_path.clone();
    let loader: Loader = Box::new(move || Ok(Arc::new(WavoEngine::load(&path)?) as Arc<dyn Engine>));
    let asr = WavoAsr::new(loader, Logger::stderr());
    let (mut same_text, mut same_words, mut same_chunk) = (0, 0, 0);
    let mut differences = Vec::new();
    for ((name, samples), expected) in clips.iter().zip(&answers) {
        let result = asr.transcribe_with_timestamps(Clip::F32(samples), 16000).unwrap();
        let chunk = asr.transcribe_chunk(Clip::F32(samples), 16000);
        let words: Vec<Json> = result.words.iter().map(|w| json!([w.text, w.start, w.end])).collect();
        let text_ok = json!(result.text) == expected["text"];
        let words_ok = Json::Array(words) == expected["words"];
        let chunk_ok = json!(chunk) == expected["chunk"];
        same_text += usize::from(text_ok);
        same_words += usize::from(words_ok);
        same_chunk += usize::from(chunk_ok);
        if !(text_ok && words_ok && chunk_ok) {
            differences.push(format!("{name}: rust {:?} python {}", result.text, expected["text"]));
        }
        // The count only: the examples are personal recordings.
        eprintln!("{name}: {} words", result.words.len());
    }
    eprintln!(
        "real-model ASR parity on {} clips ({}): text {same_text}, words {same_words}, transcribe_chunk {same_chunk} identical",
        clips.len(),
        model_path.display()
    );
    assert!(differences.is_empty(), "{differences:#?}");
}
