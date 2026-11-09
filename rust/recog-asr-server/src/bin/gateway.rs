//! `recog-asr-gateway`: the ASR gateway, as `python -m recog.asr_server.gateway` runs
//! it. Configured by the environment only (`ASR_GATEWAY_HOST`, `ASR_GATEWAY_PORT`,
//! `GIGAAM_URL`, `QWEN_ASR_URL`, `ASR_TIMEOUT_S`; docs/asr-server.md).
#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;

use recog_asr_server::config::{self, GatewayConfig};
use recog_asr_server::gateway::{self, Gateway};

const USAGE: &str = "usage: recog-asr-gateway\n\nSettings come from the environment: ASR_GATEWAY_HOST (0.0.0.0), \
ASR_GATEWAY_PORT (8770), GIGAAM_URL (http://127.0.0.1:9002), QWEN_ASR_URL (http://127.0.0.1:9001), ASR_TIMEOUT_S (1800).";

fn main() -> ExitCode {
    if recog_asr_server::wants_help(std::env::args()) {
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
    let (host, port) = (config.host.clone(), config.port);
    recog_asr_server::run_blocking("recog-asr-gateway", "asr-gateway", &host, port, move |listener| {
        gateway::serve(listener, Arc::new(Gateway::new(config)), recog_asr_server::shutdown_signal())
    })
}
