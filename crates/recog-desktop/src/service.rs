//! `recog/desktop/service.py`: the low-cost observer hosted by the web server, with
//! inspector work in a child process, and `status()`, which only reads the files.
//!
//! The observer polls every 2 s. While an installation is configured it holds
//! `observer.lock` (one observer per data directory), keeps `heartbeat.json` fresh,
//! recovers an interrupted attachment from the journal, and attaches each new main
//! process of the selected application once; failures that may clear by themselves
//! (a busy port, an application still starting) are retried.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use recog_core::pyjson::Value;
use tokio::sync::watch;

use crate::host::{Host, Platform};
use crate::processes::{self, Target};
use crate::py::{self, PyErr};
use crate::storage;

pub const POLL_SECONDS: f64 = 2.0;
/// The deadline of one worker, after which only the worker is killed.
pub const WORKER_DEADLINE_S: f64 = 17.0;
const RETRYABLE: [&str; 3] = ["inspector_port_busy", "application_not_ready_or_debugging", "attachment_in_progress"];

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn is_state(value: Option<&Value>, states: &[&str]) -> bool {
    value.and_then(Value::as_str).is_some_and(|s| states.contains(&s))
}

/// `value not in RETRYABLE` (a set: an unhashable value is a `TypeError`).
fn retryable(value: Option<&Value>) -> Result<bool, PyErr> {
    match value {
        Some(Value::Str(s)) => Ok(RETRYABLE.contains(&s.as_str())),
        Some(v @ (Value::Array(_) | Value::Object(_))) => {
            Err(PyErr::type_error(format!("unhashable type: '{}'", v.type_name())))
        }
        _ => Ok(false),
    }
}

fn same_target(value: Option<&Value>, wanted: &Value) -> bool {
    value.is_some_and(|v| v.py_eq(wanted))
}

/// `previous` is a final result for the target `wanted`, unless its reason may
/// clear.
fn settled(previous: &Value, wanted: &Value) -> Result<bool, PyErr> {
    Ok(same_target(py::get(previous, "target")?, wanted)
        && is_state(py::get(previous, "state")?, &["attached", "incompatible", "error", "inspector_unclosed"])
        && !retryable(py::get(previous, "reason")?)?)
}

/// The page reported the target's runtime incompatible.
fn runtime_incompatible(runtime: &Value, wanted: &Value) -> Result<bool, PyErr> {
    Ok(same_target(py::get(runtime, "target")?, wanted) && is_state(py::get(runtime, "state")?, &["incompatible"]))
}

/// A page of the target initialized after the last result recorded a page failure.
fn page_recovered(runtime: &Value, previous: &Value, wanted: &Value) -> Result<bool, PyErr> {
    Ok(runtime_ready(runtime, wanted)? && page_failed(previous, wanted)?)
}

fn runtime_ready(runtime: &Value, wanted: &Value) -> Result<bool, PyErr> {
    Ok(same_target(py::get(runtime, "target")?, wanted) && is_state(py::get(runtime, "state")?, &["ready"]))
}

fn page_failed(previous: &Value, wanted: &Value) -> Result<bool, PyErr> {
    Ok(same_target(py::get(previous, "target")?, wanted)
        && is_state(py::get(previous, "state")?, &["incompatible"])
        && is_state(py::get(previous, "reason")?, &["page_initialization_failed"]))
}

/// A result of an observation that does not attach to `target`.
fn unattached(state: &str, reason: &str, target: &Target) -> Value {
    py::obj([
        ("state", Value::from(state)),
        ("reason", Value::from(reason)),
        ("target", target.to_value()),
        ("inspector_closed", Value::Null),
    ])
}

fn caught(error: &PyErr) -> bool {
    error.is_os() || error.is_value() || error.is("TypeError") || error.is("AttributeError")
}

fn try_status(root: &Path, now: f64) -> Result<Value, PyErr> {
    let existing = storage::read(&root.join("status.json"))?;
    if py::get(&existing, "state")?.and_then(Value::as_str) == Some("inspector_unclosed") {
        return unclosed_status(root, &existing);
    }
    let config = storage::read(&root.join("config.json"))?;
    if !py::get(&config, "enabled")?.is_some_and(Value::truthy) {
        return Ok(py::obj([
            ("enabled", Value::Bool(false)),
            ("state", Value::from("disabled")),
            ("reason", Value::Null),
            ("app_version", Value::Null),
            ("inspector_closed", Value::Null),
        ]));
    }
    enabled_status(root, now)
}

/// The unclosed-inspector warning, whatever the configuration says.
fn unclosed_status(root: &Path, existing: &Value) -> Result<Value, PyErr> {
    let enabled = configured_enabled(root)?;
    let app_version = py::get(existing, "app_version")?.cloned().unwrap_or(Value::Null);
    py::spread(
        existing,
        &py::obj([
            ("enabled", Value::Bool(enabled)),
            ("app_version", app_version),
            ("inspector_closed", Value::Bool(false)),
        ]),
    )
}

/// `enabled` of a configuration that may not be read, which counts as enabled.
fn configured_enabled(root: &Path) -> Result<bool, PyErr> {
    match storage::read(&root.join("config.json")).and_then(|c| Ok(py::get(&c, "enabled")?.cloned())) {
        Ok(enabled) => Ok(enabled.is_some_and(|v| v.truthy())),
        Err(e) if caught(&e) => Ok(true),
        Err(e) => Err(e),
    }
}

/// The status of an enabled installation: the last result, unless the web server
/// that would act on it is not running.
fn enabled_status(root: &Path, now: f64) -> Result<Value, PyErr> {
    let mut result = storage::read_json(&root.join("status.json"), Some(py::obj([("state", Value::from("waiting"))])))?;
    let heartbeat = storage::read(&root.join("heartbeat.json"))?;
    if heartbeat_stale(&heartbeat, now)?
        && py::get(&result, "state")?.and_then(Value::as_str) != Some("inspector_unclosed")
    {
        result = py::spread(
            &result,
            &py::obj([("state", Value::from("server_offline")), ("reason", Value::from("web_server_not_running"))]),
        )?;
    }
    py::spread(
        &py::obj([
            ("enabled", Value::Bool(true)),
            ("app_version", Value::Null),
            ("reason", Value::Null),
            ("inspector_closed", Value::Null),
        ]),
        &result,
    )
}

/// The heartbeat says the observer is not running, or was written over 8 s ago.
fn heartbeat_stale(heartbeat: &Value, now: f64) -> Result<bool, PyErr> {
    if !py::get(heartbeat, "running")?.is_some_and(Value::truthy) {
        return Ok(true);
    }
    let updated = py::get(heartbeat, "updated_at")?.cloned().unwrap_or(Value::Int(0));
    Ok(now - py::number(&updated, "-")? > 8.0)
}

/// `status(root)` at `now`: what the settings page and `desktop status` show.
#[must_use]
pub fn status_at(root: &Path, now: f64) -> Value {
    match try_status(root, now) {
        Ok(value) => value,
        // Python catches (OSError, ValueError, TypeError, AttributeError); nothing
        // else can come out of reading these files.
        Err(_) => py::obj([
            ("enabled", Value::Bool(true)),
            ("state", Value::from("error")),
            ("reason", Value::from("invalid_configuration")),
            ("app_version", Value::Null),
            ("inspector_closed", Value::Null),
        ]),
    }
}

/// `status(root)`.
#[must_use]
pub fn status(root: &Path) -> Value {
    status_at(root, crate::host::SystemClock::shared().time())
}

/// A started worker process.
pub trait WorkerProcess: Send {
    /// Waits at most `timeout` for it to exit; true once it has.
    fn wait_for(&mut self, timeout: Duration) -> BoxFuture<'_, bool>;
    /// `kill()`, then waits for it.
    fn kill_and_reap(&mut self) -> BoxFuture<'_, ()>;
}

/// `asyncio.create_subprocess_exec(*args)` of the worker, output discarded.
pub trait Spawner: Send + Sync {
    fn spawn(&self, args: Vec<OsString>) -> BoxFuture<'_, Result<Box<dyn WorkerProcess>, PyErr>>;
}

/// What runs the worker: a program and its leading arguments, to which
/// `--root ROOT (--target JSON | --recover)` is appended.
#[derive(Clone, Debug)]
pub struct WorkerCommand {
    pub program: PathBuf,
    pub prefix: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Variables set for the worker on top of the inherited environment.
    pub env: Vec<(OsString, OsString)>,
}

struct Child(tokio::process::Child);

impl WorkerProcess for Child {
    fn wait_for(&mut self, timeout: Duration) -> BoxFuture<'_, bool> {
        Box::pin(async move { tokio::time::timeout(timeout, self.0.wait()).await.is_ok() })
    }

    fn kill_and_reap(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Only our worker, never KTalk.
            let _ = self.0.start_kill();
            let _ = self.0.wait().await;
        })
    }
}

impl Spawner for WorkerCommand {
    fn spawn(&self, args: Vec<OsString>) -> BoxFuture<'_, Result<Box<dyn WorkerProcess>, PyErr>> {
        Box::pin(async move {
            let mut command = tokio::process::Command::new(&self.program);
            command.args(&self.prefix).args(args).stdin(std::process::Stdio::null());
            command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            command.envs(self.env.iter().map(|(k, v)| (k, v)));
            if let Some(cwd) = &self.cwd {
                command.current_dir(cwd);
            }
            let child = command.spawn().map_err(|e| PyErr::os(&e, Some(&self.program)))?;
            Ok(Box::new(Child(child)) as Box<dyn WorkerProcess>)
        })
    }
}

/// `launch_worker` as the observer calls it.
pub trait Launcher: Send + Sync {
    fn launch<'a>(
        &'a self,
        root: &'a Path,
        target: Option<Target>,
        recover: bool,
    ) -> BoxFuture<'a, Result<Value, PyErr>>;
}

/// The real launcher: [`launch_worker`] through a spawner.
pub struct ProcessLauncher {
    pub spawner: Arc<dyn Spawner>,
    pub host: Host,
}

impl Launcher for ProcessLauncher {
    fn launch<'a>(
        &'a self,
        root: &'a Path,
        target: Option<Target>,
        recover: bool,
    ) -> BoxFuture<'a, Result<Value, PyErr>> {
        Box::pin(launch_worker(&self.host, self.spawner.as_ref(), root, target, recover))
    }
}

fn heartbeat(host: &Host, running: bool) -> Value {
    py::obj([("running", Value::Bool(running)), ("updated_at", Value::Float(host.clock.time()))])
}

/// `launch_worker(root, target, recover)`: one worker within its deadline, the
/// heartbeat kept fresh meanwhile; a worker that left an open journal entry is
/// followed by a recovery, and one that did not report is `worker_failed`.
///
/// # Errors
/// What starting the worker or reading and writing the files fails with.
pub fn launch_worker<'a>(
    host: &'a Host,
    spawner: &'a dyn Spawner,
    root: &'a Path,
    target: Option<Target>,
    recover: bool,
) -> BoxFuture<'a, Result<Value, PyErr>> {
    Box::pin(async move {
        let args = worker_args(root, target.as_ref(), recover);
        let started = host.clock.time();
        let mut child = spawner.spawn(args).await?;
        supervise(host, child.as_mut(), root).await?;
        let operation = storage::read(&root.join("operation.json"))?;
        if !recover && !crate::worker::journal_closed(py::get(&operation, "phase")?) {
            return launch_worker(host, spawner, root, None, true).await;
        }
        let result = storage::read(&root.join("status.json"))?;
        reported(host, root, result, started, target.as_ref())
    })
}

/// `--root ROOT (--recover | --target JSON)`.
fn worker_args(root: &Path, target: Option<&Target>, recover: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--root".into(), root.as_os_str().to_owned()];
    if recover {
        args.push("--recover".into());
    } else if let Some(target) = target {
        args.push("--target".into());
        args.push(py::dumps_ascii(&target.to_value()).into());
    }
    args
}

/// Waits for the worker until its deadline, the heartbeat fresh meanwhile, and
/// kills it past the deadline.
async fn supervise(host: &Host, child: &mut dyn WorkerProcess, root: &Path) -> Result<(), PyErr> {
    let deadline = host.clock.monotonic() + WORKER_DEADLINE_S;
    loop {
        let remaining = deadline - host.clock.monotonic();
        if remaining <= 0.0 {
            child.kill_and_reap().await;
            return Ok(());
        }
        if child.wait_for(Duration::from_secs_f64(remaining.min(1.0))).await {
            return Ok(());
        }
        storage::write_json(&root.join("heartbeat.json"), &heartbeat(host, true))?;
    }
}

/// What the worker reported, `worker_failed` when it left no fresh result.
fn reported(host: &Host, root: &Path, result: Value, started: f64, target: Option<&Target>) -> Result<Value, PyErr> {
    let state = py::get(&result, "state")?.cloned();
    let state = state.as_ref().and_then(Value::as_str);
    if state == Some("inspector_unclosed") || !(reported_before(&result, started)? || state == Some("attaching")) {
        return Ok(result);
    }
    let mut failed = py::obj([
        ("state", Value::from("error")),
        ("reason", Value::from("worker_failed")),
        ("inspector_closed", Value::Null),
        ("updated_at", Value::Float(host.clock.time())),
    ]);
    if let Some(target) = target
        && let Some(map) = failed.as_object_mut()
    {
        map.insert("target", target.to_value());
    }
    storage::write_status(root, &failed)?;
    Ok(failed)
}

/// The result is older than the worker (`result.get("updated_at", 0) < started`).
fn reported_before(result: &Value, started: f64) -> Result<bool, PyErr> {
    let updated = py::get(result, "updated_at")?.cloned().unwrap_or(Value::Int(0));
    match updated {
        Value::Bool(_) | Value::Int(_) | Value::Float(_) => Ok(py::number(&updated, "<")? < started),
        other => Err(PyErr::type_error(format!(
            "'<' not supported between instances of '{}' and 'float'",
            other.type_name()
        ))),
    }
}

/// How a notification command ended.
pub enum NotifyOutcome {
    Exited,
    /// Still running after 2 s: terminated and reaped.
    Terminated,
}

/// Runs a notification command, output discarded, for at most 2 s.
pub trait Notifier: Send + Sync {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, std::io::Result<NotifyOutcome>>;
    /// `shutil.which(name)`.
    fn which(&self, name: &str) -> Option<PathBuf>;
}

/// The desktop's notifications: `osascript` on macOS, `notify-send` on Linux.
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, std::io::Result<NotifyOutcome>> {
        Box::pin(async move {
            let (program, rest) = args.split_first().ok_or_else(|| std::io::Error::other("no command"))?;
            let mut child = tokio::process::Command::new(program)
                .args(rest)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            if tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_ok() {
                return Ok(NotifyOutcome::Exited);
            }
            if let Some(pid) = child.id().and_then(|p| i32::try_from(p).ok()) {
                let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGTERM);
            }
            let _ = child.wait().await;
            Ok(NotifyOutcome::Terminated)
        })
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
    }
}

/// `log_rate_limited` and `log.warning`: lines on stderr, or wherever a test wants
/// them.
pub struct Log {
    sink: Box<dyn Fn(&str) + Send + Sync>,
    limited: Mutex<std::collections::HashMap<String, (f64, u64)>>,
}

impl Log {
    #[must_use]
    pub fn stderr() -> Self {
        Self::to(|line| eprintln!("{line}"))
    }

    pub fn to(sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self { sink: Box::new(sink), limited: Mutex::new(std::collections::HashMap::new()) }
    }

    pub fn line(&self, line: &str) {
        (self.sink)(line);
    }

    /// The first message of a key goes out at once, repeats within `interval_s` are
    /// counted, and the next one out says `(+N suppressed)`.
    pub fn rate_limited(&self, key: &str, message: &str, interval_s: f64, now: f64) {
        let line = {
            let mut state = self.limited.lock().unwrap_or_else(PoisonError::into_inner);
            let suppressed = match state.get_mut(key) {
                Some((last, suppressed)) if now - *last < interval_s => {
                    *suppressed += 1;
                    return;
                }
                Some((_, suppressed)) => *suppressed,
                None => 0,
            };
            state.insert(key.to_owned(), (now, 0));
            if suppressed > 0 { format!("{message} (+{suppressed} suppressed)") } else { message.to_owned() }
        };
        self.line(&line);
    }
}

/// What the observer runs with.
pub struct Context {
    pub host: Host,
    pub launcher: Arc<dyn Launcher>,
    pub notifier: Arc<dyn Notifier>,
    pub log: Arc<Log>,
    pub poll: Duration,
}

impl Context {
    /// The real observer: this system, workers run by `command`.
    #[must_use]
    pub fn system(command: WorkerCommand) -> Self {
        let host = Host::system();
        Self {
            launcher: Arc::new(ProcessLauncher { spawner: Arc::new(command), host: host.clone() }),
            host,
            notifier: Arc::new(SystemNotifier),
            log: Arc::new(Log::stderr()),
            poll: Duration::from_secs_f64(POLL_SECONDS),
        }
    }
}

#[derive(Default)]
struct State {
    lock: Option<fs::File>,
    attempted: HashSet<String>,
    recovered: HashSet<String>,
    notified: HashSet<(String, String)>,
}

/// `DesktopIntegration`: the observer of one data directory's `desktop/`.
pub struct DesktopIntegration {
    pub root: PathBuf,
    context: Context,
    state: tokio::sync::Mutex<State>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl DesktopIntegration {
    #[must_use]
    pub fn new(root: PathBuf, context: Context) -> Arc<Self> {
        Arc::new(Self {
            root,
            context,
            state: tokio::sync::Mutex::new(State::default()),
            stop: watch::channel(false).0,
            task: Mutex::new(None),
        })
    }

    /// `start()`: the polling task, once.
    pub fn start(self: &Arc<Self>) {
        let mut task = self.task.lock().unwrap_or_else(PoisonError::into_inner);
        if task.is_some() {
            return;
        }
        self.stop.send_replace(false);
        let this = Arc::clone(self);
        *task = Some(tokio::spawn(async move { this.run().await }));
    }

    /// `stop()`: the task finishes its tick (an open inspector operation is cleaned
    /// up, never cancelled) and releases the lock.
    pub async fn stop(&self) {
        self.stop.send_replace(true);
        let task = self.task.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// Whether this observer holds `observer.lock`.
    pub async fn holds_lock(&self) -> bool {
        self.state.lock().await.lock.is_some()
    }

    /// Releases `observer.lock` without stopping (tests: "the web server restarted").
    pub async fn release_lock(&self) {
        self.state.lock().await.lock = None;
    }

    fn claim(&self, state: &mut State) -> Result<bool, PyErr> {
        use std::os::unix::fs::OpenOptionsExt as _;
        if state.lock.is_some() {
            return Ok(true);
        }
        recog_core::paths::mkdir_parents(&self.root, 0o700).map_err(|e| PyErr::os(&e, Some(&self.root)))?;
        let path = self.root.join("observer.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| PyErr::os(&e, Some(&path)))?;
        match file.try_lock() {
            Ok(()) => {
                state.lock = Some(file);
                Ok(true)
            }
            Err(fs::TryLockError::WouldBlock) => Ok(false),
            Err(fs::TryLockError::Error(e)) => Err(PyErr::os(&e, None)),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Host) -> Result<T, PyErr> + Send + 'static,
    ) -> Result<T, PyErr> {
        let host = self.context.host.clone();
        tokio::task::spawn_blocking(move || work(&host))
            .await
            .unwrap_or_else(|e| Err(PyErr::new("RuntimeError", e.to_string())))
    }

    /// `_save(result)`: the result with `updated_at`, through `write_status`.
    ///
    /// # Errors
    /// What writing the status fails with.
    pub async fn save(&self, result: &Value) -> Result<(), PyErr> {
        let stamped = py::spread(result, &py::obj([("updated_at", Value::Float(self.context.host.clock.time()))]))?;
        storage::write_status(&self.root, &stamped).map(|_| ())
    }

    /// `_notify(result)`: a desktop notification once per failure reason and target.
    pub async fn notify(&self, result: &Value) {
        let mut state = self.state.lock().await;
        self.notify_with(&mut state, result).await;
    }

    async fn notify_with(&self, state: &mut State, result: &Value) {
        let kind = py::str_at(result, "state");
        if !matches!(kind.as_deref(), Some("error" | "incompatible" | "inspector_unclosed")) {
            return;
        }
        let map = result.as_object();
        let reason = map.and_then(|m| m.get("reason")).cloned().unwrap_or(Value::Null);
        let target = map.and_then(|m| m.get("target")).cloned().unwrap_or(Value::Null);
        let key = (py::dumps(&reason), py::dumps_ascii(&target));
        if !state.notified.insert(key) {
            return;
        }
        let message = if kind.as_deref() == Some("inspector_unclosed") {
            "Не удалось закрыть отладочное подключение. Перезапустите Толк после встречи."
        } else {
            "Не удалось подключить Recog к Толку. Подробности в настройках Recog."
        };
        let args = if self.context.host.platform == Platform::Darwin {
            vec![
                "osascript".to_owned(),
                "-e".to_owned(),
                format!("display notification {} with title \"Recog\"", py::dumps(&Value::from(message))),
            ]
        } else if self.context.notifier.which("notify-send").is_some() {
            vec!["notify-send".to_owned(), "Recog".to_owned(), message.to_owned()]
        } else {
            return;
        };
        if self.context.notifier.run(args).await.is_err() {
            self.context.log.line("Desktop integration notification unavailable");
        }
    }

    /// `_tick()`: one observation.
    ///
    /// # Errors
    /// What the observation fails with; the loop logs it and records `observer_failed`.
    pub async fn tick(&self) -> Result<(), PyErr> {
        let mut state = self.state.lock().await;
        self.tick_with(&mut state).await
    }

    async fn tick_with(&self, state: &mut State) -> Result<(), PyErr> {
        let config = storage::read(&self.root.join("config.json"))?;
        let operation = storage::read(&self.root.join("operation.json"))?;
        if !config.truthy() && !operation.truthy() {
            return Ok(());
        }
        if !self.claim(state)? {
            return Ok(());
        }
        storage::write_json(&self.root.join("heartbeat.json"), &heartbeat(&self.context.host, true))?;
        self.observe(state, &config, &operation).await
    }

    /// The recovery an open journal entry calls for, else the application's main
    /// process when enabled.
    async fn observe(&self, state: &mut State, config: &Value, operation: &Value) -> Result<(), PyErr> {
        if !crate::worker::journal_closed(py::get(operation, "phase")?) {
            return self.recover_operation(state, operation, config).await;
        }
        if !py::get(config, "enabled")?.is_some_and(Value::truthy) {
            return Ok(());
        }
        self.observe_application(state, config).await
    }

    /// Attaches to the one main process of the configured application; with none or
    /// several, waits.
    async fn observe_application(&self, state: &mut State, config: &Value) -> Result<(), PyErr> {
        let raw = crate::worker::path_arg(py::index(config, "app")?)?.to_owned();
        let targets = self
            .blocking(move |host| {
                let app = processes::application_path(host, &raw)?;
                processes::find_targets(host, &app)
            })
            .await?;
        match <[Target; 1]>::try_from(targets) {
            Ok([target]) => self.attach_target(state, target).await,
            Err(targets) => {
                let reason = if targets.is_empty() { Value::Null } else { Value::from("multiple_processes") };
                self.save(&py::obj([
                    ("state", Value::from("waiting")),
                    ("reason", reason),
                    ("inspector_closed", Value::Null),
                ]))
                .await
            }
        }
    }

    async fn recover_operation(&self, state: &mut State, operation: &Value, config: &Value) -> Result<(), PyErr> {
        let target = Target::from_value(py::index(operation, "target")?)?;
        if !self.blocking(move |host| processes::same_process(host, &target, None)).await? {
            return self.close_exited_operation(operation, config).await;
        }
        let token = py::dumps(py::get(operation, "token")?.unwrap_or(&Value::Null));
        if !state.recovered.contains(&token) {
            self.recover_once(state, token).await?;
        }
        Ok(())
    }

    /// The journal entry of a process that is gone, closed: nothing is left open.
    async fn close_exited_operation(&self, operation: &Value, config: &Value) -> Result<(), PyErr> {
        storage::write_json(
            &self.root.join("operation.json"),
            &py::spread(operation, &py::obj([("phase", Value::from("closed"))]))?,
        )?;
        let enabled = py::get(config, "enabled")?.is_some_and(Value::truthy);
        self.save(&py::obj([
            ("state", Value::from(if enabled { "waiting" } else { "disabled" })),
            ("reason", Value::Null),
            ("inspector_closed", Value::Bool(true)),
        ]))
        .await
    }

    /// A recovery worker for the operation `token`, run again only while another
    /// attachment held the lock.
    async fn recover_once(&self, state: &mut State, token: String) -> Result<(), PyErr> {
        let result = self.context.launcher.launch(&self.root, None, true).await?;
        if py::get(&result, "reason")?.and_then(Value::as_str) != Some("attachment_in_progress") {
            state.recovered.insert(token);
        }
        self.notify_with(state, &result).await;
        Ok(())
    }

    async fn attach_target(&self, state: &mut State, target: Target) -> Result<(), PyErr> {
        let previous = storage::read(&self.root.join("status.json"))?;
        if settled(&previous, &target.to_value())? {
            state.attempted.insert(target.key());
        }
        self.apply_runtime(state, &target, &previous).await?;
        if state.attempted.contains(&target.key()) {
            return Ok(());
        }
        if !self.ready_to_attach(state, &target).await? {
            return Ok(());
        }
        self.launch_attachment(state, target).await
    }

    /// What the page reported (`runtime.json`) about this target, over the last
    /// result.
    async fn apply_runtime(&self, state: &mut State, target: &Target, previous: &Value) -> Result<(), PyErr> {
        let wanted = target.to_value();
        let runtime = storage::read(&self.root.join("runtime.json"))?;
        if runtime_incompatible(&runtime, &wanted)? {
            return self.record_incompatible_runtime(state, target, previous, &runtime).await;
        }
        if page_recovered(&runtime, previous, &wanted)? {
            // A later page initialized fine, so the persisted page failure is stale.
            self.save(&py::spread(previous, &py::obj([("state", Value::from("attached")), ("reason", Value::Null)]))?)
                .await?;
        }
        Ok(())
    }

    async fn record_incompatible_runtime(
        &self,
        state: &mut State,
        target: &Target,
        previous: &Value,
        runtime: &Value,
    ) -> Result<(), PyErr> {
        let closed = py::get(previous, "inspector_closed")?.cloned().unwrap_or(Value::Null);
        let result = py::spread(&py::spread(previous, runtime)?, &py::obj([("inspector_closed", closed)]))?;
        self.save(&result).await?;
        self.notify_with(state, &result).await;
        state.attempted.insert(target.key());
        Ok(())
    }

    /// The application ready, the port free and the inspector allowed; otherwise
    /// the reason saved (and an incompatible target not tried again).
    async fn ready_to_attach(&self, state: &mut State, target: &Target) -> Result<bool, PyErr> {
        if let Some(result) = self.retryable_obstacle(target).await? {
            self.save(&result).await?;
            return Ok(false);
        }
        let checked = target.clone();
        if self.blocking(move |host| processes::inspector_supported(host, &checked)).await? {
            return Ok(true);
        }
        let result = unattached("incompatible", "inspector_disabled_or_unverified", target);
        self.save(&result).await?;
        state.attempted.insert(target.key());
        self.notify_with(state, &result).await;
        Ok(false)
    }

    /// An application still starting (or debugged) or a busy port: the result
    /// saying so, tried again on a later tick.
    async fn retryable_obstacle(&self, target: &Target) -> Result<Option<Value>, PyErr> {
        let checked = target.clone();
        if !self.blocking(move |host| processes::ready(host, &checked)).await? {
            return Ok(Some(unattached("waiting", "application_not_ready_or_debugging", target)));
        }
        if !self.blocking(|host| Ok(processes::port_available(host))).await? {
            return Ok(Some(unattached("error", "inspector_port_busy", target)));
        }
        Ok(None)
    }

    /// The worker for `target`, which is not tried again unless it failed for a
    /// reason that may clear.
    async fn launch_attachment(&self, state: &mut State, target: Target) -> Result<(), PyErr> {
        self.save(&py::obj([
            ("state", Value::from("attaching")),
            ("target", target.to_value()),
            ("inspector_closed", Value::Null),
        ]))
        .await?;
        let result = self.context.launcher.launch(&self.root, Some(target.clone()), false).await?;
        if !retryable(py::get(&result, "reason")?)? {
            state.attempted.insert(target.key());
        }
        self.notify_with(state, &result).await;
        Ok(())
    }

    /// `_run()`: tick every 2 s until stopped; a failing tick is logged (rate
    /// limited, a persistent cause fails every tick) and recorded.
    pub async fn run(&self) {
        self.run_ticks(None).await;
    }

    /// [`run`](Self::run) stopping by itself after `limit` ticks (tests); a bounded
    /// run starts afresh after an earlier stop, as `_stop.clear()` lets Python's.
    pub async fn run_ticks(&self, limit: Option<usize>) {
        if limit.is_some() {
            self.stop.send_replace(false);
        }
        let mut stop = self.stop.subscribe();
        let mut ticks = 0usize;
        loop {
            if *stop.borrow() {
                break;
            }
            ticks += 1;
            if limit.is_some_and(|limit| ticks >= limit) {
                self.stop.send_replace(true);
            }
            let mut state = self.state.lock().await;
            if let Err(error) = self.tick_with(&mut state).await {
                self.context.log.rate_limited(
                    &format!("desktop-observer:{}", error.kind),
                    &format!("Desktop observer iteration failed: {}", error.described()),
                    60.0,
                    self.context.host.clock.monotonic(),
                );
                if state.lock.is_some() {
                    let result = py::obj([
                        ("state", Value::from("error")),
                        ("reason", Value::from("observer_failed")),
                        ("inspector_closed", Value::Null),
                    ]);
                    if self.save(&result).await.is_err() {
                        drop(state);
                        break;
                    }
                    self.notify_with(&mut state, &result).await;
                }
            }
            drop(state);
            let _ = tokio::time::timeout(self.context.poll, stop.wait_for(|stopped| *stopped)).await;
        }
        let mut state = self.state.lock().await;
        if state.lock.is_some() {
            let _ = storage::write_json(&self.root.join("heartbeat.json"), &heartbeat(&self.context.host, false));
            state.lock = None;
        }
    }
}
