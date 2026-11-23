//! The AI functions level of `ai-settings.json`: how it is read and how a missing
//! settings file is settled at startup, on temporary data directories only.

use std::path::Path;
use std::time::Duration;

use recog_ai::AiError;
use recog_ai::access::Access;
use recog_ai::install::LegacyEnv;
use recog_ai::json::Json;
use recog_ai::settings::{AiSettingsStore, Fallback};

fn store(data: &Path) -> AiSettingsStore {
    AiSettingsStore::new(data, None).with_legacy_env(LegacyEnv::default())
}

const PRE_CHANGE: &str =
    r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {}}"#;

fn write_settings(data: &Path, text: &str) {
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(data.join("ai-settings.json"), text).unwrap();
}

fn saved(data: &Path) -> Json {
    Json::parse(&std::fs::read_to_string(data.join("ai-settings.json")).unwrap()).unwrap()
}

fn meeting(data: &Path, name: &str, meta: &str) {
    let dir = data.join("meetings").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("meta.json"), meta).unwrap();
}

fn settle(data: &Path) -> Access {
    store(data).settle_access(&data.join("meetings")).unwrap()
}

#[test]
fn file_without_access_when_resolved_then_automatic() {
    let dir = tempfile::tempdir().unwrap();
    write_settings(dir.path(), PRE_CHANGE);
    let store = store(dir.path());
    assert_eq!(store.resolve_task("title", None, Fallback::default()).unwrap().access, Access::Automatic);
    assert_eq!(store.view().unwrap().to_json()["access"].as_str(), Some("automatic"));
}

#[test]
fn no_file_and_no_meetings_when_settled_then_unasked_is_written_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(settle(dir.path()), Access::Unasked);
    assert_eq!(saved(dir.path())["access"].as_str(), Some("unasked"));
    meeting(dir.path(), "a", r#"{"ended_at": "2026-11-01T10:00:00"}"#);
    assert_eq!(settle(dir.path()), Access::Unasked);
}

#[test]
fn finished_meeting_of_an_earlier_run_when_settled_then_automatic_without_the_field() {
    let dir = tempfile::tempdir().unwrap();
    meeting(dir.path(), "a", r#"{"ended_at": "2026-11-01T10:00:00"}"#);
    assert_eq!(settle(dir.path()), Access::Automatic);
    let keys: Vec<String> = saved(dir.path()).as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["version", "revision", "models", "tasks"]);
}

#[test]
fn only_an_open_recording_when_settled_then_unasked() {
    let dir = tempfile::tempdir().unwrap();
    meeting(dir.path(), "open", r#"{"started_at": "2026-11-23T10:00:00"}"#);
    meeting(dir.path(), "null", r#"{"ended_at": null}"#);
    meeting(dir.path(), "broken", "{");
    assert_eq!(settle(dir.path()), Access::Unasked);
}

#[test]
fn access_present_when_settled_again_then_file_and_revision_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    settle(dir.path());
    let before = std::fs::read(dir.path().join("ai-settings.json")).unwrap();
    assert_eq!(settle(dir.path()), Access::Unasked);
    assert_eq!(std::fs::read(dir.path().join("ai-settings.json")).unwrap(), before);
}

#[test]
fn existing_file_without_access_when_settled_then_automatic_and_bytes_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    write_settings(dir.path(), PRE_CHANGE);
    assert_eq!(settle(dir.path()), Access::Automatic);
    assert_eq!(std::fs::read_to_string(dir.path().join("ai-settings.json")).unwrap(), PRE_CHANGE);
}

#[test]
fn settle_waiting_on_the_lock_when_another_process_writes_meanwhile_then_its_file_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    std::fs::create_dir_all(&data).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("ai-settings.lock"))
        .unwrap();
    lock.lock().unwrap();
    let settling = {
        let data = data.clone();
        std::thread::spawn(move || settle(&data))
    };
    std::thread::sleep(Duration::from_millis(100));
    let theirs = r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {}, "access": "off"}"#;
    write_settings(&data, theirs);
    lock.unlock().unwrap();
    assert_eq!(settling.join().unwrap(), Access::Off);
    assert_eq!(std::fs::read_to_string(data.join("ai-settings.json")).unwrap(), theirs);
}

#[test]
fn write_access_off_when_viewed_then_off_and_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    write_settings(dir.path(), PRE_CHANGE);
    let store = store(dir.path());
    let view = store.write_access(Access::Off, Some("0f8fad5b-d9cb-469f-a165-70867728950e")).unwrap();
    assert_eq!(view.access, Access::Off);
    assert_ne!(view.revision, "0f8fad5b-d9cb-469f-a165-70867728950e");
    assert_eq!(saved(dir.path())["access"].as_str(), Some("off"));
}

#[test]
fn write_access_unasked_when_saved_then_invalid_ai_access() {
    let dir = tempfile::tempdir().unwrap();
    write_settings(dir.path(), PRE_CHANGE);
    let error = store(dir.path()).write_access(Access::Unasked, None).unwrap_err();
    assert!(matches!(error, AiError::Value(ref message) if message == "Invalid AI access"), "{error:?}");
}

#[test]
fn unknown_access_value_in_file_when_read_then_settings_read_error() {
    let dir = tempfile::tempdir().unwrap();
    write_settings(
        dir.path(),
        r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {}, "access": "sometimes"}"#,
    );
    assert!(matches!(store(dir.path()).read(), Err(AiError::SettingsRead(_))));
}
