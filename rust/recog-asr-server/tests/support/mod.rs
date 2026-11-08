//! What the differential tests share: the interpreter of the Python side, free ports,
//! a raw HTTP/1.1 client, multipart and WAV builders, a Python server process and the
//! comparison of two answers. Nothing here uses the ports of installed services.

#![allow(dead_code)]

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Ports of the user's services and of a production ASR stack, never used by a test.
const RESERVED: [u16; 8] = [8765, 8766, 8767, 9080, 8770, 9001, 9002, 9003];

pub fn repo_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

/// The interpreter of the Python side: `RECOG_ASR_SERVER_PARITY_PYTHON`
/// (scripts/check.sh sets it), else the checkout's venv; without either the
/// differential tests are skipped with a note.
pub fn python() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RECOG_ASR_SERVER_PARITY_PYTHON") {
        return Some(PathBuf::from(path));
    }
    let venv = repo_root().join("venv-gigaam/bin/python");
    if venv.exists() {
        return Some(venv);
    }
    eprintln!("differential test skipped: no venv-gigaam/bin/python and RECOG_ASR_SERVER_PARITY_PYTHON is not set");
    None
}

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

/// Collects the differences between the Python and the Rust answers.
pub struct Diff {
    failures: Vec<String>,
    steps: usize,
}

impl Diff {
    pub fn new() -> Self {
        Self { failures: Vec::new(), steps: 0 }
    }

    /// Compares the status, the compared headers (each side's own port replaced by
    /// `<port>`) and the body bytes (the same replacement).
    pub fn check(&mut self, step: &str, py: &HttpResponse, py_port: u16, rs: &HttpResponse, rs_port: u16) {
        self.steps += 1;
        let local = |text: &str, port: u16| text.replace(&format!("127.0.0.1:{port}"), "127.0.0.1:<port>");
        if py.status != rs.status {
            self.failures.push(format!(
                "{step}: status {} != {}\n  py: {}\n  rs: {}",
                py.status,
                rs.status,
                py.text(),
                rs.text()
            ));
            return;
        }
        for name in COMPARED_HEADERS {
            let a = py.header(name).map(|v| local(v, py_port));
            let b = rs.header(name).map(|v| local(v, rs_port));
            if a != b {
                self.failures.push(format!("{step}: header {name}: {a:?} != {b:?}"));
            }
        }
        let a = local(&py.text(), py_port);
        let b = local(&rs.text(), rs_port);
        if a != b || (py.body != rs.body && String::from_utf8(py.body.clone()).is_err()) {
            self.failures.push(format!("{step}: body differs\n  py: {a:?}\n  rs: {b:?}"));
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
