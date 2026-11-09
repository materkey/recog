//! The storage's cross-process lock: two processes (the live one and the web server)
//! rewrite the same `meta.json` and `index.json`, and no read-modify-write may drop
//! another's change.

use std::process::Command;
use std::time::Duration;

use super::*;

/// Tells a child of this test binary which storage to write and its field prefix.
const CHILD_ENV: &str = "RECOG_STORAGE_LOCK_CHILD";
const CHILD_TEST: &str = "meeting_storage::tests::locking::a_child_when_started_by_the_race_then_stores_its_own_fields";
const ROUNDS: i64 = 500;

/// m1 not ended: a stored title then rewrites only its `meta.json`.
fn open_meeting(storage: &MeetingStorage) -> PathBuf {
    stored_meeting(storage, &STORED_META.replace("\"ended_at\": \"2026-07-04T09:30:00+00:00\"", "\"ended_at\": null"))
}

/// One field per round, `<prefix>_<round>`: a read-modify-write that drops another's
/// field loses it for good, since the next round rereads the file without it.
fn store_rounds(storage: &MeetingStorage, prefix: &str) {
    for round in 0..ROUNDS {
        let mut fields = Map::new();
        fields.insert(format!("{prefix}_{round}"), Value::from(round));
        storage.store_title("m1", &fields).unwrap();
    }
}

/// The fields of `prefixes` missing from the meta.
fn lost(meta: &Value, prefixes: &[&str]) -> Vec<String> {
    prefixes
        .iter()
        .flat_map(|prefix| (0..ROUNDS).map(move |round| format!("{prefix}_{round}")))
        .filter(|key| field(meta, &[key]).is_null())
        .collect()
}

#[test]
fn a_child_when_started_by_the_race_then_stores_its_own_fields() {
    let Ok(job) = std::env::var(CHILD_ENV) else { return };
    let (dir, prefix) = job.split_once('\n').unwrap();
    let dir = Path::new(dir);
    let (storage, _) = storage_at(dir);
    while !dir.join("go").exists() {
        std::thread::sleep(Duration::from_millis(1));
    }
    store_rounds(&storage, prefix);
}

#[test]
fn two_processes_when_storing_into_one_meta_then_no_update_is_lost() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let meeting = open_meeting(&storage);
    let children: Vec<_> = ["a", "b"]
        .iter()
        .map(|prefix| {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
                .env(CHILD_ENV, format!("{}\n{prefix}", dir.path().display()))
                .spawn()
                .unwrap()
        })
        .collect();
    // when
    fs::write(dir.path().join("go"), "").unwrap();
    for child in children {
        assert!(child.wait_with_output().unwrap().status.success());
    }
    // then
    let lost = lost(&read_json(&meeting.join("meta.json")).unwrap(), &["a", "b"]);
    assert!(lost.is_empty(), "{} of {} updates lost, first {:?}", lost.len(), 2 * ROUNDS, &lost[..lost.len().min(5)]);
}

#[test]
fn two_storages_of_one_process_when_storing_into_one_meta_then_no_update_is_lost() {
    // given: the web server opens a storage per request, each with its own state
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let meeting = open_meeting(&storage);
    let (done, finished) = mpsc::channel();
    // when
    for prefix in ["a", "b"] {
        let (dir, done) = (dir.path().to_path_buf(), done.clone());
        std::thread::spawn(move || {
            store_rounds(&storage_at(&dir).0, prefix);
            done.send(()).unwrap();
        });
    }
    // then
    for _ in 0..2 {
        finished.recv_timeout(Duration::from_secs(60)).expect("a writer did not finish");
    }
    let lost = lost(&read_json(&meeting.join("meta.json")).unwrap(), &["a", "b"]);
    assert!(lost.is_empty(), "{} of {} updates lost, first {:?}", lost.len(), 2 * ROUNDS, &lost[..lost.len().min(5)]);
}

/// The lock of a storage directory, held as the other process would hold it.
fn hold(base: &Path) -> fs::File {
    let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(base.join(".lock"));
    let lock = lock.unwrap();
    lock.lock().unwrap();
    lock
}

/// A storage in a session with a segment, beside an ended meeting m1 with a
/// transcript, its backup and old audio: what each writer below needs to write. No
/// index: the stored one's rows lack what ending a meeting sorts them by.
fn staged(dir: &Path) -> MeetingStorage {
    let (storage, _) = storage_at(dir);
    storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&said("Alice", "hello")).unwrap();
    let meeting = stored_meeting(&storage, STORED_META);
    fs::remove_file(storage.base_path().join("index.json")).unwrap();
    fs::write(meeting.join("transcript.jsonl"), "{\"id\": 1, \"text\": \"a\"}\n").unwrap();
    fs::write(meeting.join("transcript.jsonl.backup"), "{\"id\": 1, \"text\": \"b\"}\n").unwrap();
    wav(&meeting.join("audio_mic.wav"), 25.0);
    storage
}

type Writer = fn(&MeetingStorage);

#[test]
fn every_writer_when_another_process_holds_the_lock_then_waits_for_it() {
    // given
    let writers: [(&str, Writer); 18] = [
        ("start_session", |s| {
            let _ = s.start_session(&ktalk()).unwrap();
        }),
        ("on_meeting_join", |s| {
            let _ = s.on_meeting_join("https://k.example.org/room", None, "ktalk").unwrap();
        }),
        ("on_ktalk_leave", |s| {
            let _ = s.on_ktalk_leave("user_left").unwrap();
        }),
        ("split_on_silence_timeout", |s| {
            let _ = s.split_on_silence_timeout().unwrap();
        }),
        ("end_session", |s| {
            let _ = s.end_session().unwrap();
        }),
        ("append_segment", |s| {
            let _ = s.append_segment(&said("Bob", "hi")).unwrap();
        }),
        ("update_segments", |s| s.update_segments(&[text_update(1, "hello there")], None).unwrap()),
        ("update_title", |s| s.update_title("Daily").unwrap()),
        ("update_participants", |s| s.update_participants(&Map::new()).unwrap()),
        ("set_recording_active", |s| s.set_recording_active(true).unwrap()),
        ("set_audio_paths", |s| s.set_audio_paths(Some("mic.wav"), None, None).unwrap()),
        ("create_transcript_backup", |s| {
            let _ = s.create_transcript_backup("m1").unwrap();
        }),
        ("restore_transcript_backup", |s| {
            let _ = s.restore_transcript_backup("m1").unwrap();
        }),
        ("cleanup_old_audio", |s| {
            let _ = s.cleanup_old_audio(24.0).unwrap();
        }),
        ("delete_meeting", |s| {
            let _ = s.delete_meeting("m1").unwrap();
        }),
        ("set_diarization_result", |s| s.set_diarization_result("m1", "mic", 2).unwrap()),
        ("store_title", |s| s.store_title("m1", &title_fields()).unwrap()),
        ("the title thread", |s| s.shared.finish_meeting_title(&s.base_path().join("m1"))),
    ];
    let dirs: Vec<_> = writers.iter().map(|_| tempfile::tempdir().unwrap()).collect();
    let (done, finished) = mpsc::channel();
    let mut holders = Vec::new();
    // when
    for ((name, write), dir) in writers.iter().zip(&dirs) {
        let storage = staged(dir.path());
        holders.push(hold(storage.base_path()));
        let (name, write, done) = (*name, *write, done.clone());
        std::thread::spawn(move || {
            write(&storage);
            done.send(name).unwrap();
        });
    }
    std::thread::sleep(Duration::from_millis(300));
    let early: Vec<&str> = finished.try_iter().collect();
    drop(holders);
    // then
    assert!(early.is_empty(), "wrote without the lock: {early:?}");
    let mut names = Vec::new();
    while names.len() < writers.len() {
        let missing = writers.iter().map(|(n, _)| *n).filter(|n| !names.contains(n)).collect::<Vec<_>>();
        names.push(finished.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| panic!("{missing:?}")));
    }
}

#[test]
fn a_conference_left_and_rejoined_when_its_nested_cycles_share_one_lock_then_it_completes() {
    // given: leaving ends the meeting, counts its pause marker and indexes it; the rejoin
    // reopens it, unindexes it and counts its resume marker; the title thread writes in
    // between
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (done, finished) = mpsc::channel();
    // when
    std::thread::spawn(move || {
        let (storage, _) = storage_at(&path);
        storage.start_session(&ktalk()).unwrap();
        let joined = storage.on_meeting_join("https://k.example.org/room", None, "ktalk").unwrap();
        storage.append_segment(&said("Alice", "hello")).unwrap();
        storage.on_ktalk_leave("user_left").unwrap();
        let rejoined = storage.on_meeting_join("https://k.example.org/room", None, "ktalk").unwrap();
        storage.end_session().unwrap();
        done.send((joined, rejoined)).unwrap();
    });
    // then
    let (joined, rejoined) = finished.recv_timeout(Duration::from_secs(30)).expect("the storage locked itself out");
    assert_eq!(rejoined, joined);
}

#[test]
fn the_lock_file_when_meetings_are_scanned_and_listed_then_it_is_no_meeting() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = storage_at(dir.path());
    let first = storage.start_session(&ktalk()).unwrap();
    storage.append_segment(&said("Alice", "hello")).unwrap();
    storage.end_session().unwrap();
    assert!(storage.base_path().join(".lock").is_file());
    // when: a session start scans every entry for stale sessions and old audio
    let second = storage.start_session(&ktalk()).unwrap();
    // then
    let mut listed: Vec<String> =
        storage.list_meetings(true, false).unwrap().iter().map(|m| field(m, &["id"]).py_str()).collect();
    listed.sort();
    let mut expected = [first, second];
    expected.sort();
    assert_eq!(listed, expected);
    assert_eq!(storage.delete_meeting(".lock"), Ok(false));
}
