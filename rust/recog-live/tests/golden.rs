//! The golden recorder: the recorder-level cases of tests/test_golden.py.
//!
//! Not ported, `LiveTranscriber` wiring that belongs to the main loop:
//! `test_live_asr_calls_with_recording_on_their_answers_are_recorded`,
//! `test_golden_record_setting_start_golden_recording_opens_a_session_under_the_data_dir`
//! and `test_no_conference_live_audio_and_asr_are_not_recorded`.

mod common;

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use recog_core::pyjson::{self, Value};
use recog_live::asr::{Transcription, Word};
use recog_live::clock::ManualClock;
use recog_live::golden::{
    AsrOutput, Audio, AudioData, EVENTS, GoldenRecorder, audio_key, read_session, remove_old_sessions,
};
use recog_live::log::Logger;

/// A clock that advances 0.5 s per reading, from 100.
fn ticks() -> Box<dyn Fn() -> f64 + Send + Sync> {
    let now = Mutex::new(100.0);
    Box::new(move || {
        let mut n = now.lock().unwrap();
        *n += 0.5;
        *n
    })
}

fn recorder(dir: &std::path::Path) -> GoldenRecorder {
    GoldenRecorder::new(dir).unwrap()
}

#[test]
fn recorded_session_read_session_returns_every_input_in_order_exactly() {
    // given: a session with a connect, a frame, captured audio, an ASR answer and a disconnect
    let dir = tempfile::tempdir().unwrap();
    let mut rng = common::Rng::new(1);
    #[allow(clippy::cast_possible_truncation)]
    let mic: Vec<f32> = (0..48000).map(|_| ((rng.uniform() - 0.5) * 0.2) as f32).collect();
    let (logger, _) = Logger::capture(ManualClock::new(0.0).shared());
    let recorder = GoldenRecorder::with_clocks(dir.path(), ticks(), Box::new(|| 1_700_000_000.0), logger).unwrap();
    recorder.client_connected(1);
    let frame = pyjson::loads(r#"{"type": "speaker_update", "speaker": {"id": "a", "name": "Alice"}}"#).unwrap();
    recorder.frame(1, &frame);
    recorder.capture("mic", Audio::F32(&mic), 1_699_999_997.0);
    recorder.asr("transcribe_chunk", &[Audio::F32(&mic)], AsrOutput::Text("привет"));
    recorder.client_disconnected(1);
    recorder.close();

    // when: it is read back
    let events = read_session(dir.path()).unwrap();

    // then: the same events in order, times from the start, and the audio bit for bit
    let kinds: Vec<_> = events.iter().map(|e| e.get("kind").unwrap().as_str().unwrap().to_owned()).collect();
    assert_eq!(kinds, ["connect", "frame", "capture", "asr", "disconnect"]);
    let times: Vec<_> = events.iter().map(|e| e.get("t").unwrap().clone()).collect();
    assert_eq!(times, [0.5, 1.0, 1.5, 2.0, 2.5].map(Value::Float));
    assert_eq!(events[1].get("data"), Some(&frame));
    assert_eq!(events[2].get("source").unwrap().as_str(), Some("mic"));
    assert_eq!(events[2].get("start_ts"), Some(&Value::Float(1_699_999_997.0)));
    assert_eq!(events[2].audio, Some(AudioData::F32(mic.clone())));
    assert_eq!(events[3].get("inputs"), Some(&Value::Array(vec![Value::Str(audio_key(Audio::F32(&mic)))])));
    assert_eq!(events[3].get("output").unwrap().as_str(), Some("привет"));
}

#[test]
fn asr_outputs_of_every_shape_are_kept_as_plain_data() {
    // given: a batch answer and a refine answer with words
    let dir = tempfile::tempdir().unwrap();
    let clips = [[0i16; 10], [1i16; 10]];
    let refine = Transcription {
        text: "Привет мир".into(),
        words: vec![Word { text: "Привет".into(), start: 0, end: 80 }],
    };
    let recorder = recorder(dir.path());
    let texts = ["а".to_owned(), "б".to_owned()];
    recorder.asr("transcribe_batch", &[Audio::I16(&clips[0]), Audio::I16(&clips[1])], AsrOutput::Texts(&texts));
    recorder.asr("transcribe_with_timestamps", &[Audio::I16(&clips[0])], AsrOutput::Timed(&refine));
    recorder.asr("transcribe_chunk", &[Audio::I16(&clips[0])], AsrOutput::None);
    recorder.close();

    // when: they are read back
    let events = read_session(dir.path()).unwrap();

    // then: the texts and the words (text, start ms, end ms), with a key per input clip
    let keys: Vec<Value> = clips.iter().map(|c| Value::Str(audio_key(Audio::I16(c)))).collect();
    assert_eq!(events[0].get("inputs"), Some(&Value::Array(keys)));
    assert_eq!(pyjson::dumps(events[0].get("output").unwrap(), None), r#"["а", "б"]"#);
    assert_eq!(
        pyjson::dumps(events[1].get("output").unwrap(), None),
        r#"{"text": "Привет мир", "words": [["Привет", 0, 80]]}"#
    );
    assert_eq!(events[2].get("output"), Some(&Value::Null));
}

#[test]
fn same_samples_other_type_audio_key_differs() {
    // given: the same numbers as int16 and as float32 / when: keyed / then: different keys
    assert_ne!(audio_key(Audio::I16(&[0; 4])), audio_key(Audio::F32(&[0.0; 4])));
}

#[test]
fn threads_recording_at_once_every_line_is_whole() {
    // given: the bridge thread, the ingest worker and the main loop recording at once
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(recorder(dir.path()));
    let threads: Vec<_> = (0..4)
        .map(|n| {
            let recorder = recorder.clone();
            std::thread::spawn(move || {
                for i in 0..200u16 {
                    recorder.capture(&format!("s{n}"), Audio::F32(&[f32::from(i); 160]), f64::from(i));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    recorder.close();

    // when: the session is read
    let text = std::fs::read_to_string(dir.path().join(EVENTS)).unwrap();
    // then: every event is there and every line parses
    assert_eq!(text.lines().count(), 800);
    assert!(text.lines().all(|l| pyjson::loads(l).is_ok()));
    for event in read_session(dir.path()).unwrap() {
        let Some(Value::Float(start)) = event.get("start_ts") else { panic!("no start_ts") };
        #[allow(clippy::cast_possible_truncation)]
        let expected = *start as f32;
        let Some(AudioData::F32(samples)) = &event.audio else { panic!("no audio") };
        assert!(samples.iter().all(|&s| s.to_bits() == expected.to_bits()));
    }
}

struct Full;

impl io::Write for Full {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(28))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn disk_error_recording_stops_quietly_and_the_caller_goes_on() {
    // given: a write that fails (a full disk)
    let dir = tempfile::tempdir().unwrap();
    let (logger, captured) = Logger::capture(ManualClock::new(0.0).shared());
    let recorder = GoldenRecorder::with_clocks(dir.path(), ticks(), Box::new(|| 0.0), logger).unwrap();
    recorder.replace_audio_writer(Box::new(Full));
    // when: more events come
    recorder.capture("mic", Audio::F32(&[0.0; 10]), 0.0);
    recorder.capture("mic", Audio::F32(&[0.0; 10]), 0.0);
    recorder.frame(1, &pyjson::loads(r#"{"type": "x"}"#).unwrap());
    // then: no panic reaches the caller; recording stops and says so once
    assert_eq!(captured.lines(), vec!["[golden] golden recording stopped: [Errno 28] No space left on device"]);
    assert_eq!(std::fs::read_to_string(dir.path().join(EVENTS)).unwrap(), "");
}

fn set_mtime(path: &std::path::Path, seconds: f64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs_f64(seconds)).unwrap();
}

#[test]
fn sessions_of_every_age_remove_old_sessions_keeps_recent_and_marked_ones() {
    // given: sessions 20 and 2 days old, one 20 days old marked -keep, and a stray file
    let root = tempfile::tempdir().unwrap();
    let now = 1_800_000_000.0;
    for (name, days) in [("old", 20.0), ("recent", 2.0), ("old-keep", 20.0)] {
        recorder(&root.path().join(name)).close();
        set_mtime(&root.path().join(name).join(EVENTS), now - days * 86400.0);
    }
    std::fs::write(root.path().join("notes.txt"), "x").unwrap();
    // when: old sessions are removed with a 14-day limit
    let removed = remove_old_sessions(root.path(), 14.0, Some(now)).unwrap();
    // then: only the unmarked 20-day-old session goes
    let mut left: Vec<_> =
        std::fs::read_dir(root.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    left.sort();
    assert_eq!(left, ["notes.txt", "old-keep", "recent"]);
    assert_eq!(removed, vec![root.path().join("old")]);
    assert!(remove_old_sessions(&root.path().join("missing"), 14.0, Some(now)).unwrap().is_empty());
}

#[test]
fn open_session_read_session_sees_every_event_so_far() {
    // given: a session still being recorded
    let dir = tempfile::tempdir().unwrap();
    let recorder = recorder(dir.path());
    recorder.frame(1, &pyjson::loads(r#"{"type": "clock_sync"}"#).unwrap());
    recorder.capture("mic", Audio::F32(&[1.0; 160]), 0.0);
    // when: it is read now
    let events = read_session(dir.path()).unwrap();
    // then: every event so far is on disk, with its audio
    let kinds: Vec<_> = events.iter().map(|e| e.get("kind").unwrap().as_str().unwrap().to_owned()).collect();
    assert_eq!(kinds, ["frame", "capture"]);
    assert_eq!(events[1].audio, Some(AudioData::F32(vec![1.0; 160])));
    recorder.close();
}

#[test]
fn existing_session_reopened_offsets_continue_after_the_audio_already_there() {
    // given: a session with audio, reopened by a second recorder (append, as Python's "ab")
    let dir = tempfile::tempdir().unwrap();
    let first = recorder(dir.path());
    first.capture("mic", Audio::I16(&[1, 2, 3]), 0.0);
    first.close();
    let second = recorder(dir.path());
    second.capture("system", Audio::F32(&[0.5]), 1.0);
    second.close();
    // when: read back / then: both chunks, the second at offset 6
    let events = read_session(dir.path()).unwrap();
    assert_eq!(events[1].get("offset"), Some(&Value::Int(6)));
    assert_eq!(events[0].audio, Some(AudioData::I16(vec![1, 2, 3])));
    assert_eq!(events[1].audio, Some(AudioData::F32(vec![0.5])));
}
