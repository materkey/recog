//! tests/test_ktalk_bridge.py from the top through the Copilot channel primers
//! (up to `test_copilot_chat_message_forwarded_to_channel_client`): client
//! lifecycle and roles, the Origin gate, participants and the local microphone,
//! `pcm_chunk` ingress, `send_refine`, and the subscriber and channel hellos.
//!
//! Python's `FakeWebSocket` (its queued messages, then the end of the stream) is
//! [`run_fake`]; `ControllableFakeWebSocket` (open until `signal_close()`) is
//! `Harness::connect` with `fake.close()`.
//!
//! Not ported:
//! - `test_given_legacy_request_headers_when_bridge_handles_client_then_guard_applied`:
//!   websockets 12/13 expose the handshake as `request_headers`; a [`Connection`]
//!   carries its Origin values whatever the transport, so there is no second path.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Harness, NOW, field, json, settle};
use futures_util::StreamExt;
use recog_bridge::hooks::Participant;
use recog_bridge::testing::{FakeConnection, FakeStore, RecordingLive, SwitchablePolicy, collecting_log};
use recog_bridge::{Bridge, BridgeConfig, Client, ClientId, Connection, RefineUpdate, Role};
use recog_core::pyjson::{self, Map, Value};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const ADDRESS: &str = "('127.0.0.1', 12345)";

/// Python's `FakeWebSocket`: the bridge handles a connection that delivers `messages`
/// and then ends, to completion.
async fn run_fake(h: &Harness, origins: Option<Vec<String>>, messages: &[&str]) -> FakeConnection {
    let (fake, connection): (FakeConnection, Connection) = FakeConnection::new(ADDRESS, origins);
    for message in messages {
        fake.send_text(message);
    }
    fake.close();
    h.bridge.handle_client(connection).await;
    fake
}

/// A client that is in no set (Python's `object()` used as a websocket).
fn loose_client(id: u64) -> Arc<Client> {
    Client::new(ClientId(id), recog_bridge::testing::RecordingSink::new(), ADDRESS)
}

fn num(value: &Value) -> f64 {
    match value {
        Value::Int(n) => *n as f64,
        Value::Float(x) => *x,
        other => panic!("not a number: {other:?}"),
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `_make_pcm_chunk_message()` with its defaults.
fn pcm_frame() -> Map {
    pcm_frame_with("participant-1", "Alice", false, 1, 1000.0, 4300.0, 16000, b"\x00\x01\x02\x03")
}

#[allow(clippy::too_many_arguments)]
fn pcm_frame_with(
    pid: &str,
    speaker_name: &str,
    is_local: bool,
    seq: i64,
    chunk_start_ms: f64,
    chunk_end_ms: f64,
    sample_rate: i64,
    pcm: &[u8],
) -> Map {
    let frame = recog_bridge::obj! {
        "type" => "pcm_chunk",
        "pid" => pid,
        "speaker_name" => speaker_name,
        "is_local" => is_local,
        "seq" => seq,
        "chunk_start_ms" => Value::Float(chunk_start_ms),
        "chunk_end_ms" => Value::Float(chunk_end_ms),
        "sample_rate" => sample_rate,
        "pcm_base64" => base64(pcm),
    };
    match frame {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

/// `_attach_meeting_storage(bridge, source)` with a PCM recorder
/// (`set_pcm_chunk_callback(received.append)`).
fn pcm_harness(source: &str) -> (Harness, Arc<FakeStore>, Arc<RecordingLive>) {
    let h = Harness::new();
    let storage = FakeStore::with_source(source);
    h.bridge.set_meeting_storage(storage.clone());
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    (h, storage, live)
}

/// The chunks the PCM callback received.
fn chunks(live: &RecordingLive) -> Vec<Value> {
    live.calls()
        .into_iter()
        .filter(|c| field(c, "call").as_str() == Some("pcm_chunk"))
        .map(|c| field(&c, "chunk").clone())
        .collect()
}

fn lines_with(h: &Harness, needle: &str) -> usize {
    h.logs().iter().filter(|l| l.contains(needle)).count()
}

fn participant(id: &str, name: &str, is_local: bool, mic_muted: bool) -> Participant {
    Participant { id: id.into(), name: name.into(), is_local, mic_muted }
}

fn set_participants(h: &Harness, participants: Vec<Participant>) {
    let () = h.bridge.with_speakers(|s| {
        s.participants.clear();
        for p in participants {
            s.participants.insert(p.id.clone(), p);
        }
    });
}

fn refine(segment_id: i64, text: &str) -> RefineUpdate {
    RefineUpdate { segment_id, text: text.into() }
}

// ---- client disconnect and the meeting ----

#[tokio::test]
async fn given_active_ktalk_meeting_and_one_client_when_it_disconnects_then_meeting_left_as_disconnected() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    h.bridge.set_meeting_storage(storage.clone());
    let (fake, task) = h.connect(None).await;
    // when
    fake.close();
    task.await.unwrap();
    settle().await;
    // then
    assert_eq!(storage.ops(), ["on_ktalk_leave"]);
    assert_eq!(field(&storage.calls()[0], "reason").as_str(), Some("disconnected"));
}

#[tokio::test]
async fn given_orphan_meeting_when_client_disconnects_then_meeting_not_ended() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("orphan");
    h.bridge.set_meeting_storage(storage.clone());
    let (fake, task) = h.connect(None).await;
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert!(storage.ops().is_empty());
}

#[tokio::test]
async fn given_another_client_connected_when_one_disconnects_then_meeting_not_ended() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    h.bridge.set_meeting_storage(storage.clone());
    let (_other_sink, _other) = h.add(Role::Extension);
    let (fake, task) = h.connect(None).await;
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert!(storage.ops().is_empty());
}

// ---- the Origin allowlist ----

#[tokio::test]
async fn given_disallowed_origin_when_bridge_handles_client_then_closed_before_registration() {
    // given
    let h = Harness::new();
    let hello = r#"{"type": "subscriber_hello"}"#;
    // when
    let fake = run_fake(&h, Some(vec!["https://evil.example".into()]), &[hello]).await;
    // then
    assert_eq!(fake.sink.closes.lock().unwrap().iter().map(|c| c.0).collect::<Vec<_>>(), [1008]);
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    assert!(h.bridge.clients_in(Role::Subscriber).is_empty());
    assert!(
        h.logs().iter().any(|l| l.starts_with(
            "[KTalk Bridge] Rejected WS connection: disallowed origin 'https://evil.example' (('127.0.0.1', 12345))"
        )),
        "{:?}",
        h.logs()
    );
    // The subscriber_hello frame was never read from the socket.
    assert_eq!(fake.queued(), 1);
}

#[tokio::test]
async fn given_allowed_origin_when_bridge_handles_client_then_frame_dispatched() {
    // given
    let h = Harness::new();
    // A frame whose effect survives the disconnect cleanup (speaker state does not).
    let frame = format!(r#"{{"type": "clock_sync", "browser_time": {}}}"#, (NOW - 5.0) * 1000.0);
    // when
    let fake = run_fake(&h, Some(vec!["https://example.ktalk.ru".into()]), &[&frame]).await;
    // then
    assert!(fake.sink.closes.lock().unwrap().is_empty());
    assert!((h.bridge.clock_offset() - 5.0).abs() < 1e-6, "{}", h.bridge.clock_offset());
}

#[tokio::test]
async fn given_duplicate_origins_when_bridge_handles_client_then_closed_with_1008() {
    // given
    let h = Harness::new();
    let origins = vec!["https://example.ktalk.ru".to_owned(), "https://example.ktalk.ru".to_owned()];
    // when
    let fake = run_fake(&h, Some(origins), &[]).await;
    // then
    assert_eq!(fake.sink.closes.lock().unwrap().iter().map(|c| c.0).collect::<Vec<_>>(), [1008]);
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    assert!(h.bridge.clients_in(Role::Subscriber).is_empty());
    assert!(h.logged("[KTalk Bridge] Rejected WS connection: duplicate Origin headers"), "{:?}", h.logs());
}

type ClientStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn real_bridge() -> (Arc<Bridge>, String) {
    let config = BridgeConfig { host: "127.0.0.1".into(), port: 0, debug_file: None, ..BridgeConfig::default() };
    let (log, _lines) = collecting_log();
    let bridge =
        Bridge::with_clock(config, SwitchablePolicy::enabled(), Arc::new(recog_bridge::hooks::SystemClock), log)
            .unwrap();
    let addrs = bridge.start().unwrap();
    (bridge, format!("ws://{}", addrs[0]))
}

async fn connect_with_origin(url: &str, origin: &str) -> ClientStream {
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert("Origin", origin.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}

#[test]
fn given_real_bridge_and_disallowed_origin_when_connected_then_closed_with_1008() {
    // given
    let (bridge, url) = real_bridge();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // when
    let close = runtime.block_on(async {
        let mut ws = connect_with_origin(&url, "https://evil.example").await;
        let close = loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap() {
                Some(Ok(Message::Close(frame))) => break frame,
                Some(Ok(_)) => {}
                other => panic!("{other:?}"),
            }
        };
        let _ = ws.close(None).await;
        close
    });
    // then
    assert_eq!(u16::from(close.unwrap().code), 1008);
    assert!(bridge.clients_in(Role::Extension).is_empty());
    bridge.stop();
}

#[test]
fn given_real_bridge_and_allowed_origin_when_connected_then_connection_stays_open() {
    // given
    let (bridge, url) = real_bridge();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // when
    let frame = runtime.block_on(async {
        let mut ws = connect_with_origin(&url, "https://example.ktalk.ru").await;
        let frame = loop {
            let message = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
            if let Message::Text(text) = message {
                let frame = pyjson::loads(text.as_str()).unwrap();
                if field(&frame, "type").as_str() == Some("copilot_status") {
                    break frame;
                }
            }
        };
        ws.close(None).await.unwrap();
        frame
    });
    // then
    assert_eq!(field(&frame, "status").as_str(), Some("agent_state"));
    bridge.stop();
}

// ---- get_sole_unmuted_remote_participant ----

#[test]
fn given_three_remotes_one_unmuted_when_sole_unmuted_asked_then_its_name() {
    // given
    let h = Harness::new();
    set_participants(
        &h,
        vec![
            participant("p1", "Alice", false, true),
            participant("p2", "Bob", false, false),
            participant("p3", "Carol", false, true),
        ],
    );
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole.as_deref(), Some("Bob"));
}

#[test]
fn given_all_remotes_muted_when_sole_unmuted_asked_then_none() {
    // given
    let h = Harness::new();
    set_participants(&h, vec![participant("p1", "Alice", false, true), participant("p2", "Bob", false, true)]);
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole, None);
}

#[test]
fn given_two_remotes_unmuted_when_sole_unmuted_asked_then_none() {
    // given
    let h = Harness::new();
    set_participants(
        &h,
        vec![
            participant("p1", "Alice", false, false),
            participant("p2", "Bob", false, false),
            participant("p3", "Carol", false, true),
        ],
    );
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole, None);
}

#[test]
fn given_only_local_unmuted_when_sole_unmuted_asked_then_none() {
    // given
    let h = Harness::new();
    set_participants(&h, vec![participant("p1", "Me", true, false), participant("p2", "Alice", false, true)]);
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole, None);
}

// ---- is_local_mic_muted: localMicMuted of participants_update ----

const ME: &str = r#"{"id": "me", "name": "Me", "isLocal": true, "micMuted": false}"#;

async fn participants_update(h: &Harness, participants: &str, extra: &str) {
    let frame = format!(r#"{{"type": "participants_update", "participants": {participants}{extra}}}"#);
    h.process(&frame, None).await.unwrap();
}

async fn local_mic_reported_without_local_participant(local_mic_muted: bool) {
    // given
    let h = Harness::new();
    let remote = r#"[{"id": "h", "name": "Host", "isLocal": false, "micMuted": false}]"#;
    participants_update(&h, remote, &format!(r#", "localMicMuted": {local_mic_muted}"#)).await;
    // when
    let muted = h.bridge.is_local_mic_muted();
    // then
    assert_eq!(muted, local_mic_muted);
}

#[tokio::test]
async fn given_local_mic_reported_muted_without_local_participant_when_asked_then_report_wins() {
    local_mic_reported_without_local_participant(true).await;
}

#[tokio::test]
async fn given_local_mic_reported_open_without_local_participant_when_asked_then_report_wins() {
    local_mic_reported_without_local_participant(false).await;
}

#[tokio::test]
async fn given_local_mic_reported_and_local_tile_disagrees_when_asked_then_report_wins() {
    // given
    let h = Harness::new();
    participants_update(&h, &format!("[{ME}]"), r#", "localMicMuted": true"#).await;
    // when
    let muted = h.bridge.is_local_mic_muted();
    // then
    assert!(muted);
}

async fn no_local_mic_report_after_one(extra: &str) {
    // given
    let h = Harness::new();
    participants_update(&h, &format!("[{ME}]"), r#", "localMicMuted": true"#).await;
    participants_update(&h, &format!("[{ME}]"), extra).await;
    // when
    let muted = h.bridge.is_local_mic_muted();
    // then
    assert!(!muted);
}

#[tokio::test]
async fn given_no_local_mic_field_after_a_report_when_asked_then_local_participant_decides() {
    no_local_mic_report_after_one("").await;
}

#[tokio::test]
async fn given_null_local_mic_after_a_report_when_asked_then_local_participant_decides() {
    no_local_mic_report_after_one(r#", "localMicMuted": null"#).await;
}

#[tokio::test]
async fn given_string_local_mic_after_a_report_when_asked_then_local_participant_decides() {
    no_local_mic_report_after_one(r#", "localMicMuted": "yes""#).await;
}

/// The owner is client 100, the refused tab 101.
async fn participants_from(sender: Option<u64>, accepted: bool) {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_owner(Some(ClientId(100)));
    let websocket = sender.map(loose_client);
    let frame = format!(r#"{{"type": "participants_update", "participants": [{ME}], "localMicMuted": true}}"#);
    // when
    h.process(&frame, websocket.as_ref()).await.unwrap();
    // then
    assert_eq!(h.bridge.is_local_mic_muted(), accepted);
    assert_eq!(h.bridge.get_participants().contains_key("me"), accepted);
}

#[tokio::test]
async fn given_meeting_owner_when_participants_arrive_from_owner_then_they_count() {
    participants_from(Some(100), true).await;
}

#[tokio::test]
async fn given_meeting_owner_when_participants_arrive_from_other_tab_then_ignored() {
    participants_from(Some(101), false).await;
}

#[tokio::test]
async fn given_meeting_owner_when_participants_arrive_without_socket_then_they_count() {
    participants_from(None, true).await;
}

async fn speaker_from(sender: u64, accepted: bool) {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_owner(Some(ClientId(100)));
    let timestamp = (NOW * 1000.0) as i64;
    let frame = format!(
        r#"{{"type": "speaker_update", "speaker": {{"id": "a", "name": "Alice", "isLocal": false}}, "timestamp": {timestamp}}}"#
    );
    // when
    h.process(&frame, Some(&loose_client(sender))).await.unwrap();
    // then
    let current = h.bridge.with_speakers(|s| s.current_speaker.clone());
    assert_eq!(current.as_deref() == Some("Alice"), accepted);
}

#[tokio::test]
async fn given_meeting_owner_when_speaker_arrives_from_owner_then_it_counts() {
    speaker_from(100, true).await;
}

#[tokio::test]
async fn given_meeting_owner_when_speaker_arrives_from_other_tab_then_ignored() {
    speaker_from(101, false).await;
}

#[test]
fn given_local_mic_reported_when_speaker_state_cleared_then_it_is_forgotten() {
    // given
    let h = Harness::new();
    let () = h.bridge.with_speakers(|s| s.local_mic_muted = Some(true));
    // when
    let () = h.bridge.with_speakers(|s| s.clear());
    // then
    assert!(!h.bridge.is_local_mic_muted());
}

// ---- PCM transport: pcm_chunk parsing ----

#[tokio::test]
async fn given_pcm_callback_when_well_formed_chunk_arrives_then_dispatched_with_offset_times() {
    // given
    let (h, _storage, live) = pcm_harness("ktalk");
    h.bridge.set_clock_offset(10.0);
    let pcm: Vec<u8> = 1234i16.to_le_bytes().repeat(8);
    let frame = pcm_frame_with("pid-42", "Bob", false, 7, 2_000.0, 5_300.0, 16000, &pcm);
    // when
    h.bridge.process_message(&frame, None).await.unwrap();
    // then
    let received = chunks(&live);
    assert_eq!(received.len(), 1);
    let chunk = &received[0];
    assert_eq!(field(chunk, "pid").as_str(), Some("pid-42"));
    assert_eq!(field(chunk, "speaker_name").as_str(), Some("Bob"));
    assert_eq!(field(chunk, "is_local"), &Value::Bool(false));
    assert_eq!(field(chunk, "seq"), &Value::Int(7));
    assert_eq!(field(chunk, "sample_rate"), &Value::Int(16000));
    assert_eq!(field(chunk, "pcm").as_str(), Some(hex(&pcm).as_str()));
    assert!((num(field(chunk, "chunk_start_ts")) - 12.0).abs() < 1e-9);
    assert!((num(field(chunk, "chunk_end_ts")) - 15.3).abs() < 1e-9);
}

#[tokio::test]
async fn given_no_pcm_callback_when_chunk_arrives_then_no_error() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(FakeStore::with_source("ktalk"));
    // when
    let result = h.bridge.process_message(&pcm_frame(), None).await;
    // then
    assert!(result.is_ok());
}

#[tokio::test]
async fn given_transport_diagnostics_when_pcm_transport_stats_arrive_then_kept_and_written_to_debug_file() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ktalk_debug.json");
    let debug_file = path.clone();
    let h = Harness::with(move |c| c.debug_file = Some(debug_file));
    let frame = r#"{"type": "pcm_transport_stats", "stats": {"active_count": 1, "total_rebinds": 2, "per_pid": {"pid-a": {"bound_via": "receiver"}}}}"#;
    // when
    h.process(frame, None).await.unwrap();
    // then
    let stats = h.bridge.pcm_transport_stats();
    assert_eq!(stats.get("active_count"), Some(&Value::Int(1)));
    assert_eq!(stats.get("total_rebinds"), Some(&Value::Int(2)));
    assert!(stats.contains_key("received_at"));
    let state = pyjson::loads(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let transport = field(&state, "pcm_transport");
    assert_eq!(field(transport, "active_count"), &Value::Int(1));
    assert_eq!(field(field(field(transport, "per_pid"), "pid-a"), "bound_via").as_str(), Some("receiver"));
}

#[tokio::test]
async fn given_pcm_callback_when_malformed_chunks_arrive_then_logged_and_not_dispatched() {
    // given
    let (h, _storage, live) = pcm_harness("ktalk");
    // when: invalid base64
    let mut bad_b64 = pcm_frame();
    bad_b64.insert("pcm_base64", Value::from("!!not-base64!!"));
    h.bridge.process_message(&bad_b64, None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    // when: a required field missing
    let mut missing_field = pcm_frame();
    missing_field.remove("seq");
    h.bridge.process_message(&missing_field, None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    // when: a seq that is not a number
    let mut bad_type = pcm_frame();
    bad_type.insert("seq", Value::from("not-a-number"));
    h.bridge.process_message(&bad_type, None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert_eq!(lines_with(&h, "pcm_chunk"), 3, "{:?}", h.logs());
}

#[tokio::test]
async fn given_pcm_callback_when_pid_is_numeric_and_speaker_null_then_coerced_to_strings() {
    // given
    let (h, _storage, live) = pcm_harness("ktalk");
    let mut int_pid = pcm_frame();
    int_pid.insert("pid", Value::Int(42));
    int_pid.insert("speaker_name", Value::Null);
    // when
    h.bridge.process_message(&int_pid, None).await.unwrap();
    // then
    let received = chunks(&live);
    assert_eq!(received.len(), 1);
    assert_eq!(field(&received[0], "pid").as_str(), Some("42"));
    assert_eq!(field(&received[0], "speaker_name").as_str(), Some(""));
    // when: an empty pid
    let mut empty_pid = pcm_frame();
    empty_pid.insert("pid", Value::from(""));
    h.bridge.process_message(&empty_pid, None).await.unwrap();
    // then
    assert_eq!(chunks(&live).len(), 1);
    assert!(h.logged("pcm_chunk"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_pcm_callback_when_payload_has_odd_length_then_skipped_with_warning() {
    // given
    let (h, _storage, live) = pcm_harness("ktalk");
    let odd =
        pcm_frame_with("participant-1", "Alice", false, 1, 1000.0, 4300.0, 16000, b"\x01\x02\x03\x04\x05\x06\x07");
    // when
    h.bridge.process_message(&odd, None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert!(h.logged("odd-length"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_pcm_callback_when_payload_is_empty_then_skipped_with_warning() {
    // given
    let (h, _storage, live) = pcm_harness("ktalk");
    let empty = pcm_frame_with("participant-1", "Alice", false, 1, 1000.0, 4300.0, 16000, b"");
    // when
    h.bridge.process_message(&empty, None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert!(h.logged("empty payload"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_current_meeting_when_chunk_of_another_meeting_arrives_then_dropped() {
    // given
    let (h, storage, live) = pcm_harness("ktalk");
    *storage.current_id.lock().unwrap() = Some("current-meeting-id".into());
    let mut cross = pcm_frame_with("pid-sibling", "Alice", false, 1, 1000.0, 4300.0, 16000, b"\x00\x01\x02\x03");
    cross.insert("meeting_id", Value::from("sibling-tab-meeting-id"));
    // when
    h.bridge.process_message(&cross, None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert!(h.logged("cross-meeting"), "{:?}", h.logs());
    // when: the current meeting's own chunk
    let mut own = pcm_frame_with("pid-self", "Alice", false, 1, 1000.0, 4300.0, 16000, b"\x00\x01\x02\x03");
    own.insert("meeting_id", Value::from("current-meeting-id"));
    h.bridge.process_message(&own, None).await.unwrap();
    // then
    let received = chunks(&live);
    assert_eq!(received.len(), 1);
    assert_eq!(field(&received[0], "pid").as_str(), Some("pid-self"));
}

#[tokio::test]
async fn given_current_meeting_when_chunk_without_meeting_id_arrives_then_passes() {
    // given
    let (h, storage, live) = pcm_harness("ktalk");
    *storage.current_id.lock().unwrap() = Some("current-meeting-id".into());
    let legacy = pcm_frame();
    assert!(!legacy.contains_key("meeting_id"));
    // when
    h.bridge.process_message(&legacy, None).await.unwrap();
    // then
    assert_eq!(chunks(&live).len(), 1);
}

#[tokio::test]
async fn given_pcm_callback_when_sample_rate_is_not_16000_then_skipped_with_warning() {
    // given
    let (h, _storage, live) = pcm_harness("ktalk");
    let frame = pcm_frame_with("participant-1", "Alice", false, 1, 1000.0, 4300.0, 48000, b"\x00\x01\x02\x03");
    // when
    h.bridge.process_message(&frame, None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert!(h.logged("sample_rate"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_pcm_callback_when_frame_claims_is_local_then_dispatched_as_remote() {
    // given
    let (h, _storage, live) = pcm_harness("ktalk");
    let forged = pcm_frame_with("participant-1", "Alice", true, 1, 1000.0, 4300.0, 16000, b"\x00\x01\x02\x03");
    // when
    h.bridge.process_message(&forged, None).await.unwrap();
    // then
    let received = chunks(&live);
    assert_eq!(received.len(), 1);
    assert_eq!(field(&received[0], "is_local"), &Value::Bool(false));
}

#[tokio::test]
async fn given_no_meeting_storage_when_valid_chunk_arrives_then_dropped() {
    // given
    let h = Harness::new();
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    // when
    h.bridge.process_message(&pcm_frame(), None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert!(h.logged("outside active ktalk meeting"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_storage_back_on_orphan_when_stale_chunk_arrives_then_dropped() {
    // given
    let (h, _storage, live) = pcm_harness("orphan");
    // when
    h.bridge.process_message(&pcm_frame(), None).await.unwrap();
    // then
    assert!(chunks(&live).is_empty());
    assert!(h.logged("outside active ktalk meeting"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_pcm_callback_that_raises_when_chunk_arrives_then_logged_and_bridge_keeps_working() {
    // given
    let (h, _storage, failing) = pcm_harness("ktalk");
    *failing.pcm_error.lock().unwrap() = Some("callback intentionally failed".into());
    // when
    let result = h.bridge.process_message(&pcm_frame(), None).await;
    // then
    assert!(result.is_ok());
    assert!(h.logged("callback raised"), "{:?}", h.logs());
    // when: a working callback, the next chunk
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    let frame = pcm_frame_with("participant-1", "Alice", false, 2, 1000.0, 4300.0, 16000, b"\x00\x01\x02\x03");
    h.bridge.process_message(&frame, None).await.unwrap();
    // then
    let received = chunks(&live);
    assert_eq!(received.len(), 1);
    assert_eq!(field(&received[0], "seq"), &Value::Int(2));
}

// ---- send_refine: tier-2 refined segment broadcast ----

#[tokio::test]
async fn given_connected_client_when_send_refine_then_one_transcript_refine_frame() {
    // given
    let h = Harness::new();
    h.attach();
    let (sink, _client) = h.add(Role::Extension);
    // when
    h.bridge.send_refine(&[refine(7, "первый вариант текста"), refine(8, "второй")], Some("2026-04-24_100101_meeting"));
    settle().await;
    // then
    let frames = sink.frames();
    assert_eq!(frames.len(), 1);
    let payload = &frames[0];
    assert_eq!(field(payload, "type").as_str(), Some("transcript_refine"));
    assert_eq!(
        field(payload, "updates"),
        &json(r#"[{"segment_id": 7, "text": "первый вариант текста"}, {"segment_id": 8, "text": "второй"}]"#)
    );
    assert_eq!(field(payload, "meeting_id").as_str(), Some("2026-04-24_100101_meeting"));
    assert!(field(payload, "refined_at").as_str().is_some_and(|t| t.contains('T')));
}

#[tokio::test]
async fn given_connected_client_when_send_refine_with_no_updates_then_nothing_sent() {
    // given
    let h = Harness::new();
    h.attach();
    let (sink, _client) = h.add(Role::Extension);
    // when
    h.bridge.send_refine(&[], None);
    settle().await;
    // then
    assert!(sink.frames().is_empty());
}

#[tokio::test]
async fn given_no_clients_when_send_refine_then_no_error() {
    // given
    let h = Harness::new();
    h.attach();
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    // when
    h.bridge.send_refine(&[refine(1, "x")], None);
    settle().await;
    // then: nothing to observe but the absence of a panic
}

#[tokio::test]
async fn given_connected_client_when_send_refine_then_updates_carry_only_segment_id_and_text() {
    // given: `RefineUpdate` has no room for extra keys such as `refined`, which
    // Python strips from its dicts; what reaches the wire is the same
    let h = Harness::new();
    h.attach();
    let (sink, _client) = h.add(Role::Extension);
    // when
    h.bridge.send_refine(&[refine(42, "уточнённый")], None);
    settle().await;
    // then
    let frames = sink.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "updates"), &json(r#"[{"segment_id": 42, "text": "уточнённый"}]"#));
    assert!(!frames[0].as_object().unwrap().contains_key("meeting_id"));
}

#[tokio::test]
async fn given_connected_client_when_send_refine_without_meeting_id_then_envelope_omits_it() {
    // given
    let h = Harness::new();
    h.attach();
    let (sink, _client) = h.add(Role::Extension);
    // when
    h.bridge.send_refine(&[refine(1, "x")], None);
    settle().await;
    // then
    let frames = sink.frames();
    assert_eq!(frames.len(), 1);
    assert!(!frames[0].as_object().unwrap().contains_key("meeting_id"));
}

#[tokio::test]
async fn given_only_a_subscriber_when_send_refine_then_subscriber_receives_it() {
    // given
    let h = Harness::new();
    h.attach();
    let (sink, _subscriber) = h.add(Role::Subscriber);
    // when
    h.bridge.send_refine(&[refine(3, "t")], None);
    settle().await;
    // then
    let frames = sink.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "type").as_str(), Some("transcript_refine"));
    assert_eq!(field(&frames[0], "updates"), &json(r#"[{"segment_id": 3, "text": "t"}]"#));
}

// ---- subscriber_hello ----

#[tokio::test]
async fn given_client_when_subscriber_hello_then_moved_to_subscribers_and_purged_on_close() {
    // given
    let h = Harness::new();
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "subscriber_hello"}"#);
    fake.idle().await;
    // then
    assert_eq!(h.bridge.clients_in(Role::Subscriber).len(), 1);
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    fake.close();
    task.await.unwrap();
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    assert!(h.bridge.clients_in(Role::Subscriber).is_empty());
}

#[tokio::test]
async fn given_extension_present_when_subscriber_disconnects_then_meeting_continues() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    h.bridge.set_meeting_storage(storage.clone());
    let (_extension_sink, extension) = h.add(Role::Extension);
    // when
    run_fake(&h, None, &[r#"{"type": "subscriber_hello"}"#]).await;
    // then
    assert!(storage.ops().is_empty());
    assert_eq!(h.bridge.clients_in(Role::Extension), [extension.id]);
}

#[tokio::test]
async fn given_subscriber_registered_when_extension_disconnects_then_meeting_left_as_disconnected() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    h.bridge.set_meeting_storage(storage.clone());
    let (_subscriber_sink, _subscriber) = h.add(Role::Subscriber);
    // when
    run_fake(&h, None, &[]).await;
    // then
    assert_eq!(storage.ops(), ["on_ktalk_leave"]);
    assert_eq!(field(&storage.calls()[0], "reason").as_str(), Some("disconnected"));
}

#[tokio::test]
async fn given_clock_offset_when_client_connects_then_offset_kept() {
    // given
    let h = Harness::new();
    h.bridge.set_clock_offset(1.234);
    // when
    run_fake(&h, None, &[]).await;
    // then
    assert!((h.bridge.clock_offset() - 1.234).abs() < f64::EPSILON);
}

#[tokio::test]
async fn given_extension_left_during_race_when_subscriber_hello_arrives_then_meeting_left_as_disconnected() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    *storage.current_id.lock().unwrap() = Some("m".into());
    h.bridge.set_meeting_storage(storage.clone());
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "subscriber_hello"}"#);
    fake.idle().await;
    // then
    assert_eq!(storage.ops(), ["on_ktalk_leave"]);
    assert_eq!(field(&storage.calls()[0], "reason").as_str(), Some("disconnected"));
    assert_eq!(h.bridge.clients_in(Role::Subscriber).len(), 1);
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    fake.close();
    task.await.unwrap();
}

#[tokio::test]
async fn given_no_active_meeting_when_subscriber_hello_arrives_then_nothing_ended() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    *storage.current_path.lock().unwrap() = None;
    h.bridge.set_meeting_storage(storage.clone());
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "subscriber_hello"}"#);
    fake.idle().await;
    // then
    assert_eq!(h.bridge.clients_in(Role::Subscriber).len(), 1);
    assert!(storage.ops().is_empty());
    fake.close();
    task.await.unwrap();
}

// ---- copilot_channel_hello ----

#[tokio::test]
async fn given_client_when_copilot_channel_hello_then_moved_to_channels_and_purged_on_close() {
    // given
    let h = Harness::new();
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "copilot_channel_hello"}"#);
    fake.idle().await;
    // then
    assert_eq!(h.bridge.clients_in(Role::Channel).len(), 1);
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    assert!(h.bridge.clients_in(Role::Subscriber).is_empty());
    fake.close();
    task.await.unwrap();
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    assert!(h.bridge.clients_in(Role::Subscriber).is_empty());
    assert!(h.bridge.clients_in(Role::Channel).is_empty());
}

#[tokio::test]
async fn given_extension_left_during_race_when_copilot_channel_hello_arrives_then_meeting_left_as_disconnected() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    *storage.current_id.lock().unwrap() = Some("m".into());
    h.bridge.set_meeting_storage(storage.clone());
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "copilot_channel_hello"}"#);
    fake.idle().await;
    // then
    assert_eq!(storage.ops(), ["on_ktalk_leave"]);
    assert_eq!(field(&storage.calls()[0], "reason").as_str(), Some("disconnected"));
    assert_eq!(h.bridge.clients_in(Role::Channel).len(), 1);
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    fake.close();
    task.await.unwrap();
}

#[tokio::test]
async fn given_extension_present_when_copilot_channel_disconnects_then_meeting_continues() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    h.bridge.set_meeting_storage(storage.clone());
    let (_extension_sink, extension) = h.add(Role::Extension);
    // when
    run_fake(&h, None, &[r#"{"type": "copilot_channel_hello"}"#]).await;
    // then
    assert!(storage.ops().is_empty());
    assert_eq!(h.bridge.clients_in(Role::Extension), [extension.id]);
}

#[tokio::test]
async fn given_pending_requests_when_last_channel_disconnects_then_channel_disconnected_and_live_requests_dropped() {
    // given
    let h = Harness::new();
    let (extension_sink, _extension) = h.add(Role::Extension);
    let (fake, task) = h.connect(None).await;
    fake.send_text(r#"{"type": "copilot_channel_hello"}"#);
    fake.idle().await;
    assert_eq!(h.bridge.clients_in(Role::Channel).len(), 1);
    extension_sink.clear();
    h.bridge.set_pending_request("req-1", "meeting-A");
    h.bridge.set_pending_request("req-2", "meeting-A");
    h.bridge.set_pending_request("req-3-stale", "stale:meeting-OLD");
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert_eq!(h.bridge.pending_requests(), [("req-3-stale".to_owned(), "stale:meeting-OLD".to_owned())]);
    let frames = extension_sink.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "type").as_str(), Some("copilot_status"));
    assert_eq!(field(&frames[0], "request_id").as_str(), Some(""));
    assert_eq!(field(&frames[0], "status").as_str(), Some("channel_disconnected"));
}

#[tokio::test]
async fn given_extension_and_no_channel_when_first_channel_hello_then_extension_gets_channel_connected() {
    // given
    let h = Harness::new();
    let (extension_sink, _extension) = h.add(Role::Extension);
    assert!(h.bridge.clients_in(Role::Channel).is_empty());
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "copilot_channel_hello"}"#);
    fake.idle().await;
    // then
    assert_eq!(h.bridge.clients_in(Role::Channel).len(), 1);
    let frames = extension_sink.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "type").as_str(), Some("copilot_status"));
    assert_eq!(field(&frames[0], "request_id").as_str(), Some(""));
    assert_eq!(field(&frames[0], "status").as_str(), Some("channel_connected"));
    fake.close();
    task.await.unwrap();
}

#[tokio::test]
async fn given_channel_already_present_when_new_client_connects_then_it_gets_channel_connected_primer() {
    // given
    let h = Harness::new();
    let (_channel_sink, _channel) = h.add(Role::Channel);
    // when
    let (fake, task) = h.connect(None).await;
    // then
    let primers: Vec<Value> = fake
        .sink
        .frames()
        .into_iter()
        .filter(|f| f.as_object().and_then(|m| m.get("status")).and_then(Value::as_str) == Some("channel_connected"))
        .collect();
    assert_eq!(primers.len(), 1);
    assert_eq!(field(&primers[0], "type").as_str(), Some("copilot_status"));
    assert_eq!(field(&primers[0], "request_id").as_str(), Some(""));
    fake.close();
    task.await.unwrap();
}
