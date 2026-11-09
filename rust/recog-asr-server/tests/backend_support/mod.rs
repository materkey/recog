//! The GigaAM backend's steps over one fake model and one fake ffmpeg, which
//! tests/backend_frozen.rs replays against the answers the Python backend gave to
//! them, recorded into tests/fixtures/backend_python.json by the differential test the
//! Python side went with.
//!
//! The model answers from one scenarios file by the number of frames of the WAV ffmpeg
//! wrote; ffmpeg is a script that copies its input (or fails on input starting with
//! `BAD`, or is killed on `SIG`) and logs its command line beside the temporary
//! directory. Each step's answer (status, the headers that matter, body) is recorded,
//! and so are ffmpeg's command lines.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use recog_asr_server::backend::{self, Backend};
use recog_asr_server::config::{BackendConfig, Environ};
use recog_asr_server::engine::{Longform, Span, Transcriber};
use recog_core::pyjson::{self, Value};
use serde_json::{Value as Json, json};

use crate::support::{self, Part, field, file, multipart, try_request};

pub const FFMPEG: &str = r#"#!/bin/sh
# ffmpeg -y -i SRC -ar 16000 -ac 1 -c:a pcm_s16le DST, as the backend calls it.
src=$3
dst=${10}
printf '%s %s %s %s %s %s %s %s %s %s\n' "$1" "$2" "$(basename "$src")" "$4" "$5" "$6" "$7" "$8" "$9" "$(basename "$dst")" >> "$(dirname "$src").log"
case "$(head -c 3 "$src")" in
BAD) printf 'Invalid data found when processing input\n' >&2; exit 1 ;;
SIG) kill -9 $$ ;;
esac
cp "$src" "$dst"
"#;

/// The scenarios by frame count: segments and words as `[text, start, end]`, or a
/// failure of the model.
pub const SCENARIOS: &str = r#"{
  "1600": {"segments": [["  Привет, мир.  ", 0.0, 1.5], ["Второй", 1.5, 3.25]],
           "words": [["Привет,", 0.08, 0.52], ["мир.", 0.6, 1.0], ["Второй", 1.6, 2.04]]},
  "1601": {"segments": [["час", 3599.9995, 3725.999], ["int", 1, 2], ["neg", -1.5, -0.25],
                        ["sum", 0.30000000000000004, 1e20], ["tiny", 1e-7, 59.9999999]],
           "words": [["час", 3599.9995, 3600.5]]},
  "1602": {"segments": [], "words": []},
  "1603": {"segments": [["", 0.0, 0.5], ["  ", 0.5, 1.0], ["\tтаб\n", 1.0, 2.0]]},
  "1604": {"segments": [["x", NaN, 1.0]], "words": [["x", 0.0, Infinity]]},
  "1605": {"fail": "decoder exploded"},
  "1606": {"segments": [["\"кавычки\" и \\ слэш", 0.04, 0.12]], "words": [["\"кавычки\"", 0.04, 0.08]]}
}"#;

/// The model of the scenarios.
struct FakeModel {
    scenarios: Value,
}

fn spans(value: Option<&Value>) -> Vec<Span> {
    let number = |v: &Value| match v {
        Value::Int(i) => *i as f64,
        Value::Float(x) => *x,
        other => panic!("not a number: {other:?}"),
    };
    let Some(Value::Array(rows)) = value else { return Vec::new() };
    rows.iter()
        .map(|row| {
            let Value::Array(cells) = row else { panic!("not a row: {row:?}") };
            Span { text: cells[0].as_str().unwrap().to_owned(), start: number(&cells[1]), end: number(&cells[2]) }
        })
        .collect()
}

impl Transcriber for FakeModel {
    fn transcribe_longform(&self, pcm: &[f32], _word_timestamps: bool) -> Result<Longform, String> {
        let scenario = self.scenarios.as_object().unwrap().get(&pcm.len().to_string()).expect("no such scenario");
        let scenario = scenario.as_object().unwrap();
        if let Some(fail) = scenario.get("fail") {
            return Err(fail.py_str());
        }
        Ok(Longform { segments: spans(scenario.get("segments")), words: spans(scenario.get("words")) })
    }
}

/// The Rust backend in this process, on its own runtime thread.
pub fn start_rust(dir: &Path, with_model: bool) -> u16 {
    let config = BackendConfig::from_environ(&Environ::new()).unwrap();
    let engine: Option<Arc<dyn Transcriber>> = with_model
        .then(|| Arc::new(FakeModel { scenarios: pyjson::loads(SCENARIOS).unwrap() }) as Arc<dyn Transcriber>);
    let backend = Backend::new(config, engine).with_ffmpeg(dir.join("bin/ffmpeg")).with_temp_dir(dir.join("rs-tmp"));
    let listener = support::listener();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            backend::serve(listener, Arc::new(backend), std::future::pending()).await.unwrap();
        });
    });
    port
}

/// The fake ffmpeg and the backend's temporary directory.
pub fn prepare(dir: &Path) {
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("rs-tmp")).unwrap();
    let ffmpeg = dir.join("bin/ffmpeg");
    std::fs::write(&ffmpeg, FFMPEG).unwrap();
    std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// One request of the steps.
pub struct Step {
    pub name: String,
    pub method: &'static str,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Step {
    fn to_json(&self) -> Json {
        json!({
            "name": self.name,
            "method": self.method,
            "path": self.path,
            "headers": self.headers,
            "body_sha256": support::sha256_hex(&self.body),
        })
    }
}

struct Steps(Vec<Step>);

impl Steps {
    fn same(&mut self, name: &str, method: &'static str, path: &str, headers: &[(&str, &str)], body: &[u8]) {
        self.0.push(Step {
            name: name.to_owned(),
            method,
            path: path.to_owned(),
            headers: headers.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            body: body.to_vec(),
        });
    }

    fn transcribe(&mut self, name: &str, parts: &[Part<'_>]) {
        let (content_type, body) = multipart(parts);
        self.same(name, "POST", "/v1/audio/transcriptions", &[("Content-Type", &content_type)], &body);
    }
}

const FORM: [(&str, &str); 1] = [("Content-Type", "application/x-www-form-urlencoded")];

/// Every response format over segments with awkward times and texts, words,
/// diarization requests, decoding and model failures, request validation and routing,
/// for a backend whose model has loaded.
pub fn steps() -> Vec<Step> {
    let mut run = Steps(Vec::new());

    run.same("healthz", "GET", "/healthz", &[], b"");
    run.same("healthz with a trailing slash", "GET", "/healthz/", &[], b"");
    run.same("healthz by HEAD", "HEAD", "/healthz", &[], b"");
    run.same("models are the gateway's", "GET", "/v1/models", &[], b"");
    run.same("transcriptions by GET", "GET", "/v1/audio/transcriptions", &[], b"");

    // Every format over every scenario.
    for frames in [1600, 1601, 1602, 1603, 1604, 1605, 1606] {
        let audio = support::wav(frames);
        for format in ["json", "text", "srt", "vtt", "verbose_json", "xml", "SRT", "Verbose_JSON"] {
            run.transcribe(
                &format!("{frames} frames as {format}"),
                &[field("response_format", format), file("a.wav", Some("audio/wav"), &audio)],
            );
        }
        for granularities in ["word", "segment,word", "WORD", "segment"] {
            run.transcribe(
                &format!("{frames} frames, verbose_json, granularities {granularities}"),
                &[
                    field("response_format", "verbose_json"),
                    field("timestamp_granularities", granularities),
                    file("a.wav", None, &audio),
                ],
            );
        }
        run.transcribe(
            &format!("{frames} frames, diarized_json"),
            &[field("response_format", "diarized_json"), file("a.wav", None, &audio)],
        );
        run.transcribe(
            &format!("{frames} frames, a diarized model"),
            &[field("model", "GigaAM-Diarized"), file("a.wav", None, &audio)],
        );
    }

    let audio = support::wav(1600);
    run.transcribe("no format", &[file("a.wav", None, &audio)]);
    run.transcribe("json words", &[field("timestamp_granularities", "word"), file("a.wav", None, &audio)]);
    run.transcribe(
        "a language",
        &[field("language", "en"), field("response_format", "verbose_json"), file("a.wav", None, &audio)],
    );
    run.transcribe(
        "an empty language and format",
        &[field("language", ""), field("response_format", ""), file("a.wav", None, &audio)],
    );
    run.transcribe(
        "prompt and temperature",
        &[field("prompt", "x"), field("temperature", "0.5"), file("a.wav", None, &audio)],
    );

    // File names: the suffix of the temporary file ffmpeg gets.
    for filename in ["rec.mp3", "noext", "", "dir/x.ogg", ".hidden", "a.", "x.tar.gz", "Запись.WAV"] {
        run.transcribe(&format!("file name {filename:?}"), &[file(filename, None, &audio)]);
    }

    // Decoding and model failures.
    run.transcribe("ffmpeg fails", &[file("a.mp3", None, b"BAD audio")]);
    run.transcribe(
        "ffmpeg fails, diarization asked",
        &[field("model", "gigaam-diarized"), file("a.mp3", None, b"BAD audio")],
    );
    run.transcribe("ffmpeg is killed", &[file("a.mp3", None, b"SIG")]);
    run.transcribe("ffmpeg output is not a WAV", &[file("a.mp3", None, b"not audio at all")]);
    run.transcribe("an empty upload", &[file("a.wav", None, b"")]);

    // Request validation and the form parser.
    run.same("no file", "POST", "/v1/audio/transcriptions", &FORM, b"model=gigaam");
    run.same("no body", "POST", "/v1/audio/transcriptions", &[], b"");
    run.same("a file as text", "POST", "/v1/audio/transcriptions", &FORM, b"file=abc&temperature=x");
    run.transcribe("a bad temperature", &[field("temperature", "hot"), file("a.wav", None, &audio)]);
    run.same(
        "no boundary",
        "POST",
        "/v1/audio/transcriptions",
        &[("Content-Type", "multipart/form-data; charset=utf-8")],
        b"x",
    );
    run.same(
        "not multipart data",
        "POST",
        "/v1/audio/transcriptions",
        &[("Content-Type", "multipart/form-data; boundary=zz")],
        b"--zz\r\nbroken header\r\n\r\n",
    );
    run.transcribe(
        "a part without a name",
        &[Part { disposition: "filename=\"a.wav\"".to_owned(), content_type: None, data: audio.clone() }],
    );
    let big = "x".repeat(1024 * 1024 + 1);
    run.transcribe("a field over 1 MiB", &[field("prompt", &big), file("a.wav", None, &audio)]);

    run.0
}

/// The steps of a backend whose model has not loaded.
pub fn loading_steps() -> Vec<Step> {
    let mut run = Steps(Vec::new());
    run.same("loading: healthz", "GET", "/healthz", &[], b"");
    run.transcribe("loading: a transcription", &[file("a.wav", None, &support::wav(1600))]);
    run.same("loading: no file", "POST", "/v1/audio/transcriptions", &FORM, b"");
    run.0
}

/// The SHA-256 of both lists of steps, the scenarios and the fake ffmpeg: the
/// recorded answers hold only for these.
pub fn steps_sha256() -> String {
    let list = |steps: Vec<Step>| Json::from(steps.iter().map(Step::to_json).collect::<Vec<_>>());
    let all =
        json!({"steps": list(steps()), "loading": list(loading_steps()), "scenarios": SCENARIOS, "ffmpeg": FFMPEG});
    support::json_sha256(&all)
}

/// The Python answers, recorded by `RECOG_RECORD_ASR_SERVER_FIXTURE=1 cargo test -p
/// recog-asr-server --test backend_differential` on a checkout that still has the
/// Python side (the commit "Freeze the Python answers of recog-asr-server's
/// differential tests").
pub fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/backend_python.json")
}

/// Runs the steps against the backend on `port`: each step's answer.
pub fn ask(port: u16, steps: &[Step]) -> Vec<Json> {
    let masks = [(port, "<port>")];
    steps
        .iter()
        .map(|step| {
            let headers: Vec<(&str, &str)> = step.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let response = try_request(port, step.method, &step.path, &headers, &step.body)
                .unwrap_or_else(|e| panic!("{}: {e}", step.name));
            json!({"step": step.name, "answer": support::answer(&response, &masks)})
        })
        .collect()
}

/// The lines of the fake ffmpeg's log for the temporary directory `side`, the random
/// part of the temporary names masked.
pub fn ffmpeg_lines(dir: &Path, side: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(format!("{side}.log"))).unwrap_or_default();
    text.lines()
        .map(|line| {
            line.split(' ')
                .map(|word| match word.strip_prefix("tmp") {
                    Some(rest) if rest.len() >= 8 => format!("tmp<8>{}", &rest[8..]),
                    _ => word.to_owned(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// What is left in the temporary directory `side`.
pub fn remaining(dir: &Path, side: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir.join(side)).unwrap().flatten().map(|e| e.path()).collect()
}
