//! The cases of tests/test_user_profile_api.py and the browser case of
//! tests/test_privacy.py: the profile routes over temporary data and the live
//! acknowledgement protocol (pending until an acknowledgement of exactly the saved
//! revision on the current bridge connection).

mod support;

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use recog_core::privacy::{CopilotPrivacy, PrivacyStore};
use recog_core::pyjson::{Map, Value};
use recog_core::user_profile::{Profile, ProfileError, UserProfileStore, resolve_identity};
use recog_web::bridge_subscriber::RawSender;
use recog_web::user_profile::{ProfileCoordinator, ProfileStore};
use support::{App, fake_bridge, get, next, obj, recorder, text};
use tokio::sync::mpsc::UnboundedReceiver;

fn app() -> App {
    App::with(|config| config.mic_speaker_name = "Legacy".into())
}

fn store(app: &App) -> UserProfileStore {
    UserProfileStore::new(&app.data())
}

async fn connect(coordinator: &ProfileCoordinator, revision: &str) -> (Arc<dyn RawSender>, UnboundedReceiver<Value>) {
    let (bridge, mut rx) = fake_bridge(true);
    coordinator.connected(Arc::clone(&bridge)).await.unwrap();
    assert_eq!(next(&mut rx).await, obj(&format!(r#"{{"type": "user_profile_refresh", "revision": "{revision}"}}"#)));
    (bridge, rx)
}

fn applied(profile: &Profile, override_name: Option<&str>) -> Map {
    let mut frame = Map::new();
    frame.insert("type", Value::from("user_profile_applied"));
    frame.insert("revision", Value::from(profile.revision.as_str()));
    frame.insert("identity", resolve_identity(profile, Some("Legacy"), override_name));
    frame
}

async fn acknowledge(coordinator: &ProfileCoordinator, profile: &Profile, override_name: Option<&str>) {
    coordinator.handle_frame(&applied(profile, override_name)).await.unwrap();
}

#[tokio::test]
async fn missing_profile_when_get_then_the_legacy_name_is_suggested_and_nothing_is_written() {
    let app = app();

    let (status, body) = app.json("GET", "/api/user-profile", None).await;

    assert_eq!(status, 200);
    assert_eq!(text(&body, "suggested_name"), "Legacy");
    assert_eq!(get(get(&body, "profile"), "configured"), &Value::Bool(false));
    assert_eq!(text(get(&body, "identity"), "local_user_name"), "Legacy");
    assert_eq!(text(&body, "application_state"), "pending");
    assert!(!store(&app).path.exists());
}

#[tokio::test]
async fn offline_bridge_when_put_then_saved_pending_and_broadcast() {
    let app = app();
    let (client, mut rx) = recog_web::util::Client::new();
    app.state.live_watcher.register(client);

    let (status, body) =
        app.json("PUT", "/api/user-profile", Some(r#"{"name": " Алексей ", "aliases": ["Лёша"]}"#)).await;
    let (_, second_browser) = app.json("GET", "/api/user-profile", None).await;

    assert_eq!(status, 200);
    assert_eq!(get(&body, "profile"), &store(&app).read().unwrap().to_json());
    assert_eq!(text(get(&body, "profile"), "name"), "Алексей");
    assert_eq!(get(get(&body, "profile"), "aliases"), &obj(r#"["Лёша"]"#));
    assert_eq!(text(&body, "application_state"), "pending");
    assert_eq!(second_browser, body);
    let mut last = None;
    while let Ok(frame) = rx.try_recv() {
        last = Some(obj(&frame));
    }
    let mut expected = Map::new();
    expected.insert("type", Value::from("user_profile_status"));
    for (k, v) in body.as_object().unwrap().iter() {
        expected.insert(k.clone(), v.clone());
    }
    assert_eq!(last, Some(Value::Object(expected)));
}

#[tokio::test]
async fn connected_bridge_when_a_valid_ack_arrives_then_put_reports_the_applied_identity() {
    let app = app();
    let coordinator = &app.state.profile;
    let (_bridge, mut rx) = connect(coordinator, "unconfigured").await;

    let ((status, body), ()) =
        tokio::join!(app.json("PUT", "/api/user-profile", Some(r#"{"name": "Alex", "aliases": ["Al"]}"#)), async {
            let refresh = next(&mut rx).await;
            let profile = store(&app).read().unwrap();
            assert_eq!(text(&refresh, "revision"), profile.revision);
            acknowledge(coordinator, &profile, Some("CLI")).await;
        });

    assert_eq!(status, 200);
    assert_eq!(text(&body, "application_state"), "applied");
    assert_eq!(text(get(&body, "identity"), "local_user_name"), "CLI");
    assert_eq!(get(get(&body, "identity"), "local_user_aliases"), &obj(r#"["Alex", "CLI", "Al"]"#));
    assert_eq!(text(get(&body, "identity"), "local_user_profile_revision"), text(get(&body, "profile"), "revision"));
}

#[tokio::test]
async fn overlapping_saves_when_the_older_ack_arrives_then_the_new_revision_stays_pending() {
    let app = app();
    let coordinator = &app.state.profile;
    let (_bridge, mut rx) = connect(coordinator, "unconfigured").await;

    let (first, second, ()) = tokio::join!(
        app.json("PUT", "/api/user-profile", Some(r#"{"name": "First"}"#)),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            app.json("PUT", "/api/user-profile", Some(r#"{"name": "Second"}"#)).await
        },
        async {
            next(&mut rx).await;
            let first = store(&app).read().unwrap();
            next(&mut rx).await;
            let second = store(&app).read().unwrap();
            assert_ne!(first.revision, second.revision);
            acknowledge(coordinator, &first, None).await;
            let pending = app.json("GET", "/api/user-profile", None).await.1;
            assert_eq!(get(&pending, "profile"), &second.to_json());
            assert_eq!(text(&pending, "application_state"), "pending");
            acknowledge(coordinator, &second, None).await;
        },
    );

    assert_eq!((first.0, second.0), (200.try_into().unwrap(), 200.try_into().unwrap()));
    assert_eq!(text(&second.1, "application_state"), "applied");
    assert_eq!(get(get(&second.1, "identity"), "local_user_aliases"), &obj(r#"["Second"]"#));
}

#[tokio::test]
async fn applied_profile_when_reconnecting_then_the_saved_revision_is_replayed_and_the_old_ack_cleared() {
    let app = app();
    let coordinator = &app.state.profile;
    let original = store(&app).write("Original", &["Old alias".into()]).unwrap();
    connect(coordinator, &original.revision).await;
    acknowledge(coordinator, &original, None).await;
    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "applied");
    coordinator.disconnected().await.unwrap();
    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "pending");

    let fresh = store(&app).write("Fresh", &[]).unwrap();
    connect(coordinator, &fresh.revision).await;

    let pending = coordinator.snapshot().await.unwrap();
    assert_eq!(get(&pending, "profile"), &fresh.to_json());
    assert_eq!(text(&pending, "application_state"), "pending");
    assert_eq!(get(get(&pending, "identity"), "local_user_aliases"), &obj(r#"["Fresh"]"#));
    acknowledge(coordinator, &original, None).await;
    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "pending");
    acknowledge(coordinator, &fresh, None).await;
    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "applied");
}

#[tokio::test]
async fn same_revision_when_reconnected_then_a_new_ack_is_required() {
    let app = app();
    let coordinator = &app.state.profile;
    connect(coordinator, "unconfigured").await;
    acknowledge(coordinator, &store(&app).read().unwrap(), None).await;
    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "applied");

    connect(coordinator, "unconfigured").await;

    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "pending");
}

/// A profile store whose next read, once armed, waits until released.
struct PausingStore {
    inner: UserProfileStore,
    armed: Mutex<bool>,
    gate: (Mutex<(bool, bool)>, Condvar),
}

impl PausingStore {
    fn new(inner: UserProfileStore) -> Arc<Self> {
        Arc::new(Self { inner, armed: Mutex::new(false), gate: (Mutex::new((false, false)), Condvar::new()) })
    }

    fn arm(&self) {
        *self.armed.lock().unwrap() = true;
    }

    fn captured(&self) -> bool {
        self.gate.0.lock().unwrap().0
    }

    fn release(&self) {
        self.gate.0.lock().unwrap().1 = true;
        self.gate.1.notify_all();
    }
}

impl ProfileStore for PausingStore {
    fn read(&self) -> Result<Profile, ProfileError> {
        let pause = std::mem::take(&mut *self.armed.lock().unwrap());
        let result = self.inner.read();
        if pause {
            let mut gate = self.gate.0.lock().unwrap();
            gate.0 = true;
            let (gate, _) = self.gate.1.wait_timeout_while(gate, Duration::from_secs(3), |g| !g.1).unwrap();
            assert!(gate.1, "the paused read was never released");
        }
        result
    }
    fn write(&self, name: &str, aliases: &[String]) -> Result<Profile, ProfileError> {
        self.inner.write(name, aliases)
    }
    fn clear(&self) -> Result<Profile, ProfileError> {
        self.inner.clear()
    }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_ack_read_in_flight_when_a_new_save_is_applied_then_the_new_identity_cannot_be_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let inner = UserProfileStore::new(dir.path());
    let first = inner.write("First", &[]).unwrap();
    let store = PausingStore::new(inner);
    let (broadcast, _) = recorder();
    let coordinator =
        Arc::new(ProfileCoordinator::new(Arc::clone(&store) as Arc<dyn ProfileStore>, "Legacy", broadcast));
    let (_bridge, mut rx) = connect(&coordinator, &first.revision).await;

    store.arm();
    let old_ack = {
        let coordinator = Arc::clone(&coordinator);
        let first = first.clone();
        tokio::spawn(async move { acknowledge(&coordinator, &first, None).await })
    };
    until("the old acknowledgement's read", || store.captured()).await;
    let saving = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move { coordinator.save("Second", &[]).await })
    };
    // The save is serialized behind the old acknowledgement.
    assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
    store.release();
    old_ack.await.unwrap();
    next(&mut rx).await;
    let second = store.inner.read().unwrap();
    acknowledge(&coordinator, &second, None).await;
    saving.await.unwrap().unwrap();

    let snapshot = coordinator.snapshot().await.unwrap();
    assert_eq!(text(get(&snapshot, "profile"), "name"), "Second");
    assert_eq!(text(&snapshot, "application_state"), "applied");
    assert_eq!(get(get(&snapshot, "identity"), "local_user_aliases"), &obj(r#"["Second"]"#));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_connection_ack_read_in_flight_when_reconnected_then_a_fresh_ack_is_still_required() {
    let dir = tempfile::tempdir().unwrap();
    let inner = UserProfileStore::new(dir.path());
    let profile = inner.write("Alex", &[]).unwrap();
    let store = PausingStore::new(inner);
    let (broadcast, _) = recorder();
    let coordinator =
        Arc::new(ProfileCoordinator::new(Arc::clone(&store) as Arc<dyn ProfileStore>, "Legacy", broadcast));
    connect(&coordinator, &profile.revision).await;

    store.arm();
    let old_ack = {
        let coordinator = Arc::clone(&coordinator);
        let profile = profile.clone();
        tokio::spawn(async move { acknowledge(&coordinator, &profile, None).await })
    };
    until("the old acknowledgement's read", || store.captured()).await;
    coordinator.disconnected().await.unwrap();
    connect(&coordinator, &profile.revision).await;
    store.release();
    old_ack.await.unwrap();

    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "pending");
    acknowledge(&coordinator, &profile, None).await;
    assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "applied");
}

#[tokio::test]
async fn saved_profile_when_deleted_then_a_tombstone_and_the_legacy_alias_are_applied() {
    let app = app();
    let coordinator = &app.state.profile;
    let original = store(&app).write("Alex", &["Al".into()]).unwrap();
    let (_bridge, mut rx) = connect(coordinator, &original.revision).await;
    acknowledge(coordinator, &original, None).await;

    let ((status, body), ()) = tokio::join!(app.json("DELETE", "/api/user-profile", None), async {
        next(&mut rx).await;
        let tombstone = store(&app).read().unwrap();
        assert_ne!(tombstone.revision, original.revision);
        assert!(tombstone.name.is_empty() && tombstone.aliases.is_empty());
        acknowledge(coordinator, &tombstone, None).await;
    });

    assert_eq!(status, 200);
    assert_eq!(get(&body, "profile"), &store(&app).read().unwrap().to_json());
    assert_eq!(get(get(&body, "profile"), "configured"), &Value::Bool(false));
    assert_eq!(text(&body, "application_state"), "applied");
    assert_eq!(get(get(&body, "identity"), "local_user_aliases"), &obj(r#"["Legacy"]"#));
}

#[tokio::test]
async fn invalid_input_when_put_then_422_and_the_saved_profile_unchanged() {
    let long = format!(r#"{{"name": "{}"}}"#, "a".repeat(81));
    let many = format!(r#"{{"name": "Alex", "aliases": [{}]}}"#, vec!["\"a\""; 17].join(", "));
    for payload in [
        "{}",
        r#"{"name": ""}"#,
        r#"{"name": "Me"}"#,
        r#"{"name": "Alex\n"}"#,
        &long,
        r#"{"name": null}"#,
        r#"{"name": 123}"#,
        r#"{"name": "Alex", "aliases": "Al"}"#,
        r#"{"name": "Alex", "aliases": [null]}"#,
        &many,
    ] {
        let app = app();
        let original = store(&app).write("Original", &[]).unwrap();
        let (client, mut rx) = recog_web::util::Client::new();
        app.state.live_watcher.register(client);

        let (status, _) = app.json("PUT", "/api/user-profile", Some(payload)).await;

        assert_eq!(status, 422, "{payload}");
        assert_eq!(store(&app).read().unwrap(), original);
        assert!(rx.try_recv().is_err(), "{payload} was broadcast");
    }
}

#[tokio::test]
async fn unwritable_data_directory_when_put_then_500_without_private_detail_or_broadcast() {
    let app = app();
    let original = store(&app).write("Original", &[]).unwrap();
    let (client, mut rx) = recog_web::util::Client::new();
    app.state.live_watcher.register(client);
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(app.data(), std::fs::Permissions::from_mode(0o500)).unwrap();

    let (status, body) = app.call("PUT", "/api/user-profile", Some(r#"{"name": "Alex"}"#)).await;

    std::fs::set_permissions(app.data(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(status, 500);
    assert!(!String::from_utf8_lossy(&body).contains(&app.data().to_string_lossy().into_owned()));
    assert_eq!(store(&app).read().unwrap(), original);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn corrupt_file_when_get_then_a_visible_error_without_mutation() {
    let app = app();
    support::write(&store(&app).path, "broken");

    let (status, _) = app.call("GET", "/api/user-profile", None).await;

    assert_eq!(status, 500);
    assert_eq!(std::fs::read(store(&app).path).unwrap(), b"broken");
}

#[tokio::test]
async fn malformed_ack_when_received_then_it_cannot_mark_the_profile_applied() {
    for (field, value) in [
        ("local_user_name", "null"),
        ("local_user_aliases", r#""Alex""#),
        ("local_user_aliases", "[123]"),
        ("local_user_identity_known", "1"),
        ("local_user_profile_revision", r#""stale""#),
    ] {
        let app = app();
        let coordinator = &app.state.profile;
        let profile = store(&app).write("Alex", &[]).unwrap();
        connect(coordinator, &profile.revision).await;
        let mut frame = applied(&profile, None);
        if let Some(Value::Object(identity)) = frame.get_mut("identity") {
            identity.insert(field, obj(value));
        }

        coordinator.handle_frame(&frame).await.unwrap();

        assert_eq!(text(&coordinator.snapshot().await.unwrap(), "application_state"), "pending", "{field}");
    }
}

#[tokio::test]
async fn bridge_apply_error_while_a_save_waits_then_pending_with_an_error_and_the_profile_kept() {
    let app = app();
    let coordinator = &app.state.profile;
    let (_bridge, mut rx) = connect(coordinator, "unconfigured").await;

    let ((status, body), ()) = tokio::join!(app.json("PUT", "/api/user-profile", Some(r#"{"name": "Alex"}"#)), async {
        next(&mut rx).await;
        let frame = obj(r#"{"type": "user_profile_error", "message": "private runtime details"}"#);
        coordinator.handle_frame(frame.as_object().unwrap()).await.unwrap();
    });

    assert_eq!(status, 200);
    assert_eq!(get(&body, "profile"), &store(&app).read().unwrap().to_json());
    assert_eq!(text(&body, "application_state"), "pending");
    assert!(!text(&body, "error").is_empty());
    assert!(!recog_core::pyjson::dumps(&body, None).contains("private runtime"));
}

#[tokio::test]
async fn browser_toggle_when_saved_then_the_channel_observes_the_same_setting() {
    let app = app();
    let (client, mut rx) = recog_web::util::Client::new();
    app.state.live_watcher.register(client);
    let mut privacy = CopilotPrivacy::new(PrivacyStore::new(&app.data()));
    let event =
        || obj(r#"{"meeting_id": "m1", "local_user_name": "Alice", "segments": []}"#).as_object().unwrap().clone();

    assert_eq!(get(&app.json("GET", "/api/privacy", None).await.1, "configured"), &Value::Bool(false));
    assert_eq!(app.call("PUT", "/api/privacy", Some(r#"{"enabled": "false"}"#)).await.0, 422);
    assert!(!PrivacyStore::new(&app.data()).path.exists());
    assert_eq!(app.call("PUT", "/api/privacy", Some(r#"{"enabled": true}"#)).await.0, 200);
    assert_eq!(text(&Value::Object(privacy.apply(&event()).unwrap()), "local_user_name"), "speaker_0");
    assert_eq!(get(&app.json("GET", "/api/privacy", None).await.1, "enabled"), &Value::Bool(true));
    let mut last = None;
    while let Ok(frame) = rx.try_recv() {
        last = Some(obj(&frame));
    }
    assert_eq!(last, Some(obj(r#"{"type": "privacy_updated", "enabled": true, "configured": true}"#)));
    app.call("PUT", "/api/privacy", Some(r#"{"enabled": false}"#)).await;
    assert_eq!(text(&Value::Object(privacy.apply(&event()).unwrap()), "local_user_name"), "Alice");
}
