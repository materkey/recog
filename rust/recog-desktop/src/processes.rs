//! `recog/desktop/processes.py`: find only the explicitly selected application's main
//! process, never helpers, and check what may be done with it.
//!
//! Each function lets through the errors its Python counterpart lets through (an
//! `except` tuple catches only its classes), so that a caller fails, retries or
//! reports where Python's does.

use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use recog_core::pyjson::Value;
use recog_core::{paths, py as core_py};

use crate::host::{Host, Platform};
use crate::py::{self, PyErr};

/// `Target`: one process, identified by its pid, start time and executable.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub pid: i64,
    pub started: f64,
    pub executable: String,
}

impl Target {
    /// `key`: `"<pid>:<started with 6 decimals>"`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{:.6}", self.pid, self.started)
    }

    /// `to_dict()`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        py::obj([
            ("pid", Value::Int(i128::from(self.pid))),
            ("started", Value::Float(self.started)),
            ("executable", Value::from(self.executable.as_str())),
        ])
    }

    /// `Target.from_dict(data)`: `int()`, `float()` and `str()` of the three fields.
    ///
    /// # Errors
    /// What those conversions and the lookups raise.
    pub fn from_value(data: &Value) -> Result<Self, PyErr> {
        let pid = py::to_int(py::index(data, "pid")?)?;
        let pid = i64::try_from(pid).map_err(|_| PyErr::new("OverflowError", "Python int too large to convert"))?;
        let started = py::to_float(py::index(data, "started")?)?;
        let executable = py::index(data, "executable")?.py_str();
        Ok(Self { pid, started, executable })
    }
}

/// `Path.parent`: the path itself for a root, `.` for a bare name.
fn parent(path: &Path) -> PathBuf {
    match path.parent() {
        None => path.to_path_buf(),
        Some(p) if p.as_os_str().is_empty() => PathBuf::from("."),
        Some(p) => p.to_path_buf(),
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `PurePath.stem`: the name without its last suffix, a suffix being a dot neither
/// first nor last.
fn stem_of(path: &Path) -> String {
    let name = name_of(path);
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => name[..i].to_owned(),
        _ => name,
    }
}

/// `posixpath.join(a, b)`.
fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        b.to_owned()
    } else if a.is_empty() || a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `posixpath.split(p)`.
fn split(p: &str) -> (String, String) {
    let i = p.rfind('/').map_or(0, |i| i + 1);
    let (head, tail) = (&p[..i], &p[i..]);
    let head = if !head.is_empty() && head.chars().any(|c| c != '/') { head.trim_end_matches('/') } else { head };
    (head.to_owned(), tail.to_owned())
}

/// `posixpath.normpath(p)` of an absolute path.
fn normpath(p: &str) -> String {
    let initial = if p.starts_with("//") && !p.starts_with("///") { 2 } else { usize::from(p.starts_with('/')) };
    let mut parts: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (initial == 0 && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(comp);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let joined = format!("{}{}", "/".repeat(initial), parts.join("/"));
    if joined.is_empty() { ".".to_owned() } else { joined }
}

/// `Path(path).resolve(strict=True)`: `os.path.realpath(path, strict=True)`, every
/// component required to exist, failing on the first that does not (named as
/// resolved so far); a symlink loop is a `RuntimeError`.
///
/// # Errors
/// The `OSError` of the missing or unreadable component, or the loop.
pub fn resolve_strict(path: &Path) -> Result<PathBuf, PyErr> {
    fn join_real(mut path: String, rest: &str, seen: &mut HashMap<String, Option<String>>) -> Result<String, PyErr> {
        let mut rest = rest;
        if let Some(stripped) = rest.strip_prefix('/') {
            rest = stripped;
            path = "/".to_owned();
        }
        while !rest.is_empty() {
            let (name, remainder) = rest.split_once('/').unwrap_or((rest, ""));
            rest = remainder;
            if name.is_empty() || name == "." {
                continue;
            }
            if name == ".." {
                if path.is_empty() {
                    path = "..".to_owned();
                } else {
                    let (head, tail) = split(&path);
                    path = if tail == ".." { join(&join(&head, ".."), "..") } else { head };
                }
                continue;
            }
            let newpath = join(&path, name);
            let meta = fs::symlink_metadata(&newpath).map_err(|e| PyErr::os(&e, Some(Path::new(&newpath))))?;
            if !meta.file_type().is_symlink() {
                path = newpath;
                continue;
            }
            match seen.get(&newpath) {
                Some(Some(resolved)) => {
                    path = resolved.clone();
                    continue;
                }
                Some(None) => {
                    return Err(PyErr::new("RuntimeError", format!("Symlink loop from {}", core_py::repr(&newpath))));
                }
                None => {}
            }
            seen.insert(newpath.clone(), None);
            let link = fs::read_link(&newpath).map_err(|e| PyErr::os(&e, Some(Path::new(&newpath))))?;
            path = join_real(path, &link.to_string_lossy(), seen)?;
            seen.insert(newpath, Some(path.clone()));
        }
        Ok(path)
    }
    let text = path.to_string_lossy();
    let resolved = join_real(String::new(), &text, &mut HashMap::new())?;
    let absolute = if resolved.starts_with('/') {
        resolved
    } else {
        join(&std::env::current_dir().unwrap_or_default().to_string_lossy(), &resolved)
    };
    Ok(PathBuf::from(normpath(&absolute)))
}

/// `Path(text).resolve()`: as far as the path exists; `""` is the working directory.
#[must_use]
pub fn resolve(text: &str) -> PathBuf {
    paths::resolve(&paths::normalize(text))
}

/// `plistlib.loads(path.read_bytes())` reduced to what is read from an `Info.plist`.
fn read_plist(path: &Path) -> Result<plist::Value, PyErr> {
    let raw = fs::read(path).map_err(|e| PyErr::os(&e, Some(path)))?;
    plist::Value::from_reader(std::io::Cursor::new(raw)).map_err(|_| PyErr::new("InvalidFileException", "Invalid file"))
}

fn plist_type(value: &plist::Value) -> &'static str {
    match value {
        plist::Value::Array(_) => "list",
        plist::Value::Dictionary(_) => "dict",
        plist::Value::Boolean(_) => "bool",
        plist::Value::Data(_) => "bytes",
        plist::Value::Date(_) => "datetime",
        plist::Value::Real(_) => "float",
        plist::Value::Integer(_) => "int",
        plist::Value::String(_) => "str",
        plist::Value::Uid(_) => "UID",
        _ => "object",
    }
}

/// `info.get(key)` of a plist root, which plistlib returns as whatever its type is.
fn plist_get<'a>(info: &'a plist::Value, key: &str) -> Result<Option<&'a plist::Value>, PyErr> {
    match info {
        plist::Value::Dictionary(map) => Ok(map.get(key)),
        other => Err(PyErr::new("AttributeError", format!("'{}' object has no attribute 'get'", plist_type(other)))),
    }
}

/// `app / "Contents/MacOS" / info["CFBundleExecutable"]`.
fn bundle_executable(app: &Path, info: &plist::Value) -> Result<PathBuf, PyErr> {
    match plist_get(info, "CFBundleExecutable")? {
        None => match info {
            plist::Value::Dictionary(_) => Err(PyErr::key("CFBundleExecutable")),
            _ => unreachable!("plist_get fails for a non-dictionary"),
        },
        Some(plist::Value::String(name)) => Ok(app.join("Contents/MacOS").join(name)),
        Some(other) => Err(PyErr::type_error(format!(
            "unsupported operand type(s) for /: 'PosixPath' and '{}'",
            plist_type(other)
        ))),
    }
}

/// `application_path(raw)`: the application the user selected, checked for this
/// platform (the KTalk bundle on macOS, an executable file on Linux).
///
/// # Errors
/// `OSError` for a path that does not exist, `ValueError` for the wrong file, and
/// what reading the bundle's `Info.plist` raises.
pub fn application_path(host: &Host, raw: &str) -> Result<PathBuf, PyErr> {
    let path = resolve_strict(&paths::expanduser(raw, &host.environ()))?;
    match host.platform {
        Platform::Darwin => {
            let info = read_plist(&path.join("Contents/Info.plist"))?;
            let identifier = plist_get(&info, "CFBundleIdentifier")?;
            if !matches!(identifier, Some(plist::Value::String(s)) if s == "kontur.talk") {
                return Err(PyErr::value("Select the official KTalk application bundle"));
            }
            if !bundle_executable(&path, &info)?.is_file() {
                return Err(PyErr::value("Application executable is missing"));
            }
        }
        Platform::Linux => {
            let executable = nix::unistd::access(&path, nix::unistd::AccessFlags::X_OK).is_ok();
            if !path.is_file() || !executable {
                return Err(PyErr::value("Select an executable KTalk AppImage or binary"));
            }
        }
        Platform::Other(_) => return Err(PyErr::value("Desktop integration supports Linux and macOS")),
    }
    Ok(path)
}

/// `int(x)` inside `is_ktalk_executable`, whose `except` catches all but overflow.
enum AsarFailure {
    No,
    Raise(PyErr),
}

impl From<PyErr> for AsarFailure {
    fn from(error: PyErr) -> Self {
        // struct.error is the class Python's struct module names `error`.
        if error.is_os() || error.is_value() || error.is("KeyError") || error.is("TypeError") || error.is("error") {
            Self::No
        } else {
            Self::Raise(error)
        }
    }
}

/// `archive.read(n)`: up to `n` bytes, fewer only at the end of the file.
fn read_up_to(file: &mut fs::File, n: usize) -> Result<Vec<u8>, PyErr> {
    let mut out = Vec::new();
    file.take(n as u64).read_to_end(&mut out).map_err(|e| PyErr::os(&e, None))?;
    Ok(out)
}

/// `is_ktalk_executable(executable)`: the packaged application is KTalk's (its
/// `app.asar` holds a `package.json` named `ktalk` with main `build/main.js`), read
/// without executing the binary.
///
/// # Errors
/// What the Python function does not catch: an overflowing size and a
/// `package.json` that is not an object.
pub fn is_ktalk_executable(host: &Host, executable: &Path) -> Result<bool, PyErr> {
    let resources = match host.platform {
        Platform::Darwin => parent(&parent(executable)).join("Resources"),
        _ => parent(executable).join("resources"),
    };
    let check = || -> Result<bool, AsarFailure> {
        let path = resources.join("app.asar");
        let mut archive = fs::File::open(&path).map_err(|e| PyErr::os(&e, Some(&path)))?;
        let head = read_up_to(&mut archive, 16)?;
        if head.len() != 16 {
            return Err(PyErr::new("error", "unpack requires a buffer of 16 bytes").into());
        }
        let word = |i: usize| u32::from_le_bytes([head[i * 4], head[i * 4 + 1], head[i * 4 + 2], head[i * 4 + 3]]);
        if !(0 < word(3) && word(3) <= 8 * 1024 * 1024) {
            return Ok(false);
        }
        let tree = pyjson_bytes(&read_up_to(&mut archive, word(3) as usize)?)?;
        let package = py::index(py::index(&tree, "files")?, "package.json")?;
        let size = py::to_int(py::index(package, "size")?)?;
        if !(0 < size && size <= 65536) {
            return Ok(false);
        }
        let offset = py::to_int(py::index(package, "offset")?)?;
        let position = 8 + i128::from(word(1)) + offset;
        let position = u64::try_from(position).map_err(|_| {
            if position < 0 {
                PyErr::value(format!("negative seek position {position}"))
            } else {
                PyErr::new("OverflowError", "Python int too large to convert to C long")
            }
        })?;
        archive.seek(SeekFrom::Start(position)).map_err(|e| PyErr::os(&e, None))?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let metadata = pyjson_bytes(&read_up_to(&mut archive, size as usize)?)?;
        let name = py::get(&metadata, "name").map_err(AsarFailure::Raise)?;
        let main = py::get(&metadata, "main").map_err(AsarFailure::Raise)?;
        Ok(name.and_then(Value::as_str) == Some("ktalk") && main.and_then(Value::as_str) == Some("build/main.js"))
    };
    match check() {
        Ok(found) => Ok(found),
        Err(AsarFailure::No) => Ok(false),
        Err(AsarFailure::Raise(error)) => Err(error),
    }
}

/// `json.loads(data)` of bytes.
fn pyjson_bytes(raw: &[u8]) -> Result<Value, PyErr> {
    recog_core::pyjson::loads_bytes(raw).map_err(|e| PyErr::new("JSONDecodeError", e.0))
}

/// `os.major(dev)` and `os.minor(dev)` of this platform's `dev_t`.
#[must_use]
pub fn major_minor(dev: u64) -> (u64, u64) {
    if cfg!(target_os = "macos") {
        ((dev >> 24) & 0xff, dev & 0x00ff_ffff)
    } else {
        (((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff), (dev & 0xff) | ((dev >> 12) & !0xff))
    }
}

/// `os.makedev(major, minor)` of this platform.
#[must_use]
pub fn makedev(major: u64, minor: u64) -> u64 {
    if cfg!(target_os = "macos") {
        (major << 24) | minor
    } else {
        ((major & 0xffff_f000) << 32) | ((major & 0xfff) << 8) | ((minor & 0xffff_ff00) << 12) | (minor & 0xff)
    }
}

/// `psutil.process_iter(["pid", "name"])`: each process that still exists, with its
/// name or None when it may not be read.
fn process_iter(host: &Host) -> Vec<(i64, Option<String>)> {
    let mut out = Vec::new();
    for pid in host.table.pids() {
        if host.table.open(pid).is_err() {
            continue;
        }
        match host.table.name(pid) {
            Ok(name) => out.push((pid, Some(name))),
            Err(e) if e.is("NoSuchProcess") => {}
            Err(_) => out.push((pid, None)),
        }
    }
    out
}

/// `fuse_source_matches(executable, app)`: the AppImage `app` is the FUSE runtime
/// that mounted `executable`, by the kernel's connection id.
///
/// # Errors
/// The executable's `stat` failing.
pub fn fuse_source_matches(host: &Host, executable: &Path, app: &Path) -> Result<bool, PyErr> {
    fuse_device_matches(host, (host.device)(executable)?, app)
}

/// [`fuse_source_matches`] for an executable on `device`.
///
/// # Errors
/// None the Python loop lets through for the processes the tests script; reading a
/// process fails into the next candidate.
pub fn fuse_device_matches(host: &Host, device: u64, app: &Path) -> Result<bool, PyErr> {
    let (major, connection) = major_minor(device);
    if major != 0 {
        return Ok(false);
    }
    let app_name = name_of(app);
    for (pid, name) in process_iter(host) {
        let name = name.unwrap_or_default();
        if name.is_empty() || !app_name.starts_with(&name) {
            continue;
        }
        let candidate = || -> Result<bool, PyErr> {
            if host.table.uid_real(pid)? != host.uid || resolve(&host.table.exe(pid)?) != app {
                return Ok(false);
            }
            let directory = host.proc_root.join(pid.to_string()).join("fdinfo");
            for entry in fs::read_dir(&directory).map_err(|e| PyErr::os(&e, Some(&directory)))? {
                let path = entry.map_err(|e| PyErr::os(&e, Some(&directory)))?.path();
                let raw = fs::read(&path).map_err(|e| PyErr::os(&e, Some(&path)))?;
                let text = std::str::from_utf8(&raw).map_err(|_| PyErr::new("UnicodeDecodeError", "utf-8"))?;
                for line in core_py::splitlines(text) {
                    if let Some(value) = line.strip_prefix("fuse_connection:") {
                        let value = core_py::parse_int(value).map_err(PyErr::value)?;
                        if u64::try_from(value) == Ok(connection) {
                            return Ok(true);
                        }
                    }
                }
            }
            Ok(false)
        };
        match candidate() {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(e) if e.is_psutil() || e.is_os() || e.is_value() => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// `matches(proc, app)`: `pid` is the selected application's main process, not a
/// helper (`--type=`), owned by this user.
///
/// # Errors
/// What reading the process or the bundle raises.
pub fn matches(host: &Host, pid: i64, app: &Path) -> Result<bool, PyErr> {
    if host.table.uid_real(pid)? != host.uid {
        return Ok(false);
    }
    if host.table.cmdline(pid)?.iter().any(|arg| arg.starts_with("--type=")) {
        return Ok(false);
    }
    let executable = resolve(&host.table.exe(pid)?);
    if !is_ktalk_executable(host, &executable)? {
        return Ok(false);
    }
    if host.platform == Platform::Darwin {
        let info = read_plist(&app.join("Contents/Info.plist"))?;
        return Ok(executable == paths::resolve(&bundle_executable(app, &info)?));
    }
    if executable == app {
        return Ok(true);
    }
    // AppImage's runtime mounts a new path each launch: match the selected source
    // file as well as the actual main executable, not the process name.
    let environ = host.table.environ(pid)?;
    let image = environ.iter().rev().find(|(k, _)| k == "APPIMAGE").map_or("", |(_, v)| v.as_str());
    if name_of(&executable) != "ktalk" {
        return Ok(false);
    }
    if !image.is_empty() && resolve(image) == app {
        return Ok(true);
    }
    fuse_source_matches(host, &executable, app)
}

fn caught(error: &PyErr) -> bool {
    error.is_psutil() || error.is_os() || error.is_value() || error.is("KeyError")
}

/// `find_targets(app)`: the main processes of the selected application.
///
/// # Errors
/// What `matches` raises beyond `psutil.Error`, `OSError`, `ValueError`, `KeyError`.
pub fn find_targets(host: &Host, app: &Path) -> Result<Vec<Target>, PyErr> {
    let names = ["ktalk".to_owned(), "Толк".to_owned(), stem_of(app), name_of(app)];
    let mut targets = Vec::new();
    for (pid, name) in process_iter(host) {
        if !name.is_some_and(|name| names.contains(&name)) {
            continue;
        }
        let found = matches(host, pid, app).and_then(|matched| {
            if !matched {
                return Ok(None);
            }
            Ok(Some(Target { pid, started: host.table.create_time(pid)?, executable: host.table.exe(pid)? }))
        });
        match found {
            Ok(Some(target)) => targets.push(target),
            Ok(None) => {}
            Err(e) if caught(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(targets)
}

/// `same_process(target, app)`: the process is still the one recorded (same user,
/// start time and executable) and, when `app` is given, still the application's.
///
/// # Errors
/// What `matches` raises beyond the classes caught.
pub fn same_process(host: &Host, target: &Target, app: Option<&Path>) -> Result<bool, PyErr> {
    let check = || -> Result<bool, PyErr> {
        let table = &host.table;
        table.open(target.pid)?;
        if table.uid_real(target.pid)? != host.uid
            || table.create_time(target.pid)? != target.started
            || table.exe(target.pid)? != target.executable
        {
            return Ok(false);
        }
        match app {
            Some(app) => matches(host, target.pid, app),
            None => Ok(true),
        }
    };
    match check() {
        Err(e) if caught(&e) => Ok(false),
        other => other,
    }
}

/// `listeners(target)`: the TCP ports the recorded process listens on, none when it
/// is not that process any more.
///
/// # Errors
/// What reading its connections raises.
pub fn listeners(host: &Host, target: &Target) -> Result<Vec<(String, u16)>, PyErr> {
    if !same_process(host, target, None)? {
        return Ok(Vec::new());
    }
    host.table.open(target.pid)?;
    host.table.tcp_listeners(target.pid)
}

fn loopback(address: &str) -> bool {
    address == "127.0.0.1" || address == "::1"
}

/// `owns_inspector(target)`: the process listens on the inspector port, on loopback
/// only.
///
/// # Errors
/// As [`listeners`].
pub fn owns_inspector(host: &Host, target: &Target) -> Result<bool, PyErr> {
    let addresses: Vec<String> =
        listeners(host, target)?.into_iter().filter(|(_, port)| *port == host.inspector_port).map(|(a, _)| a).collect();
    Ok(!addresses.is_empty() && addresses.iter().all(|a| loopback(a)))
}

/// `port_available()`: nothing listens on the inspector port and it can be bound. A
/// specific-address `SO_REUSEADDR` bind succeeds beside a wildcard listener on BSD,
/// so binding alone would not tell.
#[must_use]
pub fn port_available(host: &Host) -> bool {
    use std::net::{Ipv4Addr, SocketAddr};
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, host.inspector_port));
    match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(200)) {
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
        _ => return false,
    }
    let Ok(socket) = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None) else { return false };
    socket.set_reuse_address(true).is_ok() && socket.bind(&address.into()).is_ok()
}

const DEBUG_FLAGS: [&str; 6] = [
    "--inspect",
    "--remote-debugging",
    "--debug",
    "--disable-sigusr1",
    "--experimental-config-file",
    "--experimental-default-config-file",
];

/// `ready(target)`: the application finished initializing (its URL-handler listener
/// is open, so Node installed its SIGUSR1 handler) and runs neither as plain Node nor
/// under a debugger or a flag that changes the signal or Node's configuration.
///
/// # Errors
/// What reading the process raises.
pub fn ready(host: &Host, target: &Target) -> Result<bool, PyErr> {
    host.table.open(target.pid)?;
    let environment = host.table.environ(target.pid)?;
    if environment.iter().any(|(k, _)| k == "ELECTRON_RUN_AS_NODE") {
        return Ok(false);
    }
    let mut options = host.table.cmdline(target.pid)?;
    let node_options = environment.iter().rev().find(|(k, _)| k == "NODE_OPTIONS").map_or("", |(_, v)| v.as_str());
    match crate::shlex::split(node_options) {
        Ok(words) => options.extend(words),
        Err(_) => return Ok(false),
    }
    if options.iter().any(|arg| {
        let arg = arg.replace('_', "-");
        DEBUG_FLAGS.iter().any(|flag| arg.starts_with(flag))
    }) {
        return Ok(false);
    }
    Ok(listeners(host, target)?.iter().any(|(address, port)| *port == host.app_ready_port && loopback(address)))
}

/// `FUSE_SENTINEL`: what precedes Electron's fuse wire in its binary.
pub const FUSE_SENTINEL: &[u8] = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX";

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > data.len() {
        return None;
    }
    data[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// `_inspect_fuses(path, fingerprint)` without its cache: every architecture's v1
/// fuse wire read in bounded-memory chunks, each with `EnableNodeCliInspectArguments`
/// (fuse 3) on, one or two of them.
///
/// # Errors
/// The `OSError` of opening or reading the binary.
pub fn inspect_fuses(path: &Path) -> Result<bool, PyErr> {
    const CHUNK: usize = 65536;
    let overlap = FUSE_SENTINEL.len() + 2 + 255;
    let mut binary = fs::File::open(path).map_err(|e| PyErr::os(&e, Some(path)))?;
    let mut found: HashMap<usize, bool> = HashMap::new();
    let mut offset = 0usize;
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let chunk = read_up_to(&mut binary, CHUNK)?;
        let mut data = tail.clone();
        data.extend_from_slice(&chunk);
        let mut index = find(&data, FUSE_SENTINEL, 0);
        while let Some(at) = index {
            let position = offset - tail.len() + at;
            let header = at + FUSE_SENTINEL.len();
            if data.len() >= header + 2 {
                let (version, length) = (data[header], usize::from(data[header + 1]));
                if version != 1 || length < 4 {
                    return Ok(false);
                }
                if data.len() >= header + 2 + length {
                    found.insert(position, data[header + 2 + 3] == 0x31);
                } else if chunk.is_empty() {
                    return Ok(false);
                }
            } else if chunk.is_empty() {
                return Ok(false);
            }
            index = find(&data, FUSE_SENTINEL, at + 1);
        }
        if chunk.is_empty() {
            break;
        }
        offset += chunk.len();
        tail = data[data.len().saturating_sub(overlap)..].to_vec();
    }
    Ok((1..=2).contains(&found.len()) && found.values().all(|enabled| *enabled))
}

/// `_inspect_fuses` with its `lru_cache(maxsize=8)`, keyed by the path and the file's
/// identity.
///
/// # Errors
/// As [`inspect_fuses`] (failures are not cached).
pub fn inspect_fuses_cached(host: &Host, path: &Path, fingerprint: &str) -> Result<bool, PyErr> {
    let key = format!("{}\0{fingerprint}", path.display());
    {
        let mut cache = host.fuse_cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
            let entry = cache.remove(i).unwrap_or_default();
            let value = entry.1;
            cache.push_back(entry);
            return Ok(value);
        }
    }
    let value = inspect_fuses(path)?;
    let mut cache = host.fuse_cache.lock().unwrap_or_else(PoisonError::into_inner);
    cache.push_back((key, value));
    while cache.len() > 8 {
        cache.pop_front();
    }
    Ok(value)
}

/// `st_ctime` as Python computes it from the timespec.
#[allow(clippy::cast_precision_loss)]
fn ctime(meta: &fs::Metadata) -> f64 {
    meta.ctime() as f64 + meta.ctime_nsec() as f64 * 1e-9
}

/// `inspector_supported(target)`: the running image's fuses allow the inspector. A
/// binary changed after the process started cannot attest to the running image.
///
/// # Errors
/// A symlink loop on the way to the binary, which Python does not catch either.
pub fn inspector_supported(host: &Host, target: &Target) -> Result<bool, PyErr> {
    let executable = paths::normalize(&target.executable);
    let binary = match host.platform {
        Platform::Darwin => {
            parent(&parent(&executable)).join("Frameworks/Electron Framework.framework/Electron Framework")
        }
        _ => executable,
    };
    let check = || -> Result<bool, PyErr> {
        let binary = resolve_strict(&binary)?;
        let meta = fs::metadata(&binary).map_err(|e| PyErr::os(&e, Some(&binary)))?;
        if ctime(&meta) > target.started {
            return Ok(false);
        }
        let identity = format!(
            "{} {} {} {} {}",
            meta.dev(),
            meta.ino(),
            meta.size(),
            i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
            i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec())
        );
        inspect_fuses_cached(host, &binary, &identity)
    };
    match check() {
        Err(e) if e.is_os() || e.is_value() => Ok(false),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_values_when_named_then_their_python_types() {
        // given
        let date = plist::Date::from(std::time::UNIX_EPOCH);
        let cases = [
            (plist::Value::Array(Vec::new()), "list"),
            (plist::Value::Dictionary(plist::Dictionary::new()), "dict"),
            (plist::Value::Boolean(true), "bool"),
            (plist::Value::Data(Vec::new()), "bytes"),
            (plist::Value::Date(date), "datetime"),
            (plist::Value::Real(1.5), "float"),
            (plist::Value::Integer(plist::Integer::from(1i64)), "int"),
            (plist::Value::String(String::new()), "str"),
            (plist::Value::Uid(plist::Uid::new(1)), "UID"),
        ];
        for (value, name) in cases {
            // when / then
            assert_eq!(plist_type(&value), name, "{value:?}");
        }
    }

    #[test]
    fn paths_when_split_and_normalized_then_like_posixpath() {
        // given / when / then
        assert_eq!(split("/a/b"), ("/a".to_owned(), "b".to_owned()));
        assert_eq!(split("/a"), ("/".to_owned(), "a".to_owned()));
        assert_eq!(split("a"), (String::new(), "a".to_owned()));
        assert_eq!(split("//a//b"), ("//a".to_owned(), "b".to_owned()));
        assert_eq!(split("//x"), ("//".to_owned(), "x".to_owned()));
        assert_eq!(normpath("//a/./b/../c"), "//a/c");
        assert_eq!(normpath("///a//b/"), "/a/b");
        assert_eq!(normpath("a/../../b"), "../b");
        assert_eq!(normpath("../x/.."), "..");
        assert_eq!(normpath("/../a"), "/a");
        assert_eq!(normpath(""), ".");
        assert_eq!(normpath("a/.."), ".");
    }
}
