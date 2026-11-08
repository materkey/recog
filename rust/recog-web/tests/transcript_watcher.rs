//! The cases of tests/test_frontend_transcript_watcher.py: the per-meeting watcher's
//! rewrite detection and the release of its state with the last client.

mod support;

use std::path::{Path, PathBuf};

use recog_core::pyjson::Value;
use recog_web::transcript_watcher::TranscriptWatcher;
use recog_web::util::Client;
use support::obj;
use tokio::sync::mpsc::UnboundedReceiver;

fn transcript(dir: &Path, meeting: &str, rows: &[&str]) -> PathBuf {
    let path = dir.join(meeting).join("transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    path
}

fn rewrite(path: &Path, rows: &[&str]) {
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn drain(rx: &mut UnboundedReceiver<String>) -> Vec<Vec<i128>> {
    let mut frames = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        let Value::Array(rows) = support::get(&obj(&frame), "data").clone() else { panic!("{frame}") };
        frames.push(
            rows.iter()
                .filter_map(|r| if let Value::Int(n) = support::get(r, "id") { Some(*n) } else { None })
                .collect(),
        );
    }
    frames
}

#[tokio::test]
async fn segments_seen_when_the_transcript_is_rewritten_atomically_then_nothing_is_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = transcript(dir.path(), "m-2026-04-24", &[r#"{"id": 1, "text": "one"}"#, r#"{"id": 2, "text": "two"}"#]);
    let watcher = TranscriptWatcher::new();
    let (client, mut rx) = Client::new();
    watcher.register("m-2026-04-24", client);
    watcher.notify_clients("m-2026-04-24", &path).await;
    assert_eq!(drain(&mut rx), [vec![1, 2]]);

    rewrite(&path, &[r#"{"id": 1, "text": "one refined"}"#, r#"{"id": 2, "text": "two refined"}"#]);
    watcher.notify_clients("m-2026-04-24", &path).await;

    assert!(drain(&mut rx).is_empty());
}

#[tokio::test]
async fn rewrite_followed_by_an_append_when_read_then_only_the_new_segment_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let path = transcript(dir.path(), "m", &[r#"{"id": 1, "text": "one"}"#, r#"{"id": 2, "text": "two"}"#]);
    let watcher = TranscriptWatcher::new();
    let (client, mut rx) = Client::new();
    watcher.register("m", client);
    watcher.notify_clients("m", &path).await;
    rewrite(&path, &[r#"{"id": 1, "text": "ONE"}"#, r#"{"id": 2, "text": "TWO"}"#, r#"{"id": 3, "text": "three"}"#]);

    watcher.notify_clients("m", &path).await;

    assert_eq!(drain(&mut rx), [vec![1, 2], vec![3]]);
}

#[tokio::test]
async fn rewrite_of_the_same_size_when_read_then_the_new_inode_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = transcript(dir.path(), "m", &[r#"{"id": 1, "text": "aaa"}"#, r#"{"id": 2, "text": "bbb"}"#]);
    let watcher = TranscriptWatcher::new();
    let (client, mut rx) = Client::new();
    watcher.register("m", client);
    watcher.notify_clients("m", &path).await;
    let old = watcher.tail(&path).unwrap().inode;

    rewrite(&path, &[r#"{"id": 1, "text": "AAA"}"#, r#"{"id": 2, "text": "BBB"}"#]);
    watcher.notify_clients("m", &path).await;

    assert_eq!(drain(&mut rx).len(), 1);
    assert_ne!(watcher.tail(&path).unwrap().inode, old);
}

#[tokio::test]
async fn state_of_two_meetings_when_the_last_client_of_one_leaves_then_only_its_state_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path_a = transcript(dir.path(), "m-A", &[r#"{"id": 1, "text": "a"}"#]);
    let path_b = transcript(dir.path(), "m-B", &[r#"{"id": 1, "text": "b"}"#]);
    let watcher = TranscriptWatcher::new();
    let ((a, _rx_a), (b, _rx_b)) = (Client::new(), Client::new());
    watcher.register("m-A", a.clone());
    watcher.register("m-B", b);
    watcher.notify_clients("m-A", &path_a).await;
    watcher.notify_clients("m-B", &path_b).await;
    assert!(watcher.tail(&path_a).is_some() && watcher.tail(&path_b).is_some());

    watcher.unregister("m-A", &a);

    assert!(watcher.tail(&path_a).is_none());
    let b_state = watcher.tail(&path_b).unwrap();
    assert!(b_state.position > 0 && b_state.inode.is_some() && !b_state.seen.is_empty());
}

#[tokio::test]
async fn two_clients_of_a_meeting_when_one_leaves_then_the_state_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = transcript(dir.path(), "m-two-clients", &[r#"{"id": 1, "text": "a"}"#]);
    let watcher = TranscriptWatcher::new();
    let ((one, _rx1), (two, _rx2)) = (Client::new(), Client::new());
    watcher.register("m-two-clients", one.clone());
    watcher.register("m-two-clients", two);
    watcher.notify_clients("m-two-clients", &path).await;

    watcher.unregister("m-two-clients", &one);

    let state = watcher.tail(&path).unwrap();
    assert!(state.position > 0 && state.inode.is_some() && !state.seen.is_empty());
}
