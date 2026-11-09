//! The remote backend's threaded paths: the lone recovery probe among concurrent
//! callers, a late success not closing a circuit reopened after it started, and the
//! local refine path's abort check after a remote failure.

use std::net::TcpListener;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use recog_live::asr::remote::CircuitBreaker;
use recog_live::asr::wavo::{EngineAnswer, Loader};
use recog_live::asr::{AsrError, Engine, FallbackAsr, RemoteAsrClient, WavoAsr};
use recog_live::clock::ManualClock;
use recog_live::log::Logger;

fn breaker(clock: &ManualClock) -> CircuitBreaker {
    let (logger, _) = Logger::capture(clock.shared());
    CircuitBreaker::new(1, 10.0, "live", clock.shared(), logger)
}

#[test]
fn open_window_over_eight_threads_deciding_at_once_then_exactly_one_probes_the_remote() {
    // given
    let clock = ManualClock::new(0.0);
    clock.advance(100.0);
    let breaker = Arc::new(breaker(&clock));
    let (_, epoch) = breaker.decide();
    breaker.record_failure(epoch);
    assert!(!breaker.using_remote());
    clock.advance(11.0);
    let barrier = Arc::new(Barrier::new(8));
    // when
    let probes: usize = (0..8)
        .map(|_| {
            let (breaker, barrier) = (breaker.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                breaker.decide().0
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum();
    // then
    assert_eq!(probes, 1);
}

#[test]
fn success_started_before_the_circuit_reopened_recording_it_then_the_circuit_stays_open() {
    // given
    let clock = ManualClock::new(0.0);
    clock.advance(100.0);
    let breaker = breaker(&clock);
    let (_, slow_call) = breaker.decide();
    let (_, failing_call) = breaker.decide();
    breaker.record_failure(failing_call);
    // when
    breaker.record_success(slow_call);
    // then
    assert!(!breaker.using_remote());
}

struct Engine1;

impl Engine for Engine1 {
    fn transcribe(&self, _pcm: &[f32]) -> Result<EngineAnswer, AsrError> {
        Ok(("local".into(), Vec::new()))
    }

    fn device(&self) -> String {
        "fake".into()
    }
}

#[test]
fn remote_down_and_local_loaded_refining_with_live_waiting_then_the_local_path_gives_way() {
    // given: a gateway address nobody listens on, and a local model that loads only
    // once the test lets it, so the first call surely finds it still loading
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let clock = ManualClock::new(0.0);
    clock.advance(100.0);
    let (logger, _) = Logger::capture(clock.shared());
    let loading = Arc::new(Barrier::new(2));
    let held = Arc::clone(&loading);
    let loader: Loader = Box::new(move || {
        held.wait();
        Ok(Arc::new(Engine1) as Arc<dyn Engine>)
    });
    let fb = FallbackAsr::new(
        RemoteAsrClient::new(&format!("http://127.0.0.1:{port}"), "gigaam", 2.0, Some(1.0)),
        Arc::new(WavoAsr::new(loader, logger.clone())),
        3,
        60.0,
        clock.shared(),
        logger,
    );
    let pcm = [100i16; 1600];
    assert_eq!(fb.transcribe_with_timestamps(&pcm, None), Err(AsrError::TemporarilyUnavailable));
    loading.wait();
    assert!(fb.wait_local_ready(Duration::from_secs(10)));
    // when
    let aborted = fb.transcribe_with_timestamps(&pcm, Some(&|| true));
    let refined = fb.transcribe_with_timestamps(&pcm, Some(&|| false));
    // then
    assert_eq!(aborted, Err(AsrError::Aborted));
    assert_eq!(refined.map(|r| r.text).as_deref(), Ok("local"));
}
