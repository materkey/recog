//! tests/test_increment.py with a fake provider factory on temporary meetings.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::FakeFactory;
use recog_ai::AiError;
use recog_ai::increment::IncrementGenerator;
use recog_ai::install::LegacyEnv;
use recog_ai::json::Json;
use recog_ai::prompt_registry::PromptConfig;
use recog_ai::settings::AiSettingsStore;

fn default_prompt() -> PromptConfig {
    PromptConfig {
        id: "my-increment".into(),
        name: "My increment".into(),
        filter: None,
        template: "Участник: {user_name}\nТранскрипт:\n{transcript}".into(),
    }
}

fn default_segments() -> Vec<Json> {
    [
        r#"{"id": 1, "ts_display": "14:30:01", "speaker": "Вася", "text": "Привет"}"#,
        r#"{"id": 2, "ts_display": "14:30:05", "speaker": "Петя", "text": "Привет всем"}"#,
        r#"{"id": 3, "ts_display": "14:30:10", "speaker": "Вася", "text": "Начинаем"}"#,
    ]
    .iter()
    .map(|line| Json::parse(line).unwrap())
    .collect()
}

fn make_meeting(base: &Path, meeting_id: &str, segments: Option<Vec<Json>>) -> PathBuf {
    let dir = base.join(meeting_id);
    std::fs::create_dir_all(&dir).unwrap();
    let meta = format!(
        r#"{{"id": "{meeting_id}", "title": "Daily sync", "created_at": "2026-03-21T14:30:00+00:00",
        "participants": {{"speaker_1": {{"name": "Вася", "is_local": true}}, "speaker_2": {{"name": "Петя", "is_local": false}}}}}}"#
    );
    std::fs::write(dir.join("meta.json"), meta).unwrap();
    let lines: Vec<String> =
        segments.unwrap_or_else(default_segments).iter().map(|segment| segment.dumps(None)).collect();
    std::fs::write(dir.join("transcript.jsonl"), lines.join("\n") + "\n").unwrap();
    dir
}

fn numbered(count: i64) -> Vec<Json> {
    (1..=count)
        .map(|id| {
            Json::parse(&format!(
                r#"{{"id": {id}, "ts_display": "14:30:{id:02}", "speaker": "Вася", "text": "Seg {id}"}}"#
            ))
            .unwrap()
        })
        .collect()
}

fn make_v1_meta(dir: &Path, start: i64, end: i64) {
    std::fs::create_dir_all(dir).unwrap();
    let meta = format!(
        r#"{{"version": 1, "generated_at": "2026-03-21T14:45:00+00:00", "provider": "codex", "model": "gpt-5.4",
        "reasoning_effort": "xhigh", "segment_range": [{start}, {end}], "segment_count": {}, "status": "completed",
        "duration_seconds": 5.0, "error": null}}"#,
        end - start + 1
    );
    std::fs::write(dir.join("v1_meta.json"), meta).unwrap();
    std::fs::write(dir.join("v1.md"), "---\nmeeting: Daily sync\nversion: 1\n---\n\n## Задачи\n- Task 1\n").unwrap();
}

fn generator(base: &Path, prompts: Option<PathBuf>, factory: &Arc<FakeFactory>) -> IncrementGenerator {
    let prompts = prompts.unwrap_or_else(|| base.join("no-prompts"));
    IncrementGenerator::new(base, Some(prompts)).with_factory(factory.clone()).with_legacy_env(LegacyEnv::default())
}

fn read_json(path: &Path) -> Json {
    Json::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const MEETING: &str = "2026-03-21_test";

#[tokio::test]
async fn meeting_without_increments_when_generated_then_v1_files_in_prompt_directory() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = FakeFactory::new("codex", "gpt-5.4", Some("xhigh"), "## Задачи\n- Сделать ревью\n");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), Some(1));
    let calls = factory.calls();
    assert_eq!(calls[0].1, "increment");
    assert_eq!(calls[0].0.id, "saved:my-increment");

    let inc = meeting.join("increments/my-increment");
    let md = std::fs::read_to_string(inc.join("v1.md")).unwrap();
    for expected in
        ["---", "meeting: \"Daily sync\"", &format!("meeting_id: {MEETING}"), "date: \"2026-03-21\"", "version: 1"]
    {
        assert!(md.contains(expected), "{expected}: {md}");
    }
    assert!(md.contains("segment_range: [1, 3]") && md.contains("## Задачи") && md.contains("Сделать ревью"));
    let meta = read_json(&inc.join("v1_meta.json"));
    assert_eq!(meta["version"], Json::Int(1));
    assert_eq!(meta["status"], Json::from("completed"));
    assert_eq!(meta["segment_range"], Json::parse("[1, 3]").unwrap());
    assert_eq!(meta["segment_count"], Json::Int(3));
    assert_eq!(meta["error"], Json::Null);
    assert_eq!(meta["provider"], Json::from("codex"));
    assert_eq!(meta["prompt_id"], Json::from("my-increment"));
    assert_eq!(meta["prompt_name"], Json::from("My increment"));
    assert_eq!(meta["participant_id"], Json::Null);
    assert_eq!(meta["participant_name"], Json::from("Вася"));
    let prompt = &factory.prompts()[0];
    assert!(prompt.contains("Привет") && prompt.contains("Начинаем"));
}

#[tokio::test]
async fn local_participants_one_without_a_name_when_generated_then_the_unnamed_is_recorded_by_id() {
    // given
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    std::fs::write(
        meeting.join("meta.json"),
        r#"{"id": "m", "title": "Daily sync", "created_at": "2026-03-21T14:30:00+00:00",
        "participants": {"speaker_1": {"name": "Вася", "is_local": true}, "mic": {"is_local": true}}}"#,
    )
    .unwrap();
    let factory = FakeFactory::new("codex", "gpt-5.4", Some("xhigh"), "## Задачи\n- Сделать ревью\n");
    // when
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    // then
    assert_eq!(version.unwrap(), Some(1));
    let meta = read_json(&meeting.join("increments/my-increment/v1_meta.json"));
    assert_eq!(meta["participant_name"], Json::from("Вася, mic"));
}

#[tokio::test]
async fn other_participant_when_generated_then_uses_their_name_and_metadata() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = FakeFactory::new("codex", "gpt-5.6-sol", Some("xhigh"), "## Задачи\n- Сделать ревью\n");
    let version = generator(dir.path(), None, &factory)
        .generate(MEETING, &meeting, &default_prompt(), false, Some("speaker_2"))
        .await;
    assert_eq!(version.unwrap(), Some(1));
    assert!(factory.prompts()[0].contains("Участник: Петя"));
    let meta = read_json(&meeting.join("increments/my-increment/v1_meta.json"));
    assert_eq!(meta["participant_id"], Json::from("speaker_2"));
    assert_eq!(meta["participant_name"], Json::from("Петя"));
    assert_eq!(meta["model"], Json::from("gpt-5.6-sol"));
}

#[tokio::test]
async fn unknown_participant_when_generated_then_value_error_and_nothing_claimed() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let error = generator(dir.path(), None, &factory)
        .generate(MEETING, &meeting, &default_prompt(), false, Some("nobody"))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Participant 'nobody' not found");
    assert!(!meeting.join("increments").exists());
}

#[tokio::test]
async fn local_increment_exists_when_other_participant_generated_then_full_transcript_is_used() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    make_v1_meta(&meeting.join("increments/my-increment"), 1, 3);
    let factory = FakeFactory::new("codex", "gpt-5.6-sol", Some("xhigh"), "## Задачи\n");
    let version = generator(dir.path(), None, &factory)
        .generate(MEETING, &meeting, &default_prompt(), false, Some("speaker_2"))
        .await;
    assert_eq!(version.unwrap(), Some(2));
    let prompt = &factory.prompts()[0];
    assert!(prompt.contains("Привет") && prompt.contains("Начинаем"));
}

#[tokio::test]
async fn v1_covers_first_segments_when_generated_then_v2_has_only_the_delta() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, Some(numbered(8)));
    let inc = meeting.join("increments/my-increment");
    make_v1_meta(&inc, 1, 5);
    let factory = FakeFactory::new("codex", "gpt-5.4", Some("xhigh"), "## Задачи\n- Delta task\n");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), Some(2));
    let prompt = &factory.prompts()[0];
    assert!(prompt.contains("Seg 6") && prompt.contains("Seg 7") && prompt.contains("Seg 8"));
    assert!(!prompt.contains("Seg 1") && !prompt.contains("Seg 5"));
    let meta = read_json(&inc.join("v2_meta.json"));
    assert_eq!(meta["version"], Json::Int(2));
    assert_eq!(meta["segment_range"], Json::parse("[6, 8]").unwrap());
    assert_eq!(meta["segment_count"], Json::Int(3));
}

#[tokio::test]
async fn zero_delta_when_generated_then_skipped_without_provider_or_files() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, Some(numbered(5)));
    let inc = meeting.join("increments/my-increment");
    make_v1_meta(&inc, 1, 5);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), None);
    assert!(factory.prompts().is_empty());
    assert!(!inc.join("v2_meta.json").exists() && !inc.join("v2.md").exists());
}

#[tokio::test]
async fn provider_failure_when_generated_then_failed_meta_without_markdown() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = FakeFactory::failing("codex", "gpt-5.4", "Codex CLI error: timeout");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), None);
    let inc = meeting.join("increments/my-increment");
    let meta = read_json(&inc.join("v1_meta.json"));
    assert_eq!(meta["status"], Json::from("failed"));
    assert!(meta["error"].as_str().unwrap().contains("timeout"));
    assert_eq!(meta["prompt_id"], Json::from("my-increment"));
    assert_eq!(meta["provider"], Json::from("codex"));
    assert!(!inc.join("v1.md").exists());
}

#[tokio::test]
async fn first_provider_refusing_when_generated_then_the_next_one_writes_the_version_and_is_named() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = Arc::new(common::KeyedFactory::new(vec![
        ("codex", common::Answer::Fail("usage limit reached".into())),
        ("claude", common::Answer::Text("## Итоги".into())),
    ]));
    let generator = IncrementGenerator::new(dir.path(), Some(dir.path().join("no-prompts")))
        .with_factory(factory.clone())
        .with_legacy_env(LegacyEnv::default());
    let version = generator.generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), Some(1));
    let inc = meeting.join("increments/my-increment");
    let meta = read_json(&inc.join("v1_meta.json"));
    assert_eq!(meta["provider"], Json::from("claude"));
    assert_eq!(meta["model"], Json::from("claude-model"));
    assert!(std::fs::read_to_string(inc.join("v1.md")).unwrap().ends_with("## Итоги"));
    assert_eq!(factory.asked(), ["codex", "claude"]);
}

#[tokio::test]
async fn every_provider_refusing_when_generated_then_failed_meta_names_the_last_one_tried() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = Arc::new(common::KeyedFactory::new(vec![
        ("codex", common::Answer::Fail("codex limit".into())),
        ("claude", common::Answer::Fail("claude limit".into())),
    ]));
    let generator = IncrementGenerator::new(dir.path(), Some(dir.path().join("no-prompts")))
        .with_factory(factory)
        .with_legacy_env(LegacyEnv::default());
    assert_eq!(generator.generate(MEETING, &meeting, &default_prompt(), false, None).await.unwrap(), None);
    let meta = read_json(&meeting.join("increments/my-increment/v1_meta.json"));
    assert_eq!(meta["status"], Json::from("failed"));
    assert_eq!(meta["provider"], Json::from("claude"));
    assert!(meta["error"].as_str().unwrap().contains("claude limit"));
}

#[tokio::test]
async fn failed_version_when_generated_again_then_counter_moves_past_it_and_segments_are_retried() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let failing = FakeFactory::failing("codex", "m", "boom");
    generator(dir.path(), None, &failing).generate(MEETING, &meeting, &default_prompt(), false, None).await.unwrap();
    let factory = FakeFactory::new("codex", "m", None, "ok");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), Some(2));
    let meta = read_json(&meeting.join("increments/my-increment/v2_meta.json"));
    assert_eq!(meta["segment_range"], Json::parse("[1, 3]").unwrap());
}

#[tokio::test]
async fn stale_generating_placeholder_when_generated_then_marked_failed_and_segments_retried() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let inc = meeting.join("increments/my-increment");
    std::fs::create_dir_all(&inc).unwrap();
    let stale = r#"{"version": 1, "participant_id": null, "status": "generating", "generated_at": "2026-03-21T14:45:00+00:00", "segment_range": [1, 3]}"#;
    std::fs::write(inc.join("v1_meta.json"), stale).unwrap();
    let factory = FakeFactory::new("codex", "m", None, "ok");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), Some(2));
    let old = read_json(&inc.join("v1_meta.json"));
    assert_eq!(old["status"], Json::from("failed"));
    assert_eq!(old["error"], Json::from("stale generating placeholder (process crash recovery)"));
}

#[tokio::test]
async fn fresh_generating_placeholder_when_generated_then_its_segments_are_not_duplicated() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let inc = meeting.join("increments/my-increment");
    std::fs::create_dir_all(&inc).unwrap();
    let fresh = format!(
        r#"{{"version": 1, "participant_id": null, "status": "generating", "generated_at": "{}", "segment_range": [1, 3]}}"#,
        recog_ai::timestamp::utc_now_isoformat()
    );
    std::fs::write(inc.join("v1_meta.json"), fresh).unwrap();
    let factory = FakeFactory::new("codex", "m", None, "ok");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), None);
    assert!(factory.prompts().is_empty());
}

#[tokio::test]
async fn malformed_transcript_line_when_generated_then_error_names_the_line() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    std::fs::write(meeting.join("transcript.jsonl"), "{\"id\": 1, \"text\": \"a\"}\n{broken\n").unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    let error = generator(dir.path(), None, &factory)
        .generate(MEETING, &meeting, &default_prompt(), false, None)
        .await
        .unwrap_err();
    assert!(error.to_string().starts_with("Malformed JSONL in transcript.jsonl line 2:"), "{error}");
}

#[tokio::test]
async fn only_events_in_delta_when_generated_then_placeholder_removed_and_skipped() {
    let dir = common::meetings_dir();
    let segments = vec![Json::parse(r#"{"id": 1, "event_type": "gap", "text": "marker"}"#).unwrap()];
    let meeting = make_meeting(dir.path(), MEETING, Some(segments));
    let factory = FakeFactory::new("codex", "m", None, "x");
    let version =
        generator(dir.path(), None, &factory).generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), None);
    assert!(!meeting.join("increments/my-increment/v1_meta.json").exists());
}

#[tokio::test]
async fn missing_meta_or_transcript_when_generated_then_skipped() {
    let dir = common::meetings_dir();
    let meeting = dir.path().join("empty");
    std::fs::create_dir_all(&meeting).unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    let generator = generator(dir.path(), None, &factory);
    assert_eq!(generator.generate("empty", &meeting, &default_prompt(), false, None).await.unwrap(), None);
    std::fs::write(meeting.join("meta.json"), "{}").unwrap();
    assert_eq!(generator.generate("empty", &meeting, &default_prompt(), false, None).await.unwrap(), None);
}

#[test]
fn two_versions_when_listed_then_sorted_by_version() {
    let dir = common::meetings_dir();
    let inc = dir.path().join(MEETING).join("increments/my-increment");
    std::fs::create_dir_all(&inc).unwrap();
    for version in [2, 1] {
        let meta = format!(r#"{{"version": {version}, "status": "completed", "segment_range": [1, {}]}}"#, version * 3);
        std::fs::write(inc.join(format!("v{version}_meta.json")), meta).unwrap();
    }
    std::fs::write(inc.join("v3_meta.json"), "{broken").unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    let result = generator(dir.path(), None, &factory).list_increments(MEETING, "my-increment").unwrap();
    assert_eq!(result.iter().map(|meta| meta["version"].clone()).collect::<Vec<_>>(), [Json::Int(1), Json::Int(2)]);
}

#[test]
fn stored_version_when_fetched_then_meta_and_content() {
    let dir = common::meetings_dir();
    let inc = dir.path().join(MEETING).join("increments/my-increment");
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::write(inc.join("v1_meta.json"), r#"{"version": 1, "status": "completed"}"#).unwrap();
    std::fs::write(inc.join("v1.md"), "---\nversion: 1\n---\n\n## Задачи\n- Task\n").unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    let (meta, content) =
        generator(dir.path(), None, &factory).get_increment(MEETING, "my-increment", 1).unwrap().unwrap();
    assert_eq!(meta["version"], Json::Int(1));
    assert_eq!(meta["status"], Json::from("completed"));
    assert!(content.unwrap().contains("## Задачи"));
}

#[test]
fn no_increments_when_fetched_then_none() {
    let dir = common::meetings_dir();
    std::fs::create_dir_all(dir.path().join(MEETING)).unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    assert!(generator(dir.path(), None, &factory).get_increment(MEETING, "my-increment", 1).unwrap().is_none());
}

#[tokio::test]
async fn custom_prompt_when_generated_then_every_variable_is_substituted() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let prompt = PromptConfig {
        id: "team-facts".into(),
        name: "Team facts".into(),
        filter: None,
        template: "Title: {meeting_title}\nParticipants: {participants}\nUser: {user_name}\n{transcript}".into(),
    };
    let factory = FakeFactory::new("codex", "gpt-5.4", Some("xhigh"), "Facts here");
    let version = generator(dir.path(), None, &factory).generate(MEETING, &meeting, &prompt, false, None).await;
    assert_eq!(version.unwrap(), Some(1));
    let rendered = &factory.prompts()[0];
    for expected in ["Title: Daily sync", "Вася", "Петя", "User: Вася"] {
        assert!(rendered.contains(expected), "{expected}");
    }
    let inc = meeting.join("increments/team-facts");
    assert!(inc.join("v1.md").exists() && inc.join("v1_meta.json").exists());
}

fn write_prompts(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    let prompts = dir.join("prompts");
    std::fs::create_dir_all(&prompts).unwrap();
    for (name, text) in files {
        std::fs::write(prompts.join(name), text).unwrap();
    }
    prompts
}

#[tokio::test]
async fn filtered_and_universal_prompts_when_generating_all_then_both_produce_v1() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let prompts = write_prompts(
        dir.path(),
        &[
            ("my-increment.md", "---\nname: My increment\nfilter: null\n---\n\nUser: {user_name}\n{transcript}"),
            ("sync-only.md", "---\nname: Sync only\nfilter: sync\n---\n\nSync report for {user_name}\n{transcript}"),
        ],
    );
    let factory = FakeFactory::new("codex", "gpt-5.4", Some("xhigh"), "Generated content");
    generator(dir.path(), Some(prompts), &factory).generate_all(MEETING, &meeting).await.unwrap();
    assert!(meeting.join("increments/my-increment/v1.md").exists());
    assert!(meeting.join("increments/sync-only/v1.md").exists());
    assert_eq!(factory.prompts().len(), 2);
}

#[tokio::test]
async fn non_matching_filter_when_generating_all_then_nothing_generated() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let prompts =
        write_prompts(dir.path(), &[("retro-only.md", "---\nname: Retro only\nfilter: retro\n---\n\n{transcript}")]);
    let factory = FakeFactory::new("codex", "m", None, "x");
    generator(dir.path(), Some(prompts), &factory).generate_all(MEETING, &meeting).await.unwrap();
    assert!(factory.prompts().is_empty());
    assert!(!meeting.join("increments/retro-only").exists());
}

#[tokio::test]
async fn callback_when_generation_succeeds_then_receives_meeting_version_and_prompt() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = FakeFactory::new("codex", "gpt-5.4", Some("xhigh"), "Result");
    let seen: Arc<Mutex<Vec<(String, i64, String)>>> = Arc::default();
    let mut generator = generator(dir.path(), None, &factory);
    let sink = seen.clone();
    generator.set_callback(Arc::new(move |meeting: &str, version: i64, prompt: &str| {
        sink.lock().unwrap().push((meeting.to_owned(), version, prompt.to_owned()));
    }));
    generator.generate(MEETING, &meeting, &default_prompt(), false, None).await.unwrap();
    assert_eq!(*seen.lock().unwrap(), [(MEETING.to_owned(), 1, "my-increment".to_owned())]);
}

// --- Migration ---

#[test]
fn flat_increments_when_migrated_then_moved_into_default_prompt_directory() {
    let dir = common::meetings_dir();
    let meeting = dir.path().join(MEETING);
    let inc = meeting.join("increments");
    std::fs::create_dir_all(&inc).unwrap();
    let files = [
        ("v1.md", "# Old v1"),
        ("v1_meta.json", r#"{"version": 1, "status": "completed"}"#),
        ("v2.md", "# Old v2"),
        ("v2_meta.json", r#"{"version": 2, "status": "completed"}"#),
    ];
    for (name, text) in files {
        std::fs::write(inc.join(name), text).unwrap();
    }
    let factory = FakeFactory::new("codex", "m", None, "x");
    generator(dir.path(), None, &factory).migrate_flat_increments(&meeting, "my-increment").unwrap();
    for (name, text) in files {
        assert!(!inc.join(name).exists());
        assert_eq!(std::fs::read_to_string(inc.join("my-increment").join(name)).unwrap(), text);
    }
}

#[test]
fn migrated_layout_when_migrated_again_then_nothing_changes() {
    let dir = common::meetings_dir();
    let meeting = dir.path().join(MEETING);
    let target = meeting.join("increments/my-increment");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("v1.md"), "# Already migrated").unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    let generator = generator(dir.path(), None, &factory);
    generator.migrate_flat_increments(&meeting, "my-increment").unwrap();
    generator.migrate_flat_increments(&meeting, "my-increment").unwrap();
    assert_eq!(std::fs::read_to_string(target.join("v1.md")).unwrap(), "# Already migrated");
}

#[test]
fn no_increments_directory_when_migrated_then_nothing_created() {
    let dir = common::meetings_dir();
    let meeting = dir.path().join(MEETING);
    std::fs::create_dir_all(&meeting).unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    generator(dir.path(), None, &factory).migrate_flat_increments(&meeting, "my-increment").unwrap();
    assert!(!meeting.join("increments").exists());
}

#[tokio::test]
async fn flat_increments_when_generating_all_then_migrated_first_and_covered_segments_skipped() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let inc = meeting.join("increments");
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::write(inc.join("v1.md"), "# Old").unwrap();
    std::fs::write(inc.join("v1_meta.json"), r#"{"version": 1, "status": "completed", "segment_range": [1, 3]}"#)
        .unwrap();
    let prompts = write_prompts(
        dir.path(),
        &[("my-increment.md", "---\nname: My increment\nfilter: null\n---\n\nUser: {user_name}\n{transcript}")],
    );
    let factory = FakeFactory::new("codex", "gpt-5.4", Some("xhigh"), "New content");
    generator(dir.path(), Some(prompts), &factory).generate_all(MEETING, &meeting).await.unwrap();
    assert!(!inc.join("v1.md").exists() && !inc.join("v1_meta.json").exists());
    assert_eq!(std::fs::read_to_string(inc.join("my-increment/v1.md")).unwrap(), "# Old");
    assert!(factory.prompts().is_empty());
}

#[tokio::test]
async fn saved_prompt_override_when_generated_then_config_and_revision_recorded_without_rewriting_history() {
    let dir = tempfile::tempdir().unwrap();
    common::pre_change_settings(dir.path());
    let meetings = dir.path().join("meetings");
    let meeting = make_meeting(&meetings, "configured", None);
    let prompts = write_prompts(dir.path(), &[("my-increment.md", "---\nname: Registry\n---\nRegistry {transcript}")]);
    let store = AiSettingsStore::new(dir.path(), Some(prompts.clone())).with_legacy_env(LegacyEnv::default());
    let values = Json::parse(
        r#"{"prompt": "Custom {user_name}: {transcript}", "providers": ["claude"], "models": {"claude": "custom-claude"}}"#,
    )
    .unwrap();
    let saved = store.write_task("saved:my-increment", &values, None).unwrap();
    let factory = FakeFactory::new("claude", "custom-claude", None, "First result");
    let generator = generator(&meetings, Some(prompts), &factory);
    assert_eq!(generator.generate("configured", &meeting, &default_prompt(), false, None).await.unwrap(), Some(1));
    let calls = factory.calls();
    let settings = &calls[0].0;
    assert_eq!(settings.providers, ["claude"]);
    assert_eq!(settings.models["claude"], "custom-claude");
    assert!(factory.prompts()[0].starts_with("Custom Вася:"));
    let versions = meeting.join("increments/my-increment");
    let first_meta = std::fs::read(versions.join("v1_meta.json")).unwrap();
    let first_text = std::fs::read(versions.join("v1.md")).unwrap();
    let parsed = Json::parse(std::str::from_utf8(&first_meta).unwrap()).unwrap();
    assert_eq!(parsed["settings_revision"], Json::from(saved.revision.as_str()));
    assert_eq!(parsed["provider"], Json::from("claude"));
    assert_eq!(parsed["model"], Json::from("custom-claude"));
    assert_eq!(parsed["reasoning_effort"], Json::Null);
    store.reset_task("saved:my-increment", None).unwrap();
    factory.set_answer("Second result");
    assert_eq!(generator.generate("configured", &meeting, &default_prompt(), true, None).await.unwrap(), Some(2));
    assert_eq!(std::fs::read(versions.join("v1_meta.json")).unwrap(), first_meta);
    assert_eq!(std::fs::read(versions.join("v1.md")).unwrap(), first_text);
    assert!(factory.prompts()[1].starts_with("Участник: Вася"));
}

#[tokio::test]
async fn same_registry_id_when_caller_template_supplied_then_caller_text_and_transcript_braces_preserved() {
    let dir = tempfile::tempdir().unwrap();
    common::pre_change_settings(dir.path());
    let meetings = dir.path().join("meetings");
    let literal = r#"spoken {participants} {meeting_title} {user_name} {"outer":{"key":1}}"#;
    let segment = recog_ai::json::object([
        ("id", Json::Int(1)),
        ("ts_display", Json::from("12:00:00")),
        ("speaker", Json::from("User")),
        ("text", Json::from(literal)),
    ]);
    let meeting = make_meeting(&meetings, "single-pass", Some(vec![segment]));
    let prompts = write_prompts(dir.path(), &[("my-increment.md", "---\nname: Registry\n---\nRegistry {transcript}")]);
    let prompt = PromptConfig {
        id: "my-increment".into(),
        name: "Caller".into(),
        filter: None,
        template: "Caller: {transcript} / {participants}".into(),
    };
    let factory = FakeFactory::new("codex", "model", Some("high"), "Result");
    let version =
        generator(&meetings, Some(prompts), &factory).generate("single-pass", &meeting, &prompt, false, None).await;
    assert_eq!(version.unwrap(), Some(1));
    let rendered = &factory.prompts()[0];
    assert!(rendered.starts_with("Caller: "));
    assert!(rendered.contains(literal));
    assert!(rendered.ends_with(" / Вася, Петя"));
}

#[tokio::test]
async fn generation_dropped_mid_call_when_cancelled_then_failed_meta_replaces_placeholder() {
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
            _settings: &recog_ai::settings::ResolvedTask,
            _task: &str,
            _provider: Option<&str>,
        ) -> recog_ai::Result<Box<dyn recog_ai::provider::AiProvider>> {
            Ok(Box::new(Hanging))
        }
    }
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let generator = IncrementGenerator::new(dir.path(), Some(dir.path().join("none")))
        .with_factory(Arc::new(HangingFactory))
        .with_legacy_env(LegacyEnv::default());
    let prompt = default_prompt();
    let run = generator.generate(MEETING, &meeting, &prompt, false, None);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(200), run).await.is_err());
    let meta = read_json(&meeting.join("increments/my-increment/v1_meta.json"));
    assert_eq!(meta["status"], Json::from("failed"));
    assert_eq!(meta["error"], Json::from("cancelled: "));
    assert_eq!(meta["provider"], Json::from("codex"));
}

// --- What tests/parity.rs compared with increment.py, as Python's answers ---

fn inc_dir(meeting: &Path) -> PathBuf {
    meeting.join("increments/my-increment")
}

fn write_meta(meeting: &Path, version: i64, content: &[u8]) {
    std::fs::create_dir_all(inc_dir(meeting)).unwrap();
    std::fs::write(inc_dir(meeting).join(format!("v{version}_meta.json")), content).unwrap();
}

async fn generate(base: &Path, meeting: &Path, factory: &Arc<FakeFactory>) -> recog_ai::Result<Option<i64>> {
    generator(base, None, factory).generate(MEETING, meeting, &default_prompt(), false, None).await
}

#[tokio::test]
async fn completed_stale_and_broken_versions_when_generated_then_v4_covers_after_completed() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, Some(numbered(9)));
    let completed = r#"{"version": 1, "participant_id": null, "status": "completed", "generated_at": "2026-03-21T14:45:00+00:00", "segment_range": [1, 5]}"#;
    let stale = r#"{"version": 2, "participant_id": null, "status": "generating", "generated_at": "2026-03-21T14:45:00+00:00", "segment_range": [6, 6]}"#;
    write_meta(&meeting, 1, completed.as_bytes());
    write_meta(&meeting, 2, stale.as_bytes());
    write_meta(&meeting, 3, b"{broken");
    let factory = FakeFactory::new("codex", "m", None, "x");
    assert_eq!(generate(dir.path(), &meeting, &factory).await.unwrap(), Some(4));
    let v2 = read_json(&inc_dir(&meeting).join("v2_meta.json"));
    assert_eq!(v2["status"], Json::from("failed"));
    assert_eq!(v2["error"], Json::from("stale generating placeholder (process crash recovery)"));
    assert_eq!(read_json(&inc_dir(&meeting).join("v4_meta.json"))["segment_range"], Json::parse("[6, 9]").unwrap());
    let prompt = &factory.prompts()[0];
    assert!(prompt.contains("Seg 6") && prompt.contains("Seg 9") && !prompt.contains("Seg 5"), "{prompt}");
}

#[tokio::test]
async fn unreadable_time_or_range_when_generated_then_placeholder_kept_and_bad_range_ignored() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let generating = r#"{"version": 1, "status": "generating", "generated_at": "garbage", "segment_range": [1, 3]}"#;
    write_meta(&meeting, 1, generating.as_bytes());
    let factory = FakeFactory::new("codex", "m", None, "x");
    assert_eq!(generate(dir.path(), &meeting, &factory).await.unwrap(), None);
    assert_eq!(read_json(&inc_dir(&meeting).join("v1_meta.json"))["status"], Json::from("generating"));

    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    write_meta(&meeting, 1, br#"{"version": 1, "status": "completed", "segment_range": [3]}"#);
    write_meta(&meeting, 2, br#"{"version": 2, "status": "completed", "segment_range": [1, "3"]}"#);
    write_meta(&meeting, 4, br#"{"status": "completed", "segment_range": [1, 1]}"#);
    assert_eq!(generate(dir.path(), &meeting, &factory).await.unwrap(), Some(3));
    assert_eq!(read_json(&inc_dir(&meeting).join("v3_meta.json"))["segment_range"], Json::parse("[2, 3]").unwrap());
}

#[tokio::test]
async fn version_file_claiming_a_lower_number_when_generated_then_runtime_error_after_retries() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    write_meta(&meeting, 1, br#"{"version": 0, "status": "failed"}"#);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let error = generate(dir.path(), &meeting, &factory).await.unwrap_err();
    assert!(
        matches!(&error, AiError::Runtime(message) if message == "Could not claim version for my-increment after retries")
    );
    assert!(factory.calls().is_empty());
}

#[tokio::test]
async fn malformed_version_metadata_when_generated_then_python_errors_and_no_claim() {
    let cases: [(&[u8], &str); 3] = [
        (b"[]", "'list' object has no attribute 'get'"),
        (br#"{"version": "1"}"#, "version is a str"),
        (b"\xff", "invalid utf-8 sequence of 1 bytes from index 0"),
    ];
    for (content, message) in cases {
        let dir = common::meetings_dir();
        let meeting = make_meeting(dir.path(), MEETING, None);
        write_meta(&meeting, 1, content);
        let factory = FakeFactory::new("codex", "m", None, "x");
        let error = generate(dir.path(), &meeting, &factory).await.unwrap_err();
        assert_eq!(error.to_string(), message);
        assert!(!inc_dir(&meeting).join("v2_meta.json").exists());
    }
}

#[tokio::test]
async fn segment_ids_given_transcript_when_claimed_then_missing_id_is_zero_and_text_id_is_an_error() {
    let dir = common::meetings_dir();
    let segments = vec![
        Json::parse(r#"{"speaker": "Вася", "text": "Без номера"}"#).unwrap(),
        Json::parse(r#"{"id": 4, "speaker": "Вася", "text": "Четыре"}"#).unwrap(),
    ];
    let meeting = make_meeting(dir.path(), MEETING, Some(segments));
    let factory = FakeFactory::new("codex", "m", None, "x");
    assert_eq!(generate(dir.path(), &meeting, &factory).await.unwrap(), Some(1));
    assert_eq!(read_json(&inc_dir(&meeting).join("v1_meta.json"))["segment_range"], Json::parse("[0, 4]").unwrap());

    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, Some(vec![Json::parse(r#"{"id": "x", "text": "t"}"#).unwrap()]));
    let error = generate(dir.path(), &meeting, &factory).await.unwrap_err();
    assert_eq!(error.to_string(), "segment id is a str");
}

#[tokio::test]
async fn covered_meeting_when_forced_then_next_version_has_the_whole_transcript() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let generator = generator(dir.path(), None, &factory);
    assert_eq!(generator.generate(MEETING, &meeting, &default_prompt(), false, None).await.unwrap(), Some(1));
    assert_eq!(generator.generate(MEETING, &meeting, &default_prompt(), false, None).await.unwrap(), None);
    assert_eq!(generator.generate(MEETING, &meeting, &default_prompt(), true, None).await.unwrap(), Some(2));
    assert_eq!(read_json(&inc_dir(&meeting).join("v2_meta.json"))["segment_range"], Json::parse("[1, 3]").unwrap());
    assert_eq!(factory.prompts().len(), 2);
}

fn write_meeting_meta(meeting: &Path, meta: &[u8]) {
    std::fs::write(meeting.join("meta.json"), meta).unwrap();
}

#[tokio::test]
async fn unreadable_meeting_meta_when_generated_then_error_before_any_claim() {
    let cases: [(&[u8], Option<&str>, &str); 7] = [
        (b"{broken", None, "Invalid JSON in meta.json"),
        (b"\xff", None, "invalid utf-8 sequence of 1 bytes from index 0"),
        (b"[]", Some("speaker_2"), "'list' object has no attribute 'get'"),
        (br#"{"participants": []}"#, Some("speaker_2"), "'list' object has no attribute 'get'"),
        (br#"{"participants": {"speaker_2": 5}}"#, Some("speaker_2"), "'int' object has no attribute 'get'"),
        (
            br#"{"participants": {"speaker_2": {"name": 5}}}"#,
            Some("speaker_2"),
            "'int' object has no attribute 'replace'",
        ),
        (
            br#"{"participants": {"speaker_1": {"name": 5, "is_local": true}}}"#,
            None,
            "expected str instance, int found",
        ),
    ];
    for (meta, participant, message) in cases {
        let dir = common::meetings_dir();
        let meeting = make_meeting(dir.path(), MEETING, None);
        write_meeting_meta(&meeting, meta);
        let factory = FakeFactory::new("codex", "m", None, "x");
        let error = generator(dir.path(), None, &factory)
            .generate(MEETING, &meeting, &default_prompt(), false, participant)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), message, "{}", String::from_utf8_lossy(meta));
        assert!(!meeting.join("increments").exists());
    }
}

#[tokio::test]
async fn participant_without_name_when_generated_then_recorded_by_id() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    write_meeting_meta(&meeting, br#"{"participants": {"speaker_2": {"name": ""}}}"#);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let version = generator(dir.path(), None, &factory)
        .generate(MEETING, &meeting, &default_prompt(), false, Some("speaker_2"))
        .await;
    assert_eq!(version.unwrap(), Some(1));
    assert_eq!(read_json(&inc_dir(&meeting).join("v1_meta.json"))["participant_name"], Json::from("speaker_2"));
    assert!(factory.prompts()[0].starts_with("Участник: speaker_2\n"));
}

#[tokio::test]
async fn malformed_meta_fields_after_the_claim_when_generated_then_failed_version_with_the_error() {
    let cases: [(&[u8], &str, usize); 3] = [
        (
            br#"{"participants": {"speaker_1": {"name": "Vasya", "is_local": true}, "speaker_2": {"name": 5}}}"#,
            "expected str instance, int found",
            0,
        ),
        (br#"{"title": 5, "participants": {}}"#, "'int' object has no attribute 'replace'", 0),
        (br#"{"created_at": 5, "participants": {}}"#, "'int' object is not subscriptable", 1),
    ];
    for (meta, message, calls) in cases {
        let dir = common::meetings_dir();
        let meeting = make_meeting(dir.path(), MEETING, None);
        write_meeting_meta(&meeting, meta);
        let factory = FakeFactory::new("codex", "m", None, "x");
        assert_eq!(generate(dir.path(), &meeting, &factory).await.unwrap(), None);
        let failed = read_json(&inc_dir(&meeting).join("v1_meta.json"));
        assert_eq!(failed["status"], Json::from("failed"), "{message}");
        assert_eq!(failed["error"], Json::from(message));
        assert!(!inc_dir(&meeting).join("v1.md").exists());
        assert_eq!(factory.prompts().len(), calls, "{message}");
    }
}

#[test]
fn several_prompt_directories_when_listing_all_then_sorted_nonempty_ones_after_migration() {
    let dir = common::meetings_dir();
    let meeting = dir.path().join(MEETING);
    let increments = meeting.join("increments");
    for (prompt, version) in [("team", 2), ("team", 1), ("alpha", 1)] {
        std::fs::create_dir_all(increments.join(prompt)).unwrap();
        let meta = format!(r#"{{"version": {version}, "status": "completed"}}"#);
        std::fs::write(increments.join(prompt).join(format!("v{version}_meta.json")), meta).unwrap();
    }
    std::fs::create_dir_all(increments.join("empty")).unwrap();
    std::fs::write(increments.join("v1_meta.json"), r#"{"version": 1, "status": "completed"}"#).unwrap();
    std::fs::write(increments.join("notes.txt"), "not a prompt").unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    let all = generator(dir.path(), None, &factory).list_all_increments(MEETING).unwrap();
    let listed: Vec<(String, Vec<Json>)> = all
        .into_iter()
        .map(|(prompt, metas)| (prompt, metas.iter().map(|meta| meta["version"].clone()).collect()))
        .collect();
    assert_eq!(
        listed,
        [
            ("alpha".to_owned(), vec![Json::Int(1)]),
            ("my-increment".to_owned(), vec![Json::Int(1)]),
            ("team".to_owned(), vec![Json::Int(1), Json::Int(2)]),
        ]
    );
    assert!(increments.join("my-increment/v1_meta.json").exists());
}

#[test]
fn meeting_without_increments_when_listing_all_then_empty() {
    let dir = common::meetings_dir();
    std::fs::create_dir_all(dir.path().join(MEETING)).unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    assert!(generator(dir.path(), None, &factory).list_all_increments(MEETING).unwrap().is_empty());
}

#[test]
fn malformed_version_metadata_when_listed_then_python_errors_and_unversioned_first() {
    let cases: [(&[u8], &str); 3] = [
        (b"\xff", "invalid utf-8 sequence of 1 bytes from index 0"),
        (b"[]", "'list' object has no attribute 'get'"),
        (br#"{"version": 1.5}"#, "version is a float"),
    ];
    for (content, message) in cases {
        let dir = common::meetings_dir();
        let meeting = dir.path().join(MEETING);
        write_meta(&meeting, 1, content);
        let factory = FakeFactory::new("codex", "m", None, "x");
        let error = generator(dir.path(), None, &factory).list_increments(MEETING, "my-increment").unwrap_err();
        assert_eq!(error.to_string(), message);
    }
    let dir = common::meetings_dir();
    let meeting = dir.path().join(MEETING);
    write_meta(&meeting, 2, br#"{"version": 2}"#);
    write_meta(&meeting, 1, br#"{"status": "completed"}"#);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let metas = generator(dir.path(), None, &factory).list_increments(MEETING, "my-increment").unwrap();
    assert_eq!(metas, [Json::parse(r#"{"status": "completed"}"#).unwrap(), Json::parse(r#"{"version": 2}"#).unwrap()]);
}

#[test]
fn unreadable_version_when_fetched_then_none_or_decode_error_and_no_markdown_is_none() {
    let dir = common::meetings_dir();
    let meeting = dir.path().join(MEETING);
    write_meta(&meeting, 1, b"{broken");
    write_meta(&meeting, 2, b"\xff");
    write_meta(&meeting, 3, br#"{"version": 3}"#);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let generator = generator(dir.path(), None, &factory);
    assert!(generator.get_increment(MEETING, "my-increment", 1).unwrap().is_none());
    assert_eq!(
        generator.get_increment(MEETING, "my-increment", 2).unwrap_err().to_string(),
        "invalid utf-8 sequence of 1 bytes from index 0"
    );
    assert_eq!(
        generator.get_increment(MEETING, "my-increment", 3).unwrap(),
        Some((Json::parse(r#"{"version": 3}"#).unwrap(), None))
    );
}

#[tokio::test]
async fn meeting_meta_missing_or_broken_when_generating_all_then_nothing_or_error() {
    let dir = common::meetings_dir();
    let prompts = write_prompts(dir.path(), &[("all.md", "---\nname: All\nfilter: null\n---\n\n{transcript}")]);
    let meeting = make_meeting(dir.path(), MEETING, None);
    let factory = FakeFactory::new("codex", "m", None, "x");
    let generator = generator(dir.path(), Some(prompts), &factory);
    std::fs::remove_file(meeting.join("meta.json")).unwrap();
    generator.generate_all(MEETING, &meeting).await.unwrap();
    assert!(!meeting.join("increments").exists());
    let cases: [(&[u8], &str); 3] = [
        (b"{broken", "Invalid JSON in meta.json"),
        (b"\xff", "invalid utf-8 sequence of 1 bytes from index 0"),
        (b"[]", "'list' object has no attribute 'get'"),
    ];
    for (meta, message) in cases {
        write_meeting_meta(&meeting, meta);
        assert_eq!(generator.generate_all(MEETING, &meeting).await.unwrap_err().to_string(), message);
    }
    assert!(factory.calls().is_empty());
}

const AGED: &str = "2000-01-01T00:00:00+00:00";

/// A provider that ages the `generating` placeholder, then waits for the heartbeat to
/// refresh it, and records the metadata it saw refreshed.
struct HeartbeatWatch {
    meta: PathBuf,
    seen: Arc<Mutex<Option<Json>>>,
}

impl recog_ai::provider::AiProvider for HeartbeatWatch {
    fn key(&self) -> &str {
        "codex"
    }
    fn name(&self) -> String {
        "Watch".into()
    }
    fn model(&self) -> &str {
        "m"
    }
    fn reasoning_effort(&self) -> Option<&str> {
        None
    }
    fn complete<'a>(&'a self, _prompt: &'a str) -> recog_ai::provider::BoxFuture<'a, recog_ai::Result<String>> {
        Box::pin(async move {
            let mut meta = read_json(&self.meta);
            meta.as_object_mut().unwrap().insert("generated_at".into(), Json::from(AGED));
            std::fs::write(&self.meta, meta.dumps(None)).unwrap();
            for _ in 0..500 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let current = std::fs::read_to_string(&self.meta).ok().and_then(|text| Json::parse(&text).ok());
                if let Some(current) = current.filter(|m| m["generated_at"] != Json::from(AGED)) {
                    *self.seen.lock().unwrap() = Some(current);
                    break;
                }
            }
            Ok("## Итог\n- ok\n".to_owned())
        })
    }
}

struct HeartbeatFactory {
    meta: PathBuf,
    seen: Arc<Mutex<Option<Json>>>,
}

impl recog_ai::provider::ProviderFactory for HeartbeatFactory {
    fn build(
        &self,
        _settings: &recog_ai::settings::ResolvedTask,
        _task: &str,
        _provider: Option<&str>,
    ) -> recog_ai::Result<Box<dyn recog_ai::provider::AiProvider>> {
        Ok(Box::new(HeartbeatWatch { meta: self.meta.clone(), seen: self.seen.clone() }))
    }
}

#[tokio::test]
async fn long_generation_when_heartbeat_fires_then_generating_placeholder_gets_a_fresh_generated_at() {
    let dir = common::meetings_dir();
    let meeting = make_meeting(dir.path(), MEETING, None);
    let seen = Arc::new(Mutex::new(None));
    let factory = Arc::new(HeartbeatFactory { meta: inc_dir(&meeting).join("v1_meta.json"), seen: seen.clone() });
    let generator = IncrementGenerator::new(dir.path(), Some(dir.path().join("no-prompts")))
        .with_factory(factory)
        .with_legacy_env(LegacyEnv::default())
        .with_heartbeat_interval(std::time::Duration::from_millis(50));
    let version = generator.generate(MEETING, &meeting, &default_prompt(), false, None).await;
    assert_eq!(version.unwrap(), Some(1));
    let refreshed = seen.lock().unwrap().clone().expect("the heartbeat refreshed the placeholder");
    assert_eq!(refreshed["status"], Json::from("generating"));
    let stamp = refreshed["generated_at"].as_str().unwrap().to_owned();
    assert!(stamp.as_str() > "2026", "{stamp}");
    assert_eq!(read_json(&inc_dir(&meeting).join("v1_meta.json"))["status"], Json::from("completed"));
}

#[tokio::test]
async fn failing_prompt_when_generating_all_then_reported_and_the_others_still_run() {
    let dir = common::meetings_dir();
    let prompts = write_prompts(
        dir.path(),
        &[
            ("all.md", "---\nname: All\nfilter: null\n---\n\n{transcript}"),
            ("other.md", "---\nname: Other\nfilter: null\n---\n\n{transcript}"),
        ],
    );
    let meeting = make_meeting(dir.path(), MEETING, None);
    std::fs::create_dir_all(meeting.join("increments/other")).unwrap();
    std::fs::write(meeting.join("increments/other/v1_meta.json"), "[]").unwrap();
    let factory = FakeFactory::new("codex", "m", None, "x");
    generator(dir.path(), Some(prompts), &factory).generate_all(MEETING, &meeting).await.unwrap();
    assert!(meeting.join("increments/all/v1.md").exists());
    assert!(!meeting.join("increments/other/v2_meta.json").exists());
    assert_eq!(factory.prompts().len(), 1);
}

// --- The AI functions level ---

fn settings_with_access(data: &Path, access: &str) {
    std::fs::create_dir_all(data).unwrap();
    let text = format!(
        r#"{{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {{}}, "tasks": {{}}, "access": "{access}"}}"#
    );
    std::fs::write(data.join("ai-settings.json"), text).unwrap();
}

#[tokio::test]
async fn level_manual_when_meeting_ends_then_no_summary_version_and_no_provider_call() {
    let dir = common::meetings_dir();
    settings_with_access(dir.path(), "manual");
    let meetings = dir.path().join("meetings");
    let meeting = make_meeting(&meetings, MEETING, None);
    let prompts = write_prompts(dir.path(), &[("my-increment.md", "---\nname: Mine\nfilter: null\n---\n{transcript}")]);
    let factory = FakeFactory::new("claude", "model", None, "Summary");
    generator(&meetings, Some(prompts), &factory).generate_all(MEETING, &meeting).await.unwrap();
    assert!(!meeting.join("increments").exists());
    assert!(factory.calls().is_empty());
}

#[tokio::test]
async fn unreadable_settings_when_meeting_ends_then_no_summary() {
    let dir = common::meetings_dir();
    std::fs::write(dir.path().join("ai-settings.json"), "{").unwrap();
    let meetings = dir.path().join("meetings");
    let meeting = make_meeting(&meetings, MEETING, None);
    let prompts = write_prompts(dir.path(), &[("my-increment.md", "---\nname: Mine\nfilter: null\n---\n{transcript}")]);
    let factory = FakeFactory::new("claude", "model", None, "Summary");
    generator(&meetings, Some(prompts), &factory).generate_all(MEETING, &meeting).await.unwrap();
    assert!(!meeting.join("increments").exists());
    assert!(factory.calls().is_empty());
}

#[tokio::test]
async fn level_off_when_regenerating_then_disabled_and_no_version_claimed() {
    let dir = common::meetings_dir();
    settings_with_access(dir.path(), "off");
    let meetings = dir.path().join("meetings");
    let meeting = make_meeting(&meetings, MEETING, None);
    let factory = FakeFactory::new("claude", "model", None, "Summary");
    let error = generator(&meetings, None, &factory)
        .generate(MEETING, &meeting, &default_prompt(), true, None)
        .await
        .unwrap_err();
    assert!(matches!(error, AiError::Disabled(_)), "{error:?}");
    assert_eq!(error.to_string(), recog_ai::error::DISABLED);
    assert!(!meeting.join("increments").exists());
    assert!(factory.calls().is_empty());
}
