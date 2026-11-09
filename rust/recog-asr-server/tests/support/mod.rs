//! What the frozen tests share: free ports, a raw HTTP/1.1 client, multipart and WAV
//! builders, a server process, an answer as it is compared and recorded, and the
//! comparison of two runs. Nothing here uses the ports of installed services.

#![allow(dead_code)]

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};
use sha2::{Digest as _, Sha256};

/// Ports of the user's services and of a production ASR stack, never used by a test.
const RESERVED: [u16; 8] = [8765, 8766, 8767, 9080, 8770, 9001, 9002, 9003];

/// An ephemeral port nothing listens on any more.
pub fn free_port() -> u16 {
    loop {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if !RESERVED.contains(&port) {
            return port;
        }
    }
}

/// A bound listener on an ephemeral port.
pub fn listener() -> std::net::TcpListener {
    loop {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        if !RESERVED.contains(&listener.local_addr().unwrap().port()) {
            return listener;
        }
    }
}

/// A response as the wire carried it.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(end) = raw.windows(2).position(|w| w == b"\r\n") else { return out };
        let size = usize::from_str_radix(String::from_utf8_lossy(&raw[..end]).split(';').next().unwrap().trim(), 16)
            .unwrap_or(0);
        raw = &raw[end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&raw[..size.min(raw.len())]);
        raw = &raw[(size + 2).min(raw.len())..];
    }
}

/// One request over a fresh connection, read to the end. `headers` are sent as
/// given, `Content-Length` added for a body or a POST.
pub fn try_request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<HttpResponse> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() || method == "POST" {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| std::io::Error::other("no head"))?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default().split(' ').nth(1).unwrap_or("0").parse().unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers.iter().any(|(k, v)| k == "transfer-encoding" && v.contains("chunked")) {
        body = dechunk(&body);
    }
    Ok(HttpResponse { status, headers, body })
}

/// Waits until `GET path` answers 200.
pub fn wait_ready(port: u16, path: &str, log: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(response) = try_request(port, "GET", path, &[], b"")
            && response.status == 200
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("server on port {port} did not start:\n{}", std::fs::read_to_string(log).unwrap_or_default());
}

/// A server process, killed when dropped.
pub struct Process {
    pub child: Child,
    pub port: u16,
    pub log: PathBuf,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Process {
    /// Starts `command` (its environment cleared but for `env`, PATH and HOME) with
    /// its output in `log`, and waits for `ready_path`.
    pub fn start(mut command: Command, env: &[(&str, String)], port: u16, log: PathBuf, ready_path: &str) -> Self {
        let file = std::fs::File::create(&log).unwrap();
        command.env_clear().env("PATH", "/usr/bin:/bin").env("LANG", "C.UTF-8");
        if let Some(home) = std::env::var_os("HOME") {
            command.env("HOME", home);
        }
        for (k, v) in env {
            command.env(k, v);
        }
        command.stdin(Stdio::null()).stdout(file.try_clone().unwrap()).stderr(file);
        let child = command.spawn().unwrap();
        let process = Process { child, port, log };
        wait_ready(port, ready_path, &process.log);
        process
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// One part of a multipart body.
pub struct Part<'a> {
    /// The raw `Content-Disposition` parameters after `form-data; `.
    pub disposition: String,
    pub content_type: Option<&'a str>,
    pub data: Vec<u8>,
}

/// A text field.
pub fn field(name: &str, value: &str) -> Part<'static> {
    Part { disposition: format!("name=\"{name}\""), content_type: None, data: value.as_bytes().to_vec() }
}

/// A file part named `file`.
pub fn file<'a>(filename: &str, content_type: Option<&'a str>, data: &[u8]) -> Part<'a> {
    Part { disposition: format!("name=\"file\"; filename=\"{filename}\""), content_type, data: data.to_vec() }
}

/// A multipart body with boundary `B0undary`, and its content type.
pub fn multipart(parts: &[Part<'_>]) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(b"--B0undary\r\n");
        body.extend_from_slice(format!("Content-Disposition: form-data; {}\r\n", part.disposition).as_bytes());
        if let Some(t) = part.content_type {
            body.extend_from_slice(format!("Content-Type: {t}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--B0undary--\r\n");
    ("multipart/form-data; boundary=B0undary".to_owned(), body)
}

/// A 16 kHz mono 16-bit WAV of `frames` frames of a quiet sine.
pub fn wav(frames: usize) -> Vec<u8> {
    let data_len = u32::try_from(frames * 2).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let sample = ((i as f64 * 0.05).sin() * 1000.0) as i16;
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// The headers whose values are compared.
pub const COMPARED_HEADERS: [&str; 4] = ["content-type", "content-length", "allow", "location"];

/// `text` with each `127.0.0.1:<port>` of `masks` replaced by `127.0.0.1:<name>`.
pub fn mask(text: &str, masks: &[(u16, &str)]) -> String {
    masks.iter().fold(text.to_owned(), |text, (port, name)| {
        text.replace(&format!("127.0.0.1:{port}"), &format!("127.0.0.1:{name}"))
    })
}

/// The lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A response as the tests compare and record it: the status, the compared headers and
/// the body, the ports of `masks` (the server's own, the backends it is pointed at)
/// replaced by their names, so that runs on different ports compare. A
/// `Content-Length` equal to the body's length is `<body>`, since a masked port can
/// change the length; a body that is not UTF-8 is kept as hex.
pub fn answer(response: &HttpResponse, masks: &[(u16, &str)]) -> Json {
    let headers: serde_json::Map<String, Json> = COMPARED_HEADERS
        .iter()
        .map(|name| {
            let value = response.header(name).map(|v| {
                if *name == "content-length" && v == response.body.len().to_string() {
                    "<body>".to_owned()
                } else {
                    mask(v, masks)
                }
            });
            ((*name).to_owned(), json!(value))
        })
        .collect();
    let body = match std::str::from_utf8(&response.body) {
        Ok(text) => json!(mask(text, masks)),
        Err(_) => json!({"hex": hex(&response.body)}),
    };
    json!({"status": response.status, "headers": headers, "body": body})
}

/// Reads a fixture of recorded Python answers, checking that it was recorded for the
/// steps whose SHA-256 is `steps_sha256`.
pub fn read_fixture(path: &Path, steps_sha256: &str, how_to_record: &str) -> Json {
    let fixture: Json = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        fixture["steps_sha256"].as_str().unwrap(),
        steps_sha256,
        "the steps changed since the answers were recorded: record them again on a checkout with the Python side \
         (the commit \"Freeze the Python answers of recog-asr-server's differential tests\", {how_to_record})"
    );
    fixture
}

/// Collects the differences between two runs of the same steps: the answers the Rust
/// ones are held to (Python's, as recorded) and the Rust ones.
pub struct Diff {
    expected: &'static str,
    failures: Vec<String>,
    steps: usize,
}

impl Diff {
    /// `expected` names the side the Rust answers are held to.
    pub fn new(expected: &'static str) -> Self {
        Self { expected, failures: Vec::new(), steps: 0 }
    }

    /// Compares the answers step by step, each named by its `step` field.
    pub fn compare(&mut self, expected: &[Json], got: &[Json]) {
        if expected.len() != got.len() {
            self.failures.push(format!("{}: {} steps, Rust: {}", self.expected, expected.len(), got.len()));
        }
        for (want, have) in expected.iter().zip(got) {
            self.steps += 1;
            if want != have {
                let step = have["step"].as_str().unwrap_or("?");
                self.failures.push(format!("{step}: differs\n  {}: {want}\n  Rust: {have}", self.expected));
            }
        }
    }

    pub fn fail(&mut self, message: String) {
        self.failures.push(message);
    }

    pub fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{} of {} steps differ:\n\n{}",
            self.failures.len(),
            self.steps,
            self.failures.join("\n\n")
        );
        eprintln!("{} steps compared", self.steps);
    }
}
