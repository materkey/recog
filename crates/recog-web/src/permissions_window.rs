//! The local macOS settings button opens one native window. Only the native window
//! can ask for a grant. Neither a readiness read nor a launch accepts a permission.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

use crate::responses;
use crate::router::Route;
use crate::server::AppState;

pub(super) const PATH: &str = "/api/readiness/macos-permissions";

/// The address of the listener itself. An accepted socket can appear local even
/// when the listener accepts connections on every interface.
#[derive(Clone, Copy)]
pub(super) struct BoundAddress(pub SocketAddr);

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(first)
}

fn loopback_listener(request: &Request) -> Option<SocketAddr> {
    let BoundAddress(bound) = *request.extensions().get::<BoundAddress>()?;
    let ConnectInfo(peer) = *request.extensions().get::<ConnectInfo<SocketAddr>>()?;
    (bound.ip().is_loopback() && peer.ip().is_loopback() && request.uri().authority().is_none()).then_some(bound)
}

fn has_proxy_header(headers: &HeaderMap) -> bool {
    headers.keys().any(|key| {
        matches!(
            key.as_str(),
            "forwarded" | "via" | "x-real-ip" | "x-client-ip" | "cf-connecting-ip" | "true-client-ip"
        ) || key.as_str().starts_with("x-forwarded")
    })
}

fn matches_if_present(headers: &HeaderMap, key: &str, expected: &str) -> bool {
    !headers.contains_key(key) || single_header(headers, key) == Some(expected)
}

/// Only the listener's literal address or localhost on its actual port. Browsers
/// omit the default port from Origin, even if Host explicitly says :80.
fn host_origin(bound: SocketAddr, host: &str) -> Option<String> {
    let suffix = format!(":{}", bound.port());
    let name = host.strip_suffix(&suffix).or_else(|| (bound.port() == 80).then_some(host))?;
    if name != "localhost" && format!("{name}{suffix}") != bound.to_string() {
        return None;
    }
    Some(format!("http://{}", if bound.port() == 80 { name } else { host }))
}

fn local_origin(request: &Request) -> Option<HeaderValue> {
    let headers = request.headers();
    if has_proxy_header(headers) || !matches_if_present(headers, "sec-fetch-site", "same-origin") {
        return None;
    }
    let origin = host_origin(loopback_listener(request)?, single_header(headers, "host")?)?;
    if !matches_if_present(headers, "origin", &origin) {
        return None;
    }
    HeaderValue::from_str(&origin).ok()
}

fn available_origin(request: &Request, macos: bool) -> Option<HeaderValue> {
    if macos { local_origin(request) } else { None }
}

pub(super) fn readiness_headers(route: Route, request: &Request) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if route == Route::Readiness {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(origin) = available_origin(request, cfg!(target_os = "macos")) {
            headers.insert("recog-permissions-origin", origin);
        }
    }
    headers
}

/// Unlike a same-origin readiness GET, a launch must supply an Origin.
async fn validate_open(request: Request, macos: bool) -> Result<(), StatusCode> {
    if !request.headers().contains_key(header::ORIGIN) || available_origin(&request, macos).is_none() {
        return Err(StatusCode::FORBIDDEN);
    }
    if request.uri().query().is_some() || axum::body::to_bytes(request.into_body(), 0).await.is_err() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

pub(super) async fn open(State(state): State<Arc<AppState>>, request: Request) -> responses::Reply {
    validate_open(request, cfg!(target_os = "macos")).await.map_err(responses::status_detail)?;
    launch(&state)
}

#[cfg(target_os = "macos")]
fn launch(state: &AppState) -> responses::Reply {
    launch_once(&state.permissions_window, &state.config.repo_root, |root| {
        tokio::process::Command::new(root.join("bin/recog-permissions"))
            .arg(root)
            .args(state.config.install.live.zoom_app.then_some("--zoom"))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
    })
}

#[cfg(not(target_os = "macos"))]
fn launch(_state: &AppState) -> responses::Reply {
    Err(responses::status_detail(StatusCode::FORBIDDEN))
}

#[cfg(any(target_os = "macos", test))]
fn launch_once(
    window: &Arc<tokio::sync::Mutex<()>>,
    root: &std::path::Path,
    spawn: impl FnOnce(&std::path::Path) -> std::io::Result<tokio::process::Child>,
) -> responses::Reply {
    let guard = window.clone().try_lock_owned().map_err(|_| responses::status_detail(StatusCode::CONFLICT))?;
    let root = std::path::absolute(root).map_err(|_| responses::internal_error())?;
    let mut child = spawn(&root).map_err(|error| {
        eprintln!("[permissions] opening the native window failed: {error}");
        responses::status_detail(StatusCode::SERVICE_UNAVAILABLE)
    })?;
    tokio::spawn(async move {
        let _guard = guard;
        if let Err(error) = child.wait().await {
            eprintln!("[permissions] waiting for the native window failed: {error}");
        }
    });
    Ok(crate::server::empty(StatusCode::ACCEPTED))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn local_post(uri: &str, headers: &[(&str, &str)], body: &str) -> Request {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .extension(BoundAddress("127.0.0.1:9080".parse().unwrap()))
            .extension(ConnectInfo("127.0.0.1:3000".parse::<SocketAddr>().unwrap()));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    #[test]
    fn socket_addresses_when_deciding_local_access_then_require_both_the_bound_listener_and_the_peer() {
        for (bound, peer, host, allowed) in [
            ("127.0.0.1:9080", "127.0.0.1:3000", "127.0.0.1:9080", true),
            ("[::1]:9080", "[::1]:3000", "[::1]:9080", true),
            ("127.0.0.1:80", "127.0.0.1:3000", "localhost", true),
            ("127.0.0.1:80", "127.0.0.1:3000", "localhost:80", true),
            ("0.0.0.0:9080", "127.0.0.1:3000", "localhost:9080", false),
            ("[::]:9080", "[::1]:3000", "localhost:9080", false),
            ("127.0.0.1:9080", "192.0.2.1:3000", "localhost:9080", false),
            ("127.0.0.1:9080", "127.0.0.1:3000", "localhost", false),
        ] {
            let mut req = Request::builder().header(header::HOST, host).body(axum::body::Body::empty()).unwrap();
            assert!(local_origin(&req).is_none());
            req.extensions_mut().insert(BoundAddress(bound.parse().unwrap()));
            assert!(local_origin(&req).is_none());
            req.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            assert_eq!(local_origin(&req).is_some(), allowed, "{bound} {peer} {host}");
        }
    }

    #[tokio::test]
    async fn local_post_when_platform_origin_or_payload_changes_then_only_the_empty_macos_request_is_allowed() {
        let host = ("host", "localhost:9080");
        let origin = ("origin", "http://localhost:9080");
        let valid = [host, origin, ("sec-fetch-site", "same-origin")];
        assert_eq!(validate_open(local_post(PATH, &valid, ""), true).await, Ok(()));
        assert_eq!(validate_open(local_post(PATH, &valid, ""), false).await, Err(StatusCode::FORBIDDEN));
        for (uri, body) in [(format!("{PATH}?"), ""), (PATH.into(), "--zoom")] {
            assert_eq!(validate_open(local_post(&uri, &valid, body), true).await, Err(StatusCode::BAD_REQUEST));
        }
        for rejected in [
            vec![host],
            vec![origin],
            vec![host, host, origin],
            vec![host, origin, origin],
            vec![host, ("origin", "http://127.0.0.1:9080")],
            vec![("host", "localhost:9081"), origin],
            vec![("host", "server.test:9080"), ("origin", "http://server.test:9080")],
            vec![host, origin, ("sec-fetch-site", "cross-site")],
            vec![host, origin, ("sec-fetch-site", "same-origin"), ("sec-fetch-site", "same-origin")],
        ] {
            assert_eq!(validate_open(local_post(PATH, &rejected, ""), true).await, Err(StatusCode::FORBIDDEN));
        }
        let absolute = format!("http://localhost:9080{PATH}");
        assert_eq!(validate_open(local_post(&absolute, &valid, ""), true).await, Err(StatusCode::FORBIDDEN));
    }

    #[test]
    fn forwarding_metadata_when_checked_on_any_platform_then_the_origin_is_unavailable() {
        for name in
            ["forwarded", "via", "x-real-ip", "x-client-ip", "cf-connecting-ip", "true-client-ip", "x-forwarded-host"]
        {
            let req = local_post(PATH, &[("host", "localhost:9080"), (name, "test")], "");
            assert!(local_origin(&req).is_none(), "{name}");
        }
    }

    #[tokio::test]
    async fn child_process_when_launch_fails_or_exits_then_its_slot_can_be_reused() {
        let window = Arc::new(tokio::sync::Mutex::new(()));
        let root = std::path::Path::new(".");
        let failed = launch_once(&window, root, |_| Err(std::io::Error::other("unavailable")));
        assert_eq!(failed.unwrap_err().status(), StatusCode::SERVICE_UNAVAILABLE);
        let mut input = None;
        let opened = launch_once(&window, root, |path| {
            assert!(path.is_absolute());
            let mut child = tokio::process::Command::new("/bin/cat").stdin(std::process::Stdio::piped()).spawn()?;
            input = child.stdin.take();
            Ok(child)
        });
        assert_eq!(opened.unwrap().status(), StatusCode::ACCEPTED);
        let duplicate = launch_once(&window, root, |_| panic!("another child would have been started"));
        assert_eq!(duplicate.unwrap_err().status(), StatusCode::CONFLICT);
        drop(input);
        let _guard = tokio::time::timeout(std::time::Duration::from_secs(5), window.lock()).await.unwrap();
    }
}
