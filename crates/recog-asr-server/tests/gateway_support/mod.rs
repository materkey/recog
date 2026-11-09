//! The gateway's steps over one fake backend, which tests/gateway_frozen.rs replays
//! against the answers the Python gateway gave to them, recorded into
//! tests/fixtures/gateway_python.json by the differential test the Python side went
//! with.
//!
//! The fake answers `/healthz` in the shape the step sets and every other request by
//! its `prompt` field (other content types, statuses, a redirect, a body that is not
//! UTF-8, a reply slower than the gateway's timeout); the Qwen URL points where nothing
//! listens. Each step's answer (status, the headers that matter, body) and the requests
//! the fake received during it (boundary masked, the test's WAV named) are recorded.

#![allow(dead_code)]

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as Json, json};

use crate::support::{self, HttpResponse, Part, field, file, multipart, try_request};

/// What the fake answers at `/healthz`.
#[derive(Clone, Copy, Debug)]
pub enum Health {
    Json,
    Html,
    Error500,
    BadUtf8,
    Nan,
    Sleep,
}

/// A request the fake received.
#[derive(Clone, Debug, PartialEq)]
pub struct Received {
    method: String,
    path: String,
    content_type: String,
    body: Vec<u8>,
}

/// `haystack` with every `needle` replaced by `marker`.
fn replace_bytes(haystack: &[u8], needle: &[u8], marker: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if !needle.is_empty() && haystack[i..].starts_with(needle) {
            out.extend_from_slice(marker);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

impl Received {
    /// The request as recorded: its multipart boundary replaced by `<boundary>`, the
    /// test's WAV by `<wav>`, the body read as UTF-8 with replacement characters.
    fn record(&self, wav: &[u8]) -> Json {
        let boundary = self.content_type.split("boundary=").nth(1).unwrap_or("").to_owned();
        let body = String::from_utf8_lossy(&replace_bytes(&self.body, wav, b"<wav>")).into_owned();
        let (content_type, body) = if boundary.is_empty() {
            (self.content_type.clone(), body)
        } else {
            (self.content_type.replace(&boundary, "<boundary>"), body.replace(&boundary, "<boundary>"))
        };
        json!({"method": self.method, "path": self.path, "content_type": content_type, "body": body})
    }
}

pub struct FakeBackend {
    pub port: u16,
    health: Arc<Mutex<Health>>,
    received: Arc<Mutex<Vec<Received>>>,
}

/// The value of a text field of a multipart body, if present.
fn field_value(body: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let marker = format!("name=\"{name}\"\r\n\r\n");
    let start = text.find(&marker)? + marker.len();
    let end = text[start..].find("\r\n--")? + start;
    Some(text[start..end].to_owned())
}

fn answer(stream: &mut TcpStream, status: u16, content_type: Option<&str>, body: &[u8], extra: &str) {
    let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}", body.len());
    if let Some(t) = content_type {
        head.push_str(&format!("Content-Type: {t}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn read_request(stream: &mut TcpStream) -> Option<Received> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 65536];
    let head_end = loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let mut length = 0;
    let mut content_type = String::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = v.trim().parse().unwrap_or(0),
                "content-type" => content_type = v.trim().to_owned(),
                _ => {}
            }
        }
    }
    let mut body = raw[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Some(Received { method, path, content_type, body })
}

fn serve(mut stream: TcpStream, health: Health, received: &Mutex<Vec<Received>>) {
    let Some(request) = read_request(&mut stream) else { return };
    if request.path == "/healthz" {
        match health {
            Health::Json => answer(
                &mut stream,
                200,
                Some("application/json"),
                br#"{"ok": true, "variant": "v3_e2e_rnnt", "diarization": false, "load": 0.25}"#,
                "",
            ),
            Health::Html => answer(&mut stream, 200, Some("text/html"), b"<html>down</html>", ""),
            Health::Error500 => answer(&mut stream, 500, Some("application/json"), br#"{"detail": "boom"}"#, ""),
            Health::BadUtf8 => answer(&mut stream, 200, Some("application/json"), b"\xff{}", ""),
            Health::Nan => answer(&mut stream, 200, Some("application/json"), br#"{"x": NaN}"#, ""),
            Health::Sleep => {
                std::thread::sleep(Duration::from_secs(7));
                answer(&mut stream, 200, Some("application/json"), b"{}", "");
            }
        }
        return;
    }
    received.lock().unwrap().push(request.clone());
    match field_value(&request.body, "prompt").as_deref() {
        Some("vtt") => answer(
            &mut stream,
            200,
            Some("text/vtt; charset=utf-8"),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nпривет\n".as_bytes(),
            "",
        ),
        Some("plain") => answer(&mut stream, 200, Some("text/plain"), "привет".as_bytes(), ""),
        Some("srt") => {
            answer(&mut stream, 200, Some("application/x-subrip"), b"1\n00:00:00,000 --> 00:00:01,000\nx\n", "")
        }
        Some("none") => answer(&mut stream, 200, None, b"raw", ""),
        Some("created") => answer(&mut stream, 201, Some("application/json"), b"{}", ""),
        Some("redirect") => answer(&mut stream, 302, None, b"", "Location: /elsewhere\r\n"),
        Some("e422") => answer(&mut stream, 422, Some("application/json"), br#"{"detail":[{"type":"missing"}]}"#, ""),
        Some("e503") => {
            answer(&mut stream, 503, Some("application/json"), br#"{"detail":"diarization pipeline unavailable"}"#, "")
        }
        Some("e500") => answer(&mut stream, 500, Some("text/plain; charset=utf-8"), b"Internal Server Error", ""),
        Some("e400bin") => answer(&mut stream, 400, Some("text/plain"), b"\xff\xfeoops \xd0", ""),
        Some("sleep") => {
            std::thread::sleep(Duration::from_secs(4));
            answer(&mut stream, 200, Some("application/json"), b"{}", "");
        }
        _ => answer(&mut stream, 200, Some("application/json"), r#"{"text":"привет"}"#.as_bytes(), ""),
    }
}

impl FakeBackend {
    pub fn start() -> FakeBackend {
        let listener: TcpListener = support::listener();
        let port = listener.local_addr().unwrap().port();
        let health = Arc::new(Mutex::new(Health::Json));
        let received = Arc::new(Mutex::new(Vec::new()));
        let (h, r) = (Arc::clone(&health), Arc::clone(&received));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (h, r) = (*h.lock().unwrap(), Arc::clone(&r));
                std::thread::spawn(move || serve(stream, h, &r));
            }
        });
        FakeBackend { port, health, received }
    }

    fn set_health(&self, health: Health) {
        *self.health.lock().unwrap() = health;
    }

    fn count(&self) -> usize {
        self.received.lock().unwrap().len()
    }

    fn since(&self, from: usize) -> Vec<Received> {
        self.received.lock().unwrap()[from..].to_vec()
    }
}

/// The environment a gateway on `port` is started with, as both sides read it.
pub fn gateway_env(port: u16, fake: u16, refused: u16) -> Vec<(&'static str, String)> {
    vec![
        ("ASR_GATEWAY_HOST", "127.0.0.1".to_owned()),
        ("ASR_GATEWAY_PORT", port.to_string()),
        ("GIGAAM_URL", format!("http://127.0.0.1:{fake}")),
        ("QWEN_ASR_URL", format!("http://127.0.0.1:{refused}")),
        ("ASR_TIMEOUT_S", "2".to_owned()),
    ]
}

/// `recog-asr-gateway` on a free port, pointed at `fake` and `refused`.
pub fn start_rust(dir: &Path, fake: u16, refused: u16) -> support::Process {
    let port = support::free_port();
    support::Process::start(
        std::process::Command::new(env!("CARGO_BIN_EXE_recog-asr-gateway")),
        &gateway_env(port, fake, refused),
        port,
        dir.join("rs.log"),
        "/v1/models",
    )
}

/// One request of the steps, and what the fake answers at `/healthz` meanwhile.
pub struct Step {
    pub name: String,
    pub health: Health,
    pub method: &'static str,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Step {
    fn to_json(&self) -> Json {
        json!({
            "name": self.name,
            "health": format!("{:?}", self.health),
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
            health: Health::Json,
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

    fn health(&mut self, name: &str, health: Health) {
        self.same(name, "GET", "/healthz", &[], b"");
        self.0.last_mut().unwrap().health = health;
    }
}

/// The 1600-frame WAV the steps upload.
pub fn wav() -> Vec<u8> {
    support::wav(1600)
}

/// Every route, the model aliases, the optional fields, the backend's answers passed
/// on or turned into errors, the health aggregation, timeouts and request validation.
pub fn steps() -> Vec<Step> {
    let mut run = Steps(Vec::new());
    let wav = wav();

    // Routes.
    run.same("models", "GET", "/v1/models", &[], b"");
    run.same("models with a trailing slash", "GET", "/v1/models/?x=a%20b&y", &[], b"");
    run.same("healthz with a trailing slash", "GET", "/healthz//", &[], b"");
    run.same("healthz by POST", "POST", "/healthz", &[], b"");
    run.same("healthz by HEAD", "HEAD", "/healthz", &[], b"");
    run.same("transcriptions by GET", "GET", "/v1/audio/transcriptions", &[], b"");
    run.same("an unknown path", "GET", "/nope", &[], b"");
    run.same("an unknown path with a slash", "GET", "/nope/", &[], b"");
    run.same("a percent-encoded route", "GET", "/v1/model%73", &[], b"");

    // Health aggregation: Qwen refused, GigaAM in each shape.
    for (name, health) in [
        ("json", Health::Json),
        ("html", Health::Html),
        ("500", Health::Error500),
        ("bad utf-8", Health::BadUtf8),
        ("NaN", Health::Nan),
    ] {
        run.health(&format!("healthz, backend {name}"), health);
    }
    run.health("healthz, backend slower than 5 s", Health::Sleep);

    // Aliases and the default model.
    run.transcribe("file only", &[file("a.wav", Some("audio/wav"), &wav)]);
    for model in [
        "gigaam",
        "gigaam-v3",
        "gigaam-v3-e2e-rnnt",
        "gigaam-diarized",
        "gigaam-diarized-v3",
        "whisper-1",
        "GigaAM-V3",
        "qwen",
        "qwen3-asr",
        "QWEN3-ASR-1.7B",
        "whisper",
        "",
    ] {
        run.transcribe(&format!("model {model:?}"), &[field("model", model), file("a.wav", Some("audio/wav"), &wav)]);
    }
    run.transcribe(
        "the last of two models",
        &[field("model", "qwen"), field("model", "gigaam"), file("a.wav", None, &wav)],
    );

    // Optional fields, forwarded or left out.
    for temperature in ["0", "0.7", "1e-5", "1_0", " 2 ", "-0", "inf", "1e400", "nan", "", "abc"] {
        run.transcribe(
            &format!("temperature {temperature:?}"),
            &[field("temperature", temperature), file("a.wav", Some("audio/wav"), &wav)],
        );
    }
    run.transcribe(
        "every field",
        &[
            field("model", "gigaam"),
            field("language", "ru"),
            field("response_format", "verbose_json"),
            field("prompt", "hello"),
            field("temperature", "0.2"),
            field("timestamp_granularities", "word"),
            file("a.wav", Some("audio/wav"), &wav),
        ],
    );
    run.transcribe(
        "empty optional fields",
        &[
            field("language", ""),
            field("response_format", ""),
            field("prompt", ""),
            field("timestamp_granularities", ""),
            file("a.wav", Some("audio/wav"), &wav),
        ],
    );

    // File names and types as httpx sends them on.
    for (name, filename, content_type) in [
        ("no content type", "a.wav", None),
        ("an empty content type", "a.wav", Some("")),
        ("an mpeg file", "rec.mp3", Some("audio/mpeg")),
        ("an empty file name", "", Some("audio/wav")),
        ("a Cyrillic file name", "запись.ogg", Some("audio/ogg")),
        ("an escaped quote in the name", "a\\\"b.wav", None),
        ("a Windows path", "C:\\\\dir\\\\y.wav", None),
        ("a control character in the name", "a\u{1}b.wav", None),
        ("a percent sign in the name", "a%0Ab.wav", None),
    ] {
        run.transcribe(name, &[file(filename, content_type, &wav)]);
    }
    run.transcribe(
        "an RFC 2231 file name",
        &[Part {
            disposition: "name=\"file\"; filename*=UTF-8''%D0%B0.wav".to_owned(),
            content_type: None,
            data: wav.clone(),
        }],
    );

    // The backend's answers.
    for prompt in ["vtt", "plain", "srt", "none", "created", "redirect", "e422", "e503", "e500", "e400bin", "sleep"] {
        run.transcribe(&format!("backend answer {prompt}"), &[field("prompt", prompt), file("a.wav", None, &wav)]);
    }

    // Request validation.
    let form = [("Content-Type", "application/x-www-form-urlencoded")];
    run.same("no file", "POST", "/v1/audio/transcriptions", &form, b"model=gigaam");
    run.same("no body", "POST", "/v1/audio/transcriptions", &[], b"");
    run.same("a JSON body", "POST", "/v1/audio/transcriptions", &[("Content-Type", "application/json")], b"{}");
    run.same("a file as text", "POST", "/v1/audio/transcriptions", &form, b"file=abc&temperature=x");
    run.transcribe("a bad temperature with a file", &[field("temperature", "warm"), file("a.wav", None, &wav)]);
    run.same("no boundary", "POST", "/v1/audio/transcriptions", &[("Content-Type", "multipart/form-data")], b"--x\r\n");
    run.same(
        "not multipart data",
        "POST",
        "/v1/audio/transcriptions",
        &[("Content-Type", "multipart/form-data; boundary=abc")],
        b"garbage",
    );
    run.same(
        "an uppercase multipart type with parameters",
        "POST",
        "/v1/audio/transcriptions",
        &[("Content-Type", "Multipart/Form-Data; boundary=B0undary")],
        &multipart(&[file("a.wav", None, &wav)]).1,
    );
    run.transcribe(
        "a part without a name",
        &[Part { disposition: "filename=\"a.wav\"".to_owned(), content_type: None, data: wav.clone() }],
    );
    let mut latin = multipart(&[field("language", "x"), file("a.wav", None, &wav)]).1;
    let at = latin.iter().position(|&b| b == b'x').unwrap();
    latin[at] = 0xe9;
    run.same(
        "a field that is not UTF-8",
        "POST",
        "/v1/audio/transcriptions",
        &[("Content-Type", "multipart/form-data; boundary=B0undary")],
        &latin,
    );
    let mut cut = multipart(&[field("model", "gigaam"), file("a.wav", None, &wav)]).1;
    cut.truncate(cut.len() - 20);
    run.same(
        "a body cut inside the file",
        "POST",
        "/v1/audio/transcriptions",
        &[("Content-Type", "multipart/form-data; boundary=B0undary")],
        &cut,
    );

    run.0
}

/// The SHA-256 of the steps: the recorded answers hold only for these.
pub fn steps_sha256(steps: &[Step]) -> String {
    let steps: Vec<Json> = steps.iter().map(Step::to_json).collect();
    support::json_sha256(&Json::from(steps))
}

/// The Python answers, recorded by `RECOG_RECORD_ASR_SERVER_FIXTURE=1 cargo test -p
/// recog-asr-server --test gateway_differential` on a checkout that still has the
/// Python side (the commit "Freeze the Python answers of recog-asr-server's
/// differential tests").
pub fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gateway_python.json")
}

/// Runs the steps against the gateway on `port`, pointed at `fake` and at `refused`:
/// each step's answer and the requests the fake received during it.
pub fn ask(port: u16, fake: &FakeBackend, refused: u16, steps: &[Step]) -> Vec<Json> {
    let masks = [(port, "<port>"), (fake.port, "<backend>"), (refused, "<refused>")];
    let wav = wav();
    steps
        .iter()
        .map(|step| {
            fake.set_health(step.health);
            let before = fake.count();
            let headers: Vec<(&str, &str)> = step.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let response: HttpResponse = try_request(port, step.method, &step.path, &headers, &step.body)
                .unwrap_or_else(|e| panic!("{}: {e}", step.name));
            let received: Vec<Json> = fake.since(before).iter().map(|r| r.record(&wav)).collect();
            fake.set_health(Health::Json);
            json!({"step": step.name, "answer": support::answer(&response, &masks), "received": received})
        })
        .collect()
}
