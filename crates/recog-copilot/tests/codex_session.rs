//! A whole Copilot session on Codex: a stub `codex` that speaks the app-server protocol
//! from a shell script, the real channel server with queued events, and the pane.

mod channel_support;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use channel_support::{link_in, mcp_endpoint, object};
use recog_copilot::codex::session::{Options, Pane, prepare_home, run, thread_params};

/// Answers initialize, account/read and thread/start, completes every turn at once with
/// one message, and logs every line it reads to $STUB_LOG.
const STUB: &str = r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$STUB_LOG"
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
    // SAFETY of the test: the stub reads its log path from the environment it inherits.
    unsafe { std::env::set_var("STUB_LOG", &log) };
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
