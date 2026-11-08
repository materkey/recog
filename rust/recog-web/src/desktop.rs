//! `GET /api/desktop/status`: `status()` of `recog/desktop/service.py`, which only
//! reads the files the desktop integration keeps under `<data dir>/desktop/`.
//!
//! The integration itself (the observer started from the web server's lifespan and
//! its inspector worker) is not ported: this server never writes `heartbeat.json`, so
//! an installation that enabled the integration sees `server_offline` here, as it
//! would with the Python observer stopped.

use std::path::Path;

use recog_core::pyjson::{self, Map, Value};

use crate::json::object;

/// Why a file could not be used: Python's `(OSError, ValueError, TypeError,
/// AttributeError)`.
struct Invalid;

/// `read_json(path, default)`: `{}` (or the default) for a missing file.
fn read_json(path: &Path, default: Option<Value>) -> Result<Value, Invalid> {
    match std::fs::read(path) {
        Ok(raw) => pyjson::loads_bytes(&raw).map_err(|_| Invalid),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default.unwrap_or(Value::Object(Map::new()))),
        Err(_) => Err(Invalid),
    }
}

/// `value.get(key)` of a dict; anything else is an `AttributeError`.
fn get<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, Invalid> {
    value.as_object().map(|m| m.get(key)).ok_or(Invalid)
}

fn spread(base: Value, extra: &Value) -> Result<Value, Invalid> {
    let Value::Object(mut map) = base else { return Err(Invalid) };
    for (k, v) in extra.as_object().ok_or(Invalid)?.iter() {
        map.insert(k.clone(), v.clone());
    }
    Ok(Value::Object(map))
}

fn number(value: Option<&Value>, default: f64) -> Result<f64, Invalid> {
    match value {
        None => Ok(default),
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(x)) => Ok(*x),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(_) => Err(Invalid),
    }
}

fn try_status(root: &Path, now: f64) -> Result<Value, Invalid> {
    let existing = read_json(&root.join("status.json"), None)?;
    if get(&existing, "state")?.and_then(Value::as_str) == Some("inspector_unclosed") {
        let enabled = match read_json(&root.join("config.json"), None) {
            Ok(config) => match config.as_object() {
                Some(map) => map.get("enabled").is_some_and(Value::truthy),
                None => true,
            },
            Err(Invalid) => true,
        };
        let app_version = get(&existing, "app_version")?.cloned().unwrap_or(Value::Null);
        return spread(
            existing.clone(),
            &object([
                ("enabled", Value::Bool(enabled)),
                ("app_version", app_version),
                ("inspector_closed", Value::Bool(false)),
            ]),
        );
    }
    let config = read_json(&root.join("config.json"), None)?;
    if !get(&config, "enabled")?.is_some_and(Value::truthy) {
        return Ok(object([
            ("enabled", Value::Bool(false)),
            ("state", Value::from("disabled")),
            ("reason", Value::Null),
            ("app_version", Value::Null),
            ("inspector_closed", Value::Null),
        ]));
    }
    let mut result = read_json(&root.join("status.json"), Some(object([("state", Value::from("waiting"))])))?;
    let heartbeat = read_json(&root.join("heartbeat.json"), None)?;
    let running = get(&heartbeat, "running")?.is_some_and(Value::truthy);
    if (!running || now - number(get(&heartbeat, "updated_at")?, 0.0)? > 8.0)
        && get(&result, "state")?.and_then(Value::as_str) != Some("inspector_unclosed")
    {
        result = spread(
            result,
            &object([("state", Value::from("server_offline")), ("reason", Value::from("web_server_not_running"))]),
        )?;
    }
    spread(
        object([
            ("enabled", Value::Bool(true)),
            ("app_version", Value::Null),
            ("reason", Value::Null),
            ("inspector_closed", Value::Null),
        ]),
        &result,
    )
}

/// `status(root)`.
#[must_use]
pub fn status(root: &Path) -> Value {
    try_status(root, crate::util::unix_time()).unwrap_or_else(|Invalid| {
        object([
            ("enabled", Value::Bool(true)),
            ("state", Value::from("error")),
            ("reason", Value::from("invalid_configuration")),
            ("app_version", Value::Null),
            ("inspector_closed", Value::Null),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_integration_when_status_read_then_disabled_and_nothing_created() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("desktop");
        // when
        let value = status(&root);
        // then
        assert_eq!(
            pyjson::dumps(&value, None),
            r#"{"enabled": false, "state": "disabled", "reason": null, "app_version": null, "inspector_closed": null}"#
        );
        assert!(!root.exists());
    }

    #[test]
    fn enabled_integration_without_heartbeat_when_status_read_then_server_offline() {
        // given
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"enabled": true}"#).unwrap();
        // when
        let value = status(dir.path());
        // then
        assert_eq!(
            pyjson::dumps(&value, None),
            r#"{"enabled": true, "app_version": null, "reason": "web_server_not_running", "inspector_closed": null, "state": "server_offline"}"#
        );
    }

    #[test]
    fn broken_configuration_when_status_read_then_invalid_configuration() {
        // given
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{broken").unwrap();
        // when
        let value = status(dir.path());
        // then
        assert_eq!(value.as_object().unwrap().get("reason"), Some(&Value::from("invalid_configuration")));
    }
}
