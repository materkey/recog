//! The cases of tests/test_meeting_catalog.py, tests/test_frontend_meeting_catalog.py
//! and the catalog case of tests/test_install_config.py.
//!
//! Python cases that patch `Path.read_text` or `os.scandir` mid-refresh, or pause the
//! worker thread, check the serialization of workers and the retry of a file replaced
//! while it was read; here the file cache stays locked by the worker for its whole run
//! and the post-read signature check is the same code path, so those are covered by the
//! cases below that change files between refreshes.

mod support;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use recog_core::install_config::InstallConfig;
use recog_core::pyjson::{self, Map, Value};
use recog_web::meeting_catalog::{EventKind, MeetingCatalog, Timings};
use support::{App, get, obj, text, write};

fn meta_value(meeting_id: &str, updates: &str) -> Value {
    let mut meta = obj(&format!(
        r#"{{"id": "{meeting_id}", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "2026-09-01T11:00:00+00:00",
        "title": "Original", "source": "ktalk", "stats": {{"segment_count": 3, "duration_seconds": 3600}},
        "participants": {{"alice": {{"name": "Alice"}}}}, "conference": {{"live_url": "https://example.test/meeting"}}}}"#
    ));
    if let (Value::Object(map), Value::Object(extra)) = (&mut meta, obj(updates)) {
        for (k, v) in extra.iter() {
            map.insert(k.clone(), v.clone());
        }
    }
    meta
}

fn write_meta(root: &Path, meeting_id: &str, updates: &str) -> PathBuf {
    let path = root.join(meeting_id).join("meta.json");
    write(&path, &pyjson::dumps(&meta_value(meeting_id, updates), None));
    path
}

fn write_index(root: &Path, rows: &[&str]) -> PathBuf {
    let path = root.join("index.json");
    write(&path, &format!(r#"{{"meetings": [{}]}}"#, rows.join(", ")));
    path
}

/// A temporary directory by its real path, as FSEvents reports paths.
struct Tmp(#[allow(dead_code)] tempfile::TempDir, PathBuf);

impl Tmp {
    fn path(&self) -> &Path {
        &self.1
    }
}

fn tempdir() -> Tmp {
    let dir = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(dir.path()).unwrap();
    Tmp(dir, real)
}

fn catalog(root: &Path) -> MeetingCatalog {
    MeetingCatalog::new(root, &[], &InstallConfig::default(), Timings::default())
}

fn quick(root: &Path, reconcile: Duration, retry: Duration) -> MeetingCatalog {
    MeetingCatalog::new(root, &[], &InstallConfig::default(), Timings { reconcile, debounce: Duration::ZERO, retry })
}

fn rows(catalog: &MeetingCatalog) -> Vec<(String, Value)> {
    catalog.list_meetings().iter().map(|r| (text(r, "id"), get(r, "title").clone())).collect()
}

fn one(path: &Path) -> Option<BTreeSet<PathBuf>> {
    Some(BTreeSet::from([path.to_path_buf()]))
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn cached_files_when_reconciled_then_unchanged_json_is_not_reopened_and_lists_touch_no_file() {
    let dir = tempdir();
    write_meta(dir.path(), "meeting", "{}");
    write_index(dir.path(), &[]);
    let catalog = catalog(dir.path());
    catalog.refresh(None).await;
    let read = catalog.files_read();

    catalog.refresh(None).await;

    assert_eq!(catalog.files_read(), read);
    std::fs::remove_dir_all(dir.path().join("meeting")).unwrap();
    assert_eq!(text(&catalog.list_meetings()[0], "title"), "Original");
}

#[tokio::test]
async fn index_and_metadata_when_merged_then_metadata_wins_and_archives_remain() {
    let dir = tempdir();
    write_meta(dir.path(), "meeting", "{}");
    write_index(
        dir.path(),
        &[
            r#"{"id": "meeting", "created_at": "2026-09-01", "title": "Stale"}"#,
            r#"{"id": "archive", "created_at": "2026-08-01", "title": "Archive"}"#,
        ],
    );
    let catalog = catalog(dir.path());

    catalog.refresh(None).await;

    assert_eq!(
        rows(&catalog),
        [("meeting".into(), Value::from("Original")), ("archive".into(), Value::from("Archive"))]
    );
}

#[tokio::test]
async fn active_meeting_when_ended_and_reopened_then_no_gap_or_duplicate() {
    let dir = tempdir();
    let meta = write_meta(dir.path(), "meeting", r#"{"ended_at": null}"#);
    let index = write_index(dir.path(), &[]);
    let catalog = catalog(dir.path());
    catalog.refresh(None).await;
    assert_eq!(get(&catalog.list_meetings()[0], "is_active"), &Value::Bool(true));

    write_meta(dir.path(), "meeting", "{}");
    catalog.refresh(one(&meta)).await;
    assert_eq!(catalog.list_meetings().len(), 1);
    assert_eq!(get(&catalog.list_meetings()[0], "is_active"), &Value::Null);
    let row = pyjson::dumps(&catalog.list_meetings()[0], None);
    write_index(dir.path(), &[&row]);
    catalog.refresh(one(&index)).await;
    write_meta(dir.path(), "meeting", r#"{"ended_at": null}"#);
    catalog.refresh(one(&meta)).await;
    assert_eq!(catalog.list_meetings().len(), 1);
    assert_eq!(get(&catalog.list_meetings()[0], "is_active"), &Value::Bool(true));
    write_index(dir.path(), &[]);
    catalog.refresh(one(&index)).await;
    assert_eq!(catalog.list_meetings().len(), 1);
}

#[tokio::test]
async fn active_snapshot_when_read_then_duration_is_current_and_copies_are_isolated() {
    let dir = tempdir();
    let created = recog_core::time::PyDateTime::utc(
        recog_core::time::now_utc(&recog_core::time::SystemClock).utc_micros().unwrap() - 120_000_000,
    )
    .isoformat();
    write_meta(dir.path(), "meeting", &format!(r#"{{"ended_at": null, "created_at": "{created}"}}"#));
    let catalog = catalog(dir.path());
    catalog.refresh(None).await;

    let mut first = catalog.list_meetings();

    let Value::Int(duration) = get(&first[0], "duration_seconds") else { panic!() };
    assert!((120..=122).contains(duration));
    if let Value::Object(map) = &mut first[0] {
        map.insert("title", Value::from("Injected"));
        if let Some(Value::Array(names)) = map.get_mut("participants") {
            names.push(Value::from("Injected"));
        }
    }
    let again = catalog.list_meetings();
    assert_eq!(get(&again[0], "participants"), &obj(r#"["Alice"]"#));
    assert_eq!(text(&again[0], "title"), "Original");
}

#[tokio::test]
async fn external_metadata_when_edited_then_the_change_is_seen_and_rows_carry_their_origin() {
    let dir = tempdir();
    let (local, external) = (dir.path().join("local"), dir.path().join("external"));
    std::fs::create_dir_all(&local).unwrap();
    let meta = write_meta(&external, "meeting", r#"{"ended_at": null}"#);
    let catalog = MeetingCatalog::new(
        &local,
        &[(external.clone(), Some("export".into()))],
        &InstallConfig::default(),
        Timings::default(),
    );
    catalog.refresh(None).await;

    write_meta(&external, "meeting", r#"{"title": "Edited", "ended_at": null}"#);
    catalog.refresh(None).await;

    let row = &catalog.list_meetings()[0];
    assert_eq!(text(row, "title"), "Edited");
    assert_eq!(text(row, "source"), "export");
    assert_eq!(text(row, "_external_path"), std::fs::canonicalize(&external).unwrap().to_string_lossy());
    assert_eq!(text(row, "conference_url"), "https://example.test/meeting");
    let map = row.as_object().unwrap();
    assert!(!map.contains_key("ktalk_live_url") && !map.contains_key("is_active"));
    std::fs::remove_file(meta).unwrap();
    catalog.refresh(None).await;
    assert!(catalog.list_meetings().is_empty());
}

#[tokio::test]
async fn atomic_replace_when_mtime_and_size_match_then_the_new_inode_is_detected() {
    let dir = tempdir();
    let meta = write_meta(dir.path(), "meeting", "{}");
    let catalog = catalog(dir.path());
    catalog.refresh(None).await;
    let before = std::fs::metadata(&meta).unwrap().modified().unwrap();

    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, std::fs::read_to_string(&meta).unwrap().replace("Original", "Replaced")).unwrap();
    std::fs::File::options().write(true).open(&replacement).unwrap().set_modified(before).unwrap();
    std::fs::rename(&replacement, &meta).unwrap();
    catalog.refresh(None).await;

    assert_eq!(text(&catalog.list_meetings()[0], "title"), "Replaced");
}

#[tokio::test]
async fn invalid_metadata_when_refreshed_then_the_last_good_entry_survives_until_repaired() {
    for bad in [r#"{"id":"#, "[]", r#"{"id":"meeting"}"#] {
        let dir = tempdir();
        let meta = write_meta(dir.path(), "meeting", "{}");
        let catalog = catalog(dir.path());
        catalog.refresh(None).await;

        std::fs::write(&meta, bad).unwrap();
        catalog.refresh(one(&meta)).await;
        assert_eq!(text(&catalog.list_meetings()[0], "title"), "Original", "{bad}");

        write_meta(dir.path(), "meeting", r#"{"title": "Recovered"}"#);
        catalog.refresh(Some(BTreeSet::new())).await;
        assert_eq!(text(&catalog.list_meetings()[0], "title"), "Recovered", "{bad}");
    }
}

#[tokio::test]
async fn invalid_index_when_refreshed_then_the_archive_survives_until_repaired() {
    let dir = tempdir();
    let index = write_index(dir.path(), &[r#"{"id": "archive", "created_at": "2026-08-01"}"#]);
    let catalog = catalog(dir.path());
    catalog.refresh(None).await;

    std::fs::write(&index, r#"{"meetings": [null]}"#).unwrap();
    catalog.refresh(one(&index)).await;
    assert_eq!(text(&catalog.list_meetings()[0], "id"), "archive");

    write_index(dir.path(), &[]);
    catalog.refresh(Some(BTreeSet::new())).await;
    assert!(catalog.list_meetings().is_empty());
}

#[tokio::test]
async fn invalid_index_fields_when_refreshed_then_the_last_good_archive_survives() {
    for bad in [
        r#"{"created_at": 123}"#,
        r#"{"participants": null}"#,
        r#"{"title": ["bad"]}"#,
        r#"{"is_active": true, "created_at": null}"#,
    ] {
        let dir = tempdir();
        write_meta(dir.path(), "meeting", "{}");
        let archived = r#"{"id": "archive", "created_at": "2026-08-01", "title": "Valid"}"#;
        let index = write_index(dir.path(), &[archived]);
        let catalog = catalog(dir.path());
        catalog.refresh(None).await;

        let mut row = obj(archived);
        if let (Value::Object(map), Value::Object(extra)) = (&mut row, obj(bad)) {
            for (k, v) in extra.iter() {
                map.insert(k.clone(), v.clone());
            }
        }
        write_index(dir.path(), &[&pyjson::dumps(&row, None)]);
        catalog.refresh(one(&index)).await;

        assert_eq!(catalog.list_meetings()[1], obj(archived), "{bad}");
    }
}

#[tokio::test]
async fn move_event_when_received_then_the_destination_metadata_refreshes() {
    let dir = tempdir();
    let meta = write_meta(dir.path(), "meeting", "{}");
    let catalog = quick(dir.path(), Duration::from_secs(30), Duration::from_secs(1));
    catalog.start().await;

    write_meta(dir.path(), "meeting", r#"{"title": "Changed"}"#);
    let tmp = PathBuf::from(format!("{}.tmp", meta.display()));
    catalog.on_any_event(EventKind::Moved, false, &tmp, Some(&std::fs::canonicalize(&meta).unwrap()));

    until("the moved metadata", || text(&catalog.list_meetings()[0], "title") == "Changed").await;
    catalog.stop().await;
}

#[tokio::test]
async fn lost_events_when_reconciliation_runs_then_new_and_deleted_meetings_are_seen() {
    let dir = tempdir();
    let meta = write_meta(dir.path(), "meeting", "{}");
    let catalog = quick(dir.path(), Duration::from_millis(20), Duration::from_secs(1));
    catalog.start().await;

    std::fs::remove_file(meta).unwrap();
    write_meta(dir.path(), "new", "{}");

    until("reconciliation", || rows(&catalog).iter().map(|(id, _)| id.clone()).collect::<Vec<_>>() == ["new"]).await;
    catalog.stop().await;
}

#[tokio::test]
async fn events_in_quick_succession_when_published_then_the_last_change_is_not_lost() {
    let dir = tempdir();
    let meta = write_meta(dir.path(), "meeting", "{}");
    let catalog = quick(dir.path(), Duration::from_secs(30), Duration::from_secs(1));
    catalog.start().await;
    let real = std::fs::canonicalize(&meta).unwrap();

    write_meta(dir.path(), "meeting", r#"{"title": "First"}"#);
    catalog.on_any_event(EventKind::Modified, false, &real, None);
    tokio::task::yield_now().await;
    write_meta(dir.path(), "meeting", r#"{"title": "Second"}"#);
    catalog.on_any_event(EventKind::Modified, false, &real, None);

    until("the second title", || text(&catalog.list_meetings()[0], "title") == "Second").await;
    catalog.stop().await;
}

#[tokio::test]
async fn scan_error_when_the_directory_recovers_then_the_retry_does_not_wait_for_reconciliation() {
    let dir = tempdir();
    let root = dir.path().join("meetings");
    write_meta(&root, "meeting", "{}");
    let catalog = quick(&root, Duration::from_secs(60), Duration::from_millis(10));
    catalog.start().await;

    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
    catalog.refresh(None).await;
    assert_eq!(catalog.list_meetings().len(), 1);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    write_meta(&root, "new", "{}");

    until("the retry", || catalog.list_meetings().len() == 2).await;
    catalog.stop().await;
}

#[tokio::test]
async fn meeting_with_a_callrec_recorder_when_listed_then_the_row_carries_it() {
    let dir = tempdir();
    write_meta(
        dir.path(),
        "meeting",
        r#"{"source": "callrec", "title": "Call", "callrec": {"package": "com.example", "recorder": "samsung"}}"#,
    );
    let catalog = catalog(dir.path());

    catalog.refresh(None).await;

    assert_eq!(text(&catalog.list_meetings()[0], "recorder"), "samsung");
    assert_eq!(text(&catalog.list_meetings()[0], "title"), "Call");
}

#[tokio::test]
async fn meeting_without_a_recorder_when_listed_then_the_row_carries_none() {
    for updates in [r#"{"callrec": {"package": "com.example"}}"#, "{}"] {
        let dir = tempdir();
        write_meta(dir.path(), "meeting", updates);
        let catalog = catalog(dir.path());

        catalog.refresh(None).await;

        assert_eq!(get(&catalog.list_meetings()[0], "recorder"), &Value::Null);
    }
}

#[tokio::test]
async fn legacy_source_when_listed_then_canonical_without_changing_the_recording() {
    let dir = tempdir();
    let meta = write_meta(dir.path(), "meeting", r#"{"source": "old-meet"}"#);
    let raw = std::fs::read_to_string(&meta).unwrap();
    let config =
        InstallConfig { source_aliases: vec![("old-meet".into(), "jitsi".into())], ..InstallConfig::default() };
    let catalog = MeetingCatalog::new(dir.path(), &[], &config, Timings::default());

    catalog.refresh(None).await;

    assert_eq!(text(&catalog.list_meetings()[0], "source"), "jitsi");
    assert_eq!(std::fs::read_to_string(&meta).unwrap(), raw);
}

// --- The catalog behind the HTTP routes ---

async fn started(app: &App) -> MeetingCatalog {
    let config = &app.state.config;
    let catalog =
        MeetingCatalog::new(&config.meetings_dir, &config.external_paths, &config.install, Timings::default());
    catalog.start().await;
    *app.state.catalog.write().unwrap() = Some(catalog.clone());
    catalog
}

fn archived(app: &App) {
    write(
        &app.meetings().join("index.json"),
        r#"{"meetings": [{"id": "archived", "created_at": "2026-09-01T10:00:00+00:00", "ended_at": "2026-09-01T11:00:00+00:00", "title": "Archived"}]}"#,
    );
}

#[tokio::test]
async fn warm_catalog_when_listing_then_the_storage_is_not_scanned() {
    let app = App::new();
    archived(&app);
    let catalog = started(&app).await;
    let read = catalog.files_read();

    let (status, body) = app.json("GET", "/api/meetings", None).await;

    assert_eq!(status, 200);
    assert_eq!(catalog.files_read(), read);
    let Value::Array(rows) = body else { panic!() };
    assert_eq!(rows.iter().map(|r| text(r, "id")).collect::<Vec<_>>(), ["archived"]);
    catalog.stop().await;
}

fn recorded_meeting(app: &App) -> String {
    let id = "2026-09-03_10-00-00_aaaabbbb";
    let dir = app.meetings().join(id);
    write(
        &dir.join("meta.json"),
        &format!(
            r#"{{"id": "{id}", "created_at": "2026-09-03T10:00:00+00:00", "ended_at": "2026-09-03T10:30:00+00:00", "source": "ktalk", "stats": {{"segment_count": 1}}}}"#
        ),
    );
    write(
        &dir.join("transcript.jsonl"),
        "{\"id\": 1, \"speaker\": \"Alice\", \"speaker_id\": \"alice\", \"text\": \"Hello there, this is a long enough line for a title\"}\n",
    );
    write(
        &app.meetings().join("index.json"),
        &format!(
            r#"{{"meetings": [{{"id": "{id}", "created_at": "2026-09-03T10:00:00+00:00", "ended_at": "2026-09-03T10:30:00+00:00", "title": null}}]}}"#
        ),
    );
    id.to_owned()
}

#[tokio::test]
async fn deleted_meeting_when_delete_returns_then_the_list_is_already_refreshed() {
    let app = App::new();
    let id = recorded_meeting(&app);
    let catalog = started(&app).await;
    let listed = |body: Value| match body {
        Value::Array(rows) => rows.iter().map(|r| text(r, "id")).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert!(listed(app.json("GET", "/api/meetings", None).await.1).contains(&id));

    assert_eq!(app.call("DELETE", &format!("/api/meetings/{id}"), None).await.0, 200);

    assert!(!listed(app.json("GET", "/api/meetings", None).await.1).contains(&id));
    catalog.stop().await;
}

#[tokio::test]
async fn title_generated_when_post_returns_then_the_list_has_the_new_title() {
    let app = App::new();
    app.factory.answer(support::Answer::Text("New title".into()));
    let id = recorded_meeting(&app);
    let catalog = started(&app).await;

    let (status, _) = app.json("POST", &format!("/api/meetings/{id}/generate-title"), None).await;

    assert_eq!(status, 200);
    let (_, Value::Array(rows)) = app.json("GET", "/api/meetings", None).await else { panic!() };
    let row = rows.iter().find(|r| text(r, "id") == id).unwrap();
    assert_eq!(text(row, "title"), "New title");
    catalog.stop().await;
}

#[tokio::test]
async fn external_directory_that_cannot_be_watched_when_starting_then_the_catalog_still_lists() {
    let app = App::with(|config| {
        config.external_paths = vec![(PathBuf::from("/nonexistent/recog-external"), Some("external".into()))]
    });
    let services = recog_web::server::start_services(&app.state).await;

    assert_eq!(app.call("GET", "/api/meetings", None).await.0, 200);
    recog_web::server::stop_services(&app.state, services).await;
}

#[tokio::test]
async fn configured_source_label_when_listing_then_its_rows_carry_it() {
    let app = App::with(|config| config.install.source_labels = vec![("importer".into(), "Phone Importer".into())]);
    for (id, source) in [("imported", "importer"), ("recorded", "ktalk")] {
        write_meta(&app.meetings(), id, &format!(r#"{{"source": "{source}"}}"#));
    }
    let catalog = started(&app).await;

    let (_, Value::Array(rows)) = app.json("GET", "/api/meetings", None).await else { panic!() };

    let labels: Map = rows.iter().map(|r| (text(r, "id"), get(r, "source_label").clone())).collect();
    assert_eq!(labels.get("imported"), Some(&Value::from("Phone Importer")));
    assert_eq!(labels.get("recorded"), Some(&Value::Null));
    catalog.stop().await;
}
