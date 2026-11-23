//! Private, revisioned AI settings shared by the CLI, web and live processes
//! (`recog/ai_settings.py`).
//!
//! `<data dir>/ai-settings.json` holds global model overrides and per-scenario
//! overrides. Every write takes an `flock` on the stable `ai-settings.lock`, checks the
//! caller's revision, validates, and replaces the file atomically (private temporary
//! file, fsync, rename) with a new UUID revision. Reading never changes the file.
//! Model precedence: scenario > saved global > the installation's (`[ai]` of
//! config.toml, `CLAUDE_MODEL`/`CODEX_MODEL` over it) > shipped default.

use std::borrow::Cow;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::defaults::{
    CLAUDE_EFFORTS, DEFAULT_MODELS, DEFAULT_TASKS, PLACEHOLDERS, PROVIDERS, REASONING_EFFORTS, default_task,
};
use crate::error::{AiError, Result};
use crate::install::{LegacyEnv, default_prompts_dir};
use crate::json::{self, Json, Map};
use crate::prompt_registry::PromptRegistry;
use crate::pytext;
use crate::template::template_variables;

pub use crate::template::{Variables, render_prompt, variables};

pub const MAX_SETTINGS_BYTES: usize = 256 * 1024;
pub const MAX_PROMPT_CHARS: usize = 32000;
pub const MAX_TASKS: usize = 256;
pub const INCREMENT_TASK: &str = "analysis.increment";
pub const SAVED_INCREMENT_TASK: &str = "saved:my-increment";
const TASK_FIELDS: [&str; 6] = ["prompt", "providers", "models", "reasoning_effort", "claude_effort", "enabled"];

/// The parsed settings file: `version`, `revision`, `models`, `tasks`, in file order.
pub type Snapshot = Map;

fn value_error(message: impl Into<String>) -> AiError {
    AiError::Value(message.into())
}

fn known_fields<'a>(value: &'a Json, allowed: &[&str], label: &str) -> Result<&'a Map> {
    let Some(map) = value.as_object() else {
        return Err(value_error(format!("{label} must be an object")));
    };
    let mut unknown: Vec<&str> = map.keys().map(String::as_str).filter(|key| !allowed.contains(key)).collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(value_error(format!("Unknown {label} fields: {}", unknown.join(", "))));
    }
    Ok(map)
}

/// `_models`: provider → model, blank values dropped (they mean "inherit").
pub fn validate_models(value: &Json) -> Result<IndexMap<String, String>> {
    let map = known_fields(value, &PROVIDERS, "models")?;
    let mut normalized = IndexMap::new();
    for (provider, model) in map {
        let Some(model) = model.as_str() else {
            return Err(value_error("Model must be text"));
        };
        if pytext::strip(model).is_empty() {
            continue;
        }
        if pytext::char_len(model) > 160 || model.chars().any(|ch| pytext::is_space(ch) || pytext::is_other(ch)) {
            return Err(value_error("Model must be at most 160 characters without whitespace or control characters"));
        }
        if model.starts_with('-') {
            return Err(value_error("Model must not start with a dash"));
        }
        normalized.insert(provider.clone(), model.to_owned());
    }
    Ok(normalized)
}

/// `_SAVED_ID.fullmatch`: `saved:` and 1 to 160 of `\w`, `.`, `-`, starting with `\w`.
fn is_saved_id(task_id: &str) -> bool {
    let Some(rest) = task_id.strip_prefix("saved:") else {
        return false;
    };
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    pytext::is_word(first)
        && pytext::char_len(rest) <= 160
        && chars.all(|ch| pytext::is_word(ch) || ch == '.' || ch == '-')
}

fn valid_task_id(task_id: &str) -> Result<()> {
    if default_task(task_id).is_some() || is_saved_id(task_id) { Ok(()) } else { Err(value_error("Unknown AI task")) }
}

/// `_task_values`: the canonical override of one scenario, fields in a fixed order.
pub fn task_values(task_id: &str, values: &Json) -> Result<Map> {
    valid_task_id(task_id)?;
    let values = known_fields(values, &TASK_FIELDS, "task")?;
    let mut normalized = Map::new();
    for (field, check) in TASK_FIELD_CHECKS {
        if let Some(value) = values.get(field)
            && let Some(value) = check(task_id, value)?
        {
            normalized.insert(field.into(), value);
        }
    }
    Ok(normalized)
}

/// The canonical value of one field of a scenario's override; `None` leaves it out.
type FieldCheck = fn(&str, &Json) -> Result<Option<Json>>;

/// The fields of an override in the order they are checked and written.
const TASK_FIELD_CHECKS: [(&str, FieldCheck); 6] = [
    ("enabled", enabled_value),
    ("prompt", prompt_value),
    ("providers", providers_value),
    ("models", models_value),
    ("reasoning_effort", reasoning_effort_value),
    ("claude_effort", claude_effort_value),
];

fn enabled_value(task_id: &str, enabled: &Json) -> Result<Option<Json>> {
    let Json::Bool(enabled) = enabled else {
        return Err(value_error("Enabled must be a boolean for Copilot"));
    };
    if task_id != "copilot" {
        return Err(value_error("Enabled must be a boolean for Copilot"));
    }
    Ok(Some(Json::Bool(*enabled)))
}

fn prompt_value(task_id: &str, prompt: &Json) -> Result<Option<Json>> {
    let valid = prompt.as_str().filter(|prompt| {
        !pytext::strip(prompt).is_empty() && pytext::char_len(prompt) <= MAX_PROMPT_CHARS && !prompt.contains('\0')
    });
    let Some(prompt) = valid else {
        return Err(value_error(format!("Prompt must be nonempty text of at most {MAX_PROMPT_CHARS} characters")));
    };
    if task_id != "copilot" && !template_variables(prompt)?.contains("transcript") {
        return Err(value_error("Prompt must include {transcript}"));
    }
    Ok(Some(Json::from(prompt)))
}

fn providers_value(task_id: &str, providers: &Json) -> Result<Option<Json>> {
    let allowed: &[&str] = if task_id == "copilot" { &["claude"] } else { &PROVIDERS };
    let names: Option<Vec<&str>> = providers.as_array().and_then(|items| items.iter().map(Json::as_str).collect());
    let valid = names.filter(|names| {
        (1..=allowed.len()).contains(&names.len())
            && names.iter().all(|name| allowed.contains(name))
            && names.iter().enumerate().all(|(index, name)| !names[..index].contains(name))
    });
    let Some(names) = valid else {
        return Err(value_error("Invalid provider selection"));
    };
    Ok(Some(Json::from(names.iter().map(|name| name.to_string()).collect::<Vec<_>>())))
}

fn models_value(task_id: &str, models: &Json) -> Result<Option<Json>> {
    let models = validate_models(models)?;
    if task_id == "copilot" && models.keys().any(|provider| provider != "claude") {
        return Err(value_error("Copilot supports only Claude"));
    }
    Ok((!models.is_empty()).then(|| json::string_map(&models)))
}

fn reasoning_effort_value(task_id: &str, effort: &Json) -> Result<Option<Json>> {
    let Some(effort) = effort.as_str().filter(|effort| REASONING_EFFORTS.contains(effort)) else {
        return Err(value_error("Invalid Codex reasoning effort"));
    };
    if task_id == "copilot" {
        return Err(value_error("Copilot does not support Codex reasoning effort"));
    }
    Ok(Some(Json::from(effort)))
}

fn claude_effort_value(_task_id: &str, effort: &Json) -> Result<Option<Json>> {
    let Some(effort) = effort.as_str().filter(|effort| CLAUDE_EFFORTS.contains(effort)) else {
        return Err(value_error("Invalid Claude effort"));
    };
    Ok(Some(Json::from(effort)))
}

/// A scenario after precedence is applied (`resolve_task`).
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTask {
    pub id: String,
    pub label: String,
    /// Only for Copilot.
    pub enabled: Option<bool>,
    pub prompt: String,
    pub default_prompt: String,
    pub providers: Vec<String>,
    pub allowed_providers: Vec<String>,
    pub multiple: bool,
    pub models: IndexMap<String, String>,
    pub model_overrides: IndexMap<String, String>,
    pub reasoning_effort: String,
    pub claude_effort: String,
    pub placeholders: Vec<String>,
    pub required_placeholders: Vec<String>,
    pub apply_mode: String,
    pub customized: bool,
    pub revision: String,
}

impl ResolvedTask {
    /// The dict `resolve_task` returns, keys in its order.
    pub fn to_json(&self) -> Json {
        let mut map = Map::new();
        map.insert("id".into(), Json::from(self.id.as_str()));
        map.insert("label".into(), Json::from(self.label.as_str()));
        if let Some(enabled) = self.enabled {
            map.insert("enabled".into(), Json::Bool(enabled));
        }
        map.insert("prompt".into(), Json::from(self.prompt.as_str()));
        map.insert("default_prompt".into(), Json::from(self.default_prompt.as_str()));
        map.insert("providers".into(), Json::from(self.providers.clone()));
        map.insert("allowed_providers".into(), Json::from(self.allowed_providers.clone()));
        map.insert("multiple".into(), Json::Bool(self.multiple));
        map.insert("models".into(), json::string_map(&self.models));
        map.insert("model_overrides".into(), json::string_map(&self.model_overrides));
        map.insert("reasoning_effort".into(), Json::from(self.reasoning_effort.as_str()));
        map.insert("claude_effort".into(), Json::from(self.claude_effort.as_str()));
        map.insert("placeholders".into(), Json::from(self.placeholders.clone()));
        map.insert("required_placeholders".into(), Json::from(self.required_placeholders.clone()));
        map.insert("apply_mode".into(), Json::from(self.apply_mode.as_str()));
        map.insert("customized".into(), Json::Bool(self.customized));
        map.insert("revision".into(), Json::from(self.revision.as_str()));
        Json::Object(map)
    }

    /// The selected (first) provider.
    pub fn provider(&self) -> &str {
        self.providers.first().map_or("", String::as_str)
    }
}

/// The settings page: effective global models and every scenario.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub revision: String,
    pub models: IndexMap<String, String>,
    pub model_overrides: IndexMap<String, String>,
    pub tasks: Vec<ResolvedTask>,
}

impl View {
    pub fn to_json(&self) -> Json {
        json::object([
            ("revision", Json::from(self.revision.as_str())),
            ("models", json::string_map(&self.models)),
            ("model_overrides", json::string_map(&self.model_overrides)),
            ("tasks", Json::Array(self.tasks.iter().map(ResolvedTask::to_json).collect())),
        ])
    }
}

/// The prompt of a scenario before overrides (`_definition`).
#[derive(Clone, Debug)]
struct Definition {
    label: String,
    prompt: String,
    multiple: bool,
    providers: Option<Vec<String>>,
}

/// A prompt given by the caller for a saved scenario that has no file in the registry.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fallback<'a> {
    pub prompt: Option<&'a str>,
    pub label: Option<&'a str>,
}

#[cfg(test)]
thread_local! {
    /// The write step that fails in a unit test ("fsync" or "replace").
    static FAIL_AT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

fn injected_failure(_step: &'static str) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_AT.with(|fail| fail.get()) == Some(_step) {
        return Err(std::io::Error::other("simulated failure"));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct AiSettingsStore {
    pub path: PathBuf,
    pub prompts_dir: PathBuf,
    /// `None`: the variables of the process at each call, like `os.getenv`.
    legacy_env: Option<LegacyEnv>,
}

impl AiSettingsStore {
    /// `AiSettingsStore(data_dir, prompts_dir)`; the shipped prompts by default.
    pub fn new(data_dir: impl AsRef<Path>, prompts_dir: Option<PathBuf>) -> AiSettingsStore {
        AiSettingsStore {
            path: data_dir.as_ref().join("ai-settings.json"),
            prompts_dir: prompts_dir.unwrap_or_else(default_prompts_dir),
            legacy_env: None,
        }
    }

    /// Fixed legacy variables instead of the process environment.
    pub fn with_legacy_env(mut self, env: LegacyEnv) -> AiSettingsStore {
        self.legacy_env = Some(env);
        self
    }

    fn legacy(&self) -> LegacyEnv {
        self.legacy_env.clone().unwrap_or_else(LegacyEnv::from_process)
    }

    fn registry(&self) -> PromptRegistry {
        PromptRegistry::new(&self.prompts_dir)
    }

    /// The saved settings, or the defaults when there is no file (nothing is created).
    /// Retired features of older versions are dropped from the result, not the file.
    pub fn read(&self) -> Result<Snapshot> {
        match self.read_bytes()? {
            Some(data) => parse_snapshot(&data),
            None => Ok(default_snapshot()),
        }
    }

    /// The file's bytes, one past the size limit at most; `None` when there is no file.
    fn read_bytes(&self) -> Result<Option<Vec<u8>>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut data = Vec::new();
        file.take(MAX_SETTINGS_BYTES as u64 + 1).read_to_end(&mut data)?;
        Ok(Some(data))
    }

    fn effective_models(&self, snapshot: &Snapshot) -> Result<IndexMap<String, String>> {
        let saved = snapshot.get("models").and_then(Json::as_object);
        let mut models: IndexMap<String, String> =
            DEFAULT_MODELS.iter().map(|(provider, model)| (provider.to_string(), model.to_string())).collect();
        let legacy = self.legacy();
        for provider in PROVIDERS {
            if saved.is_some_and(|saved| saved.contains_key(provider)) {
                continue;
            }
            let value = legacy.model(provider);
            if !pytext::strip(value).is_empty() {
                models.extend(validate_models(&json::object([(provider, Json::from(value))]))?);
            }
        }
        if let Some(saved) = saved {
            for (provider, model) in saved {
                models.insert(provider.clone(), model.as_str().unwrap_or_default().to_owned());
            }
        }
        Ok(models)
    }

    fn definition(&self, task_id: &str, fallback: Fallback<'_>) -> Result<Definition> {
        valid_task_id(task_id)?;
        if task_id == INCREMENT_TASK || task_id == SAVED_INCREMENT_TASK {
            let increment = default_task(INCREMENT_TASK).expect("shipped scenario");
            let prompt = match self.registry().get("my-increment") {
                Some(config) => config.template,
                None => fallback.prompt.unwrap_or(increment.prompt).to_owned(),
            };
            return Ok(Definition {
                label: increment.label.to_owned(),
                prompt,
                multiple: increment.multiple,
                providers: increment.providers.map(|providers| providers.iter().map(|p| p.to_string()).collect()),
            });
        }
        if let Some(task) = default_task(task_id) {
            return Ok(Definition {
                label: task.label.to_owned(),
                prompt: task.prompt.to_owned(),
                multiple: task.multiple,
                providers: task.providers.map(|providers| providers.iter().map(|p| p.to_string()).collect()),
            });
        }
        if let Some(config) = self.registry().get(&task_id["saved:".len()..]) {
            return Ok(Definition { label: config.name, prompt: config.template, multiple: false, providers: None });
        }
        if let Some(prompt) = fallback.prompt {
            task_values(task_id, &json::object([("prompt", Json::from(prompt))]))?;
            let label = fallback.label.filter(|label| !label.is_empty()).unwrap_or(task_id);
            return Ok(Definition {
                label: label.to_owned(),
                prompt: prompt.to_owned(),
                multiple: false,
                providers: None,
            });
        }
        Err(value_error("Unknown saved prompt"))
    }

    /// One scenario from the given snapshot (or a fresh read).
    pub fn resolve_task(
        &self,
        task_id: &str,
        snapshot: Option<&Snapshot>,
        fallback: Fallback<'_>,
    ) -> Result<ResolvedTask> {
        let snapshot = match snapshot {
            Some(snapshot) => Cow::Borrowed(snapshot),
            None => Cow::Owned(self.read()?),
        };
        let definition = self.definition(task_id, fallback)?;
        let override_values = task_override(&snapshot, task_id);
        let text = |key: &str| override_values.get(key).and_then(Json::as_str).map(str::to_owned);
        let effort = text("reasoning_effort").unwrap_or_else(|| self.legacy_reasoning_effort());
        if !REASONING_EFFORTS.contains(&effort.as_str()) {
            return Err(value_error("Invalid legacy CODEX_REASONING_EFFORT"));
        }
        let model_overrides = text_values(override_values.get("models"));
        let mut models = self.effective_models(&snapshot)?;
        models.extend(model_overrides.clone());
        let providers = task_providers(task_id, definition.providers.as_deref(), &override_values);
        let copilot = task_id == "copilot";
        let shape = TaskShape::of(copilot);
        Ok(ResolvedTask {
            id: task_id.to_owned(),
            label: definition.label,
            enabled: copilot.then(|| !matches!(override_values.get("enabled"), Some(Json::Bool(false)))),
            prompt: text("prompt").unwrap_or_else(|| definition.prompt.clone()),
            default_prompt: definition.prompt,
            providers,
            allowed_providers: shape.allowed_providers,
            multiple: definition.multiple,
            models,
            model_overrides,
            reasoning_effort: effort,
            claude_effort: text("claude_effort").unwrap_or_default(),
            placeholders: shape.placeholders,
            required_placeholders: shape.required_placeholders,
            apply_mode: shape.apply_mode,
            customized: !override_values.is_empty(),
            revision: snapshot.get("revision").and_then(Json::as_str).unwrap_or_default().to_owned(),
        })
    }

    /// `CODEX_REASONING_EFFORT` when set and not empty, else `xhigh`.
    fn legacy_reasoning_effort(&self) -> String {
        self.legacy().codex_reasoning_effort.filter(|effort| !effort.is_empty()).unwrap_or_else(|| "xhigh".into())
    }

    fn view_of(&self, snapshot: &Snapshot) -> Result<View> {
        let mut task_ids: Vec<String> = DEFAULT_TASKS.iter().map(|task| task.id.to_owned()).collect();
        task_ids.extend(
            self.registry()
                .load_all()
                .into_iter()
                .filter(|config| config.id != "my-increment")
                .map(|config| format!("saved:{}", config.id)),
        );
        // The order in which Python evaluates the dict literal, so that the first error
        // raised is the same.
        let models = self.effective_models(snapshot)?;
        let tasks = task_ids
            .iter()
            .map(|task_id| self.resolve_task(task_id, Some(snapshot), Fallback::default()))
            .collect::<Result<Vec<_>>>()?;
        let model_overrides = snapshot
            .get("models")
            .and_then(Json::as_object)
            .map(|models| {
                models.iter().map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_owned())).collect()
            })
            .unwrap_or_default();
        Ok(View {
            revision: snapshot.get("revision").and_then(Json::as_str).unwrap_or_default().to_owned(),
            models,
            model_overrides,
            tasks,
        })
    }

    pub fn view(&self) -> Result<View> {
        self.view_of(&self.read()?)
    }

    /// A stable inode to lock: locking the replaced settings file itself would let
    /// another process in through the new inode.
    fn write_lock(&self) -> Result<File> {
        let parent = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.path.with_extension("lock"))?;
        lock.set_permissions(Permissions::from_mode(0o600))?;
        lock.lock()?;
        Ok(lock)
    }

    fn mutate(&self, expected_revision: Option<&str>, update: impl FnOnce(&mut Snapshot)) -> Result<View> {
        let _lock = self.write_lock()?;
        let mut snapshot = self.read()?;
        check_revision(&snapshot, expected_revision)?;
        update(&mut snapshot);
        snapshot.insert("revision".into(), Json::from(uuid::Uuid::new_v4().to_string()));
        let raw = Json::Object(snapshot);
        validate_raw(&raw)?;
        let Json::Object(snapshot) = raw else { unreachable!() };
        // Resolve before committing so that a failing view cannot report an error after
        // the user's settings have changed.
        let view = self.view_of(&snapshot)?;
        self.replace_file(&(Json::Object(snapshot).dumps(Some(2)) + "\n"))?;
        Ok(view)
    }

    /// Replaces the settings file with `data` atomically: a private temporary file in
    /// the same directory, fsync, rename.
    fn replace_file(&self, data: &str) -> Result<()> {
        if data.len() > MAX_SETTINGS_BYTES {
            return Err(value_error("AI settings exceed size limit"));
        }
        let parent = self.path.parent().unwrap_or(Path::new("."));
        let temporary = private_temporary(parent, data)?;
        injected_failure("fsync")?;
        temporary.as_file().sync_all()?;
        injected_failure("replace")?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }

    /// Replace the given global model overrides; a blank value removes one.
    pub fn write_models(&self, models: &Json, expected_revision: Option<&str>) -> Result<View> {
        let normalized = validate_models(models)?;
        let given: Vec<String> = models.as_object().map(|map| map.keys().cloned().collect()).unwrap_or_default();
        self.mutate(expected_revision, |snapshot| {
            let saved = object_entry(snapshot, "models");
            for provider in &given {
                saved.shift_remove(provider);
            }
            for (provider, model) in normalized {
                saved.insert(provider, Json::Str(model));
            }
        })
    }

    /// Replace one scenario's override (an empty one removes it). Copilot keeps its
    /// on/off switch unless the values set it.
    pub fn write_task(&self, task_id: &str, values: &Json, expected_revision: Option<&str>) -> Result<View> {
        let task_id = if task_id == SAVED_INCREMENT_TASK { INCREMENT_TASK } else { task_id };
        self.definition(task_id, Fallback::default())?;
        let mut normalized = task_values(task_id, values)?;
        self.mutate(expected_revision, |snapshot| {
            let tasks = object_entry(snapshot, "tasks");
            if task_id == "copilot"
                && !normalized.contains_key("enabled")
                && let Some(enabled) = tasks.get(task_id).and_then(|previous| previous.get("enabled"))
            {
                normalized.insert("enabled".into(), enabled.clone());
            }
            if task_id == INCREMENT_TASK {
                tasks.shift_remove(SAVED_INCREMENT_TASK);
            }
            if normalized.is_empty() {
                tasks.shift_remove(task_id);
            } else {
                tasks.insert(task_id.to_owned(), Json::Object(normalized));
            }
        })
    }

    /// Back to the shipped scenario; Copilot keeps its on/off switch.
    pub fn reset_task(&self, task_id: &str, expected_revision: Option<&str>) -> Result<View> {
        let task_id = if task_id == SAVED_INCREMENT_TASK { INCREMENT_TASK } else { task_id };
        self.definition(task_id, Fallback::default())?;
        self.mutate(expected_revision, |snapshot| {
            let tasks = object_entry(snapshot, "tasks");
            let previous = tasks.shift_remove(task_id);
            if task_id == "copilot"
                && let Some(enabled) = previous.as_ref().and_then(|previous| previous.get("enabled"))
            {
                tasks.insert(task_id.to_owned(), json::object([("enabled", enabled.clone())]));
            }
            if task_id == INCREMENT_TASK {
                tasks.shift_remove(SAVED_INCREMENT_TASK);
            }
        })
    }
}

/// The object under `key` of a validated snapshot.
fn object_entry<'a>(snapshot: &'a mut Snapshot, key: &str) -> &'a mut Map {
    snapshot
        .entry(key.to_owned())
        .or_insert_with(|| Json::Object(Map::new()))
        .as_object_mut()
        .expect("validated object")
}

/// A conflict when the caller's draft was based on another revision than the file's.
fn check_revision(snapshot: &Snapshot, expected_revision: Option<&str>) -> Result<()> {
    let current = snapshot.get("revision").and_then(Json::as_str).unwrap_or_default();
    if expected_revision.is_some_and(|expected| expected != current) {
        return Err(AiError::Conflict("AI settings changed; reload before saving".to_owned()));
    }
    Ok(())
}

/// `data` in a new file readable only by the user, next to the settings.
fn private_temporary(parent: &Path, data: &str) -> Result<tempfile::NamedTempFile> {
    let mut temporary = tempfile::Builder::new().prefix(".ai-settings-").suffix(".tmp").tempfile_in(parent)?;
    temporary.as_file().set_permissions(Permissions::from_mode(0o600))?;
    temporary.write_all(data.as_bytes())?;
    temporary.flush()?;
    Ok(temporary)
}

/// The saved override of a scenario, empty without one.
fn task_override(snapshot: &Snapshot, task_id: &str) -> Map {
    let tasks = snapshot.get("tasks").and_then(Json::as_object);
    let saved = |id: &str| tasks.and_then(|tasks| tasks.get(id)).and_then(Json::as_object).cloned().unwrap_or_default();
    if task_id == INCREMENT_TASK || task_id == SAVED_INCREMENT_TASK {
        // Both historical IDs are read without rewriting the file; the visible
        // scenario wins on a field customized twice.
        let mut merged = saved(SAVED_INCREMENT_TASK);
        merged.extend(saved(INCREMENT_TASK));
        return merged;
    }
    saved(task_id)
}

/// The text values of an object of model names, empty for anything else.
fn text_values(models: Option<&Json>) -> IndexMap<String, String> {
    models
        .and_then(Json::as_object)
        .map(|models| {
            models.iter().map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_owned())).collect()
        })
        .unwrap_or_default()
}

/// The selected providers: the override's, else the scenario's default. An analysis
/// asks every one of them; any other scenario takes the answer of the first that gives
/// one, so by default the other provider answers when the first is not installed or
/// refuses (a spent subscription).
fn task_providers(task_id: &str, defined: Option<&[String]>, override_values: &Map) -> Vec<String> {
    let mut providers: Vec<String> = if task_id.starts_with("analysis.") {
        vec!["claude".into(), "codex".into()]
    } else if task_id == "copilot" {
        vec!["claude".into()]
    } else {
        defined.map_or_else(|| vec!["codex".into(), "claude".into()], <[String]>::to_vec)
    };
    if let Some(Json::Array(items)) = override_values.get("providers") {
        providers = items.iter().filter_map(Json::as_str).map(str::to_owned).collect();
    }
    if task_id == SAVED_INCREMENT_TASK {
        // A saved version holds one result: Codex first when comparison selects both.
        providers.sort_by_key(|provider| provider != "codex");
    }
    providers
}

/// What a scenario offers by kind: Copilot takes only Claude and no placeholders, and
/// applies a model change at the next session.
struct TaskShape {
    allowed_providers: Vec<String>,
    placeholders: Vec<String>,
    required_placeholders: Vec<String>,
    apply_mode: String,
}

impl TaskShape {
    fn of(copilot: bool) -> TaskShape {
        if copilot {
            return TaskShape {
                allowed_providers: vec!["claude".into()],
                placeholders: Vec::new(),
                required_placeholders: Vec::new(),
                apply_mode: "next_event_model_next_session".into(),
            };
        }
        TaskShape {
            allowed_providers: PROVIDERS.map(str::to_owned).to_vec(),
            placeholders: PLACEHOLDERS.iter().map(|name| name.to_string()).collect(),
            required_placeholders: vec!["transcript".into()],
            apply_mode: "next_request".into(),
        }
    }
}

/// What `read` gives without a settings file.
fn default_snapshot() -> Snapshot {
    let mut snapshot = Map::new();
    snapshot.insert("version".into(), Json::Int(1));
    snapshot.insert("revision".into(), Json::from("defaults"));
    snapshot.insert("models".into(), Json::Object(Map::new()));
    snapshot.insert("tasks".into(), Json::Object(Map::new()));
    snapshot
}

/// The settings in a file's bytes, retired features dropped; `SettingsRead` for a file
/// too large, undecodable or invalid.
fn parse_snapshot(data: &[u8]) -> Result<Snapshot> {
    if data.len() > MAX_SETTINGS_BYTES {
        return Err(AiError::SettingsRead("AI settings file exceeds size limit".to_owned()));
    }
    let invalid = || AiError::SettingsRead("Invalid AI settings JSON".to_owned());
    let text = decode_json_bytes(data).ok_or_else(invalid)?;
    let mut raw = Json::parse(&text).map_err(|_| invalid())?;
    drop_retired_features(&mut raw);
    validate_raw(&raw).map_err(|error| AiError::SettingsRead(error.to_string()))?;
    match raw {
        Json::Object(map) => Ok(map),
        _ => unreachable!("validated as an object"),
    }
}

/// Settings saved by an earlier version may still hold retired features.
fn drop_retired_features(raw: &mut Json) {
    let Some(raw) = raw.as_object_mut() else {
        return;
    };
    if let Some(models) = raw.get_mut("models").and_then(Json::as_object_mut) {
        models.shift_remove("openrouter");
    }
    let Some(tasks) = raw.get_mut("tasks").and_then(Json::as_object_mut) else {
        return;
    };
    for retired in ["anchor", "analysis.custom", "analysis.summary"] {
        tasks.shift_remove(retired);
    }
    for values in tasks.values_mut() {
        let Some(values) = values.as_object_mut() else {
            continue;
        };
        if let Some(models) = values.get_mut("models").and_then(Json::as_object_mut)
            && models.contains_key("openrouter")
        {
            models.shift_remove("openrouter");
            if models.is_empty() {
                values.shift_remove("models");
            }
        }
        if let Some(Json::Array(providers)) = values.get_mut("providers")
            && providers.iter().any(|provider| provider.as_str() == Some("openrouter"))
        {
            providers.retain(|provider| provider.as_str() != Some("openrouter"));
            if providers.is_empty() {
                values.shift_remove("providers");
            }
        }
    }
}

/// `_validate_raw`.
fn validate_raw(raw: &Json) -> Result<()> {
    let map = known_fields(raw, &["version", "revision", "models", "tasks"], "settings")?;
    validate_header(map)?;
    let models = &map["models"];
    if json::string_map(&validate_models(models)?) != *models {
        return Err(value_error("Noncanonical model overrides"));
    }
    validate_tasks(&map["tasks"])
}

/// Every field present, version 1, a canonical UUID revision.
fn validate_header(map: &Map) -> Result<()> {
    if map.len() != 4 {
        return Err(value_error("Incomplete AI settings"));
    }
    if !matches!(map.get("version"), Some(Json::Int(1))) {
        return Err(value_error("Unsupported AI settings version"));
    }
    if !map.get("revision").and_then(Json::as_str).is_some_and(is_canonical_uuid) {
        return Err(value_error("Invalid settings revision"));
    }
    Ok(())
}

/// At most [`MAX_TASKS`] overrides, each already in its canonical form.
fn validate_tasks(tasks: &Json) -> Result<()> {
    let tasks = match tasks {
        Json::Object(tasks) if tasks.len() <= MAX_TASKS => tasks,
        _ => return Err(value_error("Invalid task overrides")),
    };
    for (task_id, values) in tasks {
        if Json::Object(task_values(task_id, values)?) != *values {
            return Err(value_error("Noncanonical task override"));
        }
    }
    Ok(())
}

/// `str(uuid.UUID(text)) == text`: lowercase 8-4-4-4-12 hexadecimal.
fn is_canonical_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

/// `json.loads(bytes)`: the encoding detected from a BOM or the zero bytes of the first
/// four, as `json.detect_encoding` does; `None` when the bytes do not decode.
fn decode_json_bytes(data: &[u8]) -> Option<String> {
    let (encoding, bom) = detect_encoding(data);
    let data = &data[bom..];
    match encoding {
        Encoding::Utf8 => String::from_utf8(data.to_vec()).ok(),
        Encoding::Utf16 { big } => decode_utf16(data, big),
        Encoding::Utf32 { big } => decode_utf32(data, big),
    }
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf16 { big: bool },
    Utf32 { big: bool },
}

/// The byte order marks `json.detect_encoding` looks for, in its order.
const BOMS: [(&[u8], Encoding); 5] = [
    (&[0, 0, 0xfe, 0xff], Encoding::Utf32 { big: true }),
    (&[0xff, 0xfe, 0, 0], Encoding::Utf32 { big: false }),
    (&[0xfe, 0xff], Encoding::Utf16 { big: true }),
    (&[0xff, 0xfe], Encoding::Utf16 { big: false }),
    (&[0xef, 0xbb, 0xbf], Encoding::Utf8),
];

/// `json.detect_encoding`: the encoding and the length of the BOM to skip.
fn detect_encoding(data: &[u8]) -> (Encoding, usize) {
    for (bom, encoding) in BOMS {
        if data.starts_with(bom) {
            return (encoding, bom.len());
        }
    }
    (encoding_by_zeros(data), 0)
}

/// Without a BOM: the zero bytes among the first four (or of exactly two bytes).
fn encoding_by_zeros(data: &[u8]) -> Encoding {
    match data {
        [0, 0, _, _, ..] => Encoding::Utf32 { big: true },
        [0, _, _, _, ..] => Encoding::Utf16 { big: true },
        [_, 0, 0, 0, ..] => Encoding::Utf32 { big: false },
        [_, 0, _, _, ..] => Encoding::Utf16 { big: false },
        [0, _] => Encoding::Utf16 { big: true },
        [_, 0] => Encoding::Utf16 { big: false },
        _ => Encoding::Utf8,
    }
}

fn decode_utf16(data: &[u8], big: bool) -> Option<String> {
    if !data.len().is_multiple_of(2) {
        return None;
    }
    let units = data
        .chunks(2)
        .map(|pair| if big { u16::from_be_bytes([pair[0], pair[1]]) } else { u16::from_le_bytes([pair[0], pair[1]]) });
    char::decode_utf16(units).collect::<Result<String, _>>().ok()
}

fn decode_utf32(data: &[u8], big: bool) -> Option<String> {
    if !data.len().is_multiple_of(4) {
        return None;
    }
    data.chunks(4)
        .map(|quad| {
            let bytes = [quad[0], quad[1], quad[2], quad[3]];
            char::from_u32(if big { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> AiSettingsStore {
        AiSettingsStore::new(dir.join("data"), Some(dir.join("prompts"))).with_legacy_env(LegacyEnv::default())
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn fail_at(step: &'static str) {
        FAIL_AT.with(|fail| fail.set(Some(step)));
    }

    #[test]
    fn replace_fails_when_saving_then_previous_bytes_and_no_temp_files() {
        for step in ["replace", "fsync"] {
            let dir = tempfile::tempdir().unwrap();
            let store = store(dir.path());
            store.write_models(&json::object([("codex", Json::from("old"))]), None).unwrap();
            let before = std::fs::read(&store.path).unwrap();
            fail_at(step);
            let error = store.write_models(&json::object([("codex", Json::from("new"))]), None).unwrap_err();
            FAIL_AT.with(|fail| fail.set(None));
            assert!(error.to_string().contains("simulated"), "{step}: {error}");
            assert_eq!(std::fs::read(&store.path).unwrap(), before);
            assert_eq!(entries(store.path.parent().unwrap()), ["ai-settings.json", "ai-settings.lock"]);
        }
    }

    #[test]
    fn bom_or_utf16_given_settings_bytes_when_decoded_then_like_json_loads() {
        assert_eq!(decode_json_bytes(b"\xef\xbb\xbf{}").as_deref(), Some("{}"));
        assert_eq!(decode_json_bytes(&[b'{', 0, b'}', 0]).as_deref(), Some("{}"));
        assert_eq!(decode_json_bytes(&[0xff, 0xfe, b'{', 0, b'}', 0]).as_deref(), Some("{}"));
        assert_eq!(decode_json_bytes(b"\xff"), None);
    }

    #[test]
    fn bom_and_zero_patterns_given_settings_bytes_when_decoded_then_json_detect_encoding() {
        let cases: [(&[u8], Option<&str>); 18] = [
            (&[0, 0, 0xfe, 0xff, 0, 0, 0, b'{', 0, 0, 0, b'}'], Some("{}")),
            (&[0xff, 0xfe, 0, 0, b'{', 0, 0, 0, b'}', 0, 0, 0], Some("{}")),
            (&[0xfe, 0xff, 0, b'{', 0, b'}'], Some("{}")),
            (&[0, b'{', 0, b'}'], Some("{}")),
            (&[0, 0, 0, b'1'], Some("1")),
            (&[b'1', 0, 0, 0], Some("1")),
            (&[b'1', 0, 0, 0, b'2', 0, 0, 0], Some("12")),
            (&[0, b'1'], Some("1")),
            (&[b'1', 0], Some("1")),
            (b"[1]", Some("[1]")),
            (b"", Some("")),
            (&[0xff, 0xfe, 0, 0], Some("")),
            (&[0xfe, 0xff, 0], None),
            (&[0, 0, 0xfe, 0xff, 0, 0], None),
            (&[0, 0, 0xfe, 0xff, 0, 0x11, 0, 0], None),
            (&[0xfe, 0xff, 0xd8, 0x00], None),
            (&[0xef, 0xbb, 0xbf, 0xff], None),
            (&[b'1', 0, b'2'], Some("1\u{0}2")),
        ];
        for (data, text) in cases {
            assert_eq!(decode_json_bytes(data).as_deref(), text, "{data:?}");
        }
    }

    fn raw_settings(models: Json, tasks: Json) -> Json {
        json::object([
            ("version", Json::Int(1)),
            ("revision", Json::from("0f8fad5b-d9cb-469f-a165-70867728950e")),
            ("models", models),
            ("tasks", tasks),
        ])
    }

    fn raw_error(raw: &Json) -> String {
        validate_raw(raw).unwrap_err().to_string()
    }

    #[test]
    fn malformed_files_given_validate_raw_when_checked_then_python_messages() {
        let empty = || Json::Object(Map::new());
        let parse = |text: &str| Json::parse(text).unwrap();
        assert!(
            validate_raw(&raw_settings(parse(r#"{"codex": "m"}"#), parse(r#"{"title": {"claude_effort": "max"}}"#)))
                .is_ok()
        );
        assert_eq!(raw_error(&Json::Array(Vec::new())), "settings must be an object");
        let mut extra = raw_settings(empty(), empty());
        extra.as_object_mut().unwrap().insert("secret".into(), Json::Null);
        assert_eq!(raw_error(&extra), "Unknown settings fields: secret");
        let mut incomplete = raw_settings(empty(), empty());
        incomplete.as_object_mut().unwrap().shift_remove("tasks");
        assert_eq!(raw_error(&incomplete), "Incomplete AI settings");
        for version in [Json::Bool(true), Json::Float(1.0), Json::Int(2)] {
            let mut raw = raw_settings(empty(), empty());
            raw.as_object_mut().unwrap().insert("version".into(), version);
            assert_eq!(raw_error(&raw), "Unsupported AI settings version");
        }
        for revision in [Json::from("defaults"), Json::Int(5)] {
            let mut raw = raw_settings(empty(), empty());
            raw.as_object_mut().unwrap().insert("revision".into(), revision);
            assert_eq!(raw_error(&raw), "Invalid settings revision");
        }
        assert_eq!(raw_error(&raw_settings(parse(r#"{"codex": " "}"#), empty())), "Noncanonical model overrides");
        assert_eq!(raw_error(&raw_settings(parse(r#"{"codex": 5}"#), empty())), "Model must be text");
        assert_eq!(raw_error(&raw_settings(empty(), Json::Array(Vec::new()))), "Invalid task overrides");
        let many = Json::Object((0..=MAX_TASKS).map(|index| (format!("t{index}"), empty())).collect());
        assert_eq!(raw_error(&raw_settings(empty(), many)), "Invalid task overrides");
        let blank_models = parse(r#"{"title": {"prompt": "x {transcript}", "models": {}}}"#);
        assert_eq!(raw_error(&raw_settings(empty(), blank_models)), "Noncanonical task override");
        assert_eq!(raw_error(&raw_settings(empty(), parse(r#"{"nope": {}}"#))), "Unknown AI task");
    }

    fn task_error(task_id: &str, values: &str) -> String {
        task_values(task_id, &Json::parse(values).unwrap()).unwrap_err().to_string()
    }

    #[test]
    fn invalid_fields_given_task_values_when_checked_then_first_failing_field_named() {
        let cases = [
            ("unknown", "{}", "Unknown AI task"),
            ("title", r#"{"x": 1, "a": 2}"#, "Unknown task fields: a, x"),
            ("title", r#"{"enabled": true}"#, "Enabled must be a boolean for Copilot"),
            ("copilot", r#"{"enabled": 1}"#, "Enabled must be a boolean for Copilot"),
            ("title", r#"{"prompt": "", "enabled": true}"#, "Enabled must be a boolean for Copilot"),
            ("title", r#"{"prompt": ""}"#, "Prompt must be nonempty text of at most 32000 characters"),
            (
                "title",
                r#"{"prompt": "a\u0000{transcript}"}"#,
                "Prompt must be nonempty text of at most 32000 characters",
            ),
            ("title", r#"{"prompt": 5}"#, "Prompt must be nonempty text of at most 32000 characters"),
            ("title", r#"{"prompt": "No transcript"}"#, "Prompt must include {transcript}"),
            ("title", r#"{"providers": ["claude", "codex", "claude"]}"#, "Invalid provider selection"),
            ("copilot", r#"{"providers": ["codex"]}"#, "Invalid provider selection"),
            ("analysis.increment", r#"{"providers": ["codex", "codex"]}"#, "Invalid provider selection"),
            ("copilot", r#"{"models": {"codex": "m"}}"#, "Copilot supports only Claude"),
            ("title", r#"{"reasoning_effort": "max"}"#, "Invalid Codex reasoning effort"),
            ("copilot", r#"{"reasoning_effort": "max"}"#, "Invalid Codex reasoning effort"),
            ("copilot", r#"{"reasoning_effort": "high"}"#, "Copilot does not support Codex reasoning effort"),
            ("title", r#"{"claude_effort": "turbo"}"#, "Invalid Claude effort"),
        ];
        for (task_id, values, message) in cases {
            assert_eq!(task_error(task_id, values), message, "{task_id} {values}");
        }
    }

    #[test]
    fn valid_fields_given_task_values_when_normalized_then_fixed_order_and_blank_models_dropped() {
        let values = Json::parse(
            r#"{"claude_effort": "high", "reasoning_effort": "low", "models": {"codex": "m", "claude": " "},
                "providers": ["codex", "claude"], "prompt": "P {transcript}"}"#,
        )
        .unwrap();
        assert_eq!(
            Json::Object(task_values("analysis.increment", &values).unwrap()).dumps(None),
            r#"{"prompt": "P {transcript}", "providers": ["codex", "claude"], "models": {"codex": "m"}, "reasoning_effort": "low", "claude_effort": "high"}"#
        );
        let copilot = Json::parse(r#"{"prompt": "policy", "enabled": false, "models": {"claude": " "}}"#).unwrap();
        assert_eq!(
            Json::Object(task_values("copilot", &copilot).unwrap()).dumps(None),
            r#"{"enabled": false, "prompt": "policy"}"#
        );
        let saved = Json::parse(r#"{"providers": ["claude"]}"#).unwrap();
        assert_eq!(
            Json::Object(task_values("saved:notes", &saved).unwrap()).dumps(None),
            r#"{"providers": ["claude"]}"#
        );
    }

    #[test]
    fn unreadable_settings_path_given_read_when_called_then_os_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data"), "a file, not a directory").unwrap();
        assert!(matches!(store(dir.path()).read(), Err(AiError::Io(_))));
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data/ai-settings.json")).unwrap();
        assert!(matches!(store(dir.path()).read(), Err(AiError::Io(_))));
    }

    #[test]
    fn stale_revision_given_mutate_when_saving_then_conflict_and_file_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let first = store.write_models(&json::object([("codex", Json::from("old"))]), None).unwrap();
        let before = std::fs::read(&store.path).unwrap();
        let error = store.write_models(&json::object([("codex", Json::from("new"))]), Some("stale")).unwrap_err();
        assert!(matches!(&error, AiError::Conflict(message) if message == "AI settings changed; reload before saving"));
        assert_eq!(std::fs::read(&store.path).unwrap(), before);
        let next = store.write_models(&json::object([("codex", Json::from("new"))]), Some(&first.revision)).unwrap();
        assert_eq!(next.models["codex"], "new");
        assert_ne!(next.revision, first.revision);
    }

    #[test]
    fn uuid_texts_given_revision_check_when_validated_then_only_canonical_form_passes() {
        assert!(is_canonical_uuid("0f8fad5b-d9cb-469f-a165-70867728950e"));
        assert!(!is_canonical_uuid("0F8FAD5B-D9CB-469F-A165-70867728950E"));
        assert!(!is_canonical_uuid("{0f8fad5b-d9cb-469f-a165-70867728950e}"));
        assert!(!is_canonical_uuid("defaults"));
    }
}
