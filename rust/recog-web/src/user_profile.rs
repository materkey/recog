//! Profile edits and their acknowledgement by the live process — the port of
//! `recog/web/user_profile.py`.
//!
//! A save is persisted first, then announced to the bridge (`user_profile_refresh`);
//! the profile counts as applied only once an acknowledgement of exactly the saved
//! revision arrives on the current bridge connection. A reconnect starts over: the
//! saved revision is replayed and a fresh acknowledgement is required. Every change of
//! state is broadcast to the web UI as `user_profile_status`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use recog_core::protocol;
use recog_core::pyjson::{Map, Value};
use recog_core::user_profile::{Profile, ProfileError, UserProfileStore, resolve_identity};
use tokio::sync::Mutex as AsyncMutex;

use crate::bridge_subscriber::RawSender;
use crate::json;
use crate::util::Event;

/// What the coordinator needs from `UserProfileStore`; tests wrap it.
pub trait ProfileStore: Send + Sync + 'static {
    /// # Errors
    /// A broken or unreadable file.
    fn read(&self) -> Result<Profile, ProfileError>;
    /// # Errors
    /// An invalid name or alias, or a failed write.
    fn write(&self, name: &str, aliases: &[String]) -> Result<Profile, ProfileError>;
    /// # Errors
    /// A broken file or a failed write.
    fn clear(&self) -> Result<Profile, ProfileError>;
}

impl ProfileStore for UserProfileStore {
    fn read(&self) -> Result<Profile, ProfileError> {
        UserProfileStore::read(self)
    }
    fn write(&self, name: &str, aliases: &[String]) -> Result<Profile, ProfileError> {
        UserProfileStore::write(self, name, aliases)
    }
    fn clear(&self) -> Result<Profile, ProfileError> {
        UserProfileStore::clear(self)
    }
}

/// Where status frames go (the live-stream clients).
pub type Broadcast = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Default)]
struct State {
    send: Option<Arc<dyn RawSender>>,
    applied_identity: Option<Value>,
    error: Option<String>,
    connection_generation: u64,
}

pub struct ProfileCoordinator {
    store: Arc<dyn ProfileStore>,
    legacy_name: String,
    broadcast: Broadcast,
    state: Mutex<State>,
    changed: Event,
    write_lock: AsyncMutex<()>,
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ProfileError> + Send + 'static,
) -> Result<T, ProfileError> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(ProfileError::Io(e.to_string())))
}

fn is_str_list(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Array(items)) if items.iter().all(|i| matches!(i, Value::Str(_))))
}

impl ProfileCoordinator {
    #[must_use]
    pub fn new(store: Arc<dyn ProfileStore>, legacy_name: &str, broadcast: Broadcast) -> Self {
        Self {
            store,
            legacy_name: legacy_name.to_owned(),
            broadcast,
            state: Mutex::new(State::default()),
            changed: Event::new(),
            write_lock: AsyncMutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn read(&self) -> Result<Profile, ProfileError> {
        let store = Arc::clone(&self.store);
        blocking(move || store.read()).await
    }

    /// `snapshot()`: the saved profile, the identity in effect and whether the live
    /// process applied it.
    ///
    /// # Errors
    /// The profile cannot be read.
    pub async fn snapshot(&self) -> Result<Value, ProfileError> {
        let profile = self.read().await?;
        let (applied, identity, error) = {
            let state = self.lock();
            let applied = state.send.is_some()
                && state.applied_identity.as_ref().is_some_and(|identity| {
                    identity.as_object().and_then(|m| m.get("local_user_profile_revision"))
                        == Some(&Value::from(profile.revision.as_str()))
                });
            let identity = state
                .applied_identity
                .clone()
                .filter(Value::truthy)
                .unwrap_or_else(|| resolve_identity(&profile, Some(&self.legacy_name), None));
            (applied, identity, state.error.clone())
        };
        let mut suggested = recog_core::py::strip(&self.legacy_name).to_owned();
        if recog_core::py::casefold(&suggested) == "me" {
            suggested.clear();
        }
        let mut result = Map::new();
        result.insert("profile", profile.to_json());
        result.insert("suggested_name", Value::from(suggested));
        result.insert("identity", identity);
        result.insert("application_state", Value::from(if applied { "applied" } else { "pending" }));
        if let Some(error) = error {
            result.insert("error", Value::from(error));
        }
        Ok(Value::Object(result))
    }

    async fn publish(&self) -> Result<(), ProfileError> {
        let snapshot = self.snapshot().await?;
        let mut frame = Map::new();
        frame.insert("type", Value::from("user_profile_status"));
        if let Value::Object(map) = snapshot {
            for (k, v) in map.iter() {
                frame.insert(k.clone(), v.clone());
            }
        }
        (self.broadcast)(json::unicode(&Value::Object(frame)));
        Ok(())
    }

    /// The bridge link came up: forget the acknowledgement, replay the saved revision.
    ///
    /// # Errors
    /// The profile cannot be read.
    pub async fn connected(&self, send: Arc<dyn RawSender>) -> Result<(), ProfileError> {
        {
            let mut state = self.lock();
            state.connection_generation += 1;
            state.send = Some(send);
            state.applied_identity = None;
            state.error = None;
        }
        let profile = self.read().await?;
        self.refresh(&profile).await;
        self.publish().await
    }

    /// The bridge link went down.
    ///
    /// # Errors
    /// The profile cannot be read.
    pub async fn disconnected(&self) -> Result<(), ProfileError> {
        {
            let mut state = self.lock();
            state.connection_generation += 1;
            state.send = None;
            state.applied_identity = None;
        }
        self.changed.set();
        self.publish().await
    }

    async fn refresh(&self, profile: &Profile) -> bool {
        self.changed.clear();
        let Some(send) = self.lock().send.clone() else { return false };
        let frame = json::ascii(&json::object([
            ("type", Value::from(protocol::USER_PROFILE_REFRESH)),
            ("revision", Value::from(profile.revision.as_str())),
        ]));
        let sent = send.send(frame).await;
        if !sent {
            self.lock().applied_identity = None;
        }
        sent
    }

    /// `save(name, aliases)`.
    ///
    /// # Errors
    /// An invalid profile, or one that cannot be written or read back.
    pub async fn save(&self, name: &str, aliases: &[String]) -> Result<Value, ProfileError> {
        let (name, aliases) = (name.to_owned(), aliases.to_vec());
        self.write(move |store| store.write(&name, &aliases)).await
    }

    /// `clear()`.
    ///
    /// # Errors
    /// A broken profile, or one that cannot be written or read back.
    pub async fn clear(&self) -> Result<Value, ProfileError> {
        self.write(|store| store.clear()).await
    }

    async fn write(
        &self,
        operation: impl FnOnce(&dyn ProfileStore) -> Result<Profile, ProfileError> + Send + 'static,
    ) -> Result<Value, ProfileError> {
        let profile = {
            let _guard = self.write_lock.lock().await;
            let store = Arc::clone(&self.store);
            let profile = blocking(move || operation(store.as_ref())).await?;
            let mut state = self.lock();
            state.error = None;
            state.applied_identity = None;
            profile
        };
        // The acknowledgement may come back at once: never hold the lock over I/O.
        let sent = self.refresh(&profile).await;
        self.publish().await?;
        if sent {
            self.changed.wait_timeout(Duration::from_secs(1)).await;
        }
        self.snapshot().await
    }

    /// A `user_profile_applied` or `user_profile_error` frame from the bridge.
    ///
    /// # Errors
    /// The profile cannot be read.
    pub async fn handle_frame(&self, frame: &Map) -> Result<(), ProfileError> {
        match frame.get("type").and_then(Value::as_str) {
            Some(protocol::USER_PROFILE_APPLIED) => {
                let identity = frame.get("identity").cloned();
                let connection = self.lock().connection_generation;
                {
                    let _guard = self.write_lock.lock().await;
                    let profile = self.read().await?;
                    let revision = Value::from(profile.revision.as_str());
                    let mut state = self.lock();
                    let Some(Value::Object(fields)) = identity.as_ref() else { return Ok(()) };
                    if connection != state.connection_generation
                        || state.send.is_none()
                        || frame.get("revision") != Some(&revision)
                        || fields.get("local_user_profile_revision") != Some(&revision)
                        || !matches!(fields.get("local_user_name"), Some(Value::Str(_)))
                        || !is_str_list(fields.get("local_user_aliases"))
                        || !matches!(fields.get("local_user_identity_known"), Some(Value::Bool(_)))
                    {
                        return Ok(());
                    }
                    state.applied_identity = identity;
                    state.error = None;
                    drop(state);
                    self.changed.set();
                }
                self.publish().await
            }
            Some(protocol::USER_PROFILE_ERROR) => {
                {
                    let mut state = self.lock();
                    state.applied_identity = None;
                    state.error = Some("Профиль сохранён, но пока не применён к записи.".to_owned());
                }
                self.changed.set();
                self.publish().await
            }
            _ => Ok(()),
        }
    }
}
