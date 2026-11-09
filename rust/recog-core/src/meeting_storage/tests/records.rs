//! A meeting read back, its audio paths, its recording flag and diarization results,
//! transcript backups, and the external directories a configuration names.

use super::*;

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "RIFF").unwrap();
}

#[test]
fn a_stored_meeting_when_read_then_meta_gains_the_local_wavs_and_canonical_source_and_the_file_stays() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = dir.path().join("ext");
    let (storage, _) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), Some("phone".into()))]);
    let base = storage.base_path().to_path_buf();
    let stored = r#"{"id": "m1", "source": "old-meet", "audio_mic_path": "/gone/audio_mic.wav"}"#;
    put(&base, "m1", stored);
    touch(&base.join("m1/audio_system.wav"));
    fs::write(base.join("m1/transcript.jsonl"), "{\"id\": 1}\n\n{\"id\": 2}\n").unwrap();
    put(&ext, "e1", r#"{"id": "e1"}"#);
    // when
    let meeting = storage.get_meeting("m1").unwrap().unwrap();
    let external = storage.get_meeting("e1").unwrap().unwrap();
    // then
    let system = base.join("m1/audio_system.wav").to_string_lossy().into_owned();
    assert_eq!(
        pyjson::dumps(&meeting, None),
        format!(
            "{{\"meta\": {{\"id\": \"m1\", \"source\": \"jitsi\", \"audio_mic_path\": \"/gone/audio_mic.wav\", \
             \"audio_system_path\": \"{system}\"}}, \"segments\": [{{\"id\": 1}}, {{\"id\": 2}}]}}"
        )
    );
    assert_eq!(fs::read_to_string(base.join("m1/meta.json")).unwrap(), stored);
    assert_eq!(pyjson::dumps(&external, None), "{\"meta\": {\"id\": \"e1\"}, \"segments\": []}");
    assert_eq!(storage.get_meeting("missing"), Ok(None));
    assert_eq!(storage.find_meeting_path("m1"), Some(base.join("m1")));
    assert_eq!(storage.find_meeting_path("e1"), Some(ext.join("e1")));
    assert_eq!(storage.find_meeting_path("missing"), None);
}

#[test]
fn a_meeting_with_a_broken_transcript_or_meta_when_read_then_the_decode_error_is_raised() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "bad-line", "{}");
    fs::write(base.join("bad-line/transcript.jsonl"), "{\"id\": 1}\nnope\n").unwrap();
    put(&base, "bad-meta", "[");
    fs::create_dir_all(base.join("no-meta")).unwrap();
    // when / then
    assert_eq!(
        storage.get_meeting("bad-line"),
        Err(StorageError::Json("Expecting value: line 1 column 1 (char 0)".into()))
    );
    assert!(matches!(storage.get_meeting("bad-meta"), Err(StorageError::Json(_))));
    assert!(matches!(storage.get_meeting("no-meta"), Err(StorageError::Os(_))));
}

#[test]
fn recorded_audio_when_its_paths_are_asked_then_only_files_that_exist_are_returned() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = dir.path().join("ext");
    let (storage, _) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), None)]);
    let base = storage.base_path().to_path_buf();
    let elsewhere = dir.path().join("elsewhere.wav");
    touch(&elsewhere);
    put(&base, "local", r#"{"audio_mic_path": "/gone/audio_mic.wav"}"#);
    touch(&base.join("local/audio_system.wav"));
    put(&base, "moved", &format!("{{\"audio_mic_path\": \"{}\", \"audio_system_path\": \"\"}}", elsewhere.display()));
    put(&base, "odd", r#"{"audio_mic_path": 5}"#);
    touch(&base.join("nometa/audio_mic.wav"));
    put(&ext, "e", "{}");
    touch(&ext.join("e/audio_mic.wav"));
    // when / then
    assert_eq!(storage.get_audio_paths("local"), Ok((None, Some(base.join("local/audio_system.wav")))));
    assert_eq!(storage.get_audio_paths("moved"), Ok((Some(elsewhere), None)));
    assert_eq!(storage.get_audio_paths("odd"), Ok((None, None)));
    assert_eq!(storage.get_audio_paths("nometa"), Ok((None, None)));
    assert_eq!(storage.get_audio_paths("e"), Ok((None, None)));
    put(&base, "broken", "{");
    assert!(matches!(storage.get_audio_paths("broken"), Err(StorageError::Json(_))));
}

#[test]
fn audio_paths_when_set_then_the_non_empty_ones_are_stored_in_the_current_or_the_given_meeting() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    assert_eq!(storage.set_audio_paths(Some("/a.wav"), None, None), Ok(()));
    let first = storage.start_session(&ktalk()).unwrap();
    let pinned = storage.base_path().join(&first);
    storage.append_segment(&said("Alice", "hello")).unwrap();
    storage.split_on_silence_timeout().unwrap();
    // when
    storage.set_audio_paths(Some("/m.wav"), Some(""), None).unwrap();
    storage.set_audio_paths(None, Some("/s.wav"), Some(&pinned)).unwrap();
    storage.set_audio_paths(Some("/x.wav"), None, Some(&storage.base_path().join("absent"))).unwrap();
    // then
    let current = current_meta(&storage);
    assert_eq!(
        (field(&current, &["audio_mic_path"]), field(&current, &["audio_system_path"])),
        (&Value::from("/m.wav"), &Value::Null)
    );
    let old = read_json(&pinned.join("meta.json")).unwrap();
    assert_eq!(
        (field(&old, &["audio_mic_path"]), field(&old, &["audio_system_path"])),
        (&Value::Null, &Value::from("/s.wav"))
    );
    assert_eq!(field(&old, &["updated_at"]), &Value::from("2026-07-04T10:00:00+00:00"));
    assert!(!storage.base_path().join("absent").exists());
}

#[test]
fn recording_when_flagged_then_meta_takes_the_session_mode_and_the_flag() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    assert_eq!(storage.set_recording_active(true), Ok(()));
    let mode = SessionMode {
        mic: true,
        ktalk: true,
        ktalk_only: true,
        system_audio_backend: "pulse".into(),
        ..SessionMode::default()
    };
    storage.start_session(&mode).unwrap();
    let path = storage.current_meeting_path().unwrap().join("meta.json");
    let mut meta = read_json(&path).unwrap();
    set(&mut meta, "mode", Value::from("another policy")).unwrap();
    save_json(&path, &meta).unwrap();
    // when
    storage.set_recording_active(true).unwrap();
    // then
    let meta = current_meta(&storage);
    assert_eq!(
        pyjson::dumps(field(&meta, &["mode"]), None),
        "{\"mic\": true, \"system_audio\": false, \"ktalk\": true, \"screencapture\": false, \
         \"system_audio_backend\": \"pulse\", \"ktalk_only\": true}"
    );
    assert_eq!(field(&meta, &["recording_active"]), &Value::Bool(true));
    storage.set_recording_active(false).unwrap();
    assert_eq!(field(&current_meta(&storage), &["recording_active"]), &Value::Bool(false));
}

#[test]
fn a_diarization_result_when_set_then_stored_per_track_and_a_missing_meeting_is_left_alone() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    put(storage.base_path(), "m", r#"{"id": "m"}"#);
    // when
    storage.set_diarization_result("m", "mic", 2).unwrap();
    storage.set_diarization_result("absent", "mic", 1).unwrap();
    // then
    assert_eq!(
        fs::read_to_string(storage.base_path().join("m/meta.json")).unwrap(),
        "{\n  \"id\": \"m\",\n  \"diarization_mic\": {\n    \"performed_at\": \"2026-07-04T10:00:00+00:00\",\n    \
         \"speakers_found\": 2\n  },\n  \"updated_at\": \"2026-07-04T10:00:00+00:00\"\n}"
    );
    assert!(!storage.base_path().join("absent").exists());
    put(storage.base_path(), "list", "[]");
    assert!(matches!(storage.set_diarization_result("list", "mic", 1), Err(StorageError::Type(_))));
}

#[test]
fn a_transcript_when_backed_up_then_copied_with_its_times_once_and_restored_with_the_marks_removed() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "m", r#"{"id": "m", "diarization_mic": {}, "diarization_system": {}}"#);
    let transcript = base.join("m/transcript.jsonl");
    fs::write(&transcript, "{\"id\": 1}\n").unwrap();
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    fs::File::options()
        .write(true)
        .open(&transcript)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(past))
        .unwrap();
    let backup = base.join("m/transcript.jsonl.backup");
    // when
    let made = storage.create_transcript_backup("m").unwrap();
    fs::write(&transcript, "{\"id\": 1, \"speaker\": \"changed\"}\n").unwrap();
    let again = storage.create_transcript_backup("m").unwrap();
    // then
    assert_eq!((made.as_ref(), again.as_ref()), (Some(&backup), Some(&backup)));
    assert_eq!(fs::read_to_string(&backup).unwrap(), "{\"id\": 1}\n");
    assert_eq!(fs::metadata(&backup).unwrap().modified().unwrap(), past);
    let meta = read_json(&base.join("m/meta.json")).unwrap();
    assert_eq!(field(&meta, &["transcript_backup_path"]), &Value::from(backup.to_string_lossy().into_owned()));
    // when restored
    assert_eq!(storage.restore_transcript_backup("m"), Ok(true));
    // then
    assert_eq!(fs::read_to_string(&transcript).unwrap(), "{\"id\": 1}\n");
    assert!(!backup.exists());
    assert_eq!(
        fs::read_to_string(base.join("m/meta.json")).unwrap(),
        "{\n  \"id\": \"m\",\n  \"updated_at\": \"2026-07-04T10:00:00+00:00\"\n}"
    );
    assert_eq!(storage.restore_transcript_backup("m"), Ok(false));
}

#[test]
fn no_transcript_or_a_meta_of_the_wrong_shape_when_backed_up_then_none_or_the_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "empty", "{}");
    put(&base, "list", "[]");
    fs::write(base.join("list/transcript.jsonl"), "").unwrap();
    // when / then
    assert_eq!(storage.create_transcript_backup("empty"), Ok(None));
    assert_eq!(storage.create_transcript_backup("absent"), Ok(None));
    assert!(matches!(storage.create_transcript_backup("list"), Err(StorageError::Type(_))));
    assert_eq!(
        storage.restore_transcript_backup("list"),
        Err(StorageError::Attribute("'NoneType' object has no attribute 'pop'".into()))
    );
    assert!(!base.join("list/transcript.jsonl.backup").exists());
}

#[test]
fn a_transcript_that_cannot_be_copied_when_backed_up_then_os_error_and_meta_untouched() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let base = storage.base_path().to_path_buf();
    put(&base, "m", "{}");
    fs::create_dir_all(base.join("m/transcript.jsonl")).unwrap();
    // when
    let result = storage.create_transcript_backup("m");
    // then
    assert!(matches!(result, Err(StorageError::Os(_))), "{result:?}");
    assert_eq!(fs::read_to_string(base.join("m/meta.json")).unwrap(), "{}");
}

#[test]
fn configured_external_entries_when_split_then_the_source_is_named_or_the_parent_directory() {
    // given
    let entries = [" /data/a : phone ", "/data/phone/meetings", "meetings", "/top", ".", "rel/dir/x"];
    // when
    let parsed = external_paths(&entries.map(str::to_owned));
    // then
    assert_eq!(
        parsed,
        vec![
            (PathBuf::from("/data/a"), Some("phone".to_owned())),
            (PathBuf::from("/data/phone/meetings"), Some("phone".to_owned())),
            (PathBuf::from("meetings"), None),
            (PathBuf::from("/top"), None),
            (PathBuf::from("."), None),
            (PathBuf::from("rel/dir/x"), Some("dir".to_owned())),
        ]
    );
}
