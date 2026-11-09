//! What the server answers before any route runs: unknown paths and methods, the
//! slash redirects, the static directory's refusals, and WebSocket upgrades to
//! unknown paths, from refused origins or without a valid handshake.

mod support;

use axum::http::StatusCode;
use http_body_util::BodyExt as _;
use support::{App, text};

async fn body(response: axum::response::Response) -> Vec<u8> {
    response.into_body().collect().await.unwrap().to_bytes().to_vec()
}

#[tokio::test]
async fn unknown_path_or_method_when_requested_then_404_or_405_with_allow() {
    let app = App::new();

    let (status, answer) = app.json("GET", "/api/nope", None).await;
    assert_eq!((status, text(&answer, "detail")), (StatusCode::NOT_FOUND, "Not Found".into()));
    let response = app.response("PATCH", "/api/privacy", &[]).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response.headers()["allow"].to_str().unwrap().contains("GET"));
}

#[tokio::test]
async fn path_with_or_without_its_slash_when_requested_then_307_to_its_twin_with_the_query() {
    let app = App::new();
    for (path, location) in
        [("/api/meetings/?x=1", "http://ui.test/api/meetings?x=1"), ("/static", "http://ui.test/static/")]
    {
        let response = app.response("GET", path, &[("host", "ui.test")]).await;

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT, "{path}");
        assert_eq!(response.headers()["location"], location, "{path}");
    }
}

#[tokio::test]
async fn static_directory_when_written_to_or_missing_a_file_then_405_or_404() {
    let app = App::new();

    let response = app.response("POST", "/static/app.js", &[]).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = app.response("GET", "/static/nope.js", &[]).await;
    assert_eq!(
        (response.status(), body(response).await),
        (StatusCode::NOT_FOUND, br#"{"detail":"Not Found"}"#.to_vec())
    );
    let response = app.response("HEAD", "/static/app.js", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn upgrade_to_an_unknown_path_or_from_a_refused_origin_when_requested_then_403() {
    let app = App::new();
    let upgrade = [("upgrade", "websocket"), ("connection", "upgrade")];
    for (path, origin) in [("/api/nope", None), ("/api/live/stream", Some("https://evil.example"))] {
        let mut headers = upgrade.to_vec();
        if let Some(origin) = origin {
            headers.push(("origin", origin));
        }

        let response = app.response("GET", path, &headers).await;

        assert_eq!((response.status(), body(response).await), (StatusCode::FORBIDDEN, Vec::new()), "{path}");
    }
}

#[tokio::test]
async fn upgrade_without_a_valid_handshake_when_requested_then_rejected_before_any_socket() {
    let app = App::new();

    let response =
        app.response("GET", "/api/meetings/m/watch", &[("upgrade", "websocket"), ("connection", "upgrade")]).await;

    assert!(response.status().is_client_error(), "{}", response.status());
    assert_eq!(app.state.transcript_watcher.tail(&app.meetings().join("m/transcript.jsonl")), None);
}
