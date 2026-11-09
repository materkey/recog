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
    if recog_asr_server::wants_help(std::env::args()) {
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
    match WavoTranscriber::load(&config.variant) {
        Ok(engine) => serve(config, engine),
        Err(e) => {
            log::error(&format!("Application startup failed: {e}"));
            ExitCode::from(3)
        }
    }
}

/// Reports the loaded model and the missing diarization, then serves until stopped.
fn serve(config: BackendConfig, engine: WavoTranscriber) -> ExitCode {
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
    let engine: Arc<dyn Transcriber> = Arc::new(engine);
    let (host, port) = (config.host.clone(), config.port);
    recog_asr_server::run_blocking("recog-asr-gigaam", "gigaam-backend", &host, port, move |listener| {
        backend::serve(listener, Arc::new(Backend::new(config, Some(engine))), recog_asr_server::shutdown_signal())
    })
}
