//! The Copilot scenario of `ai-settings.json`: its switch, model and Claude effort.
//!
//! A reader of `recog/ai_settings.py` for the one task the supervisor needs,
//! `resolve_task("copilot")`, with the same validation of the whole file: Python
//! treats a file it refuses as "Copilot off" (`_read_enabled` fails closed), so a laxer
//! reader would keep a session the Python supervisor stops. Writing stays in Python
//! (the web UI) until `recog-ai` owns this store.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::py;

pub const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_CODEX_MODEL: &str = "gpt-6-astra";
const PROVIDERS: [&str; 2] = ["claude", "codex"];
const REASONING_EFFORTS: [&str; 6] = ["", "minimal", "low", "medium", "high", "xhigh"];
const CLAUDE_EFFORTS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];
const PLACEHOLDERS: [&str; 4] = ["transcript", "user_name", "participants", "meeting_title"];
const DEFAULT_TASKS: [&str; 4] = ["analysis.increment", "title", "meeting_chat", "copilot"];
const TASK_FIELDS: [&str; 6] = ["prompt", "providers", "models", "reasoning_effort", "claude_effort", "enabled"];
const MAX_SETTINGS_BYTES: u64 = 256 * 1024;
const MAX_PROMPT_CHARS: usize = 32000;
const MAX_TASKS: usize = 256;

/// What a Copilot session is started with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotTask {
    pub enabled: bool,
    pub model: String,
    pub claude_effort: String,
}

/// Where the supervisor learns the Copilot task, read afresh on every call as Python
/// reads the file on every check.
pub trait CopilotSettings {
    fn resolve(&self) -> Result<CopilotTask, String>;
}

/// The legacy environment `resolve_task` consults (`CLAUDE_MODEL`, `CODEX_MODEL`,
/// `CODEX_REASONING_EFFORT`), captured once: the process environment does not change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyEnv {
    pub claude_model: String,
    pub codex_model: String,
    pub codex_reasoning_effort: String,
}

/// `<data dir>/ai-settings.json`.
#[derive(Clone, Debug)]
pub struct AiSettingsFile {
    pub path: PathBuf,
    pub legacy: LegacyEnv,
}

impl AiSettingsFile {
    pub fn new(data_dir: impl Into<PathBuf>, legacy: LegacyEnv) -> Self {
        AiSettingsFile { path: data_dir.into().join("ai-settings.json"), legacy }
    }

    /// `AiSettingsStore.read()`: the validated snapshot, or the defaults without a file.
    fn read(&self) -> Result<Value, String> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Ok(json!({"version": 1, "revision": "defaults", "models": {}, "tasks": {}}));
            }
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        let mut data = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        if data.len() as u64 > MAX_SETTINGS_BYTES {
            return Err("AI settings file exceeds size limit".to_owned());
        }
        // json.loads of bytes takes a UTF-8 BOM.
        let text = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data);
        let mut raw: Value = serde_json::from_slice(text).map_err(|_| "Invalid AI settings JSON".to_owned())?;
        drop_retired(&mut raw);
        validate_raw(&raw)?;
        Ok(raw)
    }

    /// `_effective_models`: the defaults, the legacy variables for providers the file
    /// leaves unset, the file's global models.
    fn effective_models(&self, snapshot: &Value) -> Result<Map<String, Value>, String> {
        let mut models = Map::new();
        models.insert("claude".to_owned(), json!(DEFAULT_CLAUDE_MODEL));
        models.insert("codex".to_owned(), json!(DEFAULT_CODEX_MODEL));
        let saved = snapshot["models"].as_object().cloned().unwrap_or_default();
        for (provider, legacy) in [("claude", &self.legacy.claude_model), ("codex", &self.legacy.codex_model)] {
            if saved.contains_key(provider) || py::strip(legacy).is_empty() {
                continue;
            }
            let mut one = Map::new();
            one.insert(provider.to_owned(), json!(legacy));
            models.extend(normalize_models(&Value::Object(one))?);
        }
        models.extend(saved);
        Ok(models)
    }
}

impl CopilotSettings for AiSettingsFile {
    /// `resolve_task("copilot")`, reduced to what the supervisor uses.
    fn resolve(&self) -> Result<CopilotTask, String> {
        let snapshot = self.read()?;
        let task = snapshot["tasks"].get("copilot").cloned().unwrap_or_else(|| json!({}));
        let effort = if self.legacy.codex_reasoning_effort.is_empty() {
            "xhigh"
        } else {
            self.legacy.codex_reasoning_effort.as_str()
        };
        if !REASONING_EFFORTS.contains(&effort) {
            return Err("Invalid legacy CODEX_REASONING_EFFORT".to_owned());
        }
        let mut models = self.effective_models(&snapshot)?;
        if let Some(own) = task.get("models").and_then(Value::as_object) {
            models.extend(own.clone());
        }
        Ok(CopilotTask {
            enabled: task.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            model: models.get("claude").and_then(Value::as_str).unwrap_or(DEFAULT_CLAUDE_MODEL).to_owned(),
            claude_effort: task.get("claude_effort").and_then(Value::as_str).unwrap_or("").to_owned(),
        })
    }
}

/// Settings saved by an earlier version may name retired features; `read()` ignores
/// them without rewriting the file.
fn drop_retired(raw: &mut Value) {
    let Some(raw) = raw.as_object_mut() else { return };
    if let Some(models) = raw.get_mut("models").and_then(Value::as_object_mut) {
        models.shift_remove("openrouter");
    }
    let Some(tasks) = raw.get_mut("tasks").and_then(Value::as_object_mut) else { return };
    for retired in ["anchor", "analysis.custom", "analysis.summary"] {
        tasks.shift_remove(retired);
    }
    for values in tasks.values_mut() {
        let Some(values) = values.as_object_mut() else { continue };
        if let Some(models) = values.get_mut("models").and_then(Value::as_object_mut)
            && models.contains_key("openrouter")
        {
            models.shift_remove("openrouter");
            if models.is_empty() {
                values.shift_remove("models");
            }
        }
        if let Some(providers) = values.get_mut("providers").and_then(Value::as_array_mut)
            && providers.iter().any(|p| p == "openrouter")
        {
            providers.retain(|p| p != "openrouter");
            if providers.is_empty() {
                values.shift_remove("providers");
            }
        }
    }
}

fn known_fields<'a>(value: &'a Value, allowed: &[&str], label: &str) -> Result<&'a Map<String, Value>, String> {
    let map = value.as_object().ok_or_else(|| format!("{label} must be an object"))?;
    if map.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("Unknown {label} fields"));
    }
    Ok(map)
}

/// `_validate_raw`.
fn validate_raw(raw: &Value) -> Result<(), String> {
    let map = known_fields(raw, &["version", "revision", "models", "tasks"], "settings")?;
    if map.len() != 4 {
        return Err("Incomplete AI settings".to_owned());
    }
    if !(map["version"].is_i64() || map["version"].is_u64()) || map["version"].as_i64() != Some(1) {
        return Err("Unsupported AI settings version".to_owned());
    }
    if !map["revision"].as_str().is_some_and(is_canonical_uuid) {
        return Err("Invalid settings revision".to_owned());
    }
    if Value::Object(normalize_models(&map["models"])?) != map["models"] {
        return Err("Noncanonical model overrides".to_owned());
    }
    let tasks = map["tasks"].as_object().filter(|t| t.len() <= MAX_TASKS).ok_or("Invalid task overrides")?;
    for (task_id, values) in tasks {
        if Value::Object(task_values(task_id, values)?) != *values {
            return Err("Noncanonical task override".to_owned());
        }
    }
    Ok(())
}

/// `str(uuid.UUID(text)) == text`: lowercase hex in the 8-4-4-4-12 form.
fn is_canonical_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

/// `_models`: provider -> model, blank entries dropped, the rest checked.
fn normalize_models(value: &Value) -> Result<Map<String, Value>, String> {
    let map = known_fields(value, &PROVIDERS, "models")?;
    let mut normalized = Map::new();
    for (provider, model) in map {
        let model = model.as_str().ok_or("Model must be text")?;
        if py::strip(model).is_empty() {
            continue;
        }
        if model.chars().count() > 160 || model.chars().any(|c| py::is_space(c) || py::is_other_category(c)) {
            return Err("Model must be at most 160 characters without whitespace or control characters".to_owned());
        }
        if model.starts_with('-') {
            return Err("Model must not start with a dash".to_owned());
        }
        normalized.insert(provider.clone(), json!(model));
    }
    Ok(normalized)
}

/// `_valid_task_id`: a built-in scenario or `saved:` and a prompt id (`[\w][\w.-]{0,159}`).
fn valid_task_id(task_id: &str) -> bool {
    if DEFAULT_TASKS.contains(&task_id) {
        return true;
    }
    let Some(rest) = task_id.strip_prefix("saved:") else { return false };
    let mut chars = rest.chars();
    chars.next().is_some_and(py::is_word)
        && rest.chars().count() <= 160
        && chars.all(|c| py::is_word(c) || c == '.' || c == '-')
}

/// `_task_values`: the canonical form of one task override, or why it is refused.
fn task_values(task_id: &str, values: &Value) -> Result<Map<String, Value>, String> {
    if !valid_task_id(task_id) {
        return Err("Unknown AI task".to_owned());
    }
    let values = known_fields(values, &TASK_FIELDS, "task")?;
    let copilot = task_id == "copilot";
    let mut normalized = Map::new();
    if let Some(enabled) = values.get("enabled") {
        if !copilot || !enabled.is_boolean() {
            return Err("Enabled must be a boolean for Copilot".to_owned());
        }
        normalized.insert("enabled".to_owned(), enabled.clone());
    }
    if let Some(prompt) = values.get("prompt") {
        let text = prompt
            .as_str()
            .filter(|p| !py::strip(p).is_empty() && p.chars().count() <= MAX_PROMPT_CHARS && !p.contains('\0'));
        let text =
            text.ok_or_else(|| format!("Prompt must be nonempty text of at most {MAX_PROMPT_CHARS} characters"))?;
        if !copilot && !template_variables(text)?.iter().any(|name| name == "transcript") {
            return Err("Prompt must include {transcript}".to_owned());
        }
        normalized.insert("prompt".to_owned(), prompt.clone());
    }
    if let Some(providers) = values.get("providers") {
        let allowed: &[&str] = if copilot { &["claude"] } else { &PROVIDERS };
        let limit = if task_id.starts_with("analysis.") { allowed.len() } else { 1 };
        let list = providers.as_array().filter(|list| (1..=limit).contains(&list.len()));
        let names: Option<Vec<&str>> =
            list.and_then(|list| list.iter().map(|p| p.as_str().filter(|p| allowed.contains(p))).collect());
        let unique = names.as_ref().is_some_and(|n| n.iter().enumerate().all(|(i, p)| !n[..i].contains(p)));
        if !unique {
            return Err("Invalid provider selection".to_owned());
        }
        normalized.insert("providers".to_owned(), providers.clone());
    }
    if let Some(models) = values.get("models") {
        let models = normalize_models(models)?;
        if copilot && models.keys().any(|k| k != "claude") {
            return Err("Copilot supports only Claude".to_owned());
        }
        if !models.is_empty() {
            normalized.insert("models".to_owned(), Value::Object(models));
        }
    }
    if let Some(effort) = values.get("reasoning_effort") {
        if !effort.as_str().is_some_and(|e| REASONING_EFFORTS.contains(&e)) {
            return Err("Invalid Codex reasoning effort".to_owned());
        }
        if copilot {
            return Err("Copilot does not support Codex reasoning effort".to_owned());
        }
        normalized.insert("reasoning_effort".to_owned(), effort.clone());
    }
    if let Some(effort) = values.get("claude_effort") {
        if !effort.as_str().is_some_and(|e| CLAUDE_EFFORTS.contains(&e)) {
            return Err("Invalid Claude effort".to_owned());
        }
        normalized.insert("claude_effort".to_owned(), effort.clone());
    }
    Ok(normalized)
}

/// `_template_variables`: the placeholder names a prompt uses, refusing unknown ones.
fn template_variables(template: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = template.chars().collect();
    let mut found = Vec::new();
    let mut opens: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' && chars.get(i + 1) == Some(&'{') {
            opens.push(true);
            i += 2;
        } else if c == '{' {
            match placeholder_at(&chars, i) {
                Some((name, end)) => {
                    found.push(name);
                    i = end;
                }
                None => {
                    opens.push(false);
                    i += 1;
                }
            }
        } else if c == '}' {
            let escaped = opens.pop().unwrap_or(true);
            i += if escaped && chars.get(i + 1) == Some(&'}') { 2 } else { 1 };
        } else {
            i += 1;
        }
    }
    let mut unknown: Vec<&String> = found.iter().filter(|name| !PLACEHOLDERS.contains(&name.as_str())).collect();
    if !unknown.is_empty() {
        unknown.sort();
        unknown.dedup();
        let names: Vec<&str> = unknown.iter().map(|n| n.as_str()).collect();
        return Err(format!("Unknown prompt placeholders: {}", names.join(", ")));
    }
    Ok(found)
}

/// `_PLACEHOLDER_TOKEN.match(template, start)`:
/// `{([A-Za-z_][A-Za-z0-9_]*(?:[.!:\[][^{\n}]*)?)}`, returning group 1 and the end.
fn placeholder_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start + 1;
    if !chars.get(i).is_some_and(|c| c.is_ascii_alphabetic() || *c == '_') {
        return None;
    }
    i += 1;
    while chars.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') {
        i += 1;
    }
    if chars.get(i).is_some_and(|c| ".!:[".contains(*c)) {
        i += 1;
        while chars.get(i).is_some_and(|c| !matches!(c, '{' | '\n' | '}')) {
            i += 1;
        }
    }
    (chars.get(i) == Some(&'}')).then(|| (chars[start + 1..i].iter().collect(), i + 1))
}
