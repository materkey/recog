//! The PulseAudio cases and the backend choice of tests/test_system_audio.py.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::fakes::{FakeChild, FakeSpawner, SR, Spawn};
use crate::capture::backend::{
    BackendError, SystemCaptureSettings, create_system_capture, resolve_system_audio_backend,
};
use crate::capture::pulse::{
    PULSE_DEFAULT_MONITOR, PactlOutput, PactlRunner, PulseAudioCapture, parec_argv, pulse_builder,
    resolve_pulse_monitor_source,
};
use crate::capture::subprocess::SourceSwitch as _;
use crate::clock::{ManualClock, SystemClock};
use crate::echo::EchoSink;
use crate::log::Logger;

#[test]
fn a_monitor_parec_argv_asks_for_raw_mono_float32_from_it() {
    let argv = parec_argv("alsa_output.pci.analog-stereo.monitor", SR);
    assert_eq!(argv[0], "parec");
    for flag in ["--raw", "--format=float32le", "--rate=16000", "--channels=1", "--latency-msec=100"] {
        assert!(argv.contains(&flag.to_owned()), "{flag}");
    }
    assert!(argv.contains(&"--device=alsa_output.pci.analog-stereo.monitor".to_owned()));
}

#[test]
fn rate_48000_parec_argv_follows_it() {
    assert!(parec_argv("x.monitor", 48_000).contains(&"--rate=48000".to_owned()));
}

#[test]
fn empty_pin_resolve_monitor_gives_the_default_monitor() {
    assert_eq!(resolve_pulse_monitor_source(""), PULSE_DEFAULT_MONITOR);
}

#[test]
fn padded_pin_resolve_monitor_gives_it_stripped() {
    assert_eq!(resolve_pulse_monitor_source("  bluez.monitor  "), "bluez.monitor");
}

#[test]
fn blank_pin_resolve_monitor_falls_back_to_the_default_monitor() {
    assert_eq!(resolve_pulse_monitor_source("   "), PULSE_DEFAULT_MONITOR);
}

/// `pactl` answers in order: a sink name, an exit code with its output, or an error.
pub enum Answer {
    Sink(&'static str),
    Exit(&'static str, i32),
    Error(&'static str),
}

pub struct ScriptedPactl {
    answers: Mutex<Vec<Answer>>,
    pub calls: Mutex<Vec<f64>>,
}

impl ScriptedPactl {
    pub fn new(answers: Vec<Answer>) -> Arc<Self> {
        Arc::new(Self { answers: Mutex::new(answers), calls: Mutex::new(Vec::new()) })
    }
}

impl PactlRunner for ScriptedPactl {
    fn get_default_sink(&self, timeout_s: f64) -> Result<PactlOutput, String> {
        self.calls.lock().unwrap().push(timeout_s);
        let mut answers = self.answers.lock().unwrap();
        let answer = if answers.is_empty() { Answer::Sink("") } else { answers.remove(0) };
        match answer {
            Answer::Sink(s) => Ok(PactlOutput { returncode: 0, stdout: format!("{s}\n") }),
            Answer::Exit(s, rc) => Ok(PactlOutput { returncode: rc, stdout: s.to_owned() }),
            Answer::Error(e) => Err(e.to_owned()),
        }
    }
}

fn pulse(answers: Vec<Answer>, pinned: &str) -> (PulseAudioCapture, Arc<ScriptedPactl>, Arc<FakeSpawner>) {
    let clock = ManualClock::new(1000.0);
    let pactl = ScriptedPactl::new(answers);
    let spawner = FakeSpawner::new(Vec::new(), || Spawn::Child(FakeChild::silent().arc()));
    let (log, _) = Logger::capture(clock.shared());
    let base = pulse_builder().clock(clock.shared()).logger(log).spawner(spawner.clone()).reader_thread(false);
    (PulseAudioCapture::new(base, None, pinned, pactl.clone(), 0.01), pactl, spawner)
}

#[test]
fn no_pin_pulse_capture_starts_on_the_default_monitor_and_follows() {
    let (cap, _, _) = pulse(Vec::new(), "");
    assert_eq!(cap.capture().argv(), parec_argv(PULSE_DEFAULT_MONITOR, SR));
    assert!(cap.follows_default_sink());
    assert_eq!(cap.capture().name(), "pulse");
}

#[test]
fn pinned_monitor_pulse_capture_does_not_follow_nor_ask_pactl() {
    let (cap, pactl, _) = pulse(Vec::new(), "bluez.monitor");
    assert_eq!(cap.capture().argv(), parec_argv("bluez.monitor", SR));
    assert!(!cap.follows_default_sink());
    assert!(pactl.calls.lock().unwrap().is_empty());
}

#[test]
fn default_sink_query_uses_the_injected_runner_with_a_timeout() {
    let (cap, pactl, _) = pulse(vec![Answer::Sink("alsa_output.a")], "");
    assert_eq!(cap.state().query_default_sink().as_deref(), Some("alsa_output.a"));
    assert!(pactl.calls.lock().unwrap()[0] > 0.0);
}

#[test]
fn pinned_on_a_then_b_default_sink_change_reconnects_once() {
    let (cap, _, spawner) =
        pulse(vec![Answer::Sink("sinkA"), Answer::Sink("sinkA"), Answer::Sink("sinkB"), Answer::Sink("sinkB")], "");
    let state = cap.state().clone();
    cap.pin_initial_source();

    state.poll_default_sink_once();
    assert_eq!(state.take_pending(), None);

    state.poll_default_sink_once();
    let pending = state.take_pending().unwrap();
    assert_eq!(pending, "sinkB.monitor");
    state.apply(cap.capture(), &pending).unwrap();

    state.poll_default_sink_once();
    assert_eq!(state.take_pending(), None);

    assert_eq!(*spawner.argvs.lock().unwrap(), vec![parec_argv("sinkB.monitor", SR)]);
    assert_eq!(cap.capture().argv(), parec_argv("sinkB.monitor", SR));
}

#[test]
fn default_sink_a_at_start_start_pins_its_monitor() {
    let (cap, _, _) = pulse(vec![Answer::Sink("sinkA")], "");
    cap.start_with_watch(false).unwrap();
    assert_eq!(cap.capture().argv(), parec_argv("sinkA.monitor", SR));
    assert_eq!(cap.state().last_default_sink().as_deref(), Some("sinkA"));
    cap.stop();
}

#[test]
fn pactl_silent_at_start_start_keeps_the_default_monitor() {
    let (cap, _, _) = pulse(vec![Answer::Exit("", 1)], "");
    cap.start_with_watch(false).unwrap();
    assert_eq!(cap.capture().argv(), parec_argv(PULSE_DEFAULT_MONITOR, SR));
    assert_eq!(cap.state().last_default_sink(), None);
    cap.stop();
}

#[test]
fn sink_changed_before_the_first_poll_the_poll_still_reconnects() {
    let (cap, _, _) = pulse(vec![Answer::Sink("sinkA"), Answer::Sink("sinkB")], "");
    cap.start_with_watch(false).unwrap();
    cap.state().poll_default_sink_once();
    assert_eq!(cap.state().take_pending().as_deref(), Some("sinkB.monitor"));
    cap.stop();
}

#[test]
fn failed_pin_then_sink_b_the_first_poll_reconnects_to_a_concrete_monitor() {
    let (cap, _, _) = pulse(vec![Answer::Exit("", 1), Answer::Sink("sinkB"), Answer::Sink("sinkB")], "");
    cap.start_with_watch(false).unwrap();
    assert_eq!(cap.capture().argv(), parec_argv(PULSE_DEFAULT_MONITOR, SR));
    let state = cap.state().clone();

    state.poll_default_sink_once();
    let pending = state.take_pending().unwrap();
    assert_eq!(pending, "sinkB.monitor");
    state.apply(cap.capture(), &pending).unwrap();

    state.poll_default_sink_once();
    assert_eq!(state.take_pending(), None);
    cap.stop();
}

#[test]
fn source_switch_source_epoch_grows() {
    let (cap, _, _) = pulse(Vec::new(), "");
    let before = cap.capture().source_epoch();
    cap.state().apply(cap.capture(), "sinkB.monitor").unwrap();
    assert_eq!(cap.capture().source_epoch(), before + 1);
}

#[test]
fn audio_of_the_old_source_switch_drops_the_buffer() {
    let (cap, _, _) = pulse(Vec::new(), "");
    cap.capture().buffer().append(&vec![1.0; SR as usize], 1000.0);
    cap.state().apply(cap.capture(), "sinkB.monitor").unwrap();
    assert!(cap.capture().buffer().is_empty());
    assert_eq!(cap.capture().buffer().buffer_start_ts(), None);
    assert_eq!(cap.capture().buffer().buffered_samples(), 0);
}

fn pactl_failure_keeps_the_stream(failure: Answer) {
    let (cap, _, spawner) = pulse(vec![Answer::Sink("sinkA"), Answer::Sink("sinkA"), failure], "");
    cap.pin_initial_source();
    cap.state().poll_default_sink_once();
    cap.state().poll_default_sink_once();
    assert_eq!(cap.state().take_pending(), None);
    assert_eq!(cap.capture().argv(), parec_argv("sinkA.monitor", SR));
    assert_eq!(cap.state().last_default_sink().as_deref(), Some("sinkA"));
    assert_eq!(spawner.count(), 0);
}

#[test]
fn pactl_timing_out_the_poll_keeps_the_current_stream() {
    pactl_failure_keeps_the_stream(Answer::Error(
        "Command '['pactl', 'get-default-sink']' timed out after 2.0 seconds",
    ));
}

#[test]
fn pactl_exiting_1_the_poll_keeps_the_current_stream() {
    pactl_failure_keeps_the_stream(Answer::Exit("", 1));
}

#[test]
fn pactl_printing_nothing_the_poll_keeps_the_current_stream() {
    pactl_failure_keeps_the_stream(Answer::Exit("", 0));
}

#[test]
fn running_pulse_capture_stop_ends_the_default_sink_watch() {
    let pactl = ScriptedPactl::new((0..100).map(|_| Answer::Sink("sinkA")).collect());
    let spawner = FakeSpawner::new(Vec::new(), || Spawn::Child(FakeChild::silent().arc()));
    let (log, _) = Logger::capture(SystemClock::shared());
    let base = pulse_builder().logger(log).spawner(spawner).reader_thread(false);
    let cap = PulseAudioCapture::new(base, None, "", pactl, 0.01);

    cap.start().unwrap();
    assert!(cap.watching());
    cap.stop();
    assert!(!cap.watching());
}

#[test]
fn pinned_monitor_start_neither_watches_nor_asks_pactl() {
    let (cap, pactl, _) = pulse(Vec::new(), "bluez.monitor");
    cap.start().unwrap();
    assert!(!cap.watching());
    assert!(pactl.calls.lock().unwrap().is_empty());
    cap.stop();
}

fn which_none(_: &str) -> Option<PathBuf> {
    None
}

fn which_any(name: &str) -> Option<PathBuf> {
    Some(PathBuf::from("/usr/bin").join(name))
}

#[test]
fn auto_on_macos_resolve_backend_gives_screencapture() {
    assert_eq!(resolve_system_audio_backend(Some("auto"), "darwin", &which_none).unwrap(), "screencapture");
}

#[test]
fn auto_on_linux_with_parec_resolve_backend_gives_pulse() {
    assert_eq!(resolve_system_audio_backend(Some("auto"), "linux", &which_any).unwrap(), "pulse");
}

#[test]
fn auto_on_linux_without_parec_resolve_backend_explains_how_to_install_it() {
    let e = resolve_system_audio_backend(Some("auto"), "linux", &which_none).unwrap_err();
    assert!(matches!(&e, BackendError::Runtime(m) if m.contains("pulseaudio-utils")));
}

#[test]
fn auto_on_an_unknown_platform_resolve_backend_names_it() {
    let e = resolve_system_audio_backend(Some("auto"), "win32", &which_any).unwrap_err();
    assert_eq!(e, BackendError::Runtime("no system-audio backend for platform 'win32'".into()));
}

#[test]
fn screencapture_outside_macos_resolve_backend_says_it_needs_macos() {
    let e = resolve_system_audio_backend(Some("screencapture"), "linux", &which_any).unwrap_err();
    assert!(matches!(&e, BackendError::Runtime(m) if m.contains("macOS")));
}

#[test]
fn pulse_outside_linux_resolve_backend_says_it_needs_linux() {
    let e = resolve_system_audio_backend(Some("pulse"), "darwin", &which_any).unwrap_err();
    assert!(matches!(&e, BackendError::Runtime(m) if m.contains("Linux")));
}

#[test]
fn pulse_without_parec_resolve_backend_names_parec() {
    let e = resolve_system_audio_backend(Some("pulse"), "linux", &which_none).unwrap_err();
    assert!(matches!(&e, BackendError::Runtime(m) if m.contains("parec")));
}

#[test]
fn none_empty_and_device_specs_resolve_backend_passes_them_through() {
    for (spec, expected) in [
        (None, "none"),
        (Some(""), "none"),
        (Some("none"), "none"),
        (Some("device:3"), "device:3"),
        (Some("device:auto"), "device:auto"),
    ] {
        assert_eq!(resolve_system_audio_backend(spec, "linux", &which_none).unwrap(), expected);
    }
}

#[test]
fn unknown_name_resolve_backend_is_a_value_error() {
    let e = resolve_system_audio_backend(Some("bogus"), "darwin", &which_any).unwrap_err();
    assert_eq!(e, BackendError::Value("unknown system audio backend: 'bogus'".into()));
}

struct NoStage;

impl EchoSink for NoStage {
    fn add_mic(&self, samples: &[f32], _time_s: f64) -> Result<Vec<f32>, String> {
        Ok(samples.to_vec())
    }
    fn flush_raw(&self) -> Vec<f32> {
        Vec::new()
    }
    fn active(&self) -> bool {
        false
    }
    fn add_reference(&self, _samples: &[f32], _time_s: f64, _echo_possible: bool) -> Result<(), String> {
        Ok(())
    }
}

fn settings(root: &std::path::Path) -> SystemCaptureSettings<'_> {
    SystemCaptureSettings {
        repo_root: root,
        pulse_monitor_source: "",
        max_buffer_s: 600.0,
        log: Logger::capture(SystemClock::shared()).0,
    }
}

#[test]
fn screencapture_create_runs_the_swift_helper_and_frames_only_with_a_stage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("sck_audio_capture"), b"").unwrap();
    let helper = root.path().join("sck_audio_capture").display().to_string();

    let plain = create_system_capture("screencapture", SR, None, &settings(root.path())).unwrap();
    assert_eq!(plain.capture().argv(), vec![helper.clone()]);
    assert_eq!(plain.capture().name(), "screencapture");

    let framed = create_system_capture("screencapture", SR, Some(Arc::new(NoStage)), &settings(root.path())).unwrap();
    assert_eq!(framed.capture().argv(), vec![helper, "--frames".to_owned()]);
}

#[test]
fn pulse_create_gives_parec_on_the_default_monitor() {
    let root = tempfile::tempdir().unwrap();
    let cap = create_system_capture("pulse", SR, None, &settings(root.path())).unwrap();
    assert_eq!(cap.capture().argv(), parec_argv(PULSE_DEFAULT_MONITOR, SR));
    assert_eq!(cap.capture().name(), "pulse");
}

#[test]
fn none_create_is_an_error_naming_it() {
    let root = tempfile::tempdir().unwrap();
    let e = create_system_capture("none", SR, None, &settings(root.path())).unwrap_err();
    assert_eq!(e, "unknown system audio backend: 'none'");
}
