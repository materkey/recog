//! The Codex harness's event source: an MCP client of the channel that long-polls
//! `await_copilot_event` and renders each event as the `<channel>` block a Claude
//! session receives, against the real channel server on a random port.

mod channel_support;

use channel_support::{link_in, mcp_endpoint, object};
use recog_copilot::codex::events::{ChannelClient, relay};

#[tokio::test]
async fn queued_question_when_polled_then_the_block_a_claude_session_would_get() {
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (_server, address) = mcp_endpoint(link.clone()).await;
    link.events.push(object(
        r#"{"type": "copilot_user_message", "text": "Что решили?", "request_id": "r1", "meeting_id": "m1"}"#,
    ));
    let mut client = ChannelClient::new(&format!("http://{address}/mcp")).unwrap();

    client.connect().await.unwrap();
    let event = client.next_event(2).await.unwrap().expect("an event");

    assert_eq!(event.event_type, "copilot_user_message");
    assert!(
        event.text.starts_with(
            r#"<channel source="copilot" event_type="copilot_user_message" request_id="r1" meeting_id="m1""#
        ),
        "{}",
        event.text
    );
    assert!(event.text.contains("Text: Что решили?"), "{}", event.text);
    assert!(event.text.ends_with("\n</channel>"), "{}", event.text);
}

#[tokio::test]
async fn quotes_in_an_attribute_when_rendered_then_escaped() {
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (_server, address) = mcp_endpoint(link.clone()).await;
    link.events
        .push(object(r#"{"type": "meeting_started", "meeting_id": "m1", "meeting_title": "Обзор \"A\" & <B>"}"#));
    let mut client = ChannelClient::new(&format!("http://{address}/mcp")).unwrap();
    client.connect().await.unwrap();

    let event = client.next_event(2).await.unwrap().expect("an event");

    assert!(event.text.contains(r#"meeting_title="Обзор &quot;A&quot; &amp; &lt;B&gt;""#), "{}", event.text);
}

#[tokio::test]
async fn nothing_queued_when_polled_then_none_after_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, address) = mcp_endpoint(link_in(&dir)).await;
    let mut client = ChannelClient::new(&format!("http://{address}/mcp")).unwrap();
    client.connect().await.unwrap();

    assert!(client.next_event(1).await.unwrap().is_none());
}

#[tokio::test]
async fn no_channel_when_connecting_then_an_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut client = ChannelClient::new(&format!("http://{address}/mcp")).unwrap();

    assert!(client.connect().await.is_err());
    assert!(ChannelClient::new("https://example.org/mcp").is_err());
}

#[tokio::test]
async fn unreachable_channel_when_relayed_then_try_again() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut client = ChannelClient::new(&format!("http://{address}/mcp")).unwrap();
    let (events, _received) = tokio::sync::mpsc::unbounded_channel();

    assert!(relay(&mut client, &events, 1).await);
}

#[tokio::test]
async fn session_gone_when_an_event_is_relayed_then_stop() {
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (_server, address) = mcp_endpoint(link.clone()).await;
    link.events.push(object(r#"{"type": "transcript_tick", "new_segments": [1]}"#));
    let mut client = ChannelClient::new(&format!("http://{address}/mcp")).unwrap();
    let (events, received) = tokio::sync::mpsc::unbounded_channel();
    drop(received);

    assert!(!relay(&mut client, &events, 1).await);
}

#[tokio::test]
async fn quiet_channel_then_an_event_when_relayed_then_the_event_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (_server, address) = mcp_endpoint(link.clone()).await;
    let mut client = ChannelClient::new(&format!("http://{address}/mcp")).unwrap();
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let relaying = tokio::spawn(async move { relay(&mut client, &events, 1).await });

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    link.events.push(object(r#"{"type": "meeting_ended", "meeting_id": "m1"}"#));
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv()).await.unwrap().unwrap();

    assert_eq!(event.event_type, "meeting_ended");
    relaying.abort();
}
