//! Choosing and building the system-audio capture, in place of
//! `resolve_system_audio_backend` and `create_system_capture`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::capture::framed::framed_system_capture;
use crate::capture::pulse::{OsPactl, PULSE_POLL_INTERVAL_S, PulseAudioCapture, pulse_builder};
use crate::capture::sck::{OsToolchain, find_or_build_sck_utility};
use crate::capture::subprocess::{CaptureBuilder, SubprocessCapture};
use crate::echo::EchoSink;
use crate::log::Logger;

/// Why no backend: `RuntimeError` (wrong system, parec missing) or `ValueError` (a name
/// that is no backend), with Python's messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    Runtime(String),
    Value(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::Runtime(m) | BackendError::Value(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for BackendError {}

/// `sys.platform` of this build: `darwin`, `linux` or the OS name.
#[must_use]
pub fn current_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// `resolve_system_audio_backend`: `auto` resolved for `platform`, an explicit choice
/// checked against it; `none` (or nothing) and `device:<id>` pass through.
///
/// # Errors
/// The backend cannot run here, or `spec` names none.
pub fn resolve_system_audio_backend(
    spec: Option<&str>,
    platform: &str,
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<String, BackendError> {
    let spec = match spec {
        None | Some("" | "none") => return Ok("none".into()),
        Some(s) if s.starts_with("device:") => return Ok(s.to_owned()),
        Some(s) => s,
    };
    let backend = choose_backend(spec, platform)?;
    if backend == "pulse" && which("parec").is_none() {
        return Err(BackendError::Runtime(
            "parec not found — install it: `apt install pulseaudio-utils` (Debian/Ubuntu) or `pacman -S libpulse` (Arch/Manjaro)"
                .into(),
        ));
    }
    Ok(backend)
}

/// The backend this platform captures with: ScreenCaptureKit on a Mac, the sound
/// server on Linux.
fn native_backend(platform: &str) -> Option<&'static str> {
    if platform == "darwin" {
        Some("screencapture")
    } else if platform.starts_with("linux") {
        Some("pulse")
    } else {
        None
    }
}

/// `spec` (`auto` or a backend's name) checked against the platform.
fn choose_backend(spec: &str, platform: &str) -> Result<String, BackendError> {
    let native = native_backend(platform);
    let platform_repr = recog_core::py::repr(platform);
    match spec {
        "auto" => native
            .map(str::to_owned)
            .ok_or_else(|| BackendError::Runtime(format!("no system-audio backend for platform {platform_repr}"))),
        "screencapture" | "pulse" if native == Some(spec) => Ok(spec.to_owned()),
        "screencapture" => Err(BackendError::Runtime(format!(
            "the screencapture backend needs macOS (ScreenCaptureKit), got platform {platform_repr} — use --system-audio pulse"
        ))),
        "pulse" => Err(BackendError::Runtime(format!(
            "the pulse backend needs Linux (PulseAudio/PipeWire), got platform {platform_repr} — use --system-audio screencapture"
        ))),
        other => Err(BackendError::Value(format!("unknown system audio backend: {}", recog_core::py::repr(other)))),
    }
}

/// A system capture: a plain or framed child, or `parec` following the default sink.
#[derive(Debug)]
pub enum SystemCapture {
    Subprocess(SubprocessCapture),
    Pulse(PulseAudioCapture),
}

impl SystemCapture {
    /// The capture underneath (buffer, restart policy, loss).
    #[must_use]
    pub fn capture(&self) -> &SubprocessCapture {
        match self {
            SystemCapture::Subprocess(c) => c,
            SystemCapture::Pulse(p) => p.capture(),
        }
    }

    /// # Errors
    /// The child could not be started at all.
    pub fn start(&self) -> std::io::Result<()> {
        match self {
            SystemCapture::Subprocess(c) => c.start(),
            SystemCapture::Pulse(p) => p.start(),
        }
    }

    pub fn stop(&self) {
        match self {
            SystemCapture::Subprocess(c) => c.stop(),
            SystemCapture::Pulse(p) => p.stop(),
        }
    }

    pub fn get_chunk(&self, duration: f64) -> Option<(Vec<f32>, f64)> {
        self.capture().get_chunk(duration)
    }
}

/// What `create_system_capture` reads from the installation.
pub struct SystemCaptureSettings<'a> {
    /// Where `sck_audio_capture` and its source are.
    pub repo_root: &'a Path,
    /// `[live] pulse_monitor_source`.
    pub pulse_monitor_source: &'a str,
    /// `[asr] pending_buffer_cap_s`.
    pub max_buffer_s: f64,
    pub log: Logger,
}

/// The screencapture argv: the helper, with `--frames` for an echo stage.
///
/// # Errors
/// No helper and no way to build one.
pub fn sck_argv(settings: &SystemCaptureSettings<'_>, framed: bool) -> Result<Vec<String>, String> {
    let helper =
        find_or_build_sck_utility(settings.repo_root, &OsToolchain, &settings.log).map_err(|e| e.to_string())?;
    Ok(if framed { vec![helper, "--frames".into()] } else { vec![helper] })
}

/// `create_system_capture`: the capture of an already resolved backend.
///
/// # Errors
/// An unknown backend (Python's `ValueError` message), or no screencapture helper.
pub fn create_system_capture(
    backend: &str,
    sample_rate: u32,
    echo_stage: Option<Arc<dyn EchoSink>>,
    settings: &SystemCaptureSettings<'_>,
) -> Result<SystemCapture, String> {
    let configure =
        |b: CaptureBuilder| b.sample_rate(sample_rate).max_buffer_s(settings.max_buffer_s).logger(settings.log.clone());
    match (backend, echo_stage) {
        ("screencapture", Some(stage)) => {
            let argv = sck_argv(settings, true)?;
            Ok(SystemCapture::Subprocess(configure(framed_system_capture(argv, stage)).build()))
        }
        ("screencapture", None) => {
            let argv = sck_argv(settings, false)?;
            Ok(SystemCapture::Subprocess(configure(SubprocessCapture::builder(argv, "screencapture")).build()))
        }
        ("pulse", _) => Ok(SystemCapture::Pulse(PulseAudioCapture::new(
            configure(pulse_builder()),
            None,
            settings.pulse_monitor_source,
            Arc::new(OsPactl),
            PULSE_POLL_INTERVAL_S,
        ))),
        (other, _) => Err(format!("unknown system audio backend: {}", recog_core::py::repr(other))),
    }
}
