//! Sessions left open by an earlier run, the silence split, the participants a
//! conference reports, and the host a stored conference names.

use super::*;

fn pretty(compact: &str) -> String {
    pyjson::dumps(&pyjson::loads(compact).unwrap(), Some(2))
}

const STALE_WITH_UPDATE: &str = r#"{"id": "s1", "created_at": "2026-07-04T09:00:00+00:00", "ended_at": null,
    "updated_at": "2026-07-04T09:20:00+00:00", "source": "ktalk", "title": "Daily", "end_reason": null,
    "conference": {"live_url": "https://k.example.org/r"}, "stats": {"segment_count": 2, "duration_seconds": 0,
    "words_count": 3}, "participants": {"p": {"name": "Ann"}}}"#;
const STALE_WITHOUT_UPDATE: &str = r#"{"id": "s2", "created_at": "2026-07-04T09:30:00+00:00", "ended_at": null,
    "title": null, "source": "orphan", "end_reason": null, "stats": {"segment_count": 1, "duration_seconds": 0},
    "participants": {}}"#;
const STALE_EMPTY: &str = r#"{"id": "s3", "created_at": "2026-07-04T09:40:00+00:00", "title": null, "source": "orphan",
    "end_reason": null, "stats": {"segment_count": 0}, "participants": {}}"#;
const ENDED: &str =
    r#"{"id": "done", "created_at": "2026-07-04T08:00:00+00:00", "ended_at": "2026-07-04T08:30:00+00:00"}"#;

#[test]
fn sessions_left_open_when_a_session_starts_then_closed_as_stale_indexed_and_the_empty_one_deleted() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "s1", STALE_WITH_UPDATE);
    put(&base, "s2", STALE_WITHOUT_UPDATE);
    put(&base, "s3", STALE_EMPTY);
    put(&base, "done", ENDED);
    fs::create_dir_all(base.join("nometa")).unwrap();
    // when
    storage.start_session(&SessionMode::default()).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(base.join("s1/meta.json")).unwrap(),
        pretty(
            &STALE_WITH_UPDATE
                .replace("\"ended_at\": null", "\"ended_at\": \"2026-07-04T09:20:00+00:00\"")
                .replace("\"end_reason\": null", "\"end_reason\": \"stale_session\"")
                .replace("\"duration_seconds\": 0", "\"duration_seconds\": 1200")
        )
    );
    assert_eq!(
        fs::read_to_string(base.join("s2/meta.json")).unwrap(),
        pretty(
            &STALE_WITHOUT_UPDATE
                .replace("\"ended_at\": null", "\"ended_at\": \"2026-07-04T10:00:00+00:00\"")
                .replace("\"end_reason\": null", "\"end_reason\": \"stale_session\"")
                .replace("\"duration_seconds\": 0", "\"duration_seconds\": 1800")
        )
    );
    assert!(!base.join("s3").exists());
    assert_eq!(fs::read_to_string(base.join("done/meta.json")).unwrap(), ENDED);
    assert_eq!(
        fs::read_to_string(base.join("index.json")).unwrap(),
        pretty(
            r#"{"meetings": [{"id": "s2", "created_at": "2026-07-04T09:30:00+00:00",
            "ended_at": "2026-07-04T10:00:00+00:00", "duration_seconds": 1800, "title": null, "source": "orphan",
            "end_reason": "stale_session", "ktalk_live_url": null, "segment_count": 1, "participants": []},
            {"id": "s1", "created_at": "2026-07-04T09:00:00+00:00", "ended_at": "2026-07-04T09:20:00+00:00",
            "duration_seconds": 1200, "title": "Daily", "source": "ktalk", "end_reason": "stale_session",
            "ktalk_live_url": "https://k.example.org/r", "segment_count": 2, "participants": ["Ann"]}]}"#
        )
    );
    assert_eq!(printed(&lines), vec![(Stream::Stdout, "Closed 3 stale session(s) from previous runs".to_owned())]);
}

#[test]
fn stale_sessions_that_cannot_be_read_when_closed_then_warned_and_the_others_still_closed() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "s1", STALE_WITH_UPDATE);
    put(&base, "broken", "{");
    put(&base, "nocreated", r#"{"ended_at": null}"#);
    put(&base, "badtime", r#"{"created_at": "soon"}"#);
    // when
    storage.start_session(&SessionMode::default()).unwrap();
    // then
    assert_eq!(
        printed(&lines),
        vec![
            (Stream::Stdout, "Closed 1 stale session(s) from previous runs".to_owned()),
            (
                Stream::Stdout,
                "Warning: Could not close stale session badtime: Invalid isoformat string: 'soon'".to_owned()
            ),
            (
                Stream::Stdout,
                "Warning: Could not close stale session broken: Expecting property name enclosed in double quotes: \
                 line 1 column 2 (char 1)"
                    .to_owned()
            ),
            (Stream::Stdout, "Warning: Could not close stale session nocreated: 'created_at'".to_owned()),
        ]
    );
    assert_eq!(fs::read_to_string(base.join("badtime/meta.json")).unwrap(), r#"{"created_at": "soon"}"#);
}

#[test]
fn a_stale_session_of_another_kind_of_wrong_when_closed_then_the_session_does_not_start() {
    for (meta, error) in [
        (
            r#"{"created_at": "2026-07-04T09:00:00"}"#,
            StorageError::Type("can't subtract offset-naive and offset-aware datetimes".into()),
        ),
        (r#"{"created_at": 5}"#, StorageError::Type("fromisoformat: argument must be str, not int".into())),
        (
            r#"{"created_at": "2026-07-04T09:00:00+00:00", "stats": []}"#,
            StorageError::Type("list indices must be integers or slices, not str".into()),
        ),
        (r#"[]"#, StorageError::Attribute("'list' object has no attribute 'get'".into())),
    ] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, _) = storage_with(dir.path(), |_| {});
        put(storage.base_path(), "s", meta);
        // when
        let started = storage.start_session(&SessionMode::default());
        // then
        assert_eq!(started, Err(error), "{meta}");
        assert_eq!(storage.current_meeting_id(), None);
    }
}

#[test]
fn an_orphan_with_speech_when_silence_times_out_then_it_ends_and_a_linked_orphan_opens() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let first = storage.start_session(&ktalk()).unwrap();
    assert_eq!(storage.split_on_silence_timeout(), Ok(None));
    storage.append_segment(&said("Alice", "hello")).unwrap();
    // when
    let second = storage.split_on_silence_timeout().unwrap();
    // then
    assert_eq!(second.as_deref(), Some("2026-07-04_100000_00000002"));
    let old = read_json(&storage.base_path().join(&first).join("meta.json")).unwrap();
    assert_eq!(field(&old, &["end_reason"]), &Value::from("silence_timeout"));
    assert_eq!(field(&old, &["next_meeting_id"]), &Value::from(second.as_deref()));
    let new = current_meta(&storage);
    assert_eq!(field(&new, &["prev_meeting_id"]), &Value::from(first.as_str()));
    assert_eq!(field(&new, &["source"]), &Value::from("orphan"));
}

#[test]
fn no_meeting_or_a_conference_when_silence_times_out_then_nothing_splits() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    // when / then
    assert_eq!(storage.split_on_silence_timeout(), Ok(None));
    storage.start_session(&ktalk()).unwrap();
    let joined = storage.on_meeting_join("https://k.example.org/room", None, "ktalk").unwrap();
    storage.append_segment(&said("Alice", "hello")).unwrap();
    assert_eq!(storage.split_on_silence_timeout(), Ok(None));
    assert_eq!(storage.current_meeting_id(), joined);
}

fn participants(json: &str) -> Map {
    pyjson::loads(json).unwrap().as_object().unwrap().clone()
}

#[test]
fn participants_of_a_conference_when_reported_then_added_renamed_and_the_local_user_merged() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    assert_eq!(storage.update_participants(&participants(r#"{"x": {"name": "Nobody"}}"#)), Ok(()));
    storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&NewSegment { is_local: true, ..said("Me", "hi") }).unwrap();
    storage.append_segment(&said("Bob", "hello")).unwrap();
    // when
    storage
        .update_participants(&participants(
            r#"{"p1": {"name": "Ann"}, "p2": {}, "Bob": {"name": "Robert"}, "Me": {"name": ""},
                "conf-me": {"name": "Vyacheslav", "is_local": true}, "other-me": {"is_local": true}}"#,
        ))
        .unwrap();
    // then
    assert_eq!(
        pyjson::dumps(field(&current_meta(&storage), &["participants"]), None),
        "{\"Me\": {\"name\": \"Vyacheslav\", \"is_local\": true, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}, \
         \"Bob\": {\"name\": \"Robert\", \"is_local\": false, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}, \
         \"p1\": {\"name\": \"Ann\", \"is_local\": false, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}, \
         \"p2\": {\"name\": \"Participant_p2\", \"is_local\": false, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}}"
    );
}

#[test]
fn a_local_participant_when_none_is_known_then_it_is_added_and_its_own_id_renames_itself() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    // when
    storage.update_participants(&participants(r#"{"me": {"name": "Me", "is_local": true}}"#)).unwrap();
    storage.update_participants(&participants(r#"{"me": {"name": "Myself", "is_local": true}}"#)).unwrap();
    // then
    let meta = current_meta(&storage);
    assert_eq!(
        pyjson::dumps(field(&meta, &["participants"]), None),
        "{\"me\": {\"name\": \"Myself\", \"is_local\": true, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}}"
    );
    assert_eq!(field(&meta, &["updated_at"]), &Value::from("2026-07-04T10:00:00+00:00"));
}

#[test]
fn stored_participants_of_the_wrong_shape_when_updated_then_the_error_is_raised() {
    for (stored, update, error) in [
        (
            "{\"participants\": []}",
            r#"{"a": {}}"#,
            StorageError::Type("list indices must be integers or slices, not str".into()),
        ),
        ("{}", r#"{"a": {}}"#, StorageError::Key("'participants'".into())),
        (
            "{\"participants\": {\"b\": {\"is_local\": true}}}",
            r#"{"a": []}"#,
            StorageError::Attribute("'list' object has no attribute 'get'".into()),
        ),
        (
            "{\"participants\": {\"b\": 1}}",
            r#"{"a": {"is_local": true}}"#,
            StorageError::Attribute("'int' object has no attribute 'get'".into()),
        ),
        (
            "{\"participants\": {\"a\": 1}}",
            r#"{"a": {"name": "A"}}"#,
            StorageError::Type("'int' object is not subscriptable".into()),
        ),
    ] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, _) = storage_with(dir.path(), |_| {});
        storage.start_session(&ktalk()).unwrap();
        fs::write(storage.current_meeting_path().unwrap().join("meta.json"), stored).unwrap();
        // when
        let result = storage.update_participants(&participants(update));
        // then
        assert_eq!(result, Err(error), "{stored} {update}");
    }
}

#[test]
fn a_stored_conference_when_its_host_is_asked_then_the_host_or_the_url_netloc() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let host = |json: &str| storage.shared.stored_host(&pyjson::loads(json).unwrap());
    // when / then
    assert_eq!(
        host(r#"{"host": "h.example.org", "live_url": "https://u.example.org/r"}"#),
        Ok(Value::from("h.example.org"))
    );
    assert_eq!(
        host(r#"{"host": "", "live_url": "https://u.example.org:8443/r"}"#),
        Ok(Value::from("u.example.org:8443"))
    );
    assert_eq!(host(r#"{"live_url": ""}"#), Ok(Value::from("")));
    assert_eq!(host(r#"{"live_url": null}"#), Ok(Value::from("")));
    assert_eq!(host("{}"), Ok(Value::from("")));
    assert_eq!(
        host(r#"{"live_url": 5}"#),
        Err(StorageError::Type("Cannot mix str and non-str arguments (int)".into()))
    );
    assert_eq!(host(r#"{"live_url": "http://[::1"}"#), Err(StorageError::Value("Invalid IPv6 URL".into())));
    assert!(matches!(host("[]"), Err(StorageError::Attribute(_))));
}
