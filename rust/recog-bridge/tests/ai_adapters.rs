//! The `recog-ai` adapters: the Copilot policy the bridge reads matches what Python's
//! `KTalkBridge._read_copilot_policy` reads from the same data directory, and the
//! increment adapter runs `IncrementGenerator.generate_all`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use recog_bridge::ai::{AiIncrements, AiSettingsPolicy};
use recog_bridge::hooks::{IncrementGenerator, PolicySource};
use recog_core::pyjson::{self, Value};

fn python() -> PathBuf {
    if let Some(p) = std::env::var_os("RECOG_BRIDGE_PARITY_PYTHON").filter(|p| !p.is_empty()) {
        return p.into();
    }
    let venv = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../venv-gigaam/bin/python");
    assert!(venv.exists(), "no Python for the parity tests: set RECOG_BRIDGE_PARITY_PYTHON");
    venv
}

/// `KTalkBridge(ai_settings_store=AiSettingsStore(data_dir))._read_copilot_policy()`.
fn python_policy(data_dir: &Path) -> Value {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo");
    let code = "import json, sys; sys.path.insert(0, sys.argv[1]); \
                from recog.ai_settings import AiSettingsStore; \
                from recog.ktalk_bridge import KTalkBridge; \
                b = KTalkBridge(ai_settings_store=AiSettingsStore(sys.argv[2])); \
                print(json.dumps(b._read_copilot_policy(), ensure_ascii=False))";
    let home = data_dir.parent().expect("parent").join("home");
    let output = Command::new(python())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("RECOG_CONFIG_FILE", home.join("absent.toml"))
        .env("RECOG_CONFERENCE_HOSTS_FILE", home.join("absent.json"))
        .env("RECOG_DATA_DIR", data_dir)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .args(["-I", "-c", code])
        .arg(&repo)
        .arg(data_dir)
        .output()
        .expect("python");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    pyjson::loads(String::from_utf8_lossy(&output.stdout).trim()).expect("json")
}

#[test]
fn given_no_settings_file_when_policy_read_then_same_as_python() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // when
    let rust = AiSettingsPolicy::new(&data).read_policy().unwrap();
    // then
    assert_eq!(Value::Object(rust.to_map()), python_policy(&data));
    assert_eq!(rust.enabled, Value::Bool(true));
}

#[test]
fn given_copilot_switched_off_and_a_custom_prompt_when_policy_read_then_same_as_python() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = recog_ai::settings::AiSettingsStore::new(&data, None);
    let values = recog_ai::json::Json::parse(r#"{"enabled": false, "prompt": "Отвечай кратко."}"#).unwrap();
    store.write_task("copilot", &values, None).unwrap();
    // when
    let rust = AiSettingsPolicy::new(&data).read_policy().unwrap();
    // then
    assert_eq!(Value::Object(rust.to_map()), python_policy(&data));
    assert_eq!(rust.enabled, Value::Bool(false));
    assert_eq!(rust.instructions, Value::from("Отвечай кратко."));
}

#[test]
fn given_corrupt_settings_when_policy_read_then_error_as_python_raises() {
    // given
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ai-settings.json"), "{not json").unwrap();
    // when / then
    assert!(AiSettingsPolicy::new(dir.path()).read_policy().is_err());
}

#[tokio::test]
async fn given_meeting_without_meta_when_increments_run_then_nothing_to_do() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let generator = recog_ai::increment::IncrementGenerator::new(dir.path().join("meetings"), None);
    let adapter = AiIncrements(Arc::new(generator));
    // when
    let result = adapter.generate_all("m1".to_owned(), dir.path().join("meetings/m1")).await;
    // then
    assert_eq!(result, Ok(()));
}
