//! `recog-asr-gigaam`: the GigaAM backend, as `python -m
//! recog.asr_server.gigaam_backend` runs it, with the model on wavo. Configured by the
//! environment only (`GIGAAM_HOST`, `GIGAAM_PORT`, `GIGAAM_VARIANT`, ...;
//! docs/asr-server.md). The model loads (downloaded into the Hugging Face cache when
//! missing) before the port is bound, as the Python lifespan loads it before uvicorn
//! listens; a failed load exits with uvicorn's startup-failure code 3.
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;

use recog_asr_server::backend::{self, Backend};
use recog_asr_server::config::{self, BackendConfig};
use recog_asr_server::engine::{Transcriber, WavoTranscriber};
use recog_asr_server::log;

const USAGE: &str = "usage: recog-asr-gigaam\n\nSettings come from the environment: GIGAAM_HOST (127.0.0.1), \
GIGAAM_PORT (9002), GIGAAM_VARIANT (v3_e2e_rnnt), GIGAAM_DEVICE, GIGAAM_FP16_ENCODER, GIGAAM_DIARIZATION (1), \
DIAR_MODEL; see docs/asr-server.md.";

fn main() -> ExitCode {
    if std::env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let config = match BackendConfig::from_environ(&config::environ()) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("recog-asr-gigaam: {e}");
            return ExitCode::FAILURE;
        }
    };
    log::info(&format!("loading gigaam {}", config.variant));
    let engine = match WavoTranscriber::load(&config.variant) {
        Ok(engine) => engine,
        Err(e) => {
            log::error(&format!("Application startup failed: {e}"));
            return ExitCode::from(3);
        }
    };
    log::info(&format!(
        "gigaam on {} (GIGAAM_DEVICE={}, GIGAAM_FP16_ENCODER={}: wavo chooses its GPU and runs Q8_0)",
        engine.device(),
        config.device,
        u8::from(config.fp16_encoder)
    ));
    if config.diarization {
        log::info(&format!("loading diarization pipeline {}", config.diar_model));
        log::warning("pyannote pipeline failed to load — diarization will be disabled");
    }
    log::info("ready");
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("recog-asr-gigaam: {e}");
            return ExitCode::FAILURE;
        }
    };
    let engine: Arc<dyn Transcriber> = Arc::new(engine);
    runtime.block_on(async move {
        let (host, port) = (config.host.clone(), config.port);
        let listener = match recog_asr_server::bind(&host, port).await {
            Ok(listener) => listener,
            Err(e) => {
                log::error(&format!("error while attempting to bind on address ({host}, {port}): {e}"));
                return ExitCode::FAILURE;
            }
        };
        log::info(&format!("gigaam-backend running on http://{host}:{port}"));
        let backend = Arc::new(Backend::new(config, Some(engine)));
        match backend::serve(listener, backend, recog_asr_server::shutdown_signal()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                log::error(&e.to_string());
                ExitCode::FAILURE
            }
        }
    })
}
