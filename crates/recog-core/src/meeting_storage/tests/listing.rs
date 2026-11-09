//! What the listings return: the active meetings, the external directories and their
//! cached indexes, and `list_meetings` over all of them.

use super::*;

fn dumped(values: &[Value]) -> String {
    pyjson::dumps(&Value::Array(values.to_vec()), None)
}

fn active_fixture(base: &Path) {
    put(
        base,
        "a1",
        r#"{"id": "a1", "created_at": "2026-07-04T09:00:00+00:00", "ended_at": null, "source": "old-meet",
            "title": "Daily", "conference": {"live_url": "https://x.example.org/r"}, "stats": {"segment_count": 4},
            "participants": {"p": {"name": "Ann"}, "q": {"name": "Bob"}}}"#,
    );
    put(
        base,
        "a2",
        r#"{"id": "a2", "created_at": "2026-07-04T09:30:00+00:00", "ktalk": {"live_url": "https://o.example.org/k"}}"#,
    );
    put(
        base,
        "a3",
        r#"{"id": "a3", "created_at": "garbage", "mode": {"ktalk_only": true}, "recording_active": true, "source": "zoom"}"#,
    );
    put(
        base,
        "ended",
        r#"{"id": "ended", "created_at": "2026-07-04T09:10:00+00:00", "ended_at": "2026-07-04T09:20:00+00:00"}"#,
    );
    put(
        base,
        "waiting",
        r#"{"id": "waiting", "created_at": "2026-07-04T09:40:00+00:00", "mode": {"ktalk_only": true}}"#,
    );
    put(base, "broken", "{not json");
    put(base, "nocreated", r#"{"id": "nocreated"}"#);
    put(base, "noname", r#"{"id": "noname", "created_at": "2026-07-04T09:50:00+00:00", "participants": {"p": {}}}"#);
    fs::create_dir_all(base.join("nometa")).unwrap();
    fs::write(base.join("stray.txt"), "a file, not a meeting").unwrap();
}

const ACTIVE: &str = "[{\"id\": \"a3\", \"created_at\": \"garbage\", \"ended_at\": null, \"duration_seconds\": 0, \
    \"title\": null, \"source\": \"zoom\", \"end_reason\": null, \"ktalk_live_url\": null, \"segment_count\": 0, \
    \"participants\": [], \"is_active\": true}, {\"id\": \"a2\", \"created_at\": \"2026-07-04T09:30:00+00:00\", \
    \"ended_at\": null, \"duration_seconds\": 1800, \"title\": null, \"source\": null, \"end_reason\": null, \
    \"ktalk_live_url\": \"https://o.example.org/k\", \"segment_count\": 0, \"participants\": [], \"is_active\": true}, \
    {\"id\": \"a1\", \"created_at\": \"2026-07-04T09:00:00+00:00\", \"ended_at\": null, \"duration_seconds\": 3600, \
    \"title\": \"Daily\", \"source\": \"jitsi\", \"end_reason\": null, \"ktalk_live_url\": \"https://x.example.org/r\", \
    \"segment_count\": 4, \"participants\": [\"Ann\", \"Bob\"], \"is_active\": true}]";

#[test]
fn meetings_not_ended_when_listed_then_entries_newest_first_and_the_unreadable_skipped() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    active_fixture(storage.base_path());
    // when
    let active = storage.list_active_meetings().unwrap();
    // then
    assert_eq!(dumped(&active), ACTIVE);
}

#[test]
fn an_active_meeting_of_the_wrong_shape_when_listed_then_the_error_is_raised() {
    for meta in [
        r#"{"id": "x", "created_at": "2026-07-04T09:00:00+00:00", "participants": []}"#,
        r#"{"id": "x", "created_at": "2026-07-04T09:00:00+00:00", "mode": "ktalk_only"}"#,
        r#"[1]"#,
    ] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let (storage, _) = storage_with(dir.path(), |_| {});
        put(storage.base_path(), "x", meta);
        // when
        let result = storage.list_active_meetings();
        // then
        assert!(matches!(result, Err(StorageError::Attribute(_))), "{meta}: {result:?}");
    }
}

#[test]
fn active_meetings_with_keys_that_do_not_compare_when_listed_then_type_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    put(storage.base_path(), "a", r#"{"id": "a", "created_at": "2026-07-04T09:00:00+00:00"}"#);
    put(storage.base_path(), "b", r#"{"id": "b", "created_at": 5}"#);
    // when / then
    assert!(matches!(storage.list_active_meetings(), Err(StorageError::Type(_))));
}

fn external_fixture(root: &Path) -> PathBuf {
    let ext = root.join("ext/a/meetings");
    put(
        &ext,
        "m1",
        r#"{"id": "m1", "created_at": "2026-07-01T08:00:00+00:00", "ended_at": "2026-07-01T09:00:00+00:00",
            "source": "ktalk", "title": "Phone call", "end_reason": "user_left", "stats": {"duration_seconds": 3600,
            "segment_count": 7}, "conference": {"live_url": "https://k.example.org/r"},
            "participants": {"p1": {"name": "Ann"}, "p2": {}}}"#,
    );
    ext
}

const EXTERNAL_M1: &str = "{\"id\": \"m1\", \"created_at\": \"2026-07-01T08:00:00+00:00\", \
    \"ended_at\": \"2026-07-01T09:00:00+00:00\", \"duration_seconds\": 3600, \"title\": \"Phone call\", \
    \"source\": \"jitsi\", \"end_reason\": \"user_left\", \"conference_url\": \"https://k.example.org/r\", \
    \"segment_count\": 7, \"participants\": [\"Ann\", \"p2\"], \"_external\": true, \"_external_path\": ";

fn cache_name(ext: &Path, source: &str) -> String {
    let digest = Md5::digest(format!("{}:{source}", ext.display()).as_bytes());
    format!("{}.json", &digest.iter().map(|b| format!("{b:02x}")).collect::<String>()[..8])
}

#[test]
fn an_external_directory_when_listed_then_its_meetings_carry_the_override_and_the_index_is_cached() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = external_fixture(dir.path());
    let (storage, _) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), Some("old-meet".into()))]);
    // when
    let meetings = storage.list_meetings(false, false).unwrap();
    // then
    let expected =
        format!("[{EXTERNAL_M1}{}}}]", pyjson::dumps(&Value::from(ext.to_string_lossy().into_owned()), None));
    assert_eq!(dumped(&meetings), expected);
    let cache = read_json(&dir.path().join("cache").join(cache_name(&ext, "old-meet"))).unwrap();
    assert_eq!(field(&cache, &["path"]), &Value::from(ext.to_string_lossy().into_owned()));
    assert_eq!(field(&cache, &["source_override"]), &Value::from("old-meet"));
    assert_eq!(field(&cache, &["updated_at"]), &Value::from("2026-07-04T10:00:00+00:00"));
    // the cache holds the scanned entries, their source already canonical
    assert_eq!(field(&cache, &["meetings"]), &Value::Array(meetings));
    let text = fs::read_to_string(dir.path().join("cache").join(cache_name(&ext, "old-meet"))).unwrap();
    assert!(text.starts_with("{\n  \"path\": "), "{text}");
}

#[test]
fn an_external_meeting_without_an_override_when_scanned_then_its_own_source_or_unknown() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = dir.path().join("ext/b");
    put(&ext, "own", r#"{"id": "own", "created_at": "2026-07-02T08:00:00+00:00", "source": "old-meet"}"#);
    put(&ext, "none", r#"{"id": "none", "created_at": "2026-07-03T08:00:00+00:00", "ktalk": {"live_url": "u"}}"#);
    let (storage, _) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), None)]);
    // when
    let meetings = storage.list_meetings(false, false).unwrap();
    // then
    let pick = |i: usize, key: &str| field(&meetings[i], &[key]).clone();
    assert_eq!(
        (pick(0, "id"), pick(0, "source"), pick(0, "conference_url")),
        (Value::from("none"), Value::from("unknown"), Value::from("u"))
    );
    assert_eq!((pick(1, "id"), pick(1, "source")), (Value::from("own"), Value::from("jitsi")));
    assert_eq!(
        (pick(1, "ended_at"), pick(1, "duration_seconds"), pick(1, "segment_count")),
        (Value::Null, Value::Int(0), Value::Int(0))
    );
    assert!(dir.path().join("cache").join(cache_name(&ext, "None")).exists());
}

#[test]
fn unreadable_external_meetings_when_scanned_then_warned_on_stderr_and_skipped() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = external_fixture(dir.path());
    put(&ext, "broken", "{");
    put(&ext, "noid", r#"{"created_at": "2026-07-01T08:00:00+00:00"}"#);
    fs::create_dir_all(ext.join("nometa")).unwrap();
    let (storage, lines) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), Some("phone".into()))]);
    // when
    let meetings = storage.list_meetings(false, true).unwrap();
    // then
    assert_eq!(meetings.len(), 1);
    assert_eq!(
        printed(&lines),
        vec![
            (
                Stream::Stderr,
                format!(
                    "Warning: Failed to load {}: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
                    ext.join("broken/meta.json").display()
                )
            ),
            (Stream::Stderr, format!("Warning: Failed to load {}: 'id'", ext.join("noid/meta.json").display())),
        ]
    );
}

#[test]
fn an_external_meeting_of_the_wrong_shape_when_scanned_then_the_error_is_raised() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = dir.path().join("ext");
    put(&ext, "m", r#"{"id": "m", "participants": ["Ann"]}"#);
    let (storage, _) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), None)]);
    // when / then
    assert_eq!(
        storage.list_meetings(false, false),
        Err(StorageError::Attribute("'list' object has no attribute 'items'".into()))
    );
}

#[test]
fn a_fresh_cache_when_listed_then_read_instead_of_scanning_and_a_refresh_or_a_newer_directory_rescans() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = external_fixture(dir.path());
    let (storage, _) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), Some("phone".into()))]);
    storage.list_meetings(false, false).unwrap();
    let retitled = fs::read_to_string(ext.join("m1/meta.json")).unwrap().replace("Phone call", "Renamed");
    fs::write(ext.join("m1/meta.json"), retitled).unwrap();
    let title = |storage: &MeetingStorage, refresh: bool| {
        field(&storage.list_meetings(false, refresh).unwrap()[0], &["title"]).clone()
    };
    // when / then: the cache is newer than the directory
    assert_eq!(title(&storage, false), Value::from("Phone call"));
    // a refresh rescans
    assert_eq!(title(&storage, true), Value::from("Renamed"));
    // a cache older than the directory is rebuilt
    let cache_file = dir.path().join("cache").join(cache_name(&ext, "phone"));
    let cached = fs::read_to_string(&cache_file).unwrap().replace("Renamed", "From the cache");
    fs::write(&cache_file, cached).unwrap();
    assert_eq!(title(&storage, false), Value::from("From the cache"));
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&cache_file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(past))
        .unwrap();
    assert_eq!(title(&storage, false), Value::from("Renamed"));
}

#[test]
fn a_cache_that_is_not_json_or_has_no_list_when_listed_then_rebuilt_or_read_as_empty() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = external_fixture(dir.path());
    let (storage, _) = storage_with(dir.path(), |o| o.external_paths = vec![(ext.clone(), Some("phone".into()))]);
    let cache_file = dir.path().join("cache").join(cache_name(&ext, "phone"));
    fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
    // when / then: not JSON is rebuilt
    fs::write(&cache_file, "garbage").unwrap();
    assert_eq!(storage.list_meetings(false, false).unwrap().len(), 1);
    assert!(fs::read_to_string(&cache_file).unwrap().contains("\"m1\""));
    // a cache whose meetings are not a list lists nothing
    fs::write(&cache_file, r#"{"meetings": 3}"#).unwrap();
    assert_eq!(storage.list_meetings(false, false).unwrap(), Vec::<Value>::new());
}

#[test]
fn the_index_externals_and_active_meetings_when_listed_then_merged_newest_first_with_canonical_sources() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let ext = external_fixture(dir.path());
    let missing = dir.path().join("ext/missing");
    let (storage, _) = storage_with(dir.path(), |o| {
        o.external_paths = vec![(ext.clone(), Some("phone".into())), (missing, Some("x".into()))]
    });
    let base = storage.base_path().to_path_buf();
    fs::write(
        base.join("index.json"),
        r#"{"meetings": [{"id": "i1", "created_at": "2026-07-02T10:00:00+00:00", "source": "old-meet"},
            {"id": "i2", "source": "ktalk"}]}"#,
    )
    .unwrap();
    put(&base, "live", r#"{"id": "live", "created_at": "2026-07-04T09:00:00+00:00", "source": "old-meet"}"#);
    // when
    let all = storage.list_meetings(true, false).unwrap();
    let ended = storage.list_meetings(false, false).unwrap();
    // then
    let ids = |list: &[Value]| list.iter().map(|m| field(m, &["id"]).py_str()).collect::<Vec<_>>();
    assert_eq!(ids(&all), ["live", "i1", "m1", "i2"]);
    assert_eq!(ids(&ended), ["i1", "m1", "i2"]);
    let sources = all.iter().map(|m| field(m, &["source"]).clone()).collect::<Vec<_>>();
    assert_eq!(sources, [Value::from("jitsi"), Value::from("jitsi"), Value::from("phone"), Value::from("ktalk")]);
    assert_eq!(field(&all[0], &["is_active"]), &Value::Bool(true));
    // the stored index keeps its own source names
    assert!(fs::read_to_string(base.join("index.json")).unwrap().contains("old-meet"));
}

#[test]
fn an_index_without_meetings_or_with_a_non_list_when_listed_then_empty_or_type_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_with(dir.path(), |_| {});
    let index = storage.base_path().join("index.json");
    // when / then
    assert_eq!(storage.list_meetings(false, false), Ok(Vec::new()));
    fs::write(&index, "{}").unwrap();
    assert_eq!(storage.list_meetings(false, false), Ok(Vec::new()));
    fs::write(&index, r#"{"meetings": 1}"#).unwrap();
    assert_eq!(storage.list_meetings(false, false), Err(StorageError::Type("'int' object is not iterable".into())));
    fs::write(&index, r#"{"meetings": [{"id": "a"}, 2]}"#).unwrap();
    assert_eq!(
        storage.list_meetings(false, false),
        Err(StorageError::Attribute("'int' object has no attribute 'get'".into()))
    );
}
