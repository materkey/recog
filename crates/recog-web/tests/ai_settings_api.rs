//! The cases of tests/test_ai_settings_api.py: the AI settings routes over temporary
//! data and the acknowledgement of the live Copilot policy.

mod support;

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use recog_ai::AiError;
use recog_ai::install::LegacyEnv;
use recog_ai::json::Json;
use recog_ai::settings::{AiSettingsStore, Fallback};
use recog_core::pyjson::{Map, Value};
use recog_web::ai_settings::{AiSettingsCoordinator, AiStore};
use recog_web::bridge_subscriber::RawSender;
use support::{App, fake_bridge, get, next, obj, recorder, text};
use tokio::sync::mpsc::UnboundedReceiver;

fn store(app: &App) -> AiSettingsStore {
    AiSettingsStore::new(app.data(), Some(app.state.config.prompts_dir.clone())).with_legacy_env(LegacyEnv::default())
}

fn revision(app: &App) -> String {
    store(app).read().unwrap().get("revision").and_then(Json::as_str).unwrap().to_owned()
}

async fn connect(
    coordinator: &AiSettingsCoordinator,
    revision: &str,
) -> (Arc<dyn RawSender>, UnboundedReceiver<Value>) {
    let (bridge, mut rx) = fake_bridge(true);
    coordinator.connected(Arc::clone(&bridge)).await.unwrap();
    assert_eq!(next(&mut rx).await, obj(&format!(r#"{{"type": "ai_settings_refresh", "revision": "{revision}"}}"#)));
    (bridge, rx)
}

async fn acknowledge(coordinator: &AiSettingsCoordinator, revision: &str) {
    let frame = obj(&format!(
        r#"{{"type": "ai_settings_applied", "revision": "{revision}", "model_application": "next_session"}}"#
    ));
    coordinator.handle_frame(frame.as_object().unwrap()).await.unwrap();
}

fn task_payload(revision: &str, prompt: &str, extra: &str) -> String {
    format!(r#"{{"revision": "{revision}", "prompt": "{prompt}", "providers": ["claude"], "models": {{}}{extra}}}"#)
}

fn task_view<'a>(view: &'a Value, task_id: &str) -> &'a Value {
    let Value::Array(tasks) = get(view, "tasks") else { panic!("{view:?}") };
    tasks.iter().find(|t| text(t, "id") == task_id).unwrap()
}

fn state(view: &Value) -> String {
    text(get(view, "copilot_application"), "state")
}

fn listening(app: &App) -> UnboundedReceiver<String> {
    let (client, rx) = recog_web::util::Client::new();
    app.state.live_watcher.register(client);
    rx
}

fn last(rx: &mut UnboundedReceiver<String>) -> Option<Value> {
    let mut last = None;
    while let Ok(frame) = rx.try_recv() {
        last = Some(obj(&frame));
    }
    last
}

#[tokio::test]
async fn claude_effort_given_an_api_save_when_reloaded_then_preserved_and_an_invalid_value_rejected() {
    let app = App::without_settings();

    let (status, body) = app
        .json(
            "PUT",
            "/api/ai-settings/tasks/copilot",
            Some(&task_payload("defaults", "Reply in English", r#", "claude_effort": "max""#)),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(text(task_view(&body, "copilot"), "claude_effort"), "max");
    let loaded = app.json("GET", "/api/ai-settings", None).await.1;
    assert_eq!(text(task_view(&loaded, "copilot"), "claude_effort"), "max");

    let (status, _) = app
        .json(
            "PUT",
            "/api/ai-settings/tasks/copilot",
            Some(&task_payload(&revision(&app), "Reply in English", r#", "claude_effort": "minimal""#)),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(store(&app).resolve_task("copilot", None, Fallback::default()).unwrap().claude_effort, "max");
}

#[tokio::test]
async fn removed_features_given_a_request_when_routed_then_unavailable() {
    let app = App::without_settings();

    assert_eq!(app.call("POST", "/api/live/anchor", Some("{}")).await.0, 404);
    assert_eq!(app.call("GET", "/api/speakers", None).await.0, 404);
    assert_eq!(app.call("POST", "/api/speakers/enroll-from-segments", Some("{}")).await.0, 404);
    assert_eq!(app.call("POST", "/api/meetings/test/analyze", Some(r#"{"question": "custom"}"#)).await.0, 422);
    assert_eq!(app.call("GET", "/api/meetings/test/analyze-stream?question=custom", None).await.0, 422);
}

#[tokio::test]
async fn retired_summary_given_a_request_when_made_then_no_setting_or_generation_is_available() {
    let app = App::without_settings();

    let settings = app.json("GET", "/api/ai-settings", None).await.1;
    let Value::Array(tasks) = get(&settings, "tasks") else { panic!() };
    assert!(!tasks.iter().any(|t| text(t, "id") == "analysis.summary"));
    for endpoint in ["analyze", "analyze-multi"] {
        assert_eq!(
            app.call("POST", &format!("/api/meetings/test/{endpoint}"), Some(r#"{"question": "summary"}"#)).await.0,
            422
        );
    }
    assert_eq!(app.call("GET", "/api/meetings/test/analyze-stream?question=summary", None).await.0, 422);
    let body = r#"{"revision": "defaults", "prompt": "{transcript}", "providers": ["claude"]}"#;
    assert_eq!(app.call("PUT", "/api/ai-settings/tasks/analysis.summary", Some(body)).await.0, 422);
    assert!(!store(&app).path.exists());
}

#[tokio::test]
async fn copilot_switch_given_an_api_save_when_reloaded_then_the_boolean_is_persisted() {
    let app = App::without_settings();

    let (status, body) = app
        .json(
            "PUT",
            "/api/ai-settings/tasks/copilot",
            Some(&task_payload("defaults", "Reply in English", r#", "enabled": false"#)),
        )
        .await;

    assert_eq!(status, 200);
    assert_eq!(get(task_view(&body, "copilot"), "enabled"), &Value::Bool(false));
    let loaded = app.json("GET", "/api/ai-settings", None).await.1;
    assert_eq!(get(task_view(&loaded, "copilot"), "enabled"), &Value::Bool(false));
    let invalid = app
        .call(
            "PUT",
            "/api/ai-settings/tasks/copilot",
            Some(&task_payload(&revision(&app), "x", r#", "enabled": "false""#)),
        )
        .await;
    assert_eq!(invalid.0, 422);
}

#[tokio::test]
async fn access_off_when_saved_then_view_off_and_bridge_refreshed() {
    let app = App::without_settings();
    let coordinator = &app.state.ai_settings;
    let (_bridge, mut rx) = connect(coordinator, "defaults").await;

    let payload = r#"{"access": "off", "revision": "defaults"}"#;
    let ((status, body), ()) = tokio::join!(app.json("PUT", "/api/ai-settings/access", Some(payload)), async {
        let refresh = next(&mut rx).await;
        assert_eq!(text(&refresh, "type"), "ai_settings_refresh");
        acknowledge(coordinator, &revision(&app)).await;
    });

    assert_eq!(status, 200);
    assert_eq!(text(&body, "access"), "off");
    assert_eq!(text(&app.json("GET", "/api/ai-settings", None).await.1, "access"), "off");
}

#[tokio::test]
async fn access_unasked_when_saved_then_422() {
    let app = App::without_settings();

    let unasked =
        app.call("PUT", "/api/ai-settings/access", Some(r#"{"access": "unasked", "revision": "defaults"}"#)).await;
    let unknown =
        app.call("PUT", "/api/ai-settings/access", Some(r#"{"access": "sometimes", "revision": "defaults"}"#)).await;

    assert_eq!(unasked.0, 422);
    assert_eq!(unknown.0, 422);
    assert!(!store(&app).path.exists());
}

#[tokio::test]
async fn stale_revision_when_access_saved_then_409() {
    let app = App::without_settings();
    store(&app).write_models(&Json::parse(r#"{"claude": "human-edit"}"#).unwrap(), None).unwrap();
    let before = std::fs::read(&store(&app).path).unwrap();

    let (status, _) =
        app.call("PUT", "/api/ai-settings/access", Some(r#"{"access": "off", "revision": "defaults"}"#)).await;

    assert_eq!(status, 409);
    assert_eq!(std::fs::read(&store(&app).path).unwrap(), before);
}

#[tokio::test]
async fn absent_settings_given_get_when_resolved_then_defaults_without_persistence() {
    let app = App::without_settings();

    let (status, body) = app.json("GET", "/api/ai-settings", None).await;

    assert_eq!(status, 200);
    assert_eq!(text(&body, "revision"), "defaults");
    assert_eq!(get(&body, "copilot_application"), &obj(r#"{"state": "pending", "model_application": "next_session"}"#));
    let copilot = task_view(&body, "copilot");
    assert_eq!(get(copilot, "providers"), &obj(r#"["claude", "codex"]"#));
    assert_eq!(get(copilot, "allowed_providers"), &obj(r#"["claude", "codex"]"#));
    assert_eq!(get(copilot, "prompt"), get(copilot, "default_prompt"));
    assert!(!store(&app).path.exists());
}

#[tokio::test]
async fn disconnected_bridge_given_a_model_save_when_read_back_then_persisted_pending() {
    let app = App::without_settings();
    let mut rx = listening(&app);

    let (status, body) = app
        .json(
            "PUT",
            "/api/ai-settings/models",
            Some(r#"{"revision": "defaults", "models": {"claude": "selected-claude", "codex": "selected-codex"}}"#),
        )
        .await;
    let other_browser = app.json("GET", "/api/ai-settings", None).await.1;

    assert_eq!(status, 200);
    assert_eq!(body, other_browser);
    assert_ne!(text(&body, "revision"), "defaults");
    assert_eq!(text(get(&body, "models"), "claude"), "selected-claude");
    assert_eq!(text(get(task_view(&body, "copilot"), "models"), "claude"), "selected-claude");
    assert_eq!(state(&body), "pending");
    let mut expected = Map::new();
    expected.insert("type", Value::from("ai_settings_status"));
    expected.insert("revision", get(&body, "revision").clone());
    expected.insert("copilot_application", get(&body, "copilot_application").clone());
    assert_eq!(last(&mut rx), Some(Value::Object(expected)));
}

#[tokio::test]
async fn connected_bridge_given_a_task_save_when_the_current_ack_arrives_then_applied() {
    let app = App::without_settings();
    let coordinator = &app.state.ai_settings;
    let (_bridge, mut rx) = connect(coordinator, "defaults").await;
    let mut live = listening(&app);

    let payload = task_payload("defaults", "Reply in English", "");
    let ((status, body), ()) = tokio::join!(app.json("PUT", "/api/ai-settings/tasks/copilot", Some(&payload)), async {
        let refresh = next(&mut rx).await;
        assert_eq!(text(&refresh, "revision"), revision(&app));
        assert_eq!(state(&coordinator.view().await.unwrap()), "pending");
        acknowledge(coordinator, &revision(&app)).await;
    });

    assert_eq!(status, 200);
    assert_eq!(text(task_view(&body, "copilot"), "prompt"), "Reply in English");
    assert_eq!(get(&body, "copilot_application"), &obj(r#"{"state": "applied", "model_application": "next_session"}"#));
    assert_eq!(state(&last(&mut live).unwrap()), "applied");
}

#[tokio::test]
async fn stale_browser_given_a_model_task_or_reset_when_written_then_409_and_the_settings_kept() {
    let app = App::without_settings();
    let current = store(&app).write_models(&Json::parse(r#"{"claude": "human-edit"}"#).unwrap(), None).unwrap();
    let before = std::fs::read(&store(&app).path).unwrap();
    let mut live = listening(&app);

    let statuses = [
        app.call("PUT", "/api/ai-settings/models", Some(r#"{"revision": "defaults", "models": {"claude": "stale"}}"#))
            .await
            .0,
        app.call("PUT", "/api/ai-settings/tasks/copilot", Some(&task_payload("defaults", "Reply", ""))).await.0,
        app.call("DELETE", "/api/ai-settings/tasks/copilot?revision=defaults", None).await.0,
    ];

    assert!(statuses.iter().all(|s| *s == 409), "{statuses:?}");
    assert_eq!(std::fs::read(&store(&app).path).unwrap(), before);
    assert_eq!(revision(&app), current.revision);
    assert!(live.try_recv().is_err());
}

#[tokio::test]
async fn task_override_given_a_reset_when_deleted_then_the_defaults_and_the_global_model_return() {
    let app = App::without_settings();
    store(&app).write_models(&Json::parse(r#"{"claude": "global-model"}"#).unwrap(), None).unwrap();
    let original = store(&app)
        .write_task(
            "copilot",
            &Json::parse(r#"{"prompt": "Custom policy", "models": {"claude": "task-model"}}"#).unwrap(),
            None,
        )
        .unwrap();

    let (status, body) =
        app.json("DELETE", &format!("/api/ai-settings/tasks/copilot?revision={}", original.revision), None).await;

    assert_eq!(status, 200);
    assert_ne!(text(&body, "revision"), original.revision);
    let task = task_view(&body, "copilot");
    assert_eq!(get(task, "prompt"), get(task, "default_prompt"));
    assert_eq!(text(get(task, "models"), "claude"), "global-model");
    assert_eq!(get(task, "customized"), &Value::Bool(false));
    assert_eq!(state(&body), "pending");
}

#[tokio::test]
async fn invalid_task_given_put_when_validated_then_422_and_no_mutation() {
    for extra in [
        r#", "providers": ["codex", "codex"]"#,
        r#", "providers": ["gemini"]"#,
        r#", "providers": []"#,
        r#", "prompt": """#,
        &format!(r#", "prompt": "{}""#, "x".repeat(32001)),
        r#", "models": {"claude": "has whitespace"}"#,
        r#", "models": {"claude": "--bad-option"}"#,
        r#", "models": {"unknown": "model"}"#,
        r#", "secret": "never-store-this""#,
        r#", "prompt": null"#,
        r#", "revision": null"#,
    ] {
        let app = App::without_settings();
        store(&app).write_task("copilot", &Json::parse(r#"{"prompt": "Original"}"#).unwrap(), None).unwrap();
        let before = std::fs::read(&store(&app).path).unwrap();
        let mut live = listening(&app);
        // A key given twice keeps the last value, as Python's dict does.
        let body = task_payload(&revision(&app), "Reply in English", extra);

        let (status, _) = app.call("PUT", "/api/ai-settings/tasks/copilot", Some(&body)).await;

        assert_eq!(status, 422, "{extra}");
        assert_eq!(std::fs::read(&store(&app).path).unwrap(), before);
        assert!(live.try_recv().is_err());
    }
}

#[tokio::test]
async fn invalid_scenario_given_put_when_validated_then_422_without_creating_settings() {
    for (route, payload) in [
        ("analysis.increment", r#"{"prompt": "Missing transcript", "providers": ["claude"]}"#),
        ("title", r#"{"prompt": "{transcript} {unknown}", "providers": ["codex"]}"#),
        ("title", r#"{"prompt": "{transcript}", "providers": ["codex"], "reasoning_effort": "invalid"}"#),
        ("unknown", r#"{"prompt": "{transcript}", "providers": ["claude"]}"#),
    ] {
        let app = App::without_settings();
        let body = payload.replacen('{', r#"{"revision": "defaults", "#, 1);

        let (status, _) = app.call("PUT", &format!("/api/ai-settings/tasks/{route}"), Some(&body)).await;

        assert_eq!(status, 422, "{route} {payload}");
        assert!(!store(&app).path.exists());
    }
}

#[tokio::test]
async fn overlapping_saves_given_an_old_ack_when_the_current_revision_differs_then_it_stays_pending() {
    let app = App::without_settings();
    let coordinator = &app.state.ai_settings;
    let (_bridge, mut rx) = connect(coordinator, "defaults").await;

    let first_payload = task_payload("defaults", "First", "");
    let (first, second, ()) = tokio::join!(
        app.json("PUT", "/api/ai-settings/tasks/copilot", Some(&first_payload)),
        async {
            // The second browser saves over the first one's revision.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while revision(&app) == "defaults" {
                assert!(std::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            app.json("PUT", "/api/ai-settings/tasks/copilot", Some(&task_payload(&revision(&app), "Second", ""))).await
        },
        async {
            let old = text(&next(&mut rx).await, "revision");
            let new = text(&next(&mut rx).await, "revision");
            assert_ne!(old, new);
            acknowledge(coordinator, &old).await;
            let pending = app.json("GET", "/api/ai-settings", None).await.1;
            assert_eq!(text(&pending, "revision"), new);
            assert_eq!(state(&pending), "pending");
            acknowledge(coordinator, &new).await;
        },
    );

    assert_eq!((first.0, second.0), (200.try_into().unwrap(), 200.try_into().unwrap()));
    assert_eq!(text(task_view(&first.1, "copilot"), "prompt"), "First");
    assert_eq!(state(&first.1), "pending");
    assert_eq!(text(task_view(&second.1, "copilot"), "prompt"), "Second");
    assert_eq!(state(&second.1), "applied");
    assert_ne!(text(&first.1, "revision"), text(&second.1, "revision"));
}

#[tokio::test]
async fn applied_settings_given_a_disconnect_and_reconnect_when_the_revision_is_the_same_then_a_fresh_ack_is_required()
{
    let app = App::without_settings();
    let coordinator = &app.state.ai_settings;
    connect(coordinator, "defaults").await;
    acknowledge(coordinator, "defaults").await;
    assert_eq!(state(&coordinator.view().await.unwrap()), "applied");

    coordinator.disconnected().await.unwrap();
    acknowledge(coordinator, "defaults").await;
    assert_eq!(state(&coordinator.view().await.unwrap()), "pending");
    connect(coordinator, "defaults").await;
    assert_eq!(state(&coordinator.view().await.unwrap()), "pending");
    acknowledge(coordinator, "defaults").await;
    assert_eq!(state(&coordinator.view().await.unwrap()), "applied");
}

/// An AI settings store whose next read, once armed, waits until released.
struct PausingStore {
    inner: AiSettingsStore,
    armed: Mutex<bool>,
    gate: (Mutex<(bool, bool)>, Condvar),
}

impl AiStore for PausingStore {
    fn read(&self) -> Result<Value, AiError> {
        let pause = std::mem::take(&mut *self.armed.lock().unwrap());
        let result = AiStore::read(&self.inner);
        if pause {
            let mut gate = self.gate.0.lock().unwrap();
            gate.0 = true;
            let (gate, _) = self.gate.1.wait_timeout_while(gate, Duration::from_secs(3), |g| !g.1).unwrap();
            assert!(gate.1, "the paused read was never released");
        }
        result
    }
    fn view(&self) -> Result<Value, AiError> {
        AiStore::view(&self.inner)
    }
    fn write_models(&self, models: &Value, expected: Option<&str>) -> Result<Value, AiError> {
        AiStore::write_models(&self.inner, models, expected)
    }
    fn write_access(&self, access: &str, expected: Option<&str>) -> Result<Value, AiError> {
        AiStore::write_access(&self.inner, access, expected)
    }
    fn write_task(&self, task_id: &str, values: &Value, expected: Option<&str>) -> Result<Value, AiError> {
        AiStore::write_task(&self.inner, task_id, values, expected)
    }
    fn reset_task(&self, task_id: &str, expected: Option<&str>) -> Result<Value, AiError> {
        AiStore::reset_task(&self.inner, task_id, expected)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_connection_given_an_ack_read_in_flight_when_reconnected_then_the_stale_ack_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(PausingStore {
        inner: AiSettingsStore::new(dir.path(), Some(support::repo_root().join("crates/recog-ai/prompts")))
            .with_legacy_env(LegacyEnv::default()),
        armed: Mutex::new(false),
        gate: (Mutex::new((false, false)), Condvar::new()),
    });
    let (broadcast, _) = recorder();
    let coordinator = Arc::new(AiSettingsCoordinator::new(Arc::clone(&store) as Arc<dyn AiStore>, broadcast));
    connect(&coordinator, "defaults").await;

    *store.armed.lock().unwrap() = true;
    let old_ack = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { acknowledge(&coordinator, "defaults").await })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !store.gate.0.lock().unwrap().0 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    coordinator.disconnected().await.unwrap();
    connect(&coordinator, "defaults").await;
    store.gate.0.lock().unwrap().1 = true;
    store.gate.1.notify_all();
    old_ack.await.unwrap();

    assert_eq!(state(&coordinator.view().await.unwrap()), "pending");
    acknowledge(&coordinator, "defaults").await;
    assert_eq!(state(&coordinator.view().await.unwrap()), "applied");
}

#[tokio::test]
async fn bridge_apply_error_given_a_waiting_save_when_the_error_arrives_then_saved_pending_without_internal_details() {
    let app = App::without_settings();
    let coordinator = &app.state.ai_settings;
    let (_bridge, mut rx) = connect(coordinator, "defaults").await;

    let payload = task_payload("defaults", "Reply in English", "");
    let ((status, body), ()) = tokio::join!(app.json("PUT", "/api/ai-settings/tasks/copilot", Some(&payload)), async {
        next(&mut rx).await;
        let frame = obj(r#"{"type": "ai_settings_error", "message": "private runtime detail"}"#);
        coordinator.handle_frame(frame.as_object().unwrap()).await.unwrap();
    });

    assert_eq!(status, 200);
    assert_eq!(text(task_view(&body, "copilot"), "prompt"), "Reply in English");
    assert_eq!(state(&body), "pending");
    assert!(!text(get(&body, "copilot_application"), "error").is_empty());
    assert!(!recog_core::pyjson::dumps(&body, None).contains("private runtime detail"));
    acknowledge(coordinator, &revision(&app)).await;
    assert!(!get(&coordinator.view().await.unwrap(), "copilot_application").as_object().unwrap().contains_key("error"));
}

#[tokio::test]
async fn bridge_send_failure_given_a_save_when_not_delivered_then_saved_pending() {
    let app = App::without_settings();
    let (bridge, _rx) = fake_bridge(false);
    app.state.ai_settings.connected(bridge).await.unwrap();

    let (status, body) = app
        .json("PUT", "/api/ai-settings/tasks/copilot", Some(&task_payload("defaults", "Reply in English", "")))
        .await;

    assert_eq!(status, 200);
    assert_eq!(text(task_view(&body, "copilot"), "prompt"), "Reply in English");
    assert_eq!(state(&body), "pending");
}

#[tokio::test]
async fn unwritable_data_directory_given_put_when_the_file_system_fails_then_500_without_details_or_broadcast() {
    let app = App::without_settings();
    std::fs::create_dir_all(app.data()).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(app.data(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut live = listening(&app);

    let (status, body) = app
        .call("PUT", "/api/ai-settings/models", Some(r#"{"revision": "defaults", "models": {"claude": "new"}}"#))
        .await;

    std::fs::set_permissions(app.data(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(status, 500);
    assert!(!String::from_utf8_lossy(&body).contains(&app.data().to_string_lossy().into_owned()));
    assert!(!store(&app).path.exists());
    assert!(live.try_recv().is_err());
}

#[tokio::test]
async fn corrupt_file_given_get_when_the_read_fails_then_500_without_overwriting() {
    let app = App::without_settings();
    support::write(&store(&app).path, "broken JSON");

    let (status, _) = app.call("GET", "/api/ai-settings", None).await;

    assert_eq!(status, 500);
    assert_eq!(std::fs::read(&store(&app).path).unwrap(), b"broken JSON");
}

#[tokio::test]
async fn invalid_saved_storage_given_a_mutation_when_the_read_fails_then_500_without_overwriting() {
    for corrupt in ["broken JSON", r#"{"version":999,"revision":"unsupported","models":{},"tasks":{}}"#] {
        for (method, route, payload) in [
            ("PUT", "/api/ai-settings/models", Some(r#"{"revision": "defaults", "models": {"claude": "new-model"}}"#)),
            (
                "PUT",
                "/api/ai-settings/tasks/copilot",
                Some(r#"{"revision": "defaults", "prompt": "New policy", "providers": ["claude"]}"#),
            ),
            ("DELETE", "/api/ai-settings/tasks/copilot?revision=defaults", None),
        ] {
            let app = App::without_settings();
            support::write(&store(&app).path, corrupt);
            let mut live = listening(&app);

            let (status, _) = app.call(method, route, payload).await;

            assert_eq!(status, 500, "{corrupt} {method} {route}");
            assert_eq!(std::fs::read(&store(&app).path).unwrap(), corrupt.as_bytes());
            assert!(live.try_recv().is_err());
        }
    }
}
