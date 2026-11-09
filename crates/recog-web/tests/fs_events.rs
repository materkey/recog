//! File system events reaching the watchers: the observer of the lifespan hands a
//! new meeting to the catalog without waiting for its reconciliation, and the
//! per-meeting watcher reads a transcript it is told changed, for its clients only.

mod support;

use std::time::{Duration, Instant};

use recog_core::pyjson::Value;
use recog_web::transcript_watcher::TranscriptWatcher;
use recog_web::util::Client;
use support::{App, text, write};

fn listed(body: &Value) -> Vec<String> {
    match body {
        Value::Array(rows) => rows.iter().map(|r| text(r, "id")).collect(),
        _ => Vec::new(),
    }
}

async fn until_listed(app: &App, wanted: bool) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ids = listed(&app.json("GET", "/api/meetings", None).await.1);
        if ids.contains(&"new".to_owned()) == wanted || Instant::now() > deadline {
            return ids;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meeting_written_while_serving_when_listed_then_the_catalog_has_it_before_reconciling() {
    let app = App::new();
    std::fs::create_dir_all(app.meetings()).unwrap();
    let services = recog_web::server::start_services(&app.state).await;
    let started = Instant::now();

    write(
        &app.meetings().join("new/meta.json"),
        r#"{"id": "new", "created_at": "2026-11-09T10:00:00+00:00", "stats": {"segment_count": 1}}"#,
    );
    write(&app.meetings().join("new/transcript.jsonl"), "{\"id\": 1, \"text\": \"hi\"}\n");

    assert!(until_listed(&app, true).await.contains(&"new".to_owned()));
    // The catalog reconciles every 30 s: only the file event can have brought it.
    assert!(started.elapsed() < Duration::from_secs(20));

    std::fs::remove_dir_all(app.meetings().join("new")).unwrap();
    assert!(!until_listed(&app, false).await.contains(&"new".to_owned()));
    recog_web::server::stop_services(&app.state, services).await;
}

#[tokio::test]
async fn changed_transcript_of_a_watched_meeting_when_reported_then_its_new_rows_reach_the_client() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m/transcript.jsonl");
    write(&path, "{\"id\": 1, \"text\": \"one\"}\n");
    let watcher = std::sync::Arc::new(TranscriptWatcher::new());
    let (client, mut rx) = Client::new();
    watcher.register("m", client);

    watcher.on_modified(&path, false);

    let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap();
    assert!(frame.contains("\"one\""), "{frame}");
}

#[tokio::test]
async fn other_paths_or_meetings_without_clients_when_reported_then_nothing_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m/transcript.jsonl");
    write(&path, "{\"id\": 1, \"text\": \"one\"}\n");
    write(&dir.path().join("other/transcript.jsonl"), "{\"id\": 1, \"text\": \"x\"}\n");
    let watcher = std::sync::Arc::new(TranscriptWatcher::new());
    let (client, mut rx) = Client::new();
    watcher.register("m", client);

    watcher.on_modified(&path, true);
    watcher.on_modified(&dir.path().join("m/meta.json"), false);
    watcher.on_modified(&dir.path().join("other/transcript.jsonl"), false);
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(rx.try_recv().is_err());
    assert!(watcher.tail(&dir.path().join("other/transcript.jsonl")).is_none());
    assert!(watcher.tail(&path).is_none());
}
