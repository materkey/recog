//! storage.rs on the inputs tests/differential.rs gave both implementations, with
//! the answers Python gave stated: snapshots of the extension and the identifier a
//! configuration names.
#![forbid(unsafe_code)]

mod common;

use common::*;
use recog_core::pyjson::Value;
use recog_desktop::{py, storage};

#[test]
fn unbuilt_extension_when_snapshotted_then_refused_and_nothing_written() {
    // given: everything but the worklet
    let dir = tempfile::tempdir().unwrap();
    let source = dist(dir.path());
    std::fs::remove_file(source.join("src/audio-worklet/downsampler.js")).unwrap();
    let root = dir.path().join("root");
    // when
    let error = storage::prepare_bundle(&source, &root).unwrap_err();
    // then
    assert_eq!(error.described(), "ValueError: Build browser-extension before enabling desktop integration");
    assert!(!root.exists());
}

#[test]
fn damaged_snapshot_when_prepared_again_then_integrity_refused() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let source = dist(dir.path());
    let root = dir.path().join("root");
    let (digest, snapshot) = storage::prepare_bundle(&source, &root).unwrap();
    assert_eq!(snapshot, root.join("extensions").join(&digest));
    assert_eq!(storage::prepare_bundle(&source, &root).unwrap(), (digest.clone(), snapshot.clone()));
    // when
    std::fs::write(snapshot.join("injected.js"), "tampered").unwrap();
    // then
    assert_eq!(
        storage::prepare_bundle(&source, &root).unwrap_err().described(),
        "ValueError: Extension snapshot integrity check failed"
    );
}

#[test]
fn configured_identifiers_when_validated_then_python_answers() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let (digest, snapshot) = storage::prepare_bundle(&dist(dir.path()), &root).unwrap();
    let linked = "0".repeat(64);
    std::os::unix::fs::symlink(&snapshot, root.join("extensions").join(&linked)).unwrap();
    let invalid = "ValueError: Invalid extension snapshot identifier";
    let damaged = "ValueError: Extension snapshot integrity check failed";
    let cases: Vec<(Value, Result<std::path::PathBuf, String>)> = vec![
        (py::obj([("bundle", Value::from(digest.as_str()))]), Ok(snapshot.clone())),
        (obj("{}"), Err(invalid.into())),
        (py::obj([("bundle", Value::from("A".repeat(64)))]), Err(invalid.into())),
        (py::obj([("bundle", Value::from(&digest[1..]))]), Err(invalid.into())),
        (py::obj([("bundle", Value::Int(5))]), Err("TypeError: object of type 'int' has no len()".into())),
        (py::obj([("bundle", Value::from("f".repeat(64)))]), Err(damaged.into())),
        (py::obj([("bundle", Value::from(linked.as_str()))]), Err(damaged.into())),
        (obj("[]"), Err("AttributeError: 'list' object has no attribute 'get'".into())),
    ];
    for (config, expected) in cases {
        // when
        let result = storage::validate_bundle(&root, &config).map_err(|e| e.described());
        // then
        assert_eq!(result, expected, "{}", py::dumps(&config));
    }
}

#[test]
fn directory_in_place_of_a_file_when_read_then_os_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    // when
    let error = storage::read(dir.path()).unwrap_err();
    // then
    assert_eq!(error.described(), format!("IsADirectoryError: [Errno 21] Is a directory: '{}'", dir.path().display()));
}
