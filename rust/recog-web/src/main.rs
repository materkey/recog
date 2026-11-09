//! `recog-web`: the web backend, as `python -m recog.web.server` runs it.
//!
//! `recog-web [--host HOST] [--port PORT] [--repo DIR]`: 127.0.0.1:9080 and the
//! checkout the binary was built from by default. `recog-web desktop-worker …` is the
//! desktop integration's inspector worker, which the server's observer starts.
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

fn parse_args(argv: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args =
        Args { host: config::DEFAULT_HOST.to_owned(), port: config::DEFAULT_PORT, repo: config::default_repo_root() };
    let mut iter = argv.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--host" | "--port" | "--repo" => {
                let value = iter.next().ok_or_else(|| format!("{arg} needs a value"))?;
                set_option(&mut args, &arg, value)?;
            }
            "-h" | "--help" => {
                return Err("usage: recog-web [--host HOST] [--port PORT] [--repo DIR]".to_owned());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

/// `--host`, `--port` or `--repo` with its value.
fn set_option(args: &mut Args, flag: &str, value: String) -> Result<(), String> {
    match flag {
        "--host" => args.host = value,
        "--port" => args.port = value.parse().map_err(|e| format!("--port: {e}"))?,
        _ => args.repo = PathBuf::from(value),
    }
    Ok(())
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

fn desktop_worker(args: &[String]) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(recog_desktop::worker::main(&recog_desktop::host::Host::system(), args));
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some(recog_web::desktop::WORKER_ARGUMENT) {
        return desktop_worker(&argv[2..]);
    }
    match parse_args(argv.into_iter().skip(1)) {
        Ok(args) => serve(args),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

/// The server on the given address until SIGINT or SIGTERM.
fn serve(args: Args) -> ExitCode {
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
    runtime.block_on(run(args, config))
}

async fn run(args: Args, config: Config) -> ExitCode {
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
    exit_code(tokio::time::timeout(Duration::from_secs(5), serving).await)
}

/// The exit status of a stopped server: a server still closing after the timeout
/// counts as stopped.
fn exit_code(
    stopped: Result<Result<std::io::Result<()>, tokio::task::JoinError>, tokio::time::error::Elapsed>,
) -> ExitCode {
    match stopped {
        Ok(Ok(Ok(()))) | Err(_) => ExitCode::SUCCESS,
        Ok(Ok(Err(e))) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
        Ok(Err(_)) => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn no_arguments_when_parsed_then_the_module_defaults() {
        let args = parsed(&[]).unwrap();

        assert_eq!((args.host.as_str(), args.port), (config::DEFAULT_HOST, config::DEFAULT_PORT));
        assert_eq!(args.repo, config::default_repo_root());
    }

    #[test]
    fn every_option_when_parsed_then_each_is_taken_the_last_winning() {
        let args = parsed(&["--host", "0.0.0.0", "--port", "1", "--repo", "/r", "--port", "8123"]).unwrap();

        assert_eq!((args.host.as_str(), args.port, args.repo), ("0.0.0.0", 8123, PathBuf::from("/r")));
    }

    #[test]
    fn bad_arguments_when_parsed_then_the_reason() {
        for (args, message) in [
            (&["--host"][..], "--host needs a value"),
            (&["--repo"][..], "--repo needs a value"),
            (&["--port", "70000"][..], "--port: number too large to fit in target type"),
            (&["x"][..], "unknown argument x"),
        ] {
            assert_eq!(parsed(args).err().as_deref(), Some(message), "{args:?}");
        }
    }

    #[tokio::test]
    async fn stopped_server_when_its_end_is_judged_then_success_unless_it_failed() {
        let ok = tokio::spawn(async { Ok(()) }).await;
        let failed = tokio::spawn(async { Err(std::io::Error::other("boom")) }).await;
        let panicked = tokio::spawn(async { panic!("server task") }).await;
        let elapsed = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>()).await.unwrap_err();

        assert_eq!(exit_code(Ok(ok)), ExitCode::SUCCESS);
        assert_eq!(exit_code(Err(elapsed)), ExitCode::SUCCESS);
        assert_eq!(exit_code(Ok(failed)), ExitCode::FAILURE);
        assert_eq!(exit_code(Ok(panicked)), ExitCode::FAILURE);
    }
}
