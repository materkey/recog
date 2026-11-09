//! What the two programs do before and after they serve: the usage, the settings
//! Python refuses, the model that cannot load, a port in use, and SIGTERM. The GigaAM
//! backend's model is never loaded: every case here stops before it.

mod support;

use std::process::{Command, Output};

/// The variables both programs read (docs/asr-server.md).
const SETTINGS: [&str; 12] = [
    "ASR_GATEWAY_HOST",
    "ASR_GATEWAY_PORT",
    "GIGAAM_URL",
    "QWEN_ASR_URL",
    "ASR_TIMEOUT_S",
    "GIGAAM_HOST",
    "GIGAAM_PORT",
    "GIGAAM_VARIANT",
    "GIGAAM_DEVICE",
    "GIGAAM_FP16_ENCODER",
    "GIGAAM_DIARIZATION",
    "DIAR_MODEL",
];

/// Runs `program` with only `env` of the settings set; the rest of the environment
/// is kept (a coverage run's profile path among it).
fn run(program: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(program);
    command.args(args);
    for k in SETTINGS {
        command.env_remove(k);
    }
    for (k, v) in env {
        command.env(k, v);
    }
    command.output().unwrap()
}

const GATEWAY: &str = env!("CARGO_BIN_EXE_recog-asr-gateway");
const GIGAAM: &str = env!("CARGO_BIN_EXE_recog-asr-gigaam");

#[test]
fn a_help_flag_when_either_program_starts_then_its_usage_and_success() {
    for (program, name) in [(GATEWAY, "recog-asr-gateway"), (GIGAAM, "recog-asr-gigaam")] {
        for flag in ["-h", "--help"] {
            let output = run(program, &[flag], &[]);
            assert!(output.status.success(), "{name} {flag}");
            assert!(String::from_utf8_lossy(&output.stdout).starts_with(&format!("usage: {name}\n")), "{name} {flag}");
        }
    }
}

#[test]
fn a_port_python_refuses_when_the_backend_starts_then_it_exits_with_the_message() {
    let output = run(GIGAAM, &[], &[("GIGAAM_PORT", "eighty")]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("recog-asr-gigaam: GIGAAM_PORT: invalid literal for int() with base 10: 'eighty'")
    );
}

#[test]
fn a_variant_wavo_lacks_when_the_backend_starts_then_uvicorns_startup_failure_code() {
    let output = run(GIGAAM, &[], &[("GIGAAM_VARIANT", "v2_rnnt")]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("INFO loading gigaam v2_rnnt"), "{stderr}");
    assert!(stderr.contains("ERROR Application startup failed: GIGAAM_VARIANT v2_rnnt is not available"), "{stderr}");
}

#[test]
fn a_port_in_use_when_the_gateway_starts_then_uvicorns_bind_error() {
    let taken = support::listener();
    let port = taken.local_addr().unwrap().port().to_string();
    let output = run(GATEWAY, &[], &[("ASR_GATEWAY_HOST", "127.0.0.1"), ("ASR_GATEWAY_PORT", &port)]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("ERROR error while attempting to bind on address (127.0.0.1, {port}): ")),
        "{stderr}"
    );
}

#[test]
fn a_running_gateway_when_sent_sigterm_then_it_stops_with_success() {
    let dir = tempfile::tempdir().unwrap();
    let port = support::free_port();
    let unused = support::free_port();
    let env = [
        ("ASR_GATEWAY_HOST", "127.0.0.1".to_owned()),
        ("ASR_GATEWAY_PORT", port.to_string()),
        ("GIGAAM_URL", format!("http://127.0.0.1:{unused}")),
        ("QWEN_ASR_URL", format!("http://127.0.0.1:{unused}")),
    ];
    let mut gateway = support::Process::start(Command::new(GATEWAY), &env, port, dir.path().join("log"), "/v1/models");
    let pid = gateway.child.id().to_string();
    assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
    let status = gateway.child.wait().unwrap();
    assert!(status.success(), "{status:?}\n{}", gateway.log_text());
    assert!(gateway.log_text().contains(&format!("INFO asr-gateway running on http://127.0.0.1:{port}")));
}
