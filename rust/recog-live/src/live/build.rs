//! Building the captures `LiveTranscriber.__init__` builds: the echo stage
//! (`create_echo_stage`), the microphone (`create_mic_capture`) and the system audio
//! (`create_system_capture` or a device).
//!
//! Known differences from Python, which captures some devices in its own process
//! through sounddevice: on Linux the microphone is `parec` on the default source (or
//! the named one) instead of PortAudio's `pulse` device, so it does not follow a change
//! of the default source until the capture is restarted; `device:<id>` system audio is
//! the microphone worker on the named input on a Mac and `parec` on the named source
//! on Linux.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use recog_core::install_config::InstallConfig;

use crate::capture::backend::{SystemCaptureSettings, create_system_capture};
use crate::capture::buffer::max_buffer_s;
use crate::capture::framed::{MicDecoder, child_mic_capture, mic_worker_argv};
use crate::capture::{SubprocessCapture, parec_argv};
use crate::config::SAMPLE_RATE;
use crate::echo::EchoSink;
use crate::echo::stage::EchoStage;
use crate::live::parts::LiveCapture;
use crate::log::Logger;

/// The microphone after `--mic-device` is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicChoice {
    /// `--mic-device none`.
    Off,
    /// No device found: the system's default input.
    Default,
    /// The input of this name.
    Named(String),
}

/// What the captures are built from.
pub struct CaptureSettings<'a> {
    pub repo_root: &'a Path,
    pub config: &'a InstallConfig,
    /// The interpreter of the Python microphone worker.
    pub python: PathBuf,
    /// `sys.platform`.
    pub platform: &'a str,
    pub log: Logger,
}

/// The two captures, either possibly absent.
pub type Captures = (Option<Arc<dyn LiveCapture>>, Option<Arc<dyn LiveCapture>>);

/// `create_echo_stage`: a stage when it is switched on and the system audio is
/// ScreenCaptureKit, the only reference it can use.
#[must_use]
pub fn create_echo_stage(system_audio: &str, config: &InstallConfig) -> Option<Arc<dyn EchoSink>> {
    if !config.live.echo_cancel || system_audio != "screencapture" {
        return None;
    }
    Some(Arc::new(EchoStage::new()))
}

/// `[live] mic_worker`, except that an installation without Python
/// (`[install] runtime = "rust"`) has only the Rust worker.
#[must_use]
pub fn mic_worker_choice(config: &InstallConfig) -> &str {
    if config.install.runtime == "rust" { "rust" } else { &config.live.mic_worker }
}

/// `create_mic_capture`: the worker child on a Mac, `parec` elsewhere.
///
/// # Errors
/// `[live] mic_worker` holds neither "python" nor "rust".
pub fn create_mic_capture(
    device: Option<&str>,
    echo_stage: Option<Arc<dyn EchoSink>>,
    settings: &CaptureSettings<'_>,
) -> Result<Arc<dyn LiveCapture>, String> {
    let cap = max_buffer_s(settings.config);
    if settings.platform == "darwin" {
        let argv = mic_worker_argv(
            mic_worker_choice(settings.config),
            settings.repo_root,
            &settings.python,
            device,
            SAMPLE_RATE,
            &settings.log,
        )?;
        let capture =
            child_mic_capture(argv, echo_stage).sample_rate(SAMPLE_RATE).max_buffer_s(cap).logger(settings.log.clone());
        return Ok(Arc::new(capture.build()));
    }
    let argv = parec_argv(device.unwrap_or("@DEFAULT_SOURCE@"), SAMPLE_RATE);
    let capture =
        SubprocessCapture::builder(argv, "mic").sample_rate(SAMPLE_RATE).max_buffer_s(cap).logger(settings.log.clone());
    Ok(Arc::new(capture.build()))
}

/// The system audio of `device:<name>`: an input device as the microphone worker
/// opens it (Mac), or a PulseAudio source (Linux).
///
/// # Errors
/// As [`create_mic_capture`].
pub fn create_device_capture(name: &str, settings: &CaptureSettings<'_>) -> Result<Arc<dyn LiveCapture>, String> {
    let cap = max_buffer_s(settings.config);
    let builder = if settings.platform == "darwin" {
        let argv = mic_worker_argv(
            mic_worker_choice(settings.config),
            settings.repo_root,
            &settings.python,
            Some(name),
            SAMPLE_RATE,
            &settings.log,
        )?;
        SubprocessCapture::builder(argv, "system").decoder(Box::new(MicDecoder::new(None)))
    } else {
        SubprocessCapture::builder(parec_argv(name, SAMPLE_RATE), "system")
    };
    Ok(Arc::new(builder.sample_rate(SAMPLE_RATE).max_buffer_s(cap).logger(settings.log.clone()).build()))
}

/// The captures of `LiveTranscriber.__init__` for a resolved system backend.
///
/// # Errors
/// A capture cannot be built (no helper, a bad setting).
pub fn build_captures(system_audio: &str, mic: &MicChoice, settings: &CaptureSettings<'_>) -> Result<Captures, String> {
    let named = match mic {
        MicChoice::Named(name) => Some(name.as_str()),
        _ => None,
    };
    let mic_enabled = *mic != MicChoice::Off;
    if system_audio == "screencapture" || system_audio == "pulse" {
        let mut mic_capture = None;
        let mut echo = None;
        if let Some(name) = named {
            echo = create_echo_stage(system_audio, settings.config);
            mic_capture = Some(create_mic_capture(Some(name), echo.clone(), settings)?);
        }
        let system = create_system_capture(
            system_audio,
            SAMPLE_RATE,
            echo,
            &SystemCaptureSettings {
                repo_root: settings.repo_root,
                pulse_monitor_source: &settings.config.live.pulse_monitor_source,
                max_buffer_s: max_buffer_s(settings.config),
                log: settings.log.clone(),
            },
        )?;
        return Ok((mic_capture, Some(Arc::new(system))));
    }
    if let Some(device) = system_audio.strip_prefix("device:") {
        let system = create_device_capture(device, settings)?;
        let mic_capture = named.map(|name| create_mic_capture(Some(name), None, settings)).transpose()?;
        return Ok((mic_capture, Some(system)));
    }
    if mic_enabled {
        return Ok((Some(create_mic_capture(named, None, settings)?), None));
    }
    Ok((None, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_without_python_when_choosing_the_mic_worker_then_rust_whatever_mic_worker_says() {
        // given
        let mut config = InstallConfig::default();
        // when / then
        assert_eq!(mic_worker_choice(&config), "python");
        config.live.mic_worker = "rust".into();
        assert_eq!(mic_worker_choice(&config), "rust");
        config.live.mic_worker = "python".into();
        config.install.runtime = "rust".into();
        assert_eq!(mic_worker_choice(&config), "rust");
    }
}
