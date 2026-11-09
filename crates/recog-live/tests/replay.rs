//! The golden replay (`recog replay`): its ASR answering from the recording, and the
//! frozen KTalk call of tests/fixtures/golden_ktalk_pcm replayed into the live process
//! in this process, as tests/test_e2e_live_process.py replays it into the binary.
//!
//! The frozen session lasts 22.5 s and is replayed in real time.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use recog_core::install_config::InstallConfig;
use recog_core::pyjson::{self, Map, Value};
use recog_live::asr::{AsrError, Transcription, Word};
use recog_live::clock::{ManualClock, SystemClock};
use recog_live::golden::{Audio, Event, audio_key};
use recog_live::live::LiveAsr;
use recog_live::live::replay::{ReplayAsr, ReplaySettings, replay};
use recog_live::log::Logger;

fn event(json: &str) -> Event {
    let Value::Object(fields) = pyjson::loads(json).unwrap() else { panic!("{json}") };
    Event { fields, audio: None }
}

fn asr_event(call: &str, inputs: &[String], output: &str) -> Event {
    let inputs = inputs.iter().map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(", ");
    event(&format!(r#"{{"kind": "asr", "call": "{call}", "inputs": [{inputs}], "output": {output}}}"#))
}

const CHUNK: [f32; 4] = [0.1, -0.1, 0.2, -0.2];
const FIRST: [i16; 3] = [1, 2, 3];
const SECOND: [i16; 3] = [4, 5, 6];
const REFINED: [i16; 3] = [7, 8, 9];

fn recorded_asr() -> ReplayAsr {
    let chunk = audio_key(Audio::F32(&CHUNK));
    let first = audio_key(Audio::I16(&FIRST));
    let second = audio_key(Audio::I16(&SECOND));
    let refined = audio_key(Audio::I16(&REFINED));
    ReplayAsr::from_events(&[
        event(r#"{"kind": "frame", "client": 1, "data": {}}"#),
        asr_event("transcribe_chunk", std::slice::from_ref(&chunk), r#""привет""#),
        asr_event("transcribe_batch", &[first, second], r#"["раз", "два"]"#),
        asr_event(
            "transcribe_with_timestamps",
            &[refined],
            r#"{"text": "Раз Два", "words": [["Раз", 0, 400], ["Два", 400.9, 800], ["лишнее", 1], 7]}"#,
        ),
    ])
}

#[test]
fn recorded_answers_when_asked_for_the_same_audio_then_the_recorded_answers_and_no_misses() {
    // given
    let asr = recorded_asr();
    // when
    let chunk = asr.transcribe_chunk(&CHUNK).unwrap();
    let batch = asr.transcribe_batch(&[&FIRST, &SECOND]).unwrap();
    let timed = asr.transcribe_with_timestamps(&REFINED, None).unwrap();
    // then: a float ms is truncated as int(), a word that is not [text, start, end] is left out
    assert_eq!(chunk.as_deref(), Some("привет"));
    assert_eq!(batch, ["раз", "два"]);
    assert_eq!(
        timed,
        Transcription {
            text: "Раз Два".into(),
            words: vec![
                Word { text: "Раз".into(), start: 0, end: 400 },
                Word { text: "Два".into(), start: 400, end: 800 },
            ],
        }
    );
    assert!(asr.misses().is_empty());
}

#[test]
fn audio_the_recording_never_saw_when_asked_then_empty_answers_and_each_call_counted_as_a_miss() {
    // given
    let asr = recorded_asr();
    let other = [9_i16; 3];
    // when
    let chunk = asr.transcribe_chunk(&[0.5; 4]).unwrap();
    let batch = asr.transcribe_batch(&[&other, &FIRST]).unwrap();
    let timed = asr.transcribe_with_timestamps(&other, None).unwrap();
    // then
    assert_eq!(chunk, None);
    assert_eq!(batch, ["", "раз"]);
    assert_eq!(timed, Transcription::default());
    assert_eq!(asr.misses(), ["transcribe_chunk", "transcribe_batch", "transcribe_with_timestamps"]);
}

#[test]
fn a_refine_told_to_abort_when_asked_then_aborted_before_any_lookup() {
    // given
    let asr = recorded_asr();
    // when
    let result = asr.transcribe_with_timestamps(&[9; 3], Some(&|| true));
    // then
    assert_eq!(result, Err(AsrError::Aborted));
    assert!(asr.misses().is_empty());
}

#[test]
fn a_refine_answer_without_words_or_of_another_shape_when_asked_then_its_text_or_nothing() {
    // given
    let first = audio_key(Audio::I16(&FIRST));
    let second = audio_key(Audio::I16(&SECOND));
    let asr = ReplayAsr::from_events(&[
        asr_event("transcribe_with_timestamps", std::slice::from_ref(&first), r#"{"text": "только текст"}"#),
        asr_event("transcribe_with_timestamps", std::slice::from_ref(&second), r#""не словарь""#),
        event(r#"{"kind": "asr", "call": "transcribe_chunk", "output": "без входа"}"#),
    ]);
    // when
    let words_missing = asr.transcribe_with_timestamps(&FIRST, None).unwrap();
    let not_a_map = asr.transcribe_with_timestamps(&SECOND, None).unwrap();
    // then
    assert_eq!(words_missing, Transcription { text: "только текст".into(), words: Vec::new() });
    assert_eq!(not_a_map, Transcription::default());
    assert!(asr.misses().is_empty());
}

fn frozen_session(into: &Path) -> PathBuf {
    let frozen = common::repo_root().join("tests/fixtures/golden_ktalk_pcm");
    let session = into.join("session");
    std::fs::create_dir_all(&session).unwrap();
    for name in ["events.jsonl", "audio.bin"] {
        let output =
            std::process::Command::new("gzip").arg("-dc").arg(frozen.join(format!("{name}.gz"))).output().unwrap();
        assert!(output.status.success(), "gzip -dc {name}.gz failed");
        std::fs::write(session.join(name), output.stdout).unwrap();
    }
    session
}

fn rows(data: &Path) -> Vec<Map> {
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(data.join("meetings")).unwrap() {
        let transcript = entry.unwrap().path().join("transcript.jsonl");
        let Ok(text) = std::fs::read_to_string(transcript) else { continue };
        for line in text.lines() {
            if let Value::Object(row) = pyjson::loads(line).unwrap() {
                rows.push(row);
            }
        }
    }
    rows
}

fn str_of<'a>(row: &'a Map, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

struct Utc;

impl recog_core::time::Clock for Utc {
    fn now_micros(&self) -> i64 {
        0
    }
    fn utc_offset(&self, _utc_micros: i64) -> i64 {
        0
    }
}

#[test]
fn the_frozen_ktalk_call_when_replayed_then_every_asr_call_answered_and_both_remote_speakers_refined() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let session = frozen_session(dir.path());
    let out = dir.path().join("out");
    let saved = dir.path().join("saved");
    recog_core::user_profile::UserProfileStore::new(&saved).write("Вера", &[]).unwrap();
    let (log, _stderr) = Logger::capture(ManualClock::new(0.0).shared());
    let stdout = Arc::new(Mutex::new(String::new()));
    let sink = stdout.clone();
    let settings = ReplaySettings {
        config: InstallConfig::default(),
        legacy_mic_name: "Me".into(),
        profile: Some(saved.join("user-profile.json")),
        tail_s: 3.0,
        clock: SystemClock::shared(),
        zone: Arc::new(Utc),
        log,
        out: Arc::new(move |text: &str| sink.lock().unwrap().push_str(text)),
        policy: None,
    };
    // when
    let summary = replay(&session, &out, settings).unwrap();
    // then: the summary is the one written, every ASR call found its recorded answer
    let written = pyjson::loads(&std::fs::read_to_string(out.join("replay.json")).unwrap()).unwrap();
    assert_eq!(written, summary);
    let summary = summary.as_object().unwrap().clone();
    assert_eq!(summary.get("asr_misses"), Some(&Value::Int(0)));
    assert_eq!(summary.get("events"), Some(&Value::Int(31)));
    assert_eq!(summary.get("session").and_then(Value::as_str), Some(session.display().to_string().as_str()));
    // and the transcript holds both remote speakers on their PCM, refined
    let rows = rows(&out.join("data"));
    let speakers: Vec<&str> = rows.iter().map(|r| str_of(r, "speaker")).collect();
    assert!(speakers.contains(&"Alice") && speakers.contains(&"Bob"), "{speakers:?}");
    assert!(rows.iter().any(|r| str_of(r, "text").contains("Привет Коллеги")), "{rows:?}");
    assert!(rows.iter().any(|r| r.get("refined") == Some(&Value::Bool(true))), "{rows:?}");
    // and the microphone rows carry the profile replayed with
    assert!(out.join("data/user-profile.json").is_file());
    let local: Vec<&str> =
        rows.iter().filter(|r| r.get("is_local") == Some(&Value::Bool(true))).map(|r| str_of(r, "speaker")).collect();
    assert!(!local.is_empty() && local.iter().all(|s| *s == "Вера"), "{local:?}");
    assert!(stdout.lock().unwrap().contains(" LIVE TRANSCRIPTION\n"));
}

#[test]
fn a_session_that_is_missing_or_empty_when_replayed_then_an_error_naming_it() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(empty.join("events.jsonl"), "").unwrap();
    std::fs::write(empty.join("audio.bin"), "").unwrap();
    let settings = || ReplaySettings {
        config: InstallConfig::default(),
        legacy_mic_name: "Me".into(),
        profile: None,
        tail_s: 0.0,
        clock: SystemClock::shared(),
        zone: Arc::new(Utc),
        log: Logger::capture(ManualClock::new(0.0).shared()).0,
        out: Arc::new(|_: &str| {}),
        policy: None,
    };
    // when
    let missing = replay(&dir.path().join("missing"), &dir.path().join("out"), settings()).unwrap_err();
    let nothing = replay(&empty, &dir.path().join("out"), settings()).unwrap_err();
    // then
    assert!(missing.starts_with(&format!("{}: ", dir.path().join("missing").display())), "{missing}");
    assert_eq!(nothing, format!("{} holds no events", empty.display()));
}
