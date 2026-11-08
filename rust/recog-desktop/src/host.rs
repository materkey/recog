//! What the Python modules reach through `psutil`, `os`, `sys.platform`, `time` and
//! module constants, gathered so that tests can replace each of them as the Python
//! tests monkeypatch them: the process table, the user, the ports, `/proc`, the
//! signal sent to the application and the clock.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::py::PyErr;

/// `INSPECTOR_PORT`: the port Node's inspector opens on SIGUSR1.
pub const INSPECTOR_PORT: u16 = 9229;
/// `APP_READY_PORT`: KTalk's native URL-handler listener, open once it initialized.
pub const APP_READY_PORT: u16 = 44258;

/// `sys.platform`, as far as the integration distinguishes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Platform {
    Darwin,
    Linux,
    Other(String),
}

impl Platform {
    /// The platform this binary runs on.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other(std::env::consts::OS.to_owned())
        }
    }
}

/// psutil's view of the processes: each call fails as psutil does (`NoSuchProcess`,
/// `AccessDenied`, `ZombieProcess`).
pub trait ProcessTable: Send + Sync {
    /// `psutil.pids()`: every process, sorted.
    fn pids(&self) -> Vec<i64>;
    /// `psutil.Process(pid)`: fails for a process that does not exist.
    fn open(&self, pid: i64) -> Result<(), PyErr>;
    /// `name()`, with psutil's extension of a truncated name from the command line.
    fn name(&self, pid: i64) -> Result<String, PyErr>;
    /// `uids().real`.
    fn uid_real(&self, pid: i64) -> Result<u32, PyErr>;
    /// `create_time()`, seconds since the epoch.
    fn create_time(&self, pid: i64) -> Result<f64, PyErr>;
    /// `exe()`.
    fn exe(&self, pid: i64) -> Result<String, PyErr>;
    /// `cmdline()`.
    fn cmdline(&self, pid: i64) -> Result<Vec<String>, PyErr>;
    /// `environ()`, in the order the block lists them (a repeated name: the last).
    fn environ(&self, pid: i64) -> Result<Vec<(String, String)>, PyErr>;
    /// `net_connections(kind="tcp")` in `LISTEN`: the local address and port of each.
    fn tcp_listeners(&self, pid: i64) -> Result<Vec<(String, u16)>, PyErr>;
}

/// `time.time()` and `time.monotonic()`.
pub trait Clock: Send + Sync {
    fn time(&self) -> f64;
    fn monotonic(&self) -> f64;
}

/// The system clock.
pub struct SystemClock {
    start: std::time::Instant,
}

impl SystemClock {
    #[must_use]
    pub fn shared() -> Arc<dyn Clock> {
        Arc::new(Self { start: std::time::Instant::now() })
    }
}

impl Clock for SystemClock {
    fn time(&self) -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
    }

    fn monotonic(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

/// A clock for tests: a fixed `time()` and scripted `monotonic()` readings (the last
/// one repeats).
pub struct ScriptedClock {
    pub now: Mutex<f64>,
    monotonic: Mutex<VecDeque<f64>>,
}

impl ScriptedClock {
    #[must_use]
    pub fn new(now: f64, monotonic: &[f64]) -> Arc<Self> {
        Arc::new(Self { now: Mutex::new(now), monotonic: Mutex::new(monotonic.iter().copied().collect()) })
    }
}

impl Clock for ScriptedClock {
    fn time(&self) -> f64 {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn monotonic(&self) -> f64 {
        let mut readings = self.monotonic.lock().unwrap_or_else(PoisonError::into_inner);
        if readings.len() > 1 { readings.pop_front().unwrap_or(0.0) } else { readings.front().copied().unwrap_or(0.0) }
    }
}

/// `os.kill(pid, signal.SIGUSR1)`.
pub type Signaller = Arc<dyn Fn(i64) -> Result<(), PyErr> + Send + Sync>;

/// `path.stat().st_dev`.
pub type DeviceOf = Arc<dyn Fn(&std::path::Path) -> Result<u64, PyErr> + Send + Sync>;

fn device_of(path: &std::path::Path) -> Result<u64, PyErr> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).map(|m| m.dev()).map_err(|e| PyErr::os(&e, Some(path)))
}

/// Everything the integration asks the operating system.
#[derive(Clone)]
pub struct Host {
    pub platform: Platform,
    pub table: Arc<dyn ProcessTable>,
    /// `os.getuid()`.
    pub uid: u32,
    pub inspector_port: u16,
    pub app_ready_port: u16,
    /// Where `/proc` is (FUSE connection lookup).
    pub proc_root: PathBuf,
    /// `HOME`, for `expanduser` and `Path.home()`.
    pub home: PathBuf,
    pub signal_usr1: Signaller,
    /// The device of the executable a FUSE mount serves.
    pub device: DeviceOf,
    pub clock: Arc<dyn Clock>,
    /// `_inspect_fuses`'s `lru_cache(maxsize=8)`.
    pub fuse_cache: Arc<Mutex<VecDeque<(String, bool)>>>,
}

impl Host {
    /// The real system: the process table of this OS, the real signal and clock.
    #[must_use]
    pub fn system() -> Self {
        Self::with_table(crate::system::table())
    }

    /// The real system with another process table (tests).
    #[must_use]
    pub fn with_table(table: Arc<dyn ProcessTable>) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).or_else(std::env::home_dir).unwrap_or_default();
        Self {
            platform: Platform::current(),
            table,
            uid: nix::unistd::getuid().as_raw(),
            inspector_port: INSPECTOR_PORT,
            app_ready_port: APP_READY_PORT,
            proc_root: PathBuf::from("/proc"),
            home,
            signal_usr1: Arc::new(send_usr1),
            device: Arc::new(device_of),
            clock: SystemClock::shared(),
            fuse_cache: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// `HOME` as an environment for `recog_core::paths::expanduser`.
    #[must_use]
    pub fn environ(&self) -> recog_core::install_config::Environ {
        [("HOME".to_owned(), self.home.to_string_lossy().into_owned())].into_iter().collect()
    }
}

fn send_usr1(pid: i64) -> Result<(), PyErr> {
    let pid = i32::try_from(pid).map_err(|_| PyErr::new("OverflowError", "signed integer is greater than maximum"))?;
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGUSR1)
        .map_err(|errno| crate::py::errno_error(errno, None))
}
