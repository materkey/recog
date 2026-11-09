//! service.rs on the timelines and status directories tests/differential.rs gave
//! both implementations, with the files Python left stated: what one observation
//! records, the worker launch, the notifications and `status()`.
#![forbid(unsafe_code)]

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use recog_core::pyjson::Value;
use recog_desktop::host::{Host, Platform, ScriptedClock};
use recog_desktop::processes::Target;
use recog_desktop::py::{self, PyErr};
use recog_desktop::service::{
    self, BoxFuture, Context, DesktopIntegration, Launcher, Log, Notifier, NotifyOutcome, Spawner, SystemNotifier,
    WorkerCommand, WorkerProcess,
};
use recog_desktop::storage;

fn s(text: &str) -> Value {
    Value::from(text)
}

struct Fixture {
    dir: tempfile::TempDir,
    world: World,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let world = World::new(&dir.path().join("world"));
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self { dir, world, home }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn root(&self) -> PathBuf {
        self.path().join("data/desktop")
    }

    /// A Linux host over the world, its clock at 100 s.
    fn host(&self) -> Host {
        let mut host = host(&self.world, free_port(), free_port(), &self.home);
        host.platform = Platform::Linux;
        host.clock = ScriptedClock::new(100.0, &[0.0]);
        host
    }

    fn read(&self, name: &str) -> Value {
        storage::read(&self.root().join(name)).unwrap()
    }

    fn write(&self, name: &str, value: &str) {
        storage::write_json(&self.root().join(name), &obj(value)).unwrap();
    }

    /// The configuration selecting `app`, enabled or not.
    fn configure(&self, app: &FakeApp, enabled: bool) {
        let config = py::obj([
            ("enabled", Value::Bool(enabled)),
            ("app", s(&app.app.to_string_lossy())),
            ("bundle", s("unused")),
        ]);
        storage::write_json(&self.root().join("config.json"), &config).unwrap();
    }
}

/// `launch_worker` replaced: the calls recorded, each answered with `answer`.
struct FakeLauncher {
    calls: Mutex<Vec<(Option<Target>, bool)>>,
    answer: Value,
}

impl Launcher for FakeLauncher {
    fn launch<'a>(&'a self, _: &'a Path, target: Option<Target>, recover: bool) -> BoxFuture<'a, Result<Value, PyErr>> {
        self.calls.lock().unwrap().push((target, recover));
        let answer = self.answer.clone();
        Box::pin(async move { Ok(answer) })
    }
}

/// Notifications recorded instead of shown; `notify-send` installed.
#[derive(Default)]
struct FakeNotifier {
    calls: Mutex<Vec<Vec<String>>>,
}

impl Notifier for FakeNotifier {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, std::io::Result<NotifyOutcome>> {
        self.calls.lock().unwrap().push(args);
        Box::pin(async { Ok(NotifyOutcome::Exited) })
    }

    fn which(&self, _: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/usr/bin/notify-send"))
    }
}

struct Observed {
    observer: Arc<DesktopIntegration>,
    launcher: Arc<FakeLauncher>,
    notifier: Arc<FakeNotifier>,
}

fn observer(fixture: &Fixture, host: Host, answer: &str) -> Observed {
    let launcher = Arc::new(FakeLauncher { calls: Mutex::default(), answer: obj(answer) });
    let notifier = Arc::new(FakeNotifier::default());
    let context = Context {
        host,
        launcher: launcher.clone(),
        notifier: notifier.clone(),
        log: Arc::new(Log::to(|_| {})),
        poll: Duration::ZERO,
    };
    Observed { observer: DesktopIntegration::new(fixture.root(), context), launcher, notifier }
}

fn target_of(process: &Value) -> Value {
    py::obj([
        ("pid", field(process, "pid").clone()),
        ("started", field(process, "started").clone()),
        ("executable", field(process, "exe").clone()),
    ])
}

/// The status an observation saves when it does not attach to `process`.
fn unattached(state: &str, reason: &str, process: &Value) -> Value {
    py::obj([
        ("state", s(state)),
        ("reason", s(reason)),
        ("target", target_of(process)),
        ("inspector_closed", Value::Null),
        ("updated_at", Value::Float(100.0)),
    ])
}

const HEARTBEAT: &str = r#"{"running": true, "updated_at": 100.0}"#;

#[tokio::test]
async fn no_or_several_main_processes_when_observed_then_waiting_for_one() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host();
    let app = fake_app(fixture.path(), "FakeTalk");
    fixture.configure(&app, true);
    let observed = observer(&fixture, host.clone(), "{}");
    // when: none
    observed.observer.tick().await.unwrap();
    // then
    assert_eq!(fixture.read("heartbeat.json"), obj(HEARTBEAT));
    assert_eq!(
        fixture.read("status.json"),
        obj(r#"{"state": "waiting", "reason": null, "inspector_closed": null, "updated_at": 100.0}"#)
    );
    // when: two
    let ready = host.app_ready_port;
    fixture.world.set(&world_of(vec![ktalk_process(42, &app, ready), ktalk_process(43, &app, ready)]));
    observed.observer.tick().await.unwrap();
    // then
    assert_eq!(
        fixture.read("status.json"),
        obj(r#"{"state": "waiting", "reason": "multiple_processes", "inspector_closed": null, "updated_at": 100.0}"#)
    );
    assert!(observed.launcher.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn disabled_configuration_when_observed_then_only_the_heartbeat() {
    // given
    let fixture = Fixture::new();
    let app = fake_app(fixture.path(), "FakeTalk");
    fixture.configure(&app, false);
    let observed = observer(&fixture, fixture.host(), "{}");
    // when
    observed.observer.tick().await.unwrap();
    // then
    assert_eq!(fixture.read("heartbeat.json"), obj(HEARTBEAT));
    assert!(!fixture.root().join("status.json").exists());
    assert!(observed.observer.holds_lock().await);
}

#[tokio::test]
async fn second_observer_of_a_directory_when_ticked_then_does_nothing() {
    // given
    let fixture = Fixture::new();
    let app = fake_app(fixture.path(), "FakeTalk");
    fixture.configure(&app, true);
    let first = observer(&fixture, fixture.host(), "{}");
    let second = observer(&fixture, fixture.host(), "{}");
    first.observer.tick().await.unwrap();
    std::fs::remove_file(fixture.root().join("heartbeat.json")).unwrap();
    // when
    second.observer.tick().await.unwrap();
    // then
    assert!(!second.observer.holds_lock().await);
    assert!(!fixture.root().join("heartbeat.json").exists());
}

#[tokio::test]
async fn application_still_starting_when_observed_then_waiting_without_a_worker() {
    // given: no URL-handler listener yet
    let fixture = Fixture::new();
    let host = fixture.host();
    let app = fake_app(fixture.path(), "FakeTalk");
    fixture.configure(&app, true);
    let mut process = ktalk_process(42, &app, host.app_ready_port);
    process.as_object_mut().unwrap().insert("listeners", Value::Array(vec![]));
    fixture.world.set(&world_of(vec![process.clone()]));
    let observed = observer(&fixture, host, "{}");
    // when
    observed.observer.tick().await.unwrap();
    // then
    assert_eq!(fixture.read("status.json"), unattached("waiting", "application_not_ready_or_debugging", &process));
    assert!(observed.launcher.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn inspector_fuse_disabled_when_observed_then_incompatible_once_and_notified_once() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host();
    let app = fake_app(fixture.path(), "FakeTalk");
    std::fs::write(&app.fuses, fuse_wire(false, 1, 8)).unwrap();
    fixture.configure(&app, true);
    let process = ktalk_process(42, &app, host.app_ready_port);
    fixture.world.set(&world_of(vec![process.clone()]));
    let observed = observer(&fixture, host, "{}");
    // when
    observed.observer.tick().await.unwrap();
    observed.observer.tick().await.unwrap();
    // then
    assert_eq!(fixture.read("status.json"), unattached("incompatible", "inspector_disabled_or_unverified", &process));
    assert!(observed.launcher.calls.lock().unwrap().is_empty());
    assert_eq!(
        *observed.notifier.calls.lock().unwrap(),
        [vec![
            "notify-send".to_owned(),
            "Recog".to_owned(),
            "Не удалось подключить Recog к Толку. Подробности в настройках Recog.".to_owned()
        ]]
    );
}

#[tokio::test]
async fn interrupted_operation_of_an_exited_process_when_observed_then_closed_and_disabled_kept() {
    // given
    let fixture = Fixture::new();
    fixture.write("config.json", r#"{"enabled": false}"#);
    fixture.write(
        "operation.json",
        r#"{"target": {"pid": 77, "started": 1.0, "executable": "/gone"}, "phase": "opened", "token": "t"}"#,
    );
    let observed = observer(&fixture, fixture.host(), "{}");
    // when
    observed.observer.tick().await.unwrap();
    // then
    assert_eq!(text(&fixture.read("operation.json"), "phase").as_deref(), Some("closed"));
    assert_eq!(
        fixture.read("status.json"),
        obj(r#"{"state": "disabled", "reason": null, "inspector_closed": true, "updated_at": 100.0}"#)
    );
    assert!(observed.launcher.calls.lock().unwrap().is_empty());
}

// --- the worker launch -----------------------------------------------------------------

/// A worker that exits at once, having written `status` (and `operation`).
struct ScriptedSpawner {
    root: PathBuf,
    status: String,
    operation: Option<String>,
    calls: Mutex<Vec<Vec<OsString>>>,
}

struct Exited;

impl WorkerProcess for Exited {
    fn wait_for(&mut self, _: Duration) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }

    fn kill_and_reap(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

impl Spawner for ScriptedSpawner {
    fn spawn(&self, args: Vec<OsString>) -> BoxFuture<'_, Result<Box<dyn WorkerProcess>, PyErr>> {
        let first = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(args);
            calls.len() == 1
        };
        std::fs::create_dir_all(&self.root).unwrap();
        std::fs::write(self.root.join("status.json"), &self.status).unwrap();
        if let Some(operation) = self.operation.as_ref().filter(|_| first) {
            std::fs::write(self.root.join("operation.json"), operation).unwrap();
        }
        Box::pin(async { Ok(Box::new(Exited) as Box<dyn WorkerProcess>) })
    }
}

fn spawner(root: &Path, status: &str, operation: Option<&str>) -> ScriptedSpawner {
    ScriptedSpawner {
        root: root.to_path_buf(),
        status: status.to_owned(),
        operation: operation.map(str::to_owned),
        calls: Mutex::default(),
    }
}

/// "Толк" as `json.dumps` escapes it: a backslash, `u` and four hex digits each.
const ASCII_TOLK: &str = concat!('\x5c', "u0422", '\x5c', "u043e", '\x5c', "u043b", '\x5c', "u043a");

fn target() -> Target {
    Target { pid: 42, started: 12.0, executable: "/Толк".into() }
}

#[tokio::test]
async fn worker_reports_when_launched_then_kept_failed_or_refused() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host();
    let root = fixture.root();
    let cases: Vec<(&str, Result<Value, String>)> = vec![
        (
            r#"{"state": "attached", "updated_at": 1000000000000.0}"#,
            Ok(obj(r#"{"state": "attached", "updated_at": 1000000000000.0}"#)),
        ),
        (
            r#"{"state": "attaching", "updated_at": 1000000000000.0}"#,
            Ok(obj(
                r#"{"state": "error", "reason": "worker_failed", "inspector_closed": null, "updated_at": 100.0, "target": {"pid": 42, "started": 12.0, "executable": "/Толк"}}"#,
            )),
        ),
        (
            r#"{"state": "attached", "updated_at": "x"}"#,
            Err("TypeError: '<' not supported between instances of 'str' and 'float'".into()),
        ),
    ];
    for (status, expected) in cases {
        let spawner = spawner(&root, status, None);
        // when
        let result = service::launch_worker(&host, &spawner, &root, Some(target()), false).await;
        // then
        assert_eq!(result.map_err(|e| e.described()), expected, "{status}");
        let args = spawner.calls.lock().unwrap()[0].clone();
        let expected_args: Vec<OsString> = vec![
            "--root".into(),
            root.clone().into(),
            "--target".into(),
            format!(r#"{{"pid": 42, "started": 12.0, "executable": "/{}"}}"#, ASCII_TOLK).into(),
        ];
        assert_eq!(args, expected_args);
    }
}

#[tokio::test]
async fn worker_leaving_an_open_journal_when_launched_then_followed_by_a_recovery() {
    // given
    let fixture = Fixture::new();
    let root = fixture.root();
    let spawner = spawner(
        &root,
        r#"{"state": "inspector_unclosed", "reason": "cleanup_failed"}"#,
        Some(r#"{"phase": "opened"}"#),
    );
    // when
    let result = service::launch_worker(&fixture.host(), &spawner, &root, Some(target()), false).await.unwrap();
    // then
    assert_eq!(result, obj(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed"}"#));
    let calls = spawner.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1], vec![OsString::from("--root"), root.into(), "--recover".into()]);
}

#[tokio::test]
async fn worker_program_when_spawned_then_runs_with_its_directory_environment_and_arguments() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let command = WorkerCommand {
        program: PathBuf::from("/bin/sh"),
        prefix: vec!["-c".into(), "pwd > out; printf '%s %s' \"$RECOG_TEST_VALUE\" \"$1\" >> out".into(), "sh".into()],
        cwd: Some(cwd.clone()),
        env: vec![("RECOG_TEST_VALUE".into(), "set".into())],
    };
    // when
    let mut child = command.spawn(vec!["--root".into()]).await.unwrap();
    // then
    assert!(child.wait_for(Duration::from_secs(10)).await);
    assert_eq!(std::fs::read_to_string(cwd.join("out")).unwrap(), format!("{}\nset --root", cwd.display()));
}

#[tokio::test]
async fn worker_still_running_or_missing_when_spawned_then_killed_or_refused() {
    // given
    let sleeper =
        WorkerCommand { program: PathBuf::from("/bin/sleep"), prefix: vec!["30".into()], cwd: None, env: Vec::new() };
    let missing = WorkerCommand { program: PathBuf::from("/nonexistent/worker"), ..sleeper.clone() };
    // when
    let mut child = sleeper.spawn(Vec::new()).await.unwrap();
    // then
    assert!(!child.wait_for(Duration::from_millis(50)).await);
    tokio::time::timeout(Duration::from_secs(10), child.kill_and_reap()).await.unwrap();
    assert!(child.wait_for(Duration::from_secs(1)).await);
    let refused = missing.spawn(Vec::new()).await.err().unwrap();
    assert_eq!(refused.described(), "FileNotFoundError: [Errno 2] No such file or directory: '/nonexistent/worker'");
}

// --- notifications ---------------------------------------------------------------------

#[tokio::test]
async fn notification_commands_when_run_then_exited_terminated_or_failed() {
    // given
    let notifier = SystemNotifier;
    // when / then
    assert!(matches!(notifier.run(vec!["true".into()]).await, Ok(NotifyOutcome::Exited)));
    assert!(matches!(notifier.run(vec!["sleep".into(), "30".into()]).await, Ok(NotifyOutcome::Terminated)));
    assert_eq!(notifier.run(Vec::new()).await.err().unwrap().to_string(), "no command");
    assert!(notifier.run(vec!["/nonexistent/notify".into()]).await.is_err());
}

#[test]
fn notification_programs_when_looked_up_then_found_on_path_only() {
    // given
    let notifier = SystemNotifier;
    // when / then
    assert!(notifier.which("sh").is_some_and(|path| path.ends_with("sh") && path.is_absolute()));
    assert_eq!(notifier.which("recog-no-such-program"), None);
}

// --- status ------------------------------------------------------------------------------

#[test]
fn status_directories_when_read_then_python_answers() {
    // given
    let fixture = Fixture::new();
    let invalid = r#"{"enabled": true, "state": "error", "reason": "invalid_configuration", "app_version": null, "inspector_closed": null}"#;
    let fresh = r#"{"running": true, "updated_at": 99}"#;
    // config.json, status.json, heartbeat.json, the status
    type Case = (Option<&'static str>, Option<&'static str>, Option<&'static str>, &'static str);
    let cases: Vec<Case> = vec![
        (
            Some(r#"{"enabled": true}"#),
            Some(r#"{"state": "attached", "app_version": "1"}"#),
            Some(fresh),
            r#"{"enabled": true, "app_version": "1", "reason": null, "inspector_closed": null, "state": "attached"}"#,
        ),
        (
            Some(r#"{"enabled": true}"#),
            None,
            Some(r#"{"running": true, "updated_at": 91.5}"#),
            r#"{"enabled": true, "app_version": null, "reason": "web_server_not_running", "inspector_closed": null, "state": "server_offline"}"#,
        ),
        (
            Some(r#"{"enabled": true}"#),
            None,
            Some(r#"{"running": false, "updated_at": 99}"#),
            r#"{"enabled": true, "app_version": null, "reason": "web_server_not_running", "inspector_closed": null, "state": "server_offline"}"#,
        ),
        (
            Some(r#"{"enabled": 1}"#),
            Some(r#"{"reason": "x"}"#),
            Some(r#"{"running": true, "updated_at": true}"#),
            r#"{"enabled": true, "app_version": null, "reason": "web_server_not_running", "inspector_closed": null, "state": "server_offline"}"#,
        ),
        (Some(r#"{"enabled": true}"#), None, Some(r#"{"running": true, "updated_at": "99"}"#), invalid),
        (Some(r#"{"enabled": true}"#), None, Some("\"x\""), invalid),
        (Some(r#"{"enabled": true}"#), Some("[]"), Some(fresh), invalid),
        (Some(r#"{"enabled": true}"#), Some("\u{feff}{\"state\": \"waiting\"}"), Some(fresh), invalid),
        (Some("[1]"), None, None, invalid),
        (Some("{ invalid"), None, None, invalid),
        (
            Some("[1]"),
            Some(r#"{"state": "inspector_unclosed", "app_version": "9"}"#),
            None,
            r#"{"state": "inspector_unclosed", "app_version": "9", "enabled": true, "inspector_closed": false}"#,
        ),
        (
            Some(r#"{"enabled": false}"#),
            Some(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "inspector_closed": false}"#),
            None,
            r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "inspector_closed": false, "enabled": false, "app_version": null}"#,
        ),
    ];
    for (i, (config, status, heartbeat, expected)) in cases.into_iter().enumerate() {
        let root = fixture.path().join(format!("status-{i}"));
        std::fs::create_dir_all(&root).unwrap();
        for (name, contents) in [("config.json", config), ("status.json", status), ("heartbeat.json", heartbeat)] {
            if let Some(contents) = contents {
                std::fs::write(root.join(name), contents).unwrap();
            }
        }
        // when
        let result = service::status_at(&root, 100.0);
        // then
        assert_eq!(py::dumps(&result), py::dumps(&obj(expected)), "{config:?} {status:?} {heartbeat:?}");
    }
}
