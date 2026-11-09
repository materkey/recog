//! The cases of tests/test_frontend_copilot_proxy.py: the inbound whitelist and relay of
//! the live stream, the `bridge_disconnected` replies, the agent-state primer, the
//! shared dialog history, and the Origin gate. The lifespan wiring of the bridge
//! subscriber is in bridge_link.rs.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use recog_core::pyjson::Value;
use recog_web::bridge_subscriber::RawSender;
use recog_web::live_stream::handle_client_frame;
use recog_web::server::COPILOT_DIALOG_LIMIT;
use recog_web::util::{BoxFuture, Client};
use support::{App, get, obj, text};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

/// Records `send` calls and answers a scripted result: false for the request IDs in
/// `fail`, or for everything when `fail_all`.
#[derive(Default)]
struct FakeSubscriber {
    sent: Mutex<Vec<String>>,
    fail: Vec<&'static str>,
    fail_all: bool,
}

impl RawSender for FakeSubscriber {
    fn send(&self, raw: String) -> BoxFuture<'_, bool> {
        let request_id = text(&obj(&raw), "request_id");
        self.sent.lock().unwrap().push(raw);
        let ok = !self.fail_all && !self.fail.contains(&request_id.as_str());
        Box::pin(async move { ok })
    }
}

fn agent_state(state: &str) -> String {
    format!(r#"{{"type": "copilot_status", "status": "agent_state", "agent_state": "{state}"}}"#)
}

fn replies() -> (Arc<Mutex<Vec<String>>>, impl Fn(String) -> bool + Sync) {
    let replies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&replies);
    (replies, move |raw: String| {
        sink.lock().unwrap().push(raw);
        true
    })
}

fn subscribe(app: &App, subscriber: FakeSubscriber) -> Arc<FakeSubscriber> {
    let subscriber = Arc::new(subscriber);
    app.state.set_subscriber(Some(Arc::clone(&subscriber) as Arc<dyn RawSender>));
    subscriber
}

#[tokio::test]
async fn whitelisted_frames_when_the_subscriber_sends_then_relayed_verbatim_without_reply() {
    for kind in ["copilot_chat_message", "copilot_spawn_request", "copilot_stop_request"] {
        let app = App::new();
        let subscriber = subscribe(&app, FakeSubscriber::default());
        let (replies, reply) = replies();
        let raw = format!(r#"{{"type": "{kind}", "request_id": "r1", "text": "hi"}}"#);

        handle_client_frame(&app.state, &raw, &reply).await;

        assert_eq!(*subscriber.sent.lock().unwrap(), [raw]);
        assert!(replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn other_or_malformed_frames_when_handled_then_dropped_without_reply_or_panic() {
    let deep = format!("{}0{}", "[".repeat(2000), "]".repeat(2000));
    for raw in [
        r#"{"type": "transcript", "text": "x"}"#,
        r#"{"type": "no_such_type"}"#,
        "not json at all {",
        "[1, 2, 3]",
        r#""just a string""#,
        r#"{"type": 5}"#,
        r#"{"type": ["copilot_chat_message"]}"#,
        r#"{"no_type": true}"#,
        &deep,
    ] {
        let app = App::new();
        let subscriber = subscribe(&app, FakeSubscriber::default());
        let (replies, reply) = replies();

        handle_client_frame(&app.state, raw, &reply).await;

        assert!(subscriber.sent.lock().unwrap().is_empty(), "{raw}");
        assert!(replies.lock().unwrap().is_empty(), "{raw}");
    }
}

#[tokio::test]
async fn failed_send_when_a_whitelisted_frame_arrives_then_bridge_disconnected_echoes_its_request() {
    for kind in ["copilot_chat_message", "copilot_spawn_request", "copilot_stop_request"] {
        let app = App::new();
        let subscriber = subscribe(&app, FakeSubscriber { fail_all: true, ..Default::default() });
        let (replies, reply) = replies();
        let raw = format!(r#"{{"type": "{kind}", "request_id": "r1"}}"#);

        handle_client_frame(&app.state, &raw, &reply).await;

        assert_eq!(*subscriber.sent.lock().unwrap(), [raw]);
        assert_eq!(
            replies.lock().unwrap().iter().map(|r| obj(r)).collect::<Vec<_>>(),
            [obj(r#"{"type": "copilot_status", "status": "bridge_disconnected", "request_id": "r1"}"#)]
        );
    }
}

#[tokio::test]
async fn no_subscriber_when_a_whitelisted_frame_arrives_then_the_same_reply_as_a_failed_send() {
    let app = App::new();
    let (replies, reply) = replies();

    handle_client_frame(&app.state, r#"{"type": "copilot_chat_message", "request_id": "r2"}"#, &reply).await;

    let reply = obj(&replies.lock().unwrap()[0]);
    assert_eq!((text(&reply, "status"), text(&reply, "request_id")), ("bridge_disconnected".into(), "r2".into()));
}

#[tokio::test]
async fn frame_without_request_id_when_the_send_fails_then_the_reply_carries_null() {
    let app = App::new();
    subscribe(&app, FakeSubscriber { fail_all: true, ..Default::default() });
    let (replies, reply) = replies();

    handle_client_frame(&app.state, r#"{"type": "copilot_spawn_request"}"#, &reply).await;

    assert_eq!(get(&obj(&replies.lock().unwrap()[0]), "request_id"), &Value::Null);
}

#[tokio::test]
async fn originator_gone_when_the_reply_fails_then_the_frame_was_tried_once_and_nothing_breaks() {
    let app = App::new();
    let subscriber = subscribe(&app, FakeSubscriber { fail_all: true, ..Default::default() });
    let attempts = Mutex::new(0);
    let dead = |_: String| {
        *attempts.lock().unwrap() += 1;
        false
    };

    handle_client_frame(&app.state, r#"{"type": "copilot_chat_message", "request_id": "r3"}"#, &dead).await;

    assert_eq!(subscriber.sent.lock().unwrap().len(), 1);
    assert_eq!(*attempts.lock().unwrap(), 1);
}

fn listening(app: &App) -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (client, rx) = Client::new();
    app.state.live_watcher.register(client);
    rx
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        out.push(frame);
    }
    out
}

#[tokio::test]
async fn agent_state_frame_when_relayed_then_cached_as_primer_and_still_forwarded() {
    let app = App::new();
    let mut rx = listening(&app);

    app.state.on_bridge_frame(agent_state("online")).await.unwrap();

    assert_eq!(app.state.agent_state_primer(), Some(agent_state("online")));
    assert_eq!(drain(&mut rx), [agent_state("online")]);
}

#[tokio::test]
async fn cached_primer_when_other_frames_pass_then_it_is_kept() {
    let app = App::new();
    app.state.on_bridge_frame(agent_state("working")).await.unwrap();

    for other in [
        r#"{"type": "copilot_reply", "request_id": "r1", "text": "hi"}"#,
        r#"{"type": "copilot_status", "status": "queued", "request_id": "r1"}"#,
        r#"{"type": "transcript_refine", "segments": []}"#,
    ] {
        app.state.on_bridge_frame(other.into()).await.unwrap();
    }

    assert_eq!(app.state.agent_state_primer(), Some(agent_state("working")));
}

#[tokio::test]
async fn malformed_bridge_frame_when_relayed_then_the_cache_stays_and_the_frame_is_forwarded() {
    let app = App::new();
    let mut rx = listening(&app);
    app.state.on_bridge_frame(agent_state("online")).await.unwrap();

    app.state.on_bridge_frame("garbage {not json".into()).await.unwrap();

    assert_eq!(app.state.agent_state_primer(), Some(agent_state("online")));
    assert_eq!(drain(&mut rx), [agent_state("online"), "garbage {not json".to_owned()]);
}

#[tokio::test]
async fn cached_primer_when_its_client_is_gone_then_the_broadcast_drops_the_client() {
    let app = App::new();
    let (client, rx) = Client::new();
    app.state.live_watcher.register(client);
    drop(rx);

    app.state.on_bridge_frame(agent_state("online")).await.unwrap();

    assert_eq!(app.state.live_watcher.client_count(), 0);
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16, origins: &[&str]) -> Result<Ws, String> {
    let mut request = format!("ws://127.0.0.1:{port}/api/live/stream").into_client_request().unwrap();
    for origin in origins {
        request.headers_mut().append("origin", origin.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await.map(|(ws, _)| ws).map_err(|e| e.to_string())
}

async fn receive(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.expect("a frame").unwrap().unwrap();
        if let Message::Text(text) = message {
            return obj(text.as_str());
        }
    }
}

async fn send(ws: &mut Ws, raw: &str) {
    ws.send(Message::text(raw.to_owned())).await.unwrap();
}

fn active_meeting(app: &App) {
    support::write(
        &app.meetings().join("m-active/meta.json"),
        r#"{"id": "m-active", "created_at": "2026-11-08T09:00:00+00:00", "source": "ktalk"}"#,
    );
}

#[tokio::test]
async fn cached_primer_when_a_tab_connects_then_init_comes_first_and_the_primer_next() {
    let app = App::new();
    app.state.on_bridge_frame(agent_state("online")).await.unwrap();
    let mut ws = connect(app.serve().await, &[]).await.unwrap();

    assert_eq!(text(&receive(&mut ws).await, "type"), "init");
    assert_eq!(receive(&mut ws).await, obj(&agent_state("online")));
}

#[tokio::test]
async fn empty_cache_when_a_tab_sends_a_probe_then_only_init_precedes_the_reply() {
    let app = App::new();
    let mut ws = connect(app.serve().await, &[]).await.unwrap();
    assert_eq!(text(&receive(&mut ws).await, "type"), "init");

    send(&mut ws, r#"{"type": "copilot_chat_message", "request_id": "probe"}"#).await;

    assert_eq!(
        receive(&mut ws).await,
        obj(r#"{"type": "copilot_status", "status": "bridge_disconnected", "request_id": "probe"}"#)
    );
}

#[tokio::test]
async fn cached_primer_and_no_bridge_when_a_tab_sends_a_frame_then_init_primer_and_reply_in_order() {
    let app = App::new();
    app.state.on_bridge_frame(agent_state("online")).await.unwrap();
    let mut ws = connect(app.serve().await, &[]).await.unwrap();

    assert_eq!(text(&receive(&mut ws).await, "type"), "init");
    assert_eq!(receive(&mut ws).await, obj(&agent_state("online")));
    send(&mut ws, r#"{"type": "copilot_spawn_request", "request_id": "click-1"}"#).await;
    assert_eq!(
        receive(&mut ws).await,
        obj(r#"{"type": "copilot_status", "status": "bridge_disconnected", "request_id": "click-1"}"#)
    );
}

#[tokio::test]
async fn other_frame_then_probe_over_the_endpoint_when_handled_then_only_the_probe_is_answered() {
    let app = App::new();
    let mut ws = connect(app.serve().await, &[]).await.unwrap();
    receive(&mut ws).await;

    send(&mut ws, r#"{"type": "transcript", "request_id": "x"}"#).await;
    send(&mut ws, r#"{"type": "copilot_stop_request", "request_id": "probe-2"}"#).await;

    let reply = receive(&mut ws).await;
    assert_eq!((text(&reply, "status"), text(&reply, "request_id")), ("bridge_disconnected".into(), "probe-2".into()));
}

#[tokio::test]
async fn live_subscriber_when_frames_cross_the_endpoint_then_both_reach_it_and_only_the_failed_one_is_answered() {
    let app = App::new();
    let subscriber = subscribe(&app, FakeSubscriber { fail: vec!["fail-2"], ..Default::default() });
    let mut ws = connect(app.serve().await, &[]).await.unwrap();
    receive(&mut ws).await;
    let first = r#"{"type": "copilot_chat_message", "request_id": "ok-1"}"#;
    let second = r#"{"type": "copilot_chat_message", "request_id": "fail-2"}"#;

    send(&mut ws, first).await;
    send(&mut ws, second).await;

    assert_eq!(
        receive(&mut ws).await,
        obj(r#"{"type": "copilot_status", "status": "bridge_disconnected", "request_id": "fail-2"}"#)
    );
    assert_eq!(*subscriber.sent.lock().unwrap(), [first, second]);
}

#[tokio::test]
async fn manual_spawn_when_the_tab_disconnects_then_the_bridge_gets_no_stop() {
    let app = App::new();
    let subscriber = subscribe(&app, FakeSubscriber::default());
    let mut ws = connect(app.serve().await, &[]).await.unwrap();
    receive(&mut ws).await;
    let frame = r#"{"type": "copilot_spawn_request", "request_id": "pre-f5"}"#;

    send(&mut ws, frame).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    ws.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(*subscriber.sent.lock().unwrap(), [frame]);
    assert_eq!(app.state.live_watcher.client_count(), 0);
}

#[tokio::test]
async fn active_meeting_and_a_cached_primer_when_a_tab_connects_then_the_meeting_init_then_the_primer() {
    let app = App::new();
    active_meeting(&app);
    app.state.on_bridge_frame(agent_state("working")).await.unwrap();
    let mut ws = connect(app.serve().await, &[]).await.unwrap();

    let init = receive(&mut ws).await;
    assert_eq!(text(&init, "type"), "init");
    assert_eq!(text(get(&init, "meeting"), "id"), "m-active");
    assert_eq!(get(&init, "refine_enabled"), &Value::Bool(true));
    assert_eq!(receive(&mut ws).await, obj(&agent_state("working")));
}

#[tokio::test]
async fn disallowed_or_duplicate_origin_when_connecting_then_denied_before_registration() {
    let app = App::new();
    let port = app.serve().await;
    for origins in [vec!["https://evil.example"], vec!["http://127.0.0.1:9080", "https://evil.example"], vec!["null"]] {
        assert!(connect(port, &origins).await.is_err(), "{origins:?}");
        assert_eq!(app.state.live_watcher.client_count(), 0);
    }
}

#[tokio::test]
async fn no_origin_or_the_webapp_origin_when_connecting_then_init_arrives() {
    let app = App::new();
    let port = app.serve().await;
    for origins in [vec![], vec!["http://127.0.0.1:9080"]] {
        let mut ws = connect(port, &origins).await.unwrap();
        assert_eq!(text(&receive(&mut ws).await, "type"), "init");
    }
}

fn dialog_frames() -> [&'static str; 2] {
    [
        r#"{"type": "copilot_chat_echo", "request_id": "r1", "text": "вопрос", "created_at": 1.0}"#,
        r#"{"type": "copilot_reply", "request_id": "r1", "kind": "assistant", "text": "ответ", "created_at": 2.0}"#,
    ]
}

#[tokio::test]
async fn dialog_when_a_new_tab_connects_then_the_history_follows_init() {
    let app = App::new();
    for frame in dialog_frames() {
        app.state.on_bridge_frame(frame.into()).await.unwrap();
    }
    let mut ws = connect(app.serve().await, &[]).await.unwrap();

    assert_eq!(text(&receive(&mut ws).await, "type"), "init");
    let history = receive(&mut ws).await;

    assert_eq!(text(&history, "type"), "copilot_history");
    let Value::Array(frames) = get(&history, "frames") else { panic!() };
    assert_eq!(frames.iter().map(|f| text(f, "text")).collect::<Vec<_>>(), ["вопрос", "ответ"]);
}

#[tokio::test]
async fn no_dialog_when_a_new_tab_connects_then_no_history_frame_follows_the_primer() {
    let app = App::new();
    app.state.on_bridge_frame(agent_state("online")).await.unwrap();
    let mut ws = connect(app.serve().await, &[]).await.unwrap();

    assert_eq!(text(&receive(&mut ws).await, "type"), "init");
    assert_eq!(text(&receive(&mut ws).await, "status"), "agent_state");
    send(&mut ws, r#"{"type": "copilot_chat_message", "request_id": "probe"}"#).await;
    assert_eq!(text(&receive(&mut ws).await, "status"), "bridge_disconnected");
}

#[tokio::test]
async fn dialog_frame_when_forwarded_then_the_live_frame_and_the_history_share_its_id() {
    let app = App::new();
    let mut rx = listening(&app);

    app.state.on_bridge_frame(dialog_frames()[0].into()).await.unwrap();

    let live = obj(&drain(&mut rx)[0]);
    assert_eq!(text(&live, "text"), "вопрос");
    assert_eq!(text(&live, "dialog_id").len(), 32);
    assert_eq!(app.state.copilot.lock().unwrap().dialog.iter().cloned().collect::<Vec<_>>(), [live]);
}

#[tokio::test]
async fn long_dialog_when_the_limit_is_exceeded_then_the_oldest_frames_are_dropped() {
    let app = App::new();

    for index in 0..=COPILOT_DIALOG_LIMIT {
        app.state
            .on_bridge_frame(format!(r#"{{"type": "copilot_reply", "request_id": "r{index}", "text": "{index}"}}"#))
            .await
            .unwrap();
    }

    let relay = app.state.copilot.lock().unwrap();
    assert_eq!(relay.dialog.len(), COPILOT_DIALOG_LIMIT);
    assert_eq!(text(&relay.dialog[0], "text"), "1");
}

#[tokio::test]
async fn status_frame_when_forwarded_then_it_is_not_part_of_the_dialog() {
    let app = App::new();

    app.state.on_bridge_frame(agent_state("online")).await.unwrap();

    assert!(app.state.copilot.lock().unwrap().dialog.is_empty());
}
