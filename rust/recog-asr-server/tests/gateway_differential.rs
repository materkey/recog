//! `recog-asr-gateway` against `python -m recog.asr_server.gateway`, both configured
//! by the same environment and pointed at one fake backend (and a Qwen URL where
//! nothing listens): every route, the model aliases, the optional fields, the
//! backend's answers passed on or turned into errors, the health aggregation, timeouts
//! and request validation. Each answer is compared by status, the headers that matter
//! and body; each request the fake received is compared too (boundary masked).

mod support;

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use support::{Diff, HttpResponse, Part, Process, field, file, multipart, try_request};

/// What the fake answers at `/healthz`.
#[derive(Clone, Copy)]
enum Health {
    Json,
    Html,
    Error500,
    BadUtf8,
    Nan,
    Sleep,
}

/// A request the fake received.
#[derive(Clone, Debug, PartialEq)]
struct Received {
    method: String,
    path: String,
    content_type: String,
    body: Vec<u8>,
}

impl Received {
    /// The request with its multipart boundary replaced by `<boundary>`.
    fn masked(&self) -> Received {
        let boundary = self.content_type.split("boundary=").nth(1).unwrap_or("").to_owned();
        let mut out = self.clone();
        if !boundary.is_empty() {
            out.content_type = self.content_type.replace(&boundary, "<boundary>");
            out.body = String::from_utf8_lossy(&self.body).replace(&boundary, "<boundary>").into_bytes();
        }
        out
    }
}

struct FakeBackend {
    port: u16,
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
    fn start() -> FakeBackend {
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

struct Pair {
    py: Process,
    rs: Process,
}

impl Pair {
    fn start(python: &std::path::Path, dir: &std::path::Path, fake: u16, refused: u16) -> Pair {
        let env = |port: u16| {
            vec![
                ("ASR_GATEWAY_HOST", "127.0.0.1".to_owned()),
                ("ASR_GATEWAY_PORT", port.to_string()),
                ("GIGAAM_URL", format!("http://127.0.0.1:{fake}")),
                ("QWEN_ASR_URL", format!("http://127.0.0.1:{refused}")),
                ("ASR_TIMEOUT_S", "2".to_owned()),
            ]
        };
        let py_port = support::free_port();
        let mut py = Command::new(python);
        py.arg("-I")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/run_gateway.py"))
            .arg(support::repo_root());
        let py = Process::start(py, &env(py_port), py_port, dir.join("py.log"), "/v1/models");
        let rs_port = support::free_port();
        let rs = Process::start(
            Command::new(env!("CARGO_BIN_EXE_recog-asr-gateway")),
            &env(rs_port),
            rs_port,
            dir.join("rs.log"),
            "/v1/models",
        );
        Pair { py, rs }
    }
}

struct Run<'a> {
    pair: &'a Pair,
    fake: &'a FakeBackend,
    diff: Diff,
}

impl Run<'_> {
    fn request(&self, port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> HttpResponse {
        try_request(port, method, path, headers, body).unwrap_or_else(|e| panic!("{method} {path}: {e}"))
    }

    /// The same request to both; the answers and what the fake received compared.
    fn same(&mut self, step: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> HttpResponse {
        let before = self.fake.count();
        let py = self.request(self.pair.py.port, method, path, headers, body);
        let middle = self.fake.count();
        let rs = self.request(self.pair.rs.port, method, path, headers, body);
        let py_got: Vec<Received> = self.fake.since(before)[..middle - before].iter().map(Received::masked).collect();
        let rs_got: Vec<Received> = self.fake.since(middle).iter().map(Received::masked).collect();
        if py_got != rs_got {
            self.diff.fail(format!(
                "{step}: the backend received\n  py: {:?}\n  rs: {:?}",
                py_got
                    .iter()
                    .map(|r| (&r.method, &r.path, &r.content_type, String::from_utf8_lossy(&r.body)))
                    .collect::<Vec<_>>(),
                rs_got
                    .iter()
                    .map(|r| (&r.method, &r.path, &r.content_type, String::from_utf8_lossy(&r.body)))
                    .collect::<Vec<_>>(),
            ));
        }
        self.diff.check(step, &py, self.pair.py.port, &rs, self.pair.rs.port);
        rs
    }

    /// A multipart transcription request.
    fn transcribe(&mut self, step: &str, parts: &[Part<'_>]) -> HttpResponse {
        let (content_type, body) = multipart(parts);
        self.same(step, "POST", "/v1/audio/transcriptions", &[("Content-Type", &content_type)], &body)
    }

    /// The same request to both at once (for the steps that wait out a timeout).
    fn concurrently(&mut self, step: &str, method: &str, path: &str) {
        let (py_port, rs_port) = (self.pair.py.port, self.pair.rs.port);
        let (m, p) = (method.to_owned(), path.to_owned());
        let py = std::thread::spawn(move || try_request(py_port, &m, &p, &[], b"").unwrap());
        let rs = try_request(rs_port, method, path, &[], b"").unwrap();
        let py = py.join().unwrap();
        self.diff.check(step, &py, py_port, &rs, rs_port);
    }
}

#[test]
fn python_and_rust_gateways_given_one_fake_backend_when_asked_the_same_then_the_same_answers_and_requests() {
    let Some(python) = support::python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeBackend::start();
    let refused = support::free_port();
    let pair = Pair::start(&python, dir.path(), fake.port, refused);
    let mut run = Run { pair: &pair, fake: &fake, diff: Diff::new() };
    let wav = support::wav(1600);

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
        fake.set_health(health);
        run.same(&format!("healthz, backend {name}"), "GET", "/healthz", &[], b"");
    }
    fake.set_health(Health::Sleep);
    run.concurrently("healthz, backend slower than 5 s", "GET", "/healthz");
    fake.set_health(Health::Json);

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

    run.diff.finish();
}

#[test]
fn a_port_python_refuses_when_the_gateway_starts_then_it_exits_with_the_message() {
    let output =
        Command::new(env!("CARGO_BIN_EXE_recog-asr-gateway")).env("ASR_GATEWAY_PORT", "eighty").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid literal for int() with base 10: 'eighty'"));
}
