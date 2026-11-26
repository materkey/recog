//! The refine worker, in place of `recog/refine.py`.
//!
//! [`RefineWorker`] reruns ASR with word timestamps on a trailing per-pid PCM window
//! ([`RefineTask`], built by `PerSpeakerIngest::get_refine_candidate`) and hands the
//! result to `apply_fn`, which replaces live-segment text with `refined: true`.
//!
//! Threading is Python's (design.md D11): one worker thread pops tasks from a queue,
//! takes the shared model lock unless the callback manages it
//! (`transcribe_manages_lock`, what live uses), runs `transcribe_fn`, and calls
//! `apply_fn` outside the lock. Live has priority: `set_live_pending` before it takes
//! the model, `clear_live_pending` after; in cooperative mode the worker waits for the
//! flag to clear before taking the lock, checks it again once it holds the lock, and
//! passes `should_abort` to `transcribe_fn`. A task that gives way
//! ([`AsrError::Aborted`]) or finds ASR unavailable
//! ([`AsrError::TemporarilyUnavailable`], requeued after a pause) goes back on the queue.
//!
//! With wavo there is no abort inside a decode (a refine takes 60-100 ms), so the
//! callback only checks `should_abort` before starting, as `WavoASR` does; the hook
//! stays in the API for a backend that can.
//!
//! Differences from Python:
//! - The model lock is a [`ModelLock`] (`parking_lot::ReentrantMutex`), Python's
//!   `threading.RLock`: the ASR may take it again inside a callback the worker runs
//!   under it.
//! - `stop(timeout)` waits at most `timeout` for the thread and then leaves it
//!   detached, as `Thread.join(timeout)` does.
//! - Error lines name the [`AsrError`] kind (`ValueError` for invalid input,
//!   `RuntimeError` otherwise) instead of the Python exception class; no traceback.
//! - `RefineTask`/`RefineResult` immutability is the borrow checker's: the worker
//!   hands out `&RefineResult` holding an `Arc<RefineTask>`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::ReentrantMutex;

use crate::asr::{AsrError, Transcription, Word};
use crate::clock::{SharedClock, SystemClock};
use crate::log::Logger;

/// The model lock shared by live and refine, reentrant like Python's `RLock`.
pub type ModelLock = Arc<ReentrantMutex<()>>;

/// A new, unheld model lock.
#[must_use]
pub fn new_model_lock() -> ModelLock {
    Arc::new(ReentrantMutex::new(()))
}

/// Trailing PCM window submitted to the refine worker for one pid.
#[derive(Debug, Clone, PartialEq)]
pub struct RefineTask {
    /// Participant id (the extension's stable per-speaker key).
    pub pid: String,
    /// Int16 mono PCM at the ASR sample rate.
    pub pcm_samples: Vec<i16>,
    /// Per-pid audio-clock offset of `pcm_samples[0]` (ms), to rebase word times.
    pub offset_base_ms: f64,
    /// `time.time()` when the ingest built the task.
    pub submitted_at: f64,
    /// The live process's meeting generation when the task was built.
    pub meeting_generation: i64,
}

/// What one refine run produced for a task.
#[derive(Debug, Clone, PartialEq)]
pub struct RefineResult {
    pub task: Arc<RefineTask>,
    pub text: String,
    /// Words with start/end in ms from the window's start.
    pub words: Vec<Word>,
}

/// How the worker yields to live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortMode {
    /// Wait for live before taking the model, recheck under it, pass `should_abort`.
    Cooperative,
    /// Run every task to its end; live waits at most one refine.
    Coarse,
}

impl AbortMode {
    /// The mode named as in Python (`"cooperative"`, `"coarse"`).
    ///
    /// # Errors
    /// Any other name, with Python's `ValueError` message.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "cooperative" => Ok(Self::Cooperative),
            "coarse" => Ok(Self::Coarse),
            other => Err(format!("abort_mode must be 'cooperative' or 'coarse', got {}", recog_core::py::repr(other))),
        }
    }

    /// The Python name of the mode.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cooperative => "cooperative",
            Self::Coarse => "coarse",
        }
    }
}

/// `transcribe_fn(pcm, should_abort=...)`: `should_abort` is `Some` in cooperative mode.
pub type TranscribeFn = Arc<dyn Fn(&[i16], Option<&dyn Fn() -> bool>) -> Result<Transcription, AsrError> + Send + Sync>;
/// `apply_fn(result)`, called outside the model lock; an `Err` is logged.
pub type ApplyFn = Arc<dyn Fn(&RefineResult) -> Result<(), String> + Send + Sync>;
/// The current meeting generation.
pub type GenerationGetter = Arc<dyn Fn() -> i64 + Send + Sync>;

/// A settable flag with a wait, Python's `threading.Event`.
#[derive(Debug, Default)]
pub(crate) struct Event {
    flag: Mutex<bool>,
    cond: Condvar,
}

impl Event {
    fn guard(&self) -> MutexGuard<'_, bool> {
        self.flag.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set(&self) {
        *self.guard() = true;
        self.cond.notify_all();
    }

    pub(crate) fn clear(&self) {
        *self.guard() = false;
    }

    pub(crate) fn is_set(&self) -> bool {
        *self.guard()
    }

    /// Waits until set or `timeout` passed; returns the flag.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let guard = self.guard();
        let (guard, _) =
            self.cond.wait_timeout_while(guard, timeout, |set| !*set).unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

/// Construction options of [`RefineWorker`], Python's keyword arguments.
#[derive(Clone)]
pub struct RefineOptions {
    pub abort_mode: AbortMode,
    /// The callback owns the model lock for local decode; the worker takes none.
    pub transcribe_manages_lock: bool,
    /// Old tasks are rejected before inference and not resubmitted after a switch.
    pub generation_getter: Option<GenerationGetter>,
    /// Sleeps the requeue pause.
    pub clock: SharedClock,
    pub logger: Logger,
    /// `queue.get` poll interval (s).
    pub poll_s: f64,
    /// Poll interval while waiting for live to release the model (s).
    pub live_poll_s: f64,
    /// Pause before requeueing a task ASR could not serve (s).
    pub unavailable_retry_delay_s: f64,
}

impl Default for RefineOptions {
    fn default() -> Self {
        Self {
            abort_mode: AbortMode::Cooperative,
            transcribe_manages_lock: false,
            generation_getter: None,
            clock: SystemClock::shared(),
            logger: Logger::stderr(),
            poll_s: RefineWorker::POLL_S,
            live_poll_s: RefineWorker::LIVE_POLL_S,
            unavailable_retry_delay_s: RefineWorker::UNAVAILABLE_RETRY_DELAY_S,
        }
    }
}

#[derive(Default)]
struct Queue {
    paused: bool,
    /// `None` is the wake-up sentinel `stop` puts.
    items: VecDeque<Option<Arc<RefineTask>>>,
}

struct Inner {
    model_lock: ModelLock,
    transcribe_fn: TranscribeFn,
    apply_fn: ApplyFn,
    options: RefineOptions,
    queue: Mutex<Queue>,
    queue_cond: Condvar,
    live_pending: AtomicBool,
    stopped: Event,
}

/// The serialised refine worker; see the module documentation.
pub struct RefineWorker {
    inner: Arc<Inner>,
    thread: Mutex<Option<(JoinHandle<()>, Arc<Event>)>>,
}

/// Why `transcribe_task` gave no result.
enum Outcome {
    Done(Transcription),
    NotCurrent,
}

impl RefineWorker {
    /// `queue.get` poll interval: small enough to notice a stop promptly.
    pub const POLL_S: f64 = 0.2;
    /// Poll of `live_pending` while waiting for live to release the model.
    pub const LIVE_POLL_S: f64 = 0.05;
    /// Pause before a task ASR could not serve goes back on the queue.
    pub const UNAVAILABLE_RETRY_DELAY_S: f64 = 2.0;

    #[must_use]
    pub fn new(model_lock: ModelLock, transcribe_fn: TranscribeFn, apply_fn: ApplyFn, options: RefineOptions) -> Self {
        let inner = Inner {
            model_lock,
            transcribe_fn,
            apply_fn,
            options,
            queue: Mutex::new(Queue::default()),
            queue_cond: Condvar::new(),
            live_pending: AtomicBool::new(false),
            stopped: Event::default(),
        };
        Self { inner: Arc::new(inner), thread: Mutex::new(None) }
    }

    #[must_use]
    pub fn abort_mode(&self) -> AbortMode {
        self.inner.options.abort_mode
    }

    /// Spawns the worker thread; no-op while one runs.
    pub fn start(&self) {
        let mut thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, finished)) = thread.as_ref()
            && !finished.is_set()
        {
            return;
        }
        self.inner.stopped.clear();
        let inner = self.inner.clone();
        let finished = Arc::new(Event::default());
        let done = finished.clone();
        let handle = std::thread::Builder::new()
            .name("RefineWorker".into())
            .spawn(move || {
                inner.run();
                done.set();
            })
            .unwrap_or_else(|e| panic!("cannot start the RefineWorker thread: {e}"));
        *thread = Some((handle, finished));
    }

    /// Enqueues a task; no-op while paused or stopped, or of another generation.
    pub fn submit(&self, task: impl Into<Arc<RefineTask>>) {
        self.inner.submit(task.into());
    }

    /// Pauses processing and drops the queued tasks; the next trigger of their pid
    /// resubmits with a fresh window.
    pub fn pause(&self) {
        let mut queue = self.inner.lock_queue();
        queue.paused = true;
        queue.items.clear();
    }

    /// Clears the pause; later submits enqueue again.
    pub fn resume(&self) {
        self.inner.lock_queue().paused = false;
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.inner.lock_queue().paused
    }

    /// Tasks waiting in the queue.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.inner.lock_queue().items.iter().filter(|t| t.is_some()).count()
    }

    /// The queued tasks in order, without taking them.
    #[must_use]
    pub fn queued_tasks(&self) -> Vec<Arc<RefineTask>> {
        self.inner.lock_queue().items.iter().flatten().cloned().collect()
    }

    /// Whether a worker thread is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.thread.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(|(_, f)| !f.is_set())
    }

    /// Signals the worker to exit, waits up to `timeout` for it, and drains the queue.
    pub fn stop(&self, timeout: Duration) {
        self.inner.stopped.set();
        {
            self.inner.lock_queue().items.push_back(None);
            self.inner.queue_cond.notify_all();
        }
        let taken = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some((handle, finished)) = taken
            && finished.wait(timeout)
        {
            let _ = handle.join();
        }
        self.inner.lock_queue().items.clear();
    }

    /// Live is about to take the model: in cooperative mode an in-flight refine gives way.
    pub fn set_live_pending(&self) {
        self.inner.live_pending.store(true, Ordering::SeqCst);
    }

    /// Live has released the model.
    pub fn clear_live_pending(&self) {
        self.inner.live_pending.store(false, Ordering::SeqCst);
    }

    /// Whether live has signalled it needs the model.
    #[must_use]
    pub fn live_pending(&self) -> bool {
        self.inner.live_pending.load(Ordering::SeqCst)
    }

    /// One task through the worker's steps on the calling thread (`_process_task`);
    /// errors other than the requeued ones are returned, as they propagate in Python.
    ///
    /// # Errors
    /// What `transcribe_fn` failed with, except `Aborted` and `TemporarilyUnavailable`.
    pub fn process_task(&self, task: impl Into<Arc<RefineTask>>) -> Result<(), AsrError> {
        self.inner.process_task(&task.into())
    }

    /// Takes the next queued task without processing it (tests and diagnostics).
    #[must_use]
    pub fn take_queued(&self) -> Option<Arc<RefineTask>> {
        let mut queue = self.inner.lock_queue();
        while let Some(item) = queue.items.pop_front() {
            if item.is_some() {
                return item;
            }
        }
        None
    }
}

impl Drop for RefineWorker {
    fn drop(&mut self) {
        if self.thread.get_mut().unwrap_or_else(PoisonError::into_inner).is_some() {
            self.stop(Duration::from_secs(2));
        }
    }
}

impl Inner {
    fn lock_queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn generation_matches(&self, task: &RefineTask) -> bool {
        self.options.generation_getter.as_ref().is_none_or(|g| task.meeting_generation == g())
    }

    fn submit(&self, task: Arc<RefineTask>) {
        if self.stopped.is_set() {
            return;
        }
        let mut queue = self.lock_queue();
        if queue.paused || !self.generation_matches(&task) {
            return;
        }
        queue.items.push_back(Some(task));
        self.queue_cond.notify_one();
    }

    fn task_is_current(&self, task: &RefineTask) -> bool {
        let queue = self.lock_queue();
        !self.stopped.is_set() && !queue.paused && self.generation_matches(task)
    }

    /// `queue.get(timeout)`: `None` on timeout, `Some(None)` for the sentinel.
    fn get(&self, timeout: Duration) -> Option<Option<Arc<RefineTask>>> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.lock_queue();
        loop {
            if let Some(item) = queue.items.pop_front() {
                return Some(item);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            queue = self.queue_cond.wait_timeout(queue, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn run(&self) {
        let poll = Duration::from_secs_f64(self.options.poll_s);
        while !self.stopped.is_set() {
            let Some(item) = self.get(poll) else { continue };
            let Some(task) = item else { break };
            if self.stopped.is_set() {
                break;
            }
            if self.lock_queue().paused {
                // pause() may have missed this one if it raced in between.
                continue;
            }
            if let Err(e) = self.process_task(&task) {
                self.options.logger.line(&format!("[RefineWorker] task processing raised {}: {e}", e.python_name()));
            }
        }
    }

    fn wait_for_live_released(&self) {
        let poll = Duration::from_secs_f64(self.options.live_poll_s);
        while self.live_pending.load(Ordering::SeqCst) {
            if self.stopped.is_set() {
                return;
            }
            self.stopped.wait(poll);
        }
    }

    fn transcribe_task(&self, task: &RefineTask) -> Result<Outcome, AsrError> {
        let cooperative = self.options.abort_mode == AbortMode::Cooperative;
        if cooperative {
            self.wait_for_live_released();
        }
        let _guard = (!self.options.transcribe_manages_lock).then(|| self.model_lock.lock());
        if !self.task_is_current(task) {
            return Ok(Outcome::NotCurrent);
        }
        if cooperative && self.live_pending.load(Ordering::SeqCst) {
            return Err(AsrError::Aborted);
        }
        let result = if cooperative {
            let should_abort = || self.live_pending.load(Ordering::SeqCst) || !self.task_is_current(task);
            (self.transcribe_fn)(&task.pcm_samples, Some(&should_abort))
        } else {
            (self.transcribe_fn)(&task.pcm_samples, None)
        };
        result.map(Outcome::Done)
    }

    fn process_task(&self, task: &Arc<RefineTask>) -> Result<(), AsrError> {
        if !self.task_is_current(task) {
            return Ok(());
        }
        let transcription = match self.transcribe_task(task) {
            Ok(Outcome::Done(t)) => t,
            Ok(Outcome::NotCurrent) => return Ok(()),
            Err(e) => return self.resubmit_after(task, e),
        };
        // pause() may have landed during the inference: no spurious promotions.
        if !self.task_is_current(task) {
            return Ok(());
        }
        let result = RefineResult { task: task.clone(), text: transcription.text, words: transcription.words };
        if let Err(e) = (self.apply_fn)(&result) {
            self.options.logger.line(&format!("[RefineWorker] apply_fn raised RuntimeError: {e}"));
        }
        Ok(())
    }

    /// A failed inference: an unavailable model (after the retry delay, unless the
    /// task went stale) and an abort put the task back; anything else is the error.
    fn resubmit_after(&self, task: &Arc<RefineTask>, error: AsrError) -> Result<(), AsrError> {
        match error {
            AsrError::TemporarilyUnavailable => {
                if self.task_is_current(task) {
                    self.options.clock.sleep(self.options.unavailable_retry_delay_s);
                }
            }
            AsrError::Aborted => {}
            e => return Err(e),
        }
        self.submit(task.clone());
        Ok(())
    }
}
