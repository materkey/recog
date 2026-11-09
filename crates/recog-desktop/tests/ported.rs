//! The cases of tests/test_desktop_integration.py and tests/test_cli_desktop.py,
//! against the Rust modules. Where the Python tests monkeypatch a function, these
//! script what that function reads instead (the process table, the inspector, the
//! files), so the real code runs end to end.
#![forbid(unsafe_code)]

mod common;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use recog_core::pyjson::Value;
use recog_desktop::host::{Host, Platform, ScriptedClock};
use recog_desktop::inspector::Failure;
use recog_desktop::processes::{self, Target};
use recog_desktop::py;
use recog_desktop::service::{
    self, BoxFuture, Context, DesktopIntegration, Launcher, Log, Notifier, NotifyOutcome, Spawner, WorkerProcess,
};
use recog_desktop::{cli, storage, worker};

struct Fixture {
    dir: tempfile::TempDir,
    world: World,
    home: PathBuf,
    inspector_port: u16,
    ready_port: u16,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let world = World::new(&dir.path().join("world"));
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self { world, home, inspector_port: free_port(), ready_port: free_port(), dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn root(&self) -> PathBuf {
        self.path().join("data/desktop")
    }

    fn host(&self) -> Host {
        host(&self.world, self.inspector_port, self.ready_port, &self.home)
    }

    fn app(&self) -> FakeApp {
        fake_app(self.path(), "FakeTalk")
    }

    /// A ready main process of `app` as pid 42, its target, the configuration with a
    /// prepared snapshot.
    fn installed(&self, app: &FakeApp) -> Target {
        let process = ktalk_process(42, app, self.ready_port);
        self.world.set(&world_of(vec![process.clone()]));
        let (digest, _) = storage::prepare_bundle(&dist(self.path()), &self.root()).unwrap();
        let config = py::obj([
            ("enabled", Value::Bool(true)),
            ("app", Value::from(app.app.to_string_lossy().as_ref())),
            ("bundle", Value::from(digest)),
        ]);
        storage::write_json(&self.root().join("config.json"), &config).unwrap();
        Target::from_value(&py::obj([
            ("pid", Value::Int(42)),
            ("started", field(&process, "started").clone()),
            ("executable", field(&process, "exe").clone()),
        ]))
        .unwrap()
    }

    async fn inspector(&self, behaviour: Behaviour) -> Arc<Inspector> {
        Inspector::start(self.inspector_port, &self.world, behaviour, 42).await
    }
}

fn read(path: &Path) -> Value {
    storage::read(path).unwrap()
}

fn s(text: &str) -> Value {
    Value::from(text)
}

/// What a fake worker answers for (root, target, recover, call number).
type Answer = Box<dyn Fn(&Path, Option<&Target>, bool, usize) -> Value + Send + Sync>;

/// `launch_worker` replaced: records the calls and answers what `answer` says.
struct FakeLauncher {
    calls: Mutex<Vec<(Option<Target>, bool)>>,
    answer: Answer,
}

impl FakeLauncher {
    fn new(answer: impl Fn(&Path, Option<&Target>, bool, usize) -> Value + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { calls: Mutex::default(), answer: Box::new(answer) })
    }

    fn calls(&self) -> Vec<(Option<Target>, bool)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Launcher for FakeLauncher {
    fn launch<'a>(
        &'a self,
        root: &'a Path,
        target: Option<Target>,
        recover: bool,
    ) -> BoxFuture<'a, Result<Value, py::PyErr>> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push((target.clone(), recover));
            calls.len()
        };
        let value = (self.answer)(root, target.as_ref(), recover, n);
        Box::pin(async move { Ok(value) })
    }
}

/// Notifications recorded instead of shown.
#[derive(Default)]
struct FakeNotifier {
    calls: Mutex<Vec<Vec<String>>>,
    notify_send: bool,
    /// `exited`, `timeout`, `missing`.
    mode: String,
}

impl Notifier for FakeNotifier {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, std::io::Result<NotifyOutcome>> {
        self.calls.lock().unwrap().push(args);
        let outcome = match self.mode.as_str() {
            "missing" => Err(std::io::Error::from_raw_os_error(2)),
            "timeout" => Ok(NotifyOutcome::Terminated),
            _ => Ok(NotifyOutcome::Exited),
        };
        Box::pin(async move { outcome })
    }

    fn which(&self, _name: &str) -> Option<PathBuf> {
        self.notify_send.then(|| PathBuf::from("/usr/bin/notify-send"))
    }
}

fn context(host: Host, launcher: Arc<dyn Launcher>) -> (Context, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let context = Context {
        host,
        launcher,
        notifier: Arc::new(FakeNotifier::default()),
        log: Arc::new(Log::to(move |line| sink.lock().unwrap().push(line.to_owned()))),
        poll: Duration::ZERO,
    };
    (context, lines)
}

fn attached_answer(root: &Path, target: Option<&Target>, _recover: bool, _n: usize) -> Value {
    let result = py::obj([
        ("state", s("attached")),
        ("reason", Value::Null),
        ("target", target.map_or(Value::Null, Target::to_value)),
    ]);
    storage::write_json(&root.join("status.json"), &result).unwrap();
    result
}

fn no_launch(_: &Path, _: Option<&Target>, _: bool, _: usize) -> Value {
    panic!("launched a worker")
}

// --- storage ---------------------------------------------------------------------

#[test]
fn built_extension_when_snapshotted_then_immutable_and_tampering_detected() {
    // given
    let fixture = Fixture::new();
    let source = dist(fixture.path());
    let root = fixture.path().join("data");
    let (digest, snapshot) = storage::prepare_bundle(&source, &root).unwrap();
    // when
    std::fs::write(source.join("injected.js"), "new build").unwrap();
    // then
    assert_eq!(std::fs::read_to_string(snapshot.join("injected.js")).unwrap(), "injected.js");
    let config = py::obj([("bundle", s(&digest))]);
    assert_eq!(storage::validate_bundle(&root, &config).unwrap(), snapshot);
    std::fs::write(snapshot.join("injected.js"), "tampered").unwrap();
    assert!(storage::validate_bundle(&root, &config).unwrap_err().message.contains("integrity"));
}

#[test]
fn symbolic_link_when_in_source_or_snapshot_then_refused() {
    // given
    let fixture = Fixture::new();
    let source = dist(fixture.path());
    std::os::unix::fs::symlink(fixture.path().join("outside.js"), source.join("escape.js")).unwrap();
    // when / then
    assert!(storage::prepare_bundle(&source, &fixture.path().join("data")).unwrap_err().message.contains("symbolic"));
    std::fs::remove_file(source.join("escape.js")).unwrap();
    let (digest, snapshot) = storage::prepare_bundle(&source, &fixture.path().join("data")).unwrap();
    std::os::unix::fs::symlink(fixture.path().join("outside.js"), snapshot.join("escape.js")).unwrap();
    let error = storage::validate_bundle(&fixture.path().join("data"), &py::obj([("bundle", s(&digest))])).unwrap_err();
    assert!(error.message.contains("symbolic"));
}

#[test]
fn unresolved_warning_when_statuses_written_then_kept_until_closure_confirmed() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    let attached = obj(r#"{"state": "attached", "inspector_closed": true}"#);
    let error = obj(r#"{"state": "error", "reason": "worker_failed", "inspector_closed": null}"#);
    // when / then
    assert_eq!(storage::write_status(&root, &attached).unwrap(), attached);
    assert_eq!(storage::write_status(&root, &error).unwrap(), error);
    storage::write_json(&root.join("operation.json"), &obj(r#"{"phase": "opened"}"#)).unwrap();
    let critical = obj(r#"{"state": "inspector_unclosed", "inspector_closed": false}"#);
    storage::write_json(&root.join("status.json"), &critical).unwrap();
    assert_eq!(storage::write_status(&root, &error).unwrap(), critical);
    let closed = obj(r#"{"state": "waiting", "reason": null, "inspector_closed": true}"#);
    assert_eq!(storage::write_status(&root, &closed).unwrap(), closed);
    assert_eq!(read(&root.join("status.json")), closed);
    storage::write_json(&root.join("status.json"), &critical).unwrap();
    storage::write_json(&root.join("operation.json"), &obj(r#"{"phase": "closed"}"#)).unwrap();
    assert_eq!(storage::write_status(&root, &error).unwrap(), error);
    assert_eq!(read(&root.join("status.json")), error);
}

// --- processes -------------------------------------------------------------------

fn linux_host(fixture: &Fixture) -> Host {
    let mut host = fixture.host();
    host.platform = Platform::Linux;
    host
}

/// A Linux-style application: an executable beside `resources/app.asar`.
fn linux_app(dir: &Path, name: &str, package: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir.join("resources")).unwrap();
    let app = dir.join(name);
    std::fs::write(&app, "").unwrap();
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("resources/app.asar"), asar(package)).unwrap();
    std::fs::canonicalize(app).unwrap()
}

const KTALK_PACKAGE: &str = r#"{"name": "ktalk", "main": "build/main.js"}"#;

#[test]
fn reused_pid_or_helper_when_identified_then_rejected() {
    // given
    let fixture = Fixture::new();
    let app = linux_app(&fixture.path().join("bin"), "ktalk", KTALK_PACKAGE);
    let process = |args: &str, started: f64| {
        obj(&format!(
            r#"{{"pid": 42, "name": "ktalk", "started": {started:?}, "exe": {}, "cmdline": {args}}}"#,
            py::dumps(&s(&app.to_string_lossy()))
        ))
    };
    fixture.world.set(&world_of(vec![process(r#"["ktalk"]"#, 12.0)]));
    let host = linux_host(&fixture);
    let target = processes::find_targets(&host, &app).unwrap().remove(0);
    // when: a helper, then the pid reused by a later process
    fixture.world.set(&world_of(vec![process(r#"["ktalk", "--type=renderer"]"#, 12.0)]));
    // then
    assert!(processes::find_targets(&host, &app).unwrap().is_empty());
    assert!(!processes::same_process(&host, &target, Some(&app)).unwrap());
    fixture.world.set(&world_of(vec![process(r#"["ktalk"]"#, 13.0)]));
    assert!(!processes::same_process(&host, &target, Some(&app)).unwrap());
}

fn plain_process(listeners: &str, environ: &str, cmdline: &str) -> Value {
    obj(&format!(
        r#"{{"pid": 42, "name": "ktalk", "started": 12.0, "exe": "/ktalk", "cmdline": {cmdline}, "environ": {environ}, "listeners": {listeners}}}"#
    ))
}

fn target_42() -> Target {
    Target { pid: 42, started: 12.0, executable: "/ktalk".into() }
}

#[test]
fn native_listener_or_debugger_when_readiness_checked_then_only_a_plain_initialized_app_is_ready() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host();
    let ready = format!(r#"[["127.0.0.1", {}]]"#, fixture.ready_port);
    let cases = [
        (r#"[["127.0.0.1", 40000]]"#.to_owned(), "{}", r#"["ktalk"]"#, false),
        (ready.clone(), "{}", r#"["ktalk"]"#, true),
        (ready.clone(), "{}", r#"["ktalk", "--inspect"]"#, false),
    ];
    for (listeners, environ, cmdline, expected) in cases {
        // when
        fixture.world.set(&world_of(vec![plain_process(&listeners, environ, cmdline)]));
        // then
        assert_eq!(processes::ready(&host, &target_42()).unwrap(), expected, "{listeners} {cmdline}");
    }
}

#[test]
fn node_mode_debug_environment_or_bad_options_when_readiness_checked_then_not_ready() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host();
    let ready = format!(r#"[["127.0.0.1", {}]]"#, fixture.ready_port);
    let environments = [
        r#"{"ELECTRON_RUN_AS_NODE": ""}"#,
        r#"{"ELECTRON_RUN_AS_NODE": "0"}"#,
        r#"{"NODE_OPTIONS": "--inspect"}"#,
        r#"{"NODE_OPTIONS": "--inspect=127.0.0.1:9333"}"#,
        r#"{"NODE_OPTIONS": "--inspect=0.0.0.0:9229"}"#,
        r#"{"NODE_OPTIONS": "--inspect-brk=127.0.0.1:9333"}"#,
        r#"{"NODE_OPTIONS": "--debug=127.0.0.1:9333"}"#,
        r#"{"NODE_OPTIONS": "--remote-debugging-port=9333"}"#,
        r#"{"NODE_OPTIONS": "--max-old-space-size=512 --inspect-port=9333"}"#,
        r#"{"NODE_OPTIONS": "\"unterminated"}"#,
        r#"{"NODE_OPTIONS": "--disable-sigusr1"}"#,
        r#"{"NODE_OPTIONS": "--disable_sigusr1"}"#,
        r#"{"NODE_OPTIONS": "--experimental-config-file=/tmp/node.json"}"#,
        r#"{"NODE_OPTIONS": "--experimental_default_config_file=/tmp/node.json"}"#,
    ];
    for environ in environments {
        // when
        fixture.world.set(&world_of(vec![plain_process(&ready, environ, r#"["ktalk"]"#)]));
        // then
        assert!(!processes::ready(&host, &target_42()).unwrap(), "{environ}");
    }
    fixture.world.set(&world_of(vec![plain_process(
        &ready,
        r#"{"NODE_OPTIONS": "--max-old-space-size=512"}"#,
        r#"["ktalk"]"#,
    )]));
    assert!(processes::ready(&host, &target_42()).unwrap());
}

#[test]
fn inspector_listener_when_checked_then_owned_only_on_loopback() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host();
    let port = fixture.inspector_port;
    let cases = [
        ("[]".to_owned(), false),
        (format!(r#"[["0.0.0.0", {port}]]"#), false),
        (format!(r#"[["127.0.0.1", {port}]]"#), true),
    ];
    for (listeners, expected) in cases {
        // when
        fixture.world.set(&world_of(vec![plain_process(&listeners, "{}", r#"["ktalk"]"#)]));
        // then
        assert_eq!(processes::owns_inspector(&host, &target_42()).unwrap(), expected, "{listeners}");
    }
}

#[test]
fn reused_pid_when_listeners_read_then_its_connections_never_read() {
    // given: the pid now belongs to another process, whose connections may not be read
    let fixture = Fixture::new();
    let host = fixture.host();
    let mut other = plain_process(r#"[["127.0.0.1", 9229]]"#, "{}", r#"["ktalk"]"#);
    other.as_object_mut().unwrap().insert("started", Value::Float(99.0));
    other.as_object_mut().unwrap().insert("errors", obj(r#"{"listeners": "AccessDenied"}"#));
    fixture.world.set(&world_of(vec![other]));
    // when / then
    assert!(processes::listeners(&host, &target_42()).unwrap().is_empty());
    fixture.world.set(&world_of(vec![plain_process(r#"[["127.0.0.1", 9229]]"#, "{}", r#"["ktalk"]"#)]));
    assert_eq!(processes::listeners(&host, &target_42()).unwrap(), [("127.0.0.1".to_owned(), 9229)]);
}

#[test]
fn packaged_identity_when_read_then_requires_ktalk_package_and_main() {
    // given
    let fixture = Fixture::new();
    let host = linux_host(&fixture);
    let executable = fixture.path().join("ktalk");
    std::fs::write(&executable, "").unwrap();
    std::fs::create_dir_all(fixture.path().join("resources")).unwrap();
    let archive = fixture.path().join("resources/app.asar");
    let cases = [
        (asar(KTALK_PACKAGE), true),
        (asar(r#"{"name": "impostor", "main": "build/main.js"}"#), false),
        (asar(r#"{"name": "ktalk", "main": "other.js"}"#), false),
        (b"truncated".to_vec(), false),
    ];
    for (contents, expected) in cases {
        // when
        std::fs::write(&archive, &contents).unwrap();
        // then
        assert_eq!(processes::is_ktalk_executable(&host, &executable).unwrap(), expected);
    }
}

#[test]
fn selected_application_when_identity_or_permissions_invalid_then_rejected() {
    use std::os::unix::fs::PermissionsExt as _;
    let cases = [
        (Platform::Linux, true),
        (Platform::Linux, false),
        (Platform::Darwin, true),
        (Platform::Darwin, false),
        (Platform::Other("win32".into()), false),
    ];
    for (platform, valid) in cases {
        // given
        let fixture = Fixture::new();
        let mut host = fixture.host();
        host.platform = platform.clone();
        let app = fixture.path().join("KTalk");
        if platform == Platform::Darwin {
            std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
            let identifier = if valid { "kontur.talk" } else { "other" };
            std::fs::write(app.join("Contents/Info.plist"), plist(identifier, "ktalk")).unwrap();
            std::fs::write(app.join("Contents/MacOS/ktalk"), "").unwrap();
        } else {
            std::fs::write(&app, "").unwrap();
            std::fs::set_permissions(&app, std::fs::Permissions::from_mode(if valid { 0o700 } else { 0o600 })).unwrap();
        }
        // when
        let result = processes::application_path(&host, &app.to_string_lossy());
        // then
        if valid {
            assert_eq!(result.unwrap(), std::fs::canonicalize(&app).unwrap(), "{platform:?}");
        } else {
            assert!(result.unwrap_err().is_value(), "{platform:?}");
        }
    }
}

#[test]
fn appimage_process_when_resolved_then_requires_selected_source() {
    let cases = [
        ("selected", "ktalk", false, true),
        ("other", "ktalk", false, false),
        ("", "ktalk", true, true),
        ("selected", "helper", true, false),
    ];
    for (image, executable_name, fuse_match, expected) in cases {
        // given
        let fixture = Fixture::new();
        let mut host = linux_host(&fixture);
        let app = fixture.path().join("KTalk.AppImage");
        std::fs::write(&app, "").unwrap();
        let app = std::fs::canonicalize(app).unwrap();
        let mount = fixture.path().join("mount");
        let executable = linux_app(&mount, executable_name, KTALK_PACKAGE);
        let appimage = match image {
            "selected" => app.to_string_lossy().into_owned(),
            other => other.to_owned(),
        };
        let process = obj(&format!(
            r#"{{"pid": 42, "name": "ktalk", "started": 12.0, "exe": {}, "cmdline": ["ktalk"], "environ": {{"APPIMAGE": {}}}}}"#,
            py::dumps(&s(&executable.to_string_lossy())),
            py::dumps(&s(&appimage))
        ));
        let mut processes_list = vec![process.clone()];
        if fuse_match {
            // The FUSE runtime that mounted it: the selected file, connection 17.
            let runtime = obj(&format!(
                r#"{{"pid": 901, "name": "KTalk.AppImage", "exe": {}}}"#,
                py::dumps(&s(&app.to_string_lossy()))
            ));
            processes_list.push(runtime);
            std::fs::create_dir_all(fixture.path().join("proc/901/fdinfo")).unwrap();
            std::fs::write(fixture.path().join("proc/901/fdinfo/7"), "pos:\t0\nfuse_connection:\t17\n").unwrap();
            host.proc_root = fixture.path().join("proc");
            host.device = Arc::new(|_| Ok(processes::makedev(0, 17)));
        }
        fixture.world.set(&world_of(processes_list));
        // when
        let matched = processes::matches(&host, 42, &app).unwrap();
        // then
        assert_eq!(matched, expected, "{image} {executable_name} {fuse_match}");
    }
}

#[test]
fn fuse_appimage_when_matched_then_requires_runtime_owner_source_and_connection() {
    let cases = [
        ("owner", true, true, 0, true),
        ("other", true, true, 0, false),
        ("owner", false, true, 0, false),
        ("owner", true, false, 0, false),
        ("owner", true, true, 8, false),
    ];
    for (owner, backing_source, connection_matches, major, expected) in cases {
        // given
        let fixture = Fixture::new();
        let mut host = fixture.host();
        let app = fixture.path().join("KTalk.AppImage");
        let other = fixture.path().join("Other.AppImage");
        std::fs::write(&app, "").unwrap();
        std::fs::write(&other, "").unwrap();
        let app = std::fs::canonicalize(app).unwrap();
        let uid = if owner == "owner" { s("self") } else { Value::Int(i128::from(host.uid) + 1) };
        let exe = if backing_source { &app } else { &other };
        let mut runtime =
            obj(&format!(r#"{{"pid": 901, "name": "KTalk", "exe": {}}}"#, py::dumps(&s(&exe.to_string_lossy()))));
        runtime.as_object_mut().unwrap().insert("uid", uid);
        fixture.world.set(&world_of(vec![runtime]));
        std::fs::create_dir_all(fixture.path().join("proc/901/fdinfo")).unwrap();
        let observed = if connection_matches { 17 } else { 18 };
        std::fs::write(fixture.path().join("proc/901/fdinfo/7"), format!("pos:\t0\nfuse_connection:\t{observed}\n"))
            .unwrap();
        host.proc_root = fixture.path().join("proc");
        host.device = Arc::new(move |_| Ok(processes::makedev(major, 17)));
        // when
        let matched = processes::fuse_source_matches(&host, Path::new("/mounted/ktalk"), &app).unwrap();
        // then
        assert_eq!(matched, expected, "{owner} {backing_source} {connection_matches} {major}");
    }
}

#[test]
fn bare_fuse_mount_name_when_matched_then_cannot_identify_selected_appimage() {
    // given: no runtime process at all
    let fixture = Fixture::new();
    let mut host = fixture.host();
    host.device = Arc::new(|_| Ok(processes::makedev(0, 17)));
    // when
    let matched =
        processes::fuse_source_matches(&host, Path::new("/mount_KTalk.AppImage/ktalk"), Path::new("/KTalk.AppImage"));
    // then
    assert!(!matched.unwrap());
}

#[test]
fn live_loopback_or_wildcard_listener_when_port_checked_then_unavailable() {
    for address in ["127.0.0.1", "0.0.0.0"] {
        // given
        let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        socket.set_reuse_address(true).unwrap();
        socket.bind(&std::net::SocketAddr::from((address.parse::<std::net::Ipv4Addr>().unwrap(), 0)).into()).unwrap();
        socket.listen(1).unwrap();
        let mut host = Fixture::new().host();
        host.inspector_port = socket.local_addr().unwrap().as_socket().unwrap().port();
        // when / then
        assert!(!processes::port_available(&host), "{address}");
    }
}

#[test]
fn free_ephemeral_port_when_checked_then_available() {
    // given
    let mut host = Fixture::new().host();
    host.inspector_port = free_port();
    // when / then
    assert!(processes::port_available(&host));
}

fn wire_cases() -> Vec<(Vec<u8>, bool)> {
    let enabled = fuse_wire(true, 1, 8);
    vec![
        (enabled.clone(), true),
        (fuse_wire(false, 1, 8), false),
        (fuse_wire(true, 2, 8), false),
        (fuse_wire(true, 1, 3), false),
        ([enabled.clone(), b"gap".to_vec(), enabled.clone()].concat(), true),
        ([enabled.clone(), b"gap".to_vec(), fuse_wire(false, 1, 8)].concat(), false),
        (enabled.repeat(3), false),
        (b"ordinary binary".to_vec(), false),
    ]
}

#[test]
fn fuse_wire_when_inspected_then_only_known_enabled_architectures_accepted() {
    // given
    let fixture = Fixture::new();
    let binary = fixture.path().join("Electron Framework");
    for (wire, expected) in wire_cases() {
        // when
        std::fs::write(&binary, &wire).unwrap();
        // then
        assert_eq!(processes::inspect_fuses(&binary).unwrap(), expected);
    }
}

#[test]
fn fuse_wire_across_a_chunk_boundary_or_truncated_when_inspected_then_found_or_refused() {
    // given
    let fixture = Fixture::new();
    let binary = fixture.path().join("Electron Framework");
    // when / then
    std::fs::write(&binary, [b"padding".repeat(9360), fuse_wire(true, 1, 8)].concat()).unwrap();
    assert!(processes::inspect_fuses(&binary).unwrap());
    std::fs::write(
        &binary,
        [b"prefix".to_vec(), recog_desktop::processes::FUSE_SENTINEL.to_vec(), b"\x01\x08000".to_vec()].concat(),
    )
    .unwrap();
    assert!(!processes::inspect_fuses(&binary).unwrap());
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

#[test]
fn binary_identity_when_changed_then_support_cache_not_reused() {
    // given
    let fixture = Fixture::new();
    let host = linux_host(&fixture);
    let binary = fixture.path().join("ktalk");
    std::fs::write(&binary, fuse_wire(true, 1, 8)).unwrap();
    let target = Target { pid: 42, started: now() + 100.0, executable: binary.to_string_lossy().into_owned() };
    assert!(processes::inspector_supported(&host, &target).unwrap());
    // when
    std::fs::write(&binary, fuse_wire(false, 1, 8)).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&binary)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(later).set_accessed(later))
        .unwrap();
    // then
    assert!(!processes::inspector_supported(&host, &target).unwrap());
    std::fs::remove_file(&binary).unwrap();
    assert!(!processes::inspector_supported(&host, &target).unwrap());
}

#[test]
fn darwin_application_when_support_checked_then_the_electron_framework_fuses_decide() {
    // given: the executable allows nothing, the framework beside it allows the inspector
    let fixture = Fixture::new();
    let mut host = fixture.host();
    host.platform = Platform::Darwin;
    let contents = fixture.path().join("KTalk.app/Contents");
    let framework = contents.join("Frameworks/Electron Framework.framework");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::create_dir_all(&framework).unwrap();
    let executable = contents.join("MacOS/ktalk");
    std::fs::write(&executable, fuse_wire(false, 1, 8)).unwrap();
    std::fs::write(framework.join("Electron Framework"), fuse_wire(true, 1, 8)).unwrap();
    let target = Target { pid: 42, started: now() + 100.0, executable: executable.to_string_lossy().into_owned() };
    // when / then
    assert!(processes::inspector_supported(&host, &target).unwrap());
    std::fs::remove_file(framework.join("Electron Framework")).unwrap();
    assert!(!processes::inspector_supported(&host, &target).unwrap());
}

#[test]
fn binary_changed_after_process_start_when_support_checked_then_unverified() {
    // given
    let fixture = Fixture::new();
    let host = linux_host(&fixture);
    let binary = fixture.path().join("ktalk");
    std::fs::write(&binary, fuse_wire(true, 1, 8)).unwrap();
    let target = Target { pid: 42, started: now() - 100.0, executable: binary.to_string_lossy().into_owned() };
    // when / then
    assert!(!processes::inspector_supported(&host, &target).unwrap());
}

// --- status ----------------------------------------------------------------------

#[test]
fn disabled_or_stale_heartbeat_when_status_read_then_disabled_or_server_offline() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    // when / then
    assert_eq!(text(&service::status_at(&root, 100.0), "state").unwrap(), "disabled");
    storage::write_json(&root.join("config.json"), &obj(r#"{"enabled": true}"#)).unwrap();
    storage::write_json(&root.join("status.json"), &obj(r#"{"state": "attached", "app_version": "1"}"#)).unwrap();
    assert_eq!(text(&service::status_at(&root, 100.0), "state").unwrap(), "server_offline");
    storage::write_json(&root.join("heartbeat.json"), &obj(r#"{"running": true, "updated_at": 99}"#)).unwrap();
    assert_eq!(text(&service::status_at(&root, 100.0), "state").unwrap(), "attached");
    storage::write_json(&root.join("config.json"), &obj(r#"{"enabled": false}"#)).unwrap();
    assert_eq!(text(&service::status_at(&root, 100.0), "state").unwrap(), "disabled");
}

#[test]
fn disabled_configuration_when_status_read_then_unclosed_inspector_warning_kept() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    storage::write_json(&root.join("config.json"), &obj(r#"{"enabled": false}"#)).unwrap();
    storage::write_json(
        &root.join("status.json"),
        &obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "inspector_closed": false, "target": {"pid": 42}}"#),
    )
    .unwrap();
    // when
    let result = service::status_at(&root, 100.0);
    // then
    assert_eq!(field(&result, "enabled"), &Value::Bool(false));
    assert_eq!(text(&result, "state").unwrap(), "inspector_unclosed");
    assert_eq!(text(&result, "reason").unwrap(), "cleanup_failed");
    assert_eq!(field(&result, "inspector_closed"), &Value::Bool(false));
    assert_eq!(field(&result, "target"), &obj(r#"{"pid": 42}"#));
}

#[test]
fn malformed_configuration_when_status_read_then_unclosed_inspector_not_masked() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.json"), "{ invalid").unwrap();
    storage::write_json(
        &root.join("status.json"),
        &obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "inspector_closed": false}"#),
    )
    .unwrap();
    // when
    let result = service::status_at(&root, 100.0);
    // then
    assert_eq!(text(&result, "state").unwrap(), "inspector_unclosed");
    assert_eq!(text(&result, "reason").unwrap(), "cleanup_failed");
    assert_eq!(field(&result, "inspector_closed"), &Value::Bool(false));
}

// --- observer --------------------------------------------------------------------

#[tokio::test]
async fn unconfigured_installation_when_observed_then_inert() {
    // given
    let fixture = Fixture::new();
    let (context, _) = context(fixture.host(), FakeLauncher::new(no_launch));
    let observer = DesktopIntegration::new(fixture.root(), context);
    // when
    observer.tick().await.unwrap();
    // then
    assert!(!fixture.root().exists());
    assert!(!observer.holds_lock().await);
}

#[tokio::test]
async fn busy_port_then_success_when_observed_across_a_restart_then_retried_once_and_success_cached() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let target = fixture.installed(&app);
    let busy = std::net::TcpListener::bind(("127.0.0.1", fixture.inspector_port)).unwrap();
    let launcher = FakeLauncher::new(attached_answer);
    let (first_context, _) = context(fixture.host(), launcher.clone());
    let first = DesktopIntegration::new(fixture.root(), first_context);
    // when
    first.tick().await.unwrap();
    // then
    assert_eq!(text(&service::status_at(&fixture.root(), now()), "reason").unwrap(), "inspector_port_busy");
    drop(busy);
    first.tick().await.unwrap();
    assert_eq!(launcher.calls(), [(Some(target.clone()), false)]);
    first.release_lock().await;
    let (second_context, _) = context(fixture.host(), launcher.clone());
    let second = DesktopIntegration::new(fixture.root(), second_context);
    second.tick().await.unwrap();
    assert_eq!(launcher.calls(), [(Some(target), false)]);
}

#[tokio::test]
async fn exited_target_when_observed_then_old_operation_closed_and_new_process_attached() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let new = fixture.installed(&app);
    let old = Target { pid: 41, ..new.clone() };
    let mut old_process = ktalk_process(41, &app, fixture.ready_port);
    old_process.as_object_mut().unwrap().insert("started", Value::Float(new.started));
    let new_process = ktalk_process(42, &app, fixture.ready_port);
    let mut new_process = new_process;
    new_process.as_object_mut().unwrap().insert("started", Value::Float(new.started));
    fixture.world.set(&world_of(vec![old_process.clone(), new_process.clone()]));
    storage::write_json(
        &fixture.root().join("operation.json"),
        &py::obj([("target", old.to_value()), ("phase", s("opened")), ("token", s("old"))]),
    )
    .unwrap();
    let expected = new.clone();
    let launcher = FakeLauncher::new(move |root, target, recover, _| {
        if recover {
            return obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed"}"#);
        }
        assert_eq!(target, Some(&expected));
        attached_answer(root, target, false, 0)
    });
    let (context, _) = context(fixture.host(), launcher);
    let observer = DesktopIntegration::new(fixture.root(), context);
    // when / then
    observer.tick().await.unwrap();
    assert_eq!(text(&read(&fixture.root().join("operation.json")), "phase").unwrap(), "opened");
    fixture.world.set(&world_of(vec![new_process]));
    observer.tick().await.unwrap();
    assert_eq!(text(&read(&fixture.root().join("operation.json")), "phase").unwrap(), "closed");
    observer.tick().await.unwrap();
    assert_eq!(text(&read(&fixture.root().join("status.json")), "state").unwrap(), "attached");
}

#[tokio::test]
async fn busy_recovery_lock_when_observed_then_token_not_consumed() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let target = fixture.installed(&app);
    storage::write_json(&fixture.root().join("config.json"), &obj("{}")).unwrap();
    storage::write_json(
        &fixture.root().join("operation.json"),
        &py::obj([("target", target.to_value()), ("phase", s("opened")), ("token", s("old"))]),
    )
    .unwrap();
    let launcher = FakeLauncher::new(|_, _, _, n| {
        let reason = if n == 1 { "attachment_in_progress" } else { "worker_interrupted" };
        py::obj([("state", s("error")), ("reason", s(reason))])
    });
    let (context, _) = context(fixture.host(), launcher.clone());
    let observer = DesktopIntegration::new(fixture.root(), context);
    // when
    for _ in 0..3 {
        observer.tick().await.unwrap();
    }
    // then
    assert_eq!(launcher.calls().iter().map(|(_, recover)| *recover).collect::<Vec<_>>(), [true, true]);
}

#[tokio::test]
async fn page_reinjected_when_observed_then_only_a_page_failure_is_restored() {
    let cases = [
        ("page_initialization_failed", true, "attached"),
        ("page_initialization_failed", false, "incompatible"),
        ("unsupported_version", true, "incompatible"),
    ];
    for (previous, same_target, expected) in cases {
        // given
        let fixture = Fixture::new();
        let app = fixture.app();
        let target = fixture.installed(&app);
        let other = Target { pid: 43, ..target.clone() };
        storage::write_json(
            &fixture.root().join("status.json"),
            &py::obj([
                ("state", s("incompatible")),
                ("reason", s(previous)),
                ("target", target.to_value()),
                ("inspector_closed", Value::Bool(true)),
                ("bundle", s("digest")),
            ]),
        )
        .unwrap();
        let runtime_target = if same_target { &target } else { &other };
        storage::write_json(
            &fixture.root().join("runtime.json"),
            &py::obj([("state", s("ready")), ("reason", Value::Null), ("target", runtime_target.to_value())]),
        )
        .unwrap();
        let (context, _) = context(fixture.host(), FakeLauncher::new(no_launch));
        let observer = DesktopIntegration::new(fixture.root(), context);
        // when
        observer.tick().await.unwrap();
        observer.tick().await.unwrap();
        // then
        let result = read(&fixture.root().join("status.json"));
        assert_eq!(text(&result, "state").unwrap(), expected, "{previous} {same_target}");
        let reason = if expected == "attached" { Value::Null } else { s(previous) };
        assert_eq!(field(&result, "reason"), &reason);
        assert_eq!(field(&result, "inspector_closed"), &Value::Bool(true));
        assert_eq!(text(&result, "bundle").unwrap(), "digest");
    }
}

#[tokio::test]
async fn late_runtime_incompatibility_when_observed_then_overrides_the_same_target_attachment() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let target = fixture.installed(&app);
    storage::write_json(
        &fixture.root().join("status.json"),
        &py::obj([("state", s("attached")), ("target", target.to_value()), ("inspector_closed", Value::Bool(true))]),
    )
    .unwrap();
    storage::write_json(
        &fixture.root().join("runtime.json"),
        &py::obj([("state", s("incompatible")), ("reason", s("unsupported_version")), ("target", target.to_value())]),
    )
    .unwrap();
    let (context, _) = context(fixture.host(), FakeLauncher::new(no_launch));
    let observer = DesktopIntegration::new(fixture.root(), context);
    // when
    observer.tick().await.unwrap();
    // then
    let result = read(&fixture.root().join("status.json"));
    assert_eq!(text(&result, "state").unwrap(), "incompatible");
    assert_eq!(text(&result, "reason").unwrap(), "unsupported_version");
    assert_eq!(field(&result, "inspector_closed"), &Value::Bool(true));
}

#[tokio::test]
async fn repeated_observer_failure_when_running_then_cause_logged_rate_limited_and_recorded() {
    // given
    let fixture = Fixture::new();
    storage::write_json(&fixture.root().join("config.json"), &obj(r#"{"enabled": true, "app": "/missing"}"#)).unwrap();
    let mut host = fixture.host();
    host.clock = ScriptedClock::new(100.0, &[0.0, 1.0, 2.0, 70.0]);
    let (context, lines) = context(host, FakeLauncher::new(no_launch));
    let observer = DesktopIntegration::new(fixture.root(), context);
    // when
    observer.run_ticks(Some(3)).await;
    observer.run_ticks(Some(1)).await;
    // then
    let lines = lines.lock().unwrap().clone();
    let first = "Desktop observer iteration failed: FileNotFoundError: [Errno 2] No such file or directory: '/missing'";
    assert_eq!(lines, [first.to_owned(), format!("{first} (+2 suppressed)")]);
    assert_eq!(text(&read(&fixture.root().join("status.json")), "reason").unwrap(), "observer_failed");
    assert_eq!(field(&read(&fixture.root().join("heartbeat.json")), "running"), &Value::Bool(false));
}

#[tokio::test]
async fn unclosed_inspector_when_observer_saves_an_error_then_warning_kept() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    storage::write_json(&root.join("operation.json"), &obj(r#"{"phase": "opened"}"#)).unwrap();
    let critical = obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "inspector_closed": false}"#);
    storage::write_json(&root.join("status.json"), &critical).unwrap();
    let (context, _) = context(fixture.host(), FakeLauncher::new(no_launch));
    let observer = DesktopIntegration::new(root.clone(), context);
    // when
    observer.save(&obj(r#"{"state": "error", "reason": "observer_failed", "inspector_closed": null}"#)).await.unwrap();
    // then
    assert_eq!(read(&root.join("status.json")), critical);
}

#[tokio::test]
async fn disabled_observer_when_target_exited_then_unclosed_warning_cleared() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    storage::write_json(&root.join("config.json"), &obj(r#"{"enabled": false, "app": "/ktalk"}"#)).unwrap();
    storage::write_json(
        &root.join("operation.json"),
        &py::obj([("target", target_42().to_value()), ("phase", s("opened")), ("token", s("old"))]),
    )
    .unwrap();
    storage::write_json(
        &root.join("status.json"),
        &obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "inspector_closed": false}"#),
    )
    .unwrap();
    let (context, _) = context(fixture.host(), FakeLauncher::new(no_launch));
    let observer = DesktopIntegration::new(root.clone(), context);
    // when
    observer.tick().await.unwrap();
    // then
    assert_eq!(text(&read(&root.join("operation.json")), "phase").unwrap(), "closed");
    assert_eq!(text(&read(&root.join("status.json")), "state").unwrap(), "disabled");
    assert_eq!(
        service::status_at(&root, now()),
        obj(
            r#"{"enabled": false, "state": "disabled", "reason": null, "app_version": null, "inspector_closed": null}"#
        )
    );
}

#[tokio::test]
async fn repeated_error_when_notified_then_attempted_once_per_target() {
    let cases = [
        (Platform::Darwin, true, "exited"),
        (Platform::Linux, true, "exited"),
        (Platform::Linux, false, "exited"),
        (Platform::Darwin, true, "timeout"),
        (Platform::Darwin, true, "missing"),
    ];
    for (platform, available, mode) in cases {
        // given
        let fixture = Fixture::new();
        let mut host = fixture.host();
        host.platform = platform.clone();
        let notifier = Arc::new(FakeNotifier { calls: Mutex::default(), notify_send: available, mode: mode.into() });
        let (mut context, lines) = context(host, FakeLauncher::new(no_launch));
        context.notifier = notifier.clone();
        let observer = DesktopIntegration::new(fixture.path().to_path_buf(), context);
        let result = obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "target": {"pid": 42}}"#);
        // when
        observer.notify(&obj(r#"{"state": "attached"}"#)).await;
        observer.notify(&result).await;
        observer.notify(&result).await;
        // then
        let calls = notifier.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), usize::from(available), "{platform:?} {available} {mode}");
        if platform == Platform::Darwin {
            assert_eq!(calls[0][..2], ["osascript", "-e"]);
            assert_eq!(
                calls[0][2],
                "display notification \"Не удалось закрыть отладочное подключение. Перезапустите Толк после встречи.\" with title \"Recog\""
            );
        }
        let unavailable = lines.lock().unwrap().iter().any(|l| l == "Desktop integration notification unavailable");
        assert_eq!(unavailable, mode == "missing");
    }
}

// --- launch_worker ---------------------------------------------------------------

struct ScriptedChild {
    waits: VecDeque<bool>,
    events: Arc<Mutex<Vec<String>>>,
}

impl WorkerProcess for ScriptedChild {
    fn wait_for(&mut self, _timeout: Duration) -> BoxFuture<'_, bool> {
        let exited = self.waits.pop_front().unwrap_or(true);
        Box::pin(async move { exited })
    }

    fn kill_and_reap(&mut self) -> BoxFuture<'_, ()> {
        self.events.lock().unwrap().push("kill child".into());
        self.events.lock().unwrap().push("reap child".into());
        Box::pin(async {})
    }
}

/// What a fake worker does when started, given its arguments.
type OnSpawn = Box<dyn Fn(&[String]) + Send + Sync>;

struct ScriptedSpawner {
    calls: Mutex<Vec<Vec<String>>>,
    events: Arc<Mutex<Vec<String>>>,
    waits: Vec<bool>,
    on_spawn: OnSpawn,
}

impl Spawner for ScriptedSpawner {
    fn spawn(&self, args: Vec<std::ffi::OsString>) -> BoxFuture<'_, Result<Box<dyn WorkerProcess>, py::PyErr>> {
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        (self.on_spawn)(&args);
        self.calls.lock().unwrap().push(args);
        let child = ScriptedChild { waits: self.waits.iter().copied().collect(), events: Arc::clone(&self.events) };
        Box::pin(async move { Ok(Box::new(child) as Box<dyn WorkerProcess>) })
    }
}

#[tokio::test]
async fn polled_child_worker_when_it_ends_then_heartbeats_and_interruptions_recovered() {
    for mode in ["success", "stale", "attaching", "unclosed", "interrupted", "poll_timeout"] {
        // given
        let fixture = Fixture::new();
        let root = fixture.root();
        std::fs::create_dir_all(&root).unwrap();
        let target = target_42();
        let mut host = fixture.host();
        host.clock = ScriptedClock::new(100.0, &[0.0]);
        let written_target = target.to_value();
        let writer_root = root.clone();
        let spawner = ScriptedSpawner {
            calls: Mutex::default(),
            events: Arc::default(),
            waits: if mode == "poll_timeout" { vec![false, true] } else { vec![true] },
            on_spawn: Box::new(move |args| {
                let recovering = args.iter().any(|a| a == "--recover");
                let state = match mode {
                    "attaching" => "attaching",
                    "unclosed" => "inspector_unclosed",
                    _ => "attached",
                };
                let updated = if mode == "stale" { 99 } else { 101 };
                if mode == "interrupted" {
                    let phase = if recovering { "closed" } else { "opened" };
                    storage::write_json(&writer_root.join("operation.json"), &py::obj([("phase", s(phase))])).unwrap();
                }
                let result = py::obj([
                    ("state", s(state)),
                    ("updated_at", Value::Int(updated)),
                    ("target", written_target.clone()),
                ]);
                storage::write_json(&writer_root.join("status.json"), &result).unwrap();
            }),
        };
        // when
        let result = service::launch_worker(&host, &spawner, &root, Some(target), false).await.unwrap();
        // then
        let expected = match mode {
            "stale" | "attaching" => "error",
            "unclosed" => "inspector_unclosed",
            _ => "attached",
        };
        assert_eq!(text(&result, "state").unwrap(), expected, "{mode}");
        assert_eq!(spawner.calls.lock().unwrap().len(), if mode == "interrupted" { 2 } else { 1 }, "{mode}");
        if mode == "poll_timeout" {
            assert_eq!(field(&read(&root.join("heartbeat.json")), "running"), &Value::Bool(true));
        }
        assert!(spawner.events.lock().unwrap().is_empty(), "{mode}");
    }
}

#[tokio::test]
async fn worker_deadline_when_exceeded_then_only_the_spawned_child_is_killed() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    std::fs::create_dir_all(&root).unwrap();
    let mut host = fixture.host();
    host.clock = ScriptedClock::new(100.0, &[0.0, 18.0]);
    host.signal_usr1 = Arc::new(|_| panic!("signalled the application"));
    let spawner =
        ScriptedSpawner { calls: Mutex::default(), events: Arc::default(), waits: vec![], on_spawn: Box::new(|_| {}) };
    // when
    let result = service::launch_worker(&host, &spawner, &root, Some(target_42()), false).await.unwrap();
    // then
    assert_eq!(*spawner.events.lock().unwrap(), ["kill child", "reap child"]);
    assert_eq!(text(&result, "reason").unwrap(), "worker_failed");
    assert_eq!(field(&result, "target"), &target_42().to_value());
}

// --- worker ----------------------------------------------------------------------

#[tokio::test]
async fn foreign_listener_on_the_port_when_attaching_then_no_signal_sent() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let target = fixture.installed(&app);
    let _foreign = std::net::TcpListener::bind(("127.0.0.1", fixture.inspector_port)).unwrap();
    // when
    let result = worker::attach(&fixture.host(), &fixture.root(), &target).await;
    // then
    assert_eq!(result.unwrap_err(), Failure::attach("inspector_port_busy"));
    assert!(fixture.world.signalled().is_empty());
    assert_eq!(read(&fixture.root().join("operation.json")), obj("{}"));
}

#[tokio::test]
async fn unsupported_inspector_when_attaching_then_target_never_signalled() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let target = fixture.installed(&app);
    std::fs::write(&app.fuses, fuse_wire(false, 1, 8)).unwrap();
    // when
    let result = worker::attach(&fixture.host(), &fixture.root(), &target).await;
    // then
    assert_eq!(result.unwrap_err(), Failure::attach("inspector_disabled_or_unverified"));
    assert!(fixture.world.signalled().is_empty());
    assert_eq!(read(&fixture.root().join("operation.json")), obj("{}"));
}

#[tokio::test]
async fn failed_bootstrap_when_attaching_then_only_its_own_inspector_closed() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let target = fixture.installed(&app);
    let inspector = fixture.inspector(Behaviour { bootstrap: "throw".into(), ..Behaviour::default() }).await;
    // when
    let result = worker::attach(&fixture.host(), &fixture.root(), &target).await.unwrap();
    // then
    assert_eq!(fixture.world.signalled(), [42]);
    assert_eq!(text(&result, "reason").unwrap(), "inspector_evaluation_failed");
    assert_eq!(field(&result, "inspector_closed"), &Value::Bool(true));
    assert_eq!(text(&read(&fixture.root().join("operation.json")), "phase").unwrap(), "closed");
    let kinds: Vec<String> = inspector.entries().iter().filter_map(|e| text(e, "kind")).collect();
    assert_eq!(kinds.last().map(String::as_str), Some("close"));
    inspector.stop();
}

#[tokio::test]
async fn foreign_or_reused_inspector_when_cleaning_up_then_never_evaluated() {
    // given: the pid listens on the inspector port, but not on loopback only
    let fixture = Fixture::new();
    let host = fixture.host();
    let port = fixture.inspector_port;
    fixture.world.set(&world_of(vec![plain_process(&format!(r#"[["0.0.0.0", {port}]]"#), "{}", r#"["ktalk"]"#)]));
    let operation =
        py::obj([("target", target_42().to_value()), ("endpoint", s(&format!("ws://127.0.0.1:{port}/old")))]);
    let mut clock_host = host.clone();
    clock_host.clock = ScriptedClock::new(0.0, &[0.0, 0.1, 6.0]);
    // when / then
    assert!(!worker::close_owned(&clock_host, &operation).await.unwrap());
    fixture.world.set(&world_of(vec![]));
    assert!(worker::close_owned(&host, &operation).await.unwrap());
}

#[tokio::test]
async fn opening_phase_when_cleaning_up_then_inspector_never_discovered_or_evaluated() {
    // given
    let fixture = Fixture::new();
    let port = fixture.inspector_port;
    fixture.world.set(&world_of(vec![plain_process(&format!(r#"[["127.0.0.1", {port}]]"#), "{}", r#"["ktalk"]"#)]));
    let inspector = fixture.inspector(Behaviour { listen_from_start: true, ..Behaviour::default() }).await;
    // when
    let closed =
        worker::close_owned(&fixture.host(), &py::obj([("target", target_42().to_value()), ("phase", s("opening"))]))
            .await;
    // then
    assert!(!closed.unwrap());
    assert!(inspector.entries().is_empty());
    inspector.stop();
}

fn with_lock_free_home(fixture: &Fixture) -> Host {
    fixture.host()
}

#[tokio::test]
async fn interrupted_worker_when_recovered_then_closed_without_signalling_again() {
    // given: the process has gone, so its inspector is gone too
    let fixture = Fixture::new();
    let root = fixture.root();
    storage::write_json(
        &root.join("operation.json"),
        &py::obj([("target", target_42().to_value()), ("phase", s("opened")), ("token", s("old"))]),
    )
    .unwrap();
    // when
    let result = worker::run(&with_lock_free_home(&fixture), &root, None, true).await.unwrap();
    // then
    assert_eq!(text(&result, "reason").unwrap(), "worker_interrupted");
    assert_eq!(field(&result, "inspector_closed"), &Value::Bool(true));
    assert_eq!(text(&read(&root.join("operation.json")), "phase").unwrap(), "closed");
    assert!(fixture.world.signalled().is_empty());
}

#[tokio::test]
async fn journal_state_when_recovering_then_unclosed_state_preserved() {
    let cases = [(None, true), (Some("closed"), true), (Some("opened"), false)];
    for (phase, closed) in cases {
        // given: an alive process whose journal has no endpoint cannot be closed
        let fixture = Fixture::new();
        let root = fixture.root();
        std::fs::create_dir_all(&root).unwrap();
        fixture.world.set(&world_of(vec![plain_process("[]", "{}", r#"["ktalk"]"#)]));
        if let Some(phase) = phase {
            storage::write_json(
                &root.join("operation.json"),
                &py::obj([("phase", s(phase)), ("target", target_42().to_value())]),
            )
            .unwrap();
        }
        // when
        let result = worker::run(&fixture.host(), &root, None, true).await.unwrap();
        // then
        assert_eq!(field(&result, "inspector_closed"), &Value::Bool(closed), "{phase:?}");
        assert_eq!(text(&result, "state").unwrap(), if closed { "waiting" } else { "inspector_unclosed" });
    }
}

#[tokio::test]
async fn busy_attachment_lock_when_running_then_existing_cleanup_warning_kept() {
    for critical in [false, true] {
        // given
        let fixture = Fixture::new();
        let root = fixture.root();
        std::fs::create_dir_all(fixture.home.join(".cache/recog")).unwrap();
        let held = std::fs::File::create(fixture.home.join(".cache/recog/desktop-attach.lock")).unwrap();
        held.try_lock().unwrap();
        if critical {
            storage::write_json(
                &root.join("status.json"),
                &obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed"}"#),
            )
            .unwrap();
        }
        // when
        let result = worker::run(&fixture.host(), &root, Some(&target_42()), false).await.unwrap();
        // then
        let expected = if critical { "cleanup_failed" } else { "attachment_in_progress" };
        assert_eq!(text(&result, "reason").unwrap(), expected);
    }
}

#[tokio::test]
async fn corrupt_bundle_when_running_then_unclosed_inspector_not_overwritten() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    storage::write_json(&root.join("config.json"), &obj(r#"{"app": "/ktalk", "bundle": "bad"}"#)).unwrap();
    storage::write_json(
        &root.join("operation.json"),
        &py::obj([("target", target_42().to_value()), ("phase", s("opened")), ("token", s("old"))]),
    )
    .unwrap();
    let critical = py::obj([
        ("state", s("inspector_unclosed")),
        ("reason", s("cleanup_failed")),
        ("inspector_closed", Value::Bool(false)),
        ("target", target_42().to_value()),
    ]);
    storage::write_json(&root.join("status.json"), &critical).unwrap();
    // when
    let result = worker::run(&fixture.host(), &root, Some(&target_42()), false).await.unwrap();
    // then
    assert_eq!(result, critical);
    assert_eq!(read(&root.join("status.json")), critical);
}

#[tokio::test]
async fn previous_operation_when_attaching_then_identity_and_cleanup_required_before_signal() {
    let cases = [(false, true), (true, false), (true, true)];
    for (same, closed) in cases {
        // given
        let fixture = Fixture::new();
        let app = fixture.app();
        let target = fixture.installed(&app);
        let old = Target { pid: 41, started: 11.0, executable: target.executable.clone() };
        let mut processes_list = Vec::new();
        if same {
            processes_list.push(fixture.world.get().as_object().unwrap().get("processes").unwrap().as_array_first());
        }
        if !closed {
            let mut alive = ktalk_process(41, &app, fixture.ready_port);
            alive.as_object_mut().unwrap().insert("started", Value::Float(11.0));
            processes_list.push(alive);
        }
        fixture.world.set(&world_of(processes_list));
        storage::write_json(
            &fixture.root().join("operation.json"),
            &py::obj([("target", old.to_value()), ("phase", s("opened"))]),
        )
        .unwrap();
        let _busy = std::net::TcpListener::bind(("127.0.0.1", fixture.inspector_port)).unwrap();
        // when
        let result = worker::attach(&fixture.host(), &fixture.root(), &target).await;
        // then
        match (same, closed) {
            (false, _) => assert_eq!(result.unwrap_err(), Failure::attach("process_changed")),
            (true, false) => assert_eq!(
                result.unwrap(),
                py::obj([
                    ("state", s("inspector_unclosed")),
                    ("reason", s("cleanup_failed")),
                    ("inspector_closed", Value::Bool(false)),
                    ("target", old.to_value())
                ])
            ),
            (true, true) => assert_eq!(result.unwrap_err(), Failure::attach("inspector_port_busy")),
        }
        let phase = text(&read(&fixture.root().join("operation.json")), "phase").unwrap();
        assert_eq!(phase, if same && closed { "closed" } else { "opened" }, "{same} {closed}");
        assert!(fixture.world.signalled().is_empty());
    }
}

trait First {
    fn as_array_first(&self) -> Value;
}

impl First for Value {
    fn as_array_first(&self) -> Value {
        match self {
            Value::Array(items) => items[0].clone(),
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn attachment_outcome_when_operation_ends_then_cleanup_matches_signal_ownership() {
    let cases = [
        ("success", true, None),
        ("timeout", true, Some("attachment_timeout")),
        ("unexpected", true, Some("attachment_failed")),
        ("success", false, Some("cleanup_failed")),
        ("changed", true, Some("process_changed")),
        ("exited", true, Some("process_exited")),
    ];
    for (outcome, closed, reason) in cases {
        // given
        let fixture = Fixture::new();
        let app = fixture.app();
        let target = fixture.installed(&app);
        let behaviour = Behaviour {
            identity: match outcome {
                "timeout" => "slow_open".into(),
                "unexpected" => "garbage".into(),
                _ => "match".into(),
            },
            close: if closed { "close".into() } else { "ignore".into() },
            exit_on_signal: outcome == "exited",
            ..Behaviour::default()
        };
        let inspector = fixture.inspector(behaviour).await;
        let table = Arc::new(FakeTable::new(&fixture.world));
        if outcome == "changed" {
            // The first identity check and readiness see the process, the last check
            // before the signal a replacement.
            *table.started_script.lock().unwrap() = [target.started, target.started, target.started + 1.0].into();
        }
        let host = host_with(table, &fixture.world, fixture.inspector_port, fixture.ready_port, &fixture.home);
        // when
        let result = worker::attach(&host, &fixture.root(), &target).await.unwrap();
        // then
        assert_eq!(field(&result, "reason"), &reason.map_or(Value::Null, Value::from), "{outcome} {closed}");
        assert_eq!(field(&result, "inspector_closed"), &Value::Bool(closed), "{outcome}");
        assert_eq!(fixture.world.signalled().len(), usize::from(outcome != "changed"), "{outcome}");
        let phase = text(&read(&fixture.root().join("operation.json")), "phase").unwrap();
        assert_eq!(phase, if closed { "closed" } else { "opened" }, "{outcome}");
        inspector.stop();
    }
}

#[tokio::test]
async fn ownership_change_when_endpoint_discovered_then_refused() {
    let cases = [
        (false, false, None, false, None),
        (true, false, None, false, Some("inspector_owner_mismatch")),
        (false, true, None, false, Some("inspector_owner_mismatch")),
        (false, false, Some("old"), false, Some("inspector_session_changed")),
        (false, false, None, true, Some("invalid_inspector_response")),
    ];
    for (never_owned, drop_on_list, expected, large, reason) in cases {
        // given
        let fixture = Fixture::new();
        let port = fixture.inspector_port;
        let listeners = if never_owned { "[]".to_owned() } else { format!(r#"[["127.0.0.1", {port}]]"#) };
        fixture.world.set(&world_of(vec![plain_process(&listeners, "{}", r#"["ktalk"]"#)]));
        let behaviour = Behaviour {
            listen_from_start: true,
            drop_on_list,
            list: if large { "big".into() } else { "one".into() },
            ..Behaviour::default()
        };
        let inspector = fixture.inspector(behaviour).await;
        let expected = expected.map(|e| s(&format!("ws://127.0.0.1:{port}/{e}")));
        // when
        let result = worker::endpoint(&fixture.host(), &target_42(), expected.as_ref()).await;
        // then
        match reason {
            None => assert_eq!(result.unwrap(), format!("ws://127.0.0.1:{port}/session")),
            Some(reason) => assert_eq!(result.unwrap_err(), Failure::attach(reason)),
        }
        inspector.stop();
    }
}

#[tokio::test]
async fn events_or_errors_when_evaluating_then_filtered_and_redacted() {
    for (identity, expected) in [("match", Ok(obj(r#"{"pid": 42}"#))), ("throw", Err(())), ("error", Err(()))] {
        // given
        let fixture = Fixture::new();
        fixture.world.set(&world_of(vec![plain_process("[]", "{}", r#"["ktalk"]"#)]));
        let inspector = fixture
            .inspector(Behaviour { identity: identity.into(), listen_from_start: true, ..Behaviour::default() })
            .await;
        let url = format!("ws://127.0.0.1:{}/session", fixture.inspector_port);
        // when
        let result = recog_desktop::inspector::evaluate(&url, &worker::identity_expression()).await;
        // then
        match expected {
            Ok(value) => assert_eq!(result.unwrap(), value),
            Err(()) => assert_eq!(result.unwrap_err(), Failure::attach("inspector_evaluation_failed")),
        }
        let sent = inspector.entries();
        assert_eq!(field(field(&sent[0], "params"), "expression").py_str(), worker::identity_expression());
        inspector.stop();
    }
}

#[tokio::test]
async fn redirecting_listener_when_connecting_then_refused_without_following() {
    // given: the inspector port answers the upgrade with a redirect elsewhere
    let port = free_port();
    let followed = Arc::new(Mutex::new(false));
    let hit = Arc::clone(&followed);
    let app = axum::Router::new()
        .route(
            "/x",
            axum::routing::get(move || async move {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, format!("ws://127.0.0.1:{port}/other"))],
                )
            }),
        )
        .route(
            "/other",
            axum::routing::get(move || {
                *hit.lock().unwrap() = true;
                async { "other" }
            }),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let serving = tokio::spawn(async move { axum::serve(listener, app).await });
    // when
    let result = recog_desktop::inspector::evaluate(&format!("ws://127.0.0.1:{port}/x"), "1").await;
    // then
    assert!(matches!(result, Err(Failure::Other(_))));
    assert!(!*followed.lock().unwrap());
    serving.abort();
}

#[test]
fn delayed_close_when_the_inspector_was_replaced_then_replacement_not_closed() {
    // given
    let Ok(node) = which_node() else {
        eprintln!("skipped: Node is unavailable");
        return;
    };
    let script = r"
const vm = require('node:vm');
const expression = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
let endpoint = 'ws://127.0.0.1:9229/first';
let closes = 0;
let callback;
const inspector = { url: () => endpoint, close: () => { closes++; endpoint = undefined; } };
const context = {
  require: name => { if (name !== 'node:inspector') throw Error(name); return inspector; },
  setTimeout: fn => { callback = fn; return 1; },
  clearTimeout: () => {},
};
vm.createContext(context);
if (vm.runInContext(expression, context) !== true) throw Error('close was not scheduled');
endpoint = 'ws://127.0.0.1:9229/replacement';
callback();
if (closes !== 0) throw Error('replacement inspector closed');
vm.runInContext(expression, context);
callback();
if (closes !== 1) throw Error('same inspector was not closed');
";
    // when
    let mut child = std::process::Command::new(node)
        .args(["-e", script])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(py::dumps(&s(&worker::close_expression())).as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    // then
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

fn which_node() -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    std::env::split_paths(&path).map(|d| d.join("node")).find(|p| p.is_file()).ok_or(())
}

// --- cli -------------------------------------------------------------------------

fn command(args: &str) -> cli::Command {
    cli::parse(&args.split_whitespace().map(str::to_owned).collect::<Vec<_>>()).unwrap()
}

#[tokio::test]
async fn disabled_installation_when_status_dispatched_then_json_and_exit_zero() {
    // given
    let fixture = Fixture::new();
    // when
    let outcome = cli::main(&fixture.host(), &command("status --json"), fixture.path(), &repo_root()).await;
    // then
    assert_eq!(outcome.code, 0);
    assert_eq!(text(&obj(&outcome.stdout), "state").unwrap(), "disabled");
    assert!(!fixture.path().join("desktop").exists());
}

#[tokio::test]
async fn configured_installation_when_disabled_then_snapshot_preserved() {
    // given
    let fixture = Fixture::new();
    let root = fixture.path().join("desktop");
    storage::write_json(&root.join("config.json"), &obj(r#"{"enabled": true, "app": "/ktalk", "bundle": "digest"}"#))
        .unwrap();
    // when
    let outcome = cli::main(&fixture.host(), &command("disable"), fixture.path(), &repo_root()).await;
    // then
    assert_eq!(outcome.code, 0);
    assert_eq!(read(&root.join("config.json")), obj(r#"{"enabled": false, "app": "/ktalk", "bundle": "digest"}"#));
}

#[tokio::test]
async fn selected_application_when_enabled_then_verified_bundle_snapshotted() {
    // given
    let fixture = Fixture::new();
    let app = fixture.app();
    let fake_repo = fixture.path().join("repo");
    std::fs::create_dir_all(fake_repo.join("browser-extension")).unwrap();
    std::fs::rename(dist(fixture.path()), fake_repo.join("browser-extension/dist")).unwrap();
    // when
    let outcome = cli::main(
        &fixture.host(),
        &command(&format!("enable --app {}", app.app.display())),
        fixture.path(),
        &fake_repo,
    )
    .await;
    // then
    assert_eq!(outcome.code, 0, "{outcome:?}");
    let config = read(&fixture.path().join("desktop/config.json"));
    assert_eq!(field(&config, "enabled"), &Value::Bool(true));
    assert_eq!(text(&config, "app").unwrap(), app.app.to_string_lossy());
    let digest = text(&config, "bundle").unwrap();
    assert!(fixture.path().join("desktop/extensions").join(&digest).join("injected.js").is_file());
}

#[tokio::test]
async fn target_count_or_result_when_attaching_manually_then_exit_status_reports_it() {
    let cases = [(0, "attached", 1), (2, "attached", 1), (1, "attached", 0), (1, "incompatible", 1)];
    for (count, state, expected) in cases {
        // given
        let fixture = Fixture::new();
        let app = fixture.app();
        fixture.installed(&app);
        let processes_list: Vec<Value> = (0..count).map(|i| ktalk_process(42 + i, &app, fixture.ready_port)).collect();
        fixture.world.set(&world_of(processes_list));
        if state == "incompatible" {
            std::fs::write(&app.fuses, fuse_wire(false, 1, 8)).unwrap();
        }
        let inspector = fixture.inspector(Behaviour::default()).await;
        // when
        let outcome = cli::main(&fixture.host(), &command("attach"), &fixture.path().join("data"), &repo_root()).await;
        // then
        assert_eq!(outcome.code, expected, "{count} {state}: {outcome:?}");
        if count == 1 {
            assert_eq!(text(&obj(&outcome.stdout), "state").unwrap(), state);
        } else {
            assert_eq!(
                outcome.stdout,
                "Desktop integration: Нужен ровно один работающий основной процесс выбранного Толка\n"
            );
        }
        inspector.stop();
    }
}
