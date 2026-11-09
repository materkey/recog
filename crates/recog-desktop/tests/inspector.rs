//! inspector.rs against scripted peers: the target list as HTTP answers it in each
//! framing, and the WebSocket replies to one evaluation.
#![forbid(unsafe_code)]

use futures_util::{SinkExt as _, StreamExt as _};
use recog_core::pyjson::Value;
use recog_desktop::inspector::{self, Failure};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_tungstenite::tungstenite::Message;

/// A listener answering one request with `response`, then closing the connection.
async fn http_once(response: Vec<u8>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            if stream.read(&mut byte).await.unwrap() == 0 {
                break;
            }
            request.push(byte[0]);
        }
        let _ = stream.write_all(&response).await;
        let _ = stream.shutdown().await;
    });
    port
}

fn other(message: &str) -> Result<Value, Failure> {
    Err(Failure::Other(message.to_owned()))
}

#[tokio::test]
async fn target_lists_when_answered_in_each_framing_then_body_or_failure() {
    // given
    let big = format!("[\"{}\"]", "x".repeat(70_000));
    let cases: Vec<(&str, Vec<u8>, Result<Value, Failure>)> = vec![
        ("sized", b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]".to_vec(), Ok(Value::Array(vec![]))),
        (
            "chunked",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n[]\r\n0\r\n\r\n".to_vec(),
            Ok(Value::Array(vec![])),
        ),
        ("until closed", b"HTTP/1.1 200 OK\r\n\r\n[1]".to_vec(), Ok(Value::Array(vec![Value::Int(1)]))),
        (
            "server error",
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nerror".to_vec(),
            other("HTTPStatusError: 500"),
        ),
        (
            "short body",
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n[]".to_vec(),
            other("peer closed connection without sending complete message body"),
        ),
        (
            "short chunks",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n[]".to_vec(),
            other("incomplete chunked read"),
        ),
        (
            "bad chunk size",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n[]\r\n".to_vec(),
            other("invalid chunk size"),
        ),
        ("bad length", b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n[]".to_vec(), other("invalid content-length")),
        ("no answer", b"HTTP/1.1 200 OK\r\n".to_vec(), other("server disconnected without sending a response")),
        (
            "oversized",
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{big}", big.len()).into_bytes(),
            Err(Failure::attach("invalid_inspector_response")),
        ),
        (
            "oversized chunk",
            format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{big}\r\n0\r\n\r\n", big.len())
                .into_bytes(),
            Err(Failure::attach("invalid_inspector_response")),
        ),
        (
            "oversized until closed",
            format!("HTTP/1.1 200 OK\r\n\r\n{big}").into_bytes(),
            Err(Failure::attach("invalid_inspector_response")),
        ),
    ];
    for (name, response, expected) in cases {
        let port = http_once(response).await;
        // when
        let result = inspector::json_list(port).await;
        // then
        assert_eq!(result, expected, "{name}");
    }
}

#[tokio::test]
async fn target_list_not_json_or_nobody_listening_when_requested_then_other_failure() {
    // given
    let port = http_once(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_vec()).await;
    // when
    let result = inspector::json_list(port).await;
    // then
    assert_eq!(result, other("Expecting value: line 1 column 1 (char 0)"));
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert!(matches!(inspector::json_list(closed).await, Err(Failure::Other(_))));
}

/// A WebSocket peer that reads the evaluation and answers with `replies`, then
/// closes the connection (`close`) or drops it.
async fn ws_once(replies: Vec<Message>, close: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let _ = socket.next().await;
        for reply in replies {
            let _ = socket.send(reply).await;
        }
        if close {
            let _ = socket.close(None).await;
            while let Some(Ok(_)) = socket.next().await {}
        }
    });
    format!("ws://127.0.0.1:{port}/session")
}

const VALUE: &str = r#"{"id": 1, "result": {"result": {"value": 7}}}"#;

#[tokio::test]
async fn replies_in_each_frame_kind_when_evaluating_then_value_or_failure() {
    // given
    type Case = (&'static str, Vec<Message>, bool, Result<Value, Failure>);
    let cases: Vec<Case> = vec![
        ("text", vec![Message::text(VALUE)], false, Ok(Value::Int(7))),
        ("binary", vec![Message::binary(VALUE.as_bytes().to_vec())], false, Ok(Value::Int(7))),
        ("after a ping", vec![Message::Ping(vec![1].into()), Message::text(VALUE)], false, Ok(Value::Int(7))),
        ("closed", vec![], true, other("ConnectionClosed")),
        ("an empty error", vec![Message::text(r#"{"id": 1, "error": {}}"#)], false, Ok(Value::Null)),
    ];
    for (name, replies, close, expected) in cases {
        let url = ws_once(replies, close).await;
        // when
        let result = inspector::evaluate(&url, "1 + 1").await;
        // then
        assert_eq!(result, expected, "{name}");
    }
}

#[tokio::test]
async fn undecodable_or_dropped_replies_when_evaluating_then_other_failures() {
    // given
    let binary = ws_once(vec![Message::binary(b"not json".to_vec())], false).await;
    let dropped = ws_once(vec![], false).await;
    // when
    let undecodable = inspector::evaluate(&binary, "1").await;
    let gone = inspector::evaluate(&dropped, "1").await;
    // then
    assert!(matches!(&undecodable, Err(Failure::Other(m)) if m.starts_with("JSONDecodeError: ")), "{undecodable:?}");
    assert!(matches!(gone, Err(Failure::Other(_))), "{gone:?}");
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let refused = inspector::evaluate(&format!("ws://127.0.0.1:{closed}/x"), "1").await;
    assert!(matches!(refused, Err(Failure::Other(_))), "{refused:?}");
}
