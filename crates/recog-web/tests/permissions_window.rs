//! Exercise the production listener with a stand-in for the native program.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::time::Instant;

use recog_web::server;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

const OPEN: &str = "/api/readiness/macos-permissions";

async fn request(port: u16, method: &str, path: &str, headers: &str, body: &str) -> (u16, String) {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let raw = format!(
        "{method} {path} HTTP/1.1\r\n{headers}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(raw.as_bytes()).await.unwrap();
    let mut reply = String::new();
    socket.read_to_string(&mut reply).await.unwrap();
    let status = reply.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(!reply.to_ascii_lowercase().contains("access-control-allow-origin:"));
    (status, reply)
}

async fn with_server(app: &support::App, check: impl AsyncFnOnce(u16)) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server::serve(app.state.clone(), listener, async {
        let _ = stopped.await;
    }));
    check(port).await;
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

fn install_helper(root: &std::path::Path) {
    let helper = root.join("bin/recog-permissions");
    support::write(
        &helper,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$1/opened\"\nwhile [ ! -f \"$1/closed\" ]; do sleep 0.05; done\n",
    );
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn local_settings_when_viewed_or_sent_invalid_requests_then_no_window_opens() {
    let app = support::App::new();
    let root = &app.state.config.repo_root;
    install_helper(root);
    // Exercise the readiness handler without running the machine's diagnostics.
    *app.state.readiness.lock().await = Some((Instant::now(), support::obj("[]")));
    with_server(&app, async |port| {
        let host = format!("Host: localhost:{port}\r\n");
        let origin = format!("http://localhost:{port}");
        let headers = format!("{host}Origin: {origin}\r\n");
        let (status, readiness) = request(port, "GET", "/api/readiness", &host, "").await;
        assert_eq!(status, 200);
        assert_eq!(readiness.contains(&format!("recog-permissions-origin: {origin}\r\n")), cfg!(target_os = "macos"));
        let proxy = format!("{headers}Forwarded: for=192.0.2.1\r\n");
        let (_, readiness) = request(port, "GET", "/api/readiness", &proxy, "").await;
        assert!(!readiness.contains("recog-permissions-origin:"));
        for method in ["GET", "HEAD", "OPTIONS", "PUT"] {
            assert_eq!(request(port, method, OPEN, &headers, "").await.0, 405);
        }
        for rejected in [
            host.clone(),
            format!("{host}Origin: null\r\n"),
            format!("{host}Origin: http://127.0.0.1:{port}\r\n"),
            format!("{host}Origin: https://localhost:{port}\r\n"),
            "Host: localhost:1\r\nOrigin: http://localhost:1\r\n".into(),
            format!("Host: ui.example:{port}\r\nOrigin: http://ui.example:{port}\r\n"),
            format!("{headers}Origin: {origin}\r\n"),
            format!("{headers}Sec-Fetch-Site: cross-site\r\n"),
            format!("{headers}X-Forwarded-For: 127.0.0.1\r\n"),
            format!("{headers}Via: 1.1 local\r\n"),
            proxy,
        ] {
            assert_eq!(request(port, "POST", OPEN, &rejected, "").await.0, 403, "{rejected}");
        }
        let duplicate_host = request(port, "POST", OPEN, &format!("{headers}{host}"), "").await.0;
        assert!(matches!(duplicate_host, 400 | 403));
        let invalid = if cfg!(target_os = "macos") { 400 } else { 403 };
        assert_eq!(request(port, "POST", &format!("{OPEN}?zoom=1"), &headers, "").await.0, invalid);
        assert_eq!(request(port, "POST", OPEN, &headers, "--zoom").await.0, invalid);
        assert!(!root.join("opened").exists());

        if !cfg!(target_os = "macos") {
            assert_eq!(request(port, "POST", OPEN, &headers, "").await.0, 403);
        }
    })
    .await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn local_mac_when_clicked_then_one_child_receives_config_arguments_and_can_reopen_after_exit() {
    use std::time::Duration;

    for zoom in [false, true] {
        let app = support::App::with(|config| config.install.live.zoom_app = zoom);
        let root = &app.state.config.repo_root;
        let log = root.join("opened");
        with_server(&app, async |port| {
            let headers = format!("Host: localhost:{port}\r\nOrigin: http://localhost:{port}\r\n");
            assert_eq!(request(port, "POST", OPEN, &headers, "").await.0, 503);
            install_helper(root);
            assert_eq!(request(port, "POST", OPEN, &headers, "").await.0, 202);
            assert_eq!(request(port, "POST", OPEN, &headers, "").await.0, 409);
            let args = format!("{}\n{}", root.display(), if zoom { "--zoom\n" } else { "" });
            fs::write(root.join("closed"), "").unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match request(port, "POST", OPEN, &headers, "").await.0 {
                        202 => break,
                        409 => tokio::time::sleep(Duration::from_millis(10)).await,
                        other => panic!("unexpected status {other}"),
                    }
                }
                while fs::read_to_string(&log).unwrap_or_default() != args.repeat(2) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the first process exited and exactly one more window opened");
        })
        .await;
    }
}
