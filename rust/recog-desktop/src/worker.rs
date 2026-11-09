//! `recog/desktop/worker.py`: the bounded attachment worker. It never closes an
//! inspector it did not open.
//!
//! An attachment signals the application (SIGUSR1 opens Node's inspector on
//! 127.0.0.1:9229), checks that the listener is that process's, evaluates
//! `bootstrap.js` in its main process and closes the inspector again. Every step is
//! journaled in `operation.json` first, so that an interrupted worker's inspector is
//! closed by the next one (`--recover`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use recog_core::pyjson::Value;

use crate::host::Host;
use crate::inspector::{self, Failure};
use crate::processes::{self, Target};
use crate::py::{self, PyErr};
use crate::storage;

pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);
pub const CLEANUP_TIMEOUT: f64 = 5.0;

/// The code evaluated inside the application, unchanged from the Python module's
/// directory.
pub const BOOTSTRAP_JS: &str = include_str!("../../../recog/desktop/bootstrap.js");

/// `REQUIRE`: `require` in the main process whatever module system it runs.
pub const REQUIRE: &str = "(typeof require === 'function' ? require :
(process.mainModule?.require.bind(process.mainModule) ||
process.getBuiltinModule('module').createRequire(process.execPath)))";

/// `CLOSE`: close the inspector after a moment, unless another session replaced it.
#[must_use]
pub fn close_expression() -> String {
    format!(
        "(() => {{const inspector = {REQUIRE}('node:inspector');
const guard = globalThis.__recogInspectorGuard;
if (guard) {{ clearTimeout(guard.timer); delete globalThis.__recogInspectorGuard; }}
const endpoint = inspector.url();
setTimeout(() => {{ if (inspector.url() === endpoint) inspector.close(); }}, 100); return true;}})()"
    )
}

/// The first expression of an attachment: the process's pid, and a guard closing the
/// inspector after 8 s should the worker die.
#[must_use]
pub fn identity_expression() -> String {
    format!(
        "(() => {{
const inspector = {REQUIRE}('node:inspector');
const previous = globalThis.__recogInspectorGuard;
if (previous) clearTimeout(previous.timer);
const guard = globalThis.__recogInspectorGuard = {{endpoint: inspector.url()}};
guard.timer = setTimeout(() => {{
  if (globalThis.__recogInspectorGuard === guard) {{
    delete globalThis.__recogInspectorGuard;
    if (inspector.url() === guard.endpoint) inspector.close();
  }}
}}, 8000).unref();
return {{pid: process.pid}};}})()"
    )
}

/// `attachment_lock()` failing: busy (`BlockingIOError`) or anything else.
pub enum LockError {
    Busy,
    Failed(PyErr),
}

/// `attachment_lock()`: one attachment at a time for this user,
/// `~/.cache/recog/desktop-attach.lock` held with `flock`.
///
/// # Errors
/// [`LockError::Busy`] while another worker holds it.
pub fn attachment_lock(host: &Host) -> Result<fs::File, LockError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let directory = host.home.join(".cache").join("recog");
    recog_core::paths::mkdir_parents(&directory, 0o700)
        .map_err(|e| LockError::Failed(PyErr::os(&e, Some(&directory))))?;
    let path = directory.join("desktop-attach.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|e| LockError::Failed(PyErr::os(&e, Some(&path))))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(LockError::Busy),
        Err(fs::TryLockError::Error(e)) => Err(LockError::Failed(PyErr::os(&e, None))),
    }
}

/// `urlsplit(url)` reduced to what `inspector_url` checks.
struct Split {
    scheme: String,
    netloc: String,
    query: String,
    fragment: String,
}

fn urlsplit(url: &str) -> Result<Split, PyErr> {
    let url: String =
        url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let (scheme, rest) = split_scheme(&url);
    let (netloc, rest) = split_netloc(rest)?;
    let (rest, fragment) = rest.split_once('#').map_or((rest, ""), |(a, b)| (a, b));
    let (_, query) = rest.split_once('?').map_or((rest, ""), |(a, b)| (a, b));
    Ok(Split { scheme, netloc, query: query.to_owned(), fragment: fragment.to_owned() })
}

/// The lowercased scheme and what follows its colon; none when the text before the
/// first colon is not a scheme.
fn split_scheme(url: &str) -> (String, &str) {
    if let Some(colon) = url.find(':')
        && colon > 0
        && url[..colon].chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && url[..colon].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return (url[..colon].to_ascii_lowercase(), &url[colon + 1..]);
    }
    (String::new(), url)
}

/// The netloc after `//` and the rest; brackets must pair and hold a valid host.
fn split_netloc(rest: &str) -> Result<(String, &str), PyErr> {
    let Some(after) = rest.strip_prefix("//") else { return Ok((String::new(), rest)) };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let netloc = &after[..end];
    if netloc.contains('[') != netloc.contains(']') {
        return Err(PyErr::value("Invalid IPv6 URL"));
    }
    if netloc.contains('[') && netloc.contains(']') {
        check_bracketed_netloc(netloc)?;
    }
    Ok((netloc.to_owned(), &after[end..]))
}

/// `_check_bracketed_netloc`: nothing before the bracket, only a port after it, and
/// the host itself checked (also when the brackets were in the user part).
fn check_bracketed_netloc(netloc: &str) -> Result<(), PyErr> {
    let host_port = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let hostname = match host_port.split_once('[') {
        Some((before, bracketed)) => {
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !before.is_empty() || (!port.is_empty() && !port.starts_with(':')) {
                return Err(PyErr::value("Invalid IPv6 URL"));
            }
            hostname
        }
        None => host_port.split_once(':').map_or(host_port, |(h, _)| h),
    };
    check_bracketed_host(hostname)
}

/// `re.match(r"\Av[a-fA-F0-9]+\..+\Z", hostname)` after its `v`.
fn check_ipv_future(future: &str) -> Result<(), PyErr> {
    let hex = future.chars().take_while(char::is_ascii_hexdigit).count();
    let rest = &future[hex..];
    let valid = hex > 0 && rest.len() > 1 && rest.starts_with('.') && !rest.contains('\n');
    if valid { Ok(()) } else { Err(PyErr::value("IPvFuture address is invalid")) }
}

/// `_check_bracketed_host`: an IPvFuture literal or an IPv6 address (with a zone).
fn check_bracketed_host(hostname: &str) -> Result<(), PyErr> {
    if let Some(future) = hostname.strip_prefix('v') {
        return check_ipv_future(future);
    }
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err(PyErr::value("An IPv4 address cannot be in brackets"));
    }
    let (address, zone) = hostname.split_once('%').map_or((hostname, None), |(a, z)| (a, Some(z)));
    let zone_valid = zone.is_none_or(|z| !z.is_empty() && !z.contains('%'));
    if zone_valid && address.parse::<std::net::Ipv6Addr>().is_ok() {
        return Ok(());
    }
    Err(PyErr::value(format!("{} does not appear to be an IPv4 or IPv6 address", recog_core::py::repr(hostname))))
}

impl Split {
    fn userinfo(&self) -> (Option<String>, Option<String>) {
        match self.netloc.rsplit_once('@') {
            None => (None, None),
            Some((userinfo, _)) => match userinfo.split_once(':') {
                Some((user, password)) => (Some(user.to_owned()), Some(password.to_owned())),
                None => (Some(userinfo.to_owned()), None),
            },
        }
    }

    fn hostinfo(&self) -> (String, Option<String>) {
        let hostinfo = self.netloc.rsplit_once('@').map_or(self.netloc.as_str(), |(_, h)| h);
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (hostname, port.split_once(':').map_or("", |(_, p)| p))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (hostname.to_owned(), (!port.is_empty()).then(|| port.to_owned()))
    }

    fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            return None;
        }
        Some(match hostname.split_once('%') {
            Some((host, zone)) => format!("{}%{zone}", host.to_lowercase()),
            None => hostname.to_lowercase(),
        })
    }

    fn port(&self) -> Result<Option<u32>, PyErr> {
        let Some(port) = self.hostinfo().1 else { return Ok(None) };
        if !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(PyErr::value(format!(
                "Port could not be cast to integer value as {}",
                recog_core::py::repr(&port)
            )));
        }
        match port.parse::<u32>() {
            Ok(n) if n <= 65535 => Ok(Some(n)),
            _ => Err(PyErr::value("Port out of range 0-65535")),
        }
    }
}

/// `inspector_url(targets)`: the one target's WebSocket URL, which must be plain
/// `ws://127.0.0.1:<inspector port>/<path>`.
///
/// # Errors
/// `ambiguous_inspector`, `invalid_inspector_endpoint`, or `Other` for a malformed
/// list.
pub fn inspector_url(host: &Host, targets: &Value) -> Result<String, Failure> {
    let url = debugger_url(targets)?;
    if !plain_endpoint(&urlsplit(&url)?, host.inspector_port)? {
        return Err(Failure::attach("invalid_inspector_endpoint"));
    }
    Ok(url)
}

/// The `webSocketDebuggerUrl` of the one target listed.
fn debugger_url(targets: &Value) -> Result<String, Failure> {
    let Value::Array(items) = targets else { return Err(Failure::attach("ambiguous_inspector")) };
    if items.len() != 1 {
        return Err(Failure::attach("ambiguous_inspector"));
    }
    match py::get(&items[0], "webSocketDebuggerUrl")? {
        None => Ok(String::new()),
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(other) => Err(Failure::Other(format!("cannot split a {}", other.type_name()))),
    }
}

/// `ws://127.0.0.1:<port>/...` with neither credentials, a query nor a fragment.
fn plain_endpoint(parsed: &Split, port: u16) -> Result<bool, PyErr> {
    let (username, password) = parsed.userinfo();
    Ok(parsed.scheme == "ws"
        && parsed.hostname().as_deref() == Some("127.0.0.1")
        && parsed.port()? == Some(u32::from(port))
        && username.is_none_or(|u| u.is_empty())
        && password.is_none_or(|p| p.is_empty())
        && parsed.query.is_empty()
        && parsed.fragment.is_empty())
}

/// `endpoint(target, expected)`: the inspector's URL, discovered only while the
/// listener is the target's on both sides of the request.
///
/// # Errors
/// `inspector_owner_mismatch`, `inspector_session_changed` and what
/// [`inspector_url`] and the request fail with.
pub async fn endpoint(host: &Host, target: &Target, expected: Option<&Value>) -> Result<String, Failure> {
    if !processes::owns_inspector(host, target)? {
        return Err(Failure::attach("inspector_owner_mismatch"));
    }
    let targets = inspector::json_list(host.inspector_port).await?;
    let url = inspector_url(host, &targets)?;
    if let Some(expected) = expected.filter(|e| e.truthy())
        && !expected.py_eq(&Value::from(url.as_str()))
    {
        return Err(Failure::attach("inspector_session_changed"));
    }
    if !processes::owns_inspector(host, target)? {
        return Err(Failure::attach("inspector_owner_mismatch"));
    }
    Ok(url)
}

/// `close_owned(operation)`: close the inspector the journal says this integration
/// opened. Success when the process is gone or its listener disappeared; an
/// unexpected listener on the pid is not closed.
///
/// # Errors
/// A journal without a usable target, and what checking the process lets through.
pub async fn close_owned(host: &Host, operation: &Value) -> Result<bool, PyErr> {
    let target = Target::from_value(py::index(operation, "target")?)?;
    if !processes::same_process(host, &target, None)? {
        return Ok(true);
    }
    let recorded = py::get(operation, "endpoint")?.filter(|e| e.truthy()).cloned();
    let Some(recorded) = recorded else {
        // A pre-signal journal entry cannot prove ownership of a later user
        // debugger. Never discover a new endpoint during recovery.
        return Ok(false);
    };
    let deadline = host.clock.monotonic() + CLEANUP_TIMEOUT;
    while host.clock.monotonic() < deadline {
        if let Ok(Some(closed)) = close_attempt(host, &target, &recorded).await {
            return Ok(closed);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(false)
}

/// One try of [`close_owned`]: whether the inspector is gone (None while it is
/// still to be closed, after asking it to close).
async fn close_attempt(host: &Host, target: &Target, recorded: &Value) -> Result<Option<bool>, Failure> {
    if !processes::owns_inspector(host, target)? {
        // A disappeared listener is success; an unexpected listener on this
        // PID must not be treated as closed.
        let listening = processes::listeners(host, target)?;
        return Ok(Some(!listening.iter().any(|(_, port)| *port == host.inspector_port)));
    }
    let url = endpoint(host, target, Some(recorded)).await?;
    match tokio::time::timeout(Duration::from_millis(1500), inspector::evaluate(&url, &close_expression())).await {
        Ok(result) => result.map(|_| None),
        Err(_) => Err(Failure::Timeout),
    }
}

/// `bootstrap(url, root, target, bundle, digest)`: confirm the inspector is the
/// target's (its pid), then evaluate `bootstrap.js` with the snapshot, its digest,
/// the runtime status file and the target.
///
/// # Errors
/// `inspector_identity_mismatch`, `invalid_bootstrap_response` and what
/// [`inspector::evaluate`] fails with.
pub async fn bootstrap(url: &str, root: &Path, target: &Target, bundle: &Path, digest: &str) -> Result<Value, Failure> {
    let identity = inspector::evaluate(url, &identity_expression()).await?;
    if !identity.py_eq(&py::obj([("pid", Value::Int(i128::from(target.pid)))])) {
        return Err(Failure::attach("inspector_identity_mismatch"));
    }
    let expression = format!(
        "{BOOTSTRAP_JS}\nattachDesktop({},{},{},{})",
        py::dumps_ascii(&Value::from(bundle.to_string_lossy().as_ref())),
        py::dumps_ascii(&Value::from(digest)),
        py::dumps_ascii(&Value::from(root.join("runtime.json").to_string_lossy().as_ref())),
        py::dumps_ascii(&target.to_value()),
    );
    let response = inspector::evaluate(url, &expression).await?;
    let has_version = response.as_object().and_then(|m| m.get("app_version")).is_some_and(Value::truthy);
    if !has_version {
        return Err(Failure::attach("invalid_bootstrap_response"));
    }
    Ok(response)
}

/// `check_attachment_ready(target)`.
///
/// # Errors
/// `inspector_port_busy`, `application_not_ready_or_debugging`,
/// `inspector_disabled_or_unverified`, or what reading the process lets through.
pub fn check_attachment_ready(host: &Host, target: &Target) -> Result<(), Failure> {
    if !processes::port_available(host) {
        return Err(Failure::attach("inspector_port_busy"));
    }
    if !processes::ready(host, target)? {
        return Err(Failure::attach("application_not_ready_or_debugging"));
    }
    if !processes::inspector_supported(host, target)? {
        return Err(Failure::attach("inspector_disabled_or_unverified"));
    }
    Ok(())
}

fn set(map: &mut Value, key: &str, value: Value) {
    if let Some(map) = map.as_object_mut() {
        map.insert(key, value);
    }
}

fn closed_journal(operation: &Value) -> Result<Value, PyErr> {
    py::spread(operation, &py::obj([("phase", Value::from("closed"))]))
}

/// `Path(value)` of a configured path.
///
/// # Errors
/// `TypeError` for a value that is not a string.
pub fn path_arg(value: &Value) -> Result<&str, PyErr> {
    value.as_str().ok_or_else(|| {
        PyErr::type_error(format!(
            "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
            value.type_name()
        ))
    })
}

/// `attach(root, target)`: one attachment, cleanup matching signal ownership.
///
/// # Errors
/// `Failure::Attach` for the checks before the signal, `Other` for a broken
/// installation; after the signal everything ends in the returned result.
pub async fn attach(host: &Host, root: &Path, target: &Target) -> Result<Value, Failure> {
    let attempt = Attempt::checked(host, root, target)?;
    if let Some(unclosed) = attempt.close_previous().await? {
        return Ok(unclosed);
    }
    check_attachment_ready(host, target)?;
    let mut operation = attempt.open()?;
    let mut result = py::obj([
        ("state", Value::from("error")),
        ("reason", Value::from("attachment_failed")),
        ("target", target.to_value()),
    ]);
    let mut signalled = false;
    let outcome = tokio::time::timeout(ATTACH_TIMEOUT, attempt.steps(&mut operation, &mut signalled)).await;
    record_outcome(&mut result, outcome);
    attempt.close(&operation, &mut result, signalled).await?;
    Ok(result)
}

/// `phase` of a journal entry with nothing left to close.
pub(crate) fn journal_closed(phase: Option<&Value>) -> bool {
    phase.is_none_or(Value::is_null) || phase.and_then(Value::as_str) == Some("closed")
}

/// `operation.json` rewritten with the operation closed.
fn close_journal(root: &Path, operation: &Value) -> Result<(), PyErr> {
    storage::write_json(&root.join("operation.json"), &closed_journal(operation)?)
}

/// One attachment once the installation and the process are checked.
struct Attempt<'a> {
    host: &'a Host,
    root: &'a Path,
    target: &'a Target,
    app: PathBuf,
    bundle: PathBuf,
    digest: String,
}

impl<'a> Attempt<'a> {
    /// The configured snapshot and application, `target` still its main process.
    fn checked(host: &'a Host, root: &'a Path, target: &'a Target) -> Result<Self, Failure> {
        let config = storage::read(&root.join("config.json"))?;
        let bundle = storage::validate_bundle(root, &config)?;
        let app = recog_core::paths::normalize(path_arg(py::index(&config, "app")?)?);
        if !processes::same_process(host, target, Some(&app))? {
            return Err(Failure::attach("process_changed"));
        }
        let digest = py::str_at(&config, "bundle").unwrap_or_default();
        Ok(Self { host, root, target, app, bundle, digest })
    }

    /// The previous operation's inspector closed, or the result saying it could not
    /// be.
    async fn close_previous(&self) -> Result<Option<Value>, Failure> {
        let previous = storage::read(&self.root.join("operation.json"))?;
        if journal_closed(py::get(&previous, "phase")?) {
            return Ok(None);
        }
        if !close_owned(self.host, &previous).await? {
            return Ok(Some(py::obj([
                ("state", Value::from("inspector_unclosed")),
                ("reason", Value::from("cleanup_failed")),
                ("inspector_closed", Value::Bool(false)),
                ("target", py::index(&previous, "target")?.clone()),
            ])));
        }
        close_journal(self.root, &previous)?;
        Ok(None)
    }

    /// The journal entry of this operation, written before the signal.
    fn open(&self) -> Result<Value, PyErr> {
        let operation = py::obj([
            ("target", self.target.to_value()),
            ("phase", Value::from("opening")),
            ("token", Value::from(uuid::Uuid::new_v4().simple().to_string())),
            ("opened_at", Value::Float(self.host.clock.time())),
        ]);
        storage::write_json(&self.root.join("operation.json"), &operation)?;
        Ok(operation)
    }

    /// Signal, wait for the inspector, journal its endpoint, bootstrap.
    async fn steps(&self, operation: &mut Value, signalled: &mut bool) -> Result<Value, Failure> {
        if !processes::same_process(self.host, self.target, Some(&self.app))? {
            return Err(Failure::attach("process_changed"));
        }
        (self.host.signal_usr1)(self.target.pid)?;
        *signalled = true;
        self.await_inspector().await?;
        let url = endpoint(self.host, self.target, None).await?;
        set(operation, "endpoint", Value::from(url.as_str()));
        set(operation, "phase", Value::from("opened"));
        storage::write_json(&self.root.join("operation.json"), operation)?;
        bootstrap(&url, self.root, self.target, &self.bundle, &self.digest).await
    }

    /// Until the target listens on the inspector port, while it runs.
    async fn await_inspector(&self) -> Result<(), Failure> {
        while !processes::owns_inspector(self.host, self.target)? {
            if !processes::same_process(self.host, self.target, None)? {
                return Err(Failure::attach("process_exited"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    /// The inspector closed when this operation opened it, the result saying so.
    async fn close(&self, operation: &Value, result: &mut Value, signalled: bool) -> Result<(), Failure> {
        let closed = if signalled { close_owned(self.host, operation).await? } else { true };
        set(result, "inspector_closed", Value::Bool(closed));
        if closed {
            close_journal(self.root, operation)?;
        } else {
            set(result, "state", Value::from("inspector_unclosed"));
            set(result, "reason", Value::from("cleanup_failed"));
        }
        Ok(())
    }
}

/// The result as the steps left it, the deadline elapsing as a timeout.
fn record_outcome(result: &mut Value, outcome: Result<Result<Value, Failure>, tokio::time::error::Elapsed>) {
    match outcome {
        Err(_) | Ok(Err(Failure::Timeout)) => set(result, "reason", Value::from("attachment_timeout")),
        Ok(Err(Failure::Attach(reason))) => set(result, "reason", Value::from(reason)),
        Ok(Err(Failure::Other(_))) => {}
        Ok(Ok(response)) => record_response(result, &response),
    }
}

/// `result.update(state="attached", reason=None, **response)`.
fn record_response(result: &mut Value, response: &Value) {
    let fields = response.as_object();
    // dict.update(state=..., reason=None, **response) refuses a response that
    // names state or reason itself.
    if fields.is_some_and(|m| m.keys().any(|k| k == "state" || k == "reason")) {
        return;
    }
    set(result, "state", Value::from("attached"));
    set(result, "reason", Value::Null);
    for (key, value) in fields.into_iter().flat_map(|m| m.iter()) {
        set(result, key, value.clone());
    }
}

fn error_result(state: &str, reason: &str) -> Value {
    py::obj([("state", Value::from(state)), ("reason", Value::from(reason)), ("inspector_closed", Value::Null)])
}

/// `run(root, target, recover)`: one attachment (or the recovery of an interrupted
/// one) under the attachment lock, its result written as the status.
///
/// # Errors
/// What reading or writing the status fails with, which Python does not catch either.
pub async fn run(host: &Host, root: &Path, target: Option<&Target>, recover: bool) -> Result<Value, PyErr> {
    let mut result = match attachment_lock(host) {
        Err(LockError::Busy) => {
            let existing = storage::read(&root.join("status.json"))?;
            if py::get(&existing, "state")?.and_then(Value::as_str) == Some("inspector_unclosed") {
                return Ok(existing);
            }
            error_result("error", "attachment_in_progress")
        }
        Err(LockError::Failed(_)) => error_result("error", "invalid_installation"),
        Ok(lock) => {
            let outcome = under_lock(host, root, target, recover).await;
            drop(lock);
            let Some(outcome) = outcome else {
                return Ok(py::obj([("state", Value::from("waiting")), ("inspector_closed", Value::Bool(true))]));
            };
            outcome_result(outcome)
        }
    };
    stamp(host, &mut result, target);
    storage::write_status(root, &result)
}

/// The attachment or the recovery the lock was taken for; None when there was
/// nothing to recover.
async fn under_lock(
    host: &Host,
    root: &Path,
    target: Option<&Target>,
    recover: bool,
) -> Option<Result<Value, Failure>> {
    if !recover {
        return Some(match target {
            Some(target) => attach(host, root, target).await,
            None => Err(Failure::Other("AttributeError: 'NoneType' object has no attribute 'pid'".into())),
        });
    }
    match recover_operation(host, root).await {
        Ok(Recovered::Nothing) => None,
        Ok(Recovered::Result(result)) => Some(Ok(result)),
        Err(e) => Some(Err(Failure::from(e))),
    }
}

/// The status of a finished attachment or recovery.
fn outcome_result(outcome: Result<Value, Failure>) -> Value {
    match outcome {
        Ok(result) => result,
        Err(Failure::Attach(reason)) => {
            let state = if reason == "inspector_disabled_or_unverified" { "incompatible" } else { "error" };
            error_result(state, &reason)
        }
        Err(_) => error_result("error", "invalid_installation"),
    }
}

/// `updated_at`, and the target unless the result names one.
fn stamp(host: &Host, result: &mut Value, target: Option<&Target>) {
    set(result, "updated_at", Value::Float(host.clock.time()));
    if let Some(target) = target
        && let Some(map) = result.as_object_mut()
    {
        map.setdefault("target", target.to_value());
    }
}

enum Recovered {
    Nothing,
    Result(Value),
}

async fn recover_operation(host: &Host, root: &Path) -> Result<Recovered, PyErr> {
    let operation = storage::read(&root.join("operation.json"))?;
    if !operation.truthy() || py::get(&operation, "phase")?.and_then(Value::as_str) == Some("closed") {
        return Ok(Recovered::Nothing);
    }
    let closed = close_owned(host, &operation).await?;
    if closed {
        close_journal(root, &operation)?;
    }
    Ok(Recovered::Result(recovery_result(closed, py::index(&operation, "target")?.clone())))
}

/// What the recovery of an interrupted worker's operation reports.
fn recovery_result(closed: bool, target: Value) -> Value {
    py::obj([
        ("state", Value::from(if closed { "error" } else { "inspector_unclosed" })),
        ("reason", Value::from(if closed { "worker_interrupted" } else { "cleanup_failed" })),
        ("inspector_closed", Value::Bool(closed)),
        ("target", target),
    ])
}

/// The worker's arguments: `--root ROOT [--target JSON] [--recover]`.
pub struct WorkerArgs {
    pub root: PathBuf,
    pub target: Option<Value>,
    pub recover: bool,
}

/// Parses the worker's arguments as its argparse does (`--opt value` or `--opt=value`).
///
/// # Errors
/// What argparse would print before exiting with 2.
pub fn parse_args(args: &[String]) -> Result<WorkerArgs, String> {
    let mut parsed = ParsedArgs::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        parsed.take(arg, &mut iter)?;
    }
    let root = parsed.root.ok_or("the following arguments are required: --root")?;
    Ok(WorkerArgs { root, target: parsed.target, recover: parsed.recover })
}

/// The worker's arguments while they are read, `--root` not yet required.
#[derive(Default)]
struct ParsedArgs {
    root: Option<PathBuf>,
    target: Option<Value>,
    recover: bool,
}

impl ParsedArgs {
    /// One option, its value inline (`--opt=value`) or the next argument.
    fn take<'a>(&mut self, arg: &str, rest: &mut impl Iterator<Item = &'a String>) -> Result<(), String> {
        let (name, inline) = arg.split_once('=').map_or((arg, None), |(n, v)| (n, Some(v.to_owned())));
        let mut value =
            || inline.clone().or_else(|| rest.next().cloned()).ok_or(format!("argument {name}: expected one argument"));
        match name {
            "--root" => self.root = Some(PathBuf::from(value()?)),
            "--target" => self.target = Some(value().and_then(|raw| target_arg(&raw))?),
            "--recover" => self.recover = true,
            other => return Err(format!("unrecognized arguments: {other}")),
        }
        Ok(())
    }
}

/// `--target`'s `type=json.loads`.
fn target_arg(raw: &str) -> Result<Value, String> {
    recog_core::pyjson::loads(raw)
        .map_err(|_| format!("argument --target: invalid loads value: {}", recog_core::py::repr(raw)))
}

/// `python -m recog.desktop.worker`: exit 0 when attached or nothing was left to
/// recover, 1 otherwise, 2 for bad arguments.
pub async fn main(host: &Host, args: &[String]) -> i32 {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("usage: worker.py [-h] --root ROOT [--target TARGET] [--recover]\nworker.py: error: {message}");
            return 2;
        }
    };
    let target = match args.target.as_ref().filter(|t| t.truthy()).map(Target::from_value).transpose() {
        Ok(target) => target,
        Err(e) => {
            eprintln!("{}", e.described());
            return 1;
        }
    };
    match run(host, &args.root, target.as_ref(), args.recover).await {
        Ok(result) => {
            let state = py::str_at(&result, "state");
            i32::from(!matches!(state.as_deref(), Some("attached" | "waiting")))
        }
        Err(e) => {
            eprintln!("{}", e.described());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host::with_table(crate::system::table())
    }

    #[test]
    fn untrusted_inspector_discovery_when_validated_then_rejected() {
        // given
        let cases = [
            ("{}", "ambiguous_inspector"),
            ("[]", "ambiguous_inspector"),
            ("[{}, {}]", "ambiguous_inspector"),
            (r#"[{"webSocketDebuggerUrl": "http://127.0.0.1:9229/x"}]"#, "invalid_inspector_endpoint"),
            (r#"[{"webSocketDebuggerUrl": "ws://example.com:9229/x"}]"#, "invalid_inspector_endpoint"),
            (r#"[{"webSocketDebuggerUrl": "ws://127.0.0.1:9999/x"}]"#, "invalid_inspector_endpoint"),
            (r#"[{"webSocketDebuggerUrl": "ws://user@127.0.0.1:9229/x"}]"#, "invalid_inspector_endpoint"),
            (r#"[{"webSocketDebuggerUrl": "ws://:secret@127.0.0.1:9229/x"}]"#, "invalid_inspector_endpoint"),
            (r#"[{"webSocketDebuggerUrl": "ws://127.0.0.1:9229/x?query"}]"#, "invalid_inspector_endpoint"),
            (r#"[{"webSocketDebuggerUrl": "ws://127.0.0.1:9229/x#fragment"}]"#, "invalid_inspector_endpoint"),
        ];
        for (targets, reason) in cases {
            // when
            let result = inspector_url(&host(), &recog_core::pyjson::loads(targets).unwrap());
            // then
            assert_eq!(result, Err(Failure::attach(reason)), "{targets}");
        }
        let valid = r#"[{"webSocketDebuggerUrl": "ws://127.0.0.1:9229/abc"}]"#;
        assert_eq!(
            inspector_url(&host(), &recog_core::pyjson::loads(valid).unwrap()).unwrap(),
            "ws://127.0.0.1:9229/abc"
        );
        let bad_port = r#"[{"webSocketDebuggerUrl": "ws://127.0.0.1:92a/x"}]"#;
        assert!(matches!(
            inspector_url(&host(), &recog_core::pyjson::loads(bad_port).unwrap()),
            Err(Failure::Other(_))
        ));
    }

    #[test]
    fn worker_arguments_when_parsed_then_like_its_argparse() {
        // given
        let args: Vec<String> = ["--root", "/r", "--target={\"pid\": 1}", "--recover"].map(str::to_owned).to_vec();
        // when
        let parsed = parse_args(&args).unwrap();
        // then
        assert_eq!(parsed.root, PathBuf::from("/r"));
        assert!(parsed.recover);
        assert_eq!(parsed.target, Some(py::obj([("pid", Value::Int(1))])));
        assert!(parse_args(&[]).is_err());
    }
}
