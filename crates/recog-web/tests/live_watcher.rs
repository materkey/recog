//! The cases of tests/test_frontend_live_watcher.py: rewrite detection, the seen-ID
//! watermark, meeting switches, and the checks file events trigger.

mod support;

use std::path::Path;
use std::sync::Arc;

use recog_core::meeting_storage::{MeetingStorage, StorageOptions};
use recog_core::pyjson::Value;
use recog_web::live_watcher::LiveWatcher;
use recog_web::util::Client;
use support::obj;
use tokio::sync::mpsc::UnboundedReceiver;

fn write_jsonl(path: &Path, rows: &[&str]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
}

fn atomic_rewrite(path: &Path, rows: &[&str]) {
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn append(path: &Path, row: &str) {
    use std::io::Write as _;
    writeln!(std::fs::OpenOptions::new().append(true).open(path).unwrap(), "{row}").unwrap();
}

/// A watcher over `base` and the frames it broadcasts.
fn watcher(base: &Path) -> (Arc<LiveWatcher>, UnboundedReceiver<String>) {
    let base = base.to_path_buf();
    let watcher = Arc::new(LiveWatcher::new(Arc::new(move || MeetingStorage::new(&base, StorageOptions::new(&base)))));
    let (client, rx) = Client::new();
    watcher.register(client);
    (watcher, rx)
}

fn drain(rx: &mut UnboundedReceiver<String>) -> Vec<Value> {
    let mut frames = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        frames.push(obj(&frame));
    }
    frames
}

fn ids(frame: &Value) -> Vec<i128> {
    match support::get(frame, "data") {
        Value::Array(rows) => rows
            .iter()
            .filter_map(|r| match support::get(r, "id") {
                Value::Int(n) => Some(*n),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn seen(watcher: &LiveWatcher) -> Vec<i128> {
    let mut seen: Vec<i128> = watcher.state().tail.seen.into_iter().collect();
    seen.sort_unstable();
    seen
}

#[tokio::test]
async fn fresh_transcript_with_two_segments_when_first_read_then_both_broadcast_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(
        &path,
        &[r#"{"id": 1, "text": "hello", "speaker": "A"}"#, r#"{"id": 2, "text": "world", "speaker": "B"}"#],
    );
    let (watcher, mut rx) = watcher(dir.path());

    watcher.notify_new_segments(&path).await;

    let frames = drain(&mut rx);
    assert_eq!(frames.len(), 1);
    assert_eq!(support::text(&frames[0], "type"), "segments");
    assert_eq!(ids(&frames[0]), [1, 2]);
    assert_eq!(seen(&watcher), [1, 2]);
    assert!(watcher.state().tail.position > 0);
    assert!(watcher.state().tail.inode.is_some());
}

#[tokio::test]
async fn segments_seen_when_one_is_appended_then_only_it_is_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(&path, &[r#"{"id": 1, "text": "a"}"#, r#"{"id": 2, "text": "b"}"#]);
    let (watcher, mut rx) = watcher(dir.path());
    watcher.notify_new_segments(&path).await;

    append(&path, r#"{"id": 3, "text": "c"}"#);
    watcher.notify_new_segments(&path).await;

    let frames = drain(&mut rx);
    assert_eq!(frames.len(), 2);
    assert_eq!(ids(&frames[1]), [3]);
    assert_eq!(seen(&watcher), [1, 2, 3]);
}

#[tokio::test]
async fn segments_seen_when_rewritten_atomically_then_nothing_is_broadcast_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(
        &path,
        &[
            r#"{"id": 1, "text": "one", "refined": false}"#,
            r#"{"id": 2, "text": "two", "refined": false}"#,
            r#"{"id": 3, "text": "three", "refined": false}"#,
        ],
    );
    let (watcher, mut rx) = watcher(dir.path());
    watcher.notify_new_segments(&path).await;
    let original = watcher.state().tail.inode;

    atomic_rewrite(
        &path,
        &[
            r#"{"id": 1, "text": "one", "refined": false}"#,
            r#"{"id": 2, "text": "2", "refined": true}"#,
            r#"{"id": 3, "text": "three", "refined": false}"#,
        ],
    );
    watcher.notify_new_segments(&path).await;

    assert_eq!(drain(&mut rx).len(), 1);
    assert_ne!(watcher.state().tail.inode, original);
    assert!(watcher.state().tail.position > 0);
}

#[tokio::test]
async fn rewrite_followed_by_an_append_when_read_then_only_the_new_segment_is_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(&path, &[r#"{"id": 1, "text": "one"}"#, r#"{"id": 2, "text": "two"}"#]);
    let (watcher, mut rx) = watcher(dir.path());
    watcher.notify_new_segments(&path).await;
    atomic_rewrite(
        &path,
        &[
            r#"{"id": 1, "text": "one refined", "refined": true}"#,
            r#"{"id": 2, "text": "two refined", "refined": true}"#,
        ],
    );
    watcher.notify_new_segments(&path).await;

    append(&path, r#"{"id": 3, "text": "three"}"#);
    watcher.notify_new_segments(&path).await;

    let frames = drain(&mut rx);
    assert_eq!(frames.iter().map(ids).collect::<Vec<_>>(), [vec![1, 2], vec![3]]);
    assert_eq!(seen(&watcher), [1, 2, 3]);
}

#[tokio::test]
async fn rewrite_of_the_same_size_when_read_then_the_new_inode_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(&path, &[r#"{"id": 1, "text": "aaa"}"#, r#"{"id": 2, "text": "bbb"}"#]);
    let (watcher, mut rx) = watcher(dir.path());
    watcher.notify_new_segments(&path).await;
    let old = watcher.state().tail.inode;

    atomic_rewrite(&path, &[r#"{"id": 1, "text": "AAA"}"#, r#"{"id": 2, "text": "BBB"}"#]);
    watcher.notify_new_segments(&path).await;

    assert_eq!(drain(&mut rx).len(), 1);
    assert_ne!(watcher.state().tail.inode, old);
}

#[tokio::test]
async fn watermark_of_one_meeting_when_the_meeting_switches_then_the_next_file_reads_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let path_a = dir.path().join("a/transcript.jsonl");
    write_jsonl(&path_a, &[r#"{"id": 1, "text": "meeting-a"}"#]);
    let (watcher, mut rx) = watcher(dir.path());
    watcher.notify_new_segments(&path_a).await;
    assert_eq!(seen(&watcher), [1]);

    watcher.set_current_meeting(Some("meeting-b"), None);
    let path_b = dir.path().join("b/transcript.jsonl");
    write_jsonl(&path_b, &[r#"{"id": 1, "text": "meeting-b-row-1"}"#, r#"{"id": 2, "text": "meeting-b-row-2"}"#]);
    watcher.notify_new_segments(&path_b).await;

    let frames = drain(&mut rx);
    assert_eq!(frames.len(), 2);
    assert_eq!(ids(&frames[1]), [1, 2]);
    assert_eq!(seen(&watcher), [1, 2]);
}

#[tokio::test]
async fn watermark_of_meeting_a_when_set_current_meeting_b_then_tail_state_is_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a/transcript.jsonl");
    write_jsonl(&path, &[r#"{"id": 1, "text": "a1"}"#, r#"{"id": 2, "text": "a2"}"#]);
    let (watcher, _rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("meeting-a"), None);
    watcher.notify_new_segments(&path).await;
    assert_eq!(seen(&watcher), [1, 2]);

    watcher.set_current_meeting(Some("meeting-b"), None);

    let state = watcher.state();
    assert_eq!(state.current_meeting_id.as_deref(), Some("meeting-b"));
    assert!(state.tail.seen.is_empty());
    assert_eq!(state.tail.position, 0);
    assert_eq!(state.tail.inode, None);
}

#[tokio::test]
async fn watermark_of_meeting_a_when_set_current_meeting_a_again_then_it_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(&path, &[r#"{"id": 1, "text": "one"}"#]);
    let (watcher, _rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("meeting-a"), None);
    watcher.notify_new_segments(&path).await;
    let before = watcher.state();

    watcher.set_current_meeting(Some("meeting-a"), Some(Value::from("ktalk")));

    assert_eq!(watcher.state(), before);
}

#[tokio::test]
async fn init_payload_seeded_when_a_row_is_appended_then_only_the_new_row_is_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(&path, &[r#"{"id": 1, "text": "a"}"#, r#"{"id": 2, "text": "b"}"#, r#"{"id": 3, "text": "c"}"#]);
    let (watcher, mut rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("meeting-x"), None);
    let known = [Value::Int(1), Value::Int(2), Value::Int(3)];
    watcher.seed_post_init_watermark(known.iter());

    append(&path, r#"{"id": 4, "text": "d"}"#);
    watcher.notify_new_segments(&path).await;

    let frames = drain(&mut rx);
    assert_eq!(frames.len(), 1);
    assert_eq!(ids(&frames[0]), [4]);
    assert_eq!(seen(&watcher), [1, 2, 3, 4]);
}

#[tokio::test]
async fn init_ids_of_mixed_types_when_seeded_then_only_integers_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (watcher, _rx) = watcher(dir.path());

    let known = [Value::Int(1), Value::Null, Value::Int(2), Value::from("x"), Value::Int(3)];
    watcher.seed_post_init_watermark(known.iter());

    assert_eq!(seen(&watcher), [1, 2, 3]);
}

#[tokio::test]
async fn row_without_id_when_read_then_broadcast_but_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    write_jsonl(&path, &[r#"{"id": 1, "text": "has-id"}"#, r#"{"event_type": "session_pause", "message": "paused"}"#]);
    let (watcher, mut rx) = watcher(dir.path());

    watcher.notify_new_segments(&path).await;

    let frames = drain(&mut rx);
    assert_eq!(frames.len(), 1);
    assert!(matches!(support::get(&frames[0], "data"), Value::Array(rows) if rows.len() == 2));
    assert_eq!(seen(&watcher), [1]);
}

fn meeting_meta(base: &Path, meeting_id: &str, source: &str) -> std::path::PathBuf {
    let path = base.join(meeting_id).join("meta.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!(r#"{{"source": "{source}"}}"#)).unwrap();
    path
}

async fn settle() {
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn current_orphan_promoted_to_ktalk_when_checking_source_then_refine_flag_is_resent_once() {
    let dir = tempfile::tempdir().unwrap();
    meeting_meta(dir.path(), "m1", "ktalk");
    let (watcher, mut rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("m1"), Some(Value::from("orphan")));

    watcher.check_source_change("m1").await;
    watcher.check_source_change("m1").await;

    assert_eq!(drain(&mut rx), [obj(r#"{"type": "refine_state", "meeting_id": "m1", "refine_enabled": true}"#)]);
    assert_eq!(watcher.state().current_meeting_source, Some(Value::from("ktalk")));
}

#[tokio::test]
async fn other_or_missing_meeting_when_checking_source_then_nothing_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    meeting_meta(dir.path(), "other", "ktalk");
    let (watcher, mut rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("m1"), Some(Value::from("orphan")));

    watcher.check_source_change("other").await;
    watcher.check_source_change("m1").await;

    assert!(drain(&mut rx).is_empty());
    assert_eq!(watcher.state().current_meeting_source, Some(Value::from("orphan")));
}

#[tokio::test]
async fn meta_of_the_current_meeting_modified_when_watching_then_its_source_change_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let meta = meeting_meta(dir.path(), "m1", "ktalk");
    let other = meeting_meta(dir.path(), "m2", "ktalk");
    let (watcher, mut rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("m1"), Some(Value::from("orphan")));

    watcher.on_modified(meta.parent().unwrap(), true);
    watcher.on_modified(&other, false);
    settle().await;
    assert!(drain(&mut rx).is_empty());

    watcher.on_modified(&meta, false);
    settle().await;
    assert_eq!(drain(&mut rx), [obj(r#"{"type": "refine_state", "meeting_id": "m1", "refine_enabled": true}"#)]);
}

#[tokio::test]
async fn index_modified_when_watching_then_the_new_current_meeting_is_announced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.json"), r#"{"meetings": []}"#).unwrap();
    let m2 = dir.path().join("m2");
    std::fs::create_dir_all(&m2).unwrap();
    std::fs::write(
        m2.join("meta.json"),
        r#"{"id": "m2", "created_at": "2026-11-08T09:00:00+00:00", "source": "ktalk"}"#,
    )
    .unwrap();
    let (watcher, mut rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("m1"), Some(Value::from("orphan")));

    watcher.on_modified(&dir.path().join("index.json"), false);
    settle().await;

    let frames = drain(&mut rx);
    assert_eq!(frames.len(), 1);
    assert_eq!(support::text(&frames[0], "type"), "meeting_change");
    assert_eq!(support::text(support::get(&frames[0], "meeting"), "id"), "m2");
    assert_eq!(support::get(&frames[0], "refine_enabled"), &Value::Bool(true));
    assert_eq!(watcher.current_meeting_id().as_deref(), Some("m2"));
}

#[tokio::test]
async fn transcript_of_the_current_meeting_modified_when_watching_then_its_new_rows_are_sent() {
    let dir = tempfile::tempdir().unwrap();
    let current = dir.path().join("m1/transcript.jsonl");
    let other = dir.path().join("m2/transcript.jsonl");
    for path in [&current, &other] {
        write_jsonl(path, &[r#"{"id": 1, "text": "hello"}"#]);
    }
    let (watcher, mut rx) = watcher(dir.path());
    watcher.set_current_meeting(Some("m1"), Some(Value::from("ktalk")));

    watcher.on_modified(&other, false);
    settle().await;
    assert!(drain(&mut rx).is_empty());

    watcher.on_modified(&current, false);
    settle().await;
    let frames = drain(&mut rx);
    assert_eq!(ids(&frames[0]), [1]);
}

#[tokio::test]
async fn overlapping_active_meeting_reads_when_applied_then_an_older_read_cannot_win() {
    let dir = tempfile::tempdir().unwrap();
    let (watcher, _rx) = watcher(dir.path());
    let storage = || MeetingStorage::new(dir.path(), StorageOptions::new(dir.path())).unwrap();
    let applied = Arc::new(std::sync::Mutex::new(Vec::new()));

    let (first_guard, _) = watcher.read_active_meetings(storage()).await;
    let second = {
        let watcher = Arc::clone(&watcher);
        let applied = Arc::clone(&applied);
        let storage = storage();
        tokio::spawn(async move {
            let (_guard, _) = watcher.read_active_meetings(storage).await;
            applied.lock().unwrap().push("2");
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(applied.lock().unwrap().is_empty(), "the second read ran while the first was applying");
    applied.lock().unwrap().push("1");
    drop(first_guard);
    second.await.unwrap();

    assert_eq!(*applied.lock().unwrap(), ["1", "2"]);
}
