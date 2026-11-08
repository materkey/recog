//! `recog-asr-gateway`: the ASR gateway, as `python -m recog.asr_server.gateway` runs
//! it. Configured by the environment only (`ASR_GATEWAY_HOST`, `ASR_GATEWAY_PORT`,
//! `GIGAAM_URL`, `QWEN_ASR_URL`, `ASR_TIMEOUT_S`; docs/asr-server.md).
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;

use recog_asr_server::config::{self, GatewayConfig};
use recog_asr_server::gateway::{self, Gateway};
use recog_asr_server::log;

const USAGE: &str = "usage: recog-asr-gateway\n\nSettings come from the environment: ASR_GATEWAY_HOST (0.0.0.0), \
ASR_GATEWAY_PORT (8770), GIGAAM_URL (http://127.0.0.1:9002), QWEN_ASR_URL (http://127.0.0.1:9001), ASR_TIMEOUT_S (1800).";

fn main() -> ExitCode {
    if std::env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let config = match GatewayConfig::from_environ(&config::environ()) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("recog-asr-gateway: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("recog-asr-gateway: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let (host, port) = (config.host.clone(), config.port);
        let listener = match recog_asr_server::bind(&host, port).await {
            Ok(listener) => listener,
            Err(e) => {
                log::error(&format!("error while attempting to bind on address ({host}, {port}): {e}"));
                return ExitCode::FAILURE;
            }
        };
        log::info(&format!("asr-gateway running on http://{host}:{port}"));
        match gateway::serve(listener, Arc::new(Gateway::new(config)), recog_asr_server::shutdown_signal()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                log::error(&e.to_string());
                ExitCode::FAILURE
            }
        }
    })
}
