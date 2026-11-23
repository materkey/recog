//! The Codex app-server client against a scripted server over an in-process pipe. The
//! messages have the shapes codex-cli 0.171.0 sent in the spike of
//! docs/plans/2026-11-23-codex-copilot.md (Task 1), trimmed to the fields that matter.

use recog_copilot::codex::app_server::{AppServer, Error, Event, Failure, TurnStatus};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream, ReadHalf, WriteHalf};

type Client = AppServer<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

/// The server's end: reads what the client sent, writes what it answers.
struct Server {
    lines: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
}

impl Server {
    /// The next message from the client, which must call `method`.
    async fn expect(&mut self, method: &str) -> Value {
        let line = self.lines.next_line().await.unwrap().expect("client closed");
        let message: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(message["method"], method, "{message}");
        message
    }

    async fn next(&mut self) -> Value {
        serde_json::from_str(&self.lines.next_line().await.unwrap().expect("client closed")).unwrap()
    }

    async fn send(&mut self, message: Value) {
        self.writer.write_all(format!("{message}\n").as_bytes()).await.unwrap();
    }

    async fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({"id": request["id"], "result": result})).await;
    }
}

fn pair() -> (Client, Server) {
    let (client, server) = tokio::io::duplex(1 << 16);
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    (
        AppServer::new(client_read, client_write),
        Server { lines: BufReader::new(server_read).lines(), writer: server_write },
    )
}

const THREAD: &str = "01a208d8-793b-7de1-b0bd-30a5a4ec449b";
const TURN: &str = "01a208d8-7b3a-7240-a01c-115f9cfa7940";

fn turn(status: &str, error: Value) -> Value {
    json!({"id": TURN, "rootTurnId": TURN, "items": [], "status": status, "error": error})
}

fn agent_message(text: &str, phase: &str) -> Value {
    json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "id": "msg_1", "text": text,
        "phase": phase}, "threadId": THREAD, "turnId": TURN}})
}

#[tokio::test]
async fn new_session_when_set_up_then_initialize_account_and_thread_in_order_with_matching_ids() {
    let (mut client, mut server) = pair();
    let script = tokio::spawn(async move {
        let init = server.expect("initialize").await;
        assert_eq!(init["params"]["clientInfo"]["name"], "recog-copilot");
        server.reply(&init, json!({"userAgent": "codex", "codexHome": "/h", "platformOs": "macos"})).await;
        let account = server.expect("account/read").await;
        assert_ne!(account["id"], init["id"]);
        server
            .reply(&account, json!({"account": {"type": "chatgpt", "planType": "pro"}, "requiresOpenaiAuth": true}))
            .await;
        let start = server.expect("thread/start").await;
        assert_eq!(start["params"]["approvalPolicy"], "never");
        server.send(json!({"method": "mcpServer/startupStatus/updated", "params": {"name": "copilot"}})).await;
        server.send(json!({"method": "thread/started", "params": {"thread": {"id": THREAD}}})).await;
        server.reply(&start, json!({"thread": {"id": THREAD}, "model": "m", "instructionSources": []})).await;
    });

    client.initialize().await.unwrap();
    assert!(client.logged_in().await.unwrap());
    let thread = client.start_thread(json!({"approvalPolicy": "never"})).await.unwrap();

    assert_eq!(thread, THREAD);
    script.await.unwrap();
}

#[tokio::test]
async fn no_login_when_account_read_then_not_logged_in() {
    let (mut client, mut server) = pair();
    let script = tokio::spawn(async move {
        let account = server.expect("account/read").await;
        server.reply(&account, json!({"account": null, "requiresOpenaiAuth": true})).await;
    });

    assert!(!client.logged_in().await.unwrap());
    script.await.unwrap();
}

#[tokio::test]
async fn turn_when_answered_then_started_messages_and_completion_in_order() {
    let (mut client, mut server) = pair();
    let script = tokio::spawn(async move {
        let start = server.expect("turn/start").await;
        assert_eq!(start["params"], json!({"threadId": THREAD, "input": [{"type": "text", "text": "событие"}]}));
        server.reply(&start, json!({"turn": turn("inProgress", Value::Null)})).await;
        server.send(json!({"method": "turn/started", "params": {"threadId": THREAD, "turn": turn("inProgress", Value::Null)}})).await;
        server.send(json!({"method": "item/agentMessage/delta", "params": {"delta": "Отв"}})).await;
        server.send(agent_message("Отвечаю в чате.", "final_answer")).await;
        server.send(json!({"method": "turn/completed", "params": {"threadId": THREAD, "turn": turn("completed", Value::Null)}})).await;
    });

    assert_eq!(client.start_turn(THREAD, "событие").await.unwrap(), TURN);
    assert_eq!(client.next_event().await.unwrap(), Some(Event::TurnStarted { turn_id: TURN.into() }));
    assert_eq!(client.next_event().await.unwrap(), Some(Event::AgentMessage { text: "Отвечаю в чате.".into() }));
    assert_eq!(
        client.next_event().await.unwrap(),
        Some(Event::TurnCompleted { turn_id: TURN.into(), status: TurnStatus::Completed, error: None })
    );
    script.await.unwrap();
}

#[tokio::test]
async fn spent_subscription_when_turn_fails_then_error_and_failed_turn_name_the_cause() {
    let (mut client, mut server) = pair();
    let limit = json!({"message": "You've hit your usage limit.", "codexErrorInfo": "usageLimitExceeded"});
    let failed = limit.clone();
    let script = tokio::spawn(async move {
        server
            .send(json!({"method": "error", "params": {"error": limit, "willRetry": false, "threadId": THREAD}}))
            .await;
        server
            .send(json!({"method": "turn/completed", "params": {"threadId": THREAD, "turn": turn("failed", failed)}}))
            .await;
        let http = json!({"message": "502", "codexErrorInfo": {"httpConnectionFailed": {"httpStatusCode": 502}}});
        server.send(json!({"method": "error", "params": {"error": http, "willRetry": true}})).await;
    });

    let usage = Failure { info: Some("usageLimitExceeded".into()), message: "You've hit your usage limit.".into() };
    assert_eq!(client.next_event().await.unwrap(), Some(Event::Error { failure: usage.clone(), will_retry: false }));
    assert_eq!(
        client.next_event().await.unwrap(),
        Some(Event::TurnCompleted { turn_id: TURN.into(), status: TurnStatus::Failed, error: Some(usage) })
    );
    let http = Failure { info: Some("httpConnectionFailed".into()), message: "502".into() };
    assert_eq!(client.next_event().await.unwrap(), Some(Event::Error { failure: http, will_retry: true }));
    script.await.unwrap();
}

#[tokio::test]
async fn approval_request_when_server_asks_then_refused_and_reported() {
    let (mut client, mut server) = pair();
    let script = tokio::spawn(async move {
        server.send(json!({"id": "req-7", "method": "item/commandExecution/requestApproval", "params": {}})).await;
        let answer = server.next().await;
        assert_eq!(answer["id"], "req-7");
        assert!(answer["error"]["message"].as_str().unwrap().contains("approves nothing"), "{answer}");
        assert!(answer.get("result").is_none());
    });

    assert_eq!(
        client.next_event().await.unwrap(),
        Some(Event::Refused { method: "item/commandExecution/requestApproval".into() })
    );
    script.await.unwrap();
}

#[tokio::test]
async fn error_reply_when_requested_then_the_servers_error() {
    let (mut client, mut server) = pair();
    let script = tokio::spawn(async move {
        let init = server.expect("initialize").await;
        server.send(json!({"id": init["id"], "error": {"code": -32600, "message": "Already initialized"}})).await;
    });

    let error = client.initialize().await.unwrap_err();
    assert!(matches!(&error, Error::Rpc { code: -32600, message } if message == "Already initialized"), "{error}");
    script.await.unwrap();
}

#[tokio::test]
async fn server_gone_when_waiting_then_no_more_events_and_requests_fail_as_closed() {
    let (mut client, server) = pair();
    drop(server);

    assert_eq!(client.next_event().await.unwrap(), None);
    assert!(matches!(client.start_turn(THREAD, "x").await, Err(Error::Closed)));
}

#[tokio::test]
async fn garbled_line_when_read_then_protocol_error() {
    let (mut client, mut server) = pair();
    server.writer.write_all(b"not json\n").await.unwrap();

    assert!(matches!(client.next_event().await, Err(Error::Protocol(_))));
}
