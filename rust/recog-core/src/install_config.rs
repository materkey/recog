//! Installation settings: one TOML file per user, read once at startup — the port of
//! `recog/install_config.py`.
//!
//! `~/.config/recog/config.toml` (or `$XDG_CONFIG_HOME/recog/config.toml`, or
//! `RECOG_CONFIG_FILE`) holds what differs between installations and is not a secret.
//! Every setting is also an environment variable, which wins over the file; the
//! repository's `.env` fills unset variables first. Malformed values fail with the
//! Python messages, a file value an environment variable overrides with a different
//! value is reported, and the legacy `.local/conference-hosts.json` is still read with
//! a deprecation notice while `config.toml` is absent.
//!
//! The environment is passed in as a map instead of read from the process: tests and
//! callers decide what it holds, and nothing here writes the process environment.
//!
//! Known differences from Python:
//! - the message after `<path>: ` for a TOML syntax error is the toml crate's with
//!   tomllib's `(at line L, column C)` suffix (Python prints tomllib's wording); both
//!   read TOML 1.0, and the parity tests compare such errors by the path only;
//! - integers beyond 64 bits in the file are a syntax error (tomllib reads them), and
//!   an environment integer beyond i64 is rejected (Python's `int()` accepts it);
//! - `~user/` is not expanded in `RECOG_CONFIG_FILE` and `RECOG_CONFERENCE_HOSTS_FILE`
//!   (only `~` and `~/`);
//! - a `.env` that is not valid UTF-8 is read lossily (Python's import fails on it);
//! - an `OSError` reading a file other than "not found" and "is a directory" may word
//!   its strerror differently.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::pyjson;
use crate::url::{parse_ipv4, urlsplit};
use crate::{paths, py};

pub use crate::paths::Environ;

/// Every platform the extension and the bridge know.
pub const PLATFORMS: [&str; 6] = ["ktalk", "vas3k", "jitsi", "matrix", "gmeet", "zoom"];
/// Platforms whose hosts an installation configures.
pub const CONFIGURABLE_PLATFORMS: [&str; 2] = ["jitsi", "matrix"];

// Hosts the extension manifest already covers; configuring one again would register a
// second platform for the same pages.
const BUILTIN_HOSTS: [&str; 2] = ["meet.vas3k.cloud", "meet.google.com"];
const BUILTIN_WILDCARD_DOMAINS: [&str; 2] = ["ktalk.ru", "zoom.us"];

/// How a setting's value is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Flag,
    Integer,
    Number,
    Text,
    List,
}

/// One setting: its table and key in `config.toml` and its environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub table: &'static str,
    pub key: &'static str,
    pub env: &'static str,
    pub kind: Kind,
}

/// A setting's value, whatever its kind.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Flag(bool),
    Integer(Option<i64>),
    Number(f64),
    Text(String),
    List(Vec<String>),
}

trait SettingType: Sized {
    fn to_value(&self) -> SettingValue;
    fn from_value(value: SettingValue) -> Option<Self>;
}

impl SettingType for bool {
    fn to_value(&self) -> SettingValue {
        SettingValue::Flag(*self)
    }
    fn from_value(value: SettingValue) -> Option<Self> {
        if let SettingValue::Flag(b) = value { Some(b) } else { None }
    }
}

impl SettingType for i64 {
    fn to_value(&self) -> SettingValue {
        SettingValue::Integer(Some(*self))
    }
    fn from_value(value: SettingValue) -> Option<Self> {
        if let SettingValue::Integer(n) = value { n } else { None }
    }
}

impl SettingType for Option<i64> {
    fn to_value(&self) -> SettingValue {
        SettingValue::Integer(*self)
    }
    fn from_value(value: SettingValue) -> Option<Self> {
        if let SettingValue::Integer(n) = value { Some(n) } else { None }
    }
}

impl SettingType for f64 {
    fn to_value(&self) -> SettingValue {
        SettingValue::Number(*self)
    }
    fn from_value(value: SettingValue) -> Option<Self> {
        if let SettingValue::Number(x) = value { Some(x) } else { None }
    }
}

impl SettingType for String {
    fn to_value(&self) -> SettingValue {
        SettingValue::Text(self.clone())
    }
    fn from_value(value: SettingValue) -> Option<Self> {
        if let SettingValue::Text(s) = value { Some(s) } else { None }
    }
}

impl SettingType for Vec<String> {
    fn to_value(&self) -> SettingValue {
        SettingValue::List(self.clone())
    }
    fn from_value(value: SettingValue) -> Option<Self> {
        if let SettingValue::List(l) = value { Some(l) } else { None }
    }
}

/// A section of settings, the dataclasses of the Python module.
pub trait Section {
    /// The settings in declaration order, the order Python validates them in.
    const SETTINGS: &'static [Setting];
    fn get(&self, key: &str) -> SettingValue;
    fn set(&mut self, key: &str, value: SettingValue);
}

macro_rules! section {
    ($(#[$doc:meta])* $name:ident, $table:literal {
        $($(#[$fdoc:meta])* $field:ident : $ty:ty = ($env:literal, $kind:ident, $default:expr)),* $(,)?
    }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name { $($(#[$fdoc])* pub $field: $ty,)* }

        impl Default for $name {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        impl Section for $name {
            const SETTINGS: &'static [Setting] =
                &[$(Setting { table: $table, key: stringify!($field), env: $env, kind: Kind::$kind },)*];

            fn get(&self, key: &str) -> SettingValue {
                match key {
                    $(stringify!($field) => self.$field.to_value(),)*
                    _ => unreachable!("{} has no setting {key}", $table),
                }
            }

            fn set(&mut self, key: &str, value: SettingValue) {
                match key {
                    $(stringify!($field) => {
                        self.$field = <$ty as SettingType>::from_value(value)
                            .unwrap_or_else(|| unreachable!("{}.{key} got a value of another kind", $table));
                    })*
                    _ => unreachable!("{} has no setting {key}", $table),
                }
            }
        }
    };
}

section!(
    /// `[asr]`.
    AsrSettings, "asr" {
        /// "auto" | "local" | "remote" | "wavo": auto is wavo on a Mac where setup.sh
        /// built it, else local (PyTorch); remote offloads GigaAM to a gateway (ADR-0020).
        backend: String = ("RECOG_ASR_BACKEND", Text, "auto".into()),
        /// auto (cuda > mps > cpu) | cuda | mps | cpu; explicit + unavailable fails.
        device: String = ("RECOG_ASR_DEVICE", Text, "auto".into()),
        /// torch.set_num_threads() cap for CPU inference; None leaves torch alone.
        torch_threads: Option<i64> = ("RECOG_TORCH_THREADS", Integer, None),
        /// Gateway base URL; no default, so remote without it fails at startup.
        remote_url: String = ("RECOG_ASR_REMOTE_URL", Text, String::new()),
        remote_model: String = ("RECOG_ASR_REMOTE_MODEL", Text, "gigaam".into()),
        remote_timeout_s: f64 = ("RECOG_ASR_REMOTE_TIMEOUT_S", Number, 15.0),
        /// Shorter timeout of live chunk/batch calls; refine/offline keep the long one.
        live_timeout_s: f64 = ("RECOG_ASR_LIVE_TIMEOUT_S", Number, 5.0),
        circuit_fails: i64 = ("RECOG_ASR_CIRCUIT_FAILS", Integer, 3),
        circuit_open_s: f64 = ("RECOG_ASR_CIRCUIT_OPEN_S", Number, 60.0),
        /// Audio kept in memory while no ASR can serve; the oldest is dropped beyond it.
        pending_buffer_cap_s: f64 = ("ASR_PENDING_BUFFER_CAP_S", Number, 600.0),
    }
);

section!(
    /// `[live]`.
    LiveSettings, "live" {
        /// Last-words dedup applies only within this gap from the previous segment.
        dedup_max_gap_s: f64 = ("DEDUP_MAX_GAP_S", Number, 2.0),
        /// Pinned PulseAudio/PipeWire monitor (Linux); empty follows the default sink.
        pulse_monitor_source: String = ("RECOG_PULSE_MONITOR_SOURCE", Text, String::new()),
        /// Native Zoom application source on macOS, same as `live --zoom-app`.
        zoom_app: bool = ("RECOG_ZOOM_APP", Flag, false),
        /// Cancel what the built-in speakers play out of the microphone (macOS).
        echo_cancel: bool = ("RECOG_ECHO_CANCEL", Flag, false),
        /// The microphone's child process on macOS: "python" or "rust".
        mic_worker: String = ("RECOG_MIC_WORKER", Text, "python".into()),
        /// Record golden sessions under <data dir>/golden for replay.
        golden_record: bool = ("RECOG_GOLDEN_RECORD", Flag, false),
    }
);

section!(
    /// `[copilot]`.
    CopilotSettings, "copilot" {
        autospawn: bool = ("RECOG_COPILOT_AUTOSPAWN", Flag, true),
        grace_s: f64 = ("RECOG_COPILOT_GRACE_S", Number, 90.0),
        mcp_url: String = ("RECOG_COPILOT_MCP_URL", Text, "http://127.0.0.1:8767/mcp".into()),
    }
);

section!(
    /// `[web]`.
    WebSettings, "web" {
        bridge_ws_url: String = ("RECOG_BRIDGE_WS_URL", Text, "ws://localhost:8766".into()),
        /// Exact origins allowed besides the conference hosts, backend only.
        extra_ws_origins: Vec<String> = ("RECOG_EXTRA_WS_ORIGINS", List, Vec::new()),
        /// Read-only meeting directories of other sources: "path" or "path:source".
        external_meetings_paths: Vec<String> = ("EXTERNAL_MEETINGS_PATHS", List, Vec::new()),
    }
);

section!(
    /// `[macos]`.
    MacosSettings, "macos" {
        /// Signing identity of RecogZoomHelper.app; empty signs ad hoc.
        codesign_identity: String = ("RECOG_CODESIGN_IDENTITY", Text, String::new()),
    }
);

/// The section tables in the order Python declares them.
pub const SECTIONS: [&str; 5] = ["asr", "live", "copilot", "web", "macos"];

/// Every setting of every section.
pub fn all_settings() -> impl Iterator<Item = &'static Setting> {
    AsrSettings::SETTINGS
        .iter()
        .chain(LiveSettings::SETTINGS)
        .chain(CopilotSettings::SETTINGS)
        .chain(WebSettings::SETTINGS)
        .chain(MacosSettings::SETTINGS)
}

/// `SETTING_KEYS[variable]`: the `config.toml` key of an environment variable, for
/// messages (`[copilot] autospawn`).
#[must_use]
pub fn setting_key(variable: &str) -> Option<String> {
    all_settings().find(|s| s.env == variable).map(|s| format!("[{}] {}", s.table, s.key))
}

/// The typed installation settings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InstallConfig {
    pub jitsi: Vec<String>,
    pub matrix: Vec<String>,
    /// Nonstandard source names of old recordings -> the platform they were.
    pub source_aliases: Vec<(String, String)>,
    /// Web UI names of the sources the installation's own importers write.
    pub source_labels: Vec<(String, String)>,
    pub asr: AsrSettings,
    pub live: LiveSettings,
    pub copilot: CopilotSettings,
    pub web: WebSettings,
    pub macos: MacosSettings,
    /// Environment variables whose setting the file gave, to report conflicts.
    pub from_file: BTreeSet<String>,
}

impl InstallConfig {
    /// A setting by its table and key.
    #[must_use]
    pub fn get(&self, table: &str, key: &str) -> SettingValue {
        match table {
            "asr" => self.asr.get(key),
            "live" => self.live.get(key),
            "copilot" => self.copilot.get(key),
            "web" => self.web.get(key),
            "macos" => self.macos.get(key),
            _ => unreachable!("no section {table}"),
        }
    }

    fn set(&mut self, table: &str, key: &str, value: SettingValue) {
        match table {
            "asr" => self.asr.set(key, value),
            "live" => self.live.set(key, value),
            "copilot" => self.copilot.set(key, value),
            "web" => self.web.set(key, value),
            "macos" => self.macos.set(key, value),
            _ => unreachable!("no section {table}"),
        }
    }

    /// `canonical_source(source)`: legacy installation labels resolved without
    /// rewriting stored meetings.
    #[must_use]
    pub fn canonical_source<'a>(&'a self, source: Option<&'a str>) -> Option<&'a str> {
        let source = source?;
        Some(self.source_aliases.iter().find(|(name, _)| name == source).map_or(source, |(_, platform)| platform))
    }

    /// What `python -m recog.install_config` prints for the extension build.
    #[must_use]
    pub fn hosts_json(&self) -> String {
        let list =
            |hosts: &[String]| pyjson::Value::Array(hosts.iter().map(|h| pyjson::Value::from(h.as_str())).collect());
        let map: pyjson::Map =
            [("jitsi".to_owned(), list(&self.jitsi)), ("matrix".to_owned(), list(&self.matrix))].into_iter().collect();
        pyjson::dumps(&pyjson::Value::Object(map), None)
    }
}

/// A parsed `config.toml` or legacy JSON document, as Python sees it after `tomllib`
/// or `json`.
#[derive(Debug, Clone, PartialEq)]
pub enum Doc {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Datetime(String),
    Array(Vec<Doc>),
    Table(Vec<(String, Doc)>),
}

impl Doc {
    fn get(&self, key: &str) -> Option<&Doc> {
        match self {
            Doc::Table(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self {
            Doc::Table(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    fn empty_table() -> Self {
        Doc::Table(Vec::new())
    }
}

impl From<toml::Value> for Doc {
    fn from(value: toml::Value) -> Self {
        match value {
            toml::Value::String(s) => Doc::Str(s),
            toml::Value::Integer(n) => Doc::Int(i128::from(n)),
            toml::Value::Float(x) => Doc::Float(x),
            toml::Value::Boolean(b) => Doc::Bool(b),
            toml::Value::Datetime(d) => Doc::Datetime(d.to_string()),
            toml::Value::Array(items) => Doc::Array(items.into_iter().map(Doc::from).collect()),
            toml::Value::Table(table) => Doc::Table(table.into_iter().map(|(k, v)| (k, Doc::from(v))).collect()),
        }
    }
}

impl From<pyjson::Value> for Doc {
    fn from(value: pyjson::Value) -> Self {
        match value {
            pyjson::Value::Null => Doc::Null,
            pyjson::Value::Bool(b) => Doc::Bool(b),
            pyjson::Value::Int(n) => Doc::Int(n),
            pyjson::Value::Float(x) => Doc::Float(x),
            pyjson::Value::Str(s) => Doc::Str(s),
            pyjson::Value::Array(items) => Doc::Array(items.into_iter().map(Doc::from).collect()),
            pyjson::Value::Object(map) => {
                Doc::Table(map.iter().map(|(k, v)| (k.clone(), Doc::from(v.clone()))).collect())
            }
        }
    }
}

fn table<'a>(value: &'a Doc, name: &str, keys: &[&str]) -> Result<&'a Doc, String> {
    let is_table = matches!(value, Doc::Table(_));
    if !is_table || value.keys().iter().any(|k| !keys.contains(k)) {
        let mut sorted: Vec<&str> = keys.to_vec();
        sorted.sort_unstable();
        return Err(format!("{name} must be a table with only {}", sorted.join(", ")));
    }
    Ok(value)
}

fn is_builtin_host(hostname: &str) -> bool {
    BUILTIN_HOSTS.contains(&hostname)
        || BUILTIN_WILDCARD_DOMAINS.iter().any(|d| hostname == *d || hostname.ends_with(&format!(".{d}")))
}

fn canonical_hostname(hostname: &str) -> bool {
    let label_ok = |label: &str| {
        let b = label.as_bytes();
        let edge = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
        !b.is_empty() && b.len() <= 63 && edge(b[0]) && edge(b[b.len() - 1]) && b.iter().all(|&c| edge(c) || c == b'-')
    };
    !hostname.is_empty() && hostname.chars().count() <= 253 && hostname.split('.').all(label_ok)
}

/// `_ipv4_shorthand`: a numeric last label must make a canonical dotted address.
fn ipv4_shorthand(hostname: &str) -> Result<bool, String> {
    let last = hostname.rsplit('.').next().unwrap_or(hostname);
    let hex = last
        .strip_prefix("0x")
        .is_some_and(|h| !h.is_empty() && h.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    let numeric = (!last.is_empty() && last.bytes().all(|c| c.is_ascii_digit())) || hex;
    if !numeric {
        return Ok(false);
    }
    let octets = parse_ipv4(hostname)?;
    Ok(format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]) != hostname)
}

fn origin(value: &Doc) -> Result<String, String> {
    let Doc::Str(origin) = value else {
        return Err("Conference origins must be strings".into());
    };
    let hostname = urlsplit(origin)?.hostname().unwrap_or_default();
    if !canonical_hostname(&hostname) || *origin != format!("https://{hostname}") || is_builtin_host(&hostname) {
        return Err("Use canonical HTTPS origins without ports, paths or built-in hosts".into());
    }
    if ipv4_shorthand(&hostname)? {
        return Err("Use a canonical IPv4 address".into());
    }
    Ok(origin.clone())
}

fn conference_hosts(conferences: &Doc) -> Result<(Vec<String>, Vec<String>), String> {
    let mut hosts: Vec<Vec<String>> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for platform in CONFIGURABLE_PLATFORMS {
        let empty = Doc::Array(Vec::new());
        let Doc::Array(origins) = conferences.get(platform).unwrap_or(&empty) else {
            return Err(format!("{platform} must be an array of HTTPS origins"));
        };
        let list = origins.iter().map(origin).collect::<Result<Vec<_>, _>>()?;
        let unique: BTreeSet<String> = list.iter().cloned().collect();
        if unique.iter().any(|o| seen.contains(o)) || unique.len() != origins.len() {
            return Err("Conference origins must be unique across platforms".into());
        }
        seen.extend(unique);
        hosts.push(list);
    }
    let matrix = hosts.pop().unwrap_or_default();
    let jitsi = hosts.pop().unwrap_or_default();
    Ok((jitsi, matrix))
}

fn source_map(value: &Doc, message: &str, valid: fn(&str, &Doc) -> bool) -> Result<Vec<(String, String)>, String> {
    let Doc::Table(entries) = value else { return Err(message.into()) };
    if !entries.iter().all(|(name, target)| !name.is_empty() && valid(name, target)) {
        return Err(message.into());
    }
    Ok(entries
        .iter()
        .map(|(name, target)| match target {
            Doc::Str(s) => (name.clone(), s.clone()),
            _ => unreachable!("validated as a string"),
        })
        .collect())
}

fn alias(name: &str, platform: &Doc) -> bool {
    !PLATFORMS.contains(&name) && matches!(platform, Doc::Str(p) if CONFIGURABLE_PLATFORMS.contains(&p.as_str()))
}

fn label(_name: &str, label: &Doc) -> bool {
    matches!(label, Doc::Str(s) if !s.is_empty())
}

fn file_value(kind: Kind, value: &Doc, name: &str) -> Result<SettingValue, String> {
    let wrong = || format!("{name} has the wrong type");
    let positive = |n: i128| {
        if n <= 0 {
            return Err(format!("{name} must be a positive integer"));
        }
        // tomllib reads any integer; the toml crate stops at i64, so this holds.
        i64::try_from(n).map_err(|_| format!("{name} must be a positive integer"))
    };
    Ok(match (kind, value) {
        (Kind::Flag, Doc::Bool(b)) => SettingValue::Flag(*b),
        (Kind::Integer, Doc::Int(n)) => SettingValue::Integer(Some(positive(*n)?)),
        #[allow(clippy::cast_precision_loss)]
        (Kind::Number, Doc::Int(n)) => SettingValue::Number(*n as f64),
        (Kind::Number, Doc::Float(x)) => SettingValue::Number(*x),
        (Kind::Text, Doc::Str(s)) => SettingValue::Text(s.clone()),
        (Kind::List, Doc::Array(items)) => SettingValue::List(
            items
                .iter()
                .map(|item| if let Doc::Str(s) = item { Ok(s.clone()) } else { Err(wrong()) })
                .collect::<Result<_, _>>()?,
        ),
        _ => return Err(wrong()),
    })
}

fn settings_of(table_name: &str) -> &'static [Setting] {
    match table_name {
        "asr" => AsrSettings::SETTINGS,
        "live" => LiveSettings::SETTINGS,
        "copilot" => CopilotSettings::SETTINGS,
        "web" => WebSettings::SETTINGS,
        "macos" => MacosSettings::SETTINGS,
        _ => unreachable!("no section {table_name}"),
    }
}

/// `parse_install_config(document)`: validate a parsed TOML document; nothing unknown
/// is ignored.
///
/// # Errors
/// The `TypeError`/`ValueError` text Python raises, without the file's path.
pub fn parse_install_config(document: &Doc) -> Result<InstallConfig, String> {
    let mut top_keys = vec!["conferences", "sources"];
    top_keys.extend(SECTIONS);
    let document = table(document, "The configuration", &top_keys)?;
    let empty = Doc::empty_table();
    let conferences = table(document.get("conferences").unwrap_or(&empty), "[conferences]", &CONFIGURABLE_PLATFORMS)?;
    let sources = table(document.get("sources").unwrap_or(&empty), "[sources]", &["aliases", "labels"])?;
    let mut config = InstallConfig {
        source_aliases: source_map(
            sources.get("aliases").unwrap_or(&empty),
            "[sources.aliases] must map legacy names to jitsi or matrix",
            alias,
        )?,
        source_labels: source_map(
            sources.get("labels").unwrap_or(&empty),
            "[sources.labels] must map source names to nonempty names",
            label,
        )?,
        ..InstallConfig::default()
    };
    for name in SECTIONS {
        let settings = settings_of(name);
        let keys: Vec<&str> = settings.iter().map(|s| s.key).collect();
        let section = table(document.get(name).unwrap_or(&empty), &format!("[{name}]"), &keys)?;
        for setting in settings {
            if let Some(value) = section.get(setting.key) {
                let value = file_value(setting.kind, value, &format!("{name}.{}", setting.key))?;
                config.set(name, setting.key, value);
                config.from_file.insert(setting.env.to_owned());
            }
        }
    }
    (config.jitsi, config.matrix) = conference_hosts(conferences)?;
    Ok(config)
}

fn environment_value(setting: &Setting, environ: &Environ) -> Result<Option<SettingValue>, String> {
    let raw = py::strip(environ.get(setting.env).map_or("", String::as_str));
    if raw.is_empty() {
        return Ok(None);
    }
    let name = setting.env;
    let value = match setting.kind {
        Kind::Flag => {
            let lower = raw.to_lowercase();
            match lower.as_str() {
                "1" | "true" | "yes" | "on" => SettingValue::Flag(true),
                "0" | "false" | "no" | "off" => SettingValue::Flag(false),
                _ => {
                    return Err(format!(
                        "{name}: {name}={} is not one of 1/0, true/false, yes/no, on/off",
                        py::repr(raw)
                    ));
                }
            }
        }
        Kind::Integer => {
            let n = py::parse_int(raw).map_err(|e| format!("{name}: {e}"))?;
            if n <= 0 {
                return Err(format!("{name}: {name} must be a positive integer"));
            }
            SettingValue::Integer(Some(n))
        }
        Kind::Number => SettingValue::Number(py::parse_float(raw).map_err(|e| format!("{name}: {e}"))?),
        Kind::Text => SettingValue::Text(raw.to_owned()),
        Kind::List => SettingValue::List(
            raw.split(',').map(py::strip).filter(|item| !item.is_empty()).map(str::to_owned).collect(),
        ),
    };
    Ok(Some(value))
}

/// `with_environment(config, environ)`: environment values over the file's, and the
/// variables that override a different value the file gave.
///
/// # Errors
/// A malformed environment value, named by its variable.
pub fn with_environment(config: &InstallConfig, environ: &Environ) -> Result<(InstallConfig, Vec<String>), String> {
    let mut resolved = config.clone();
    let mut overridden = Vec::new();
    for setting in all_settings() {
        let Some(value) = environment_value(setting, environ)? else { continue };
        if config.from_file.contains(setting.env) && value != config.get(setting.table, setting.key) {
            overridden.push(setting.env.to_owned());
        }
        resolved.set(setting.table, setting.key, value);
    }
    Ok((resolved, overridden))
}

fn resolve(configured: &str, repo_root: &Path, environ: &Environ) -> PathBuf {
    let path = paths::expanduser(configured, environ);
    if path.is_absolute() { path } else { repo_root.join(path) }
}

/// `config_path()`.
#[must_use]
pub fn config_path(repo_root: &Path, environ: &Environ) -> PathBuf {
    if let Some(configured) = environ.get("RECOG_CONFIG_FILE").filter(|v| !v.is_empty()) {
        return resolve(configured, repo_root, environ);
    }
    let base = environ.get("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map_or("~/.config", String::as_str);
    paths::expanduser(base, environ).join("recog").join("config.toml")
}

/// `legacy_hosts_path()`: where installations kept conference hosts before
/// `config.toml`.
#[must_use]
pub fn legacy_hosts_path(repo_root: &Path, environ: &Environ) -> PathBuf {
    let configured = environ
        .get("RECOG_CONFERENCE_HOSTS_FILE")
        .filter(|v| !v.is_empty())
        .map_or(".local/conference-hosts.json", String::as_str);
    resolve(configured, repo_root, environ)
}

/// Why a file could not be read: an `OSError` escapes the loader unwrapped, a
/// decoding error (a `ValueError`) is reported with the file's path.
enum ReadError {
    Os(String),
    Decode(String),
}

/// `path.read_text(encoding="utf-8")`: universal newlines, errors as Python words them.
fn read_text(path: &Path) -> Result<String, ReadError> {
    let raw = std::fs::read(path).map_err(|e| ReadError::Os(paths::os_error_message(&e, path)))?;
    let text = std::str::from_utf8(&raw).map_err(|e| ReadError::Decode(pyjson::utf8_error_message(&raw, &e)))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn toml_error(text: &str, error: &toml::de::Error) -> String {
    let mut message = error.message().to_owned();
    if let Some(span) = error.span() {
        let before = &text[..span.start.min(text.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        let _ = write!(message, " (at line {line}, column {column})");
    }
    message
}

fn from_legacy(value: &Doc) -> Result<Doc, String> {
    let value = table(value, "Conference hosts", &["jitsi", "matrix", "source_aliases"])?;
    let conferences: Vec<(String, Doc)> = CONFIGURABLE_PLATFORMS
        .iter()
        .filter_map(|key| value.get(key).map(|v| ((*key).to_owned(), v.clone())))
        .collect();
    let mut document = vec![("conferences".to_owned(), Doc::Table(conferences))];
    if let Some(aliases) = value.get("source_aliases") {
        document.push(("sources".to_owned(), Doc::Table(vec![("aliases".to_owned(), aliases.clone())])));
    }
    Ok(Doc::Table(document))
}

/// `load_install_config(path, legacy)`. The deprecation notice of the legacy file
/// goes to `warn`, before the file is parsed, as Python prints it.
///
/// # Errors
/// `<path>: <message>` for a malformed file, and the message for both files present.
pub fn load_install_config(path: &Path, legacy: &Path, warn: &mut dyn FnMut(String)) -> Result<InstallConfig, String> {
    let (exists, legacy_exists) = (path.exists(), legacy.exists());
    if exists && legacy_exists {
        let name = legacy.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(format!(
            "Both {} and {} exist: move the hosts into the first and delete {name}",
            path.display(),
            legacy.display()
        ));
    }
    let shown = if exists { path } else { legacy };
    let wrap = |error: String| format!("{}: {error}", shown.display());
    let read = |file: &Path| {
        read_text(file).map_err(|e| match e {
            ReadError::Os(message) => message,
            ReadError::Decode(message) => wrap(message),
        })
    };
    if exists {
        let text = read(path)?;
        let document: toml::Table = toml::from_str(&text).map_err(|e| wrap(toml_error(&text, &e)))?;
        return parse_install_config(&Doc::from(toml::Value::Table(document))).map_err(wrap);
    }
    if legacy_exists {
        warn(format!(
            "recog: {} is deprecated, move its settings to {} (docs/configuration.md)",
            legacy.display(),
            path.display()
        ));
        let text = read(legacy)?;
        let document = pyjson::loads(&text).map_err(|e| wrap(e.0))?;
        return parse_install_config(&from_legacy(&Doc::from(document)).map_err(wrap)?).map_err(wrap);
    }
    Ok(InstallConfig::default())
}

/// `load_dotenv(env_path)`: fill unset variables of `environ` from a `.env` file
/// without overriding any. The file is only read.
pub fn load_dotenv(env_path: &Path, environ: &mut Environ) {
    // Python opens the file in text mode with the locale's encoding (UTF-8 here) and
    // would raise on undecodable bytes; lossy decoding keeps the readable lines.
    let Ok(raw) = std::fs::read(env_path) else { return };
    let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n").replace('\r', "\n");
    for line in text.split_inclusive('\n') {
        let line = py::strip(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = py::strip(key);
            let value = py::strip(value).trim_matches('"').trim_matches('\'');
            if !key.is_empty() && !environ.contains_key(key) {
                environ.insert(key.to_owned(), value.to_owned());
            }
        }
    }
}

/// The installation settings as recog's processes get them: the repository's `.env`
/// into `environ` (unset variables only), then the file, then the environment over
/// it. Notices (the legacy file, file values the environment overrides) go to `warn`
/// with Python's wording.
///
/// # Errors
/// A malformed file or environment value, as Python's import would raise it.
pub fn load(repo_root: &Path, environ: &mut Environ, warn: &mut dyn FnMut(String)) -> Result<InstallConfig, String> {
    load_dotenv(&repo_root.join(".env"), environ);
    let config = load_install_config(&config_path(repo_root, environ), &legacy_hosts_path(repo_root, environ), warn)?;
    let (config, overridden) = with_environment(&config, environ)?;
    for variable in overridden {
        let key = setting_key(&variable).unwrap_or_default();
        warn(format!(
            "recog: {variable} from the environment or .env overrides {key} of config.toml; keep only one of them"
        ));
    }
    Ok(config)
}

/// [`load`] over the process environment, notices on stderr, as importing the Python
/// module does. The process environment itself is not changed.
///
/// # Errors
/// As [`load`].
pub fn load_process(repo_root: &Path) -> Result<InstallConfig, String> {
    let mut environ: Environ = std::env::vars().collect();
    load(repo_root, &mut environ, &mut |notice| eprintln!("{notice}"))
}

#[cfg(test)]
mod tests;
