//! The Rust server against `recog/web/server.py` on the same synthetic data: every
//! route with its success and error cases, compared by status code, the headers that
//! matter, the body (JSON as values in key order, clock readings and fresh UUIDs
//! masked), audio bytes, the prompts the stub CLIs received and the files each server
//! wrote. See tests/common/mod.rs for the fixture and the masking.

mod common;

use std::path::Path;

use common::{
    ACTIVE, CALL, COMPARED_HEADERS, DOOMED, EMPTY, ENDED, EXTERNAL, HttpResponse, Server, Side, build_fixture,
    json_headers, mask, obj, snapshot_files,
};
use recog_core::pyjson::{self, Value};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Both servers on identical data, the bridge address pointing nowhere.
fn start_pair(base: &Path, python: &Path, bridge_ports: (u16, u16)) -> (Server, Server) {
    build_fixture(&base.join("py"));
    copy_dir(&base.join("py"), &base.join("rs"));
    let py = Server::start(Side::Python, &base.join("py"), python, bridge_ports.0);
    let rs = Server::start(Side::Rust, &base.join("rs"), python, bridge_ports.1);
    (py, rs)
}

struct Diff {
    failures: Vec<String>,
    steps: usize,
}

fn is_json(response: &HttpResponse) -> bool {
    response.header("content-type").is_some_and(|t| t.starts_with("application/json"))
}

impl Diff {
    fn new() -> Self {
        Self { failures: Vec::new(), steps: 0 }
    }

    fn check(&mut self, step: &str, py: &HttpResponse, rs: &HttpResponse) {
        self.steps += 1;
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
        let masked_changes = is_json(py) && (mask(&py.json()) != py.json() || mask(&rs.json()) != rs.json());
        for name in COMPARED_HEADERS {
            if name == "content-length" && masked_changes {
                continue;
            }
            if py.header(name) != rs.header(name) {
                self.failures.push(format!("{step}: header {name}: {:?} != {:?}", py.header(name), rs.header(name)));
            }
        }
        if is_json(py) {
            let (a, b) = (mask(&py.json()), mask(&rs.json()));
            if a != b {
                self.failures.push(format!(
                    "{step}: body differs\n  py: {}\n  rs: {}",
                    pyjson::dumps(&a, None),
                    pyjson::dumps(&b, None)
                ));
            }
        } else if py.body != rs.body {
            self.failures.push(format!(
                "{step}: body differs\n  py: {:?}\n  rs: {:?}",
                short(&py.body),
                short(&rs.body)
            ));
        }
    }

    fn same(
        &mut self,
        py: &Server,
        rs: &Server,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (HttpResponse, HttpResponse) {
        let a = py.request(method, path, headers, body);
        let b = rs.request(method, path, headers, body);
        self.check(&format!("{method} {path} {}", String::from_utf8_lossy(&body[..body.len().min(120)])), &a, &b);
        (a, b)
    }

    fn json(&mut self, py: &Server, rs: &Server, method: &str, path: &str, body: &str) -> (HttpResponse, HttpResponse) {
        self.same(py, rs, method, path, &json_headers(), body.as_bytes())
    }

    fn finish(self) {
        assert!(
            self.failures.is_empty(),
            "{} of {} steps differ:\n\n{}",
            self.failures.len(),
            self.steps,
            self.failures.join("\n\n")
        );
    }
}

fn short(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(&body[..body.len().min(300)]).into_owned();
    format!("{} bytes: {text}", body.len())
}

fn revision(response: &HttpResponse) -> String {
    let value = response.json();
    let map = value.as_object().unwrap();
    let revision = match map.get("profile") {
        Some(profile) => profile.as_object().unwrap().get("revision").unwrap(),
        None => map.get("revision").unwrap(),
    };
    revision.py_str()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_http_route_given_the_same_data_when_requested_then_python_and_rust_answer_alike() {
    let Some(python) = common::python() else { return };
    let base = tempfile::tempdir().unwrap();
    let dead = (common::free_port(), common::free_port());
    let (py, rs) = start_pair(base.path(), &python, dead);
    let mut d = Diff::new();

    // Reads of the shipped state.
    for path in [
        "/api/desktop/status",
        "/api/ai-settings",
        "/api/privacy",
        "/api/user-profile",
        "/api/meetings",
        "/api/search?q=%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82",
        "/api/search?q=HELLO",
        "/api/search?q=h",
        "/api/search",
        "/api/search?q=imported",
        &format!("/api/meetings/{ENDED}"),
        &format!("/api/meetings/{ENDED}/transcript"),
        &format!("/api/meetings/{EXTERNAL}"),
        "/api/meetings/absent",
        "/api/meetings/absent/transcript",
        "/api/meetings/a%2Fb",
        "/api/meetings/",
        "/api/meetings/x/y/z",
        "/api/prompts",
        &format!("/api/meetings/{ENDED}/increments"),
        &format!("/api/meetings/{ENDED}/increments/my-increment"),
        &format!("/api/meetings/{ENDED}/increments/my-increment/1"),
        &format!("/api/meetings/{ENDED}/increments/my-increment/9"),
        &format!("/api/meetings/{ENDED}/increments/my-increment/abc"),
        &format!("/api/meetings/{ENDED}/increments/nothing-here"),
        "/api/meetings/absent/increments",
        "/api/meetings/..%5Cx/increments",
        &format!("/api/meetings/{ENDED}/increments/..%5Cp"),
        "/api/live/status",
        "/api/pcm-diagnostics",
        "/api/debug/ai-context",
        "/api/debug/ai-context?include_segments=yes&limit=1",
        "/api/debug/ai-context?limit=x&include_segments=2",
        &format!("/api/meetings/{ENDED}/chat"),
        &format!("/api/meetings/{EXTERNAL}/chat"),
        "/api/meetings/absent/chat",
        "/api/meetings/..%5Cm1/chat",
        "/api/nope",
        "/",
        "/static/app.js",
        "/static/style.css",
        "/static/index.html",
        "/static/favicon.svg",
        "/static/nope.js",
        "/static/",
        "/static",
        "/static/../server.py",
        "/static/%2e%2e/server.py",
    ] {
        d.same(&py, &rs, "GET", path, &[], b"");
    }
    let debug_py = py.request("GET", "/api/debug", &[], b"");
    let debug_rs = rs.request("GET", "/api/debug", &[], b"");
    d.check("GET /api/debug", &debug_py, &debug_rs);

    // Methods, conditional and ranged requests.
    for (method, path) in [
        ("HEAD", "/api/meetings"),
        ("POST", "/api/user-profile"),
        ("PATCH", "/api/privacy"),
        ("POST", "/"),
        ("HEAD", "/"),
        ("HEAD", "/static/app.js"),
        ("POST", "/static/app.js"),
        ("DELETE", "/api/live/status"),
        ("GET", "/api/meetings/x/increments/p/generate"),
    ] {
        d.same(&py, &rs, method, path, &[], b"");
    }
    let etag = py.request("GET", "/static/app.js", &[], b"").header("etag").unwrap().to_owned();
    let modified = py.request("GET", "/static/app.js", &[], b"").header("last-modified").unwrap().to_owned();
    for headers in [
        vec![("If-None-Match", etag.as_str())],
        vec![("If-None-Match", "W/\"other\", \"x\"")],
        vec![("If-Modified-Since", modified.as_str())],
        vec![("If-Modified-Since", "Thu, 01 Jan 1970 00:00:00 GMT")],
        vec![("Range", "bytes=0-9")],
        vec![("Range", "bytes=100-")],
        vec![("Range", "bytes=-50")],
        vec![("Range", "bytes=99999999-")],
        vec![("Range", "items=0-1")],
        vec![("Range", "bytes=5-2")],
        vec![("Range", "bytes=x-y")],
        vec![("Range", "bytes=0-9"), ("If-Range", etag.as_str())],
        vec![("Range", "bytes=0-9"), ("If-Range", "\"stale\"")],
    ] {
        d.same(&py, &rs, "GET", "/static/app.js", &headers, b"");
    }
    d.same(&py, &rs, "GET", "/", &[("Range", "bytes=0-20")], b"");
    d.same(&py, &rs, "GET", "/", &[("If-None-Match", etag.as_str())], b"");
    // Several ranges: a random boundary, so the structure is compared.
    let (a, b) = (
        py.request("GET", "/static/app.js", &[("Range", "bytes=0-4,10-14")], b""),
        rs.request("GET", "/static/app.js", &[("Range", "bytes=0-4,10-14")], b""),
    );
    assert_eq!(
        (a.status, a.body.len(), a.header("content-length")),
        (b.status, b.body.len(), b.header("content-length"))
    );
    assert!(b.header("content-type").unwrap().starts_with("multipart/byteranges; boundary="));

    // Audio slices.
    for path in [
        format!("/api/meetings/{ENDED}/audio/0"),
        format!("/api/meetings/{ENDED}/audio/1"),
        format!("/api/meetings/{ENDED}/audio/1?track=mic"),
        format!("/api/meetings/{ENDED}/audio/4"),
        format!("/api/meetings/{ENDED}/audio/5"),
        format!("/api/meetings/{ENDED}/audio/99"),
        format!("/api/meetings/{ENDED}/audio/-1"),
        format!("/api/meetings/{ENDED}/audio/abc"),
        format!("/api/meetings/{CALL}/audio/0"),
        format!("/api/meetings/{CALL}/audio/1"),
        format!("/api/meetings/{DOOMED}/audio/0"),
        format!("/api/meetings/{EXTERNAL}/audio/0"),
        "/api/meetings/absent/audio/0".to_owned(),
        "/api/meetings/..%5Cx/audio/0".to_owned(),
    ] {
        d.same(&py, &rs, "GET", &path, &[], b"");
    }

    // User profile.
    for body in [
        "{}",
        r#"{"name": ""}"#,
        r#"{"name": "Me"}"#,
        r#"{"name": "Alex\n"}"#,
        &format!(r#"{{"name": "{}"}}"#, "a".repeat(81)),
        r#"{"name": null}"#,
        r#"{"name": 123}"#,
        r#"{"name": "Alex", "aliases": "Al"}"#,
        r#"{"name": "Alex", "aliases": [null, 1]}"#,
        &format!(r#"{{"name": "Alex", "aliases": [{}]}}"#, vec!["\"a\""; 17].join(", ")),
        "[1]",
        "null",
        "not json",
        "{\"name\": \"x\",}",
        "",
    ] {
        d.json(&py, &rs, "PUT", "/api/user-profile", body);
    }
    d.same(&py, &rs, "PUT", "/api/user-profile", &[], br#"{"name": "x"}"#);
    d.same(&py, &rs, "PUT", "/api/user-profile", &[("Content-Type", "application/json")], &[b'{', 0xff, b'}']);
    let deep = format!("{}{}", "[".repeat(12000), "]".repeat(12000));
    d.json(&py, &rs, "PUT", "/api/user-profile", &deep);
    d.json(
        &py,
        &rs,
        "PUT",
        "/api/user-profile",
        r#"{"name": " Алексей ", "aliases": ["Лёша", "Лёша", " "], "extra": 1}"#,
    );
    d.same(&py, &rs, "GET", "/api/user-profile", &[], b"");
    d.same(&py, &rs, "DELETE", "/api/user-profile", &[], b"");
    d.json(&py, &rs, "PUT", "/api/user-profile", r#"{"name": "Alex", "aliases": ["Al"]}"#);

    // Privacy.
    for body in [r#"{"enabled": "false"}"#, r#"{"enabled": true, "x": 1}"#, r#"{"enabled": 1}"#, r#"{"enabled": true}"#]
    {
        d.json(&py, &rs, "PUT", "/api/privacy", body);
    }
    d.same(&py, &rs, "GET", "/api/privacy", &[], b"");

    // AI settings: the revision each server handed out goes back to it.
    let (a, b) = d.same(&py, &rs, "GET", "/api/ai-settings", &[], b"");
    let (rev_py, rev_rs) = (revision(&a), revision(&b));
    let models =
        |rev: &str| format!(r#"{{"revision": "{rev}", "models": {{"claude": "picked-claude", "codex": ""}}}}"#);
    let (a, b) = (
        py.request("PUT", "/api/ai-settings/models", &json_headers(), models(&rev_py).as_bytes()),
        rs.request("PUT", "/api/ai-settings/models", &json_headers(), models(&rev_rs).as_bytes()),
    );
    d.check("PUT models", &a, &b);
    let (rev_py, rev_rs) = (revision(&a), revision(&b));
    for body in [
        r#"{"revision": "defaults", "models": {"claude": "stale"}}"#,
        r#"{"models": {"a": 1}, "revision": null, "z": 2}"#,
        r#"{"revision": "x"}"#,
    ] {
        d.json(&py, &rs, "PUT", "/api/ai-settings/models", body);
    }
    let task = |rev: &str, extra: &str| {
        format!(
            r#"{{"revision": "{rev}", "prompt": "Reply in English", "providers": ["claude"], "models": {{}}{extra}}}"#
        )
    };
    for extra in [
        r#", "providers": ["codex"]"#,
        r#", "providers": []"#,
        r#", "prompt": """#,
        r#", "models": {"claude": "has whitespace"}"#,
        r#", "models": {"unknown": "model"}"#,
        r#", "secret": "x""#,
        r#", "enabled": "false""#,
        r#", "claude_effort": "minimal""#,
    ] {
        let (a, b) = (
            py.request("PUT", "/api/ai-settings/tasks/copilot", &json_headers(), task(&rev_py, extra).as_bytes()),
            rs.request("PUT", "/api/ai-settings/tasks/copilot", &json_headers(), task(&rev_rs, extra).as_bytes()),
        );
        d.check(&format!("PUT task copilot {extra}"), &a, &b);
    }
    let (a, b) = (
        py.request(
            "PUT",
            "/api/ai-settings/tasks/copilot",
            &json_headers(),
            task(&rev_py, r#", "claude_effort": "max", "enabled": false"#).as_bytes(),
        ),
        rs.request(
            "PUT",
            "/api/ai-settings/tasks/copilot",
            &json_headers(),
            task(&rev_rs, r#", "claude_effort": "max", "enabled": false"#).as_bytes(),
        ),
    );
    d.check("PUT task copilot valid", &a, &b);
    let (rev_py, rev_rs) = (revision(&a), revision(&b));
    for (route, body) in [
        ("analysis.increment", r#"{"revision": "R", "prompt": "Missing transcript", "providers": ["claude"]}"#),
        ("title", r#"{"revision": "R", "prompt": "{transcript} {unknown}", "providers": ["codex"]}"#),
        (
            "title",
            r#"{"revision": "R", "prompt": "{transcript}", "providers": ["codex"], "reasoning_effort": "invalid"}"#,
        ),
        ("unknown", r#"{"revision": "R", "prompt": "{transcript}", "providers": ["claude"]}"#),
        ("analysis.summary", r#"{"revision": "R", "prompt": "{transcript}", "providers": ["claude"]}"#),
        ("copilot", r#"{"revision": "defaults", "prompt": "stale", "providers": ["claude"]}"#),
    ] {
        let path = format!("/api/ai-settings/tasks/{route}");
        let (a, b) = (
            py.request("PUT", &path, &json_headers(), body.replace("\"R\"", &format!("\"{rev_py}\"")).as_bytes()),
            rs.request("PUT", &path, &json_headers(), body.replace("\"R\"", &format!("\"{rev_rs}\"")).as_bytes()),
        );
        d.check(&format!("PUT {path} {body}"), &a, &b);
    }
    let title_task = |rev: &str| {
        format!(
            r#"{{"revision": "{rev}", "prompt": "Название для {{meeting_title}}: {{transcript}}", "providers": ["claude"], "models": {{"claude": "title-model"}}}}"#
        )
    };
    let (a, b) = (
        py.request("PUT", "/api/ai-settings/tasks/title", &json_headers(), title_task(&rev_py).as_bytes()),
        rs.request("PUT", "/api/ai-settings/tasks/title", &json_headers(), title_task(&rev_rs).as_bytes()),
    );
    d.check("PUT task title", &a, &b);
    let (rev_py, rev_rs) = (revision(&a), revision(&b));
    d.same(&py, &rs, "DELETE", "/api/ai-settings/tasks/copilot", &[], b"");
    d.same(&py, &rs, "DELETE", "/api/ai-settings/tasks/copilot?revision=defaults", &[], b"");
    d.same(&py, &rs, "DELETE", "/api/ai-settings/tasks/nothing?revision=defaults", &[], b"");
    let (a, b) = (
        py.request("DELETE", &format!("/api/ai-settings/tasks/copilot?revision={rev_py}"), &[], b""),
        rs.request("DELETE", &format!("/api/ai-settings/tasks/copilot?revision={rev_rs}"), &[], b""),
    );
    d.check("DELETE task copilot", &a, &b);
    d.same(&py, &rs, "GET", "/api/ai-settings", &[], b"");

    // Debug AI context.
    for body in [
        "{}",
        r#"{"max_segments": "x", "segments": [1], "from_id": 1.5}"#,
        &format!(r#"{{"meeting_id": "{ENDED}", "from_id": 2, "to_id": 3}}"#),
        &format!(r#"{{"meeting_id": "{ENDED}", "start_ts_display": "10:00:09", "max_segments": 2}}"#),
        &format!(r#"{{"meeting_id": "{ENDED}", "window_seconds": 10}}"#),
        r#"{"meeting_title": "обзор"}"#,
        r#"{"meeting_title": "Nothing like it"}"#,
        r#"{"meeting_id": "absent"}"#,
        &format!(r#"{{"meeting_id": "{ENDED}", "start_ts_display": "99:99:99"}}"#),
        r#"{"segments": [{"text": "debug text", "speaker": "D", "ts_display": "1"}], "user_name": "Debugger", "meeting_title": "T"}"#,
    ] {
        d.json(&py, &rs, "POST", "/api/debug/ai-context", body);
    }
    d.same(&py, &rs, "GET", "/api/debug/ai-context?include_segments=true", &[], b"");

    // Analysis with the stub CLIs.
    for (path, body) in [
        (format!("/api/meetings/{ENDED}/analyze"), "{}"),
        (format!("/api/meetings/{ENDED}/analyze"), r#"{"question": "custom"}"#),
        (format!("/api/meetings/{ENDED}/analyze"), r#"{"question": "increment", "debug": "maybe"}"#),
        (format!("/api/meetings/{ENDED}/analyze"), r#"{"debug": true}"#),
        (format!("/api/meetings/{ENDED}/analyze"), r#"{"x": 1}"#),
        (format!("/api/meetings/{ENDED}/analyze"), ""),
        (format!("/api/meetings/{ENDED}/analyze-multi"), "{}"),
        (format!("/api/meetings/{EMPTY}/analyze"), "{}"),
        ("/api/meetings/absent/analyze".to_owned(), "{}"),
        ("/api/meetings/absent/analyze-multi".to_owned(), r#"{"question": "summary"}"#),
    ] {
        d.json(&py, &rs, "POST", &path, body);
    }
    for path in [
        format!("/api/meetings/{ENDED}/analyze-stream?question=custom&debug=xx"),
        "/api/meetings/absent/analyze-stream".to_owned(),
    ] {
        d.same(&py, &rs, "GET", &path, &[], b"");
    }
    let (a, b) = (
        py.request("GET", &format!("/api/meetings/{ENDED}/analyze-stream"), &[], b""),
        rs.request("GET", &format!("/api/meetings/{ENDED}/analyze-stream"), &[], b""),
    );
    assert_eq!(
        (a.status, a.header("content-type"), a.header("cache-control")),
        (b.status, b.header("content-type"), b.header("cache-control"))
    );
    assert_eq!(sse_by_provider(&a), sse_by_provider(&b), "analyze-stream events differ");
    d.same(&py, &rs, "DELETE", "/api/debug/ai-context", &[], b"");

    // Questions about a meeting.
    for (meeting, body) in [
        (ENDED, r#"{"question": "Что сказал Bob?"}"#),
        (ENDED, r#"{"question": "А он уверен?", "extra": true}"#),
        (ENDED, r#"{"question": "   "}"#),
        (ENDED, r#"{"question": 5}"#),
        (EMPTY, r#"{"question": "Есть кто?"}"#),
        ("absent", r#"{"question": "Есть кто?"}"#),
        ("..%5Cm1", r#"{"question": "Есть кто?"}"#),
    ] {
        d.json(&py, &rs, "POST", &format!("/api/meetings/{meeting}/chat"), body);
    }
    d.json(
        &py,
        &rs,
        "POST",
        &format!("/api/meetings/{ENDED}/chat"),
        &format!(r#"{{"question": "{}"}}"#, "x".repeat(4001)),
    );
    d.same(&py, &rs, "GET", &format!("/api/meetings/{ENDED}/chat"), &[], b"");
    d.same(&py, &rs, "DELETE", &format!("/api/meetings/{CALL}/chat"), &[], b"");
    d.same(&py, &rs, "DELETE", "/api/meetings/absent/chat", &[], b"");

    // Titles and increments.
    d.same(&py, &rs, "POST", &format!("/api/meetings/{ENDED}/generate-title"), &[], b"");
    d.same(&py, &rs, "POST", &format!("/api/meetings/{EMPTY}/generate-title"), &[], b"");
    d.same(&py, &rs, "POST", "/api/meetings/absent/generate-title", &[], b"");
    for path in [
        format!("/api/meetings/{ENDED}/increments/my-increment/generate"),
        format!("/api/meetings/{ENDED}/increments/my-increment/generate"),
        format!("/api/meetings/{ENDED}/increments/my-increment/generate?force=true"),
        format!("/api/meetings/{ENDED}/increments/my-increment/generate?force=maybe"),
        format!("/api/meetings/{ENDED}/increments/my-increment/generate?participant_id=nobody"),
        format!("/api/meetings/{ENDED}/increments/my-increment/generate?participant_id=alice"),
        format!("/api/meetings/{ENDED}/increments/no-such-prompt/generate"),
        format!("/api/meetings/{EXTERNAL}/increments/my-increment/generate"),
        "/api/meetings/absent/increments/my-increment/generate".to_owned(),
    ] {
        d.same(&py, &rs, "POST", &path, &[], b"");
    }
    d.same(&py, &rs, "GET", &format!("/api/meetings/{ENDED}/increments"), &[], b"");
    d.same(&py, &rs, "GET", &format!("/api/meetings/{ENDED}/increments/my-increment/2"), &[], b"");

    // Deletion.
    for path in [
        "/api/meetings/..%5Cx".to_owned(),
        format!("/api/meetings/{ACTIVE}"),
        format!("/api/meetings/{DOOMED}"),
        format!("/api/meetings/{DOOMED}"),
        "/api/meetings/absent".to_owned(),
    ] {
        d.same(&py, &rs, "DELETE", &path, &[], b"");
    }
    d.same(&py, &rs, "GET", "/api/meetings", &[], b"");
    d.same(&py, &rs, "GET", "/api/search?q=bye", &[], b"");
    d.same(&py, &rs, "GET", "/api/live/status", &[], b"");

    // The prompts each CLI received, and the files each server wrote.
    let (calls_py, calls_rs) = (py.stub_calls(), rs.stub_calls());
    let calls = |calls: &[Value]| calls.iter().map(|c| pyjson::dumps(c, None)).collect::<Vec<_>>();
    let mut sorted_py = calls(&calls_py);
    let mut sorted_rs = calls(&calls_rs);
    sorted_py.sort();
    sorted_rs.sort();
    if sorted_py != sorted_rs {
        d.failures.push(format!("stub CLI calls differ:\n  py: {sorted_py:#?}\n  rs: {sorted_rs:#?}"));
    }
    // The comparison saw the work: every AI route reached a stub, every write a file.
    assert!(calls_py.len() >= 8, "only {} stub calls", calls_py.len());
    let (files_py, files_rs) = (snapshot_files(&py.data()), snapshot_files(&rs.data()));
    for written in [
        format!("meetings/{ENDED}/increments/my-increment/v2.md"),
        format!("meetings/{ENDED}/chat.jsonl"),
        "ai-settings.json".to_owned(),
        "user-profile.json".to_owned(),
        "privacy.json".to_owned(),
    ] {
        assert!(files_py.contains_key(&written), "{written} missing: {:?}", files_py.keys());
    }
    assert!(!files_py.keys().any(|k| k.contains(DOOMED)), "the deleted meeting is still there");
    for (name, content) in &files_py {
        match files_rs.get(name) {
            Some(other) if other == content => {}
            Some(other) => d.failures.push(format!("file {name} differs\n  py: {content}\n  rs: {other}")),
            None => d.failures.push(format!("file {name} written only by Python")),
        }
    }
    for name in files_rs.keys().filter(|n| !files_py.contains_key(*n)) {
        d.failures.push(format!("file {name} written only by Rust"));
    }
    d.finish();
}

/// The start events, each provider's events and the final event of a stream.
type SseEvents = (Vec<Value>, Vec<(String, Vec<Value>)>, Option<Value>);

/// The events of an `analyze-stream` answer: the start events in order, each
/// provider's own events in order, and the final event.
fn sse_by_provider(response: &HttpResponse) -> SseEvents {
    let events: Vec<Value> =
        response.text().split("\n\n").filter_map(|chunk| chunk.strip_prefix("data: ")).map(obj).collect();
    let kind = |e: &Value| e.as_object().unwrap().get("type").unwrap().py_str();
    let starts = events.iter().filter(|e| kind(e) == "start").cloned().collect();
    let mut providers: Vec<(String, Vec<Value>)> = Vec::new();
    for event in events.iter().filter(|e| kind(e) != "start" && kind(e) != "complete") {
        let provider = event.as_object().unwrap().get("provider").map(Value::py_str).unwrap_or_default();
        match providers.iter_mut().find(|(p, _)| *p == provider) {
            Some((_, list)) => list.push(event.clone()),
            None => providers.push((provider, vec![event.clone()])),
        }
    }
    providers.sort_by(|a, b| a.0.cmp(&b.0));
    (starts, providers, events.last().cloned())
}

#[test]
fn masking_given_clock_and_uuid_values_when_compared_then_only_those_are_hidden() {
    // given
    let value = obj(
        r#"{"revision": "8a3fe3b7-676f-420c-adba-0337d39f9ee3", "set_at": "2026-01-01T00:00:00", "title": "kept", "is_active": true, "duration_seconds": 5}"#,
    );
    // when
    let masked = mask(&value);
    // then
    assert_eq!(
        pyjson::dumps(&masked, None),
        r#"{"revision": "<uuid>", "set_at": "<clock>", "title": "kept", "is_active": true, "duration_seconds": "<varies>"}"#
    );
}
