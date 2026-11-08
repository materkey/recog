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
