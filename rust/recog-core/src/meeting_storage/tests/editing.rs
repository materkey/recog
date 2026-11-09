//! Changes to a meeting's records: segments appended and rewritten, the title, the
//! index entry, deletion with its chain, stored titles and old audio.

use super::*;

fn meta_at(storage: &MeetingStorage, id: &str) -> Value {
    read_json(&storage.base_path().join(id).join("meta.json")).unwrap()
}

fn participants_of(storage: &MeetingStorage) -> String {
    pyjson::dumps(field(&current_meta(storage), &["participants"]), None)
}

#[test]
fn no_current_meeting_when_a_segment_arrives_then_an_orphan_opens_for_it() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    // when
    let id = storage.append_segment(&said("Alice", "a  b\tc")).unwrap();
    // then
    assert_eq!(id, 1);
    assert_eq!(storage.current_meeting_id().as_deref(), Some("2026-07-04_100000_00000001"));
    let meta = current_meta(&storage);
    assert_eq!(field(&meta, &["source"]), &Value::from("orphan"));
    assert_eq!(
        (field(&meta, &["stats", "segment_count"]), field(&meta, &["stats", "words_count"])),
        (&Value::Int(1), &Value::Int(3))
    );
}

#[test]
fn speakers_when_their_segments_arrive_then_renamed_by_id_and_the_local_user_kept_as_one() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&NewSegment { is_local: true, speaker_id: "mic".into(), ..said("Me", "one") }).unwrap();
    storage.append_segment(&NewSegment { speaker_id: "b".into(), ..said("Bob", "two") }).unwrap();
    // when
    storage
        .append_segment(&NewSegment { is_local: true, speaker_id: "mic2".into(), ..said("Vyacheslav", "three") })
        .unwrap();
    storage.append_segment(&NewSegment { is_local: true, speaker_id: "mic3".into(), ..said("", "four") }).unwrap();
    storage.append_segment(&NewSegment { speaker_id: "b".into(), ..said("Robert", "five") }).unwrap();
    storage.append_segment(&NewSegment { speaker_id: "b".into(), ..said("Robert", "six") }).unwrap();
    // then
    assert_eq!(
        participants_of(&storage),
        "{\"mic\": {\"name\": \"Vyacheslav\", \"is_local\": true, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}, \
         \"b\": {\"name\": \"Robert\", \"is_local\": false, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}}"
    );
    assert_eq!(field(&current_meta(&storage), &["stats", "words_count"]), &Value::Int(6));
}

#[test]
fn a_local_participant_with_an_empty_id_when_another_local_speaks_then_a_second_one_is_added() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&NewSegment { is_local: true, speaker_id: String::new(), ..said("Me", "one") }).unwrap();
    // when
    storage.append_segment(&NewSegment { is_local: true, speaker_id: "mic".into(), ..said("Me", "two") }).unwrap();
    // then
    assert_eq!(
        participants_of(&storage),
        "{\"\": {\"name\": \"Me\", \"is_local\": true, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}, \
         \"mic\": {\"name\": \"Me\", \"is_local\": true, \"first_seen\": \"2026-07-04T10:00:00+00:00\"}}"
    );
}

#[test]
fn a_meta_of_the_wrong_shape_when_a_segment_arrives_then_the_error_is_raised_after_the_row_is_written() {
    for (meta, error) in [
        (
            r#"{"stats": {"words_count": "many"}}"#,
            StorageError::Type("unsupported operand type(s) for +=: 'str' and 'int'".into()),
        ),
        (r#"{"stats": {"words_count": 1}}"#, StorageError::Key("'participants'".into())),
        (
            r#"{"stats": {"words_count": 1}, "participants": {"Alice": []}}"#,
            StorageError::Type("list indices must be integers or slices, not str".into()),
        ),
        (r#"{"stats": {"words_count": 1.5}, "participants": {"Alice": {}}}"#, StorageError::Key("'name'".into())),
    ] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, _) = storage_with(dir.path(), |_| {});
        storage.start_session(&ktalk()).unwrap();
        fs::write(storage.current_meeting_path().unwrap().join("meta.json"), meta).unwrap();
        // when
        let result = storage.append_segment(&said("Alice", "hi"));
        // then
        assert_eq!(result, Err(error), "{meta}");
        assert_eq!(rows(&storage).len(), 1);
    }
}

#[test]
fn updates_when_applied_to_a_pinned_meeting_then_its_rows_change_and_the_current_one_stays() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let first = storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&said("Alice", "one two three")).unwrap();
    storage.split_on_silence_timeout().unwrap();
    storage.append_segment(&said("Bob", "current")).unwrap();
    let pinned = storage.base_path().join(&first);
    // when
    storage.update_segments(&[text_update(1, "ignored"), text_update(1, "one")], Some(&pinned)).unwrap();
    // then
    let old = read_rows(&pinned.join("transcript.jsonl")).unwrap();
    assert_eq!((field(&old[0], &["text"]), field(&old[0], &["refined"])), (&Value::from("one"), &Value::Bool(true)));
    assert_eq!(field(&meta_at(&storage, &first), &["stats", "words_count"]), &Value::Int(1));
    assert_eq!(field(&rows(&storage)[0], &["text"]), &Value::from("current"));
}

#[test]
fn a_transcript_with_blank_and_foreign_lines_when_rewritten_then_blanks_go_foreign_lines_stay_and_ids_match_by_value() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    let path = storage.current_meeting_path().unwrap();
    fs::write(path.join("transcript.jsonl"), "{\"id\": 1.0, \"text\": \"a b c\"}\n\nnot json\n{\"text\": \"x\"}\n")
        .unwrap();
    // when
    storage.update_segments(&[text_update(1, "z")], None).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(path.join("transcript.jsonl")).unwrap(),
        "{\"id\": 1.0, \"text\": \"z\", \"refined\": true}\nnot json\n{\"text\": \"x\"}\n"
    );
    assert_eq!(field(&current_meta(&storage), &["stats", "words_count"]), &Value::Int(0));
    assert!(!path.join("transcript.jsonl.tmp").exists());
}

#[test]
fn a_meeting_without_stats_or_meta_when_its_words_change_then_stats_are_created_or_nothing_is_written() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    for id in ["nostats", "nometa"] {
        fs::create_dir_all(base.join(id)).unwrap();
        fs::write(base.join(id).join("transcript.jsonl"), "{\"id\": 1, \"text\": \"a\"}\n").unwrap();
    }
    fs::write(base.join("nostats/meta.json"), "{\"id\": \"nostats\"}").unwrap();
    // when
    storage.update_segments(&[text_update(1, "a b")], Some(&base.join("nostats"))).unwrap();
    storage.update_segments(&[text_update(1, "a b")], Some(&base.join("nometa"))).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(base.join("nostats/meta.json")).unwrap(),
        pretty(r#"{"id": "nostats", "stats": {"words_count": 1}}"#)
    );
    assert!(!base.join("nometa/meta.json").exists());
    assert_eq!(field(&read_rows(&base.join("nometa/transcript.jsonl")).unwrap()[0], &["text"]), &Value::from("a b"));
}

#[test]
fn nothing_to_update_when_updates_are_applied_then_no_file_is_touched() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    // when / then: no meeting at all
    assert_eq!(storage.update_segments(&[text_update(1, "x")], None), Ok(()));
    // a meeting without a transcript
    fs::create_dir_all(base.join("m")).unwrap();
    assert_eq!(storage.update_segments(&[text_update(1, "x")], Some(&base.join("m"))), Ok(()));
    assert!(!base.join("m/transcript.jsonl").exists());
    // no updates
    fs::write(base.join("m/transcript.jsonl"), "{\"id\": 1}\n\n").unwrap();
    assert_eq!(storage.update_segments(&[], Some(&base.join("m"))), Ok(()));
    assert_eq!(fs::read_to_string(base.join("m/transcript.jsonl")).unwrap(), "{\"id\": 1}\n\n");
    // a row of the wrong shape
    fs::write(base.join("m/transcript.jsonl"), "[1]\n").unwrap();
    assert_eq!(
        storage.update_segments(&[text_update(1, "x")], Some(&base.join("m"))),
        Err(StorageError::Attribute("'list' object has no attribute 'get'".into()))
    );
}

#[test]
fn a_title_when_updated_then_only_an_empty_or_joining_title_is_replaced() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    assert_eq!(storage.update_title("Too early"), Ok(()));
    storage.start_session(&ktalk()).unwrap();
    let title = |storage: &MeetingStorage| field(&current_meta(storage), &["title"]).clone();
    // when / then
    storage.update_title("").unwrap();
    assert_eq!(title(&storage), Value::Null);
    storage.update_title("Joining the meeting").unwrap();
    assert_eq!(title(&storage), Value::from("Joining the meeting"));
    assert_eq!(field(&current_meta(&storage), &["title_source"]), &Value::from("ktalk"));
    storage.update_title("Real title").unwrap();
    assert_eq!(title(&storage), Value::from("Real title"));
    storage.update_title("Too late").unwrap();
    assert_eq!(title(&storage), Value::from("Real title"));
    let path = storage.current_meeting_path().unwrap().join("meta.json");
    let mut meta = read_json(&path).unwrap();
    set(&mut meta, "title", Value::Int(5)).unwrap();
    save_json(&path, &meta).unwrap();
    assert_eq!(
        storage.update_title("x"),
        Err(StorageError::Attribute("'int' object has no attribute 'startswith'".into()))
    );
}

#[test]
fn an_index_that_is_not_a_list_when_a_meeting_ends_then_type_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&said("Alice", "hello")).unwrap();
    fs::write(storage.base_path().join("index.json"), r#"{"meetings": 3}"#).unwrap();
    // when / then
    assert_eq!(storage.on_ktalk_leave("user_left"), Err(StorageError::Type("'int' object is not iterable".into())));
}

#[test]
fn ended_meetings_when_indexed_then_replaced_by_id_newest_first_and_empty_ones_removed() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    fs::write(
        base.join("index.json"),
        r#"{"meetings": [{"id": "old", "created_at": "2026-07-01T00:00:00+00:00"}, {"id": "m", "created_at": "x"}]}"#,
    )
    .unwrap();
    let meta = |count: i64| {
        pyjson::loads(&format!(
            r#"{{"id": "m", "created_at": "2026-07-04T09:00:00+00:00", "ended_at": "2026-07-04T09:30:00+00:00",
                "title": "T", "source": "zoom", "end_reason": "user_left", "ktalk": {{"live_url": "u"}},
                "stats": {{"segment_count": {count}, "duration_seconds": 1800}}, "participants": {{"a": {{"name": "A"}}}}}}"#
        ))
        .unwrap()
    };
    fs::create_dir_all(base.join("m")).unwrap();
    // when
    storage.shared.update_index(&meta(2)).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(base.join("index.json")).unwrap(),
        pretty(
            r#"{"meetings": [{"id": "m", "created_at": "2026-07-04T09:00:00+00:00",
            "ended_at": "2026-07-04T09:30:00+00:00", "duration_seconds": 1800, "title": "T", "source": "zoom",
            "end_reason": "user_left", "ktalk_live_url": "u", "segment_count": 2, "participants": ["A"]},
            {"id": "old", "created_at": "2026-07-01T00:00:00+00:00"}]}"#
        )
    );
    // when the meeting has no segments
    storage.shared.update_index(&meta(0)).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(base.join("index.json")).unwrap(),
        pretty(r#"{"meetings": [{"id": "old", "created_at": "2026-07-01T00:00:00+00:00"}]}"#)
    );
    assert!(!base.join("m").exists());
    let mut broken = meta(1);
    set(&mut broken, "participants", Value::Array(Vec::new())).unwrap();
    assert_eq!(
        storage.shared.update_index(&broken),
        Err(StorageError::Attribute("'list' object has no attribute 'values'".into()))
    );
}

#[test]
fn a_meeting_without_meta_when_deleted_then_its_directory_goes_and_an_index_without_it_is_not_rewritten() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    fs::create_dir_all(base.join("bare")).unwrap();
    let index = r#"{"meetings": [{"id": "other"}, {"title": "no id"}]}"#;
    fs::write(base.join("index.json"), index).unwrap();
    // when
    let deleted = storage.delete_meeting("bare").unwrap();
    // then
    assert!(deleted);
    assert!(!base.join("bare").exists());
    assert_eq!(fs::read_to_string(base.join("index.json")).unwrap(), index);
}

#[test]
fn an_index_without_meetings_or_with_a_non_list_when_a_meeting_is_deleted_then_kept_or_type_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "m", r#"{"id": "m", "ended_at": "e"}"#);
    fs::write(base.join("index.json"), r#"{"meetings": "m"}"#).unwrap();
    // when / then
    assert_eq!(storage.delete_meeting("m"), Err(StorageError::Type("'str' object is not iterable".into())));
    assert!(base.join("m").exists());
    fs::write(base.join("index.json"), "{}").unwrap();
    assert_eq!(storage.delete_meeting("m"), Ok(true));
    assert_eq!(fs::read_to_string(base.join("index.json")).unwrap(), "{}");
    put(&base, "list", "[]");
    assert!(matches!(storage.delete_meeting("list"), Err(StorageError::Attribute(_))));
}

#[test]
fn neighbours_that_are_gone_or_point_elsewhere_when_a_meeting_is_deleted_then_left_as_they_are() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "b", r#"{"id": "b", "ended_at": "e", "prev_meeting_id": "a", "next_meeting_id": "gone"}"#);
    let a = r#"{"id": "a", "next_meeting_id": "someone-else"}"#;
    put(&base, "a", a);
    put(&base, "x", r#"{"ended_at": "e", "prev_meeting_id": "w", "next_meeting_id": null}"#);
    put(&base, "w", r#"{"id": "w"}"#);
    // when
    assert_eq!(storage.delete_meeting("b"), Ok(true));
    assert_eq!(storage.delete_meeting("x"), Ok(true));
    // then
    assert_eq!(fs::read_to_string(base.join("a/meta.json")).unwrap(), a);
    assert!(!base.join("gone").exists());
    assert_eq!(
        fs::read_to_string(base.join("w/meta.json")).unwrap(),
        pretty(r#"{"id": "w", "next_meeting_id": null}"#)
    );
}

#[test]
fn an_index_missing_or_without_meetings_when_a_title_is_stored_then_meta_only_or_the_index_rewritten_as_is() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "m1", r#"{"id": "m1", "ended_at": "e"}"#);
    let mut fields = Map::new();
    fields.insert("title", Value::from("New"));
    // when / then: no index
    storage.store_title("m1", &fields).unwrap();
    assert!(!base.join("index.json").exists());
    assert_eq!(field(&meta_at(&storage, "m1"), &["title"]), &Value::from("New"));
    // an index without meetings is written back as it is
    fs::write(base.join("index.json"), r#"{"other": 1}"#).unwrap();
    storage.store_title("m1", &fields).unwrap();
    assert_eq!(fs::read_to_string(base.join("index.json")).unwrap(), pretty(r#"{"other": 1}"#));
    // an index whose meetings are not a list
    fs::write(base.join("index.json"), r#"{"meetings": {}}"#).unwrap();
    assert_eq!(storage.store_title("m1", &fields), Err(StorageError::Type("'dict' object is not iterable".into())));
    // a row of the wrong shape
    fs::write(base.join("index.json"), r#"{"meetings": [3]}"#).unwrap();
    assert_eq!(
        storage.store_title("m1", &fields),
        Err(StorageError::Attribute("'int' object has no attribute 'get'".into()))
    );
}

#[test]
fn old_audio_beside_a_meta_that_cannot_be_read_when_cleaned_then_deleted_and_the_error_printed() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, lines) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    fs::create_dir_all(base.join("m")).unwrap();
    fs::write(base.join("m/meta.json"), "{").unwrap();
    wav(&base.join("m/audio_mic.wav"), 30.0);
    // when
    let deleted = storage.cleanup_old_audio(24.0).unwrap();
    // then
    assert_eq!(deleted, 1);
    assert!(!base.join("m/audio_mic.wav").exists());
    assert_eq!(
        printed(&lines),
        vec![
            (Stream::Stdout, "Cleaned up 1 audio file(s) older than 24h".to_owned()),
            (
                Stream::Stdout,
                "Warning: Cleanup error for m: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
                    .to_owned()
            ),
        ]
    );
}

#[test]
fn old_audio_beside_a_meta_of_the_wrong_shape_when_cleaned_then_the_error_is_raised() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "m", "[]");
    wav(&base.join("m/audio_system.wav"), 30.0);
    // when / then
    assert_eq!(
        storage.cleanup_old_audio(24.0),
        Err(StorageError::Type("list indices must be integers or slices, not str".into()))
    );
}
