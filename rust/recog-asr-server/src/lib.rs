//! The optional ASR server of recog (docs/asr-server.md), in place of
//! `recog/asr_server/`: the OpenAI-compatible gateway ([`gateway`], binary
//! `recog-asr-gateway`) and the GigaAM backend ([`backend`], binary
//! `recog-asr-gigaam`) on the wavo crate ([`engine`]).
//!
//! Both read the same environment variables ([`config`]) and answer the same routes
//! with the same bodies and status codes as the FastAPI apps, Starlette's and FastAPI's
//! behaviour around the handlers included ([`form`], [`request`], [`http`]). Speaker
//! diarization is not available: the backend answers as the Python one does when its
//! pyannote pipeline failed to load.
#![forbid(unsafe_code)]

use std::process::ExitCode;

pub mod backend;
pub mod config;
pub mod engine;
pub mod form;
pub mod format;
pub mod gateway;
pub mod http;
pub mod json;
pub mod log;
pub mod request;
pub mod wav;

/// Resolves on Ctrl+C or SIGTERM, as uvicorn stops on either.
pub async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) else {
        let _ = ctrl_c.await;
        return;
    };
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
}

/// Binds `host:port` (a name is resolved, as uvicorn does).
///
/// # Errors
/// The address cannot be bound.
pub async fn bind(host: &str, port: u16) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind((host, port)).await
}

/// Whether the command line (the program's name first) asks for the usage.
pub fn wants_help(args: impl Iterator<Item = String>) -> bool {
    args.skip(1).any(|a| a == "-h" || a == "--help")
}

/// Binds `host:port` and runs `serve` on the listener until it returns, as uvicorn
/// runs an app: `<label> running on http://host:port` once bound, and the exit code
/// of the server, a failure to bind or serve logged.
pub async fn run<F, Fut>(label: &str, host: &str, port: u16, serve: F) -> ExitCode
where
    F: FnOnce(tokio::net::TcpListener) -> Fut,
    Fut: Future<Output = std::io::Result<()>>,
{
    let listener = match bind(host, port).await {
        Ok(listener) => listener,
        Err(e) => {
            log::error(&format!("error while attempting to bind on address ({host}, {port}): {e}"));
            return ExitCode::FAILURE;
        }
    };
    log::info(&format!("{label} running on http://{host}:{port}"));
    match serve(listener).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error(&e.to_string());
            ExitCode::FAILURE
        }
    }
}

/// [`run`] on a multi-threaded runtime of its own; `program` names a runtime that
/// cannot be started.
pub fn run_blocking<F, Fut>(program: &str, label: &str, host: &str, port: u16, serve: F) -> ExitCode
where
    F: FnOnce(tokio::net::TcpListener) -> Fut,
    Fut: Future<Output = std::io::Result<()>>,
{
    match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime.block_on(run(label, host, port, serve)),
        Err(e) => {
            eprintln!("{program}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn help_flags_after_the_program_name_when_read_then_help_is_wanted() {
        assert!(wants_help(args(&["prog", "x", "-h"])));
        assert!(wants_help(args(&["prog", "--help"])));
        assert!(!wants_help(args(&["--help"])));
        assert!(!wants_help(args(&["prog", "-help", "help"])));
    }

    #[test]
    fn a_free_port_when_run_then_serve_gets_the_listener_and_its_result_is_the_exit_code() {
        let mut bound = None;
        let code = run_blocking("prog", "test", "127.0.0.1", 0, |listener| {
            bound = listener.local_addr().ok().map(|a| a.ip().to_string());
            async { Ok(()) }
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(bound.as_deref(), Some("127.0.0.1"));
        let code = run_blocking("prog", "test", "127.0.0.1", 0, |_| async { Err(std::io::Error::other("accept")) });
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn a_port_in_use_when_run_then_a_failure_without_serving() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let mut served = false;
        let code = run_blocking("prog", "test", "127.0.0.1", port, |_| {
            served = true;
            async { Ok(()) }
        });
        assert_eq!(code, ExitCode::FAILURE);
        assert!(!served);
    }
}
