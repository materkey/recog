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
    let mut rest = url.as_str();
    let mut scheme = String::new();
    if let Some(colon) = rest.find(':')
        && colon > 0
        && rest[..colon].chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && rest[..colon].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        scheme = rest[..colon].to_ascii_lowercase();
        rest = &rest[colon + 1..];
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_owned();
        rest = &after[end..];
        if netloc.contains('[') != netloc.contains(']') {
            return Err(PyErr::value("Invalid IPv6 URL"));
        }
        if netloc.contains('[') && netloc.contains(']') {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let (rest, fragment) = rest.split_once('#').map_or((rest, ""), |(a, b)| (a, b));
    let (_, query) = rest.split_once('?').map_or((rest, ""), |(a, b)| (a, b));
    Ok(Split { scheme, netloc, query: query.to_owned(), fragment: fragment.to_owned() })
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

/// `_check_bracketed_host`: an IPvFuture literal or an IPv6 address (with a zone).
fn check_bracketed_host(hostname: &str) -> Result<(), PyErr> {
    if let Some(future) = hostname.strip_prefix('v') {
        let hex = future.chars().take_while(char::is_ascii_hexdigit).count();
        let rest = &future[hex..];
        let valid = hex > 0 && rest.len() > 1 && rest.starts_with('.') && !rest.contains('\n');
        return if valid { Ok(()) } else { Err(PyErr::value("IPvFuture address is invalid")) };
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
    let Value::Array(items) = targets else { return Err(Failure::attach("ambiguous_inspector")) };
    if items.len() != 1 {
        return Err(Failure::attach("ambiguous_inspector"));
    }
    let url = match py::get(&items[0], "webSocketDebuggerUrl")? {
        None => String::new(),
        Some(Value::Str(s)) => s.clone(),
        Some(other) => return Err(Failure::Other(format!("cannot split a {}", other.type_name()))),
    };
    let parsed = urlsplit(&url)?;
    let (username, password) = parsed.userinfo();
    let invalid = parsed.scheme != "ws"
        || parsed.hostname().as_deref() != Some("127.0.0.1")
        || parsed.port()? != Some(u32::from(host.inspector_port))
        || username.is_some_and(|u| !u.is_empty())
        || password.is_some_and(|p| !p.is_empty())
        || !parsed.query.is_empty()
        || !parsed.fragment.is_empty();
    if invalid {
        return Err(Failure::attach("invalid_inspector_endpoint"));
    }
    Ok(url)
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
        let attempt = async {
            if !processes::owns_inspector(host, &target)? {
                // A disappeared listener is success; an unexpected listener on this
                // PID must not be treated as closed.
                let listening = processes::listeners(host, &target)?;
                return Ok(Some(!listening.iter().any(|(_, port)| *port == host.inspector_port)));
            }
            let url = endpoint(host, &target, Some(&recorded)).await?;
            match tokio::time::timeout(Duration::from_millis(1500), inspector::evaluate(&url, &close_expression()))
                .await
            {
                Ok(result) => result.map(|_| None),
                Err(_) => Err(Failure::Timeout),
            }
        };
        if let Ok(Some(closed)) = attempt.await {
            return Ok(closed);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(false)
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
    let config = storage::read(&root.join("config.json"))?;
    let bundle = storage::validate_bundle(root, &config)?;
    let app = recog_core::paths::normalize(path_arg(py::index(&config, "app")?)?);
    if !processes::same_process(host, target, Some(&app))? {
        return Err(Failure::attach("process_changed"));
    }
    let previous = storage::read(&root.join("operation.json"))?;
    let phase = py::get(&previous, "phase")?;
    if !(phase.is_none_or(Value::is_null) || phase.and_then(Value::as_str) == Some("closed")) {
        if !close_owned(host, &previous).await? {
            return Ok(py::obj([
                ("state", Value::from("inspector_unclosed")),
                ("reason", Value::from("cleanup_failed")),
                ("inspector_closed", Value::Bool(false)),
                ("target", py::index(&previous, "target")?.clone()),
            ]));
        }
        storage::write_json(&root.join("operation.json"), &closed_journal(&previous)?)?;
    }
    check_attachment_ready(host, target)?;
    let mut operation = py::obj([
        ("target", target.to_value()),
        ("phase", Value::from("opening")),
        ("token", Value::from(uuid::Uuid::new_v4().simple().to_string())),
        ("opened_at", Value::Float(host.clock.time())),
    ]);
    storage::write_json(&root.join("operation.json"), &operation)?;
    let mut result = py::obj([
        ("state", Value::from("error")),
        ("reason", Value::from("attachment_failed")),
        ("target", target.to_value()),
    ]);
    let mut signalled = false;
    let digest = py::str_at(&config, "bundle").unwrap_or_default();
    let steps = async {
        if !processes::same_process(host, target, Some(&app))? {
            return Err(Failure::attach("process_changed"));
        }
        (host.signal_usr1)(target.pid)?;
        signalled = true;
        while !processes::owns_inspector(host, target)? {
            if !processes::same_process(host, target, None)? {
                return Err(Failure::attach("process_exited"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let url = endpoint(host, target, None).await?;
        set(&mut operation, "endpoint", Value::from(url.as_str()));
        set(&mut operation, "phase", Value::from("opened"));
        storage::write_json(&root.join("operation.json"), &operation)?;
        bootstrap(&url, root, target, &bundle, &digest).await
    };
    match tokio::time::timeout(ATTACH_TIMEOUT, steps).await {
        Err(_) | Ok(Err(Failure::Timeout)) => set(&mut result, "reason", Value::from("attachment_timeout")),
        Ok(Err(Failure::Attach(reason))) => set(&mut result, "reason", Value::from(reason)),
        Ok(Err(Failure::Other(_))) => {}
        Ok(Ok(response)) => {
            let keys = response.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
            // dict.update(state=..., reason=None, **response) refuses a response that
            // names state or reason itself.
            if !keys.iter().any(|k| k == "state" || k == "reason") {
                set(&mut result, "state", Value::from("attached"));
                set(&mut result, "reason", Value::Null);
                for (key, value) in response.as_object().into_iter().flat_map(|m| m.iter()) {
                    set(&mut result, key, value.clone());
                }
            }
        }
    }
    let closed = if signalled { close_owned(host, &operation).await? } else { true };
    set(&mut result, "inspector_closed", Value::Bool(closed));
    if closed {
        storage::write_json(&root.join("operation.json"), &closed_journal(&operation)?)?;
    } else {
        set(&mut result, "state", Value::from("inspector_unclosed"));
        set(&mut result, "reason", Value::from("cleanup_failed"));
    }
    Ok(result)
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
            let outcome = if recover {
                match recover_operation(host, root).await {
                    Ok(Recovered::Nothing) => {
                        drop(lock);
                        return Ok(py::obj([
                            ("state", Value::from("waiting")),
                            ("inspector_closed", Value::Bool(true)),
                        ]));
                    }
                    Ok(Recovered::Result(result)) => Ok(result),
                    Err(e) => Err(Failure::from(e)),
                }
            } else {
                match target {
                    Some(target) => attach(host, root, target).await,
                    None => Err(Failure::Other("AttributeError: 'NoneType' object has no attribute 'pid'".into())),
                }
            };
            drop(lock);
            match outcome {
                Ok(result) => result,
                Err(Failure::Attach(reason)) => {
                    let state = if reason == "inspector_disabled_or_unverified" { "incompatible" } else { "error" };
                    error_result(state, &reason)
                }
                Err(_) => error_result("error", "invalid_installation"),
            }
        }
    };
    set(&mut result, "updated_at", Value::Float(host.clock.time()));
    if let Some(target) = target
        && let Some(map) = result.as_object_mut()
    {
        map.setdefault("target", target.to_value());
    }
    storage::write_status(root, &result)
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
        storage::write_json(&root.join("operation.json"), &closed_journal(&operation)?)?;
    }
    Ok(Recovered::Result(py::obj([
        ("state", Value::from(if closed { "error" } else { "inspector_unclosed" })),
        ("reason", Value::from(if closed { "worker_interrupted" } else { "cleanup_failed" })),
        ("inspector_closed", Value::Bool(closed)),
        ("target", py::index(&operation, "target")?.clone()),
    ])))
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
    let mut root = None;
    let mut target = None;
    let mut recover = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (name, inline) = arg.split_once('=').map_or((arg.as_str(), None), |(n, v)| (n, Some(v.to_owned())));
        let mut value =
            || inline.clone().or_else(|| iter.next().cloned()).ok_or(format!("argument {name}: expected one argument"));
        match name {
            "--root" => root = Some(PathBuf::from(value()?)),
            "--target" => {
                let raw = value()?;
                target =
                    Some(recog_core::pyjson::loads(&raw).map_err(|_| {
                        format!("argument --target: invalid loads value: {}", recog_core::py::repr(&raw))
                    })?);
            }
            "--recover" => recover = true,
            other => return Err(format!("unrecognized arguments: {other}")),
        }
    }
    let root = root.ok_or("the following arguments are required: --root")?;
    Ok(WorkerArgs { root, target, recover })
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
