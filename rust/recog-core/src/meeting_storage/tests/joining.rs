//! Joining and leaving conferences: which meeting a join opens, reopens or retitles,
//! the same-day lookup behind a reopen, and the titles of ended meetings.

use super::*;

fn clocked(dir: &Path) -> (MeetingStorage, Lines, Arc<TestClock>) {
    let clock = Arc::new(TestClock(AtomicI64::new(NOW)));
    let shared = Arc::clone(&clock);
    let (storage, lines) = storage_with(dir, move |o| o.clock = shared);
    (storage, lines, clock)
}

fn meta_of(storage: &MeetingStorage, id: &str) -> Value {
    read_json(&storage.base_path().join(id).join("meta.json")).unwrap()
}

fn stdout(lines: &Lines) -> Vec<String> {
    printed(lines).into_iter().filter(|(s, _)| *s == Stream::Stdout).map(|(_, l)| l).collect()
}

const ID1: &str = "2026-07-04_100000_00000001";
const ID2: &str = "2026-07-04_100000_00000002";

#[test]
fn no_current_meeting_when_a_conference_is_joined_then_a_meeting_of_its_platform_is_created_with_its_title() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    // when
    let id = storage.on_meeting_join("https://k.example.org/room?x=1", Some("Daily"), "ktalk").unwrap();
    // then
    assert_eq!(id.as_deref(), Some(ID1));
    assert_eq!(
        fs::read_to_string(storage.base_path().join(ID1).join("meta.json")).unwrap(),
        pretty(
            r#"{"id": "2026-07-04_100000_00000001", "version": 1, "created_at": "2026-07-04T10:00:00+00:00",
            "ended_at": null, "updated_at": "2026-07-04T10:00:00+00:00", "source": "ktalk", "end_reason": null,
            "mode": {}, "chunk_duration_ms": 3000, "conference": {"live_url": "https://k.example.org/room?x=1",
            "live_id": "room", "host": "k.example.org"}, "title": "Daily", "title_source": "ktalk",
            "prev_meeting_id": null, "next_meeting_id": null, "participants": {},
            "stats": {"segment_count": 0, "duration_seconds": 0, "words_count": 0}}"#
        )
    );
    assert_eq!(fs::read_to_string(storage.base_path().join(ID1).join("transcript.jsonl")).unwrap(), "");
}

#[test]
fn the_ktalk_alias_when_joined_then_the_meeting_is_a_ktalk_one_and_a_later_title_does_not_replace_the_first() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    // when
    storage.on_ktalk_join("https://k.example.org/room", Some("First")).unwrap();
    storage.on_ktalk_join("https://k.example.org/room", Some("Second")).unwrap();
    // then
    let meta = current_meta(&storage);
    assert_eq!((field(&meta, &["source"]), field(&meta, &["title"])), (&Value::from("ktalk"), &Value::from("First")));
    assert_eq!(storage.current_meeting_id().as_deref(), Some(ID1));
}

#[test]
fn an_orphan_with_speech_when_a_conference_is_joined_then_the_orphan_ends_and_a_new_meeting_opens() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&said("Alice", "before the call")).unwrap();
    // when
    let joined = storage.on_meeting_join("https://k.example.org/room", None, "ktalk").unwrap();
    storage.drain_titles();
    // then
    assert_eq!(joined.as_deref(), Some(ID2));
    let orphan = meta_of(&storage, ID1);
    assert_eq!(field(&orphan, &["end_reason"]), &Value::from("meeting_join"));
    assert_eq!(field(&orphan, &["next_meeting_id"]), &Value::Null);
    assert_eq!(field(&meta_of(&storage, ID2), &["prev_meeting_id"]), &Value::Null);
    assert_eq!(stdout(&lines), ["Warning: title_generator not available"]);
}

#[test]
fn an_empty_orphan_when_a_room_left_minutes_ago_is_joined_then_that_meeting_reopens_and_the_orphan_goes() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines, clock) = clocked(dir.path());
    storage.start_session(&ktalk()).unwrap();
    storage.on_meeting_join("https://k.example.org/room", Some("Team"), "ktalk").unwrap();
    storage.append_segment(&said("Alice", "hello")).unwrap();
    let orphan = storage.on_ktalk_leave("user_left").unwrap();
    let path = storage.base_path().join(ID1).join("meta.json");
    let mut meta = read_json(&path).unwrap();
    meta.as_object_mut().unwrap().remove("chunk_duration_ms");
    save_json(&path, &meta).unwrap();
    clock.0.fetch_add(2 * MINUTE, Ordering::SeqCst);
    // when
    let joined = storage.on_meeting_join("https://k.example.org/room", None, "ktalk").unwrap();
    // then
    assert_eq!(joined.as_deref(), Some(ID1));
    assert!(!storage.base_path().join(&orphan).exists());
    let meta = meta_of(&storage, ID1);
    assert_eq!((field(&meta, &["ended_at"]), field(&meta, &["end_reason"])), (&Value::Null, &Value::Null));
    assert_eq!(field(&meta, &["updated_at"]), &Value::from("2026-07-04T10:02:00+00:00"));
    assert_eq!(field(&meta, &["chunk_duration_ms"]), &Value::Int(3000));
    assert_eq!(field(&meta, &["stats", "segment_count"]), &Value::Int(3));
    let resumed = &rows(&storage)[2];
    assert_eq!(
        (field(resumed, &["id"]), field(resumed, &["event_type"])),
        (&Value::Int(3), &Value::from("session_resume"))
    );
    assert_eq!(fs::read_to_string(storage.base_path().join("index.json")).unwrap(), pretty(r#"{"meetings": []}"#));
    assert!(stdout(&lines).contains(&format!("Reopened same-day meeting: {ID1}")));
}

#[test]
fn a_conference_when_a_room_left_minutes_ago_is_joined_then_it_ends_as_changed_and_that_room_reopens() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines, clock) = clocked(dir.path());
    storage.start_session(&ktalk()).unwrap();
    storage.on_meeting_join("https://k.example.org/a", Some("A"), "ktalk").unwrap();
    storage.append_segment(&said("Alice", "in a")).unwrap();
    storage.on_ktalk_leave("user_left").unwrap();
    clock.0.fetch_add(MINUTE, Ordering::SeqCst);
    let b = storage.on_meeting_join("https://k.example.org/b", Some("B"), "ktalk").unwrap();
    storage.append_segment(&said("Bob", "in b")).unwrap();
    // when
    let joined = storage.on_meeting_join("https://k.example.org/a", None, "ktalk").unwrap();
    // then
    assert_eq!((b.as_deref(), joined.as_deref()), (Some(ID2), Some(ID1)));
    let left = meta_of(&storage, ID2);
    assert_eq!(
        (field(&left, &["end_reason"]), field(&left, &["title"])),
        (&Value::from("changed_meeting"), &Value::from("B"))
    );
    assert!(stdout(&lines).contains(&format!("Reopened same-day meeting: {ID1}")));
}

#[test]
fn the_same_room_on_another_platform_when_joined_then_a_new_meeting_and_an_alias_of_the_same_keeps_it() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    storage.on_meeting_join("https://j.example.org/room", None, "ktalk").unwrap();
    storage.append_segment(&said("Alice", "hello")).unwrap();
    // when
    let other = storage.on_meeting_join("https://j.example.org/room", None, "jitsi").unwrap();
    let alias = storage.on_meeting_join("https://j.example.org/room", None, "old-meet").unwrap();
    // then
    assert_eq!((other.as_deref(), alias.as_deref()), (Some(ID2), Some(ID2)));
    assert_eq!(field(&meta_of(&storage, ID1), &["end_reason"]), &Value::from("changed_meeting"));
    assert_eq!(field(&current_meta(&storage), &["source"]), &Value::from("jitsi"));
}

#[test]
fn a_conference_whose_stored_url_names_its_host_when_rejoined_then_the_meeting_stays() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    storage.on_meeting_join("https://h.example.org/room", None, "jitsi").unwrap();
    let path = storage.current_meeting_path().unwrap().join("meta.json");
    let mut meta = read_json(&path).unwrap();
    set(item_mut(&mut meta, "conference").unwrap(), "host", Value::Null).unwrap();
    save_json(&path, &meta).unwrap();
    // when
    let same = storage.on_meeting_join("https://h.example.org/room", None, "jitsi").unwrap();
    // then
    assert_eq!(same.as_deref(), Some(ID1));
    assert_eq!(field(&current_meta(&storage), &["conference", "host"]), &Value::from("h.example.org"));
}

fn ended(id: &str, source: &str, live_id: &str, host: &str, created: &str, ended: &str) -> String {
    format!(
        r#"{{"id": "{id}", "source": "{source}", "conference": {{"live_id": "{live_id}", "host": "{host}"}},
            "created_at": "{created}", "ended_at": "{ended}"}}"#
    )
}

#[test]
fn ended_meetings_when_a_room_is_looked_up_then_only_the_one_of_today_ended_minutes_ago_matches() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    let today = "2026-07-04T09:00:00+00:00";
    let recently = "2026-07-04T09:55:00+00:00";
    put(&base, "match", &ended("match", "old-meet", "room", "h.example.org", today, recently));
    put(&base, "open", r#"{"id": "open", "ended_at": null}"#);
    put(&base, "other-source", &ended("other-source", "ktalk", "room", "h.example.org", today, recently));
    put(&base, "other-room", &ended("other-room", "jitsi", "hall", "h.example.org", today, recently));
    put(&base, "other-host", &ended("other-host", "jitsi", "room", "g.example.org", today, recently));
    put(&base, "long-ago", &ended("long-ago", "jitsi", "room", "h.example.org", today, "2026-07-04T09:49:59+00:00"));
    put(
        &base,
        "yesterday",
        &ended("yesterday", "jitsi", "room", "h.example.org", "2026-07-03T23:00:00+00:00", recently),
    );
    put(&base, "broken", "{");
    put(
        &base,
        "noid",
        r#"{"source": "jitsi", "conference": {"live_id": "room", "host": "h.example.org"},
        "created_at": "2026-07-04T09:00:00+00:00", "ended_at": "2026-07-04T09:55:00+00:00"}"#,
    );
    put(
        &base,
        "badtime",
        r#"{"source": "jitsi", "conference": {"live_id": "room", "host": "h.example.org"},
        "ended_at": "late"}"#,
    );
    fs::create_dir_all(base.join("nometa")).unwrap();
    let find = |live_id: &str, platform: &str| storage.shared.find_same_day_meeting(live_id, platform, "h.example.org");
    // when / then
    assert_eq!(find("room", "jitsi"), Ok(Some("match".to_owned())));
    assert_eq!(find("", "jitsi"), Ok(None));
    assert_eq!(find("lobby", "jitsi"), Ok(None));
    fs::remove_dir_all(base.join("match")).unwrap();
    assert_eq!(find("room", "jitsi"), Ok(None));
}

#[test]
fn an_ended_meeting_of_the_wrong_shape_when_a_room_is_looked_up_then_the_error_is_raised() {
    for meta in [
        r#"{"source": "jitsi", "conference": {"live_id": "room", "host": "h"}, "ended_at": "2026-07-04T09:55:00"}"#,
        r#"{"source": "jitsi", "conference": {"live_id": "room", "live_url": 5}, "ended_at": "x"}"#,
        r#"{"source": "jitsi", "conference": [], "ended_at": "x"}"#,
    ] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, _) = storage_with(dir.path(), |_| {});
        put(storage.base_path(), "m", meta);
        // when
        let found = storage.shared.find_same_day_meeting("room", "jitsi", "h");
        // then
        assert!(matches!(found, Err(StorageError::Type(_) | StorageError::Attribute(_))), "{meta}: {found:?}");
    }
}

#[test]
fn a_meeting_when_reopened_directly_then_absent_is_false_and_a_broken_index_raises() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "m", r#"{"id": "m", "ended_at": "2026-07-04T09:00:00+00:00", "stats": {"segment_count": 0}}"#);
    let reopen = |id: &str| {
        let mut inner = storage.shared.lock();
        storage.shared.reopen_meeting(&mut inner, id)
    };
    // when / then
    assert_eq!(reopen("absent"), Ok(false));
    fs::write(base.join("index.json"), r#"{"meetings": 1}"#).unwrap();
    assert_eq!(reopen("m"), Err(StorageError::Type("'int' object is not iterable".into())));
    fs::remove_file(base.join("index.json")).unwrap();
    assert_eq!(reopen("m"), Ok(true));
    let rows = rows(&storage);
    assert_eq!((rows.len(), field(&rows[0], &["id"])), (1, &Value::Int(1)));
    assert_eq!(field(&meta_of(&storage, "m"), &["stats", "segment_count"]), &Value::Int(1));
    assert_eq!(field(&meta_of(&storage, "m"), &["chunk_duration_ms"]), &Value::Null);
}

#[test]
fn no_current_meeting_when_left_or_ended_then_an_orphan_opens_or_nothing_ends_and_the_empty_orphan_goes() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    // when / then
    assert_eq!(storage.end_session(), Ok(None));
    assert_eq!(storage.on_ktalk_leave("page_closed"), Ok(ID1.to_owned()));
    let meta = current_meta(&storage);
    assert_eq!((field(&meta, &["source"]), field(&meta, &["prev_meeting_id"])), (&Value::from("orphan"), &Value::Null));
    assert_eq!(storage.end_session(), Ok(Some(ID1.to_owned())));
    assert_eq!(storage.current_meeting_id(), None);
    assert!(!storage.base_path().join(ID1).exists());
}

#[test]
fn sources_when_asked_whether_they_are_conferences_then_platforms_and_their_aliases_are() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    // when / then
    for (source, conference) in
        [(Some("ktalk"), true), (Some("zoom"), true), (Some("old-meet"), true), (Some("orphan"), false), (None, false)]
    {
        assert_eq!(storage.is_conference_source(source), conference, "{source:?}");
    }
}

#[test]
fn system_audio_without_the_microphone_when_a_session_starts_then_the_meeting_is_diarized() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    // when
    storage.start_session(&SessionMode { system_audio: true, ..SessionMode::default() }).unwrap();
    // then
    assert_eq!(field(&current_meta(&storage), &["source"]), &Value::from("diarized"));
}

struct Recorder {
    answer: Result<Option<GeneratedTitle>, String>,
    asked: Mutex<Vec<TitleRequest>>,
}

impl TitleGenerator for Recorder {
    fn generate(&self, request: &TitleRequest) -> Result<Option<GeneratedTitle>, String> {
        self.asked.lock().unwrap().push(request.clone());
        self.answer.clone()
    }
}

fn titled(title: &str) -> Result<Option<GeneratedTitle>, String> {
    Ok(Some(GeneratedTitle {
        title: title.into(),
        provider: "claude".into(),
        model: Value::from("m1"),
        settings_revision: Value::Int(7),
    }))
}

fn with_titles(dir: &Path, answer: Result<Option<GeneratedTitle>, String>) -> (MeetingStorage, Lines, Arc<Recorder>) {
    let recorder = Arc::new(Recorder { answer, asked: Mutex::new(Vec::new()) });
    let hook = Arc::clone(&recorder);
    let (storage, lines) = storage_with(dir, move |o| o.title_generator = Some(hook));
    (storage, lines, recorder)
}

fn a_finished_call(storage: &MeetingStorage) -> String {
    storage.start_session(&ktalk()).unwrap();
    let id = storage.on_meeting_join("https://zoom.us/j/1", None, "zoom").unwrap().unwrap();
    storage
        .update_participants(
            &pyjson::loads(r#"{"me": {"name": "Me", "is_local": true}, "b": {"name": "Bob"}}"#)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    storage.append_segment(&NewSegment { is_local: true, speaker_id: "me".into(), ..said("Me", "hi") }).unwrap();
    storage.append_segment(&NewSegment { speaker_id: "b".into(), ..said("Bob", "hello") }).unwrap();
    storage.on_ktalk_leave("call_ended").unwrap();
    storage.drain_titles();
    id
}

#[test]
fn an_ended_call_when_titled_then_the_hook_gets_the_transcript_and_names_and_meta_and_index_the_title() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines, recorder) = with_titles(dir.path(), titled("Sync"));
    // when
    let id = a_finished_call(&storage);
    // then
    let asked = recorder.asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec![TitleRequest {
            data_dir: dir.path().to_path_buf(),
            meeting_path: storage.base_path().join(&id),
            transcript: "[10:00:00] Me: hi\n[10:00:00] Bob: hello".into(),
            user_name: "Me".into(),
            participants: "Me, Bob".into(),
            meeting_title: String::new(),
        }]
    );
    let meta = meta_of(&storage, &id);
    assert_eq!(
        [&["title"][..], &["title_source"], &["title_model"], &["title_settings_revision"]]
            .map(|k| field(&meta, k).clone()),
        [Value::from("Sync"), Value::from("claude"), Value::from("m1"), Value::Int(7)]
    );
    let index = read_json(&storage.base_path().join("index.json")).unwrap();
    let Value::Array(rows) = field(&index, &["meetings"]) else { panic!("{index:?}") };
    assert_eq!((rows.len(), field(&rows[0], &["title"])), (1, &Value::from("Sync")));
    assert_eq!(stdout(&lines), ["Generated title: Sync"]);
}

#[test]
fn a_title_hook_that_answers_nothing_empty_or_fails_when_asked_then_no_title_and_the_failure_printed() {
    for (answer, printed_lines) in [
        (Ok(None), vec![]),
        (titled(""), vec![]),
        (Err("boom".to_owned()), vec!["Warning: Title generation failed: boom".to_owned()]),
    ] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, lines, _) = with_titles(dir.path(), answer);
        // when
        let id = a_finished_call(&storage);
        // then
        assert_eq!(field(&meta_of(&storage, &id), &["title"]), &Value::Null);
        assert_eq!(stdout(&lines), printed_lines);
    }
}

#[test]
fn a_call_with_no_speech_when_left_then_it_is_deleted_and_with_only_system_events_the_hook_is_not_asked() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _, recorder) = with_titles(dir.path(), titled("Never"));
    storage.start_session(&ktalk()).unwrap();
    let id = storage.on_meeting_join("https://zoom.us/j/1", None, "zoom").unwrap().unwrap();
    // when: the meta saved at the end predates its pause event, so it counts no segment
    storage.on_ktalk_leave("call_ended").unwrap();
    storage.drain_titles();
    // then
    assert!(!storage.base_path().join(&id).exists());
    // given a meeting whose transcript holds only system events
    let base = storage.base_path().to_path_buf();
    put(&base, "m", r#"{"ended_at": "e"}"#);
    fs::write(base.join("m/transcript.jsonl"), "{\"speaker_id\": \"__system__\", \"text\": \"paused\"}\n").unwrap();
    // when
    storage.shared.finish_meeting_title(&base.join("m"));
    // then
    assert!(recorder.asked.lock().unwrap().is_empty());
    assert_eq!(fs::read_to_string(base.join("m/meta.json")).unwrap(), r#"{"ended_at": "e"}"#);
}

#[test]
fn a_meeting_reopened_or_titled_meanwhile_when_its_title_arrives_then_it_is_dropped() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _, _) = with_titles(dir.path(), titled("Late"));
    let base = storage.base_path().to_path_buf();
    let transcript = "{\"speaker\": \"A\", \"text\": \"x\", \"ts_display\": \"10:00:00\"}\n";
    for (id, meta) in [("open", r#"{"ended_at": null}"#), ("titled", r#"{"ended_at": "e", "title": "Own"}"#)] {
        put(&base, id, meta);
        fs::write(base.join(id).join("transcript.jsonl"), transcript).unwrap();
        // when
        storage.shared.finish_meeting_title(&base.join(id));
        // then
        assert_eq!(fs::read_to_string(base.join(id).join("meta.json")).unwrap(), meta);
    }
    assert!(!base.join("index.json").exists());
}

#[test]
fn an_index_that_cannot_be_read_when_a_title_is_persisted_then_warned_on_stderr() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines, _) = with_titles(dir.path(), titled("Late"));
    let base = storage.base_path().to_path_buf();
    put(&base, "m", r#"{"id": "m", "ended_at": "e"}"#);
    fs::write(base.join("m/transcript.jsonl"), "{\"speaker\": \"A\", \"text\": \"x\"}\n").unwrap();
    fs::write(base.join("index.json"), "{").unwrap();
    // when
    storage.shared.finish_meeting_title(&base.join("m"));
    // then
    assert_eq!(
        printed(&lines),
        vec![
            (Stream::Stdout, "Generated title: Late".to_owned()),
            (
                Stream::Stderr,
                "Warning: Title persistence failed: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
                    .to_owned()
            ),
        ]
    );
    assert_eq!(field(&meta_of(&storage, "m"), &["title"]), &Value::from("Late"));
}

#[test]
fn a_transcript_when_rendered_for_a_title_then_system_events_blank_and_broken_lines_are_left_out() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let meeting = dir.path().join("m");
    fs::create_dir_all(&meeting).unwrap();
    // when / then
    assert_eq!(transcript_as_text(&meeting), Ok(String::new()));
    fs::write(
        meeting.join("transcript.jsonl"),
        "{\"ts_display\": \"10:00:00\", \"speaker\": \"A\", \"text\": \"hi\"}\n\ngarbage\n\
         {\"speaker_id\": \"__system__\", \"text\": \"paused\"}\n{\"text\": 5}\n",
    )
    .unwrap();
    assert_eq!(transcript_as_text(&meeting), Ok("[10:00:00] A: hi\n[] Unknown: 5".to_owned()));
    fs::write(meeting.join("transcript.jsonl"), "[1]\n").unwrap();
    assert_eq!(
        transcript_as_text(&meeting),
        Err(StorageError::Attribute("'list' object has no attribute 'get'".into()))
    );
}
