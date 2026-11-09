//! Helpers shared by the ported bridge tests (the Python tests' fixtures).
#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use recog_bridge::testing::{FakeConnection, ManualClock, RecordingSink, SwitchablePolicy, collecting_log};
use recog_bridge::{Bridge, BridgeConfig, Client, Role};
use recog_core::meeting_storage::{MeetingStorage, SessionMode, StorageOptions};
use recog_core::pyjson::{self, Map, Value};
use tokio::task::JoinHandle;

/// 2026-07-04T10:00:00Z.
pub const NOW: f64 = 1_783_159_200.0;

/// A bridge with a manual clock, a collecting log and a switchable policy.
pub struct Harness {
    pub bridge: Arc<Bridge>,
    pub clock: Arc<ManualClock>,
    pub policy: Arc<SwitchablePolicy>,
    pub lines: Arc<Mutex<Vec<String>>>,
}

impl Harness {
    pub fn new() -> Self {
        Self::with(|_| {})
    }

    /// A bridge whose configuration `edit` changes first (`ktalk_only`, the send
    /// timeout, the debug file).
    pub fn with(edit: impl FnOnce(&mut BridgeConfig)) -> Self {
        let mut config =
            BridgeConfig { host: "127.0.0.1".into(), port: 0, debug_file: None, ..BridgeConfig::default() };
        edit(&mut config);
        let clock = ManualClock::at(NOW);
        let policy = SwitchablePolicy::enabled();
        let (log, lines) = collecting_log();
        let bridge = Bridge::with_clock(config, policy.clone(), clock.clone(), log).expect("policy");
        Self { bridge, clock, policy, lines }
    }

    /// Lets sends from "live threads" reach clients (`bridge._loop = loop`).
    pub fn attach(&self) {
        self.bridge.attach_runtime(tokio::runtime::Handle::current());
    }

    /// The log lines so far.
    pub fn logs(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    pub fn logged(&self, needle: &str) -> bool {
        self.logs().iter().any(|l| l.contains(needle))
    }

    /// A connection handled by the bridge (`_handle_client(ControllableFakeWebSocket())`),
    /// already through its primers.
    pub async fn connect(&self, origin: Option<&str>) -> (FakeConnection, JoinHandle<()>) {
        let origins = origin.map(|o| vec![o.to_owned()]);
        let (fake, connection) = FakeConnection::new("('127.0.0.1', 12345)", origins);
        let bridge = self.bridge.clone();
        let task = tokio::spawn(async move { bridge.handle_client(connection).await });
        fake.idle().await;
        (fake, task)
    }

    /// A client put straight into a set (`bridge._connected_clients.add(ws)`).
    pub fn add(&self, role: Role) -> (Arc<RecordingSink>, Arc<Client>) {
        let sink = RecordingSink::new();
        let client = self.bridge.add_client(role, sink.clone(), "('127.0.0.1', 12345)");
        (sink, client)
    }

    /// `await bridge._process_message(frame, websocket=client)`.
    pub async fn process(&self, frame: &str, client: Option<&Arc<Client>>) -> Result<(), String> {
        self.bridge.process_message(&map(frame), client).await
    }
}

/// A JSON object from text.
pub fn map(text: &str) -> Map {
    match pyjson::loads(text).expect("json") {
        Value::Object(map) => map,
        other => panic!("not an object: {other:?}"),
    }
}

/// A JSON value from text.
pub fn json(text: &str) -> Value {
    pyjson::loads(text).expect("json")
}

/// `frame[key]`.
pub fn field<'a>(frame: &'a Value, key: &str) -> &'a Value {
    frame.as_object().and_then(|m| m.get(key)).unwrap_or_else(|| panic!("no {key} in {frame:?}"))
}

/// Lets every ready task run.
pub async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// Polls `condition` for up to two seconds.
pub async fn wait_for(condition: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    condition()
}

/// A real storage in `dir` with a session started for conferences (current meeting:
/// an orphan), on `clock`.
pub fn real_storage(dir: &std::path::Path, clock: Arc<ManualClock>) -> Arc<MeetingStorage> {
    let mut options = StorageOptions::new(&dir.join("home"));
    options.clock = clock;
    options.external_index_cache_dir = dir.join("cache");
    let storage = MeetingStorage::new(&dir.join("meetings"), options).expect("storage");
    storage.start_session(&SessionMode { ktalk: true, ..SessionMode::default() }).expect("session");
    Arc::new(storage)
}
