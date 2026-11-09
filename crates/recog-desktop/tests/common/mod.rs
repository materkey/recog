//! What the ported and differential tests share: a scripted process table (a JSON
//! world file both implementations read), a fake Node inspector that opens its port
//! only when the application is signalled and closes it on `inspector.close()`, fake
//! KTalk application bundles, and the Python driver (tests/parity/desktop_driver.py).
//! Nothing here touches a real process: the world is a file, SIGUSR1 a line in
//! another file, and every port is a free ephemeral one.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use recog_core::pyjson::{self, Map, Value};
use recog_desktop::host::{Host, Platform, ProcessTable};
use recog_desktop::py::{self, PyErr};

pub fn repo_root() -> PathBuf {
    recog_desktop::default_repo_root()
}

/// Ports the tests must never take: the services' and the real inspector's.
const RESERVED: [u16; 6] = [8765, 8766, 8767, 9080, 9229, 44258];

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

pub fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.as_object().and_then(|m| m.get(key)).unwrap_or(&Value::Null)
}

pub fn text(value: &Value, key: &str) -> Option<String> {
    field(value, key).as_str().map(str::to_owned)
}

fn write_atomic(path: &Path, contents: &str) {
    let temporary = path.with_extension(format!("tmp{}", uuid_like()));
    std::fs::write(&temporary, contents).unwrap();
    std::fs::rename(&temporary, path).unwrap();
}

fn uuid_like() -> String {
    format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}

/// The scripted processes (`world.json`) and the signals sent to them
/// (`signals.jsonl`), in a directory of their own.
#[derive(Clone)]
pub struct World {
    pub path: PathBuf,
    pub signals: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl World {
    pub fn new(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let world = Self { path: dir.join("world.json"), signals: dir.join("signals.jsonl"), lock: Arc::default() };
        world.set(&obj(r#"{"processes": []}"#));
        world
    }

    pub fn get(&self) -> Value {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        obj(&std::fs::read_to_string(&self.path).unwrap())
    }

    pub fn set(&self, value: &Value) {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        write_atomic(&self.path, &py::dumps(value));
    }

    /// Changes the world in place.
    pub fn update(&self, change: impl FnOnce(&mut Vec<Value>)) {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut world = obj(&std::fs::read_to_string(&self.path).unwrap());
        let processes = world.as_object_mut().unwrap().get_mut("processes").unwrap();
        let Value::Array(list) = processes else { panic!("processes") };
        change(list);
        write_atomic(&self.path, &py::dumps(&world));
    }

    pub fn set_listening(&self, pid: i64, port: u16, listening: bool) {
        self.update(|list| {
            for process in list.iter_mut() {
                if field(process, "pid") != &Value::Int(i128::from(pid)) {
                    continue;
                }
                let map = process.as_object_mut().unwrap();
                let listeners = map.setdefault("listeners", Value::Array(Vec::new()));
                let Value::Array(items) = listeners else { panic!("listeners") };
                items.retain(|item| item.as_array_port() != Some(port));
                if listening {
                    items.push(Value::Array(vec![Value::from("127.0.0.1"), Value::Int(i128::from(port))]));
                }
            }
        });
    }

    pub fn signalled(&self) -> Vec<i64> {
        let Ok(text) = std::fs::read_to_string(&self.signals) else { return Vec::new() };
        text.lines()
            .map(|line| match field(&obj(line), "pid") {
                Value::Int(pid) => i64::try_from(*pid).unwrap(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// The environment that points the Python driver and the worker child here.
    pub fn env(&self, inspector_port: u16, app_ready_port: u16, home: &Path) -> Vec<(String, String)> {
        vec![
            ("DESKTOP_WORLD".into(), self.path.to_string_lossy().into_owned()),
            ("DESKTOP_SIGNALS".into(), self.signals.to_string_lossy().into_owned()),
            ("DESKTOP_INSPECTOR_PORT".into(), inspector_port.to_string()),
            ("DESKTOP_APP_READY_PORT".into(), app_ready_port.to_string()),
            ("DESKTOP_REPO".into(), repo_root().to_string_lossy().into_owned()),
            ("HOME".into(), home.to_string_lossy().into_owned()),
        ]
    }
}

trait PortOf {
    fn as_array_port(&self) -> Option<u16>;
}

impl PortOf for Value {
    fn as_array_port(&self) -> Option<u16> {
        match self {
            Value::Array(pair) => match pair.get(1) {
                Some(Value::Int(port)) => u16::try_from(*port).ok(),
                _ => None,
            },
            _ => None,
        }
    }
}

/// psutil over the world file, as the Python driver fakes it.
pub struct FakeTable {
    pub world: World,
    /// Start times returned before the world's, one per `create_time()` call (a
    /// process replaced between two checks).
    pub started_script: Mutex<std::collections::VecDeque<f64>>,
}

impl FakeTable {
    pub fn new(world: &World) -> Self {
        Self { world: world.clone(), started_script: Mutex::default() }
    }
}

impl FakeTable {
    fn entry(&self, pid: i64) -> Option<Value> {
        let world = self.world.get();
        let Value::Array(list) = field(&world, "processes") else { return None };
        list.iter().find(|p| field(p, "pid") == &Value::Int(i128::from(pid))).cloned()
    }

    fn fail(kind: &str, pid: i64) -> PyErr {
        match kind {
            "NoSuchProcess" => PyErr::no_such_process(pid),
            "AccessDenied" => PyErr::access_denied(pid),
            "ZombieProcess" => PyErr::zombie(pid),
            other => panic!("unknown error {other}"),
        }
    }

    fn field(&self, pid: i64, name: &str) -> Result<Value, PyErr> {
        let entry = self.entry(pid).ok_or_else(|| PyErr::no_such_process(pid))?;
        if let Some(kind) = text(field(&entry, "errors"), name) {
            return Err(Self::fail(&kind, pid));
        }
        Ok(entry)
    }
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().map(Value::py_str).collect(),
        _ => Vec::new(),
    }
}

impl ProcessTable for FakeTable {
    fn pids(&self) -> Vec<i64> {
        let world = self.world.get();
        let Value::Array(list) = field(&world, "processes") else { return Vec::new() };
        let mut pids: Vec<i64> = list
            .iter()
            .map(|p| match field(p, "pid") {
                Value::Int(pid) => i64::try_from(*pid).unwrap(),
                other => panic!("{other:?}"),
            })
            .collect();
        pids.sort_unstable();
        pids
    }

    fn open(&self, pid: i64) -> Result<(), PyErr> {
        self.field(pid, "open").map(|_| ())
    }

    fn name(&self, pid: i64) -> Result<String, PyErr> {
        Ok(field(&self.field(pid, "name")?, "name").py_str())
    }

    fn uid_real(&self, pid: i64) -> Result<u32, PyErr> {
        match field(&self.field(pid, "uid")?, "uid") {
            Value::Int(uid) => Ok(u32::try_from(*uid).unwrap()),
            _ => Ok(nix::unistd::getuid().as_raw()),
        }
    }

    fn create_time(&self, pid: i64) -> Result<f64, PyErr> {
        let entry = self.field(pid, "started")?;
        if let Some(started) = self.started_script.lock().unwrap_or_else(PoisonError::into_inner).pop_front() {
            return Ok(started);
        }
        py::to_float(field(&entry, "started"))
    }

    fn exe(&self, pid: i64) -> Result<String, PyErr> {
        Ok(field(&self.field(pid, "exe")?, "exe").py_str())
    }

    fn cmdline(&self, pid: i64) -> Result<Vec<String>, PyErr> {
        Ok(strings(field(&self.field(pid, "cmdline")?, "cmdline")))
    }

    fn environ(&self, pid: i64) -> Result<Vec<(String, String)>, PyErr> {
        let entry = self.field(pid, "environ")?;
        Ok(field(&entry, "environ")
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.py_str())).collect())
            .unwrap_or_default())
    }

    fn tcp_listeners(&self, pid: i64) -> Result<Vec<(String, u16)>, PyErr> {
        let entry = self.field(pid, "listeners")?;
        let Value::Array(items) = field(&entry, "listeners") else { return Ok(Vec::new()) };
        Ok(items
            .iter()
            .map(|pair| match pair {
                Value::Array(pair) => match &pair[1] {
                    Value::Int(port) => (pair[0].py_str(), u16::try_from(*port).unwrap()),
                    other => panic!("port {other:?}"),
                },
                other => panic!("listener {other:?}"),
            })
            .collect())
    }
}

/// A host over the world: its table, its signal file, the given ports and HOME.
pub fn host(world: &World, inspector_port: u16, app_ready_port: u16, home: &Path) -> Host {
    host_with(Arc::new(FakeTable::new(world)), world, inspector_port, app_ready_port, home)
}

/// [`host`] with a table of the test's own.
pub fn host_with(table: Arc<FakeTable>, world: &World, inspector_port: u16, app_ready_port: u16, home: &Path) -> Host {
    let mut host = Host::with_table(table);
    host.inspector_port = inspector_port;
    host.app_ready_port = app_ready_port;
    host.home = home.to_path_buf();
    let signals = world.signals.clone();
    host.signal_usr1 = Arc::new(move |pid| {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&signals).unwrap();
        writeln!(file, "{{\"pid\": {pid}, \"signal\": 30}}").unwrap();
        Ok(())
    });
    host
}

/// The host a worker child builds from the environment the test gave it.
pub fn host_from_env() -> Host {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
    let world = World {
        path: PathBuf::from(var("DESKTOP_WORLD")),
        signals: PathBuf::from(var("DESKTOP_SIGNALS")),
        lock: Arc::default(),
    };
    host(
        &world,
        var("DESKTOP_INSPECTOR_PORT").parse().unwrap(),
        var("DESKTOP_APP_READY_PORT").parse().unwrap(),
        Path::new(&var("HOME")),
    )
}

/// How the fake inspector answers.
#[derive(Clone, Debug)]
pub struct Behaviour {
    /// `one` (default), `two`, `big`, `status500`, `foreign`.
    pub list: String,
    /// `match` (default), `mismatch`, `throw`, `error` (an error reply), `garbage`
    /// (not JSON), `slow_open` (the WebSocket opens after 2 s).
    pub identity: String,
    /// `ok` (default), `invalid`, `throw`, `state`.
    pub bootstrap: String,
    /// `close` (default) or `ignore`: whether CLOSE closes the port.
    pub close: String,
    /// Listening before any signal (a foreign inspector, a busy port).
    pub listen_from_start: bool,
    /// A `Debugger.paused` event before each reply.
    pub events: bool,
    /// Whether SIGUSR1 opens the inspector at all.
    pub open_on_signal: bool,
    /// SIGUSR1 ends the process instead.
    pub exit_on_signal: bool,
    /// The listener goes away while the target list is requested.
    pub drop_on_list: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            list: "one".into(),
            identity: "match".into(),
            bootstrap: "ok".into(),
            close: "close".into(),
            listen_from_start: false,
            events: true,
            open_on_signal: true,
            exit_on_signal: false,
            drop_on_list: false,
        }
    }
}

struct InspectorState {
    serving: Option<tokio::task::JoinHandle<()>>,
    pid: Option<i64>,
    seen_signals: usize,
}

/// Node's inspector in the scripted application: closed until SIGUSR1, then
/// `GET /json/list` and one WebSocket session per evaluation, closed again by the
/// CLOSE expression (after Node's 100 ms).
pub struct Inspector {
    pub port: u16,
    pub world: World,
    pub behaviour: Behaviour,
    /// What arrived: `{"kind": "list"}` and `{"kind": ..., "expression": ...}`.
    pub log: Mutex<Vec<Value>>,
    state: Mutex<InspectorState>,
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    sessions: std::sync::atomic::AtomicUsize,
}

fn reply(value: &Value) -> String {
    py::dumps(&py::obj([("id", Value::Int(1)), ("result", py::obj([("result", py::obj([("value", value.clone())]))]))]))
}

impl Inspector {
    pub async fn start(port: u16, world: &World, behaviour: Behaviour, pid: i64) -> Arc<Self> {
        let inspector = Arc::new(Self {
            port,
            world: world.clone(),
            behaviour: behaviour.clone(),
            log: Mutex::new(Vec::new()),
            state: Mutex::new(InspectorState { serving: None, pid: Some(pid), seen_signals: 0 }),
            watcher: Mutex::new(None),
            sessions: std::sync::atomic::AtomicUsize::new(0),
        });
        if behaviour.listen_from_start {
            inspector.listen().await;
        }
        let watching = Arc::clone(&inspector);
        let watcher = tokio::spawn(async move {
            loop {
                let signals = watching.world.signalled();
                let new = {
                    let mut state = watching.state.lock().unwrap_or_else(PoisonError::into_inner);
                    let new = signals.len() > state.seen_signals;
                    state.seen_signals = signals.len();
                    new
                };
                if new && watching.behaviour.exit_on_signal {
                    let pid = watching.pid();
                    watching.world.update(|list| list.retain(|p| field(p, "pid") != &Value::Int(i128::from(pid))));
                } else if new && watching.behaviour.open_on_signal {
                    watching.open().await;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        *inspector.watcher.lock().unwrap() = Some(watcher);
        inspector
    }

    fn pid(&self) -> i64 {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).pid.unwrap_or(0)
    }

    async fn listen(self: &Arc<Self>) {
        if self.state.lock().unwrap_or_else(PoisonError::into_inner).serving.is_some() {
            return;
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", self.port)).await.unwrap();
        let this = Arc::clone(self);
        let list_this = Arc::clone(self);
        let app = axum::Router::new()
            .route(
                "/json/list",
                axum::routing::get(move || {
                    let this = Arc::clone(&list_this);
                    async move { this.list() }
                }),
            )
            .route(
                "/session",
                axum::routing::get(move |upgrade: WebSocketUpgrade| {
                    let this = Arc::clone(&this);
                    async move {
                        let first = this.sessions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                        if this.behaviour.identity == "slow_open" && first {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                        upgrade.on_upgrade(move |socket| async move { this.session(socket).await })
                    }
                }),
            );
        let serving = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        self.state.lock().unwrap_or_else(PoisonError::into_inner).serving = Some(serving);
    }

    /// SIGUSR1: the port opens and the process lists it.
    pub async fn open(self: &Arc<Self>) {
        self.listen().await;
        self.world.set_listening(self.pid(), self.port, true);
    }

    /// `inspector.close()`: the port closes and the process no longer lists it.
    pub fn close(&self) {
        if let Some(serving) = self.state.lock().unwrap_or_else(PoisonError::into_inner).serving.take() {
            serving.abort();
        }
        self.world.set_listening(self.pid(), self.port, false);
    }

    pub fn stop(&self) {
        if let Some(watcher) = self.watcher.lock().unwrap().take() {
            watcher.abort();
        }
        if let Some(serving) = self.state.lock().unwrap_or_else(PoisonError::into_inner).serving.take() {
            serving.abort();
        }
    }

    pub fn entries(&self) -> Vec<Value> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn record(&self, entry: Value) {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push(entry);
    }

    fn list(&self) -> axum::response::Response {
        self.record(py::obj([("kind", Value::from("list"))]));
        if self.behaviour.drop_on_list {
            self.world.set_listening(self.pid(), self.port, false);
        }
        let url = |host: &str| format!("ws://{host}:{}/session", self.port);
        let one = py::obj([("webSocketDebuggerUrl", Value::from(url("127.0.0.1")))]);
        let body = match self.behaviour.list.as_str() {
            "two" => Value::Array(vec![one.clone(), one]),
            "foreign" => Value::Array(vec![py::obj([("webSocketDebuggerUrl", Value::from(url("localhost")))])]),
            "big" => Value::Array(vec![py::obj([
                ("webSocketDebuggerUrl", Value::from(url("127.0.0.1"))),
                ("padding", Value::from("x".repeat(70_000))),
            ])]),
            _ => Value::Array(vec![one]),
        };
        if self.behaviour.list == "status500" {
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "error").into_response();
        }
        ([(axum::http::header::CONTENT_TYPE, "application/json; charset=UTF-8")], py::dumps(&body)).into_response()
    }

    async fn session(self: Arc<Self>, mut socket: WebSocket) {
        while let Some(Ok(message)) = socket.recv().await {
            let Message::Text(frame) = message else { continue };
            let frame = obj(frame.as_str());
            let expression = field(field(&frame, "params"), "expression").py_str();
            let kind = if expression.contains("return {pid: process.pid}") {
                "identity"
            } else if expression.contains("\nattachDesktop(") {
                "bootstrap"
            } else if expression.contains("inspector.close(); }, 100); return true;") {
                "close"
            } else {
                "other"
            };
            self.record(py::obj([
                ("kind", Value::from(kind)),
                ("id", field(&frame, "id").clone()),
                ("method", field(&frame, "method").clone()),
                ("params", field(&frame, "params").clone()),
            ]));
            if self.behaviour.events {
                let event = py::obj([("method", Value::from("Debugger.paused")), ("params", py::obj([]))]);
                let _ = socket.send(Message::Text(py::dumps(&event).into())).await;
            }
            let thrown = || {
                py::dumps(&py::obj([
                    ("id", Value::Int(1)),
                    ("result", py::obj([("exceptionDetails", py::obj([("description", Value::from("private"))]))])),
                ]))
            };
            let answer = match (kind, self.behaviour.identity.as_str(), self.behaviour.bootstrap.as_str()) {
                ("identity", "mismatch", _) => reply(&py::obj([("pid", Value::Int(i128::from(self.pid() + 1)))])),
                ("identity", "throw", _) => thrown(),
                ("identity", "error", _) => py::dumps(&py::obj([
                    ("id", Value::Int(1)),
                    ("error", py::obj([("message", Value::from("private"))])),
                ])),
                ("identity", "garbage", _) => "not json".to_owned(),
                ("identity", _, _) => reply(&py::obj([("pid", Value::Int(i128::from(self.pid())))])),
                ("bootstrap", _, "invalid") => reply(&py::obj([("other", Value::Int(1))])),
                ("bootstrap", _, "throw") => thrown(),
                ("bootstrap", _, "state") => {
                    reply(&py::obj([("app_version", Value::from("9.9.9")), ("state", Value::from("forged"))]))
                }
                ("bootstrap", _, _) => {
                    let tail = expression.rsplit_once("\nattachDesktop(").unwrap().1;
                    let args = obj(&format!("[{}]", tail.strip_suffix(')').unwrap()));
                    let Value::Array(args) = args else { panic!("arguments") };
                    reply(&py::obj([("app_version", Value::from("9.9.9")), ("bundle", args[1].clone())]))
                }
                ("close", _, _) => reply(&Value::Bool(true)),
                _ => reply(&Value::Null),
            };
            let _ = socket.send(Message::Text(answer.into())).await;
            if kind == "close" && self.behaviour.close == "close" {
                let this = Arc::clone(&self);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    this.close();
                });
            }
        }
    }
}

/// The three files `prepare_bundle` requires, and one more.
pub fn dist(dir: &Path) -> PathBuf {
    let source = dir.join("dist");
    std::fs::create_dir_all(source.join("src/audio-worklet")).unwrap();
    for name in ["manifest.json", "injected.js", "src/audio-worklet/downsampler.js", "src/a-b.js"] {
        std::fs::write(source.join(name), name).unwrap();
    }
    std::fs::create_dir_all(source.join("src/a")).unwrap();
    std::fs::write(source.join("src/a/b.js"), "nested").unwrap();
    source
}

/// An `app.asar` whose `package.json` is `metadata`.
pub fn asar(metadata: &str) -> Vec<u8> {
    let body = metadata.as_bytes();
    let tree = format!(r#"{{"files": {{"package.json": {{"size": {}, "offset": "0"}}}}}}"#, body.len());
    let tree = tree.as_bytes();
    let mut out = Vec::new();
    for word in [4u32, 8 + u32::try_from(tree.len()).unwrap(), 4, u32::try_from(tree.len()).unwrap()] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(tree);
    out.extend_from_slice(body);
    out
}

/// One fuse wire: the sentinel, version, length and fuses (fuse 3 the inspector).
pub fn fuse_wire(enabled: bool, version: u8, length: u8) -> Vec<u8> {
    let mut payload = b"00000000".to_vec();
    payload[3] = if enabled { 0x31 } else { 0x30 };
    let mut out = recog_desktop::processes::FUSE_SENTINEL.to_vec();
    out.extend_from_slice(&[version, length]);
    out.extend_from_slice(&payload[..usize::from(length).min(8)]);
    out
}

/// A fake KTalk installation for this platform: on macOS a bundle with its
/// `Info.plist`, `app.asar` and Electron framework, on Linux an executable beside
/// `resources/app.asar`.
pub struct FakeApp {
    /// What the user selects.
    pub app: PathBuf,
    /// The main process's executable.
    pub executable: PathBuf,
    /// The binary whose fuses are read.
    pub fuses: PathBuf,
}

pub fn plist(identifier: &str, executable: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>{identifier}</string>
	<key>CFBundleExecutable</key>
	<string>{executable}</string>
</dict>
</plist>
"#
    )
}

/// The fake application of the platform the tests run on.
pub fn fake_app(dir: &Path, name: &str) -> FakeApp {
    fake_app_for(dir, name, &Platform::current())
}

/// The fake application laid out as `platform` lays it out, on any system: a test
/// of a host set to another platform needs that platform's application.
pub fn fake_app_for(dir: &Path, name: &str, platform: &Platform) -> FakeApp {
    use std::os::unix::fs::PermissionsExt as _;
    let package = r#"{"name": "ktalk", "main": "build/main.js"}"#;
    if *platform == Platform::Darwin {
        let app = dir.join(format!("{name}.app"));
        let contents = app.join("Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::create_dir_all(contents.join("Resources")).unwrap();
        let framework = contents.join("Frameworks/Electron Framework.framework");
        std::fs::create_dir_all(&framework).unwrap();
        std::fs::write(contents.join("Info.plist"), plist("kontur.talk", name)).unwrap();
        let executable = contents.join("MacOS").join(name);
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(contents.join("Resources/app.asar"), asar(package)).unwrap();
        let fuses = framework.join("Electron Framework");
        std::fs::write(&fuses, [b"head".to_vec(), fuse_wire(true, 1, 8)].concat()).unwrap();
        FakeApp {
            app: std::fs::canonicalize(&app).unwrap(),
            executable: std::fs::canonicalize(&executable).unwrap(),
            fuses,
        }
    } else {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join("resources")).unwrap();
        let executable = root.join(name);
        std::fs::write(&executable, [b"head".to_vec(), fuse_wire(true, 1, 8)].concat()).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.join("resources/app.asar"), asar(package)).unwrap();
        let executable = std::fs::canonicalize(&executable).unwrap();
        FakeApp { app: executable.clone(), fuses: executable.clone(), executable }
    }
}

/// When every scripted process started: after any binary a test writes (so its
/// fuses attest to the running image), and the same on both sides of a comparison.
pub const STARTED: f64 = 4_000_000_000.5;

/// The scripted main process of a fake application, ready.
pub fn ktalk_process(pid: i64, app: &FakeApp, app_ready_port: u16) -> Value {
    let name = app.executable.file_name().unwrap().to_string_lossy().into_owned();
    let started = STARTED;
    let mut map = Map::new();
    map.insert("pid", Value::Int(i128::from(pid)));
    map.insert("name", Value::from(name.as_str()));
    map.insert("uid", Value::from("self"));
    map.insert("started", Value::Float(started));
    map.insert("exe", Value::from(app.executable.to_string_lossy().as_ref()));
    map.insert("cmdline", Value::Array(vec![Value::from(app.executable.to_string_lossy().as_ref())]));
    map.insert("environ", py::obj([("LANG", Value::from("C"))]));
    map.insert(
        "listeners",
        Value::Array(vec![Value::Array(vec![Value::from("127.0.0.1"), Value::Int(i128::from(app_ready_port))])]),
    );
    Value::Object(map)
}

pub fn world_of(processes: Vec<Value>) -> Value {
    py::obj([("processes", Value::Array(processes))])
}

/// `{"value": ...}` or `{"error": {"kind", "message"}}`, as the driver reports a call.
pub fn outcome<T>(result: Result<T, PyErr>, render: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(value) => py::obj([("value", render(value))]),
        Err(e) => py::obj([("error", py::obj([("kind", Value::from(e.kind)), ("message", Value::from(e.message))]))]),
    }
}

/// Replaces the clock readings and fresh tokens of a value, which differ between two
/// runs; other numbers stay.
pub fn mask(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let masked = match k.as_str() {
                        "updated_at" | "opened_at" | "token" if !v.is_null() => Value::from("<masked>"),
                        _ => mask(v),
                    };
                    (k.clone(), masked)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(mask).collect()),
        other => other.clone(),
    }
}

/// `value` with every occurrence of each `(from, to)` in its strings replaced.
pub fn localize(value: &Value, replacements: &[(String, String)]) -> Value {
    match value {
        Value::Str(s) => {
            let mut s = s.clone();
            for (from, to) in replacements {
                s = s.replace(from.as_str(), to);
            }
            Value::Str(s)
        }
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), localize(v, replacements))).collect()),
        Value::Array(items) => Value::Array(items.iter().map(|v| localize(v, replacements)).collect()),
        other => other.clone(),
    }
}

/// The files of a directory tree: relative path to (mode, contents).
pub fn tree(root: &Path) -> HashMap<String, (u32, Vec<u8>)> {
    use std::os::unix::fs::PermissionsExt as _;
    fn visit(root: &Path, dir: &Path, out: &mut HashMap<String, (u32, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let mode = meta.permissions().mode() & 0o777;
            if meta.is_dir() {
                out.insert(format!("{relative}/"), (mode, Vec::new()));
                visit(root, &path, out);
            } else {
                out.insert(relative, (mode, std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = HashMap::new();
    visit(root, root, &mut out);
    out
}
