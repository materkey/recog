//! tests/test_ai_settings.py and the settings half of tests/test_claude_effort.py, on
//! temporary data directories only.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Barrier};

use recog_ai::AiError;
use recog_ai::defaults::{COPILOT_PROMPT, DEFAULT_MODELS};
use recog_ai::install::LegacyEnv;
use recog_ai::json::{Json, object};
use recog_ai::settings::{AiSettingsStore, Fallback, ResolvedTask, render_prompt, variables};

fn store_in(dir: &Path, legacy: LegacyEnv) -> AiSettingsStore {
    AiSettingsStore::new(dir.join("data"), Some(dir.join("prompts"))).with_legacy_env(legacy)
}

fn store(dir: &Path) -> AiSettingsStore {
    store_in(dir, LegacyEnv::default())
}

fn json(text: &str) -> Json {
    Json::parse(text).unwrap()
}

fn resolve(store: &AiSettingsStore, task_id: &str) -> ResolvedTask {
    store.resolve_task(task_id, None, Fallback::default()).unwrap()
}

fn task_ids(store: &AiSettingsStore) -> Vec<String> {
    store.view().unwrap().tasks.into_iter().map(|task| task.id).collect()
}

fn write_prompt(store: &AiSettingsStore, name: &str, text: &str) {
    std::fs::create_dir_all(&store.prompts_dir).unwrap();
    std::fs::write(store.prompts_dir.join(name), text).unwrap();
}

fn uuid() -> String {
    "0f8fad5b-d9cb-469f-a165-70867728950e".to_owned()
}

#[test]
fn missing_settings_when_read_then_defaults_without_creating_data() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    assert_eq!(
        Json::Object(store.read().unwrap()),
        json(r#"{"version": 1, "revision": "defaults", "models": {}, "tasks": {}}"#)
    );
    let view = store.view().unwrap();
    let defaults: Vec<(String, String)> = DEFAULT_MODELS.iter().map(|(p, m)| (p.to_string(), m.to_string())).collect();
    assert_eq!(view.models.into_iter().collect::<Vec<_>>(), defaults);
    assert_eq!(task_ids(&store), ["analysis.increment", "title", "meeting_chat", "copilot"]);
    assert!(!store.path.parent().unwrap().exists());
}

#[test]
fn increment_registry_given_catalog_when_configured_then_one_entry_controls_both_paths() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    write_prompt(&store, "my-increment.md", "---\nname: My increment\n---\nShared {transcript}");
    let ids = task_ids(&store);
    assert_eq!(ids.iter().filter(|id| *id == "analysis.increment").count(), 1);
    assert!(!ids.contains(&"saved:my-increment".to_owned()));
    assert_eq!(resolve(&store, "analysis.increment").prompt, "Shared {transcript}");
    assert_eq!(resolve(&store, "saved:my-increment").prompt, "Shared {transcript}");
    let values = json(r#"{"prompt": "Edited {transcript}", "providers": ["claude"], "models": {"claude": "chosen"}}"#);
    store.write_task("analysis.increment", &values, None).unwrap();
    for task_id in ["analysis.increment", "saved:my-increment"] {
        let task = resolve(&store, task_id);
        assert_eq!(task.prompt, "Edited {transcript}");
        assert_eq!(task.providers, ["claude"]);
        assert_eq!(task.models["claude"], "chosen");
    }
    store.write_task("analysis.increment", &json(r#"{"providers": ["claude", "codex"]}"#), None).unwrap();
    assert_eq!(resolve(&store, "saved:my-increment").providers, ["codex"]);
}

#[test]
fn legacy_saved_increment_given_read_and_reset_when_aliased_then_preferences_survive_until_reset() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    let revision = uuid();
    let raw = format!(
        r#"{{"version": 1, "revision": "{revision}", "models": {{}}, "tasks": {{"saved:my-increment": {{"prompt": "Legacy {{transcript}}", "claude_effort": "max"}}}}}}"#
    );
    std::fs::write(&store.path, &raw).unwrap();
    assert_eq!(resolve(&store, "analysis.increment").prompt, "Legacy {transcript}");
    assert_eq!(resolve(&store, "analysis.increment").claude_effort, "max");
    assert_eq!(std::fs::read_to_string(&store.path).unwrap(), raw);
    store.reset_task("analysis.increment", Some(&revision)).unwrap();
    assert!(!resolve(&store, "saved:my-increment").customized);
}

#[test]
fn retired_features_given_legacy_file_when_loaded_then_active_settings_remain_usable() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    let raw = format!(
        r#"{{"version": 1, "revision": "{}", "models": {{"openrouter": "old", "codex": "kept"}}, "tasks": {{
        "anchor": {{"prompt": "Removed {{transcript}}"}}, "analysis.custom": {{"prompt": "Removed {{transcript}}"}},
        "analysis.summary": {{"prompt": "Retired summary {{transcript}}"}},
        "title": {{"providers": ["openrouter"], "models": {{"openrouter": "old"}}, "prompt": "Keep {{transcript}}"}}}}}}"#,
        uuid()
    );
    std::fs::write(&store.path, &raw).unwrap();
    let view = store.view().unwrap();
    assert_eq!(view.models.keys().collect::<Vec<_>>(), ["claude", "codex"]);
    assert_eq!(view.models["codex"], "kept");
    assert_eq!(task_ids(&store), ["analysis.increment", "title", "meeting_chat", "copilot"]);
    assert_eq!(resolve(&store, "title").providers, ["codex"]);
    assert_eq!(resolve(&store, "title").prompt, "Keep {transcript}");
    assert_eq!(std::fs::read_to_string(&store.path).unwrap(), raw);
    assert!(store.write_models(&json(r#"{"openrouter": "removed"}"#), None).unwrap_err().is_value_error());
    assert!(
        store.write_task("anchor", &json(r#"{"prompt": "Removed {transcript}"}"#), None).unwrap_err().is_value_error()
    );
    assert!(store.resolve_task("analysis.summary", None, Fallback::default()).unwrap_err().is_value_error());
    let retired = store.write_task("analysis.summary", &json(r#"{"prompt": "Retired {transcript}"}"#), None);
    assert!(retired.unwrap_err().is_value_error());
}

#[test]
fn retired_provider_beside_a_non_object_task_given_saved_file_when_loaded_then_settings_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    let raw = format!(
        r#"{{"version": 1, "revision": "{}", "models": {{}}, "tasks": {{"title": "not a task",
        "analysis.increment": {{"providers": ["openrouter"]}}}}}}"#,
        uuid()
    );
    std::fs::write(&store.path, &raw).unwrap();
    assert!(matches!(store.view(), Err(AiError::SettingsRead(_))));
    assert_eq!(std::fs::read_to_string(&store.path).unwrap(), raw);
}

#[test]
fn codex_auto_given_scenario_when_saved_then_empty_effort_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.write_task("analysis.increment", &json(r#"{"reasoning_effort": ""}"#), None).unwrap();
    assert_eq!(resolve(&store, "analysis.increment").reasoning_effort, "");
    assert_eq!(resolve(&store, "saved:my-increment").reasoning_effort, "");
}

#[test]
fn global_and_task_overrides_when_resolved_then_precedence_and_blank_inheritance() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = LegacyEnv {
        claude_model: Some("legacy-claude".into()),
        codex_model: Some("legacy-codex".into()),
        codex_reasoning_effort: None,
    };
    let store = store_in(dir.path(), legacy);
    assert_eq!(store.view().unwrap().models["claude"], "legacy-claude");
    let first =
        store.write_models(&json(r#"{"claude": "global-claude", "codex": "global-codex"}"#), Some("defaults")).unwrap();
    store.write_task("title", &json(r#"{"models": {"codex": "title-model"}}"#), Some(&first.revision)).unwrap();
    assert_eq!(resolve(&store, "title").models["codex"], "title-model");
    assert_eq!(resolve(&store, "analysis.increment").models["codex"], "global-codex");
    store.write_task("title", &json(r#"{"models": {"codex": " "}}"#), None).unwrap();
    assert_eq!(resolve(&store, "title").models["codex"], "global-codex");
    store.write_models(&json(r#"{"codex": ""}"#), None).unwrap();
    assert_eq!(resolve(&store, "title").models["codex"], "legacy-codex");
    assert_eq!(store.view().unwrap().models["claude"], "global-claude");
}

#[test]
fn saved_models_when_legacy_value_is_invalid_then_saved_value_wins() {
    let dir = tempfile::tempdir().unwrap();
    store(dir.path()).write_models(&json(r#"{"codex": "saved"}"#), None).unwrap();
    let legacy = LegacyEnv { codex_model: Some("invalid legacy value".into()), ..Default::default() };
    assert_eq!(store_in(dir.path(), legacy.clone()).view().unwrap().models["codex"], "saved");
    // Without a saved value the invalid legacy one fails visibly, as in Python.
    let fresh = tempfile::tempdir().unwrap();
    assert!(store_in(fresh.path(), legacy).view().unwrap_err().is_value_error());
}

#[test]
fn task_override_when_replaced_and_reset_then_only_selected_task_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.write_models(&json(r#"{"codex": "global"}"#), None).unwrap();
    let title = json(r#"{"prompt": "Title {transcript}", "providers": ["claude"], "reasoning_effort": "low"}"#);
    store.write_task("title", &title, None).unwrap();
    store.write_task("analysis.increment", &json(r#"{"prompt": "Anchor {transcript}"}"#), None).unwrap();
    let view = store.write_task("title", &json(r#"{"providers": ["codex"]}"#), None).unwrap();
    let title = resolve(&store, "title");
    assert_eq!(title.prompt, title.default_prompt);
    assert_eq!(title.reasoning_effort, "xhigh");
    let reset = store.reset_task("title", Some(&view.revision)).unwrap();
    assert_ne!(reset.revision, view.revision);
    assert!(!resolve(&store, "title").customized);
    assert_eq!(resolve(&store, "analysis.increment").prompt, "Anchor {transcript}");
    assert_eq!(reset.models["codex"], "global");
}

#[test]
fn legacy_providers_and_effort_when_resolved_then_defaults_retained() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = LegacyEnv { codex_reasoning_effort: Some("high".into()), ..Default::default() };
    let store = store_in(dir.path(), legacy);
    assert_eq!(resolve(&store, "title").reasoning_effort, "high");
    assert_eq!(resolve(&store, "analysis.increment").providers, ["claude", "codex"]);
    assert_eq!(resolve(&store, "copilot").allowed_providers, ["claude"]);
}

#[test]
fn invalid_legacy_effort_when_resolved_then_value_error() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = LegacyEnv { codex_reasoning_effort: Some("turbo".into()), ..Default::default() };
    let error = store_in(dir.path(), legacy).resolve_task("title", None, Fallback::default()).unwrap_err();
    assert_eq!(error.to_string(), "Invalid legacy CODEX_REASONING_EFFORT");
}

#[test]
fn snapshot_when_global_changes_then_all_task_values_keep_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.write_models(&json(r#"{"codex": "old"}"#), None).unwrap();
    let snapshot = store.read().unwrap();
    store.write_models(&json(r#"{"codex": "new"}"#), None).unwrap();
    let resolved = store.resolve_task("title", Some(&snapshot), Fallback::default()).unwrap();
    assert_eq!(resolved.models["codex"], "old");
    assert_eq!(Some(resolved.revision.as_str()), snapshot["revision"].as_str());
}

#[test]
fn saved_registry_when_edited_then_defaults_refresh_without_writing_registry() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    write_prompt(&store, "notes.md", "---\nname: Notes\n---\nSummarize {transcript}");
    let path = store.prompts_dir.join("notes.md");
    assert_eq!(resolve(&store, "saved:notes").label, "Notes");
    assert_eq!(resolve(&store, "saved:notes").prompt, "Summarize {transcript}");
    store.write_task("saved:notes", &json(r#"{"prompt": "Custom {transcript}"}"#), None).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().ends_with("Summarize {transcript}"));
    std::fs::write(&path, "---\nname: Updated\n---\nUpdated {transcript}").unwrap();
    assert_eq!(resolve(&store, "saved:notes").default_prompt, "Updated {transcript}");
    store.reset_task("saved:notes", None).unwrap();
    assert_eq!(resolve(&store, "saved:notes").prompt, "Updated {transcript}");
    assert!(task_ids(&store).contains(&"saved:notes".to_owned()));
    let missing = store.resolve_task("saved:missing", None, Fallback::default()).unwrap_err();
    assert!(missing.to_string().contains("Unknown saved"));
    let fallback = Fallback { prompt: Some("Fallback {transcript}"), label: Some("Generated") };
    assert_eq!(store.resolve_task("saved:generated", None, fallback).unwrap().label, "Generated");
    assert!(store.resolve_task("saved:../outside", None, Fallback::default()).unwrap_err().is_value_error());
}

#[test]
fn invalid_task_input_when_saved_then_no_settings_file() {
    let long_prompt = format!(r#"{{"prompt": "{}{{transcript}}"}}"#, "x".repeat(32001));
    let long_model = format!(r#"{{"models": {{"codex": "{}"}}}}"#, "x".repeat(161));
    let cases = [
        r#"{"unknown": true}"#,
        r#"{"prompt": ""}"#,
        r#"{"prompt": "No transcript"}"#,
        r#"{"prompt": "{transcript} {unknown}"}"#,
        r#"{"prompt": "{transcript!r}"}"#,
        r#"{"prompt": "{transcript.__class__}"}"#,
        &long_prompt,
        r#"{"providers": []}"#,
        r#"{"providers": ["claude", "claude"]}"#,
        r#"{"providers": "claude"}"#,
        r#"{"providers": ["unknown"]}"#,
        r#"{"providers": [null]}"#,
        r#"{"models": {"api_key": "secret"}}"#,
        r#"{"models": {"codex": null}}"#,
        r#"{"models": {"codex": "two words"}}"#,
        r#"{"models": {"codex": "model\n"}}"#,
        r#"{"models": {"codex": "--option"}}"#,
        &long_model,
        r#"{"models": {"codex": "a\u200bb"}}"#,
        r#"{"reasoning_effort": "max"}"#,
        r#"{"reasoning_effort": null}"#,
    ];
    for values in cases {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let error = store.write_task("analysis.increment", &json(values), None).unwrap_err();
        assert!(matches!(error, AiError::Value(_)), "{values}: {error:?}");
        assert!(!store.path.exists(), "{values}");
    }
}

#[test]
fn task_capabilities_when_unsupported_then_rejected() {
    let cases = [
        ("title", r#"{"providers": ["claude", "codex"]}"#),
        ("copilot", r#"{"providers": ["codex"]}"#),
        ("copilot", r#"{"models": {"codex": "model"}}"#),
        ("copilot", r#"{"reasoning_effort": "high"}"#),
        ("missing", r#"{"prompt": "{transcript}"}"#),
    ];
    for (task_id, values) in cases {
        let dir = tempfile::tempdir().unwrap();
        assert!(store(dir.path()).write_task(task_id, &json(values), None).unwrap_err().is_value_error(), "{task_id}");
    }
}

#[test]
fn copilot_literal_policy_when_saved_then_braces_are_not_template_variables() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let prompt = format!("{COPILOT_PROMPT}\nExample: {{unknown}} {{\"json\": true}}");
    store.write_task("copilot", &object([("prompt", Json::from(prompt.as_str()))]), None).unwrap();
    assert_eq!(resolve(&store, "copilot").prompt, prompt);
    assert!(resolve(&store, "copilot").required_placeholders.is_empty());
}

#[test]
fn json_template_and_transcript_braces_when_rendered_then_single_pass_preserves_data() {
    let transcript = r#"{unknown} {user_name} {{nested}} {"json": 1}"#;
    let template = r#"JSON: {"literal": true}, escaped {{unknown}}, user {user_name}, text {transcript}"#;
    let rendered = render_prompt(template, &variables([("transcript", transcript), ("user_name", "Alex")])).unwrap();
    assert_eq!(rendered, format!(r#"JSON: {{"literal": true}}, escaped {{unknown}}, user Alex, text {transcript}"#));
    let all = render_prompt(
        "{user_name}/{meeting_title}/{participants}/{transcript}",
        &variables([("transcript", "speech")]),
    );
    assert_eq!(all.unwrap(), "///speech");
    assert!(render_prompt("{transcript}", &variables([])).unwrap_err().to_string().contains("Missing"));
    assert!(render_prompt("{unknown}", &variables([("unknown", "text")])).unwrap_err().to_string().contains("Unknown"));
}

#[test]
fn nested_literal_json_when_rendered_then_adjacent_closing_braces_remain() {
    let literal = r#"{"outer":{"inner":{"value":1}}}"#;
    let rendered = render_prompt(&format!("{literal} {{transcript}}"), &variables([("transcript", "speech")])).unwrap();
    assert_eq!(rendered, format!("{literal} speech"));
    let escaped = r#"{{"outer":{{"inner":{{"value":1}}}}}}"#;
    assert_eq!(render_prompt(escaped, &variables([])).unwrap(), literal);
}

#[test]
fn revision_conflict_when_parallel_drafts_saved_then_one_wins() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.write_models(&json(r#"{"codex": "initial"}"#), None).unwrap();
    let revision = store.read().unwrap()["revision"].as_str().unwrap().to_owned();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|model| {
            let (store, barrier, revision) = (store.clone(), barrier.clone(), revision.clone());
            std::thread::spawn(move || {
                barrier.wait();
                store.write_models(&object([("codex", Json::from(model))]), Some(&revision))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results.iter().any(|result| matches!(result, Err(AiError::Conflict(_)))));
    let saved = store.read().unwrap()["models"]["codex"].as_str().unwrap().to_owned();
    assert!(saved == "first" || saved == "second");
    assert!(matches!(store.reset_task("title", Some(&revision)), Err(AiError::Conflict(_))));
}

#[test]
fn private_atomic_write_when_successful_then_uuid_and_permissions_persist() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let saved = store.write_models(&json(r#"{"codex": "configured"}"#), None).unwrap();
    assert_eq!(uuid::Uuid::parse_str(&saved.revision).unwrap().to_string(), saved.revision);
    assert_eq!(std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(store.path.with_extension("lock")).unwrap().permissions().mode() & 0o777, 0o600);
    let again = AiSettingsStore::new(store.path.parent().unwrap(), Some(store.prompts_dir.clone()))
        .with_legacy_env(LegacyEnv::default());
    assert_eq!(again.view().unwrap(), saved);
}

#[test]
fn corrupt_settings_when_read_or_written_then_error_preserves_file() {
    let payloads: [Vec<u8>; 5] =
        [b"not json".to_vec(), b"\xff".to_vec(), b"[]".to_vec(), b"{}".to_vec(), vec![b'x'; 256 * 1024 + 1]];
    for payload in payloads {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        std::fs::write(&store.path, &payload).unwrap();
        let operations: [Box<dyn Fn() -> recog_ai::Result<()>>; 4] = [
            Box::new(|| store.read().map(drop)),
            Box::new(|| store.view().map(drop)),
            Box::new(|| store.write_models(&json(r#"{"codex": "new"}"#), None).map(drop)),
            Box::new(|| store.reset_task("title", None).map(drop)),
        ];
        for operation in operations {
            assert!(matches!(operation(), Err(AiError::SettingsRead(_))));
            assert_eq!(std::fs::read(&store.path).unwrap(), payload);
        }
    }
}

#[test]
fn invalid_file_schema_when_read_then_visible_failure() {
    let mutations: [(&str, Json); 6] = [
        ("secret", Json::from("do not accept")),
        ("version", Json::Bool(true)),
        ("revision", Json::from("defaults")),
        ("tasks", json(r#"{"title": {"unknown": true}}"#)),
        ("models", json(r#"{"api_key": "secret"}"#)),
        ("tasks", json(r#"{"title": {"prompt": "{unknown}"}}"#)),
    ];
    for (key, value) in mutations {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.write_models(&json("{}"), None).unwrap();
        let mut raw = store.read().unwrap();
        raw.insert(key.to_owned(), value);
        std::fs::write(&store.path, Json::Object(raw).dumps(None)).unwrap();
        let before = std::fs::read(&store.path).unwrap();
        assert!(
            matches!(store.write_models(&json(r#"{"codex": "new"}"#), None), Err(AiError::SettingsRead(_))),
            "{key}"
        );
        assert_eq!(std::fs::read(&store.path).unwrap(), before);
    }
}

#[test]
fn total_limit_when_many_large_prompts_saved_then_previous_file_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    for number in 0..10 {
        write_prompt(&store, &format!("p{number}.md"), "---\nname: Large\n---\n{transcript}");
    }
    let values = object([("prompt", Json::from(format!("{}{{transcript}}", "x".repeat(31900))))]);
    for number in 0..8 {
        store.write_task(&format!("saved:p{number}"), &values, None).unwrap();
    }
    let before = std::fs::read(&store.path).unwrap();
    let error = store.write_task("saved:p8", &values, None).unwrap_err();
    assert!(error.to_string().contains("size limit"));
    assert_eq!(std::fs::read(&store.path).unwrap(), before);
}

#[test]
fn copilot_switch_when_prompt_saved_or_reset_then_switch_survives() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.write_task("copilot", &json(r#"{"enabled": false}"#), None).unwrap();
    store.write_task("copilot", &json(r#"{"prompt": "Policy"}"#), None).unwrap();
    assert_eq!(resolve(&store, "copilot").enabled, Some(false));
    assert_eq!(resolve(&store, "copilot").prompt, "Policy");
    store.reset_task("copilot", None).unwrap();
    assert_eq!(resolve(&store, "copilot").enabled, Some(false));
    assert_eq!(resolve(&store, "copilot").prompt, COPILOT_PROMPT);
    assert!(store.write_task("title", &json(r#"{"enabled": true}"#), None).is_err());
}

// --- tests/test_claude_effort.py ---

#[test]
fn scenario_given_claude_effort_when_saved_and_reset_then_roundtrips_independently() {
    for task_id in ["analysis.increment", "title", "saved:my-increment", "copilot"] {
        for effort in ["", "low", "medium", "high", "xhigh", "max"] {
            let dir = tempfile::tempdir().unwrap();
            let store = AiSettingsStore::new(dir.path(), None).with_legacy_env(LegacyEnv::default());
            store.write_task(task_id, &object([("claude_effort", Json::from(effort))]), None).unwrap();
            assert_eq!(resolve(&store, task_id).claude_effort, effort);
            store.reset_task(task_id, None).unwrap();
            assert_eq!(resolve(&store, task_id).claude_effort, "");
        }
    }
}

#[test]
fn invalid_claude_effort_given_save_when_validated_then_no_file_created() {
    for value in [Json::from("minimal"), Json::from("auto"), Json::from("--max"), Json::Int(1), Json::Null] {
        let dir = tempfile::tempdir().unwrap();
        let store = AiSettingsStore::new(dir.path(), None).with_legacy_env(LegacyEnv::default());
        let error = store.write_task("copilot", &object([("claude_effort", value)]), None).unwrap_err();
        assert!(error.to_string().contains("Claude effort"));
        assert!(!store.path.exists());
    }
}
