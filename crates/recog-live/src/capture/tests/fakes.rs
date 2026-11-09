//! Scripted children and spawners, the doubles of Python's `_FakeProcess` family.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::capture::child::{ChildProcess, ReadOutcome, Spawner, StderrTail};
use crate::capture::subprocess::SubprocessCapture;
use crate::clock::ManualClock;
use crate::log::{Captured, Logger};

pub const SR: u32 = 16_000;

type Hook = Box<dyn Fn() + Send + Sync>;

/// A child whose polls and reads follow a script.
pub struct FakeChild {
    pub pid: u32,
    /// Poll answers in order; the last one repeats.
    polls: Mutex<VecDeque<Option<i32>>>,
    /// Stdout bytes; each read takes at most `chunk` of them.
    stdout: Mutex<Vec<u8>>,
    chunk: usize,
    /// When stdout is exhausted: dead from then on (`_OneBlockThenDead`) or not.
    dies_when_drained: bool,
    /// Called when a read finds stdout exhausted (`_StopWhenDrained`).
    on_drained: Mutex<Option<Hook>>,
    /// Reads answer Timeout instead of Eof once drained (a silent live source).
    silent: bool,
    stderr: Mutex<String>,
    pub reaped: AtomicBool,
    pub reads: AtomicUsize,
}

impl FakeChild {
    pub fn alive(stdout: Vec<u8>) -> Self {
        Self {
            pid: 4242,
            polls: Mutex::new(VecDeque::from([None])),
            stdout: Mutex::new(stdout),
            chunk: usize::MAX,
            dies_when_drained: false,
            on_drained: Mutex::new(None),
            silent: false,
            stderr: Mutex::new(String::new()),
            reaped: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
        }
    }

    /// Dead from the first poll, with `stderr`.
    pub fn dead(stderr: &str) -> Self {
        let child = Self::alive(Vec::new());
        *child.polls.lock().unwrap() = VecDeque::from([Some(1)]);
        *child.stderr.lock().unwrap() = stderr.to_owned();
        child
    }

    /// Delivers `stdout`, then dies.
    pub fn one_block_then_dead(stdout: Vec<u8>) -> Self {
        Self { dies_when_drained: true, ..Self::alive(stdout) }
    }

    /// Alive and never writes.
    pub fn silent() -> Self {
        Self { silent: true, ..Self::alive(Vec::new()) }
    }

    pub fn with_polls(self, polls: &[Option<i32>]) -> Self {
        *self.polls.lock().unwrap() = polls.iter().copied().collect();
        self
    }

    pub fn with_chunk(mut self, chunk: usize) -> Self {
        self.chunk = chunk;
        self
    }

    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = pid;
        self
    }

    /// Stops `capture` when stdout runs out.
    pub fn stop_when_drained(self, capture: &SubprocessCapture) -> Self {
        let capture = capture.clone();
        *self.on_drained.lock().unwrap() = Some(Box::new(move || capture.set_running(false)));
        self
    }

    pub fn arc(self) -> Arc<dyn ChildProcess> {
        Arc::new(self)
    }
}

impl ChildProcess for FakeChild {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn poll(&self) -> Option<i32> {
        if self.dies_when_drained && self.stdout.lock().unwrap().is_empty() {
            return Some(0);
        }
        let mut polls = self.polls.lock().unwrap();
        if polls.len() > 1 { polls.pop_front().unwrap() } else { polls[0] }
    }

    fn read(&self, max: usize, _timeout: f64) -> ReadOutcome {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut stdout = self.stdout.lock().unwrap();
        if stdout.is_empty() {
            drop(stdout);
            if let Some(hook) = self.on_drained.lock().unwrap().as_ref() {
                hook();
            }
            return if self.silent { ReadOutcome::Timeout } else { ReadOutcome::Eof };
        }
        let take = stdout.len().min(max).min(self.chunk);
        let rest = stdout.split_off(take);
        ReadOutcome::Data(std::mem::replace(&mut *stdout, rest))
    }

    fn read_stderr(&self) -> String {
        std::mem::take(&mut *self.stderr.lock().unwrap())
    }

    fn drain_stderr(&self, tail: StderrTail) {
        for line in self.read_stderr().lines() {
            tail.push(format!("{line}\n"));
        }
    }

    fn reap(&self, _timeout: f64) {
        self.reaped.store(true, Ordering::SeqCst);
        let mut polls = self.polls.lock().unwrap();
        *polls = VecDeque::from([Some(-15)]);
    }
}

/// What a scripted spawn gives.
pub enum Spawn {
    Child(Arc<dyn ChildProcess>),
    Fail(&'static str),
}

/// A spawner answering from a script, then from `fallback`; it records each argv.
pub struct FakeSpawner {
    script: Mutex<VecDeque<Spawn>>,
    fallback: Box<dyn Fn() -> Spawn + Send + Sync>,
    pub argvs: Mutex<Vec<Vec<String>>>,
}

impl FakeSpawner {
    pub fn new(script: Vec<Spawn>, fallback: impl Fn() -> Spawn + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into()),
            fallback: Box::new(fallback),
            argvs: Mutex::new(Vec::new()),
        })
    }

    /// Every spawn fails.
    pub fn failing() -> Arc<Self> {
        Self::new(Vec::new(), || Spawn::Fail("parec: connection refused"))
    }

    pub fn count(&self) -> usize {
        self.argvs.lock().unwrap().len()
    }
}

impl Spawner for FakeSpawner {
    fn spawn(&self, argv: &[String]) -> io::Result<Arc<dyn ChildProcess>> {
        self.argvs.lock().unwrap().push(argv.to_vec());
        let next = self.script.lock().unwrap().pop_front().unwrap_or_else(|| (self.fallback)());
        match next {
            Spawn::Child(child) => Ok(child),
            Spawn::Fail(message) => Err(io::Error::other(message)),
        }
    }
}

/// A capture of `argv` on a manual clock and a captured log.
pub fn capture_with(name: &str, spawner: Arc<dyn Spawner>, clock: &ManualClock) -> (SubprocessCapture, Captured) {
    let (log, captured) = Logger::capture(clock.shared());
    let capture = SubprocessCapture::builder(vec!["fake".into()], name)
        .sample_rate(SR)
        .max_buffer_s(10.0)
        .spawner(spawner)
        .clock(clock.shared())
        .logger(log)
        .build();
    (capture, captured)
}

pub fn f32_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Records each respawn pause and stops the loop on the first slow one; returns the
/// pauses and whether the capture was running at that slow attempt.
pub type Recorded<T> = Arc<Mutex<Vec<T>>>;

pub fn stop_at_slow_retry(capture: &SubprocessCapture, lost_retry_s: f64) -> (Recorded<f64>, Recorded<bool>) {
    let delays = Arc::new(Mutex::new(Vec::new()));
    let running = Arc::new(Mutex::new(Vec::new()));
    let (d, r) = (delays.clone(), running.clone());
    capture.set_backoff_hook(Some(Arc::new(move |delay, cap: &SubprocessCapture| {
        d.lock().unwrap().push(delay);
        if (delay - lost_retry_s).abs() < f64::EPSILON {
            r.lock().unwrap().push(cap.running());
            cap.set_running(false);
        }
    })));
    (delays, running)
}
