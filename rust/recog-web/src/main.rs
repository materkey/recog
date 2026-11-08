//! `recog-web`: the web backend, as `python -m recog.web.server` runs it.
//!
//! `recog-web [--host HOST] [--port PORT] [--repo DIR]`: 127.0.0.1:9080 and the
//! checkout the binary was built from by default.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use recog_web::config::{self, Config};
use recog_web::server::{self, AppState};

struct Args {
    host: String,
    port: u16,
    repo: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut args =
        Args { host: config::DEFAULT_HOST.to_owned(), port: config::DEFAULT_PORT, repo: config::default_repo_root() };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--host" => args.host = value()?,
            "--port" => args.port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
            "--repo" => args.repo = PathBuf::from(value()?),
            "-h" | "--help" => {
                return Err("usage: recog-web [--host HOST] [--port PORT] [--repo DIR]".to_owned());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(term) => term,
        Err(_) => {
            let _ = ctrl_c.await;
            return;
        }
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let config = match Config::from_process(&args.repo, &args.host, args.port) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    // Deeply nested JSON from clients is parsed recursively, as Python's decoder does.
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_stack_size(16 << 20).build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!(
                    "[Errno {}] error while attempting to bind on address ({}, {}): {e}",
                    e.raw_os_error().unwrap_or(0),
                    args.host,
                    args.port
                );
                return ExitCode::FAILURE;
            }
        };
        let state = AppState::new(config);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server::serve(state, listener, async move {
            let _ = stop_rx.await;
        }));
        shutdown_signal().await;
        let _ = stop_tx.send(());
        // Open WebSockets would hold a graceful shutdown forever; uvicorn closes them.
        match tokio::time::timeout(Duration::from_secs(5), serving).await {
            Ok(Ok(Ok(()))) | Err(_) => ExitCode::SUCCESS,
            Ok(Ok(Err(e))) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
            Ok(Err(_)) => ExitCode::FAILURE,
        }
    })
}
