//! A whole Copilot session on Codex: a stub `codex` that speaks the app-server protocol
//! from a shell script, the real channel server with queued events, and the pane.

mod channel_support;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use channel_support::{link_in, mcp_endpoint, object};
use recog_copilot::codex::session::{Options, Pane, prepare_home, run, thread_params};

/// Answers initialize, account/read and thread/start, completes every turn at once with
/// one message, and logs every line it reads to `stub.log` beside itself.
const STUB: &str = r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$(dirname "$0")/stub.log"
  id=$(printf '%s' "$line" | sed -n 's/^{"id":\([0-9]*\),.*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id";;
    *'"method":"account/read"'*) printf '{"id":%s,"result":{"account":{"type":"chatgpt"}}}\n' "$id";;
    *'"method":"thread/start"'*) printf '{"id":%s,"result":{"thread":{"id":"th1"}}}\n' "$id";;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"tu%s"}}}\n' "$id" "$id"
      printf '{"method":"turn/started","params":{"turn":{"id":"tu%s"}}}\n' "$id"
      printf '{"method":"item/completed","params":{"item":{"type":"agentMessage","text":"sent %s"}}}\n' "$id"
      printf '{"method":"turn/completed","params":{"turn":{"id":"tu%s","status":"completed"}}}\n' "$id";;
  esac
done
"#;

fn options(dir: &Path, mcp_url: String) -> Options {
    let codex = dir.join("codex");
    std::fs::write(&codex, STUB).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cwd = dir.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    Options {
        codex,
        codex_home: dir.join("home"),
        cwd,
        mcp_url,
        instructions: "Copilot playbook".into(),
        model: "gpt-test".into(),
        reasoning_effort: "high".into(),
        stderr: None,
    }
}

#[tokio::test]
async fn question_then_meeting_end_when_run_then_two_turns_on_one_thread_and_a_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (_server, address) = mcp_endpoint(link.clone()).await;
    link.events.push(object(r#"{"type": "copilot_user_message", "text": "Что решили?", "request_id": "r1"}"#));
    link.events.push(object(r#"{"type": "meeting_ended", "meeting_id": "m1"}"#));
    let log = dir.path().join("stub.log");
    let options = options(dir.path(), format!("http://{address}/mcp"));
    let mut pane = Pane::new(Vec::new());

    let result = tokio::time::timeout(Duration::from_secs(20), run(&options, &mut pane)).await.unwrap();

    assert_eq!(result, Ok(()));
    let sent = std::fs::read_to_string(&log).unwrap();
    let methods: Vec<String> = sent
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(methods, ["initialize", "account/read", "thread/start", "turn/start", "turn/start"]);
    let turns: Vec<serde_json::Value> = sent
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|message| message["method"] == "turn/start")
        .collect();
    assert_eq!(turns[0]["params"]["threadId"], "th1");
    let question = turns[0]["params"]["input"][0]["text"].as_str().unwrap();
    assert!(question.starts_with(r#"<channel source="copilot" event_type="copilot_user_message""#), "{question}");
    assert!(question.contains("Text: Что решили?"), "{question}");
    assert!(turns[1]["params"]["input"][0]["text"].as_str().unwrap().contains(r#"event_type="meeting_ended""#));
    let shown = String::from_utf8(pane.into_inner()).unwrap();
    assert!(shown.contains("codex: sent 4"), "{shown}");
    assert!(shown.contains("recog-copilot-codex: online"), "{shown}");
}

#[test]
fn copilot_options_when_turned_into_thread_params_then_no_approvals_read_only_and_only_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    let options = options(dir.path(), "http://127.0.0.1:8767/mcp".into());

    let params = thread_params(&options);

    assert_eq!(params["approvalPolicy"], "never");
    assert_eq!(params["sandbox"], "read-only");
    assert_eq!(params["ephemeral"], true);
    assert_eq!(params["model"], "gpt-test");
    assert_eq!(params["developerInstructions"], "Copilot playbook");
    assert_eq!(params["config"]["mcp_servers.copilot.url"], "http://127.0.0.1:8767/mcp");
    assert_eq!(params["config"]["mcp_servers.copilot.default_tools_approval_mode"], "approve");
    assert_eq!(params["config"]["model_reasoning_effort"], "high");
}

#[test]
fn fresh_home_when_prepared_then_the_users_login_is_linked_and_an_existing_one_kept() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("copilot-home");
    let user = dir.path().join("user-codex");

    prepare_home(&home, &user).unwrap();
    prepare_home(&home, &dir.path().join("elsewhere")).unwrap();

    assert_eq!(std::fs::read_link(home.join("auth.json")).unwrap(), user.join("auth.json"));
}

/// The acceptance check of the plan (Task 9) against the real `codex app-server` with
/// the user's login: a typed question reaches the bridge as a Copilot reply. Spends a
/// few Codex turns, so it runs only on request:
/// `cargo test -p recog-copilot --test codex_session -- --ignored`.
#[tokio::test]
#[ignore = "needs an installed, signed-in Codex and spends turns of its subscription"]
async fn real_codex_when_the_user_asks_then_the_reply_reaches_the_bridge() {
    use recog_ai::provider::{CliEnv, resolve_cli};
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let socket = channel_support::RecordingSocket::new();
    link.set_socket(Some(socket.clone()));
    let (_server, address) = mcp_endpoint(link.clone()).await;
    let user = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".codex");
    let options = Options {
        codex: resolve_cli("codex", &CliEnv::from_process()).unwrap(),
        codex_home: dir.path().join("home"),
        cwd: dir.path().join("cwd"),
        mcp_url: format!("http://{address}/mcp"),
        instructions: recog_copilot::codex::instructions::developer_instructions(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        )
        .unwrap(),
        model: recog_ai::defaults::DEFAULT_CODEX_MODEL.into(),
        reasoning_effort: "low".into(),
        stderr: Some(dir.path().join("codex.stderr.log")),
    };
    prepare_home(&options.codex_home, &user).unwrap();
    std::fs::create_dir_all(&options.cwd).unwrap();
    link.events.push(object(
        r#"{"type": "copilot_user_message", "text": "Сколько будет 2+2? Ответь одним числом.", "request_id": "r1", "meeting_id": "m1"}"#,
    ));
    let session = tokio::spawn(async move {
        let mut pane = Pane::new(Vec::new());
        let result = run(&options, &mut pane).await;
        (result, String::from_utf8(pane.into_inner()).unwrap())
    });

    let mut reply = None;
    for _ in 0..240 {
        reply = socket.frames().into_iter().find(|frame| frame.get("request_id").is_some_and(|id| id.py_str() == "r1"));
        if reply.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    link.events.push(object(r#"{"type": "meeting_ended", "meeting_id": "m1"}"#));
    let (result, shown) = tokio::time::timeout(Duration::from_secs(180), session).await.unwrap().unwrap();

    let reply = reply.unwrap_or_else(|| panic!("no reply; pane:\n{shown}"));
    eprintln!("reply frame: {}\npane:\n{shown}", channel_support::dumps(&reply));
    assert!(reply.get("text").is_some_and(|text| text.py_str().contains('4')), "{reply:?}");
    assert_eq!(result, Ok(()), "{shown}");
}

/// A checkout with only the playbook files the harness reads.
fn checkout(dir: &Path) -> std::path::PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = dir.join("repo");
    for file in [".claude/agents/copilot-session.md", ".claude/skills/copilot-mode/SKILL.md"] {
        std::fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
        std::fs::copy(source.join(file), repo.join(file)).unwrap();
    }
    repo
}

#[tokio::test]
async fn installed_program_when_started_then_its_own_codex_home_and_a_session_to_the_meetings_end() {
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (_server, address) = mcp_endpoint(link.clone()).await;
    link.events.push(object(r#"{"type": "copilot_user_message", "text": "Вопрос", "request_id": "r1"}"#));
    link.events.push(object(r#"{"type": "meeting_ended", "meeting_id": "m1"}"#));
    let stub = options(dir.path(), String::new()).codex;
    let home = dir.path().join("home");
    let data = dir.path().join("data");
    let log = dir.path().join("stub.log");
    let path = format!("{}:{}", stub.parent().unwrap().display(), std::env::var("PATH").unwrap());

    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_recog-copilot-codex"))
            .arg("--repo-root")
            .arg(checkout(dir.path()))
            .env("PATH", path)
            .env("HOME", &home)
            .env_remove("CODEX_HOME")
            .env("RECOG_DATA_DIR", &data)
            .env("RECOG_CONFIG_FILE", dir.path().join("missing-config.toml"))
            .env("RECOG_COPILOT_MCP_URL", format!("http://{address}/mcp"))
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let sent = std::fs::read_to_string(&log).unwrap();
    assert_eq!(sent.matches(r#""method":"turn/start""#).count(), 2, "{sent}");
    assert!(sent.contains("You run under Codex"), "{sent}");
    let login = data.join("copilot/codex-home/auth.json");
    assert_eq!(std::fs::read_link(login).unwrap(), home.join(".codex/auth.json"));
    assert!(data.join("copilot/codex-cwd").is_dir());
    assert!(data.join("logs/copilot-codex.stderr.log").exists());
}

#[tokio::test]
async fn codex_gone_after_the_thread_when_run_then_the_session_ends_with_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, address) = mcp_endpoint(link_in(&dir)).await;
    let mut options = options(dir.path(), format!("http://{address}/mcp"));
    let quitting = STUB.replace(
        r#"printf '{"id":%s,"result":{"thread":{"id":"th1"}}}\n' "$id";;"#,
        r#"printf '{"id":%s,"result":{"thread":{"id":"th1"}}}\n' "$id"; exit 0;;"#,
    );
    assert_ne!(quitting, STUB);
    std::fs::write(&options.codex, quitting).unwrap();
    options.stderr = Some(dir.path().join("stderr.log"));

    let result = tokio::time::timeout(Duration::from_secs(20), run(&options, &mut Pane::new(Vec::new()))).await;

    assert_eq!(result.unwrap(), Err("codex app-server exited".to_owned()));
}
