//! tests/test_ai_provider.py and the provider half of tests/test_claude_effort.py, with
//! stub executables in temporary directories instead of patched subprocesses.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{shell_stub, stub_argv, stub_env, stub_stdin};
use recog_ai::provider::{
    AiProvider, CCS_AUTOMATION_MARKER_PREFIX, ClaudeCliProvider, CliEnv, CodexCliProvider, build_codex_exec_command,
    mark_ccs_automation, node_env, provider_for_settings, resolve_cli,
};
use recog_ai::settings::{AiSettingsStore, Fallback};

const AGENT_ANSWER: &str = r#"{"type":"item.completed","item":{"type":"agent_message","text":"answer"}}"#;
const TURN_FAILED: &str = r#"{"type":"turn.failed","error":{"message":"stream disconnected"}}"#;
const RECONNECT: &str = r#"{"type":"error","message":"Reconnecting... 2/5 (request timed out)"}"#;

fn agent(text: &str) -> String {
    format!(r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"{text}"}}}}"#)
}

fn executable(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn strings(argv: &[std::ffi::OsString]) -> Vec<String> {
    argv.iter().map(|arg| arg.to_string_lossy().into_owned()).collect()
}

struct Stubbed {
    _dir: tempfile::TempDir,
    bin: std::path::PathBuf,
    env: CliEnv,
}

fn stubbed(name: &str, stdout: &str, stderr: &str, code: i32) -> Stubbed {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    shell_stub(&bin, name, stdout, stderr, code);
    let env = stub_env(&bin, &dir.path().join("home"));
    Stubbed { _dir: dir, bin, env }
}

fn codex(stub: &Stubbed) -> CodexCliProvider {
    CodexCliProvider::new("analysis", None, None, Some(stub.env.clone())).unwrap()
}

async fn codex_complete(lines: &[&str], code: i32, stderr: &str) -> recog_ai::Result<String> {
    let stub = stubbed("codex", &(lines.join("\n") + "\n"), stderr, code);
    codex(&stub).complete("hello").await
}

async fn codex_stream(lines: &[&str], code: i32, stderr: &str) -> recog_ai::Result<Vec<(String, bool)>> {
    let stub = stubbed("codex", &(lines.join("\n") + "\n"), stderr, code);
    let mut chunks = Vec::new();
    codex(&stub).stream("hello", &mut |text, done| chunks.push((text, done))).await?;
    Ok(chunks)
}

// --- resolve_cli ---

#[test]
fn binary_on_path_when_resolved_then_path_entry_is_returned() {
    let stub = stubbed("codex", "", "", 0);
    assert_eq!(resolve_cli("codex", &stub.env).unwrap(), stub.bin.join("codex"));
}

#[test]
fn binary_only_in_local_bin_when_resolved_then_home_fallback_is_returned() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cli = home.join(".local/bin/recog-ai-test-cli");
    executable(&cli);
    let env = CliEnv::from_pairs([
        ("PATH", dir.path().join("empty").display().to_string()),
        ("HOME", home.display().to_string()),
    ]);
    assert_eq!(resolve_cli("recog-ai-test-cli", &env).unwrap(), cli);
}

#[test]
fn binary_nowhere_when_resolved_then_file_not_found_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let env =
        CliEnv::from_pairs([("PATH", dir.path().display().to_string()), ("HOME", dir.path().display().to_string())]);
    let error = resolve_cli("nonexistent_binary_xyz_12345", &env).unwrap_err();
    assert_eq!(error.to_string(), "nonexistent_binary_xyz_12345 not found in PATH, ~/.local/bin, or nvm");
}

#[test]
fn several_nvm_versions_when_resolved_then_highest_version_is_picked() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    for version in ["v18.0.0", "v20.0.0", "v9.9.9"] {
        executable(&home.join(".nvm/versions/node").join(version).join("bin/recog-ai-test-cli"));
    }
    let env = CliEnv::from_pairs([
        ("PATH", dir.path().join("empty").display().to_string()),
        ("HOME", home.display().to_string()),
    ]);
    let resolved = resolve_cli("recog-ai-test-cli", &env).unwrap();
    assert!(resolved.to_string_lossy().contains("v20.0.0"), "{}", resolved.display());
}

// --- Codex ---

#[test]
fn codex_provider_when_created_then_key_is_codex() {
    let stub = stubbed("codex", "", "", 0);
    assert_eq!(codex(&stub).key(), "codex");
}

#[test]
fn no_model_override_when_creating_codex_provider_then_astra_is_used() {
    let stub = stubbed("codex", "", "", 0);
    assert_eq!(codex(&stub).model, "gpt-6-astra");
}

#[test]
fn legacy_model_and_effort_when_creating_codex_provider_then_name_includes_both() {
    let mut stub = stubbed("codex", "", "", 0);
    stub.env.set("CODEX_MODEL", "gpt-5.4");
    stub.env.set("CODEX_REASONING_EFFORT", "xhigh");
    let name = codex(&stub).name();
    assert!(name.contains("gpt-5.4") && name.contains("xhigh"), "{name}");
}

#[test]
fn spark_model_when_named_then_no_reasoning_effort_is_shown() {
    let mut stub = stubbed("codex", "", "", 0);
    stub.env.set("CODEX_MODEL", "o4-mini-spark");
    let name = codex(&stub).name();
    assert!(name.contains("spark") && !name.contains("xhigh"), "{name}");
}

#[test]
fn codex_provider_when_created_then_cli_is_resolved_once_at_init() {
    let stub = stubbed("codex", "", "", 0);
    assert_eq!(codex(&stub).cli, stub.bin.join("codex"));
}

#[test]
fn prompt_when_marked_twice_then_marker_is_prepended_once() {
    let marked = mark_ccs_automation("hello", "Increment Prompt");
    assert!(marked.starts_with("<!-- ccs-automation:recog task=increment-prompt -->\n"));
    assert_eq!(mark_ccs_automation(&marked, "title"), marked);
}

#[test]
fn effort_when_building_codex_command_then_ephemeral_exact_argv() {
    let cmd = build_codex_exec_command("/usr/bin/codex".as_ref(), "gpt-5.4", Some("xhigh"), true, false);
    assert_eq!(
        strings(&cmd),
        [
            "/usr/bin/codex",
            "exec",
            "-m",
            "gpt-5.4",
            "-c",
            "model_reasoning_effort=\"xhigh\"",
            "--ephemeral",
            "--json",
            "-"
        ]
    );
}

#[test]
fn isolated_analysis_when_building_command_then_custom_context_is_disabled() {
    let cmd = strings(&build_codex_exec_command("/mock/codex".as_ref(), "test-model", Some("xhigh"), true, true));
    assert!(cmd.contains(&"--ignore-user-config".to_owned()));
    let mut overrides: Vec<&str> =
        cmd.iter().enumerate().filter(|(_, arg)| *arg == "-c").map(|(index, _)| cmd[index + 1].as_str()).collect();
    overrides.sort_unstable();
    let mut expected = vec![
        "model_reasoning_effort=\"xhigh\"",
        "skills.include_instructions=false",
        "skills.bundled.enabled=false",
        "features.plugins=false",
        "features.hooks=false",
        "features.memories=false",
        "features.skill_search=false",
        "project_doc_max_bytes=0",
    ];
    expected.sort_unstable();
    assert_eq!(overrides, expected);
    assert!(cmd.contains(&"--ephemeral".to_owned()));
    assert_eq!(&cmd[cmd.len() - 2..], ["--json", "-"]);
}

#[test]
fn codex_auto_given_command_when_built_then_no_reasoning_override() {
    let cmd = strings(&build_codex_exec_command("/mock/codex".as_ref(), "gpt-6-astra", Some(""), true, true));
    assert!(!cmd.iter().any(|arg| arg.contains("model_reasoning_effort=")));
    assert!(cmd.contains(&"--ignore-user-config".to_owned()));
}

#[tokio::test]
async fn increment_task_when_completing_then_prompt_is_marked_and_codex_is_isolated() {
    let stub = stubbed("codex", &(AGENT_ANSWER.to_owned() + "\n"), "", 0);
    let provider = CodexCliProvider::new("increment", None, None, Some(stub.env.clone())).unwrap();
    assert_eq!(provider.complete("hello").await.unwrap(), "answer");
    let argv = stub_argv(&stub.bin, "codex");
    for flag in ["--ephemeral", "--ignore-user-config", "skills.include_instructions=false"] {
        assert!(argv.contains(&flag.to_owned()), "{flag}: {argv:?}");
    }
    assert_eq!(&argv[argv.len() - 2..], ["--json", "-"]);
    assert!(
        stub_stdin(&stub.bin, "codex")
            .starts_with(&format!("{CCS_AUTOMATION_MARKER_PREFIX} task=increment -->\nhello"))
    );
}

#[tokio::test]
async fn reconnect_notice_then_answer_when_completing_then_answer_is_returned() {
    let answer = codex_complete(&[RECONNECT, AGENT_ANSWER, r#"{"type":"turn.completed"}"#], 0, "").await.unwrap();
    assert_eq!(answer, "answer");
}

#[tokio::test]
async fn turn_failed_when_completing_then_its_error_is_raised() {
    let notice = r#"{"type":"error","message":"Reconnecting... 5/5 (request timed out)"}"#;
    let error = codex_complete(&[notice, TURN_FAILED], 0, "").await.unwrap_err();
    assert_eq!(error.to_string(), "stream disconnected");
}

#[tokio::test]
async fn codex_exits_nonzero_when_completing_then_stderr_is_raised() {
    let error = codex_complete(&[], 1, "auth expired\n").await.unwrap_err();
    assert_eq!(error.to_string(), "Codex CLI error: auth expired");
}

#[tokio::test]
async fn codex_exits_nonzero_without_stderr_when_completing_then_unknown_error() {
    let error = codex_complete(&[], 1, "").await.unwrap_err();
    assert_eq!(error.to_string(), "Codex CLI error: Unknown error");
}

#[tokio::test]
async fn no_agent_message_when_completing_then_no_response_is_raised() {
    let reasoning = r#"{"type":"item.completed","item":{"type":"reasoning","text":"x"}}"#;
    let error = codex_complete(&["not json", "", reasoning], 0, "").await.unwrap_err();
    assert_eq!(error.to_string(), "No response from Codex");
}

#[tokio::test]
async fn two_agent_messages_when_completing_then_they_are_joined() {
    let answer = codex_complete(&[&agent("one"), "garbage", &agent("two")], 0, "").await.unwrap();
    assert_eq!(answer, "one\ntwo");
}

#[tokio::test]
async fn stream_ends_without_turn_completed_then_last_answer_is_final() {
    let chunks = codex_stream(&["", "garbage", &agent(""), &agent("answer")], 0, "").await.unwrap();
    assert_eq!(chunks, [("answer".to_owned(), false), ("answer".to_owned(), true)]);
}

#[tokio::test]
async fn codex_exits_nonzero_when_streaming_then_stderr_is_raised() {
    let error = codex_stream(&[], 1, "auth expired\n").await.unwrap_err();
    assert_eq!(error.to_string(), "Codex streaming error: Codex CLI error: auth expired");
}

#[tokio::test]
async fn reconnect_notice_then_answer_when_streaming_then_answer_is_returned() {
    let lines = [RECONNECT, AGENT_ANSWER, r#"{"type":"turn.completed"}"#, &agent("after the turn")];
    let chunks = codex_stream(&lines, 0, "").await.unwrap();
    assert_eq!(chunks, [("answer".to_owned(), false), ("answer".to_owned(), true)]);
}

#[tokio::test]
async fn turn_failed_when_streaming_then_its_error_is_raised() {
    let error = codex_stream(&[TURN_FAILED], 0, "").await.unwrap_err();
    assert_eq!(error.to_string(), "Codex streaming error: stream disconnected");
}

#[tokio::test]
async fn streaming_analysis_when_spawning_codex_then_context_is_isolated() {
    let stub = stubbed("codex", &format!("{AGENT_ANSWER}\n{{\"type\":\"turn.completed\"}}\n"), "", 0);
    let mut chunks = Vec::new();
    codex(&stub).stream("hello", &mut |text, done| chunks.push((text, done))).await.unwrap();
    assert_eq!(chunks, [("answer".to_owned(), false), ("answer".to_owned(), true)]);
    let argv = stub_argv(&stub.bin, "codex");
    for flag in ["--ignore-user-config", "skills.include_instructions=false", "project_doc_max_bytes=0"] {
        assert!(argv.contains(&flag.to_owned()), "{flag}");
    }
    assert_eq!(&argv[argv.len() - 2..], ["--json", "-"]);
}

#[tokio::test]
async fn agent_message_without_text_when_completing_then_empty_part_is_joined() {
    let bare = r#"{"type":"item.completed","item":{"type":"agent_message"}}"#;
    assert_eq!(codex_complete(&[bare, &agent("two")], 0, "").await.unwrap(), "\ntwo");
}

#[tokio::test]
async fn non_text_agent_messages_when_completing_then_python_join_error_names_the_item() {
    let number = r#"{"type":"item.completed","item":{"type":"agent_message","text":5}}"#;
    let null = r#"{"type":"item.completed","item":{"type":"agent_message","text":null}}"#;
    let error = codex_complete(&[number], 0, "").await.unwrap_err();
    assert_eq!(error.to_string(), "sequence item 0: expected str instance, int found");
    let error = codex_complete(&[&agent("one"), null], 0, "").await.unwrap_err();
    assert_eq!(error.to_string(), "sequence item 1: expected str instance, NoneType found");
}

#[tokio::test]
async fn invalid_utf8_answer_when_completing_then_decode_error() {
    let stub = stubbed("codex", "", "", 0);
    std::fs::write(stub.bin.join("codex.data/stdout"), b"\xff\n").unwrap();
    let error = codex(&stub).complete("hello").await.unwrap_err();
    assert!(error.to_string().starts_with("'utf-8' codec can't decode"), "{error}");
}

#[tokio::test]
async fn non_text_agent_message_when_streaming_then_type_error_after_earlier_chunks() {
    let number = r#"{"type":"item.completed","item":{"type":"agent_message","text":5}}"#;
    let stub = stubbed("codex", &format!("{}\n{number}\n", agent("one")), "", 0);
    let mut chunks = Vec::new();
    let error = codex(&stub).stream("hello", &mut |text, done| chunks.push((text, done))).await.unwrap_err();
    assert_eq!(error.to_string(), "Codex streaming error: agent message text is a int");
    assert_eq!(chunks, [("one".to_owned(), false)]);
}

#[tokio::test]
async fn turn_completed_without_message_when_streaming_then_empty_final_chunk() {
    let chunks = codex_stream(&[r#"{"type":"turn.completed"}"#], 0, "").await.unwrap();
    assert_eq!(chunks, [(String::new(), true)]);
}

#[tokio::test]
async fn no_message_and_clean_exit_when_streaming_then_no_chunk() {
    assert!(codex_stream(&[RECONNECT], 0, "").await.unwrap().is_empty());
}

// --- Claude ---

fn claude(stub: &Stubbed, task: &str, effort: Option<&str>) -> ClaudeCliProvider {
    ClaudeCliProvider::new(task, None, effort, Some(stub.env.clone())).unwrap()
}

#[test]
fn no_model_override_when_creating_claude_provider_then_opus_5_5_is_used() {
    let stub = stubbed("claude", "", "", 0);
    assert_eq!(claude(&stub, "analysis", None).model, "claude-opus-5-5");
}

#[tokio::test]
async fn prompt_when_completing_with_claude_then_marked_on_stdin_with_safe_mode() {
    let stub = stubbed("claude", "answer\n", "", 0);
    assert_eq!(claude(&stub, "anchor", None).complete("hello").await.unwrap(), "answer");
    let argv = stub_argv(&stub.bin, "claude");
    assert_eq!(argv[0], "-p");
    assert!(stub_stdin(&stub.bin, "claude").starts_with("<!-- ccs-automation:recog task=anchor -->\nhello"));
    assert!(!argv.iter().any(|arg| arg.contains("hello")));
    assert!(argv.contains(&"--safe-mode".to_owned()) && argv.contains(&"--disable-slash-commands".to_owned()));
    assert!(!argv.contains(&"--bare".to_owned()));
}

#[tokio::test]
async fn claude_exits_nonzero_when_completing_then_stderr_is_raised() {
    let stub = stubbed("claude", "", "limit reached\n", 1);
    let error = claude(&stub, "analysis", None).complete("hello").await.unwrap_err();
    assert_eq!(error.to_string(), "Claude CLI error: limit reached");
}

#[tokio::test]
async fn streaming_analysis_when_spawning_claude_then_safe_mode_preserves_auth_and_disables_skills() {
    let stdout = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]}}\n{\"type\":\"result\",\"result\":\"answer\"}\n";
    let stub = stubbed("claude", stdout, "", 0);
    let mut chunks = Vec::new();
    claude(&stub, "analysis", None).stream("hello", &mut |text, done| chunks.push((text, done))).await.unwrap();
    assert_eq!(chunks, [("answer".to_owned(), false), ("answer".to_owned(), true)]);
    assert!(stub_stdin(&stub.bin, "claude").ends_with("hello"));
    let argv = stub_argv(&stub.bin, "claude");
    assert!(!argv.iter().any(|arg| arg.contains("hello")));
    assert!(argv.contains(&"--safe-mode".to_owned()) && argv.contains(&"--disable-slash-commands".to_owned()));
    assert!(!argv.contains(&"--bare".to_owned()));
    let format = argv.iter().position(|arg| arg == "--output-format").unwrap();
    assert_eq!(argv[format + 1], "stream-json");
    assert!(argv.contains(&"--verbose".to_owned()));
}

#[tokio::test]
async fn claude_stream_without_result_event_when_ended_then_last_text_is_final() {
    let stdout =
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"partial\"}]}}\nnot json\n";
    let stub = stubbed("claude", stdout, "", 0);
    let mut chunks = Vec::new();
    claude(&stub, "analysis", None).stream("hello", &mut |text, done| chunks.push((text, done))).await.unwrap();
    assert_eq!(chunks, [("partial".to_owned(), false), ("partial".to_owned(), true)]);
}

async fn claude_stream(lines: &[&str], code: i32, stderr: &str) -> (recog_ai::Result<()>, Vec<(String, bool)>) {
    let stub = stubbed("claude", &(lines.join("\n") + "\n"), stderr, code);
    let mut chunks = Vec::new();
    let result = claude(&stub, "analysis", None).stream("hello", &mut |text, done| chunks.push((text, done))).await;
    (result, chunks)
}

fn chunk(text: &str, done: bool) -> (String, bool) {
    (text.to_owned(), done)
}

#[tokio::test]
async fn mixed_content_blocks_when_streaming_claude_then_each_nonempty_text_and_last_is_final() {
    let assistant = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"one"},{"type":"tool_use"},{"type":"text","text":""},{"type":"text","text":"two"},{"type":"text","text":7}]}}"#;
    let lines = ["", "   ", r#"{"type":"system"}"#, r#"{"type":"assistant"}"#, assistant, r#"{"type":"result"}"#];
    let (result, chunks) = claude_stream(&lines, 0, "").await;
    result.unwrap();
    assert_eq!(chunks, [chunk("one", false), chunk("two", false), chunk("7", false), chunk("7", true)]);
}

#[tokio::test]
async fn non_text_results_when_streaming_claude_then_python_str_of_the_value() {
    let (result, chunks) = claude_stream(&[r#"{"type":"result","result":5}"#, "ignored"], 0, "").await;
    result.unwrap();
    assert_eq!(chunks, [chunk("5", true)]);
    let (result, chunks) = claude_stream(&[r#"{"type":"result","result":null}"#], 0, "").await;
    result.unwrap();
    assert_eq!(chunks, [chunk("", true)]);
}

#[tokio::test]
async fn malformed_events_when_streaming_claude_then_python_type_errors() {
    let text = r#"{"type":"text","text":"a"}"#;
    let cases = [
        ("[1]".to_owned(), "'list' object has no attribute 'get'"),
        (r#"{"type":"assistant","message":5}"#.to_owned(), "'int' object has no attribute 'get'"),
        (r#"{"type":"assistant","message":{"content":"ab"}}"#.to_owned(), "'str' object has no attribute 'get'"),
        (r#"{"type":"assistant","message":{"content":{"k":1}}}"#.to_owned(), "'str' object has no attribute 'get'"),
        (r#"{"type":"assistant","message":{"content":5}}"#.to_owned(), "'int' object is not iterable"),
        (
            format!(r#"{{"type":"assistant","message":{{"content":[{text},5]}}}}"#),
            "'int' object has no attribute 'get'",
        ),
    ];
    for (line, message) in cases {
        let (result, chunks) = claude_stream(&[&line], 0, "").await;
        assert_eq!(result.unwrap_err().to_string(), format!("Claude streaming error: {message}"), "{line}");
        let expected: Vec<(String, bool)> = if line.contains(text) { vec![chunk("a", false)] } else { Vec::new() };
        assert_eq!(chunks, expected, "{line}");
    }
}

#[tokio::test]
async fn claude_exits_nonzero_when_streaming_then_stderr_after_partial_text() {
    let partial = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}"#;
    let (result, chunks) = claude_stream(&[partial], 2, "quota\n").await;
    assert_eq!(result.unwrap_err().to_string(), "Claude streaming error: Claude CLI error: quota");
    assert_eq!(chunks, [chunk("partial", false)]);
}

#[tokio::test]
async fn no_text_and_clean_exit_when_streaming_claude_then_no_chunk() {
    let (result, chunks) = claude_stream(&[r#"{"type":"system"}"#], 0, "").await;
    result.unwrap();
    assert!(chunks.is_empty());
}

#[test]
fn claude_effort_when_named_then_shown_only_when_nonempty() {
    let stub = stubbed("claude", "", "", 0);
    assert_eq!(claude(&stub, "analysis", Some("max")).name(), "Claude (claude-opus-5-5 max)");
    assert_eq!(claude(&stub, "analysis", Some("")).name(), "Claude (claude-opus-5-5)");
    assert_eq!(claude(&stub, "analysis", None).name(), "Claude (claude-opus-5-5)");
}

// --- node_env ---

fn node_at(home: &Path, version_dir: &str) -> std::path::PathBuf {
    let bin = home.join(version_dir).join("bin");
    executable(&bin.join("node"));
    bin
}

#[test]
fn node_on_path_when_node_env_built_then_environment_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    executable(&bin.join("node"));
    node_at(&dir.path().join("home"), ".nvm/versions/node/v20.0.0");
    let env = CliEnv::from_pairs([
        ("PATH", bin.display().to_string()),
        ("HOME", dir.path().join("home").display().to_string()),
    ]);
    assert_eq!(node_env(&env).unwrap(), env);
}

#[test]
fn several_nvm_versions_when_node_env_built_then_newest_with_node_is_prepended() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    node_at(&home, ".nvm/versions/node/v9.0.0");
    let newest = node_at(&home, ".nvm/versions/node/v20.1.0");
    std::fs::create_dir_all(home.join(".nvm/versions/node/v21.0.0/bin")).unwrap();
    let env = CliEnv::from_pairs([("PATH", "/nowhere".to_owned()), ("HOME", home.display().to_string())]);
    let built = node_env(&env).unwrap();
    assert_eq!(built.get("PATH").unwrap().to_string_lossy(), format!("{}:/nowhere", newest.display()));
    assert_eq!(built.get("HOME"), env.get("HOME"));
}

#[test]
fn both_nvm_layouts_when_node_env_built_then_local_share_wins() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let share = node_at(&home, ".local/share/nvm/v18.0.0");
    node_at(&home, ".nvm/versions/node/v22.0.0");
    let env = CliEnv::from_pairs([("PATH", "/nowhere".to_owned()), ("HOME", home.display().to_string())]);
    let built = node_env(&env).unwrap();
    assert_eq!(built.get("PATH").unwrap().to_string_lossy(), format!("{}:/nowhere", share.display()));
}

#[test]
fn no_node_anywhere_when_node_env_built_then_environment_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".nvm/versions/node/v20.0.0/bin")).unwrap();
    let env = CliEnv::from_pairs([("PATH", "/nowhere".to_owned()), ("HOME", home.display().to_string())]);
    assert_eq!(node_env(&env).unwrap(), env);
}

#[test]
fn nvm_base_is_a_file_when_node_env_built_then_os_error() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".local/share")).unwrap();
    std::fs::write(home.join(".local/share/nvm"), "").unwrap();
    let env = CliEnv::from_pairs([("PATH", "/nowhere".to_owned()), ("HOME", home.display().to_string())]);
    assert!(node_env(&env).is_err());
}

// --- tests/test_claude_effort.py: each provider gets only its own effort ---

#[tokio::test]
async fn independent_efforts_given_generation_when_spawned_then_claude_gets_only_its_own() {
    for stream in [false, true] {
        for effort in ["", "max"] {
            let dir = tempfile::tempdir().unwrap();
            let store =
                AiSettingsStore::new(dir.path(), Some(dir.path().join("prompts"))).with_legacy_env(Default::default());
            let values =
                recog_ai::json::Json::parse(&format!(r#"{{"reasoning_effort": "low", "claude_effort": "{effort}"}}"#))
                    .unwrap();
            store.write_task("analysis.increment", &values, None).unwrap();
            let settings = store.resolve_task("analysis.increment", None, Fallback::default()).unwrap();
            let bin = dir.path().join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            shell_stub(
                &bin,
                "claude",
                if stream { "{\"type\":\"result\",\"result\":\"answer\"}\n" } else { "answer" },
                "",
                0,
            );
            shell_stub(&bin, "codex", "", "", 0);
            let env = stub_env(&bin, &dir.path().join("home"));
            let claude = provider_for_settings(&settings, "analysis", Some("claude"), Some(env.clone())).unwrap();
            let codex = provider_for_settings(&settings, "analysis", Some("codex"), Some(env)).unwrap();
            assert_eq!(codex.reasoning_effort(), Some("low"));
            assert_eq!(claude.reasoning_effort(), Some(effort));
            if stream {
                let mut chunks = Vec::new();
                claude.stream("hello", &mut |text, done| chunks.push((text, done))).await.unwrap();
                assert_eq!(chunks, [("answer".to_owned(), true)]);
            } else {
                assert_eq!(claude.complete("hello").await.unwrap(), "answer");
            }
            let argv = stub_argv(&bin, "claude");
            assert!(argv.contains(&"--safe-mode".to_owned()) && argv.contains(&"--disable-slash-commands".to_owned()));
            assert!(!argv.contains(&"--bare".to_owned()));
            match argv.iter().position(|arg| arg == "--effort") {
                Some(index) => assert_eq!(argv[index + 1], effort),
                None => assert!(effort.is_empty(), "missing --effort {effort}"),
            }
        }
    }
}
