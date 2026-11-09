//! AI settings persistence and the acknowledgement of the live Copilot policy — the
//! port of `recog/web/ai_settings.py`.
//!
//! The same protocol as the profile ([`crate::user_profile`]): a save is announced as
//! `ai_settings_refresh`, an `ai_settings_applied` of exactly the saved revision on the
//! current connection marks it applied, and `ai_settings_status` tells the web UI.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use recog_ai::AiError;
use recog_ai::json::Json;
use recog_ai::settings::AiSettingsStore;
use recog_core::protocol;
use recog_core::pyjson::{Map, Value};
use tokio::sync::Mutex as AsyncMutex;

use crate::bridge_subscriber::RawSender;
use crate::json;
use crate::user_profile::Broadcast;
use crate::util::Event;

/// What the coordinator needs from `AiSettingsStore`; tests wrap it. Results are the
/// dicts Python returns.
pub trait AiStore: Send + Sync + 'static {
    /// # Errors
    /// Unreadable or malformed settings.
    fn read(&self) -> Result<Value, AiError>;
    /// # Errors
    /// As [`AiStore::read`].
    fn view(&self) -> Result<Value, AiError>;
    /// # Errors
    /// Invalid models, a stale revision, a failed write.
    fn write_models(&self, models: &Value, expected_revision: Option<&str>) -> Result<Value, AiError>;
    /// # Errors
    /// An unknown scenario, invalid values, a stale revision, a failed write.
    fn write_task(&self, task_id: &str, values: &Value, expected_revision: Option<&str>) -> Result<Value, AiError>;
    /// # Errors
    /// An unknown scenario, a stale revision, a failed write.
    fn reset_task(&self, task_id: &str, expected_revision: Option<&str>) -> Result<Value, AiError>;
}

impl AiStore for AiSettingsStore {
    fn read(&self) -> Result<Value, AiError> {
        Ok(json::to_core(&Json::Object(AiSettingsStore::read(self)?)))
    }
    fn view(&self) -> Result<Value, AiError> {
        Ok(json::to_core(&AiSettingsStore::view(self)?.to_json()))
    }
    fn write_models(&self, models: &Value, expected_revision: Option<&str>) -> Result<Value, AiError> {
        Ok(json::to_core(&AiSettingsStore::write_models(self, &json::to_ai(models), expected_revision)?.to_json()))
    }
    fn write_task(&self, task_id: &str, values: &Value, expected_revision: Option<&str>) -> Result<Value, AiError> {
        Ok(json::to_core(
            &AiSettingsStore::write_task(self, task_id, &json::to_ai(values), expected_revision)?.to_json(),
        ))
    }
    fn reset_task(&self, task_id: &str, expected_revision: Option<&str>) -> Result<Value, AiError> {
        Ok(json::to_core(&AiSettingsStore::reset_task(self, task_id, expected_revision)?.to_json()))
    }
}

#[derive(Default)]
struct State {
    send: Option<Arc<dyn RawSender>>,
    applied_revision: Option<Value>,
    connection_generation: u64,
    error: Option<String>,
}

pub struct AiSettingsCoordinator {
    pub store: Arc<dyn AiStore>,
    broadcast: Broadcast,
    state: Mutex<State>,
    write_lock: AsyncMutex<()>,
    changed: Event,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, AiError> + Send + 'static) -> Result<T, AiError> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(AiError::Runtime(e.to_string())))
}

fn revision(value: &Value) -> Value {
    value.as_object().and_then(|m| m.get("revision")).cloned().unwrap_or(Value::Null)
}

impl AiSettingsCoordinator {
    #[must_use]
    pub fn new(store: Arc<dyn AiStore>, broadcast: Broadcast) -> Self {
        Self {
            store,
            broadcast,
            state: Mutex::new(State::default()),
            write_lock: AsyncMutex::new(()),
            changed: Event::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn read(&self) -> Result<Value, AiError> {
        let store = Arc::clone(&self.store);
        blocking(move || store.read()).await
    }

    /// `view()`: the settings page with the Copilot application state.
    ///
    /// # Errors
    /// Unreadable or malformed settings.
    pub async fn view(&self) -> Result<Value, AiError> {
        let store = Arc::clone(&self.store);
        let result = blocking(move || store.view()).await?;
        Ok(self.with_application(result))
    }

    fn with_application(&self, result: Value) -> Value {
        let Value::Object(mut map) = result else { return result };
        let state = self.lock();
        let applied = state.send.is_some() && state.applied_revision.as_ref() == map.get("revision");
        let mut application = Map::new();
        application.insert("state", Value::from(if applied { "applied" } else { "pending" }));
        application.insert("model_application", Value::from("next_session"));
        if let Some(error) = &state.error {
            application.insert("error", Value::from(error.as_str()));
        }
        map.insert("copilot_application", Value::Object(application));
        Value::Object(map)
    }

    async fn publish(&self) -> Result<(), AiError> {
        let result = self.view().await?;
        let map = result.as_object().cloned().unwrap_or_default();
        let frame = json::object([
            ("type", Value::from("ai_settings_status")),
            ("revision", map.get("revision").cloned().unwrap_or(Value::Null)),
            ("copilot_application", map.get("copilot_application").cloned().unwrap_or(Value::Null)),
        ]);
        (self.broadcast)(json::ascii(&frame));
        Ok(())
    }

    /// The bridge link came up.
    ///
    /// # Errors
    /// Unreadable or malformed settings.
    pub async fn connected(&self, send: Arc<dyn RawSender>) -> Result<(), AiError> {
        {
            let mut state = self.lock();
            state.connection_generation += 1;
            state.send = Some(send);
            state.applied_revision = None;
            state.error = None;
        }
        let snapshot = self.read().await?;
        self.refresh(&revision(&snapshot)).await;
        self.publish().await
    }

    /// The bridge link went down.
    ///
    /// # Errors
    /// Unreadable or malformed settings.
    pub async fn disconnected(&self) -> Result<(), AiError> {
        {
            let mut state = self.lock();
            state.connection_generation += 1;
            state.send = None;
            state.applied_revision = None;
        }
        self.changed.set();
        self.publish().await
    }

    async fn refresh(&self, revision: &Value) -> bool {
        self.changed.clear();
        let Some(send) = self.lock().send.clone() else { return false };
        let frame = json::ascii(&json::object([
            ("type", Value::from(protocol::AI_SETTINGS_REFRESH)),
            ("revision", revision.clone()),
        ]));
        let sent = send.send(frame).await;
        if !sent {
            self.lock().applied_revision = None;
        }
        sent
    }

    /// `write(operation, ..., expected_revision)`: persist, announce, wait up to a
    /// second for the acknowledgement; the result keeps the revision of this write.
    ///
    /// # Errors
    /// What the operation raises, or the settings cannot be read back.
    pub async fn write(
        &self,
        operation: impl FnOnce(&dyn AiStore) -> Result<Value, AiError> + Send + 'static,
    ) -> Result<Value, AiError> {
        let result = {
            let _guard = self.write_lock.lock().await;
            let store = Arc::clone(&self.store);
            let result = blocking(move || operation(store.as_ref())).await?;
            let mut state = self.lock();
            state.applied_revision = None;
            state.error = None;
            result
        };
        let sent = self.refresh(&revision(&result)).await;
        self.publish().await?;
        if sent {
            self.changed.wait_timeout(Duration::from_secs(1)).await;
        }
        Ok(self.with_application(result))
    }

    /// An `ai_settings_applied` or `ai_settings_error` frame from the bridge.
    ///
    /// # Errors
    /// Unreadable or malformed settings.
    pub async fn handle_frame(&self, frame: &Map) -> Result<(), AiError> {
        if frame.get("type").and_then(Value::as_str) == Some(protocol::AI_SETTINGS_APPLIED) {
            let generation = self.lock().connection_generation;
            let _guard = self.write_lock.lock().await;
            let snapshot = self.read().await?;
            let current = revision(&snapshot);
            {
                let mut state = self.lock();
                if generation != state.connection_generation
                    || state.send.is_none()
                    || frame.get("revision").cloned().unwrap_or(Value::Null) != current
                {
                    return Ok(());
                }
                state.applied_revision = Some(current);
                state.error = None;
            }
            self.changed.set();
            drop(_guard);
        } else {
            {
                let mut state = self.lock();
                state.applied_revision = None;
                state.error = Some("Настройки сохранены; Copilot пока не получил новые инструкции.".to_owned());
            }
            self.changed.set();
        }
        self.publish().await
    }
}
