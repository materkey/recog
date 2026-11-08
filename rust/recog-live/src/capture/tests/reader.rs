//! The reader loop and the restart policy of tests/test_system_audio.py.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use super::fakes::{FakeChild, FakeSpawner, SR, Spawn, capture_with, f32_bytes, stop_at_slow_retry};
use crate::capture::subprocess::{PcmDecoder, RestartPolicy, SourceSwitch, SubprocessCapture};
use crate::clock::{Clock as _, ManualClock};

fn fast() -> Vec<f64> {
    let policy = RestartPolicy::default();
    (0..policy.max_restarts as usize).map(|i| policy.backoff_s[i.min(policy.backoff_s.len() - 1)]).collect()
}

const LOST: f64 = 60.0;

fn ramp(n: usize) -> Vec<f32> {
    #[allow(clippy::cast_precision_loss)]
    (0..n).map(|i| i as f32 / SR as f32).collect()
}

fn buffered(capture: &SubprocessCapture) -> Vec<f32> {
    capture.buffer().set_running(true);
    let n = usize::try_from(capture.buffer().buffered_samples()).unwrap();
    #[allow(clippy::cast_precision_loss)]
    let (chunk, _) = capture.get_chunk(n as f64 / f64::from(SR)).unwrap();
    chunk
}

#[test]
fn child_writing_float32_reader_loop_puts_the_same_samples_in_the_buffer() {
    let clock = ManualClock::new(1000.0);
    let (capture, _) = capture_with("test-capture", FakeSpawner::failing(), &clock);
    let samples = ramp(SR as usize / 5);
    capture.set_process(Some(FakeChild::alive(f32_bytes(&samples)).stop_when_drained(&capture).arc()));
    capture.set_running(true);

    capture.run_reader_loop();

    assert_eq!(buffered(&capture), samples);
    assert!(!capture.dead());
}

#[test]
fn reads_not_a_multiple_of_four_bytes_reader_loop_carries_the_partial_sample() {
    let clock = ManualClock::new(1000.0);
    let (capture, _) = capture_with("test-capture", FakeSpawner::failing(), &clock);
    let samples = ramp(SR as usize / 5);
    let payload = f32_bytes(&samples);
    assert_ne!(payload.len() % 7, 0);
    capture.set_process(Some(FakeChild::alive(payload).with_chunk(7).stop_when_drained(&capture).arc()));
    capture.set_running(true);

    capture.run_reader_loop();

    assert_eq!(buffered(&capture), samples);
}

#[test]
fn partial_sample_left_when_the_child_is_respawned_it_is_dropped() {
    let clock = ManualClock::new(1000.0);
    let spawner = FakeSpawner::new(Vec::new(), || Spawn::Child(FakeChild::silent().arc()));
    let (capture, _) = capture_with("test-capture", spawner, &clock);
    assert!(capture.decode_read(&[0, 1, 2]).is_empty());

    capture.restart_process().unwrap();

    // With the three old bytes kept, one more byte would make a whole sample.
    assert!(capture.decode_read(&[3]).is_empty());
}

#[test]
fn one_and_a_half_samples_decode_pcm_gives_one_and_keeps_half() {
    let mut decoder = PcmDecoder::new();
    assert_eq!(decoder.decode_pcm(&[0; 6]).len(), 1);
    assert_eq!(decoder.tail().len(), 2);
    assert_eq!(decoder.decode_pcm(&[0; 2]).len(), 1);
    assert!(decoder.tail().is_empty());
}

fn no_reader(name: &str, spawner: Arc<FakeSpawner>, clock: &ManualClock) -> (SubprocessCapture, crate::log::Captured) {
    let (log, captured) = crate::log::Logger::capture(clock.shared());
    let capture = SubprocessCapture::builder(vec!["fake".into()], name)
        .sample_rate(SR)
        .max_buffer_s(10.0)
        .spawner(spawner)
        .clock(clock.shared())
        .logger(log)
        .reader_thread(false)
        .build();
    (capture, captured)
}

#[test]
fn child_dead_on_the_first_spawn_start_leaves_it_to_the_reader_loop() {
    let clock = ManualClock::new(1000.0);
    let spawner = FakeSpawner::new(
        vec![Spawn::Child(FakeChild::dead("parec: Connection failure: Connection refused\n").arc())],
        || Spawn::Fail("unused"),
    );
    let (capture, log) = no_reader("pulse", spawner, &clock);

    capture.start().unwrap();

    assert!(capture.running());
    assert!(!capture.dead());
    assert!(capture.stderr_suffix().contains("Connection refused"));
    assert!(log.text().contains("[pulse] Failed to start, retrying in background (stderr: parec: Connection failure"));
}

#[test]
fn lost_capture_started_again_stays_lost_until_data_arrives() {
    let clock = ManualClock::new(1000.0);
    let spawner = FakeSpawner::new(vec![Spawn::Child(FakeChild::dead("").arc())], || Spawn::Fail("unused"));
    let (capture, _) = no_reader("pulse", spawner, &clock);
    capture.set_dead(true);

    capture.start().unwrap();

    assert!(capture.running());
    assert!(capture.dead());
}

#[test]
fn child_dying_after_every_respawn_reader_loop_backs_off_then_retries_slowly() {
    let clock = ManualClock::new(1000.0);
    let spawner = FakeSpawner::failing();
    let (capture, log) = capture_with("pulse", spawner.clone(), &clock);
    capture.set_running(true);
    let (delays, running_at_slow) = stop_at_slow_retry(&capture, LOST);

    capture.run_reader_loop();

    assert_eq!(spawner.count(), 5);
    let mut expected = fast();
    expected.push(LOST);
    assert_eq!(*delays.lock().unwrap(), expected);
    assert!(capture.dead());
    assert_eq!(*running_at_slow.lock().unwrap(), vec![true]);
    let lines = log.lines();
    assert_eq!(lines[0], "[pulse] Process died, restarting... (attempt 1)");
    assert_eq!(lines[1], "[pulse] Restart failed: parec: connection refused");
    assert!(lines.contains(&"[pulse] Max restarts (5) reached, audio lost — retrying every 60s".to_owned()));
}

#[test]
fn lost_capture_one_slow_attempt_brings_audio_it_recovers_with_a_full_budget() {
    let clock = ManualClock::new(1000.0);
    let samples = vec![1.0f32; SR as usize / 10];
    let mut script: Vec<Spawn> = (0..6).map(|_| Spawn::Fail("no display")).collect();
    script.push(Spawn::Child(FakeChild::one_block_then_dead(f32_bytes(&samples)).arc()));
    let spawner = FakeSpawner::new(script, || Spawn::Fail("no display"));
    let (capture, log) = capture_with("test-capture", spawner, &clock);
    capture.set_running(true);
    let delays = Arc::new(Mutex::new(Vec::new()));
    let dead_after = Arc::new(Mutex::new(None));
    let (d, a) = (delays.clone(), dead_after.clone());
    capture.set_backoff_hook(Some(Arc::new(move |delay, cap: &SubprocessCapture| {
        let mut d = d.lock().unwrap();
        d.push(delay);
        if d.len() == 5 + 3 {
            *a.lock().unwrap() = Some(cap.dead());
        }
        if d.len() > 5 + 2 + 5 {
            cap.set_running(false);
        }
    })));

    capture.run_reader_loop();

    assert_eq!(*dead_after.lock().unwrap(), Some(false));
    assert!(log.lines().contains(&"[test-capture] Audio recovered".to_owned()));
    assert_eq!(capture.buffer().buffered_samples(), i64::try_from(samples.len()).unwrap());
    let delays = delays.lock().unwrap();
    let mut head = fast();
    head.extend([LOST, LOST]);
    assert_eq!(delays[..7], head[..]);
    assert_eq!(delays[7..12], fast()[..]);
}

#[test]
fn healthy_stretch_between_deaths_successful_read_resets_the_budget() {
    let clock = ManualClock::new(1000.0);
    let samples = vec![1.0f32; SR as usize / 10];
    let spawner = FakeSpawner::new(
        vec![Spawn::Fail("dead again"), Spawn::Child(FakeChild::one_block_then_dead(f32_bytes(&samples)).arc())],
        || Spawn::Fail("dead again"),
    );
    let (capture, _) = capture_with("test-capture", spawner, &clock);
    capture.set_running(true);
    let (delays, _) = stop_at_slow_retry(&capture, LOST);

    capture.run_reader_loop();

    let mut expected = vec![1.0, 2.0];
    expected.extend(fast());
    expected.push(LOST);
    assert_eq!(*delays.lock().unwrap(), expected);
    assert_eq!(capture.buffer().buffered_samples(), i64::try_from(samples.len()).unwrap());
    assert!(capture.dead());
}

#[test]
fn child_that_starts_then_dies_every_respawn_waits_its_backoff() {
    let clock = ManualClock::new(1000.0);
    let spawner =
        FakeSpawner::new(Vec::new(), || Spawn::Child(FakeChild::alive(Vec::new()).with_polls(&[None, Some(0)]).arc()));
    let (capture, _) = capture_with("pulse", spawner, &clock);
    capture.set_running(true);
    let (delays, running_at_slow) = stop_at_slow_retry(&capture, LOST);

    capture.run_reader_loop();

    let mut expected = fast();
    expected.push(LOST);
    assert_eq!(*delays.lock().unwrap(), expected);
    assert!(capture.dead());
    assert_eq!(*running_at_slow.lock().unwrap(), vec![true]);
}

#[test]
fn zeros_then_sound_only_the_nonzero_block_moves_the_signal_mark() {
    let clock = ManualClock::new(1000.0);
    clock.set(500.0);
    let (capture, _) = capture_with("test-capture", FakeSpawner::failing(), &clock);
    capture.set_last_signal_at(100.0);
    capture.set_running(true);
    capture.set_process(Some(FakeChild::alive(f32_bytes(&vec![0.0; 1600])).stop_when_drained(&capture).arc()));
    capture.run_reader_loop();
    assert!(capture.digital_silence_s() >= 400.0);

    capture.set_running(true);
    capture.set_process(Some(FakeChild::alive(f32_bytes(&vec![0.01; 1600])).stop_when_drained(&capture).arc()));
    capture.run_reader_loop();
    assert!(capture.digital_silence_s() < 1.0);
}

#[test]
fn spent_budget_requested_restart_respawns_once_and_refills_it() {
    let clock = ManualClock::new(1000.0);
    let survivor = FakeChild::alive(Vec::new()).with_polls(&[None, Some(0)]).arc();
    let spawner =
        FakeSpawner::new(vec![Spawn::Fail("still dead"), Spawn::Fail("still dead"), Spawn::Child(survivor)], || {
            Spawn::Fail("still dead")
        });
    let (capture, log) = capture_with("test-capture", spawner.clone(), &clock);
    capture.set_running(true);
    let delays = Arc::new(Mutex::new(Vec::new()));
    let d = delays.clone();
    capture.set_backoff_hook(Some(Arc::new(move |delay, cap: &SubprocessCapture| {
        let mut d = d.lock().unwrap();
        d.push(delay);
        if d.len() == 2 {
            cap.request_restart("silence");
        }
        if (delay - LOST).abs() < f64::EPSILON {
            cap.set_running(false);
        }
    })));

    capture.run_reader_loop();

    assert_eq!(delays.lock().unwrap().len(), 2 + 5 + 1);
    assert_eq!(spawner.count(), 2 + 1 + 5);
    assert!(capture.dead());
    assert!(log.lines().contains(&"[test-capture] silence, restarting the capture process".to_owned()));
}

#[test]
fn long_digital_silence_respawn_restarts_the_silence_clock() {
    let clock = ManualClock::new(1000.0);
    clock.set(500.0);
    let spawner = FakeSpawner::new(Vec::new(), || Spawn::Child(FakeChild::silent().arc()));
    let (capture, log) = capture_with("test-capture", spawner, &clock);
    capture.set_last_signal_at(500.0 - 120.0);

    capture.restart_process().unwrap();

    assert!(capture.digital_silence_s() < 5.0);
    assert_eq!(log.lines(), vec!["[test-capture] Restarted (PID: 4242)"]);
}

#[test]
fn reader_spawning_while_stop_runs_stop_reaps_the_late_child_too() {
    let clock = ManualClock::new(1000.0);
    let (capture, log) = capture_with("test-capture", FakeSpawner::failing(), &clock);
    let old = Arc::new(FakeChild::silent().with_pid(1));
    let late = Arc::new(FakeChild::silent().with_pid(2));
    capture.set_running(true);
    capture.set_process(Some(old.clone()));
    let (cap, l) = (capture.clone(), late.clone());
    capture.set_reader(Some(std::thread::spawn(move || {
        while cap.running() {
            std::thread::yield_now();
        }
        cap.set_process(Some(l));
    })));

    capture.stop();

    assert!(old.reaped.load(Ordering::SeqCst));
    assert!(late.reaped.load(Ordering::SeqCst));
    assert!(capture.process().is_none());
    assert_eq!(log.lines(), vec!["[test-capture] Stopped"]);
}

#[test]
fn stop_during_the_restart_backoff_no_child_is_spawned() {
    let clock = ManualClock::new(1000.0);
    let spawner = FakeSpawner::failing();
    let (capture, _) = capture_with("test-capture", spawner.clone(), &clock);
    capture.set_running(true);
    capture.set_backoff_hook(Some(Arc::new(|_, cap: &SubprocessCapture| cap.set_running(false))));

    capture.run_reader_loop();

    assert_eq!(spawner.count(), 0);
    assert!(!capture.dead());
}

struct ScriptedSwitch {
    pending: Mutex<Vec<Option<String>>>,
    switched: Mutex<Vec<String>>,
    stop_on_apply: bool,
}

impl SourceSwitch for ScriptedSwitch {
    fn take_pending(&self) -> Option<String> {
        let mut p = self.pending.lock().unwrap();
        if p.is_empty() { None } else { p.remove(0) }
    }

    fn apply(&self, capture: &SubprocessCapture, source: &str) -> Result<(), String> {
        self.switched.lock().unwrap().push(source.to_owned());
        if self.stop_on_apply {
            capture.set_running(false);
        }
        Ok(())
    }
}

#[test]
fn silent_source_and_a_switch_queued_during_the_read_the_switch_is_applied() {
    let clock = ManualClock::new(1000.0);
    let (capture, _) = capture_with("pulse", FakeSpawner::failing(), &clock);
    capture.set_running(true);
    let silent = Arc::new(FakeChild::silent());
    capture.set_process(Some(silent.clone()));
    let switch = Arc::new(ScriptedSwitch {
        pending: Mutex::new(vec![None, Some("sinkB.monitor".into())]),
        switched: Mutex::new(Vec::new()),
        stop_on_apply: true,
    });
    capture.set_source_switch(Some(switch.clone()));

    capture.run_reader_loop();

    assert_eq!(*switch.switched.lock().unwrap(), vec!["sinkB.monitor"]);
    assert!(silent.reads.load(Ordering::SeqCst) >= 1);
}

#[test]
fn spent_budget_planned_switch_refills_it() {
    let clock = ManualClock::new(1000.0);
    let (capture, _) = capture_with("pulse", FakeSpawner::failing(), &clock);
    capture.set_running(true);
    let (delays, _) = stop_at_slow_retry(&capture, LOST);
    let switch = Arc::new(ScriptedSwitch {
        pending: Mutex::new(vec![Some("sinkB.monitor".into())]),
        switched: Mutex::new(Vec::new()),
        stop_on_apply: false,
    });
    capture.set_source_switch(Some(switch.clone()));

    capture.run_reader_loop();

    assert_eq!(*switch.switched.lock().unwrap(), vec!["sinkB.monitor"]);
    assert_eq!(delays.lock().unwrap().len(), 5 + 1);
    assert!(capture.dead());
}

#[test]
fn backoff_pause_sleep_while_running_sleeps_in_steps_of_at_most_200_ms() {
    let clock = ManualClock::new(1000.0);
    let (capture, _) = capture_with("test-capture", FakeSpawner::failing(), &clock);
    capture.set_running(true);
    capture.sleep_while_running(0.5);
    let sleeps = clock.sleeps();
    assert_eq!(sleeps.len(), 3);
    assert!((sleeps[2] - 0.1).abs() < 1e-9);
    assert!((clock.monotonic() - 0.5).abs() < 1e-9);
}

#[test]
fn child_with_a_long_stderr_suffix_quotes_its_last_five_nonblank_lines() {
    let clock = ManualClock::new(1000.0);
    let (capture, _) = capture_with("test-capture", FakeSpawner::failing(), &clock);
    let tail = capture.stderr_tail();
    for i in 0..7 {
        tail.push(format!("line {i}\n"));
    }
    tail.push("   \n".into());
    assert_eq!(capture.stderr_suffix(), " (stderr: line 3 | line 4 | line 5 | line 6)");
}
