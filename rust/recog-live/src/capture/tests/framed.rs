//! The `ChildMicCapture`, `FramedSystemCapture` and worker-argv cases of
//! tests/test_mic_child.py.
//!
//! Not ported here: the worker-side tests (`device_is_found_by_name…`, `worker_*`):
//! they test `recog/mic_worker.py`, which rust/recog-mic replaces with its own tests;
//! `macos_microphone_runs_in_a_child…`, `macos_default_input_has_no_name…` and
//! `linux_microphone_stays_in_process` test `create_mic_capture` and the
//! `create_echo_stage` cases test the stage's construction, both in `live.py`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::fakes::SR;
use crate::capture::framed::{
    FrameHeader, ONLY_ZEROS_REPORT_S, child_mic_capture, encode_mic_frame, encode_reference_frame,
    framed_system_capture, mic_worker_argv, rust_mic_worker, split_frames,
};
use crate::capture::subprocess::SubprocessCapture;
use crate::clock::ManualClock;
use crate::echo::EchoSink;
use crate::log::{Captured, Logger};

const MIC: &str = "Микрофон MacBook Pro";

fn frame(samples: &[f32], adc: f64, wall: f64) -> Vec<u8> {
    encode_mic_frame(adc, wall, samples)
}

fn mic(stage: Option<Arc<dyn EchoSink>>) -> (SubprocessCapture, Captured, ManualClock) {
    let clock = ManualClock::new(1000.0);
    let (log, captured) = Logger::capture(clock.shared());
    let capture =
        child_mic_capture(vec!["unused".into()], stage).sample_rate(SR).clock(clock.shared()).logger(log).build();
    (capture, captured, clock)
}

#[test]
fn two_frames_in_one_read_decode_joins_them_stamped_with_the_first() {
    let (cap, _, _) = mic(None);
    let mut data = frame(&[0.1, 0.2], 100.0, 1000.0);
    data.extend(frame(&[0.3], 100.0, 1000.5));
    let batches = cap.decode_read(&data);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0], (vec![0.1, 0.2, 0.3], 1000.0));
}

#[test]
fn read_ending_inside_a_frame_the_next_read_brings_it_whole() {
    for cut in [1, 19, 20, 25] {
        let (cap, _, _) = mic(None);
        let data = frame(&[0.1, 0.2, 0.3], 100.0, 1000.0);
        assert!(cap.decode_read(&data[..cut]).is_empty());
        assert_eq!(cap.decode_read(&data[cut..])[0].0, vec![0.1, 0.2, 0.3]);
    }
}

#[test]
fn adc_times_leaving_50_ms_decode_logs_the_hole_and_contiguous_frames_stay_quiet() {
    let (cap, log, _) = mic(None);
    let block = vec![0.0; 160];
    let mut data = frame(&block, 10.0, 1000.0);
    data.extend(frame(&block, 10.01, 1000.0));
    cap.decode_read(&data);
    assert!(!log.text().contains("hole"));
    cap.decode_read(&frame(&block, 10.07, 1000.0));
    assert_eq!(log.lines(), vec!["[mic] 50 ms hole in the captured audio"]);
}

#[test]
fn zeros_past_the_limit_twice_decode_reports_each_stretch_once() {
    let (cap, log, clock) = mic(None);
    clock.set(ONLY_ZEROS_REPORT_S + 1.0);
    cap.set_last_signal_at(0.0);
    cap.decode_read(&frame(&[0.0], 100.0, 1000.0));
    cap.decode_read(&frame(&[0.0], 100.0, 1000.0));
    let text = log.text();
    assert_eq!(text.matches("only exact zeros").count(), 1);
    assert!(text.contains("lid") && text.contains("permission"));

    cap.set_last_signal_at(ONLY_ZEROS_REPORT_S + 1.0);
    cap.decode_read(&frame(&[0.1], 100.0, 1000.0));
    cap.set_last_signal_at(0.0);
    cap.decode_read(&frame(&[0.0], 100.0, 1000.0));
    assert_eq!(log.text().matches("only exact zeros").count(), 2);
}

#[test]
fn child_dead_mid_frame_the_respawned_child_s_frame_decodes_on_its_own() {
    let (cap, _, _) = mic(None);
    let data = frame(&[0.1, 0.2], 100.0, 1000.0);
    assert!(cap.decode_read(&data[..data.len() - 3]).is_empty());
    cap.on_new_child();
    assert_eq!(cap.decode_read(&frame(&[0.5], 500.0, 1000.0))[0].0, vec![0.5]);
}

#[test]
fn respawned_child_seconds_later_no_hole_is_claimed() {
    let (cap, log, _) = mic(None);
    cap.decode_read(&frame(&[0.0], 1.0, 1000.0));
    cap.on_new_child();
    cap.decode_read(&frame(&[0.0], 9.0, 1000.0));
    assert!(!log.text().contains("hole"));
}

#[test]
fn split_frames_of_the_reference_header_reads_time_flag_and_samples() {
    let mut data = encode_reference_frame(50.0, 2000.0, true, &[0.1, 0.2]);
    data.extend_from_slice(&[1, 2, 3]);
    let (frames, rest) = split_frames(&data, FrameHeader::Reference);
    assert_eq!(frames.len(), 1);
    assert_eq!((frames[0].time, frames[0].wall, frames[0].count, frames[0].echo_possible), (50.0, 2000.0, 2, 1));
    assert_eq!(frames[0].samples, vec![0.1, 0.2]);
    assert_eq!(rest, vec![1, 2, 3]);
    assert_eq!(FrameHeader::Mic.size(), 20);
}

/// Holds nothing back and doubles the microphone, to tell it from raw.
#[derive(Default)]
struct FakeStage {
    references: Mutex<Vec<(Vec<f32>, f64, bool)>>,
    mic_times: Mutex<Vec<f64>>,
    fail_on: Option<&'static str>,
    active: AtomicBool,
}

impl EchoSink for FakeStage {
    fn add_mic(&self, samples: &[f32], time_s: f64) -> Result<Vec<f32>, String> {
        if self.fail_on == Some("mic") {
            return Err("stage failed".into());
        }
        self.mic_times.lock().unwrap().push(time_s);
        Ok(samples.iter().map(|s| s * 2.0).collect())
    }

    fn flush_raw(&self) -> Vec<f32> {
        vec![0.9]
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    fn add_reference(&self, samples: &[f32], time_s: f64, echo_possible: bool) -> Result<(), String> {
        if self.fail_on == Some("reference") {
            return Err("stage failed".into());
        }
        self.references.lock().unwrap().push((samples.to_vec(), time_s, echo_possible));
        Ok(())
    }
}

#[test]
fn echo_stage_decode_hands_it_each_frame_by_adc_time_and_keeps_its_output() {
    let stage = Arc::new(FakeStage::default());
    let (cap, _, _) = mic(Some(stage.clone()));
    let mut data = frame(&[0.1], 7.0, 1000.0);
    data.extend(frame(&[0.2], 7.5, 1000.0));
    let batches = cap.decode_read(&data);
    assert_eq!(*stage.mic_times.lock().unwrap(), vec![7.0, 7.5]);
    assert_eq!(batches[0].0, vec![0.2, 0.4]);
}

#[test]
fn stage_turning_on_and_off_each_change_is_logged_once() {
    let stage = Arc::new(FakeStage::default());
    let (cap, log, _) = mic(Some(stage.clone()));
    cap.decode_read(&frame(&[0.1], 1.0, 1000.0));
    assert!(!log.text().contains("cancelling"));
    stage.active.store(true, Ordering::SeqCst);
    let mut two = frame(&[0.1], 1.0, 1000.0);
    two.extend(frame(&[0.1], 1.0, 1000.0));
    cap.decode_read(&two);
    assert_eq!(log.text().matches("Echo cancelling").count(), 1);
    stage.active.store(false, Ordering::SeqCst);
    cap.decode_read(&frame(&[0.1], 1.0, 1000.0));
    assert_eq!(log.text().matches("raw").count(), 1);
}

#[test]
fn failing_stage_the_microphone_goes_on_raw_after_what_it_held() {
    let stage = Arc::new(FakeStage { fail_on: Some("mic"), ..FakeStage::default() });
    let (cap, log, _) = mic(Some(stage));
    let first = cap.decode_read(&frame(&[0.1], 1.0, 1000.0));
    let second = cap.decode_read(&frame(&[0.2], 1.01, 1000.0));
    assert_eq!(first[0].0, vec![0.9, 0.1]);
    assert_eq!(second[0].0, vec![0.2]);
    assert!(log.text().contains("[mic] Echo cancellation failed, the microphone stays raw: stage failed"));
}

fn reference(stage: Arc<FakeStage>) -> (SubprocessCapture, Captured) {
    let clock = ManualClock::new(1000.0);
    let (log, captured) = Logger::capture(clock.shared());
    (framed_system_capture(vec!["unused".into()], stage).sample_rate(SR).logger(log).build(), captured)
}

#[test]
fn framed_system_audio_cut_in_a_read_reaches_stage_and_buffer_whole() {
    let stage = Arc::new(FakeStage::default());
    let (cap, _) = reference(stage.clone());
    let data = encode_reference_frame(50.0, 2000.0, false, &[0.1, 0.2]);
    assert!(cap.decode_read(&data[..7]).is_empty());
    let batches = cap.decode_read(&data[7..]);
    assert_eq!(*stage.references.lock().unwrap(), vec![(vec![0.1, 0.2], 50.0, false)]);
    assert_eq!(batches[0], (vec![0.1, 0.2], 2000.0));
    assert_eq!(cap.name(), "screencapture");
}

#[test]
fn stage_failing_on_the_reference_the_system_audio_is_untouched_and_it_is_left() {
    let stage = Arc::new(FakeStage { fail_on: Some("reference"), ..FakeStage::default() });
    let (cap, log) = reference(stage);
    assert_eq!(cap.decode_read(&encode_reference_frame(50.0, 2000.0, true, &[0.1]))[0].0, vec![0.1]);
    assert_eq!(cap.decode_read(&encode_reference_frame(50.0, 2000.0, true, &[0.2]))[0].0, vec![0.2]);
    assert_eq!(log.text().matches("stage failed").count(), 1);
    assert!(log.text().contains("[screencapture] Echo cancellation lost its reference: stage failed"));
}

fn built_worker(root: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let binary = rust_mic_worker(root);
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    binary
}

#[test]
fn default_setting_and_a_built_rust_worker_argv_runs_the_python_worker() {
    let root = tempfile::tempdir().unwrap();
    built_worker(root.path());
    let python = std::path::Path::new("/venv/bin/python");
    let argv =
        mic_worker_argv("python", root.path(), python, None, SR, &Logger::capture(ManualClock::new(0.0).shared()).0)
            .unwrap();
    assert_eq!(argv[0], "/venv/bin/python");
    assert_eq!(argv[1], "-I");
    assert!(argv[2].ends_with("mic_worker.py"));
    assert!(!argv.contains(&"--device".to_owned()));
}

#[test]
fn installed_and_built_rust_worker_when_resolving_then_the_installed_one_runs() {
    use std::os::unix::fs::PermissionsExt as _;
    // given
    let root = tempfile::tempdir().unwrap();
    built_worker(root.path());
    let installed = root.path().join("bin/recog-mic");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
    // when
    let argv = mic_worker_argv(
        "rust",
        root.path(),
        std::path::Path::new("py"),
        None,
        SR,
        &Logger::capture(ManualClock::new(0.0).shared()).0,
    )
    .unwrap();
    // then
    assert_eq!(argv[0], installed.display().to_string());
}

#[test]
fn rust_setting_with_the_binary_built_argv_runs_it_with_the_same_arguments() {
    let root = tempfile::tempdir().unwrap();
    let binary = built_worker(root.path());
    let argv = mic_worker_argv(
        "rust",
        root.path(),
        std::path::Path::new("py"),
        Some(MIC),
        SR,
        &Logger::capture(ManualClock::new(0.0).shared()).0,
    )
    .unwrap();
    assert_eq!(
        argv,
        vec![binary.display().to_string(), "--sample-rate".into(), "16000".into(), "--device".into(), MIC.into()]
    );
}

#[test]
fn rust_setting_without_a_build_argv_falls_back_to_python_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let (log, captured) = Logger::capture(ManualClock::new(0.0).shared());
    let argv = mic_worker_argv("rust", root.path(), std::path::Path::new("py"), None, SR, &log).unwrap();
    assert!(argv[2].ends_with("mic_worker.py"));
    assert!(captured.text().contains("not built"));
}

#[test]
fn typo_in_the_setting_argv_fails_naming_it() {
    let root = tempfile::tempdir().unwrap();
    let e = mic_worker_argv(
        "rsut",
        root.path(),
        std::path::Path::new("py"),
        None,
        SR,
        &Logger::capture(ManualClock::new(0.0).shared()).0,
    )
    .unwrap_err();
    assert_eq!(e, r#"[live] mic_worker must be "python" or "rust", not 'rsut'"#);
}
