//! The installation settings and the Copilot task of `ai-settings.json`
//! (`test_autospawn_from_installation_config`, `test_grace_from_installation_config`,
//! `test_mcp_url_from_installation_config`, and what the supervisor gets from the AI
//! settings store; the file's rules are recog-ai's, tested in recog-ai/tests/settings.rs).
#![forbid(unsafe_code)]

use std::path::Path;

use recog_ai::install::LegacyEnv;
use recog_copilot::ai_settings::{AiSettingsFile, CopilotSettings, CopilotTask};
use recog_copilot::config::{self, Env};
use recog_copilot::log;

const REVISION: &str = "0b1e4c52-4f0a-4b7e-9d5e-3c2f6a7b8c9d";

fn env(dir: &Path, pairs: &[(&str, &str)]) -> Env {
    let home = dir.join("home");
    let mut all = vec![
        ("HOME".to_owned(), home.display().to_string()),
        ("RECOG_CONFIG_FILE".to_owned(), "/nonexistent/recog/config.toml".to_owned()),
    ];
    all.extend(pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
    Env::from_pairs(all)
}

#[test]
fn nothing_configured_gives_the_python_defaults() {
    // given
    let dir = tempfile::tempdir().unwrap();
    // when
    let settings = config::load(dir.path(), env(dir.path(), &[])).unwrap();
    // then
    assert!(settings.autospawn);
    assert_eq!(settings.grace_s, 90.0);
    assert_eq!(settings.mcp_url, "http://127.0.0.1:8767/mcp");
    assert_eq!(settings.data_dir, dir.path().join("home/.recog"));
}

#[test]
fn environment_values_set_autospawn_grace_and_mcp_url() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let vars = [
        ("RECOG_COPILOT_AUTOSPAWN", " Off "),
        ("RECOG_COPILOT_GRACE_S", "17.5"),
        ("RECOG_COPILOT_MCP_URL", "http://127.0.0.1:9999/mcp"),
    ];
    // when
    let settings = config::load(dir.path(), env(dir.path(), &vars)).unwrap();
    // then
    assert!(!settings.autospawn);
    assert_eq!(settings.grace_s, 17.5);
    assert_eq!(settings.mcp_url, "http://127.0.0.1:9999/mcp");
}

#[test]
fn blank_environment_values_count_as_unset() {
    // given
    let dir = tempfile::tempdir().unwrap();
    // when
    let settings = config::load(dir.path(), env(dir.path(), &[("RECOG_COPILOT_AUTOSPAWN", "  ")])).unwrap();
    // then
    assert!(settings.autospawn);
}

#[test]
fn flag_outside_the_accepted_words_stops_startup_with_its_name() {
    // given
    let dir = tempfile::tempdir().unwrap();
    // when
    let error = config::load(dir.path(), env(dir.path(), &[("RECOG_COPILOT_AUTOSPAWN", "maybe")])).unwrap_err();
    // then
    assert_eq!(
        error,
        "RECOG_COPILOT_AUTOSPAWN: RECOG_COPILOT_AUTOSPAWN='maybe' is not one of 1/0, true/false, yes/no, on/off"
    );
}

#[test]
fn dotenv_fills_unset_variables_with_quotes_stripped_and_never_overrides_the_environment() {
    // given: an .env from before 2026-11-24 turns autospawn off
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "# comment\nRECOG_COPILOT_AUTOSPAWN=0\nRECOG_COPILOT_MCP_URL=\"http://x/mcp\"\r\nCLAUDE_MODEL='legacy'\nnot a line\n",
    )
    .unwrap();
    // when
    let from_file = config::load(dir.path(), env(dir.path(), &[])).unwrap();
    let overridden = config::load(dir.path(), env(dir.path(), &[("RECOG_COPILOT_AUTOSPAWN", "1")])).unwrap();
    // then
    assert!(!from_file.autospawn);
    assert_eq!(from_file.mcp_url, "http://x/mcp");
    assert_eq!(from_file.legacy.claude_model.as_deref(), Some("legacy"));
    assert!(from_file.from_dotenv.contains(&("CLAUDE_MODEL".to_owned(), "legacy".to_owned())));
    assert!(overridden.autospawn);
}

fn write_config(dir: &Path, body: &str) -> String {
    let path = dir.join("config.toml");
    std::fs::write(&path, body).unwrap();
    path.display().to_string()
}

#[test]
fn copilot_table_of_config_toml_sets_the_values_and_the_environment_wins_with_a_warning() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "[copilot]\nautospawn = false\ngrace_s = 30\nmcp_url = \"http://file/mcp\"\n");
    let capture = log::Capture::start();
    // when
    let file = config::load(dir.path(), env(dir.path(), &[("RECOG_CONFIG_FILE", &path)])).unwrap();
    let both =
        config::load(dir.path(), env(dir.path(), &[("RECOG_CONFIG_FILE", &path), ("RECOG_COPILOT_GRACE_S", "5")]))
            .unwrap();
    // then
    assert!(!file.autospawn);
    assert_eq!(file.grace_s, 30.0);
    assert_eq!(file.mcp_url, "http://file/mcp");
    assert_eq!(both.grace_s, 5.0);
    assert!(capture.text().contains("RECOG_COPILOT_GRACE_S from the environment or .env overrides [copilot] grace_s"));
}

#[test]
fn copilot_table_with_an_unknown_key_or_a_wrong_type_stops_startup() {
    // given
    let dir = tempfile::tempdir().unwrap();
    for body in ["[copilot]\nautospawn = 1\n", "[copilot]\nspeed = 2\n", "copilot = 3\n", "[copilot\n"] {
        let path = write_config(dir.path(), body);
        // when
        let result = config::load(dir.path(), env(dir.path(), &[("RECOG_CONFIG_FILE", &path)]));
        // then
        assert!(result.is_err(), "{body:?}");
    }
}

#[test]
fn relative_data_dir_is_refused_and_a_tilde_is_expanded() {
    // given
    let dir = tempfile::tempdir().unwrap();
    // when
    let relative = config::load(dir.path(), env(dir.path(), &[("RECOG_DATA_DIR", "data")]));
    let tilde = config::load(dir.path(), env(dir.path(), &[("RECOG_DATA_DIR", "~/recog-data")])).unwrap();
    // then
    assert!(relative.is_err());
    assert_eq!(tilde.data_dir, dir.path().join("home/recog-data"));
}

#[test]
fn legacy_data_in_the_checkout_stays_the_data_dir_and_data_in_both_places_is_refused() {
    // given
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("meetings")).unwrap();
    // when
    let legacy = config::load(dir.path(), env(dir.path(), &[])).unwrap();
    std::fs::create_dir_all(dir.path().join("home/.recog/logs")).unwrap();
    let conflict = config::load(dir.path(), env(dir.path(), &[]));
    // then
    assert_eq!(legacy.data_dir, dir.path());
    assert!(conflict.unwrap_err().contains("exists in both"));
}

#[test]
fn session_environment_keeps_only_the_nonempty_session_keys() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let vars = [("PATH", "/custom/bin"), ("TERM", "xterm-256color"), ("SHELL", ""), ("OTHER", "x")];
    // when
    let settings = config::load(dir.path(), env(dir.path(), &vars)).unwrap();
    // then
    let keys: Vec<&str> = settings.session_env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["PATH", "HOME", "TERM"]);
}

/// A Copilot task with the shipped provider order and Codex settings.
fn task(enabled: bool, model: &str, effort: &str) -> CopilotTask {
    CopilotTask {
        enabled,
        automatic: true,
        providers: vec!["claude".into(), "codex".into()],
        model: model.to_owned(),
        claude_effort: effort.to_owned(),
        codex_model: "gpt-6-astra".into(),
        reasoning_effort: "xhigh".into(),
    }
}

fn resolve(body: Option<&str>, legacy: LegacyEnv) -> Result<CopilotTask, String> {
    let dir = tempfile::tempdir().unwrap();
    if let Some(body) = body {
        std::fs::write(dir.path().join("ai-settings.json"), body).unwrap();
    }
    AiSettingsFile::new(dir.path(), legacy).resolve()
}

#[test]
fn missing_settings_file_when_resolved_then_the_default_copilot_is_off_until_asked() {
    // given / when / then
    let off = CopilotTask { automatic: false, ..task(false, "claude-opus-5-5", "") };
    assert_eq!(resolve(None, LegacyEnv::default()), Ok(off));
}

#[test]
fn saved_switch_model_and_effort_are_read_back() {
    // given
    let body = format!(
        r#"{{"version": 1, "revision": "{REVISION}", "models": {{}},
        "tasks": {{"copilot": {{"enabled": false, "models": {{"claude": "m"}}, "claude_effort": "max"}}}}}}"#
    );
    // when / then
    assert_eq!(resolve(Some(&body), LegacyEnv::default()), Ok(task(false, "m", "max")));
}

#[test]
fn codex_copilot_given_saved_when_resolved_then_its_order_model_and_effort() {
    // given
    let body = format!(
        r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"providers": ["codex"],
        "models": {{"codex": "copilot-codex"}}, "reasoning_effort": "high"}}}}}}"#
    );
    // when
    let task = resolve(Some(&body), LegacyEnv::default()).unwrap();
    // then
    assert_eq!(task.providers, ["codex"]);
    assert_eq!((task.codex_model.as_str(), task.reasoning_effort.as_str()), ("copilot-codex", "high"));
}

#[test]
fn legacy_model_applies_only_where_the_file_sets_none_and_a_bad_legacy_effort_is_an_error() {
    // given
    let legacy = LegacyEnv { claude_model: Some("legacy".into()), ..LegacyEnv::default() };
    let global =
        format!(r#"{{"version": 1, "revision": "{REVISION}", "models": {{"claude": "global"}}, "tasks": {{}}}}"#);
    let bad_effort = LegacyEnv { codex_reasoning_effort: Some("turbo".into()), ..LegacyEnv::default() };
    // when / then
    assert_eq!(resolve(None, legacy.clone()).unwrap().model, "legacy");
    assert_eq!(resolve(Some(&global), legacy).unwrap().model, "global");
    assert!(resolve(None, bad_effort).is_err());
}

#[test]
fn malformed_settings_are_errors_so_the_supervisor_fails_closed() {
    // given
    let bodies = [
        "not json".to_owned(),
        format!(r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}}}"#),
        r#"{"version": 1, "revision": "defaults", "models": {}, "tasks": {}}"#.to_owned(),
        format!(r#"{{"version": 2, "revision": "{REVISION}", "models": {{}}, "tasks": {{}}}}"#),
        format!(r#"{{"version": 1, "revision": "{REVISION}", "models": {{"claude": " "}}, "tasks": {{}}}}"#),
        format!(
            r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"title": {{"prompt": "{{oops}}"}}}}}}"#
        ),
        format!(
            r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"enabled": "no"}}}}}}"#
        ),
    ];
    for body in bodies {
        // when / then
        assert!(resolve(Some(&body), LegacyEnv::default()).is_err(), "{body}");
    }
}

#[test]
fn retired_openrouter_entries_are_ignored_without_failing_the_read() {
    // given
    let body = format!(
        r#"{{"version": 1, "revision": "{REVISION}", "models": {{"openrouter": "x"}},
        "tasks": {{"anchor": {{}}, "copilot": {{"models": {{"openrouter": "y"}}}}}}}}"#
    );
    // when / then
    assert_eq!(resolve(Some(&body), LegacyEnv::default()), Ok(task(true, "claude-opus-5-5", "")));
}

#[test]
fn utf16_settings_given_copilot_off_when_resolved_then_switch_is_read_as_the_web_store_reads_it() {
    // given: json.loads detects UTF-16 by its BOM, and so does recog-ai's store
    let body = format!(
        r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"enabled": false}}}}}}"#
    );
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(body.encode_utf16().flat_map(u16::to_le_bytes));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ai-settings.json"), bytes).unwrap();
    // when
    let resolved = AiSettingsFile::new(dir.path(), LegacyEnv::default()).resolve();
    // then
    assert_eq!(resolved, Ok(task(false, "claude-opus-5-5", "")));
}

#[test]
fn unreadable_settings_given_directory_in_its_place_when_resolved_then_error_names_the_file() {
    // given
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("ai-settings.json")).unwrap();
    // when
    let error = AiSettingsFile::new(dir.path(), LegacyEnv::default()).resolve().unwrap_err();
    // then
    assert!(error.contains("ai-settings.json"), "{error}");
}

fn at_level(access: &str) -> String {
    format!(r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{}}, "access": "{access}"}}"#)
}

#[test]
fn access_manual_when_resolved_then_enabled_and_not_automatic() {
    let task = resolve(Some(&at_level("manual")), LegacyEnv::default()).unwrap();
    assert!(task.enabled);
    assert!(!task.automatic);
}

#[test]
fn access_off_when_resolved_then_disabled() {
    let task = resolve(Some(&at_level("off")), LegacyEnv::default()).unwrap();
    assert!(!task.enabled);
    assert!(!task.automatic);
}
