//! The environment variables of both servers, read once at start as the Python
//! modules read them at import (docs/asr-server.md has the table). A value Python's
//! `int()` or `float()` would refuse stops the server with its message.

use std::collections::HashMap;
use std::time::Duration;

use recog_core::py;

/// The process environment, or a test's.
pub type Environ = HashMap<String, String>;

/// The process environment (a value that is not UTF-8 lossily converted).
#[must_use]
pub fn environ() -> Environ {
    std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect()
}

fn get<'a>(env: &'a Environ, key: &str, default: &'a str) -> &'a str {
    env.get(key).map_or(default, String::as_str)
}

/// `int(os.environ.get(key, default))` as a port, which uvicorn then has to bind.
fn port(env: &Environ, key: &str, default: &str) -> Result<u16, String> {
    let value = py::parse_int(get(env, key, default)).map_err(|e| format!("{key}: {e}"))?;
    u16::try_from(value).map_err(|_| format!("{key}: bind(): port must be 0-65535."))
}

/// The gateway's settings (`recog/asr_server/gateway.py`).
#[derive(Debug, Clone, PartialEq)]
pub struct GatewayConfig {
    pub host: String,
    pub port: u16,
    pub qwen_url: String,
    pub gigaam_url: String,
    /// `ASR_TIMEOUT_S`: httpx's timeout of each phase of a transcription request;
    /// `None` for a value no duration holds (negative, infinite, NaN).
    pub timeout: Option<Duration>,
}

impl GatewayConfig {
    /// # Errors
    /// A port or timeout Python would refuse.
    pub fn from_environ(env: &Environ) -> Result<Self, String> {
        let timeout = py::parse_float(get(env, "ASR_TIMEOUT_S", "1800")).map_err(|e| format!("ASR_TIMEOUT_S: {e}"))?;
        Ok(Self {
            host: get(env, "ASR_GATEWAY_HOST", "0.0.0.0").to_owned(),
            port: port(env, "ASR_GATEWAY_PORT", "8770")?,
            qwen_url: get(env, "QWEN_ASR_URL", "http://127.0.0.1:9001").to_owned(),
            gigaam_url: get(env, "GIGAAM_URL", "http://127.0.0.1:9002").to_owned(),
            timeout: Duration::try_from_secs_f64(timeout).ok(),
        })
    }
}

/// The GigaAM backend's settings (`recog/asr_server/gigaam_backend.py`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendConfig {
    pub host: String,
    pub port: u16,
    /// `GIGAAM_VARIANT`, reported by `/healthz` and mapped to a wavo model.
    pub variant: String,
    /// `GIGAAM_DEVICE`: logged only; wavo picks its GPU itself.
    pub device: String,
    /// `GIGAAM_FP16_ENCODER`: logged only; the wavo model is Q8_0.
    pub fp16_encoder: bool,
    /// `GIGAAM_DIARIZATION`: whether to try loading the diarization pipeline.
    pub diarization: bool,
    /// `DIAR_MODEL`, for the log line.
    pub diar_model: String,
}

impl BackendConfig {
    /// # Errors
    /// A port Python would refuse.
    pub fn from_environ(env: &Environ) -> Result<Self, String> {
        let device = get(env, "GIGAAM_DEVICE", "cuda").to_owned();
        let fp16_default = if device.starts_with("cuda") { "1" } else { "0" };
        Ok(Self {
            host: get(env, "GIGAAM_HOST", "127.0.0.1").to_owned(),
            port: port(env, "GIGAAM_PORT", "9002")?,
            variant: get(env, "GIGAAM_VARIANT", "v3_e2e_rnnt").to_owned(),
            fp16_encoder: get(env, "GIGAAM_FP16_ENCODER", fp16_default) == "1",
            device,
            diarization: get(env, "GIGAAM_DIARIZATION", "1") == "1",
            diar_model: get(env, "DIAR_MODEL", "pyannote/speaker-diarization-community-1").to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Environ {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn no_variables_when_the_gateway_reads_them_then_pythons_defaults() {
        let config = GatewayConfig::from_environ(&Environ::new()).unwrap();
        assert_eq!(
            config,
            GatewayConfig {
                host: "0.0.0.0".into(),
                port: 8770,
                qwen_url: "http://127.0.0.1:9001".into(),
                gigaam_url: "http://127.0.0.1:9002".into(),
                timeout: Some(Duration::from_secs(1800)),
            }
        );
    }

    #[test]
    fn a_port_int_accepts_when_read_then_taken_and_a_bad_one_refused() {
        let config = GatewayConfig::from_environ(&env(&[("ASR_GATEWAY_PORT", " +18_770 ")])).unwrap();
        assert_eq!(config.port, 18770);
        let error = GatewayConfig::from_environ(&env(&[("ASR_GATEWAY_PORT", "x")])).unwrap_err();
        assert_eq!(error, "ASR_GATEWAY_PORT: invalid literal for int() with base 10: 'x'");
        assert!(GatewayConfig::from_environ(&env(&[("ASR_GATEWAY_PORT", "70000")])).is_err());
    }

    #[test]
    fn a_cpu_device_when_the_backend_reads_it_then_fp16_defaults_off() {
        let config =
            BackendConfig::from_environ(&env(&[("GIGAAM_DEVICE", "cpu"), ("GIGAAM_DIARIZATION", "0")])).unwrap();
        assert!(!config.fp16_encoder);
        assert!(!config.diarization);
        let cuda = BackendConfig::from_environ(&env(&[("GIGAAM_DEVICE", "cuda:1")])).unwrap();
        assert!(cuda.fp16_encoder);
        assert!(cuda.diarization);
        assert_eq!((cuda.host.as_str(), cuda.port, cuda.variant.as_str()), ("127.0.0.1", 9002, "v3_e2e_rnnt"));
    }
}
