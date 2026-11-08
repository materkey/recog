//! System audio on Linux: `parec` from the default sink's monitor, in place of
//! `recog/system_audio.py`'s `PulseAudioCapture`.
//!
//! `@DEFAULT_MONITOR@` is resolved once, when the stream is created, and does not
//! follow a change of the default sink, so without a pinned monitor the capture pins
//! the current `<sink>.monitor` before spawning `parec`, polls `pactl
//! get-default-sink` and reconnects on a change: a planned switch that marks the
//! source boundary and spends none of the crash-retry budget. When `pactl` did not
//! answer at the start, the first successful poll reconnects to a concrete monitor
//! rather than taking the then-current sink as its baseline.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::capture::subprocess::{CaptureBuilder, SourceSwitch, SubprocessCapture};
use crate::config::SAMPLE_RATE;
use crate::log::Logger;

/// The monitor of whatever sink is the default when the stream is created.
pub const PULSE_DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";
/// How often the default sink is polled (seconds).
pub const PULSE_POLL_INTERVAL_S: f64 = 2.0;
/// The timeout of one `pactl` call (seconds).
pub const PULSE_CMD_TIMEOUT_S: f64 = 2.0;

/// `parec`'s argv: raw mono float32 from the monitor `source`.
///
/// Without `--device` parec records the default *source* (the microphone).
#[must_use]
pub fn parec_argv(source: &str, sample_rate: u32) -> Vec<String> {
    vec![
        "parec".into(),
        "--raw".into(),
        "--format=float32le".into(),
        format!("--rate={sample_rate}"),
        "--channels=1".into(),
        "--latency-msec=100".into(),
        format!("--device={source}"),
    ]
}

/// The initial source: the pinned monitor (`[live] pulse_monitor_source`), stripped,
/// or `@DEFAULT_MONITOR@` when it is blank.
#[must_use]
pub fn resolve_pulse_monitor_source(pinned: &str) -> String {
    let pinned = recog_core::py::strip(pinned);
    if pinned.is_empty() { PULSE_DEFAULT_MONITOR.to_owned() } else { pinned.to_owned() }
}

/// What `pactl` printed and how it exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PactlOutput {
    pub returncode: i32,
    pub stdout: String,
}

/// Runs `pactl get-default-sink` (injectable, as Python's `run=`).
pub trait PactlRunner: Send + Sync {
    /// # Errors
    /// `pactl` could not run or timed out, as the text of Python's exception.
    fn get_default_sink(&self, timeout_s: f64) -> Result<PactlOutput, String>;
}

/// The real `pactl`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsPactl;

impl PactlRunner for OsPactl {
    fn get_default_sink(&self, timeout_s: f64) -> Result<PactlOutput, String> {
        let mut child = Command::new("pactl")
            .arg("get-default-sink")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_s);
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "Command '['pactl', 'get-default-sink']' timed out after {} seconds",
                    recog_core::py::float_repr(timeout_s)
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut stdout = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            let _ = pipe.read_to_string(&mut stdout);
        }
        Ok(PactlOutput { returncode: status.code().unwrap_or(-1), stdout })
    }
}

/// The default-sink watch's state, the loop's [`SourceSwitch`].
pub struct PulseState {
    runner: Arc<dyn PactlRunner>,
    log: Logger,
    sample_rate: u32,
    current_source: Mutex<String>,
    pending: Mutex<Option<String>>,
    last_default_sink: Mutex<Option<String>>,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PulseState {
    /// `_query_default_sink`: the default sink's name, None when pactl did not answer.
    pub fn query_default_sink(&self) -> Option<String> {
        let result = match self.runner.get_default_sink(PULSE_CMD_TIMEOUT_S) {
            Ok(result) => result,
            Err(e) => {
                self.log.rate_limited("pulse-default-sink", &format!("[pulse] pactl get-default-sink failed: {e}"));
                return None;
            }
        };
        if result.returncode != 0 {
            self.log.rate_limited(
                "pulse-default-sink",
                &format!("[pulse] pactl get-default-sink exited {}", result.returncode),
            );
            return None;
        }
        let sink = recog_core::py::strip(&result.stdout);
        if sink.is_empty() || sink.starts_with('@') {
            return None;
        }
        Some(sink.to_owned())
    }

    /// `_poll_default_sink_once`.
    pub fn poll_default_sink_once(&self) {
        let Some(sink) = self.query_default_sink() else { return };
        let mut last = locked(&self.last_default_sink);
        match last.as_deref() {
            None => {
                *last = Some(sink.clone());
                if *locked(&self.current_source) == PULSE_DEFAULT_MONITOR {
                    // The pin at the start did not happen: reconnect to a concrete
                    // monitor instead of taking the current sink as the baseline.
                    *locked(&self.pending) = Some(format!("{sink}.monitor"));
                }
            }
            Some(previous) if previous == sink => {}
            Some(_) => {
                *last = Some(sink.clone());
                *locked(&self.pending) = Some(format!("{sink}.monitor"));
            }
        }
    }

    /// `_pin_initial_source`: `@DEFAULT_MONITOR@` resolved to a concrete monitor before
    /// `parec` starts; the argv for it, None when pactl did not answer.
    pub fn pin_initial_source(&self) -> Option<Vec<String>> {
        let sink = self.query_default_sink()?;
        let source = format!("{sink}.monitor");
        *locked(&self.last_default_sink) = Some(sink);
        *locked(&self.current_source) = source.clone();
        Some(parec_argv(&source, self.sample_rate))
    }

    #[must_use]
    pub fn current_source(&self) -> String {
        locked(&self.current_source).clone()
    }

    #[must_use]
    pub fn last_default_sink(&self) -> Option<String> {
        locked(&self.last_default_sink).clone()
    }
}

impl SourceSwitch for PulseState {
    fn take_pending(&self) -> Option<String> {
        let pending = locked(&self.pending).take()?;
        if pending == *locked(&self.current_source) { None } else { Some(pending) }
    }

    fn apply(&self, capture: &SubprocessCapture, source: &str) -> Result<(), String> {
        capture.set_argv(parec_argv(source, self.sample_rate));
        *locked(&self.current_source) = source.to_owned();
        capture.mark_source_boundary();
        self.log.line(&format!("[pulse] Default sink changed, reconnecting to {source}"));
        capture.restart_process()
    }
}

/// `parec` from the default sink's monitor, following the default sink.
pub struct PulseAudioCapture {
    capture: SubprocessCapture,
    state: Arc<PulseState>,
    follow: bool,
    poll_interval_s: f64,
    watch: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for PulseAudioCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PulseAudioCapture").field("follow", &self.follow).finish_non_exhaustive()
    }
}

/// The base of a pulse capture: named `pulse`, Python's defaults otherwise.
#[must_use]
pub fn pulse_builder() -> CaptureBuilder {
    SubprocessCapture::builder(Vec::new(), "pulse").sample_rate(SAMPLE_RATE)
}

impl PulseAudioCapture {
    /// A capture of `source`, or of the pinned monitor, or following the default sink
    /// when both are empty (`source=None, pinned=""` in Python).
    #[must_use]
    pub fn new(
        base: CaptureBuilder,
        source: Option<&str>,
        pinned: &str,
        runner: Arc<dyn PactlRunner>,
        poll_interval_s: f64,
    ) -> Self {
        let resolved = source.map_or_else(|| resolve_pulse_monitor_source(pinned), ToOwned::to_owned);
        let sample_rate = base.get_sample_rate();
        let log = base.get_logger();
        let capture = base.argv(parec_argv(&resolved, sample_rate)).build();
        let follow = source.is_none() && resolved == PULSE_DEFAULT_MONITOR;
        let state = Arc::new(PulseState {
            runner,
            log,
            sample_rate,
            current_source: Mutex::new(resolved),
            pending: Mutex::new(None),
            last_default_sink: Mutex::new(None),
        });
        capture.set_source_switch(Some(state.clone()));
        Self { capture, state, follow, poll_interval_s, watch: Mutex::new(None) }
    }

    /// The underlying capture.
    #[must_use]
    pub fn capture(&self) -> &SubprocessCapture {
        &self.capture
    }

    /// The default-sink watch.
    #[must_use]
    pub fn state(&self) -> &Arc<PulseState> {
        &self.state
    }

    /// Whether the capture follows the default sink (no pinned monitor).
    #[must_use]
    pub fn follows_default_sink(&self) -> bool {
        self.follow
    }

    /// `_pin_initial_source`, applied to the capture's argv.
    pub fn pin_initial_source(&self) {
        if let Some(argv) = self.state.pin_initial_source() {
            self.capture.set_argv(argv);
        }
    }

    /// Whether the watch thread runs.
    #[must_use]
    pub fn watching(&self) -> bool {
        locked(&self.watch).as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Pins the monitor, starts the capture and the default-sink watch.
    ///
    /// # Errors
    /// `parec` could not be started at all.
    pub fn start(&self) -> std::io::Result<()> {
        self.start_with_watch(true)
    }

    /// As [`start`](Self::start), the watch thread only when `watch` (tests poll by
    /// hand, as Python's replace `_watch_loop`).
    ///
    /// # Errors
    /// `parec` could not be started at all.
    pub fn start_with_watch(&self, watch: bool) -> std::io::Result<()> {
        if self.follow && !self.capture.running() {
            self.pin_initial_source();
        }
        self.capture.start()?;
        let mut slot = locked(&self.watch);
        if self.follow && watch && slot.is_none() {
            let capture = self.capture.clone();
            let state = self.state.clone();
            let interval = self.poll_interval_s;
            *slot = Some(std::thread::Builder::new().name("pulse-watch".into()).spawn(move || {
                while capture.running() {
                    state.poll_default_sink_once();
                    capture.sleep_while_running(interval);
                }
            })?);
        }
        Ok(())
    }

    /// Stops the capture and the watch.
    pub fn stop(&self) {
        self.capture.stop();
        if let Some(handle) = locked(&self.watch).take() {
            let deadline = Instant::now() + Duration::from_secs_f64(PULSE_POLL_INTERVAL_S + 1.0);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}
