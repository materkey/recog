//! The cases of tests/test_meeting_storage.py and the storage half of
//! tests/test_audio_retention.py, against the port.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use super::*;

const NOW: i64 = 1_783_159_200_000_000; // 2026-07-04T10:00:00Z
const MINUTE: i64 = 60_000_000;

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now_micros(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn utc_offset(&self, _utc_micros: i64) -> i64 {
        0
    }
}

struct StubTitles;

impl TitleGenerator for StubTitles {
    fn generate(&self, _request: &TitleRequest) -> Result<Option<GeneratedTitle>, String> {
        Ok(Some(GeneratedTitle {
            title: "Generated title".into(),
            provider: "stub".into(),
            model: Value::from("m"),
            settings_revision: Value::from("r"),
        }))
    }
}

fn storage_at(dir: &Path) -> (MeetingStorage, Arc<TestClock>) {
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let mut options = StorageOptions::new(&dir.join("home"));
    options.clock = clock.clone();
    options.external_index_cache_dir = dir.join("cache");
    options.title_generator = Some(Arc::new(StubTitles));
    options.log = Arc::new(|_, _| {});
    (MeetingStorage::new(&dir.join("meetings"), options).unwrap(), clock)
}

fn ktalk() -> SessionMode {
    SessionMode { ktalk: true, ..SessionMode::default() }
}

fn said(speaker: &str, text: &str) -> NewSegment {
    NewSegment { speaker: speaker.into(), speaker_id: speaker.into(), text: text.into(), ..NewSegment::default() }
}

fn rows(storage: &MeetingStorage) -> Vec<Value> {
    storage.get_current_transcript().unwrap()
}

fn current_meta(storage: &MeetingStorage) -> Value {
    read_json(&storage.current_meeting_path().unwrap().join("meta.json")).unwrap()
}

fn field<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |v, key| v.as_object().and_then(|m| m.get(key)).unwrap_or(&Value::Null))
}

fn text_update(segment_id: i64, text: &str) -> SegmentUpdate {
    SegmentUpdate { segment_id, text: Some(text.into()), ..SegmentUpdate::default() }
}

#[test]
fn a_new_segment_when_appended_then_its_row_has_python_keys_and_refined_false() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    storage.start_session(&ktalk()).unwrap();
    // when
    storage.append_segment(&said("Alice", "hello")).unwrap();
    // then
    let text = fs::read_to_string(storage.current_meeting_path().unwrap().join("transcript.jsonl")).unwrap();
    assert_eq!(
        text,
        "{\"id\": 1, \"ts\": \"2026-07-04T10:00:00+00:00\", \"ts_display\": \"10:00:00\", \"speaker\": \"Alice\", \
         \"speaker_id\": \"Alice\", \"text\": \"hello\", \"is_local\": false, \"cont\": false, \"uncertain\": false, \
         \"uncertain_reason\": \"\", \"audio_offset_ms\": 0, \"refined\": false}\n"
    );
}

#[test]
fn update_segments_with_text_when_applied_then_text_rewritten_refined_true_and_words_counted() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&said("Alice", "hello world")).unwrap();
    storage.append_segment(&said("Bob", "untouched")).unwrap();
    // when
    storage.update_segments(&[text_update(1, "hello brave new world")], None).unwrap();
    // then
    let rows = rows(&storage);
    assert_eq!(field(&rows[0], &["text"]), &Value::from("hello brave new world"));
    assert_eq!(field(&rows[0], &["refined"]), &Value::Bool(true));
    assert_eq!(field(&rows[0], &["speaker"]), &Value::from("Alice"));
    assert_eq!(field(&rows[1], &["refined"]), &Value::Bool(false));
    assert_eq!(field(&current_meta(&storage), &["stats", "words_count"]), &Value::Int(5));
}

#[test]
fn update_segments_without_text_when_applied_then_speaker_changes_and_refined_stays() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    storage.start_session(&ktalk()).unwrap();
    storage
        .append_segment(&NewSegment { uncertain: true, uncertain_reason: "local".into(), ..said("Unknown", "hi") })
        .unwrap();
    // when
    let update = SegmentUpdate {
        segment_id: 1,
        speaker: Some("Alice".into()),
        uncertain: Some(false),
        uncertain_reason: Some(String::new()),
        text: None,
    };
    storage.update_segments(&[update], None).unwrap();
    // then
    let row = &rows(&storage)[0];
    assert_eq!(field(row, &["speaker"]), &Value::from("Alice"));
    assert_eq!(field(row, &["speaker_id"]), &Value::from("Alice"));
    assert_eq!(field(row, &["uncertain"]), &Value::Bool(false));
    assert_eq!(field(row, &["refined"]), &Value::Bool(false));
}

#[test]
fn a_legacy_row_without_refined_when_rewritten_then_only_refined_rows_gain_the_key() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    storage.start_session(&ktalk()).unwrap();
    let path = storage.current_meeting_path().unwrap().join("transcript.jsonl");
    fs::write(&path, "{\"id\": 1, \"text\": \"a\"}\n{\"id\": 2, \"text\": \"b\"}\n").unwrap();
    // when
    storage.update_segments(&[text_update(2, "c d")], None).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\"id\": 1, \"text\": \"a\"}\n{\"id\": 2, \"text\": \"c d\", \"refined\": true}\n"
    );
    assert!(!path.with_extension("jsonl.tmp").exists());
}

#[test]
fn an_ended_meeting_when_deleted_then_its_directory_and_index_entry_go() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let id = storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&said("Alice", "hi there")).unwrap();
    storage.update_title("Test Meeting").unwrap();
    storage.end_session().unwrap();
    let index = storage.base_path().join("index.json");
    assert!(fs::read_to_string(&index).unwrap().contains(&id));
    // when
    let deleted = storage.delete_meeting(&id).unwrap();
    // then
    assert!(deleted);
    assert!(!storage.base_path().join(&id).exists());
    assert!(!fs::read_to_string(&index).unwrap().contains(&id));
    assert!(!storage.delete_meeting("2099-01-01_000000_deadbeef").unwrap());
}

#[test]
fn an_active_meeting_when_deleted_then_refused_and_kept() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let id = storage.start_session(&ktalk()).unwrap();
    // when
    let result = storage.delete_meeting(&id);
    // then
    assert_eq!(result, Err(StorageError::Value("Cannot delete an active meeting".into())));
    assert!(storage.base_path().join(&id).is_dir());
}

#[test]
fn the_middle_of_a_chain_when_deleted_then_its_neighbours_are_relinked() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let (a, b, c) = ("2026-07-04_090000_aaaaaaaa", "2026-07-04_100000_bbbbbbbb", "2026-07-04_110000_cccccccc");
    for (id, prev, next) in [(a, None, Some(b)), (b, Some(a), Some(c)), (c, Some(b), None)] {
        let mut meta = Map::new();
        meta.insert("id", Value::from(id));
        meta.insert("ended_at", Value::from("2026-07-04T10:00:00+00:00"));
        meta.insert("prev_meeting_id", Value::from(prev));
        meta.insert("next_meeting_id", Value::from(next));
        fs::create_dir_all(storage.base_path().join(id)).unwrap();
        save_json(&storage.base_path().join(id).join("meta.json"), &Value::Object(meta)).unwrap();
    }
    // when
    assert!(storage.delete_meeting(b).unwrap());
    // then
    let meta = |id: &str| read_json(&storage.base_path().join(id).join("meta.json")).unwrap();
    assert_eq!(field(&meta(a), &["next_meeting_id"]), &Value::from(c));
    assert_eq!(field(&meta(c), &["prev_meeting_id"]), &Value::from(a));
}

#[test]
fn conference_urls_when_joined_then_live_id_and_host_come_from_the_path_or_fragment() {
    for (url, platform, live_id, host) in [
        ("https://element.example.org/#/room/!abc", "matrix", "!abc", "element.example.org"),
        (
            "https://element.example.org/#/room/#recog-spike:matrix.example.org",
            "matrix",
            "#recog-spike:matrix.example.org",
            "element.example.org",
        ),
        ("https://example.ktalk.ru/abcd1234wxyz", "ktalk", "abcd1234wxyz", "example.ktalk.ru"),
        ("https://example.ktalk.ru/history/ExampleHistoryId0001", "ktalk", "ExampleHistoryId0001", "example.ktalk.ru"),
        ("https://meet.google.com/qrs-tuvw-xyz?authuser=0", "gmeet", "qrs-tuvw-xyz", "meet.google.com"),
        ("https://zoom.us/j/12345678901?pwd=secret", "zoom", "12345678901", "zoom.us"),
        ("https://element.example.org/", "matrix", "", "element.example.org"),
    ] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, _) = storage_at(dir.path());
        storage.start_session(&ktalk()).unwrap();
        // when
        storage.on_meeting_join(url, None, platform).unwrap();
        // then
        let meta = current_meta(&storage);
        assert_eq!(field(&meta, &["conference", "live_id"]), &Value::from(live_id), "{url}");
        assert_eq!(field(&meta, &["conference", "host"]), &Value::from(host), "{url}");
        assert_eq!(field(&meta, &["source"]), &Value::from(platform), "{url}");
    }
}

#[test]
fn zoom_meeting_just_left_rejoined_with_canonical_url_should_reopen_same_meeting() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, clock) = storage_at(dir.path());
    storage.start_session(&ktalk()).unwrap();
    let first = storage.on_meeting_join("https://zoom.us/j/12345678901?pwd=secret", Some("Zoom"), "zoom").unwrap();
    storage.append_segment(&said("Tester", "hello")).unwrap();
    storage.on_ktalk_leave("user_left").unwrap();
    clock.0.fetch_add(MINUTE, Ordering::SeqCst);
    // when
    let second = storage.on_meeting_join("https://zoom.us/j/12345678901", None, "zoom").unwrap();
    // then
    assert_eq!(second, first);
    let events: Vec<Value> = rows(&storage).iter().map(|r| field(r, &["event_type"]).clone()).collect();
    assert_eq!(events, vec![Value::Null, Value::from("session_pause"), Value::from("session_resume")]);
}

#[test]
fn a_room_left_long_ago_when_rejoined_then_a_new_meeting_starts_and_recently_the_same_reopens() {
    for (minutes, same) in [(49, false), (2, true)] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, clock) = storage_at(dir.path());
        storage.start_session(&ktalk()).unwrap();
        let first = storage.on_meeting_join("https://example.ktalk.ru/team-room", Some("Team"), "ktalk").unwrap();
        storage.append_segment(&said("Tester", "hello")).unwrap();
        storage.on_ktalk_leave("user_left").unwrap();
        clock.0.fetch_add(minutes * MINUTE, Ordering::SeqCst);
        // when
        let second = storage.on_meeting_join("https://example.ktalk.ru/team-room", None, "ktalk").unwrap();
        // then
        assert_eq!(second == first, same, "{minutes} minutes");
    }
}

#[test]
fn the_same_room_on_another_host_when_joined_then_the_recordings_stay_separate() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    storage.start_session(&ktalk()).unwrap();
    let first = storage.on_meeting_join("https://a.example.org/daily", Some("A"), "jitsi").unwrap().unwrap();
    storage.append_segment(&said("Tester", "hello")).unwrap();
    // when
    let second = storage.on_meeting_join("https://b.example.org/daily", Some("B"), "jitsi").unwrap();
    // then
    assert_ne!(second.as_deref(), Some(first.as_str()));
    let meta = storage.get_meeting(&first).unwrap().unwrap();
    assert_eq!(field(&meta, &["meta", "conference", "host"]), &Value::from("a.example.org"));
}

#[test]
fn a_conference_without_a_title_when_left_then_it_gets_the_generated_title_and_a_titled_one_keeps_its_own() {
    for (title, expected, source) in [(None, "Generated title", "stub"), (Some("Weekly sync"), "Weekly sync", "zoom")] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, _) = storage_at(dir.path());
        storage.start_session(&ktalk()).unwrap();
        let id = storage.on_meeting_join("https://zoom.us/app", title, "zoom").unwrap().unwrap();
        storage.append_segment(&said("Bob", "hello")).unwrap();
        // when
        storage.on_ktalk_leave("user_left").unwrap();
        storage.end_session().unwrap();
        // then
        let meta = read_json(&storage.base_path().join(&id).join("meta.json")).unwrap();
        assert_eq!(field(&meta, &["title"]), &Value::from(expected));
        assert_eq!(field(&meta, &["title_source"]), &Value::from(source));
    }
}

#[test]
fn title_generation_is_wanted_for_untitled_orphans_and_conferences_with_speech() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    for (source, title, segments, expected) in [
        (Some("orphan"), None, 3, true),
        (Some("zoom"), None, 3, true),
        (Some("ktalk"), Some(""), 1, true),
        (Some("zoom"), Some("Weekly sync"), 3, false),
        (Some("zoom"), None, 0, false),
        (Some("orphan"), None, 0, false),
        (Some("local"), None, 3, false),
        (Some("diarized"), None, 3, false),
        (None, None, 3, false),
    ] {
        let mut stats = Map::new();
        stats.insert("segment_count", Value::Int(segments));
        let mut meta = Map::new();
        meta.insert("source", Value::from(source));
        meta.insert("title", Value::from(title));
        meta.insert("stats", Value::Object(stats));
        // when / then
        assert_eq!(storage.shared.wants_generated_title(&Value::Object(meta)), Ok(expected), "{source:?} {title:?}");
    }
}

#[test]
fn save_json_over_an_existing_file_when_written_then_new_content_kept_format_and_no_temp() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("meta.json");
    fs::write(&target, "{\"old\": true}").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let data = pyjson::loads(r#"{"new": "данные", "list": [1, {"a": null}], "empty": {}}"#).unwrap();
    // when
    save_json(&target, &data).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "{\n  \"new\": \"данные\",\n  \"list\": [\n    1,\n    {\n      \"a\": null\n    }\n  ],\n  \"empty\": {}\n}"
    );
    assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn save_json_first_write_when_written_then_created_with_the_default_mode() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (target, reference) = (dir.path().join("meta.json"), dir.path().join("reference"));
    fs::write(&reference, "").unwrap();
    // when
    save_json(&target, &Value::Object(Map::new())).unwrap();
    // then
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&target), mode(&reference));
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
}

#[test]
fn save_json_into_a_missing_directory_when_written_then_fails_without_a_temp() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("missing/meta.json");
    // when
    let result = save_json(&target, &Value::Null);
    // then
    assert!(matches!(result, Err(StorageError::Os(_))));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn concurrent_writers_and_a_reader_when_racing_then_every_read_is_a_whole_document() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let target = Arc::new(dir.path().join("meta.json"));
    save_json(&target, &pyjson::loads(r#"{"n": 0}"#).unwrap()).unwrap();
    // when
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let target = Arc::clone(&target);
            std::thread::spawn(move || {
                for i in 0..50 {
                    let doc = pyjson::loads(&format!(r#"{{"n": {i}, "writer": {w}, "pad": "{}"}}"#, "x".repeat(4096)))
                        .unwrap();
                    save_json(&target, &doc).unwrap();
                }
            })
        })
        .collect();
    let mut reads = 0;
    while writers.iter().any(|w| !w.is_finished()) {
        let text = fs::read_to_string(&*target).unwrap();
        assert!(pyjson::loads(&text).is_ok(), "torn read: {text:.40}");
        reads += 1;
    }
    for w in writers {
        w.join().unwrap();
    }
    // then
    assert!(reads > 0);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn delayed_audio_when_saved_then_the_row_keeps_the_capture_time_and_meta_the_save_time() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    storage.start_session(&ktalk()).unwrap();
    // when
    #[allow(clippy::cast_precision_loss)]
    let captured = (NOW - 90_000_000) as f64 / 1e6;
    storage.append_segment(&NewSegment { captured_at: Some(captured), ..said("Alice", "late") }).unwrap();
    // then
    assert_eq!(field(&rows(&storage)[0], &["ts"]), &Value::from("2026-07-04T09:58:30+00:00"));
    assert_eq!(field(&current_meta(&storage), &["updated_at"]), &Value::from("2026-07-04T10:00:00+00:00"));
}

fn wav(path: &Path, age_hours: f64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "RIFF").unwrap();
    #[allow(clippy::cast_precision_loss)]
    let seconds = NOW as f64 / 1e6 - age_hours * 3600.0;
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(seconds);
    fs::File::options().write(true).open(path).unwrap().set_times(fs::FileTimes::new().set_modified(at)).unwrap();
}

#[test]
fn audio_older_than_the_retention_when_cleaned_then_deleted_and_meta_nulled_fresh_audio_kept() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let base = storage.base_path().to_path_buf();
    fs::create_dir_all(base.join("m1")).unwrap();
    save_json(
        &base.join("m1/meta.json"),
        &pyjson::loads(r#"{"audio_mic_path": "x", "audio_system_path": "y"}"#).unwrap(),
    )
    .unwrap();
    wav(&base.join("m1/audio_mic.wav"), 25.0);
    wav(&base.join("m1/audio_system.wav"), 1.0);
    wav(&base.join("m2/audio_mic.wav"), 30.0);
    wav(&base.join("m3/audio_system.wav"), 2.0);
    // when
    let deleted = storage.cleanup_old_audio(24.0).unwrap();
    // then
    assert_eq!(deleted, 2);
    assert!(!base.join("m1/audio_mic.wav").exists() && base.join("m1/audio_system.wav").exists());
    assert!(!base.join("m2/audio_mic.wav").exists() && base.join("m3/audio_system.wav").exists());
    let meta = read_json(&base.join("m1/meta.json")).unwrap();
    assert_eq!(field(&meta, &["audio_mic_path"]), &Value::Null);
    assert_eq!(field(&meta, &["audio_system_path"]), &Value::from("y"));
    assert_eq!(field(&meta, &["audio_deleted_at"]), &Value::from("2026-07-04T10:00:00+00:00"));
    // and a shorter retention reaches the fresher file
    assert_eq!(storage.cleanup_old_audio(1.5).unwrap(), 1);
}

#[test]
fn a_directory_without_audio_when_cleaned_then_its_meta_is_not_read() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    fs::create_dir_all(storage.base_path().join("m")).unwrap();
    fs::write(storage.base_path().join("m/meta.json"), "not json").unwrap();
    // when / then
    assert_eq!(storage.cleanup_old_audio(24.0), Ok(0));
}

#[test]
fn no_current_meeting_when_meta_loaded_then_type_error_and_after_start_the_orphan_meta() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    // when / then
    assert!(matches!(storage.load_meta(), Err(StorageError::Type(_))));
    storage.start_session(&SessionMode { ktalk: true, ..SessionMode::default() }).unwrap();
    let meta = storage.load_meta().unwrap();
    assert_eq!(field(&meta, &["source"]), &Value::from("orphan"));
    assert_eq!(meta, read_json(&storage.current_meeting_path().unwrap().join("meta.json")).unwrap());
}

const STORED_META: &str = "{\n  \"id\": \"m1\",\n  \"title\": \"Старое\",\n  \"created_at\": \"2026-07-04T09:00:00+00:00\",\n  \"ended_at\": \"2026-07-04T09:30:00+00:00\",\n  \"stats\": {\n    \"segment_count\": 3,\n    \"duration_seconds\": 1800\n  },\n  \"participants\": {\n    \"a\": {\n      \"name\": \"Алиса\",\n      \"is_local\": true\n    }\n  },\n  \"updated_at\": \"2026-07-04T09:30:00+00:00\"\n}";
const STORED_INDEX: &str = "{\n  \"meetings\": [\n    {\n      \"id\": \"m2\",\n      \"title\": \"Другая\",\n      \"segment_count\": 1\n    },\n    {\n      \"id\": \"m1\",\n      \"title\": \"Старое\",\n      \"segment_count\": 3\n    }\n  ]\n}";

fn title_fields() -> Map {
    let mut fields = Map::new();
    fields.insert("title", Value::from("Обзор дизайна"));
    fields.insert("title_source", Value::from("claude"));
    fields.insert("title_model", Value::from("opus"));
    fields.insert("title_settings_revision", Value::from("r1"));
    fields
}

fn stored_meeting(storage: &MeetingStorage, meta: &str) -> PathBuf {
    let meeting = storage.base_path().join("m1");
    fs::create_dir_all(&meeting).unwrap();
    fs::write(meeting.join("meta.json"), meta).unwrap();
    fs::write(storage.base_path().join("index.json"), STORED_INDEX).unwrap();
    meeting
}

#[test]
fn an_ended_meeting_when_its_title_stored_then_meta_and_index_hold_the_bytes_python_wrote() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let meeting = stored_meeting(&storage, STORED_META);
    // when
    storage.store_title("m1", &title_fields()).unwrap();
    // then: json.dumps(meta, ensure_ascii=False, indent=2), updated_at as datetime.now()
    assert_eq!(
        fs::read_to_string(meeting.join("meta.json")).unwrap(),
        "{\n  \"id\": \"m1\",\n  \"title\": \"Обзор дизайна\",\n  \"created_at\": \"2026-07-04T09:00:00+00:00\",\n  \
         \"ended_at\": \"2026-07-04T09:30:00+00:00\",\n  \"stats\": {\n    \"segment_count\": 3,\n    \
         \"duration_seconds\": 1800\n  },\n  \"participants\": {\n    \"a\": {\n      \"name\": \"Алиса\",\n      \
         \"is_local\": true\n    }\n  },\n  \"updated_at\": \"2026-07-04T10:00:00\",\n  \"title_source\": \"claude\",\n  \
         \"title_model\": \"opus\",\n  \"title_settings_revision\": \"r1\"\n}"
    );
    assert_eq!(
        fs::read_to_string(storage.base_path().join("index.json")).unwrap(),
        "{\n  \"meetings\": [\n    {\n      \"id\": \"m2\",\n      \"title\": \"Другая\",\n      \"segment_count\": 1\n    \
         },\n    {\n      \"id\": \"m1\",\n      \"title\": \"Обзор дизайна\",\n      \"segment_count\": 3\n    }\n  ]\n}"
    );
}

#[test]
fn a_stored_title_when_written_then_the_old_files_are_replaced_by_rename_not_rewritten_in_place() {
    // given: a second name for each file keeps the inode a reader may hold open
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let meeting = stored_meeting(&storage, STORED_META);
    fs::hard_link(meeting.join("meta.json"), dir.path().join("meta.before")).unwrap();
    fs::hard_link(storage.base_path().join("index.json"), dir.path().join("index.before")).unwrap();
    // when
    storage.store_title("m1", &title_fields()).unwrap();
    // then: the old inodes were never truncated, and no temp file is left
    assert_eq!(fs::read_to_string(dir.path().join("meta.before")).unwrap(), STORED_META);
    assert_eq!(fs::read_to_string(dir.path().join("index.before")).unwrap(), STORED_INDEX);
    assert_ne!(fs::read_to_string(meeting.join("meta.json")).unwrap(), STORED_META);
    assert_eq!(fs::read_dir(&meeting).unwrap().count(), 1);
    assert_eq!(fs::read_dir(storage.base_path()).unwrap().count(), 2);
}

#[test]
fn a_meeting_not_ended_or_missing_when_its_title_stored_then_the_index_stays_and_nothing_is_created() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let open = STORED_META.replace("\"ended_at\": \"2026-07-04T09:30:00+00:00\"", "\"ended_at\": null");
    let meeting = stored_meeting(&storage, &open);
    // when
    storage.store_title("m1", &title_fields()).unwrap();
    storage.store_title("absent", &title_fields()).unwrap();
    // then
    assert_eq!(field(&read_json(&meeting.join("meta.json")).unwrap(), &["title"]), &Value::from("Обзор дизайна"));
    assert_eq!(fs::read_to_string(storage.base_path().join("index.json")).unwrap(), STORED_INDEX);
    assert!(!storage.base_path().join("absent").exists());
}

#[test]
fn local_participants_without_a_name_when_listed_then_the_fallback_asked_for_names_them() {
    // given
    let meta = pyjson::loads(
        r#"{"participants": {"a": {"name": "Alice", "is_local": true}, "b": {"is_local": true}, "c": {"name": "Bob"}}}"#,
    )
    .unwrap();
    // when / then
    assert_eq!(local_participant_names(&meta, UnnamedLocal::Empty), Ok(vec![Value::from("Alice"), Value::from("")]));
    assert_eq!(
        local_participant_names(&meta, UnnamedLocal::ParticipantId),
        Ok(vec![Value::from("Alice"), Value::from("b")])
    );
    assert_eq!(local_participant_names(&pyjson::loads("{}").unwrap(), UnnamedLocal::Empty), Ok(Vec::new()));
    assert!(matches!(
        local_participant_names(&pyjson::loads(r#"{"participants": []}"#).unwrap(), UnnamedLocal::Empty),
        Err(StorageError::Attribute(_))
    ));
}

/// What a storage printed, by stream.
type Lines = Arc<Mutex<Vec<(Stream, String)>>>;

/// A storage at the test clock with the alias `old-meet` → `jitsi`, meeting ids ending
/// `00000001`, `00000002`, … and its printed lines kept.
fn storage_with(dir: &Path, configure: impl FnOnce(&mut StorageOptions)) -> (MeetingStorage, Lines) {
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let mut options = StorageOptions::new(&dir.join("home"));
    options.clock = clock;
    options.external_index_cache_dir = dir.join("cache");
    options.source_aliases = vec![("old-meet".into(), "jitsi".into())];
    let count = Arc::new(AtomicI64::new(0));
    options.meeting_suffix = Arc::new(move || format!("{:08x}", count.fetch_add(1, Ordering::SeqCst) + 1));
    let lines: Lines = Arc::default();
    let sink = Arc::clone(&lines);
    options.log = Arc::new(move |stream, line| sink.lock().unwrap().push((stream, line.to_owned())));
    configure(&mut options);
    (MeetingStorage::new(&dir.join("meetings"), options).unwrap(), lines)
}

fn put(root: &Path, id: &str, meta: &str) {
    fs::create_dir_all(root.join(id)).unwrap();
    fs::write(root.join(id).join("meta.json"), meta).unwrap();
}

fn printed(lines: &Lines) -> Vec<(Stream, String)> {
    let mut printed = lines.lock().unwrap().clone();
    printed.sort_by(|a, b| a.1.cmp(&b.1));
    printed
}

/// `json.dumps(json.loads(compact), indent=2)`: the bytes `save_json` writes.
fn pretty(compact: &str) -> String {
    pyjson::dumps(&pyjson::loads(compact).unwrap(), Some(2))
}

mod editing;
mod joining;
mod lifecycle;
mod listing;
mod records;
