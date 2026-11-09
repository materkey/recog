//! A capture's child process: started, read with a timeout, reaped.
//!
//! Behind a trait so the restart policy can be driven by scripted children in tests.
//! [`OsSpawner`] starts real processes. Each real child has a thread reading its
//! stdout (`read(2)` of up to 6400 bytes, what Python's `bufsize=0` pipe read gives)
//! into a channel, which is how a read waits with a timeout without `select(2)`; its
//! stderr is read whole when it died on start, or drained line by line into the
//! capture's tail once it proved alive, as Python's drain thread does.

use std::collections::VecDeque;
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How many stderr lines a capture keeps (`deque(maxlen=200)`).
pub const STDERR_TAIL_LINES: usize = 200;

/// How many bytes a pipe read takes at most: 100 ms of float32 at 16 kHz.
pub const PIPE_READ_BYTES: usize = 6400;

/// The last stderr lines of the current child, shared with its drain thread.
#[derive(Debug, Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    pub fn clear(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    pub fn push(&self, line: String) {
        let mut tail = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }

    /// Every line, oldest first.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect()
    }
}

/// What a read of the child's stdout gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Some bytes, not necessarily whole samples.
    Data(Vec<u8>),
    /// The pipe is closed (Python's `read()` returning `b""`).
    Eof,
    /// Nothing arrived within the timeout.
    Timeout,
}

/// A started child process.
pub trait ChildProcess: Send + Sync {
    fn pid(&self) -> u32;
    /// `Popen.poll()`: None while it runs, else its exit code.
    fn poll(&self) -> Option<i32>;
    /// Waits up to `timeout` seconds for stdout and takes at most `max` bytes.
    fn read(&self, max: usize, timeout: f64) -> ReadOutcome;
    /// The whole stderr of a child that died; empty once drained or read.
    fn read_stderr(&self) -> String;
    /// Appends stderr lines to `tail` as they come, from a thread of its own.
    fn drain_stderr(&self, tail: StderrTail);
    /// terminate, wait up to `timeout`, then kill and wait (no zombie left).
    fn reap(&self, timeout: f64);
}

/// Starts children.
pub trait Spawner: Send + Sync {
    /// # Errors
    /// The executable is missing or cannot be run.
    fn spawn(&self, argv: &[String]) -> io::Result<Arc<dyn ChildProcess>>;
}

/// Real processes, stdout and stderr piped, stdin inherited (as `Popen` does).
#[derive(Debug, Clone, Copy, Default)]
pub struct OsSpawner;

impl Spawner for OsSpawner {
    fn spawn(&self, argv: &[String]) -> io::Result<Arc<dyn ChildProcess>> {
        let (program, args) =
            argv.split_first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
        let mut child = Command::new(program).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout pipe"))?;
        let stderr = child.stderr.take();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new().name("capture-stdout".into()).spawn(move || forward_stdout(&mut stdout, &tx))?;
        Ok(Arc::new(OsChild {
            pid: child.id(),
            child: Mutex::new(child),
            stdout: Mutex::new((rx, Vec::new())),
            stderr: Mutex::new(stderr),
        }))
    }
}

/// The pipe's reads into `tx` until end of file, a read error or a gone receiver.
fn forward_stdout(stdout: &mut impl io::Read, tx: &mpsc::Sender<Vec<u8>>) {
    let mut buf = vec![0u8; PIPE_READ_BYTES];
    loop {
        match stdout.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

struct OsChild {
    pid: u32,
    child: Mutex<Child>,
    stdout: Mutex<(Receiver<Vec<u8>>, Vec<u8>)>,
    stderr: Mutex<Option<ChildStderr>>,
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    status.code().unwrap_or(-1)
}

/// Python's `bytes.decode(errors="ignore")`.
fn decode_ignore(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).replace('\u{FFFD}', "")
}

impl OsChild {
    fn wait_for(&self, timeout: f64) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        loop {
            if self.poll().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl ChildProcess for OsChild {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn poll(&self) -> Option<i32> {
        match self.child.lock().unwrap_or_else(PoisonError::into_inner).try_wait() {
            Ok(Some(status)) => Some(exit_code(status)),
            Ok(None) => None,
            Err(_) => Some(-1),
        }
    }

    fn read(&self, max: usize, timeout: f64) -> ReadOutcome {
        let mut guard = self.stdout.lock().unwrap_or_else(PoisonError::into_inner);
        let (rx, leftover) = &mut *guard;
        if leftover.is_empty() {
            match rx.recv_timeout(Duration::from_secs_f64(timeout.max(0.0))) {
                Ok(data) => *leftover = data,
                Err(RecvTimeoutError::Timeout) => return ReadOutcome::Timeout,
                Err(RecvTimeoutError::Disconnected) => return ReadOutcome::Eof,
            }
        }
        let take = leftover.len().min(max.max(1));
        let rest = leftover.split_off(take);
        ReadOutcome::Data(std::mem::replace(leftover, rest))
    }

    fn read_stderr(&self) -> String {
        let Some(mut stderr) = self.stderr.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return String::new();
        };
        let mut raw = Vec::new();
        let _ = stderr.read_to_end(&mut raw);
        decode_ignore(&raw)
    }

    fn drain_stderr(&self, tail: StderrTail) {
        let Some(stderr) = self.stderr.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return;
        };
        let _ = std::thread::Builder::new().name("capture-stderr".into()).spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => tail.push(decode_ignore(&line)),
                }
            }
        });
    }

    fn reap(&self, timeout: f64) {
        if self.poll().is_none() {
            #[allow(clippy::cast_possible_wrap)]
            let pid = nix::unistd::Pid::from_raw(self.pid as i32);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        }
        if self.wait_for(timeout) {
            return;
        }
        let _ = self.child.lock().unwrap_or_else(PoisonError::into_inner).kill();
        self.wait_for(1.0);
    }
}
