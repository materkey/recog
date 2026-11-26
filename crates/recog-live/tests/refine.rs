//! The cases of tests/test_refine.py and the worker-level ones of
//! tests/test_refine_fallback.py against the Rust worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use recog_live::asr::{AsrError, Transcription, Word};
use recog_live::clock::ManualClock;
use recog_live::log::{Captured, Logger};
use recog_live::refine::{
    AbortMode, ApplyFn, RefineOptions, RefineResult, RefineTask, RefineWorker, TranscribeFn, new_model_lock,
};

fn task(pid: &str) -> RefineTask {
    RefineTask {
        pid: pid.into(),
        pcm_samples: vec![0; 16],
        offset_base_ms: 0.0,
        submitted_at: 1.7e9,
        meeting_generation: 0,
        target: None,
    }
}

fn result(text: &str) -> Transcription {
    Transcription { text: text.into(), words: Vec::new() }
}

fn quiet() -> (Logger, Captured) {
    Logger::capture(ManualClock::new(0.0).shared())
}

fn options(abort_mode: AbortMode) -> RefineOptions {
    RefineOptions { abort_mode, logger: quiet().0, ..RefineOptions::default() }
}

fn worker(transcribe: TranscribeFn, apply: ApplyFn, abort_mode: AbortMode) -> RefineWorker {
    RefineWorker::new(new_model_lock(), transcribe, apply, options(abort_mode))
}

fn no_apply() -> ApplyFn {
    Arc::new(|_| Ok(()))
}

/// A flag a test thread waits on, `threading.Event`.
#[derive(Default)]
struct Flag(Mutex<bool>, std::sync::Condvar);

impl Flag {
    fn set(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.0.lock().unwrap();
        *self.1.wait_timeout_while(guard, timeout, |s| !*s).unwrap().0
    }
}

#[test]
fn every_field_given_when_constructing_a_task_then_they_are_kept() {
    let task = RefineTask { offset_base_ms: 1234.5, pcm_samples: vec![0; 16000], ..task("p1") };
    assert_eq!(task.pid, "p1");
    assert_eq!(task.pcm_samples.len(), 16000);
    assert_eq!(task.offset_base_ms, 1234.5);
}

#[test]
fn task_and_words_given_when_constructing_a_result_then_they_are_kept() {
    let task = Arc::new(task("p1"));
    let words =
        vec![Word { text: "hi".into(), start: 0, end: 500 }, Word { text: "world".into(), start: 500, end: 1200 }];
    let result = RefineResult { task: task.clone(), text: "hi world".into(), words };
    assert!(Arc::ptr_eq(&result.task, &task));
    assert_eq!(result.text, "hi world");
    assert_eq!(result.words.len(), 2);
    assert_eq!(result.words[0].text, "hi");
}

#[test]
fn no_mode_given_when_constructing_a_worker_then_it_is_cooperative() {
    let w = RefineWorker::new(new_model_lock(), Arc::new(|_, _| Ok(result(""))), no_apply(), RefineOptions::default());
    assert_eq!(w.abort_mode(), AbortMode::Cooperative);
}

#[test]
fn coarse_mode_given_when_constructing_a_worker_then_it_is_coarse() {
    let w = worker(Arc::new(|_, _| Ok(result(""))), no_apply(), AbortMode::Coarse);
    assert_eq!(w.abort_mode(), AbortMode::Coarse);
}

#[test]
fn unknown_mode_name_when_parsing_then_python_s_value_error_message() {
    assert_eq!(AbortMode::parse("bogus"), Err("abort_mode must be 'cooperative' or 'coarse', got 'bogus'".to_owned()));
    assert_eq!(AbortMode::parse("coarse"), Ok(AbortMode::Coarse));
    assert_eq!(AbortMode::parse("cooperative").map(AbortMode::as_str), Ok("cooperative"));
}

#[test]
fn submitted_task_when_the_worker_runs_then_transcribe_comes_before_apply() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Flag::default());
    let (e1, e2, d) = (events.clone(), events.clone(), done.clone());
    let w = worker(
        Arc::new(move |_, _| {
            e1.lock().unwrap().push("transcribe".to_owned());
            Ok(Transcription { text: "hello".into(), words: vec![Word { text: "hello".into(), start: 0, end: 100 }] })
        }),
        Arc::new(move |r: &RefineResult| {
            e2.lock().unwrap().push(format!("apply {} {}", r.text, r.task.pid));
            d.set();
            Ok(())
        }),
        AbortMode::Cooperative,
    );
    w.start();
    w.submit(task("p1"));
    assert!(done.wait(Duration::from_secs(2)), "apply_fn was not invoked");
    w.stop(Duration::from_secs(1));
    assert_eq!(*events.lock().unwrap(), vec!["transcribe", "apply hello p1"]);
}

fn counting() -> (TranscribeFn, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0));
    let c = calls.clone();
    (
        Arc::new(move |_, _| {
            *c.lock().unwrap() += 1;
            Ok(result("x"))
        }),
        calls,
    )
}

#[test]
fn queued_tasks_when_paused_then_none_runs_and_submits_while_paused_are_dropped() {
    let (transcribe, calls) = counting();
    let applied = Arc::new(Mutex::new(0));
    let a = applied.clone();
    let w = worker(
        transcribe,
        Arc::new(move |_| {
            *a.lock().unwrap() += 1;
            Ok(())
        }),
        AbortMode::Cooperative,
    );
    w.submit(task("p1"));
    w.submit(task("p2"));
    w.pause();
    w.start();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(*calls.lock().unwrap(), 0);
    assert_eq!(*applied.lock().unwrap(), 0);
    w.submit(task("p3"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(*calls.lock().unwrap(), 0);
    w.stop(Duration::from_secs(1));
}

#[test]
fn paused_worker_when_submitting_then_the_task_never_reaches_the_queue() {
    let (transcribe, calls) = counting();
    let w = worker(transcribe, no_apply(), AbortMode::Cooperative);
    w.start();
    w.pause();
    w.submit(task("p1"));
    w.submit(task("p2"));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(*calls.lock().unwrap(), 0);
    assert_eq!(w.queued(), 0);
    w.stop(Duration::from_secs(1));
}

#[test]
fn resumed_worker_when_submitting_then_the_task_runs() {
    let done = Arc::new(Flag::default());
    let d = done.clone();
    let w = worker(
        Arc::new(|_, _| Ok(result("ok"))),
        Arc::new(move |_| {
            d.set();
            Ok(())
        }),
        AbortMode::Cooperative,
    );
    w.start();
    w.pause();
    w.submit(task("p1"));
    assert!(!done.wait(Duration::from_millis(200)));
    w.resume();
    w.submit(task("p2"));
    assert!(done.wait(Duration::from_secs(2)), "apply_fn not invoked after resume()");
    w.stop(Duration::from_secs(1));
}

#[test]
fn pause_and_resume_when_reading_is_paused_then_it_follows_them() {
    let w = worker(Arc::new(|_, _| Ok(result("x"))), no_apply(), AbortMode::Cooperative);
    w.start();
    assert!(!w.is_paused());
    w.pause();
    assert!(w.is_paused());
    w.resume();
    assert!(!w.is_paused());
    w.stop(Duration::from_secs(1));
}

#[test]
fn pause_landing_during_a_transcribe_when_it_finishes_then_its_result_is_dropped() {
    let entered = Arc::new(Flag::default());
    let release = Arc::new(Flag::default());
    let applied = Arc::new(Mutex::new(0));
    let (e, r, a) = (entered.clone(), release.clone(), applied.clone());
    let w = worker(
        Arc::new(move |_, _| {
            e.set();
            r.wait(Duration::from_secs(2));
            Ok(result("ok"))
        }),
        Arc::new(move |_| {
            *a.lock().unwrap() += 1;
            Ok(())
        }),
        AbortMode::Cooperative,
    );
    w.start();
    w.submit(task("p1"));
    assert!(entered.wait(Duration::from_secs(1)), "transcribe_fn should have been called");
    w.pause();
    release.set();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(*applied.lock().unwrap(), 0, "apply_fn must not run when pause() landed mid-transcribe");
    w.stop(Duration::from_secs(1));
}

#[test]
fn live_pending_set_mid_inference_when_cooperative_then_the_task_is_requeued_and_retried_after_live() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Flag::default());
    let worker_slot: Arc<Mutex<Option<Arc<RefineWorker>>>> = Arc::default();
    let (c, slot, d) = (calls.clone(), worker_slot.clone(), done.clone());
    let w = Arc::new(worker(
        Arc::new(move |_, should_abort| {
            let should_abort = should_abort.expect("cooperative mode must pass should_abort");
            let first = c.lock().unwrap().is_empty();
            if first {
                slot.lock().unwrap().as_ref().unwrap().set_live_pending();
                c.lock().unwrap().push(should_abort());
                return Err(AsrError::Aborted);
            }
            c.lock().unwrap().push(should_abort());
            Ok(result("ok"))
        }),
        Arc::new(move |_| {
            d.set();
            Ok(())
        }),
        AbortMode::Cooperative,
    ));
    *worker_slot.lock().unwrap() = Some(w.clone());
    let watcher_worker = w.clone();
    let stop_watcher = Arc::new(AtomicBool::new(false));
    let stop = stop_watcher.clone();
    let watcher = std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            if watcher_worker.live_pending() {
                std::thread::sleep(Duration::from_millis(20));
                watcher_worker.clear_live_pending();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    w.start();
    w.submit(task("p1"));
    assert!(done.wait(Duration::from_secs(2)), "apply_fn was not called after re-enqueue");
    stop_watcher.store(true, Ordering::SeqCst);
    watcher.join().unwrap();
    w.stop(Duration::from_secs(1));
    *worker_slot.lock().unwrap() = None;
    assert_eq!(*calls.lock().unwrap(), vec![true, false]);
}

#[test]
fn live_pending_before_the_task_when_cooperative_then_the_model_is_not_taken_until_it_clears() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Flag::default());
    let (c, d) = (calls.clone(), done.clone());
    let w = worker(
        Arc::new(move |_, should_abort| {
            c.lock().unwrap().push(should_abort.is_some_and(|f| f()));
            Ok(result(""))
        }),
        Arc::new(move |_| {
            d.set();
            Ok(())
        }),
        AbortMode::Cooperative,
    );
    w.set_live_pending();
    w.start();
    w.submit(task("p1"));
    std::thread::sleep(Duration::from_millis(400));
    assert!(calls.lock().unwrap().is_empty(), "transcribe_fn ran while live_pending was set");
    w.clear_live_pending();
    assert!(done.wait(Duration::from_secs(2)), "apply_fn not invoked after the gate released");
    assert_eq!(*calls.lock().unwrap(), vec![false]);
    w.stop(Duration::from_secs(1));
}

#[test]
fn live_signalling_while_the_worker_waits_for_the_lock_when_it_gets_it_then_it_requeues_without_transcribing() {
    let calls = Arc::new(Mutex::new(0));
    let c = calls.clone();
    let lock = new_model_lock();
    let w = RefineWorker::new(
        lock.clone(),
        Arc::new(move |_, _| {
            *c.lock().unwrap() += 1;
            Ok(result(""))
        }),
        no_apply(),
        options(AbortMode::Cooperative),
    );
    let guard = lock.lock();
    w.start();
    w.submit(task("p1"));
    std::thread::sleep(Duration::from_millis(400));
    w.set_live_pending();
    drop(guard);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(*calls.lock().unwrap(), 0, "the post-lock recheck did not fire");
    w.clear_live_pending();
    let deadline = Instant::now() + Duration::from_secs(2);
    while *calls.lock().unwrap() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(*calls.lock().unwrap(), 1, "retry after requeue never ran transcribe_fn");
    w.stop(Duration::from_secs(1));
}

#[test]
fn live_pending_when_coarse_then_the_task_runs_without_should_abort() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Flag::default());
    let (s, d) = (seen.clone(), done.clone());
    let w = worker(
        Arc::new(move |_, should_abort| {
            s.lock().unwrap().push(should_abort.is_some());
            Ok(result("coarse-ok"))
        }),
        Arc::new(move |_| {
            d.set();
            Ok(())
        }),
        AbortMode::Coarse,
    );
    w.set_live_pending();
    w.start();
    w.submit(task("p1"));
    assert!(done.wait(Duration::from_secs(2)), "apply_fn was not invoked");
    w.stop(Duration::from_secs(1));
    assert_eq!(*seen.lock().unwrap(), vec![false]);
}

#[test]
fn started_worker_when_stopped_then_it_joins_within_the_timeout() {
    let w = worker(Arc::new(|_, _| Ok(result(""))), no_apply(), AbortMode::Cooperative);
    w.start();
    let started = Instant::now();
    w.stop(Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!w.is_running());
}

fn unavailable_worker(clock: &ManualClock, apply: ApplyFn) -> RefineWorker {
    RefineWorker::new(
        new_model_lock(),
        Arc::new(|_, _| Err(AsrError::TemporarilyUnavailable)),
        apply,
        RefineOptions { clock: clock.shared(), ..options(AbortMode::Coarse) },
    )
}

#[test]
fn asr_temporarily_unavailable_when_processing_then_the_task_is_requeued_after_a_pause() {
    let clock = ManualClock::new(0.0);
    let w = unavailable_worker(&clock, no_apply());
    let task = Arc::new(task("p1"));
    w.process_task(task.clone()).unwrap();
    assert_eq!(w.queued(), 1, "task must be re-enqueued, not lost");
    assert!(Arc::ptr_eq(&w.take_queued().unwrap(), &task));
    assert!(clock.sleeps().first().is_some_and(|&s| s >= 2.0), "must pause before re-enqueue");
}

#[test]
fn asr_temporarily_unavailable_when_processing_then_apply_is_not_called() {
    let clock = ManualClock::new(0.0);
    let applied = Arc::new(AtomicBool::new(false));
    let a = applied.clone();
    let w = unavailable_worker(
        &clock,
        Arc::new(move |_| {
            a.store(true, Ordering::SeqCst);
            Ok(())
        }),
    );
    w.process_task(task("p1")).unwrap();
    assert!(!applied.load(Ordering::SeqCst));
}

#[test]
fn failing_transcribe_and_apply_when_the_worker_runs_then_both_are_logged_and_it_goes_on() {
    let (logger, captured) = quiet();
    let done = Arc::new(Flag::default());
    let d = done.clone();
    let w = RefineWorker::new(
        new_model_lock(),
        Arc::new(|pcm, _| if pcm.len() == 1 { Err(AsrError::Failed("boom".into())) } else { Ok(result("ok")) }),
        Arc::new(move |_| {
            d.set();
            Err("disk full".into())
        }),
        RefineOptions { logger, ..options(AbortMode::Coarse) },
    );
    w.start();
    w.submit(RefineTask { pcm_samples: vec![0], ..task("p1") });
    w.submit(task("p2"));
    assert!(done.wait(Duration::from_secs(2)));
    w.stop(Duration::from_secs(1));
    assert_eq!(
        captured.lines(),
        vec![
            "[RefineWorker] task processing raised RuntimeError: boom",
            "[RefineWorker] apply_fn raised RuntimeError: disk full",
        ]
    );
}

#[test]
fn stopped_worker_when_submitting_then_the_queue_stays_empty() {
    let w = worker(Arc::new(|_, _| Ok(result(""))), no_apply(), AbortMode::Coarse);
    w.start();
    w.stop(Duration::from_secs(1));
    w.submit(task("p1"));
    assert_eq!(w.queued(), 0);
}

#[test]
fn reentrant_model_lock_when_the_callback_takes_it_under_the_worker_then_no_deadlock() {
    let lock = new_model_lock();
    let inner = lock.clone();
    let done = Arc::new(Flag::default());
    let d = done.clone();
    let w = RefineWorker::new(
        lock,
        Arc::new(move |_, _| {
            let _again = inner.lock();
            Ok(result("ok"))
        }),
        Arc::new(move |_| {
            d.set();
            Ok(())
        }),
        options(AbortMode::Coarse),
    );
    w.start();
    w.submit(task("p1"));
    assert!(done.wait(Duration::from_secs(2)));
    w.stop(Duration::from_secs(1));
}
