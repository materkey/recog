//! What the differential tests share: a synthetic data directory, the two servers on
//! it, a raw HTTP/1.1 client, a fake bridge and the masking of values that differ by
//! nature (fresh UUIDs, clocks). Nothing here touches `~/.recog`, `~/.config/recog` or
//! the ports of the installed services, and no real CLI is ever run.

#![allow(dead_code)]

pub mod ws;

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use recog_core::pyjson::{self, Map, Value};

/// Ports of the user's live services, never used by a test.
const RESERVED: [u16; 4] = [9080, 8765, 8766, 8767];

pub fn repo_root() -> PathBuf {
    std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

pub fn free_port() -> u16 {
    loop {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if !RESERVED.contains(&port) {
            return port;
        }
    }
}

pub fn obj(text: &str) -> Value {
    pyjson::loads(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn jsonl(rows: &[Value]) -> String {
    rows.iter().map(|r| pyjson::dumps(r, None) + "\n").collect()
}

/// A 16 kHz mono int16 WAV of `seconds` of a tone.
fn tone_wav(path: &Path, seconds: u32, hz: f32) {
    let samples = 16_000 * seconds;
    let mut data = Vec::with_capacity(samples as usize * 2);
    for i in 0..samples {
        let x = (i as f32 * hz * std::f32::consts::TAU / 16_000.0).sin() * 0.3;
        data.extend_from_slice(&((x * 32767.0) as i16).to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out).unwrap();
}

pub const ENDED: &str = "2026-09-01_10-00-00_a1b2c3d4";
pub const CALL: &str = "2026-09-02_09-00-00_c0ffee00";
pub const ACTIVE: &str = "2026-11-08_09-00-00_feedbeef";
pub const DOOMED: &str = "2026-08-01_08-00-00_deadbeef";
pub const EMPTY: &str = "2026-08-02_08-00-00_00000000";
pub const EXTERNAL: &str = "2026-07-01_12-00-00_e0e0e0e0";

/// Builds the synthetic data of one server under `root`.
pub fn build_fixture(root: &Path) {
    let meetings = root.join("data/meetings");
    let ended = meetings.join(ENDED);
    write(
        &ended.join("meta.json"),
        &pyjson::dumps(
            &obj(&format!(
                r#"{{"id": "{ENDED}", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "2026-09-01T11:00:00+00:00",
                "updated_at": "2026-09-01T11:00:00+00:00", "title": "Обзор дизайна", "source": "ktalk",
                "end_reason": "ktalk_leave", "conference": {{"live_url": "https://meet.example.org/design-room"}},
                "participants": {{"alice": {{"name": "Алиса", "is_local": true}}, "bob": {{"name": "Bob"}}}},
                "stats": {{"segment_count": 5, "duration_seconds": 3600}}}}"#
            )),
            Some(2),
        ),
    );
    write(
        &ended.join("transcript.jsonl"),
        &jsonl(&[
            obj(
                r#"{"id": 1, "ts": "2026-09-01T10:00:05+00:00", "ts_display": "10:00:05", "speaker": "Алиса", "speaker_id": "alice", "text": "Привет, начнём обзор", "is_local": true, "audio_offset_ms": 0}"#,
            ),
            obj(
                r#"{"id": 2, "ts": "2026-09-01T10:00:09+00:00", "ts_display": "10:00:09", "speaker": "Bob", "speaker_id": "bob", "text": "Hello, I prefer the blue button", "is_local": false, "audio_offset_ms": 1000}"#,
            ),
            obj(
                r#"{"id": 3, "ts": "2026-09-01T10:00:12+00:00", "ts_display": "10:00:12", "speaker": "Bob", "speaker_id": "bob", "text": "and the round corners", "is_local": false, "audio_offset_ms": 4000, "cont": true}"#,
            ),
            obj(
                r#"{"id": 4, "ts": "2026-09-01T10:00:15+00:00", "ts_display": "10:00:15", "speaker": "system", "speaker_id": "__system__", "text": "", "event_type": "pcm_degraded", "audio_offset_ms": 0}"#,
            ),
            obj(
                r#"{"id": 5, "ts": "2026-09-01T10:00:20+00:00", "ts_display": "10:00:20", "speaker": "Алиса", "speaker_id": "alice", "text": "Хорошо, ПРИВЕТ синей кнопке", "is_local": true, "audio_offset_ms": 2500}"#,
            ),
            obj(
                r#"{"id": 6, "ts": "2026-09-01T10:00:25+00:00", "ts_display": "10:00:25", "speaker": "Bob", "speaker_id": "bob", "text": "no offset here", "is_local": false}"#,
            ),
        ]),
    );
    tone_wav(&ended.join("audio_mic.wav"), 6, 440.0);
    tone_wav(&ended.join("audio_system.wav"), 8, 660.0);
    write(
        &ended.join("increments/my-increment/v1.md"),
        "---\nmeeting: \"Обзор дизайна\"\nversion: 1\n---\n\nИнкремент: синяя кнопка.\n",
    );
    write(
        &ended.join("increments/my-increment/v1_meta.json"),
        &pyjson::dumps(
            &obj(
                r#"{"version": 1, "prompt_id": "my-increment", "status": "done", "segment_range": [1, 3], "generated_at": "2026-09-01T11:05:00+00:00", "provider": "codex"}"#,
            ),
            Some(2),
        ),
    );
    write(
        &ended.join("chat.jsonl"),
        &jsonl(&[obj(
            r#"{"asked_at": "2026-09-01T12:00:00+00:00", "question": "Что решили?", "answer": "Синюю кнопку.", "provider": "Codex (x)", "settings_revision": "defaults", "transcript_truncated": false}"#,
        )]),
    );

    let call = meetings.join(CALL);
    write(
        &call.join("meta.json"),
        &pyjson::dumps(
            &obj(&format!(
                r#"{{"id": "{CALL}", "created_at": "2026-09-02T09:00:00+00:00", "ended_at": "2026-09-02T09:05:00+00:00",
                "title": "Call", "source": "callrec", "callrec": {{"package": "com.example", "recorder": "samsung"}},
                "participants": {{"me": {{"name": "Me", "is_local": true}}}}, "stats": {{"segment_count": 3, "duration_seconds": 300}}}}"#
            )),
            Some(2),
        ),
    );
    write(
        &call.join("transcript.jsonl"),
        &jsonl(&[
            obj(r#"{"id": 1, "text": "line one", "speaker": "Me", "audio_offset_ms": 0, "is_local": true}"#),
            obj(r#"{"id": 2, "text": "reply", "speaker": "Them", "audio_offset_ms": 2000, "is_local": false}"#),
            obj(r#"{"id": 3, "text": "line three", "speaker": "Me", "audio_offset_ms": 4000, "is_local": true}"#),
        ]),
    );
    let mix = call.join("audio_mix.m4a");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=500:duration=6", "-ac", "1", "-ar", "16000"])
        .arg(&mix)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success());

    let active = meetings.join(ACTIVE);
    write(
        &active.join("meta.json"),
        &pyjson::dumps(
            &obj(&format!(
                r#"{{"id": "{ACTIVE}", "created_at": "2026-11-08T09:00:00+00:00", "ended_at": null, "title": null,
                "source": "ktalk", "recording_active": true, "conference": {{"live_url": "https://meet.example.org/live"}},
                "participants": {{"alice": {{"name": "Алиса", "is_local": true}}}}, "stats": {{"segment_count": 2}}}}"#
            )),
            Some(2),
        ),
    );
    write(
        &active.join("transcript.jsonl"),
        &jsonl(&[
            obj(
                r#"{"id": 1, "ts_display": "09:00:01", "speaker": "Алиса", "speaker_id": "alice", "text": "Идёт встреча", "is_local": true}"#,
            ),
            obj(
                r#"{"id": 2, "ts_display": "09:00:04", "speaker": "Bob", "speaker_id": "bob", "text": "live text", "is_local": false}"#,
            ),
        ]),
    );

    let doomed = meetings.join(DOOMED);
    write(
        &doomed.join("meta.json"),
        &pyjson::dumps(
            &obj(&format!(
                r#"{{"id": "{DOOMED}", "created_at": "2026-08-01T08:00:00+00:00", "ended_at": "2026-08-01T08:30:00+00:00", "title": "To delete", "source": "old-meet", "stats": {{"segment_count": 1}}}}"#
            )),
            Some(2),
        ),
    );
    write(&doomed.join("transcript.jsonl"), &jsonl(&[obj(r#"{"id": 1, "text": "bye", "speaker": "X"}"#)]));
    let empty = meetings.join(EMPTY);
    write(
        &empty.join("meta.json"),
        &pyjson::dumps(
            &obj(&format!(
                r#"{{"id": "{EMPTY}", "created_at": "2026-08-02T08:00:00+00:00", "ended_at": "2026-08-02T08:01:00+00:00"}}"#
            )),
            Some(2),
        ),
    );
    write(&empty.join("transcript.jsonl"), "");
    let index = obj(&format!(
        r#"{{"meetings": [
        {{"id": "{ENDED}", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "2026-09-01T11:00:00+00:00", "title": "Обзор дизайна", "source": "ktalk", "segment_count": 5, "participants": ["Алиса", "Bob"]}},
        {{"id": "{CALL}", "created_at": "2026-09-02T09:00:00+00:00", "ended_at": "2026-09-02T09:05:00+00:00", "title": "Call", "source": "callrec", "segment_count": 3}},
        {{"id": "{DOOMED}", "created_at": "2026-08-01T08:00:00+00:00", "ended_at": "2026-08-01T08:30:00+00:00", "title": "To delete", "source": "old-meet", "segment_count": 1}},
        {{"id": "{EMPTY}", "created_at": "2026-08-02T08:00:00+00:00", "ended_at": "2026-08-02T08:01:00+00:00", "title": null, "segment_count": 0}},
        {{"id": "archived-only", "created_at": "2026-01-01T00:00:00+00:00", "title": "Index only archive"}}
    ]}}"#
    ));
    write(&meetings.join("index.json"), &pyjson::dumps(&index, Some(2)));

    let external = root.join("external").join(EXTERNAL);
    write(
        &external.join("meta.json"),
        &pyjson::dumps(
            &obj(&format!(
                r#"{{"id": "{EXTERNAL}", "created_at": "2026-07-01T12:00:00+00:00", "ended_at": "2026-07-01T12:30:00+00:00", "title": "Imported call", "conference": {{"live_url": "https://example.test/imported"}}, "stats": {{"segment_count": 1}}}}"#
            )),
            Some(2),
        ),
    );
    write(
        &external.join("transcript.jsonl"),
        &jsonl(&[obj(r#"{"id": 1, "text": "imported hello", "speaker": "Ext"}"#)]),
    );
    write(
        &root.join("config.toml"),
        "[conferences]\njitsi = [\"https://meet.example.org\"]\n\n[sources.aliases]\nold-meet = \"jitsi\"\n\n[sources.labels]\nimporter = \"Phone Importer\"\n",
    );
    write(
        &root.join("ktalk_debug.json"),
        r#"{"timestamp": 1000.0, "meeting": {"id": "m-debug"}, "conference": {"participants": [{"id": "p1"}]}, "pcm": {"transport_enabled": true, "chunks": 3}, "pcm_transport": null}"#,
    );
    std::fs::create_dir_all(root.join("home")).unwrap();
}

/// Writes the stub CLIs under `bin` with the parity interpreter in their shebang, and
/// links ffmpeg beside them so that nothing else of the user's PATH is needed.
pub fn install_stubs(bin: &Path, python: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let body = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/stub_cli.py")).unwrap();
    for name in ["claude", "codex"] {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!{} -I\n{body}", python.display())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ffmpeg = which("ffmpeg").expect("ffmpeg on PATH");
    let _ = std::os::unix::fs::symlink(ffmpeg, bin.join("ffmpeg"));
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Which implementation a server is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Python,
    Rust,
}

/// One server on its own copy of the data.
pub struct Server {
    pub side: Side,
    pub port: u16,
    pub root: PathBuf,
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    pub fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn meetings(&self) -> PathBuf {
        self.root.join("data/meetings")
    }

    pub fn calls(&self) -> PathBuf {
        self.root.join("calls")
    }

    /// Starts one side on `root` with the bridge at `bridge_port`.
    pub fn start(side: Side, root: &Path, python: &Path, bridge_port: u16) -> Server {
        let port = free_port();
        let bin = root.join("bin");
        install_stubs(&bin, python);
        let root = std::fs::canonicalize(root).unwrap();
        let mut command = match side {
            Side::Python => {
                let mut c = Command::new(python);
                c.arg("-I")
                    .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/run_server.py"))
                    .arg(repo_root())
                    .arg(port.to_string())
                    .arg(root.join("ktalk_debug.json"));
                c
            }
            Side::Rust => {
                let mut c = Command::new(env!("CARGO_BIN_EXE_recog-web"));
                c.args(["--port", &port.to_string(), "--repo"]).arg(repo_root());
                c
            }
        };
        let log = std::fs::File::create(root.join(format!("{side:?}.log"))).unwrap();
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", root.join("home"))
            .env("TMPDIR", std::env::temp_dir())
            .env("LANG", "en_US.UTF-8")
            .env("TZ", "UTC")
            .env("RECOG_DATA_DIR", root.join("data"))
            .env("RECOG_CONFIG_FILE", root.join("config.toml"))
            .env("RECOG_BRIDGE_WS_URL", format!("ws://127.0.0.1:{bridge_port}"))
            .env("EXTERNAL_MEETINGS_PATHS", format!("{}:importer", root.join("external").display()))
            .env("RECOG_EXTRA_WS_ORIGINS", "https://extra.example.org")
            .env("MIC_SPEAKER_NAME", "Легаси")
            .env("CLAUDE_MODEL", "")
            .env("CODEX_MODEL", "")
            .env("CODEX_REASONING_EFFORT", "")
            .env("RECOG_STUB_CALLS", root.join("calls"))
            .env("RECOG_WEB_KTALK_DEBUG_FILE", root.join("ktalk_debug.json"))
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let child = command.spawn().unwrap();
        let server = Server { side, port, root, child };
        server.wait_ready();
        server
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(response) = try_request(self.port, "GET", "/api/desktop/status", &[], b"")
                && response.status == 200
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let log = std::fs::read_to_string(self.root.join(format!("{:?}.log", self.side))).unwrap_or_default();
        panic!("{:?} server did not start:\n{log}", self.side);
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.root.join(format!("{:?}.log", self.side))).unwrap_or_default()
    }

    /// A request; the server's own directory and port in the answer are replaced by
    /// `<root>` and `<port>`, so that the two servers' answers compare.
    pub fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> HttpResponse {
        let response = try_request(self.port, method, path, headers, body)
            .unwrap_or_else(|e| panic!("{:?} {method} {path}: {e}", self.side));
        self.localize(response)
    }

    fn localize(&self, mut response: HttpResponse) -> HttpResponse {
        let root = self.root.to_string_lossy().into_owned();
        let port = format!("127.0.0.1:{}", self.port);
        let replace = |text: &str| text.replace(&root, "<root>").replace(&port, "127.0.0.1:<port>");
        for (_, value) in &mut response.headers {
            *value = replace(value);
        }
        if let Ok(text) = String::from_utf8(response.body.clone()) {
            response.body = replace(&text).into_bytes();
        }
        response
    }

    /// The stub CLI calls in the order they were made.
    pub fn stub_calls(&self) -> Vec<Value> {
        let Ok(entries) = std::fs::read_dir(self.calls()) else { return Vec::new() };
        let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        names.sort_by_key(|p| {
            p.file_name().unwrap().to_string_lossy().split('-').nth(1).unwrap_or("0").parse::<u128>().unwrap_or(0)
        });
        names.iter().map(|p| obj(&std::fs::read_to_string(p).unwrap())).collect()
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

    pub fn json(&self) -> Value {
        pyjson::loads(&String::from_utf8_lossy(&self.body))
            .unwrap_or_else(|_| Value::from(String::from_utf8_lossy(&self.body).into_owned()))
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

/// One request over a fresh connection, read to the end.
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
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
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

/// Sends a JSON body.
pub fn json_headers() -> Vec<(&'static str, &'static str)> {
    vec![("Content-Type", "application/json")]
}

fn is_uuid_like(s: &str) -> bool {
    let hex = |part: &str, n: usize| part.len() == n && part.bytes().all(|b| b.is_ascii_hexdigit());
    let parts: Vec<&str> = s.split('-').collect();
    (parts.len() == 5
        && hex(parts[0], 8)
        && hex(parts[1], 4)
        && hex(parts[2], 4)
        && hex(parts[3], 4)
        && hex(parts[4], 12))
        || hex(s, 32)
}

/// Keys whose values come from a clock.
const CLOCK_KEYS: [&str; 9] = [
    "set_at",
    "asked_at",
    "updated_at",
    "timestamp",
    "timestamp_iso",
    "age_seconds",
    "state_age_s",
    "generated_at",
    "started_at",
];

/// The value with what differs by nature masked: fresh UUIDs, clock readings, the
/// live duration of an active meeting, the server's own address.
pub fn mask(value: &Value) -> Value {
    match value {
        Value::Str(s) if is_uuid_like(s) => Value::from("<uuid>"),
        Value::Array(items) => Value::Array(items.iter().map(mask).collect()),
        Value::Object(map) => {
            let active = map.get("is_active").is_some_and(Value::truthy);
            let mut out = Map::new();
            for (k, v) in map.iter() {
                let masked = if CLOCK_KEYS.contains(&k.as_str()) && !v.is_null() {
                    Value::from("<clock>")
                } else if (k == "duration_seconds" && active) || k == "server" {
                    Value::from("<varies>")
                } else {
                    mask(v)
                };
                out.insert(k.clone(), masked);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The headers the comparison looks at.
pub const COMPARED_HEADERS: [&str; 10] = [
    "content-type",
    "content-length",
    "cache-control",
    "etag",
    "last-modified",
    "accept-ranges",
    "content-range",
    "content-disposition",
    "allow",
    "location",
];

/// Files of a data directory with their (masked, for JSON) content.
pub fn snapshot_files(dir: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if name.ends_with(".lock") {
                files.insert(name, String::new());
                continue;
            }
            let raw = std::fs::read(&path).unwrap_or_default();
            let text = String::from_utf8_lossy(&raw).into_owned();
            let content = if name.ends_with(".json") {
                match pyjson::loads(&text) {
                    Ok(v) => format!(
                        "{}{}",
                        pyjson::dumps(&mask(&v), Some(2)),
                        if text.ends_with('\n') { "\\n" } else { "" }
                    ),
                    Err(_) => text,
                }
            } else if name.ends_with(".jsonl") {
                text.lines()
                    .map(|l| pyjson::loads(l).map(|v| pyjson::dumps(&mask(&v), None)).unwrap_or_else(|_| l.to_owned()))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if name.ends_with(".md") {
                mask_lines(&text)
            } else {
                format!("{} bytes, md5 {:x}", raw.len(), md5_of(&raw))
            };
            files.insert(name, content);
        }
    }
    files
}

fn mask_lines(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.starts_with("generated_at:") || l.starts_with("date:") {
                "<clock line>".to_owned()
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn md5_of(raw: &[u8]) -> u128 {
    // A cheap stable digest for binary files; equality is all that matters.
    let mut h: u128 = 0xcbf2_9ce4_8422_2325;
    for b in raw {
        h ^= u128::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// A fake bridge: records every frame a subscriber sends and lets the test push
/// frames to every connected subscriber.
#[derive(Clone)]
pub struct FakeBridge {
    pub port: u16,
    pub received: Arc<Mutex<Vec<String>>>,
    outbox: Arc<tokio::sync::broadcast::Sender<String>>,
    connections: Arc<Mutex<usize>>,
}

impl FakeBridge {
    pub async fn start() -> FakeBridge {
        use futures_util::{SinkExt as _, StreamExt as _};
        let port = free_port();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let (outbox, _) = tokio::sync::broadcast::channel::<String>(256);
        let bridge = FakeBridge {
            port,
            received: Arc::new(Mutex::new(Vec::new())),
            outbox: Arc::new(outbox),
            connections: Arc::new(Mutex::new(0)),
        };
        let shared = bridge.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else { return };
                    *shared.connections.lock().unwrap() += 1;
                    let (mut sink, mut stream) = ws.split();
                    let mut rx = shared.outbox.subscribe();
                    loop {
                        tokio::select! {
                            message = stream.next() => match message {
                                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                                    shared.received.lock().unwrap().push(text.as_str().to_owned());
                                }
                                Some(Ok(_)) => {}
                                _ => break,
                            },
                            frame = rx.recv() => match frame {
                                Ok(frame) => {
                                    if sink.send(tokio_tungstenite::tungstenite::Message::text(frame)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            },
                        }
                    }
                    *shared.connections.lock().unwrap() -= 1;
                });
            }
        });
        bridge
    }

    pub fn send(&self, frame: &str) {
        let _ = self.outbox.send(frame.to_owned());
    }

    pub fn connections(&self) -> usize {
        *self.connections.lock().unwrap()
    }

    pub fn frames(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }
}

/// Waits for a condition, polling.
pub async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
