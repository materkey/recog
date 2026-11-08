//! The WebSocket side against `recog/web/server.py`: each server gets its own fake
//! bridge, and the same browser sessions run against both. Compared: the frames the
//! subscriber sends the bridge (hello, profile and settings refreshes, relayed Copilot
//! frames), the live-stream frames a tab receives (`init`, the agent-state primer, the
//! dialog history, relayed bridge frames, status broadcasts, transcript tails, meeting
//! changes), the Origin gate, the acknowledgement protocol of the profile and the AI
//! settings, and the per-meeting watch.

mod common;

use std::path::Path;
use std::time::Duration;

use common::ws::{WsClient, connect};
use common::{ACTIVE, ENDED, FakeBridge, HttpResponse, Server, Side, build_fixture, json_headers, mask, obj, until};
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

fn masked(frames: &[String]) -> Vec<String> {
    frames.iter().map(|f| pyjson::dumps(&mask(&obj(f)), None)).collect()
}

fn localized(server: &Server, frames: Vec<String>) -> Vec<String> {
    let root = server.root.to_string_lossy().into_owned();
    frames.into_iter().map(|f| f.replace(&root, "<root>")).collect()
}

/// Both clients' frames, after the given count, masked and compared.
fn same_frames(what: &str, py: (&Server, &WsClient), rs: (&Server, &WsClient)) {
    let a = masked(&localized(py.0, py.1.frames()));
    let b = masked(&localized(rs.0, rs.1.frames()));
    assert_eq!(a, b, "{what}: live frames differ");
}

fn same_bridge_frames(what: &str, py: &FakeBridge, rs: &FakeBridge) {
    assert_eq!(masked(&py.frames()), masked(&rs.frames()), "{what}: frames to the bridge differ");
}

async fn wait_count(what: &str, client: &WsClient, n: usize) {
    let frames = std::sync::Arc::clone(&client.frames);
    until(what, move || frames.lock().unwrap().len() >= n).await;
}

async fn wait_bridge(what: &str, bridge: &FakeBridge, n: usize) {
    let frames = std::sync::Arc::clone(&bridge.received);
    until(what, move || frames.lock().unwrap().len() >= n).await;
}

fn last_frame_field(bridge: &FakeBridge, kind: &str, field: &str) -> String {
    let frame = bridge
        .frames()
        .into_iter()
        .rev()
        .map(|f| obj(&f))
        .find(|f| f.as_object().unwrap().get("type") == Some(&Value::from(kind)))
        .unwrap();
    frame.as_object().unwrap().get(field).unwrap().py_str()
}

fn blocking_request(
    server: &Server,
    method: &'static str,
    path: String,
    body: String,
) -> impl std::future::Future<Output = HttpResponse> + Send + 'static {
    let port = server.port;
    let root = server.root.to_string_lossy().into_owned();
    async move {
        let response = tokio::task::spawn_blocking(move || {
            common::try_request(port, method, &path, &json_headers(), body.as_bytes()).unwrap()
        })
        .await
        .unwrap();
        HttpResponse { body: String::from_utf8_lossy(&response.body).replace(&root, "<root>").into_bytes(), ..response }
    }
}

fn append(path: &Path, row: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{row}").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browser_and_bridge_sessions_given_the_same_data_when_run_then_python_and_rust_send_the_same_frames() {
    let Some(python) = common::python() else { return };
    let base = tempfile::tempdir().unwrap();
    build_fixture(&base.path().join("py"));
    copy_dir(&base.path().join("py"), &base.path().join("rs"));
    let (bridge_py, bridge_rs) = (FakeBridge::start().await, FakeBridge::start().await);
    let py = Server::start(Side::Python, &base.path().join("py"), &python, bridge_py.port);
    let rs = Server::start(Side::Rust, &base.path().join("rs"), &python, bridge_rs.port);

    // The subscriber announces itself, then replays the saved profile and settings.
    wait_bridge("python hello and refreshes", &bridge_py, 3).await;
    wait_bridge("rust hello and refreshes", &bridge_rs, 3).await;
    same_bridge_frames("connect", &bridge_py, &bridge_rs);
    assert_eq!(bridge_rs.frames()[0], r#"{"type": "subscriber_hello"}"#);

    // The Origin gate.
    for headers in [
        vec![("Origin", "https://evil.example")],
        vec![("Origin", "null")],
        vec![("Origin", "https://evil-ktalk.ru")],
        vec![("Origin", "http://127.0.0.1:9080"), ("Origin", "https://evil.example")],
    ] {
        let (a, b) = (
            connect(py.port, "/api/live/stream", &headers).await,
            connect(rs.port, "/api/live/stream", &headers).await,
        );
        assert!(a.is_err() && b.is_err(), "{headers:?}: python {:?}, rust {:?}", a.is_ok(), b.is_ok());
    }
    for headers in [
        vec![],
        vec![("Origin", "http://127.0.0.1:9080")],
        vec![("Origin", "https://team.ktalk.ru")],
        vec![("Origin", "https://meet.example.org")],
        vec![("Origin", "https://extra.example.org")],
    ] {
        let a = connect(py.port, "/api/live/stream", &headers).await.unwrap();
        let b = connect(rs.port, "/api/live/stream", &headers).await.unwrap();
        wait_count("init", &a, 1).await;
        wait_count("init", &b, 1).await;
        same_frames(&format!("init with {headers:?}"), (&py, &a), (&rs, &b));
        a.close().await;
        b.close().await;
    }

    // A tab, then bridge frames relayed to it; the dialog and the chip are kept.
    let tab_py = connect(py.port, "/api/live/stream", &[]).await.unwrap();
    let tab_rs = connect(rs.port, "/api/live/stream", &[]).await.unwrap();
    wait_count("init", &tab_py, 1).await;
    wait_count("init", &tab_rs, 1).await;
    let relayed = [
        r#"{"type": "copilot_status", "status": "agent_state", "agent_state": "online"}"#,
        r#"{"type": "copilot_chat_echo", "request_id": "r1", "text": "вопрос", "created_at": 1.0}"#,
        r#"{"type": "copilot_reply", "request_id": "r1", "kind": "assistant", "text": "ответ", "created_at": 2.0}"#,
        r#"{"type": "transcript_refine", "updates": [{"segment_id": 1, "text": "уточнено"}]}"#,
        r#"{"type": "copilot_status", "status": "working", "request_id": "r2"}"#,
        r#"{"type": "totally_unknown"}"#,
        r#"not json"#,
        r#"{"type": ["copilot_reply"]}"#,
        r#"{"type": "copilot_status", "status": "agent_state", "agent_state": "working"}"#,
    ];
    for frame in relayed {
        bridge_py.send(frame);
        bridge_rs.send(frame);
    }
    wait_count("relayed frames", &tab_py, 7).await;
    wait_count("relayed frames", &tab_rs, 7).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    same_frames("relayed bridge frames", (&py, &tab_py), (&rs, &tab_rs));
    let late_py = connect(py.port, "/api/live/stream", &[]).await.unwrap();
    let late_rs = connect(rs.port, "/api/live/stream", &[]).await.unwrap();
    wait_count("init, primer, history", &late_py, 3).await;
    wait_count("init, primer, history", &late_rs, 3).await;
    same_frames("late tab", (&py, &late_py), (&rs, &late_rs));
    late_py.close().await;
    late_rs.close().await;

    // Browser frames go upstream verbatim; others are dropped.
    let before = bridge_py.frames().len();
    for frame in [
        r#"{"type": "transcript", "request_id": "x"}"#,
        r#"[1, 2]"#,
        r#"{"type": 5}"#,
        r#"garbage {"#,
        r#"{"type": "copilot_chat_message", "request_id": "ok-1", "text": "Привет"}"#,
        r#"{"type": "copilot_spawn_request", "request_id": "spawn-1"}"#,
        r#"{"type": "copilot_stop_request"}"#,
    ] {
        tab_py.send(frame).await;
        tab_rs.send(frame).await;
    }
    wait_bridge("upstream frames", &bridge_py, before + 3).await;
    wait_bridge("upstream frames", &bridge_rs, before + 3).await;
    same_bridge_frames("upstream", &bridge_py, &bridge_rs);
    assert_eq!(
        bridge_rs.frames()[before],
        r#"{"type": "copilot_chat_message", "request_id": "ok-1", "text": "Привет"}"#
    );

    // The profile: saved, announced, acknowledged by the bridge, applied.
    let save_py =
        blocking_request(&py, "PUT", "/api/user-profile".into(), r#"{"name": "Alex", "aliases": ["Al"]}"#.into());
    let save_rs =
        blocking_request(&rs, "PUT", "/api/user-profile".into(), r#"{"name": "Alex", "aliases": ["Al"]}"#.into());
    let (saving_py, saving_rs) = (tokio::spawn(save_py), tokio::spawn(save_rs));
    let n = bridge_py.frames().len();
    wait_bridge("profile refresh", &bridge_py, n + 1).await;
    wait_bridge("profile refresh", &bridge_rs, n + 1).await;
    for bridge in [&bridge_py, &bridge_rs] {
        let revision = last_frame_field(bridge, "user_profile_refresh", "revision");
        bridge.send(&format!(
            r#"{{"type": "user_profile_applied", "revision": "{revision}", "identity": {{"local_user_name": "Alex", "local_user_aliases": ["Alex", "Al"], "local_user_profile_revision": "{revision}", "local_user_identity_known": true}}}}"#
        ));
    }
    let (a, b) = (saving_py.await.unwrap(), saving_rs.await.unwrap());
    assert_eq!(a.status, b.status);
    assert_eq!(mask(&a.json()), mask(&b.json()), "profile save answer");
    assert_eq!(a.json().as_object().unwrap().get("application_state"), Some(&Value::from("applied")));
    // A stale acknowledgement and an error frame.
    for bridge in [&bridge_py, &bridge_rs] {
        bridge.send(r#"{"type": "user_profile_applied", "revision": "stale", "identity": {}}"#);
        bridge.send(r#"{"type": "user_profile_error", "message": "private runtime detail"}"#);
    }

    // The AI settings: the same protocol.
    let view_py = py.request("GET", "/api/ai-settings", &[], b"").json();
    let view_rs = rs.request("GET", "/api/ai-settings", &[], b"").json();
    let rev = |v: &Value| v.as_object().unwrap().get("revision").unwrap().py_str();
    let body = |r: String| format!(r#"{{"revision": "{r}", "prompt": "Reply in English", "providers": ["claude"]}}"#);
    let (saving_py, saving_rs) = (
        tokio::spawn(blocking_request(&py, "PUT", "/api/ai-settings/tasks/copilot".into(), body(rev(&view_py)))),
        tokio::spawn(blocking_request(&rs, "PUT", "/api/ai-settings/tasks/copilot".into(), body(rev(&view_rs)))),
    );
    let n = bridge_py.frames().len();
    wait_bridge("settings refresh", &bridge_py, n + 1).await;
    wait_bridge("settings refresh", &bridge_rs, n + 1).await;
    for bridge in [&bridge_py, &bridge_rs] {
        let revision = last_frame_field(bridge, "ai_settings_refresh", "revision");
        bridge.send(&format!(
            r#"{{"type": "ai_settings_applied", "revision": "{revision}", "model_application": "next_session"}}"#
        ));
    }
    let (a, b) = (saving_py.await.unwrap(), saving_rs.await.unwrap());
    assert_eq!(mask(&a.json()), mask(&b.json()), "settings save answer");
    let application = a.json().as_object().unwrap().get("copilot_application").cloned().unwrap();
    assert_eq!(application.as_object().unwrap().get("state"), Some(&Value::from("applied")));
    for bridge in [&bridge_py, &bridge_rs] {
        bridge.send(r#"{"type": "ai_settings_error", "message": "private runtime detail"}"#);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        mask(&py.request("GET", "/api/user-profile", &[], b"").json()),
        mask(&rs.request("GET", "/api/user-profile", &[], b"").json())
    );
    assert_eq!(
        mask(&py.request("GET", "/api/ai-settings", &[], b"").json()),
        mask(&rs.request("GET", "/api/ai-settings", &[], b"").json())
    );
    let (a, b) = (
        py.request("PUT", "/api/privacy", &json_headers(), br#"{"enabled": true}"#),
        rs.request("PUT", "/api/privacy", &json_headers(), br#"{"enabled": true}"#),
    );
    assert_eq!((a.status, a.json()), (b.status, b.json()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    same_frames("status broadcasts", (&py, &tab_py), (&rs, &tab_rs));
    same_bridge_frames("refreshes", &bridge_py, &bridge_rs);

    // The transcript tail of the current meeting, the per-meeting watch, a new meeting.
    let watch_py = connect(py.port, &format!("/api/meetings/{ENDED}/watch"), &[]).await.unwrap();
    let watch_rs = connect(rs.port, &format!("/api/meetings/{ENDED}/watch"), &[]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let n = tab_py.frames().len();
    for server in [&py, &rs] {
        append(
            &server.meetings().join(ACTIVE).join("transcript.jsonl"),
            r#"{"id": 3, "text": "свежая строка", "speaker": "Bob"}"#,
        );
        append(
            &server.meetings().join(ENDED).join("transcript.jsonl"),
            r#"{"id": 7, "text": "late row", "speaker": "Bob"}"#,
        );
    }
    wait_count("live segments", &tab_py, n + 1).await;
    wait_count("live segments", &tab_rs, n + 1).await;
    wait_count("watched segments", &watch_py, 1).await;
    wait_count("watched segments", &watch_rs, 1).await;
    same_frames("watch", (&py, &watch_py), (&rs, &watch_rs));
    // The same rows rewritten in place are not sent again; a new row after them is.
    for server in [&py, &rs] {
        let path = server.meetings().join(ACTIVE).join("transcript.jsonl");
        let text = std::fs::read_to_string(&path).unwrap().replace("свежая строка", "уточнённая строка");
        let tmp = path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        append(&path, r#"{"id": 4, "text": "после переписи", "speaker": "Bob"}"#);
    }
    wait_count("rows after rewrite", &tab_py, n + 2).await;
    wait_count("rows after rewrite", &tab_rs, n + 2).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (a, b) = (masked(&tab_py.frames()[n..]), masked(&tab_rs.frames()[n..]));
    let rows = |frames: &[String]| -> Vec<String> {
        frames
            .iter()
            .flat_map(|f| match obj(f).as_object().unwrap().get("data") {
                Some(Value::Array(rows)) => rows.iter().map(|r| pyjson::dumps(r, None)).collect(),
                _ => vec![f.clone()],
            })
            .collect()
    };
    assert_eq!(rows(&a), rows(&b), "transcript tail after a rewrite");
    // A new active meeting announced by the index.
    for server in [&py, &rs] {
        let dir = server.meetings().join("2026-11-08_10-00-00_newnew00");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            r#"{"id": "2026-11-08_10-00-00_newnew00", "created_at": "2026-11-08T10:00:00+00:00", "ended_at": null, "source": "vas3k", "stats": {"segment_count": 0}}"#,
        )
        .unwrap();
        let index = server.meetings().join("index.json");
        let text = std::fs::read_to_string(&index).unwrap();
        std::fs::write(&index, text).unwrap();
    }
    let change = |client: &WsClient| client.frames().iter().any(|f| f.contains("meeting_change"));
    let (p, r) = (std::sync::Arc::clone(&tab_py.frames), std::sync::Arc::clone(&tab_rs.frames));
    until("meeting_change", move || {
        p.lock().unwrap().iter().any(|f| f.contains("meeting_change"))
            && r.lock().unwrap().iter().any(|f| f.contains("meeting_change"))
    })
    .await;
    assert!(change(&tab_py) && change(&tab_rs));
    let pick = |client: &WsClient| {
        masked(&client.frames().into_iter().filter(|f| f.contains("meeting_change")).collect::<Vec<_>>())
    };
    assert_eq!(pick(&tab_py), pick(&tab_rs), "meeting_change");

    // A binary frame ends the tab's connection on both.
    tab_py.send_binary(b"\x00\x01").await;
    tab_rs.send_binary(b"\x00\x01").await;
    let (cp, cr) = (std::sync::Arc::clone(&tab_py.closed), std::sync::Arc::clone(&tab_rs.closed));
    until("binary frame closes", move || cp.lock().unwrap().is_some() && cr.lock().unwrap().is_some()).await;
    assert_eq!(tab_py.closed(), tab_rs.closed());
    watch_py.close().await;
    watch_rs.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_bridge_given_a_tab_when_it_sends_copilot_frames_then_both_answer_bridge_disconnected() {
    let Some(python) = common::python() else { return };
    let base = tempfile::tempdir().unwrap();
    build_fixture(&base.path().join("py"));
    copy_dir(&base.path().join("py"), &base.path().join("rs"));
    let dead = common::free_port();
    let py = Server::start(Side::Python, &base.path().join("py"), &python, dead);
    let rs = Server::start(Side::Rust, &base.path().join("rs"), &python, dead);
    let a = connect(py.port, "/api/live/stream", &[]).await.unwrap();
    let b = connect(rs.port, "/api/live/stream", &[]).await.unwrap();
    for frame in [
        r#"{"type": "transcript", "request_id": "x"}"#,
        r#"{"type": "copilot_chat_message", "request_id": "probe"}"#,
        r#"{"type": "copilot_spawn_request"}"#,
        r#"{"type": "copilot_stop_request", "request_id": 7}"#,
    ] {
        a.send(frame).await;
        b.send(frame).await;
    }
    a.wait_frames(4).await;
    b.wait_frames(4).await;
    same_frames("bridge_disconnected replies", (&py, &a), (&rs, &b));
    assert_eq!(b.frames()[1], r#"{"type": "copilot_status", "status": "bridge_disconnected", "request_id": "probe"}"#);
}
