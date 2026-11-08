//! Capture from a child process's stdout with a bounded respawn, in place of
//! `recog/system_audio.py`'s `SubprocessPCMCapture`.
//!
//! The reader thread runs Python's `_reader_loop` step for step: a planned source
//! switch and a requested restart first (neither spends the crash-retry budget), then
//! a dead child is respawned after a backoff charged before *every* respawn
//! ([`RestartPolicy::backoff_s`], the last value repeating), and after
//! [`RestartPolicy::max_restarts`] fast attempts the capture is marked lost but keeps
//! trying every [`RestartPolicy::lost_retry_s`]; the first data clears the loss. A
//! failed first spawn is left to the same loop. The log lines are Python's.
//!
//! Known differences: an error starting the executable reads as Rust's `io::Error`
//! (`No such file or directory (os error 2)`) where Python prints its `OSError`
//! (`[Errno 2] No such file or directory: 'parec'`); a read cannot raise here, so
//! `Reader error:` is never logged; `stop()` waits for the reader thread up to one
//! second and then leaves it to finish on its own, as Python's `join(timeout=1.0)`.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::capture::buffer::BufferedAudioCapture;
use crate::capture::child::{ChildProcess, OsSpawner, ReadOutcome, Spawner, StderrTail};
use crate::clock::{SharedClock, SystemClock};
use crate::config::SAMPLE_RATE;
use crate::log::Logger;

/// Bytes per float32 sample.
pub const BYTES_PER_SAMPLE: usize = 4;

/// Seconds of nothing but exact zeros during a conference before live asks the system
/// capture for a restart (`DIGITAL_SILENCE_RESTART_S`).
pub const DIGITAL_SILENCE_RESTART_S: f64 = 60.0;

/// How many of the child's last stderr lines a message quotes.
pub const STDERR_REPORT_LINES: usize = 5;

/// The timing of the reader loop; the defaults are Python's constants.
#[derive(Debug, Clone, PartialEq)]
pub struct RestartPolicy {
    /// `RESTART_BACKOFF_S`: the pause before each fast respawn, the last repeating.
    pub backoff_s: Vec<f64>,
    /// `MAX_RESTARTS`: fast attempts before the capture counts as lost.
    pub max_restarts: u32,
    /// `LOST_RETRY_S`: the pause between attempts once lost.
    pub lost_retry_s: f64,
    /// The wait after a spawn before checking the child survived it.
    pub startup_wait_s: f64,
    /// `READ_WAIT_TIMEOUT_S`: how long a read waits before the loop looks again.
    pub read_wait_timeout_s: f64,
    /// `READ_IDLE_SLEEP_S`: the pause after a read that gave nothing.
    pub read_idle_sleep_s: f64,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            backoff_s: vec![1.0, 2.0, 5.0, 10.0],
            max_restarts: 5,
            lost_retry_s: 60.0,
            startup_wait_s: 0.5,
            read_wait_timeout_s: 1.0,
            read_idle_sleep_s: 0.05,
        }
    }
}

/// What a decoder may need besides the bytes.
#[derive(Debug, Clone, Copy)]
pub struct DecodeContext<'a> {
    /// `time.time()` at the read.
    pub wall: f64,
    /// `digital_silence_s()` before this read's samples count.
    pub silence_s: f64,
    pub name: &'a str,
    pub sample_rate: u32,
    pub log: &'a Logger,
}

/// Turns the bytes of a read into sample blocks with the wall time of each first sample.
pub trait Decoder: Send {
    fn decode(&mut self, data: &[u8], ctx: &DecodeContext<'_>) -> Vec<(Vec<f32>, f64)>;
    /// A new child, a new stream: whatever was left of the old one goes.
    fn on_new_child(&mut self);
}

/// Raw float32 mono PCM, a partial sample carried to the next read (`_decode_pcm`).
#[derive(Debug, Default)]
pub struct PcmDecoder {
    tail: Vec<u8>,
}

impl PcmDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whole samples of `data` after the carried bytes; a partial one is kept.
    pub fn decode_pcm(&mut self, data: &[u8]) -> Vec<f32> {
        let mut joined = std::mem::take(&mut self.tail);
        joined.extend_from_slice(data);
        let usable = joined.len() - joined.len() % BYTES_PER_SAMPLE;
        self.tail = joined[usable..].to_vec();
        joined[..usable].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    }

    /// The carried partial sample.
    #[must_use]
    pub fn tail(&self) -> &[u8] {
        &self.tail
    }
}

#[allow(clippy::cast_precision_loss)]
fn seconds(samples: usize, sample_rate: u32) -> f64 {
    samples as f64 / f64::from(sample_rate)
}

impl Decoder for PcmDecoder {
    fn decode(&mut self, data: &[u8], ctx: &DecodeContext<'_>) -> Vec<(Vec<f32>, f64)> {
        let samples = self.decode_pcm(data);
        if samples.is_empty() {
            return Vec::new();
        }
        let at = ctx.wall - seconds(samples.len(), ctx.sample_rate);
        vec![(samples, at)]
    }

    fn on_new_child(&mut self) {
        self.tail.clear();
    }
}

/// A planned change of source, which `PulseAudioCapture` implements.
pub trait SourceSwitch: Send + Sync {
    /// The requested source, once; None when there is no change.
    fn take_pending(&self) -> Option<String>;
    /// Switches `capture` over to `source`.
    ///
    /// # Errors
    /// The switch failed; the loop logs it and goes on.
    fn apply(&self, capture: &SubprocessCapture, source: &str) -> Result<(), String>;
}

/// Called with each respawn pause before it is slept; tests stop the loop from it.
pub type BackoffHook = Arc<dyn Fn(f64, &SubprocessCapture) + Send + Sync>;

/// What a [`SubprocessCapture`] is made of; [`SubprocessCapture::builder`] fills it.
pub struct CaptureBuilder {
    argv: Vec<String>,
    name: String,
    sample_rate: u32,
    max_buffer_s: f64,
    decoder: Box<dyn Decoder>,
    spawner: Arc<dyn Spawner>,
    clock: SharedClock,
    log: Logger,
    policy: RestartPolicy,
    reader_thread: bool,
}

impl CaptureBuilder {
    /// Whether `start()` runs the reader loop on a thread of its own (the default);
    /// tests that drive the loop themselves turn it off, as Python's stub
    /// `_reader_loop`.
    #[must_use]
    pub fn reader_thread(mut self, on: bool) -> Self {
        self.reader_thread = on;
        self
    }

    /// The command to run.
    #[must_use]
    pub fn argv(mut self, argv: Vec<String>) -> Self {
        self.argv = argv;
        self
    }

    /// The sample rate set so far.
    #[must_use]
    pub fn get_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The clock set so far.
    #[must_use]
    pub fn get_clock(&self) -> SharedClock {
        self.clock.clone()
    }

    /// The logger set so far.
    #[must_use]
    pub fn get_logger(&self) -> Logger {
        self.log.clone()
    }

    #[must_use]
    pub fn sample_rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// The pending-buffer cap in seconds (`max_buffer_s`).
    #[must_use]
    pub fn max_buffer_s(mut self, seconds: f64) -> Self {
        self.max_buffer_s = seconds;
        self
    }

    #[must_use]
    pub fn decoder(mut self, decoder: Box<dyn Decoder>) -> Self {
        self.decoder = decoder;
        self
    }

    #[must_use]
    pub fn spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = spawner;
        self
    }

    #[must_use]
    pub fn clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn logger(mut self, log: Logger) -> Self {
        self.log = log;
        self
    }

    #[must_use]
    pub fn policy(mut self, policy: RestartPolicy) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub fn build(self) -> SubprocessCapture {
        let now = self.clock.monotonic();
        SubprocessCapture {
            inner: Arc::new(Inner {
                buffer: BufferedAudioCapture::new(self.sample_rate, &self.name, self.max_buffer_s, self.clock.clone()),
                argv: Mutex::new(self.argv),
                name: self.name,
                sample_rate: self.sample_rate,
                decoder: Mutex::new(self.decoder),
                spawner: self.spawner,
                clock: self.clock,
                log: self.log,
                policy: self.policy,
                process: Mutex::new(None),
                reader: Mutex::new(None),
                stderr_tail: StderrTail::default(),
                dead: AtomicBool::new(false),
                restart_reason: Mutex::new(None),
                last_signal_at: Mutex::new(now),
                switch: Mutex::new(None),
                backoff_hook: Mutex::new(None),
                reader_thread: self.reader_thread,
            }),
        }
    }
}

struct Inner {
    buffer: BufferedAudioCapture,
    argv: Mutex<Vec<String>>,
    name: String,
    sample_rate: u32,
    decoder: Mutex<Box<dyn Decoder>>,
    spawner: Arc<dyn Spawner>,
    clock: SharedClock,
    log: Logger,
    policy: RestartPolicy,
    process: Mutex<Option<Arc<dyn ChildProcess>>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    stderr_tail: StderrTail,
    dead: AtomicBool,
    restart_reason: Mutex<Option<String>>,
    last_signal_at: Mutex<f64>,
    switch: Mutex<Option<Arc<dyn SourceSwitch>>>,
    backoff_hook: Mutex<Option<BackoffHook>>,
    reader_thread: bool,
}

fn locked<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// PCM from a child's stdout into a [`BufferedAudioCapture`], with the restart policy.
///
/// A cheap handle: clones share the capture.
#[derive(Clone)]
pub struct SubprocessCapture {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SubprocessCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubprocessCapture").field("name", &self.inner.name).finish_non_exhaustive()
    }
}

impl SubprocessCapture {
    /// A capture of `argv` named `name` (the log prefix), with Python's defaults: 16 kHz,
    /// a 600 s cap, raw PCM, real processes, the system clock, stderr.
    #[must_use]
    pub fn builder(argv: Vec<String>, name: &str) -> CaptureBuilder {
        CaptureBuilder {
            argv,
            name: name.to_owned(),
            sample_rate: SAMPLE_RATE,
            max_buffer_s: 600.0,
            decoder: Box::new(PcmDecoder::new()),
            spawner: Arc::new(OsSpawner),
            clock: SystemClock::shared(),
            log: Logger::stderr(),
            policy: RestartPolicy::default(),
            reader_thread: true,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate
    }

    /// The shared buffer.
    #[must_use]
    pub fn buffer(&self) -> &BufferedAudioCapture {
        &self.inner.buffer
    }

    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        locked(&self.inner.argv).clone()
    }

    /// The command of the next spawn.
    pub fn set_argv(&self, argv: Vec<String>) {
        *locked(&self.inner.argv) = argv;
    }

    #[must_use]
    pub fn logger(&self) -> &Logger {
        &self.inner.log
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.inner.buffer.running()
    }

    /// Sets the running flag only (tests stop the loop with it, as Python's do).
    pub fn set_running(&self, running: bool) {
        self.inner.buffer.set_running(running);
    }

    /// The capture is lost: fast respawns are spent, slow ones go on.
    #[must_use]
    pub fn dead(&self) -> bool {
        self.inner.dead.load(Ordering::SeqCst)
    }

    /// Marks the capture lost or not (`_dead`), for tests.
    pub fn set_dead(&self, dead: bool) {
        self.inner.dead.store(dead, Ordering::SeqCst);
    }

    /// Seconds the child has given nothing but exact zeros (since its last start).
    #[must_use]
    pub fn digital_silence_s(&self) -> f64 {
        self.inner.clock.monotonic() - *locked(&self.inner.last_signal_at)
    }

    /// Sets the time of the last nonzero sample (`_last_signal_at`), for tests.
    pub fn set_last_signal_at(&self, monotonic: f64) {
        *locked(&self.inner.last_signal_at) = monotonic;
    }

    /// Asks the reader thread to restart the child at its next turn.
    pub fn request_restart(&self, reason: &str) {
        *locked(&self.inner.restart_reason) = Some(reason.to_owned());
    }

    fn take_restart_request(&self) -> Option<String> {
        locked(&self.inner.restart_reason).take()
    }

    /// Hooks a planned source switch into the loop.
    pub fn set_source_switch(&self, switch: Option<Arc<dyn SourceSwitch>>) {
        *locked(&self.inner.switch) = switch;
    }

    /// Hooks the respawn pauses (tests).
    pub fn set_backoff_hook(&self, hook: Option<BackoffHook>) {
        *locked(&self.inner.backoff_hook) = hook;
    }

    /// The current child.
    #[must_use]
    pub fn process(&self) -> Option<Arc<dyn ChildProcess>> {
        locked(&self.inner.process).clone()
    }

    /// Replaces the current child (tests).
    pub fn set_process(&self, process: Option<Arc<dyn ChildProcess>>) {
        *locked(&self.inner.process) = process;
    }

    /// The last stderr lines of the current child.
    #[must_use]
    pub fn stderr_tail(&self) -> StderrTail {
        self.inner.stderr_tail.clone()
    }

    pub fn get_chunk(&self, duration: f64) -> Option<(Vec<f32>, f64)> {
        self.inner.buffer.get_chunk(duration)
    }

    pub fn clear_buffer(&self) {
        self.inner.buffer.clear_buffer();
    }

    pub fn mark_source_boundary(&self) {
        self.inner.buffer.mark_source_boundary();
    }

    #[must_use]
    pub fn source_epoch(&self) -> u64 {
        self.inner.buffer.source_epoch()
    }

    #[must_use]
    pub fn chunk_epoch(&self) -> u64 {
        self.inner.buffer.chunk_epoch()
    }

    /// Feeds bytes through the decoder as one read would (`_decode_read`).
    pub fn decode_read(&self, data: &[u8]) -> Vec<(Vec<f32>, f64)> {
        let ctx = DecodeContext {
            wall: self.inner.clock.wall(),
            silence_s: self.digital_silence_s(),
            name: &self.inner.name,
            sample_rate: self.inner.sample_rate,
            log: &self.inner.log,
        };
        locked(&self.inner.decoder).decode(data, &ctx)
    }

    /// Tells the decoder a new child starts (`_on_new_child`).
    pub fn on_new_child(&self) {
        locked(&self.inner.decoder).on_new_child();
    }

    /// `_sleep_while_running`: sleeps `delay`, waking on `stop()`.
    pub fn sleep_while_running(&self, delay: f64) {
        let clock = &self.inner.clock;
        let deadline = clock.monotonic() + delay;
        while self.running() {
            let remaining = deadline - clock.monotonic();
            if remaining <= 0.0 {
                return;
            }
            clock.sleep(remaining.min(0.2));
        }
    }

    /// `_stderr_suffix`: the child's last stderr lines, ready to append to a message.
    #[must_use]
    pub fn stderr_suffix(&self) -> String {
        let lines = self.inner.stderr_tail.lines();
        let start = lines.len().saturating_sub(STDERR_REPORT_LINES);
        let parts: Vec<&str> =
            lines[start..].iter().map(|l| recog_core::py::strip(l)).filter(|l| !l.is_empty()).collect();
        if parts.is_empty() { String::new() } else { format!(" (stderr: {})", parts.join(" | ")) }
    }

    fn mark_lost(&self, reason: &str) {
        self.set_dead(true);
        self.inner.log.line(&format!("[{}] {reason}{}", self.inner.name, self.stderr_suffix()));
    }

    fn terminate_process(&self) {
        if let Some(process) = self.process() {
            process.reap(1.0);
        }
    }

    /// `_restart_process`: the child is replaced and must survive the startup wait.
    ///
    /// # Errors
    /// The spawn failed, or the new child died during the wait (with its stderr).
    pub fn restart_process(&self) -> Result<(), String> {
        self.terminate_process();
        self.on_new_child();
        let process = self.inner.spawner.spawn(&self.argv()).map_err(|e| e.to_string())?;
        self.set_process(Some(process.clone()));
        self.set_last_signal_at(self.inner.clock.monotonic());
        self.inner.clock.sleep(self.inner.policy.startup_wait_s);
        if process.poll().is_some() {
            return Err(format!("Audio capture failed to restart: {}", process.read_stderr()));
        }
        self.inner.stderr_tail.clear();
        process.drain_stderr(self.inner.stderr_tail.clone());
        self.inner.log.line(&format!("[{}] Restarted (PID: {})", self.inner.name, process.pid()));
        Ok(())
    }

    /// `_respawn_dead_child`: the pause and the respawn of a dead child; the new count
    /// of fast attempts, None when `stop()` came during the pause.
    fn respawn_dead_child(&self, mut restart_count: u32) -> Option<u32> {
        let policy = &self.inner.policy;
        let name = &self.inner.name;
        let delay = if restart_count >= policy.max_restarts {
            if !self.dead() {
                self.mark_lost(&format!(
                    "Max restarts ({}) reached, audio lost — retrying every {:.0}s",
                    policy.max_restarts, policy.lost_retry_s
                ));
            }
            policy.lost_retry_s
        } else {
            let index = (restart_count as usize).min(policy.backoff_s.len().saturating_sub(1));
            let delay = policy.backoff_s.get(index).copied().unwrap_or(0.0);
            restart_count += 1;
            self.inner.log.line(&format!(
                "[{name}] Process died, restarting... (attempt {restart_count}){}",
                self.stderr_suffix()
            ));
            delay
        };
        let hook = locked(&self.inner.backoff_hook).clone();
        if let Some(hook) = hook {
            hook(delay, self);
        }
        self.sleep_while_running(delay);
        if !self.running() {
            return None;
        }
        if let Err(e) = self.restart_process() {
            self.inner.log.rate_limited(&format!("capture-restart-{name}"), &format!("[{name}] Restart failed: {e}"));
        }
        Some(restart_count)
    }

    /// `_reader_loop`, run on the calling thread until the capture stops.
    pub fn run_reader_loop(&self) {
        let name = self.inner.name.clone();
        let bytes_per_chunk = self.inner.sample_rate as usize * BYTES_PER_SAMPLE / 10;
        let mut restart_count: u32 = 0;
        while self.running() {
            let switch = locked(&self.inner.switch).clone();
            if let Some(switch) = switch
                && let Some(source) = switch.take_pending()
            {
                match switch.apply(self, &source) {
                    Ok(()) => restart_count = 0,
                    Err(e) => self.inner.log.rate_limited(
                        &format!("capture-switch-{name}"),
                        &format!("[{name}] source switch to {source} failed: {e}"),
                    ),
                }
                continue;
            }
            if let Some(reason) = self.take_restart_request() {
                self.inner.log.line(&format!("[{name}] {reason}, restarting the capture process"));
                match self.restart_process() {
                    Ok(()) => restart_count = 0,
                    Err(e) => self
                        .inner
                        .log
                        .rate_limited(&format!("capture-restart-{name}"), &format!("[{name}] Restart failed: {e}")),
                }
                continue;
            }
            let process = match self.process() {
                Some(p) if p.poll().is_none() => p,
                _ => {
                    match self.respawn_dead_child(restart_count) {
                        Some(count) => restart_count = count,
                        None => return,
                    }
                    continue;
                }
            };
            match process.read(bytes_per_chunk, self.inner.policy.read_wait_timeout_s) {
                ReadOutcome::Timeout => {}
                ReadOutcome::Eof => self.inner.clock.sleep(self.inner.policy.read_idle_sleep_s),
                ReadOutcome::Data(data) => {
                    restart_count = 0;
                    if self.dead() {
                        self.set_dead(false);
                        self.inner.log.line(&format!("[{name}] Audio recovered"));
                    }
                    for (samples, captured_at) in self.decode_read(&data) {
                        if samples.iter().any(|&s| s != 0.0) {
                            self.set_last_signal_at(self.inner.clock.monotonic());
                        }
                        let dropped = self.inner.buffer.append(&samples, captured_at);
                        if dropped > 0 {
                            #[allow(clippy::cast_precision_loss)]
                            let lost = dropped as f64 / f64::from(self.inner.sample_rate);
                            self.inner.log.rate_limited(
                                &format!("capture-overflow-{name}"),
                                &format!("[{name}] ASR unavailable too long — dropped {lost:.1}s of oldest audio"),
                            );
                        }
                    }
                }
            }
        }
    }

    /// `start()`: spawns the child and the reader thread; a child dead on its first
    /// spawn is left to the loop's backoff.
    ///
    /// # Errors
    /// The executable could not be started at all (Python raises too).
    pub fn start(&self) -> io::Result<()> {
        if self.running() {
            return Ok(());
        }
        self.on_new_child();
        let process = self.inner.spawner.spawn(&self.argv())?;
        self.set_process(Some(process.clone()));
        self.set_last_signal_at(self.inner.clock.monotonic());
        self.set_running(true);
        // `_dead` stays: a lost capture is recovered only when data comes.
        self.clear_buffer();
        if self.inner.reader_thread {
            let me = self.clone();
            let handle = std::thread::Builder::new().name(format!("capture-{}", self.inner.name)).spawn(move || {
                me.run_reader_loop();
            })?;
            *locked(&self.inner.reader) = Some(handle);
        }
        self.inner.clock.sleep(self.inner.policy.startup_wait_s);
        let name = &self.inner.name;
        if process.poll().is_some() {
            let text = process.read_stderr();
            for line in recog_core::py::splitlines(&text) {
                if !recog_core::py::strip(line).is_empty() {
                    self.inner.stderr_tail.push(line.to_owned());
                }
            }
            self.inner.log.line(&format!("[{name}] Failed to start, retrying in background{}", self.stderr_suffix()));
            return Ok(());
        }
        self.inner.stderr_tail.clear();
        process.drain_stderr(self.inner.stderr_tail.clone());
        self.inner.log.line(&format!("[{name}] Started (PID: {})", process.pid()));
        Ok(())
    }

    /// `stop()`: the child is reaped, also one the reader spawned while stopping.
    pub fn stop(&self) {
        let reader_present = locked(&self.inner.reader).is_some();
        if !self.running() && self.process().is_none() && !reader_present {
            return;
        }
        self.set_running(false);
        let snapshot = self.process();
        if let Some(process) = &snapshot {
            process.reap(2.0);
        }
        let reader = locked(&self.inner.reader).take();
        if let Some(handle) = reader {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        if let Some(late) = self.process()
            && !snapshot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &late))
        {
            late.reap(2.0);
        }
        self.set_process(None);
        self.inner.log.line(&format!("[{}] Stopped", self.inner.name));
    }

    /// Installs a reader thread handle (tests that run the loop themselves).
    pub fn set_reader(&self, handle: Option<JoinHandle<()>>) {
        *locked(&self.inner.reader) = handle;
    }
}
