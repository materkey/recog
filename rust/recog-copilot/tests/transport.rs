//! The MCP endpoint on the wire, step by step as the deleted differential test drove
//! the Python and the Rust channel through one conversation: the HTTP edges before
//! any session, the requests mcp 1.27 refuses before its handler, the methods FastMCP
//! serves, cancellation, the standing GET stream and its pings, and the session's end.
//! The answers are the Python channel's, which that test found identical.
#![forbid(unsafe_code)]

mod channel_support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use channel_support::*;
use http_body_util::{BodyExt as _, Full};
use recog_copilot::channel::transport::McpServer;
use recog_core::pyjson::{Map, Value};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};

const PROTOCOL: (&str, &str) = ("mcp-protocol-version", "2025-06-18");

type Headers<'a> = &'a [(&'a str, &'a str)];

/// An endpoint bound to `host`'s security settings, with a short SSE ping.
async fn endpoint_for(host: &str, ping: Duration) -> (Arc<McpServer>, SocketAddr, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut server = McpServer::new(link_in(&dir), host);
    server.ping_interval = ping;
    let server = Arc::new(server);
    tokio::spawn(server.clone().serve(listener));
    (server, address, dir)
}

async fn endpoint() -> (Arc<McpServer>, SocketAddr, tempfile::TempDir) {
    endpoint_for("127.0.0.1", Duration::from_secs(15)).await
}

/// One request with exactly these headers (no Host unless given) on `path`.
async fn raw(address: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: Bytes) -> HttpResponse {
    let stream = TcpStream::connect(address).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await.unwrap();
    tokio::spawn(connection);
    let mut builder = hyper::Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = sender.send_request(builder.body(Full::new(body)).unwrap()).await.unwrap();
    let status = response.status().as_u16();
    let headers =
        response.headers().iter().map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned())).collect();
    HttpResponse { status, headers, body: response.into_body() }
}

/// A POST of `body` on `session` with the headers a client sends.
async fn rpc(address: SocketAddr, session: &str, body: &serde_json::Value) -> HttpResponse {
    request(address, "POST", &[ACCEPT, JSON, ("mcp-session-id", session), PROTOCOL], body.to_string()).await
}

/// The one event a POST stream carries: the answer to the request.
async fn answer(address: SocketAddr, session: &str, body: &serde_json::Value) -> serde_json::Value {
    let response = rpc(address, session, body).await;
    assert_eq!(response.status, 200, "{body}");
    assert_eq!(response.header("content-type"), Some("text/event-stream"));
    let events = response.events().await;
    assert_eq!(events.len(), 1, "{body}: {events:?}");
    events[0].clone()
}

fn server_error(code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": "server-error", "error": {"code": code, "message": message}}).to_string()
}

fn invalid_params(id: serde_json::Value) -> serde_json::Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "Invalid request parameters", "data": ""}})
}

fn ping(id: i64) -> serde_json::Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "ping"})
}

fn tool_call(id: i64, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

// --- the HTTP edges ---

#[tokio::test]
async fn unknown_session_id_when_posted_then_404_session_not_found() {
    // given
    let (server, address, _dir) = endpoint().await;
    // when
    let response = rpc(address, "0123456789abcdef0123456789abcdef", &json!({})).await;
    // then
    assert_eq!(response.status, 404);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.header("mcp-session-id"), None);
    assert_eq!(response.text().await, server_error(-32600, "Session not found"));
    assert!(server.session_list().is_empty());
}

#[tokio::test]
async fn trailing_slash_when_requested_then_redirected_to_mcp_with_the_query() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let host = address.to_string();
    // when
    let response = raw(address, "GET", "/mcp/?a=1", &[("host", &host)], Bytes::new()).await;
    let bare = raw(address, "POST", "/mcp/", &[("host", &host)], Bytes::new()).await;
    // then
    assert_eq!(response.status, 307);
    assert_eq!(response.header("location"), Some(format!("http://{host}/mcp?a=1").as_str()));
    assert_eq!(response.text().await, "");
    assert_eq!(bare.header("location"), Some(format!("http://{host}/mcp").as_str()));
}

#[tokio::test]
async fn other_path_when_requested_then_plain_not_found() {
    // given
    let (server, address, _dir) = endpoint().await;
    // when
    let response = raw(address, "GET", "/other", &[("host", &address.to_string())], Bytes::new()).await;
    // then
    assert_eq!(response.status, 404);
    assert_eq!(response.header("content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(response.text().await, "Not Found");
    assert!(server.session_list().is_empty());
}

#[tokio::test]
async fn put_when_requested_then_405_allowing_get_post_delete() {
    // given
    let (_server, address, _dir) = endpoint().await;
    // when
    let response = request(address, "PUT", &[], Bytes::new()).await;
    // then
    assert_eq!(response.status, 405);
    assert_eq!(response.header("allow"), Some("GET, POST, DELETE"));
    let session = response.header("mcp-session-id").unwrap().to_owned();
    assert_eq!(session.len(), 32);
    assert_eq!(response.text().await, server_error(-32600, "Method Not Allowed"));
}

#[tokio::test]
async fn post_without_json_content_type_when_sent_then_400_invalid_content_type() {
    // given
    let (_server, address, _dir) = endpoint().await;
    // when
    let response = request(address, "POST", &[ACCEPT], ping(1).to_string()).await;
    // then
    assert_eq!(response.status, 400);
    assert_eq!(response.text().await, "Invalid Content-Type header");
}

#[tokio::test]
async fn foreign_host_or_origin_when_posted_to_loopback_then_dns_rebinding_refused() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let host = address.to_string();
    let body = Bytes::from(ping(1).to_string());
    // when
    let no_host = raw(address, "POST", "/mcp", &[ACCEPT, JSON], body.clone()).await;
    let bad_host = raw(address, "POST", "/mcp", &[ACCEPT, JSON, ("host", "evil.example")], body.clone()).await;
    let bad_origin =
        raw(address, "POST", "/mcp", &[ACCEPT, JSON, ("host", &host), ("origin", "http://evil.example")], body.clone())
            .await;
    let local_origin =
        raw(address, "POST", "/mcp", &[ACCEPT, JSON, ("host", &host), ("origin", "http://localhost:3000")], body).await;
    // then
    assert_eq!(no_host.status, 421);
    assert_eq!(no_host.text().await, "Invalid Host header");
    assert_eq!(bad_host.status, 421);
    assert_eq!(bad_origin.status, 403);
    assert_eq!(bad_origin.text().await, "Invalid Origin header");
    // Through the security check, then refused for the missing session header.
    assert_eq!(local_origin.status, 400);
}

#[tokio::test]
async fn host_not_loopback_when_bound_then_any_host_and_origin_accepted() {
    // given
    let (_server, address, _dir) = endpoint_for("0.0.0.0", Duration::from_secs(15)).await;
    // when
    let response = raw(
        address,
        "POST",
        "/mcp",
        &[ACCEPT, JSON, ("host", "evil.example"), ("origin", "http://evil.example")],
        Bytes::from(ping(1).to_string()),
    )
    .await;
    // then
    assert_eq!(response.status, 400);
    assert_eq!(response.text().await, server_error(-32600, "Bad Request: Missing session ID"));
}

// --- what the POST handler refuses ---

#[tokio::test]
async fn request_without_session_header_when_posted_then_new_session_and_missing_id_error() {
    // given
    let (server, address, _dir) = endpoint().await;
    // when
    let response = request(address, "POST", &[ACCEPT, JSON], ping(1).to_string()).await;
    // then
    assert_eq!(response.status, 400);
    let session = response.header("mcp-session-id").unwrap().to_owned();
    assert_eq!(response.text().await, server_error(-32600, "Bad Request: Missing session ID"));
    assert_eq!(server.session_list().iter().map(|s| s.id.clone()).collect::<Vec<_>>(), [session]);
}

#[tokio::test]
async fn unacceptable_headers_when_posted_then_mcp_status_and_message() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let ping = Bytes::from(ping(1).to_string());
    let sid = ("mcp-session-id", session.as_str());
    let cases: [(Headers, u16, String); 5] = [
        (
            &[("accept", "application/json"), JSON, sid],
            406,
            server_error(-32600, "Not Acceptable: Client must accept both application/json and text/event-stream"),
        ),
        (
            &[("accept", "*/*"), JSON, sid],
            406,
            server_error(-32600, "Not Acceptable: Client must accept both application/json and text/event-stream"),
        ),
        (
            &[ACCEPT, ("content-type", "application/json-x"), sid],
            415,
            server_error(-32600, "Unsupported Media Type: Content-Type must be application/json"),
        ),
        (
            &[ACCEPT, JSON, sid, ("mcp-protocol-version", "2099-01-01")],
            400,
            server_error(
                -32600,
                "Bad Request: Unsupported protocol version: 2099-01-01. Supported versions: 2024-11-05, \
                 2025-03-26, 2025-06-18, 2025-11-25",
            ),
        ),
        (&[ACCEPT, ("content-type", "application/json; charset=utf-8"), sid], 200, String::new()),
    ];
    for (headers, status, body) in cases {
        // when
        let response = request(address, "POST", headers, ping.clone()).await;
        // then
        assert_eq!(response.status, status, "{headers:?}");
        if status == 200 {
            assert_eq!(response.events().await, [json!({"jsonrpc": "2.0", "id": 1, "result": {}})]);
        } else {
            assert_eq!(response.header("mcp-session-id"), Some(session.as_str()));
            assert_eq!(response.text().await, body, "{headers:?}");
        }
    }
}

#[tokio::test]
async fn undecodable_bodies_when_posted_then_parse_and_encoding_errors() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let headers = [ACCEPT, JSON, ("mcp-session-id", session.as_str()), PROTOCOL];
    let cases: [(&[u8], u16, String); 3] = [
        (
            b"{nope",
            400,
            server_error(
                -32700,
                "Parse error: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
        ),
        (b"", 400, server_error(-32700, "Parse error: Expecting value: line 1 column 1 (char 0)")),
        (
            b"\xff",
            500,
            server_error(
                -32603,
                "Error handling POST request: 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte",
            ),
        ),
    ];
    for (body, status, text) in cases {
        // when
        let response = request(address, "POST", &headers, Bytes::from_static(body)).await;
        // then
        assert_eq!(response.status, status, "{body:?}");
        assert_eq!(response.text().await, text, "{body:?}");
    }
}

#[tokio::test]
async fn body_with_utf8_bom_when_posted_then_read_without_it() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let mut body = b"\xef\xbb\xbf".to_vec();
    body.extend(ping(9).to_string().into_bytes());
    // when
    let response =
        request(address, "POST", &[ACCEPT, JSON, ("mcp-session-id", session.as_str()), PROTOCOL], body).await;
    // then
    assert_eq!(response.events().await, [json!({"jsonrpc": "2.0", "id": 9, "result": {}})]);
}

#[tokio::test]
async fn messages_that_are_not_json_rpc_when_posted_then_validation_error() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let headers = [ACCEPT, JSON, ("mcp-session-id", session.as_str()), PROTOCOL];
    let cases = [
        ("{}", "Validation error: validation errors for JSONRPCMessage (input_value={})"),
        ("[1]", "Validation error: 4 validation errors for JSONRPCMessage (input_value=[1], input_type=list)"),
        (
            r#"{"jsonrpc": "1.0", "id": 1, "method": "ping"}"#,
            "Validation error: validation errors for JSONRPCMessage (input_value={'jsonrpc': '1.0', 'id': 1, 'method': 'ping'})",
        ),
        (
            r#"{"jsonrpc": "2.0", "method": "x", "params": 1}"#,
            "Validation error: validation errors for JSONRPCMessage (input_value={'jsonrpc': '2.0', 'method': 'x', 'params': 1})",
        ),
        (
            r#"{"jsonrpc": "2.0", "id": 1, "method": "ping", "params": [1]}"#,
            "Validation error: validation errors for JSONRPCMessage (input_value={'jsonrpc': '2.0', 'id': ...: 'ping', 'params': [1]})",
        ),
        (
            r#"{"jsonrpc": "2.0", "id": 1, "result": 5}"#,
            "Validation error: validation errors for JSONRPCMessage (input_value={'jsonrpc': '2.0', 'id': 1, 'result': 5})",
        ),
        (
            r#"{"jsonrpc": "2.0", "error": {"code": "x"}}"#,
            "Validation error: validation errors for JSONRPCMessage (input_value={'jsonrpc': '2.0', 'error': {'code': 'x'}})",
        ),
        (
            r#"{"jsonrpc": "2.0", "error": {"code": 1}}"#,
            "Validation error: validation errors for JSONRPCMessage (input_value={'jsonrpc': '2.0', 'error': {'code': 1}})",
        ),
    ];
    for (body, message) in cases {
        // when
        let response = request(address, "POST", &headers, body).await;
        // then
        assert_eq!(response.status, 400, "{body}");
        assert_eq!(response.text().await, server_error(-32602, message), "{body}");
    }
}

#[tokio::test]
async fn responses_and_notifications_when_posted_then_202_without_body() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let cases = [
        json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
        json!({"jsonrpc": "2.0", "id": "x", "error": {"code": 1, "message": "m"}}),
        json!({"jsonrpc": "2.0", "error": {"code": 1, "message": "m"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/whatever"}),
        json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progressToken": 1, "progress": 1}}),
        json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 1.5, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": true, "method": "ping"}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 999}}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": [1]}}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled"}),
    ];
    for body in cases {
        // when
        let response = rpc(address, &session, &body).await;
        // then
        assert_eq!(response.status, 202, "{body}");
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.header("mcp-session-id"), Some(session.as_str()));
        assert_eq!(response.text().await, "", "{body}");
    }
}

// --- the requests before the handler ---

#[tokio::test]
async fn session_not_initialized_when_requests_arrive_then_only_ping_answered() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let created = request(address, "POST", &[ACCEPT, JSON], ping(1).to_string()).await;
    let early = created.header("mcp-session-id").unwrap().to_owned();
    // when
    let list = answer(address, &early, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await;
    let pong = answer(address, &early, &ping(3)).await;
    let named = answer(address, &early, &json!({"jsonrpc": "2.0", "id": "s", "method": "ping"})).await;
    // then
    assert_eq!(list, invalid_params(json!(2)));
    assert_eq!(pong, json!({"jsonrpc": "2.0", "id": 3, "result": {}}));
    assert_eq!(named, json!({"jsonrpc": "2.0", "id": "s", "result": {}}));
}

#[tokio::test]
async fn early_session_when_initialized_late_then_served_like_any_other() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let created = request(address, "POST", &[ACCEPT, JSON], ping(1).to_string()).await;
    let early = created.header("mcp-session-id").unwrap().to_owned();
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "late", "version": "1"}}});
    // when
    let initialized = answer(address, &early, &init).await;
    let list = answer(address, &early, &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"})).await;
    // then
    assert_eq!(initialized["result"]["protocolVersion"], json!("2025-06-18"));
    let names: Vec<&str> =
        list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["send_copilot_message", "set_copilot_status", "get_recent_transcript", "await_copilot_event"]);
}

#[tokio::test]
async fn initialize_when_answered_then_fastmcp_server_info_and_channel_capability() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}});
    // when
    let response = request(address, "POST", &[ACCEPT, JSON], init.to_string()).await;
    // then
    assert_eq!(response.status, 200);
    assert_eq!(response.header("cache-control"), Some("no-cache, no-transform"));
    assert_eq!(response.header("x-accel-buffering"), Some("no"));
    assert_eq!(response.header("mcp-session-id").map(str::len), Some(32));
    let result = &response.events().await[0]["result"];
    assert_eq!(result["protocolVersion"], json!("2025-03-26"));
    assert_eq!(result["serverInfo"], json!({"name": "recog-copilot-channel", "version": "1.27.0"}));
    assert_eq!(result["capabilities"]["experimental"], json!({"claude/channel": {}}));
    assert_eq!(result["capabilities"]["tools"], json!({"listChanged": false}));
    assert!(result["instructions"].as_str().unwrap().starts_with("KTalk Copilot channel:"));
}

#[tokio::test]
async fn each_method_when_requested_after_initialize_then_fastmcp_answer() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let method_not_found =
        |id: i64| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}});
    let cases = [
        ("resources/list", json!(null), json!({"jsonrpc": "2.0", "id": 100, "result": {"resources": []}})),
        ("prompts/list", json!(null), json!({"jsonrpc": "2.0", "id": 101, "result": {"prompts": []}})),
        (
            "resources/templates/list",
            json!(null),
            json!({"jsonrpc": "2.0", "id": 102, "result": {"resourceTemplates": []}}),
        ),
        ("ping", json!(null), json!({"jsonrpc": "2.0", "id": 103, "result": {}})),
        (
            "prompts/get",
            json!({"name": "x"}),
            json!({"jsonrpc": "2.0", "id": 104, "error": {"code": 0, "message": "Unknown prompt: x"}}),
        ),
        (
            "resources/read",
            json!({"uri": "file:///x"}),
            json!({"jsonrpc": "2.0", "id": 105, "error": {"code": 0, "message": "Unknown resource: file:///x"}}),
        ),
        ("resources/read", json!({"uri": "no-scheme"}), invalid_params(json!(106))),
        ("resources/read", json!({"uri": ":x"}), invalid_params(json!(107))),
        ("prompts/get", json!({}), invalid_params(json!(108))),
        (
            "completion/complete",
            json!({"ref": {"type": "ref/prompt", "name": "x"}, "argument": {"name": "a", "value": "b"}}),
            method_not_found(109),
        ),
        ("logging/setLevel", json!({"level": "info"}), method_not_found(110)),
        ("resources/subscribe", json!({"uri": "file:///x"}), method_not_found(111)),
        ("tasks/list", json!(null), method_not_found(112)),
        ("foo/bar", json!(null), invalid_params(json!(113))),
        ("tools/call", json!({}), invalid_params(json!(114))),
        ("tools/call", json!({"name": "send_copilot_message", "arguments": [1]}), invalid_params(json!(115))),
        ("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}}), invalid_params(json!(116))),
        (
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": [], "clientInfo": {"name": "t", "version": "1"}}),
            invalid_params(json!(117)),
        ),
        (
            "initialize",
            json!({"protocolVersion": 5, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
            invalid_params(json!(118)),
        ),
        (
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t"}}),
            invalid_params(json!(119)),
        ),
    ];
    for (method, params, expected) in cases {
        let mut body = json!({"jsonrpc": "2.0", "id": expected["id"], "method": method});
        if !params.is_null() {
            body["params"] = params.clone();
        }
        // when
        let got = answer(address, &session, &body).await;
        // then
        assert_eq!(got, expected, "{method} {params}");
    }
}

#[tokio::test]
async fn repeated_initialize_when_version_unknown_then_latest_offered_and_session_kept() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let again = |id: i64, version: &str| {
        json!({"jsonrpc": "2.0", "id": id, "method": "initialize", "params": {
            "protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})
    };
    // when
    let old = answer(address, &session, &again(120, "1999-01-01")).await;
    let future = answer(address, &session, &again(121, "2026-07-28")).await;
    let pong = answer(address, &session, &ping(122)).await;
    // then
    assert_eq!(old["result"]["protocolVersion"], json!("2025-11-25"));
    assert_eq!(future["result"]["protocolVersion"], json!("2025-11-25"));
    assert_eq!(pong, json!({"jsonrpc": "2.0", "id": 122, "result": {}}));
}

#[tokio::test]
async fn tool_calls_when_arguments_invalid_then_fastmcp_error_text() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    // when
    let unknown = answer(address, &session, &tool_call(200, "nope", json!({}))).await;
    let wrong =
        answer(address, &session, &tool_call(201, "await_copilot_event", json!({"timeout_seconds": "[1]"}))).await;
    let without = answer(
        address,
        &session,
        &json!({"jsonrpc": "2.0", "id": 202, "method": "tools/call", "params": {"name": "set_copilot_status"}}),
    )
    .await;
    // then
    assert_eq!(
        unknown["result"],
        json!({"content": [{"type": "text", "text": "Unknown tool: nope"}], "isError": true})
    );
    assert_eq!(wrong["result"]["isError"], json!(true));
    assert!(
        wrong["result"]["content"][0]["text"].as_str().unwrap().starts_with(
            "Error executing tool await_copilot_event: 1 validation error for await_copilot_eventArguments\n\
             timeout_seconds\n  Input should be a valid integer [type=int_type, input_value=[1], input_type=list]"
        ),
        "{wrong}"
    );
    assert!(
        without["result"]["content"][0]["text"].as_str().unwrap().starts_with(
            "Error executing tool set_copilot_status: 2 validation errors for set_copilot_statusArguments\n\
             request_id\n  Field required [type=missing, input_value={}, input_type=dict]"
        ),
        "{without}"
    );
}

#[tokio::test]
async fn tool_call_when_completed_then_pydantic_json_text() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    // when
    let got =
        answer(address, &session, &tool_call(203, "send_copilot_message", json!({"kind": "", "text": "x"}))).await;
    // then
    assert_eq!(
        got["result"],
        json!({"content": [{"type": "text", "text": "{\n  \"ok\": false,\n  \"error\": \"kind must be a non-empty string\"\n}"}],
               "isError": false})
    );
}

// --- cancellation ---

#[tokio::test]
async fn long_poll_when_cancelled_then_request_cancelled_and_queue_untouched() {
    // given
    let (server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let pending = rpc(address, &session, &tool_call(800, "await_copilot_event", json!({"timeout_seconds": 30}))).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // when
    let cancel = rpc(
        address,
        &session,
        &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 800, "reason": "test"}}),
    )
    .await;
    let events = tokio::time::timeout(Duration::from_secs(5), pending.events()).await.expect("the cancelled call ends");
    server.link.events.push(Map::from_iter([("type".to_owned(), Value::from("meeting_ended"))]));
    let after = answer(address, &session, &tool_call(801, "await_copilot_event", json!({"timeout_seconds": 2}))).await;
    // then
    assert_eq!(cancel.status, 202);
    assert_eq!(events, [json!({"jsonrpc": "2.0", "id": 800, "error": {"code": 0, "message": "Request cancelled"}})]);
    let text = after["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text).unwrap(),
        json!({"ok": true, "event": {"type": "meeting_ended"}})
    );
}

#[tokio::test]
async fn string_request_id_when_cancelled_then_answered_on_its_stream() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let call = json!({"jsonrpc": "2.0", "id": "poll-1", "method": "tools/call",
                      "params": {"name": "await_copilot_event", "arguments": {"timeout_seconds": 30}}});
    let pending = rpc(address, &session, &call).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // when
    rpc(
        address,
        &session,
        &json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "poll-1"}}),
    )
    .await;
    let events = tokio::time::timeout(Duration::from_secs(5), pending.events()).await.expect("the cancelled call ends");
    // then
    assert_eq!(
        events,
        [json!({"jsonrpc": "2.0", "id": "poll-1", "error": {"code": 0, "message": "Request cancelled"}})]
    );
}

// --- the standing GET stream ---

async fn get(address: SocketAddr, headers: &[(&str, &str)]) -> HttpResponse {
    request(address, "GET", headers, Bytes::new()).await
}

#[tokio::test]
async fn get_stream_when_opened_then_one_per_session_and_pings_every_interval() {
    // given
    let (server, address, _dir) = endpoint_for("127.0.0.1", Duration::from_millis(100)).await;
    let session = initialize(address).await;
    let headers = [("accept", "text/event-stream"), ("mcp-session-id", session.as_str()), PROTOCOL];
    // when
    let first = get(address, &headers).await;
    let second = get(address, &headers).await;
    // then
    assert_eq!(first.status, 200);
    assert_eq!(first.header("content-type"), Some("text/event-stream"));
    assert_eq!(first.header("x-accel-buffering"), Some("no"));
    assert_eq!(second.status, 409);
    assert_eq!(second.text().await, server_error(-32600, "Conflict: Only one SSE stream is allowed per session"));
    assert_eq!(server.reachable_count(), 1);
    let mut body = first.body;
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame()).await.unwrap().unwrap().unwrap();
    let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    let time =
        text.strip_prefix(": ping - ").and_then(|t| t.strip_suffix("\r\n\r\n")).unwrap_or_else(|| panic!("{text:?}"));
    assert!(time.ends_with("+00:00") && time.as_bytes()[4] == b'-' && time.as_bytes()[10] == b' ', "{time}");
    drop(body);
    assert!(wait_for(|| server.reachable_count() == 0).await);
}

#[tokio::test]
async fn get_requests_the_transport_refuses_when_sent_then_mcp_errors() {
    // given
    let (_server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let sid = ("mcp-session-id", session.as_str());
    // when
    let resume = get(address, &[("accept", "text/event-stream"), sid, ("last-event-id", "5")]).await;
    let no_accept = get(address, &[sid]).await;
    let no_session = get(address, &[("accept", "text/event-stream")]).await;
    let old_protocol =
        get(address, &[("accept", "text/event-stream"), sid, ("mcp-protocol-version", "1999-01-01")]).await;
    // then
    assert_eq!(resume.status, 500);
    assert_eq!(resume.header("content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(resume.text().await, "Internal Server Error");
    assert_eq!(no_accept.status, 406);
    assert_eq!(no_accept.text().await, server_error(-32600, "Not Acceptable: Client must accept text/event-stream"));
    assert_eq!(no_session.status, 400);
    assert_eq!(no_session.text().await, server_error(-32600, "Bad Request: Missing session ID"));
    assert_eq!(old_protocol.status, 400);
}

#[tokio::test]
async fn pushed_event_with_typed_meta_when_broadcast_then_json_types_kept() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (server, address) = mcp_endpoint(link_in(&dir)).await;
    let session = initialize(address).await;
    let mut stream = open_get_stream(address, &session).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    let meta = object(
        r#"{"s": "x", "n": null, "b": true, "i": -3, "u": 18446744073709551615, "huge": 123456789012345678901234567890,
            "f": 0.5, "a": [1, "y"], "o": {"k": false}}"#,
    );
    // when
    let sent = server.broadcast("content", &meta);
    // then
    assert_eq!(sent, 1);
    let pushed = tokio::time::timeout(Duration::from_secs(2), stream.recv()).await.unwrap().unwrap();
    assert_eq!(
        pushed,
        json!({"method": "notifications/claude/channel", "jsonrpc": "2.0", "params": {"content": "content", "meta": {
            "s": "x", "n": null, "b": true, "i": -3, "u": 18446744073709551615_u64, "huge": 1.2345678901234568e29,
            "f": 0.5, "a": [1, "y"], "o": {"k": false}}}})
    );
}

// --- the end of a session ---

#[tokio::test]
async fn deleted_session_when_used_again_then_terminated_not_found() {
    // given
    let (server, address, _dir) = endpoint().await;
    let session = initialize(address).await;
    let mut stream = open_get_stream(address, &session).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    // when
    let bad_protocol = request(
        address,
        "DELETE",
        &[("mcp-session-id", session.as_str()), ("mcp-protocol-version", "1")],
        Bytes::new(),
    )
    .await;
    let deleted = request(address, "DELETE", &[("mcp-session-id", session.as_str()), PROTOCOL], Bytes::new()).await;
    let post = rpc(address, &session, &ping(900)).await;
    let get_after = get(address, &[("accept", "text/event-stream"), ("mcp-session-id", session.as_str())]).await;
    // then
    assert_eq!(bad_protocol.status, 400);
    assert_eq!(deleted.status, 200);
    assert_eq!(deleted.header("content-type"), Some("application/json"));
    assert_eq!(deleted.header("mcp-session-id"), Some(session.as_str()));
    assert_eq!(deleted.text().await, "");
    assert_eq!(tokio::time::timeout(Duration::from_secs(2), stream.recv()).await.unwrap(), None);
    for response in [post, get_after] {
        assert_eq!(response.status, 404);
        assert_eq!(response.header("mcp-session-id"), Some(session.as_str()));
        assert_eq!(response.text().await, server_error(-32600, "Not Found: Session has been terminated"));
    }
    assert_eq!(server.reachable_count(), 0);
}
