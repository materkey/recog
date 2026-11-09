//! The lifespan's link to the bridge: once it comes up the saved user profile and AI
//! settings are replayed to the bridge, and the web UI hears of each change of the
//! link, up and down.

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use recog_web::util::Client;
use support::{App, obj, text};
use tokio_tungstenite::tungstenite::Message;

/// A bridge that takes one connection, records what it receives for a moment, then
/// goes away for good.
async fn one_shot_bridge() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    tokio::spawn(async move {
        let Ok((tcp, _)) = listener.accept().await else { return };
        drop(listener);
        let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else { return };
        let until = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Some(Ok(message))) = tokio::time::timeout_at(until, ws.next()).await {
            if let Message::Text(text) = message {
                sink.lock().unwrap().push(text.as_str().to_owned());
            }
        }
    });
    (port, received)
}

fn types(frames: &[String]) -> Vec<String> {
    frames.iter().map(|f| text(&obj(f), "type")).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_link_up_then_down_when_serving_then_settings_replayed_and_each_change_published() {
    let (port, received) = one_shot_bridge().await;
    let app = App::with(|config| config.bridge_ws_url = format!("ws://127.0.0.1:{port}"));
    let (client, mut rx) = Client::new();
    app.state.live_watcher.register(client);
    let services = recog_web::server::start_services(&app.state).await;

    // One status when the link comes up, one when it goes down (the bridge never
    // accepts again).
    let mut published = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while published.iter().filter(|t: &&String| t.as_str() == "user_profile_status").count() < 2 {
        assert!(Instant::now() < deadline, "published only {published:?}");
        if let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            published.push(text(&obj(&frame), "type"));
        }
    }

    let sent = types(&received.lock().unwrap());
    assert!(sent.contains(&"user_profile_refresh".to_owned()), "{sent:?}");
    assert!(sent.iter().any(|t| t.starts_with("ai_settings")), "{sent:?}");
    let (_, profile) = app.json("GET", "/api/user-profile", None).await;
    assert_eq!(text(&profile, "application_state"), "pending");
    recog_web::server::stop_services(&app.state, services).await;
}
