//! tests/test_title_generator.py (and the provenance its orphan-meeting cases check),
//! with stub CLIs or a fake provider on temporary data.

mod common;

use std::path::Path;

use common::{Answer, FakeFactory, KeyedFactory, shell_stub, stub_argv, stub_env, stub_stdin};
use recog_ai::install::LegacyEnv;
use recog_ai::json::Json;
use recog_ai::provider::CliProviderFactory;
use recog_ai::settings::{AiSettingsStore, Fallback, ResolvedTask, variables};
use recog_ai::title::{generate_title, generate_title_blocking, meeting_title_update, sample_transcript};

fn store(dir: &Path) -> AiSettingsStore {
    AiSettingsStore::new(dir, Some(dir.join("prompts"))).with_legacy_env(LegacyEnv::default())
}

fn title_settings(store: &AiSettingsStore) -> ResolvedTask {
    store.resolve_task("title", None, Fallback::default()).unwrap()
}

#[tokio::test]
async fn default_title_settings_when_generated_then_marked_prompt_and_ephemeral_codex() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    shell_stub(
        &bin,
        "codex",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Test Title\"}}\n",
        "",
        0,
    );
    let factory = CliProviderFactory { env: Some(stub_env(&bin, &dir.path().join("home"))) };
    let transcript = format!("[12:00:00] User: {}", "обсуждаем архитектуру ".repeat(4));
    let settings = title_settings(&store(dir.path()));
    let title = generate_title(&transcript, None, Some(&settings), &variables([]), &factory).await;
    assert_eq!(title.map(|title| title.title).as_deref(), Some("Test Title"));
    let argv = stub_argv(&bin, "codex");
    assert_eq!(argv[2], "gpt-6-astra");
    assert!(argv.contains(&"--ephemeral".to_owned()));
    assert_eq!(&argv[argv.len() - 2..], ["--json", "-"]);
    assert!(stub_stdin(&bin, "codex").starts_with("<!-- ccs-automation:recog task=title -->"));
}

#[tokio::test]
async fn saved_claude_title_when_generated_then_selected_model_and_single_pass_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store.write_models(&Json::parse(r#"{"claude": "claude-custom"}"#).unwrap(), None).unwrap();
    let values =
        Json::parse(r#"{"providers": ["claude"], "prompt": "{user_name}: {transcript} / {\"outer\":{\"inner\":1}}"}"#);
    store.write_task("title", &values.unwrap(), None).unwrap();
    let settings = title_settings(&store);
    let transcript = "literal {user_name} {{braces}} ".repeat(4);
    let factory = FakeFactory::new("claude", "claude-custom", None, "  \"Название\"  ");
    let title =
        generate_title(&transcript, None, Some(&settings), &variables([("user_name", "User")]), &*factory).await;
    assert_eq!(title.map(|title| title.title).as_deref(), Some("Название"));
    let calls = factory.calls();
    assert_eq!(calls[0].0.models["claude"], "claude-custom");
    assert_eq!(calls[0].1, "title");
    assert_eq!(factory.prompts()[0], format!("User: {transcript} / {{\"outer\":{{\"inner\":1}}}}"));
}

#[tokio::test]
async fn explicit_model_when_title_generated_then_snapshot_and_saved_settings_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    store
        .write_task("title", &Json::parse(r#"{"providers": ["claude"], "models": {"claude": "saved"}}"#).unwrap(), None)
        .unwrap();
    let settings = title_settings(&store);
    let before = std::fs::read(&store.path).unwrap();
    let factory = FakeFactory::new("claude", "saved", None, &"x".repeat(150));
    let title =
        generate_title(&"speech ".repeat(20), Some("one-call"), Some(&settings), &variables([]), &*factory).await;
    assert_eq!(title.map(|title| title.title), Some("x".repeat(100)));
    assert_eq!(factory.calls()[0].0.models["claude"], "one-call");
    assert_eq!(settings.models["claude"], "saved");
    assert_eq!(std::fs::read(&store.path).unwrap(), before);
}

#[tokio::test]
async fn first_provider_refusing_when_title_generated_then_the_next_ones_title_with_its_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let settings = title_settings(&store(dir.path()));
    assert_eq!(settings.providers, ["codex", "claude"]);
    let factory = KeyedFactory::new(vec![
        ("codex", Answer::Fail("usage limit reached".into())),
        ("claude", Answer::Text("Тема встречи".into())),
    ]);
    let title = generate_title(&"speech ".repeat(20), None, Some(&settings), &variables([]), &factory).await.unwrap();
    assert_eq!(
        (title.title.as_str(), title.title_source.as_str(), title.title_model.as_str()),
        ("Тема встречи", "claude", "claude-model")
    );
    assert_eq!(title.title_settings_revision, settings.revision);
}

#[tokio::test]
async fn provider_failure_when_title_generated_then_none() {
    let dir = tempfile::tempdir().unwrap();
    let settings = title_settings(&store(dir.path()));
    let factory = FakeFactory::failing("codex", "m", "bad settings");
    assert_eq!(generate_title(&"speech ".repeat(20), None, Some(&settings), &variables([]), &*factory).await, None);
}

#[tokio::test]
async fn missing_cli_when_title_generated_then_none() {
    if common::system_cli_exists("codex") {
        eprintln!("skipped: a real codex is installed where resolve_cli falls back to");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let settings = title_settings(&store(dir.path()));
    let empty = dir.path().join("empty");
    let factory = CliProviderFactory { env: Some(stub_env(&empty, &empty)) };
    assert_eq!(generate_title(&"speech ".repeat(20), None, Some(&settings), &variables([]), &factory).await, None);
}

#[tokio::test(start_paused = true)]
async fn hanging_provider_when_title_generated_then_none_after_bounded_timeout() {
    struct Hanging;
    impl recog_ai::provider::AiProvider for Hanging {
        fn key(&self) -> &str {
            "codex"
        }
        fn name(&self) -> String {
            "Hanging".into()
        }
        fn model(&self) -> &str {
            "m"
        }
        fn reasoning_effort(&self) -> Option<&str> {
            None
        }
        fn complete<'a>(&'a self, _prompt: &'a str) -> recog_ai::provider::BoxFuture<'a, recog_ai::Result<String>> {
            Box::pin(std::future::pending())
        }
    }
    struct HangingFactory;
    impl recog_ai::provider::ProviderFactory for HangingFactory {
        fn build(
            &self,
            _settings: &ResolvedTask,
            _task: &str,
            _provider: Option<&str>,
        ) -> recog_ai::Result<Box<dyn recog_ai::provider::AiProvider>> {
            Ok(Box::new(Hanging))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let settings = title_settings(&store(dir.path()));
    let started = tokio::time::Instant::now();
    let title = generate_title(&"speech ".repeat(20), None, Some(&settings), &variables([]), &HangingFactory).await;
    assert_eq!(title, None);
    assert_eq!(started.elapsed(), recog_ai::title::TITLE_TIMEOUT);
}

#[tokio::test]
async fn short_transcript_when_title_requested_then_no_provider_or_settings_read() {
    let factory = FakeFactory::new("codex", "m", None, "x");
    assert_eq!(generate_title("short", None, None, &variables([]), &*factory).await, None);
    assert!(factory.calls().is_empty());
}

#[test]
fn long_transcript_when_sampled_then_beginning_middle_and_end_are_represented() {
    let transcript: Vec<String> = (0..120).map(|index| format!("[{index:04}] {}", "words ".repeat(20))).collect();
    let transcript = transcript.join("\n");
    let sampled = sample_transcript(&transcript, 6000, 6);
    assert!(sampled.contains("[0000]") && sampled.contains("[0060]") && sampled.contains("[0100]"));
    assert!(sampled.len() < transcript.len());
    assert_eq!(sample_transcript("short", 6000, 6), "short");
}

#[test]
fn sync_title_when_given_snapshot_then_forwards_settings_and_variables() {
    let dir = tempfile::tempdir().unwrap();
    let settings = title_settings(&store(dir.path()));
    let factory = FakeFactory::new("codex", "m", None, "Title");
    let vars = variables([("user_name", "User")]);
    let title = generate_title_blocking(&"speech ".repeat(20), None, Some(&settings), &vars, &*factory);
    assert_eq!(title.map(|title| title.title).as_deref(), Some("Title"));
    assert_eq!(factory.calls()[0].0, settings);
}

fn write_meeting(meeting: &Path, meta: &str, rows: &[&str]) {
    std::fs::create_dir_all(meeting).unwrap();
    std::fs::write(meeting.join("meta.json"), meta).unwrap();
    std::fs::write(meeting.join("transcript.jsonl"), rows.join("\n") + "\n").unwrap();
}

#[tokio::test]
async fn orphan_title_when_generated_then_actual_provider_model_revision_returned() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let saved = store
        .write_task(
            "title",
            &Json::parse(r#"{"providers": ["claude"], "models": {"claude": "meeting-model"}}"#).unwrap(),
            None,
        )
        .unwrap();
    let meeting = dir.path().join("meetings/m1");
    write_meeting(
        &meeting,
        r#"{"title": null, "participants": {"local": {"name": "User", "is_local": true}, "p2": {"name": "Пётр"}}}"#,
        &[
            r#"{"id": 1, "ts_display": "12:00:00", "speaker": "User", "speaker_id": "local", "text": "Обсуждаем архитектуру системы и все необходимые изменения."}"#,
            r#"{"id": 2, "speaker_id": "__system__", "text": "marker"}"#,
        ],
    );
    let factory = FakeFactory::new("claude", "meeting-model", None, "Архитектура системы");
    let update = meeting_title_update(&store, &meeting, &*factory).await.unwrap();
    assert_eq!(update.title, "Архитектура системы");
    assert_eq!(update.title_source, "claude");
    assert_eq!(update.title_model, "meeting-model");
    assert_eq!(update.title_settings_revision, saved.revision);
    let prompt = &factory.prompts()[0];
    assert!(prompt.contains("[12:00:00] User: Обсуждаем") && !prompt.contains("marker"));
}

#[tokio::test]
async fn corrupt_title_settings_when_orphan_ends_then_no_title_and_no_provider() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ai-settings.json"), "broken settings").unwrap();
    let meeting = dir.path().join("meetings/m1");
    write_meeting(
        &meeting,
        r#"{"participants": {}}"#,
        &[r#"{"id": 1, "speaker": "User", "text": "Обсуждаем архитектуру системы и все необходимые изменения."}"#],
    );
    let factory = FakeFactory::new("claude", "m", None, "Title");
    assert_eq!(meeting_title_update(&store(dir.path()), &meeting, &*factory).await, None);
    assert!(factory.calls().is_empty());
}

const LONG_ROW: &str = r#"{"id": 1, "ts_display": "12:00:00", "speaker": "User", "text": "Обсуждаем архитектуру системы и все необходимые изменения."}"#;

#[tokio::test]
async fn meeting_names_and_title_when_orphan_titled_then_prompt_variables_follow_meta() {
    let dir = tempfile::tempdir().unwrap();
    let prompt = r#"{"prompt": "U={user_name} P={participants} T={meeting_title} {transcript}"}"#;
    store(dir.path()).write_task("title", &Json::parse(prompt).unwrap(), None).unwrap();
    let meeting = dir.path().join("meetings/m1");
    let meta = r#"{"title": "Old", "participants": {"local": {"name": "User", "is_local": true}, "p2": {"name": "Пётр"}, "p3": {"is_local": 1}}}"#;
    write_meeting(&meeting, meta, &[LONG_ROW]);
    let factory = FakeFactory::new("codex", "m", None, "New title");
    assert_eq!(meeting_title_update(&store(dir.path()), &meeting, &*factory).await.unwrap().title, "New title");
    assert!(factory.prompts()[0].starts_with("U=User, p3 P=User, Пётр, p3 T=Old [12:00:00] User: Обсуждаем"));
}

#[tokio::test]
async fn meta_without_participants_when_orphan_titled_then_names_are_empty() {
    let dir = tempfile::tempdir().unwrap();
    let prompt = r#"{"prompt": "U=[{user_name}] P=[{participants}] {transcript}"}"#;
    store(dir.path()).write_task("title", &Json::parse(prompt).unwrap(), None).unwrap();
    let meeting = dir.path().join("meetings/m1");
    write_meeting(&meeting, r#"{"title": "Old"}"#, &[LONG_ROW]);
    let factory = FakeFactory::new("codex", "m", None, "New title");
    assert_eq!(meeting_title_update(&store(dir.path()), &meeting, &*factory).await.unwrap().title, "New title");
    assert!(factory.prompts()[0].starts_with("U=[] P=[] [12:00:00] User: Обсуждаем"), "{}", factory.prompts()[0]);
}

#[tokio::test]
async fn unreadable_meta_when_orphan_titled_then_no_title_and_no_provider() {
    let metas = [
        "not json",
        "[]",
        r#"{"title": 5}"#,
        r#"{"participants": []}"#,
        r#"{"participants": {"p": 5}}"#,
        r#"{"participants": {"p": {"name": 5}}}"#,
    ];
    for meta in metas {
        let dir = tempfile::tempdir().unwrap();
        let meeting = dir.path().join("meetings/m1");
        write_meeting(&meeting, meta, &[LONG_ROW]);
        let factory = FakeFactory::new("codex", "m", None, "Title");
        assert_eq!(meeting_title_update(&store(dir.path()), &meeting, &*factory).await, None, "{meta}");
        assert!(factory.calls().is_empty(), "{meta}");
    }
    let dir = tempfile::tempdir().unwrap();
    let meeting = dir.path().join("meetings/m1");
    write_meeting(&meeting, "{}", &[LONG_ROW]);
    std::fs::remove_file(meeting.join("meta.json")).unwrap();
    let factory = FakeFactory::new("codex", "m", None, "Title");
    assert_eq!(meeting_title_update(&store(dir.path()), &meeting, &*factory).await, None);
    assert!(factory.calls().is_empty());
}

#[tokio::test]
async fn no_transcript_when_orphan_titled_then_no_title_even_with_broken_settings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ai-settings.json"), "broken settings").unwrap();
    let meeting = dir.path().join("meetings/m1");
    std::fs::create_dir_all(&meeting).unwrap();
    std::fs::write(meeting.join("meta.json"), "{}").unwrap();
    let factory = FakeFactory::new("codex", "m", None, "Title");
    assert_eq!(meeting_title_update(&store(dir.path()), &meeting, &*factory).await, None);
    assert!(factory.calls().is_empty());
}
