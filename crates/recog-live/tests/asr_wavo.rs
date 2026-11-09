//! `WavoAsr` with a fake engine: tests/test_wavo_backend.py's adapter cases.
//!
//! Not ported, Python wiring with no Rust counterpart yet: `build_asr()` choosing the
//! backend (`test_backend_wavo_build_asr_returns_wavo_asr`,
//! `test_default_backend_auto_build_asr_takes_wavo_on_a_mac_where_it_is_built`), the
//! readiness probe of `scripts/deps.sh` (`test_backend_wavo_readiness_needs_the_rust_module`)
//! and the live process not importing torch (`test_backend_wavo_live_process_imports_no_torch`).
//! `test_wavo_refine_live_pending_aborts_only_before_the_decode` tests
//! `LiveTranscriber._refine_transcribe_fn`; its lock half is here, the abort check
//! belongs to the main loop.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use recog_live::asr::wavo::{EngineAnswer, Loader};
use recog_live::asr::{AsrError, Clip, Engine, WavoAsr, Word};
use recog_live::clock::ManualClock;
use recog_live::log::{Captured, Logger};

type Answer = Box<dyn Fn(&[f32]) -> Result<EngineAnswer, AsrError> + Send + Sync>;

/// Answers every clip with `answer` and records what it got.
struct Fake {
    answer: Mutex<Answer>,
    seen: Mutex<Vec<Vec<f32>>>,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Box::new(|_| {
                Ok(("Привет мир".into(), vec![("▁При".into(), 0), ("вет".into(), 40), ("▁мир".into(), 200)]))
            })),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn answer(&self, f: impl Fn(&[f32]) -> Result<EngineAnswer, AsrError> + Send + Sync + 'static) {
        *self.answer.lock().unwrap() = Box::new(f);
    }
}

impl Engine for Fake {
    fn transcribe(&self, pcm: &[f32]) -> Result<EngineAnswer, AsrError> {
        self.seen.lock().unwrap().push(pcm.to_vec());
        (self.answer.lock().unwrap())(pcm)
    }

    fn device(&self) -> String {
        "Fake GPU Metal".into()
    }
}

fn asr_with(fake: &Arc<Fake>) -> (WavoAsr, Captured) {
    let engine = fake.clone();
    let loader: Loader = Box::new(move || Ok(engine.clone() as Arc<dyn Engine>));
    let (logger, captured) = Logger::capture(ManualClock::new(0.0).shared());
    (WavoAsr::new(loader, logger), captured)
}

#[test]
fn tokens_with_word_starts_timestamps_give_gigaams_words() {
    // given: tokens where "▁" starts a word, 40 ms encoder frames
    let fake = Fake::new();
    let (asr, _) = asr_with(&fake);
    // when: a refine window is transcribed with timestamps
    let result = asr.transcribe_with_timestamps(Clip::F32(&[0.0; 16000]), 16000).unwrap();
    // then: words as GigaAM builds them, in ms
    assert_eq!(result.text, "Привет мир");
    assert_eq!(
        result.words,
        vec![Word { text: "Привет".into(), start: 0, end: 80 }, Word { text: "мир".into(), start: 200, end: 240 }]
    );
}

#[test]
fn int16_pcm_any_path_reaches_the_model_as_floats_in_range() {
    // given: Int16 PCM, as the extension's per-speaker stream sends it
    let fake = Fake::new();
    let (asr, _) = asr_with(&fake);
    // when: it goes through the batch path
    asr.transcribe_batch(&[Clip::I16(&[0, 16384, -32768])]).unwrap();
    // then: the model gets float32 in [-1, 1]
    assert_eq!(fake.seen.lock().unwrap()[0], vec![0.0, 0.5, -1.0]);
}

#[test]
fn batch_of_clips_transcribe_batch_answers_each_in_order() {
    // given: three clips
    let fake = Fake::new();
    let texts = Arc::new(Mutex::new(vec!["три", "два", "один"]));
    let t = texts.clone();
    fake.answer(move |_| Ok((t.lock().unwrap().pop().unwrap().into(), vec![])));
    let (asr, _) = asr_with(&fake);
    let clip = [0.0f32; 10];
    // when: they are transcribed as one batch
    let got = asr.transcribe_batch(&[Clip::F32(&clip), Clip::F32(&clip), Clip::F32(&clip)]).unwrap();
    // then: one text per clip, in the input order
    assert_eq!(got, vec!["один", "два", "три"]);
    assert!(asr.transcribe_batch(&[]).unwrap().is_empty());
}

#[test]
fn live_chunk_transcribe_chunk_strips_or_gives_none() {
    for (text, expected) in [(" слово ", Some("слово")), ("", None), ("   ", None)] {
        // given: the model's text for a live chunk
        let fake = Fake::new();
        fake.answer(move |_| Ok((text.into(), vec![])));
        let (asr, _) = asr_with(&fake);
        // when: the chunk is transcribed
        // then: the text stripped, None when there is nothing
        assert_eq!(asr.transcribe_chunk(Clip::F32(&[0.0; 10]), 16000).as_deref(), expected, "{text:?}");
    }
}

#[test]
fn model_error_transcribe_chunk_answers_none() {
    // given: a model that fails on a chunk
    let fake = Fake::new();
    fake.answer(|_| Err(AsrError::Failed("device lost".into())));
    let (asr, captured) = asr_with(&fake);
    // when: the live path transcribes it, twice
    let first = asr.transcribe_chunk(Clip::F32(&[0.0; 10]), 16000);
    let second = asr.transcribe_chunk(Clip::F32(&[0.0; 10]), 16000);
    // then: None, not an error into the loop, and one rate-limited line
    assert_eq!((first, second), (None, None));
    assert_eq!(captured.lines(), vec!["[WavoASR] transcribe_chunk failed: RuntimeError: device lost"]);
}

#[test]
fn other_sample_rate_transcribe_with_timestamps_raises_value_error() {
    // given: audio at 48 kHz
    let fake = Fake::new();
    let (asr, _) = asr_with(&fake);
    // when: it is transcribed
    let err = asr.transcribe_with_timestamps(Clip::F32(&[0.0; 10]), 48000).unwrap_err();
    // then: an input error naming 16000, and the model never called
    assert_eq!(err, AsrError::InvalidInput("wavo takes 16000 Hz audio, got 48000".into()));
    assert!(fake.seen.lock().unwrap().is_empty());
}

#[test]
fn two_threads_load_model_loads_once() {
    // given: two threads asking for the model at once
    let made = Arc::new(AtomicUsize::new(0));
    let m = made.clone();
    let loader: Loader = Box::new(move || {
        m.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        Ok(Fake::new() as Arc<dyn Engine>)
    });
    let (logger, _) = Logger::capture(ManualClock::new(0.0).shared());
    let asr = Arc::new(WavoAsr::new(loader, logger));
    // when: both call load_model
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let asr = asr.clone();
            std::thread::spawn(move || asr.load_model(true).unwrap())
        })
        .collect();
    let got: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    // then: one model is made and both get it
    assert_eq!(made.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&got[0], &got[1]));
}

#[test]
fn explicit_load_load_model_says_so_and_names_the_device() {
    // given: a fresh ASR
    let fake = Fake::new();
    let (asr, captured) = asr_with(&fake);
    assert_eq!(asr.device(), "wavo");
    // when: the model is loaded loudly, then quietly by a call
    asr.load_model(false).unwrap();
    asr.transcribe_chunk(Clip::F32(&[0.0; 10]), 16000);
    // then: one loading line, and the device named after the engine's
    assert_eq!(captured.lines(), vec!["Загрузка модели gigaam-v3 (wavo)..."]);
    assert_eq!(asr.device(), "wavo Fake GPU Metal");
}

#[test]
fn failed_load_transcribe_chunk_answers_none_and_the_next_call_loads_again() {
    // given: a loader that fails once
    let tries = Arc::new(AtomicUsize::new(0));
    let t = tries.clone();
    let loader: Loader = Box::new(move || {
        if t.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(AsrError::Failed("no GPU adapter".into()))
        } else {
            Ok(Fake::new() as Arc<dyn Engine>)
        }
    });
    let (logger, captured) = Logger::capture(ManualClock::new(0.0).shared());
    let asr = WavoAsr::new(loader, logger);
    // when: two chunks come
    let first = asr.transcribe_chunk(Clip::F32(&[0.0; 10]), 16000);
    let second = asr.transcribe_chunk(Clip::F32(&[0.0; 10]), 16000);
    // then: None for the first, the text for the second
    assert_eq!(first, None);
    assert_eq!(second.as_deref(), Some("Привет мир"));
    assert_eq!(captured.lines(), vec!["[WavoASR] transcribe_chunk failed: RuntimeError: no GPU adapter"]);
}

#[test]
fn refine_holding_the_model_lock_live_call_waits_until_it_is_released() {
    // given: the refine path holding the model lock and decoding under it
    let fake = Fake::new();
    let (asr, _) = asr_with(&fake);
    let asr = Arc::new(asr);
    let guard = asr.lock_model();
    let refined = asr.transcribe_with_timestamps_locked(&guard, Clip::I16(&[0; 1600]), 16000).unwrap();
    assert_eq!(refined.text, "Привет мир");
    // when: a live chunk comes meanwhile
    let (tx, rx) = mpsc::channel();
    let live = {
        let asr = asr.clone();
        std::thread::spawn(move || tx.send(asr.transcribe_chunk(Clip::F32(&[0.0; 10]), 16000)).unwrap())
    };
    // then: it waits for the refine, then runs
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    drop(guard);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().as_deref(), Some("Привет мир"));
    live.join().unwrap();
}

#[test]
fn audio_too_long_transcribe_with_timestamps_is_an_input_error() {
    // given: a model refusing the clip as too long (wavo's ValueError)
    let fake = Fake::new();
    fake.answer(|_| Err(AsrError::InvalidInput("audio is 40000 ms, longer than the model's 37500 ms".into())));
    let (asr, _) = asr_with(&fake);
    // when / then: the error reaches the refine caller as an input error
    let err = asr.transcribe_with_timestamps(Clip::F32(&[0.0; 10]), 16000).unwrap_err();
    assert!(matches!(err, AsrError::InvalidInput(m) if m.contains("37500")));
}
