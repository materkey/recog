//! What the server reads once at startup, as importing `recog/web/server.py` does:
//! the repository's `.env` (unset variables only), the installation configuration,
//! the data directory, the legacy microphone name and `ffmpeg` on `PATH`.
//!
//! Nothing writes the process environment: the merged variables are kept here and
//! handed to the AI settings (legacy model variables) and to the CLI providers.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use recog_ai::install::LegacyEnv;
use recog_ai::provider::{CliEnv, which};
use recog_core::install_config::{self, Environ, InstallConfig};
use recog_core::meeting_storage::{MeetingStorage, StorageError, StorageOptions, external_paths};
use recog_core::protocol;
use recog_core::{data_dir, py, ws_origin};

/// The host and port the Python module declares (`HOST`, `PORT`).
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 9080;

/// The file the live process's bridge rewrites every second.
pub const KTALK_DEBUG_FILE: &str = protocol::KTALK_DEBUG_FILE;

#[derive(Clone, Debug)]
pub struct Config {
    /// The checkout: `crates/recog-web/static` and `crates/recog-ai/prompts` are under it.
    pub repo_root: PathBuf,
    pub static_dir: PathBuf,
    pub prompts_dir: PathBuf,
    pub data_dir: PathBuf,
    pub meetings_dir: PathBuf,
    pub home: PathBuf,
    pub install: InstallConfig,
    pub bridge_ws_url: String,
    /// `[web] extra_ws_origins` and the configured conference hosts.
    pub extra_origins: Vec<String>,
    pub external_paths: Vec<(PathBuf, Option<String>)>,
    /// `MIC_SPEAKER_NAME`, `Me` when unset.
    pub mic_speaker_name: String,
    pub ffmpeg: OsString,
    pub host: String,
    pub port: u16,
    pub ktalk_debug_file: PathBuf,
    pub legacy_env: LegacyEnv,
    pub cli_env: CliEnv,
    /// `CHAT_TRANSCRIPT_MAX_CHARS`: far above the analysis limit, a question may be
    /// about any moment of the meeting.
    pub chat_transcript_max_chars: usize,
}

impl Config {
    /// The configuration from an environment (already merged with `.env`).
    ///
    /// # Errors
    /// A malformed installation configuration or data directory, as Python's import
    /// would raise it.
    pub fn from_environ(repo_root: &Path, environ: &Environ, host: &str, port: u16) -> Result<Config, String> {
        let mut environ = environ.clone();
        let mut warn = |notice: String| eprintln!("{notice}");
        let install = install_config::load(repo_root, &mut environ, &mut warn)?;
        let data_dir = data_dir::get_data_dir(repo_root, &environ, &mut warn).map_err(|e| e.to_string())?;
        let home = recog_core::paths::expanduser("~", &environ);
        let mut cli_env = CliEnv::from_process();
        let mut keys: Vec<&String> = environ.keys().collect();
        keys.sort();
        for key in keys {
            if cli_env.get(key).is_none() {
                cli_env.set(key.as_str(), environ[key].as_str());
            }
        }
        let legacy_env = LegacyEnv::from_vars(environ.iter().map(|(k, v)| (k.clone(), v.clone())));
        let mic = py::strip(environ.get("MIC_SPEAKER_NAME").map_or("", String::as_str));
        let ffmpeg = which("ffmpeg", &cli_env).map_or_else(|| OsString::from("ffmpeg"), PathBuf::into_os_string);
        let ktalk_debug_file = environ
            .get("RECOG_WEB_KTALK_DEBUG_FILE")
            .filter(|v| !v.is_empty())
            .map_or_else(|| PathBuf::from(KTALK_DEBUG_FILE), PathBuf::from);
        Ok(Config {
            repo_root: repo_root.to_path_buf(),
            static_dir: repo_root.join("crates/recog-web/static"),
            prompts_dir: repo_root.join("crates/recog-ai/prompts"),
            meetings_dir: data_dir.join("meetings"),
            data_dir,
            home,
            bridge_ws_url: install.web.bridge_ws_url.clone(),
            extra_origins: ws_origin::extra_origins(&install),
            external_paths: external_paths(&install.web.external_meetings_paths),
            mic_speaker_name: if mic.is_empty() { "Me".to_owned() } else { mic.to_owned() },
            install,
            ffmpeg,
            host: host.to_owned(),
            port,
            ktalk_debug_file,
            legacy_env,
            cli_env,
            chat_transcript_max_chars: 300_000,
        })
    }

    /// The configuration of this process: its environment and the repository's `.env`.
    ///
    /// # Errors
    /// As [`Config::from_environ`].
    pub fn from_process(repo_root: &Path, host: &str, port: u16) -> Result<Config, String> {
        let environ: Environ = std::env::vars().collect();
        Config::from_environ(repo_root, &environ, host, port)
    }

    /// `get_storage()`: a `MeetingStorage` over the meetings directory and the external
    /// ones.
    ///
    /// # Errors
    /// The meetings directory cannot be created.
    pub fn storage(&self) -> Result<MeetingStorage, StorageError> {
        let mut options = StorageOptions::new(&self.home);
        options.external_paths = self.external_paths.clone();
        options.source_aliases = self.install.source_aliases.clone();
        MeetingStorage::new(&self.meetings_dir, options)
    }

    /// `INSTALL_CONFIG.source_labels.get(source)`.
    #[must_use]
    pub fn source_label(&self, source: &str) -> Option<&str> {
        self.install.source_labels.iter().find(|(name, _)| name == source).map(|(_, label)| label.as_str())
    }

    /// `canonical_source(source)`.
    #[must_use]
    pub fn canonical_source<'a>(&'a self, source: &'a str) -> &'a str {
        self.install.canonical_source(Some(source)).unwrap_or(source)
    }
}

/// The checkout this binary is installed in (`<repo>/bin/recog-web`), else the one it
/// was built from.
#[must_use]
pub fn default_repo_root() -> PathBuf {
    recog_core::install::installed_repo().unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
