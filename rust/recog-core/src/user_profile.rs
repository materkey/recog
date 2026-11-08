//! The private local addressing profile shared by the frontend and the live pipeline
//! — the port of `recog/user_profile.py`.
//!
//! `user-profile.json` under the data root: `{"version": 1, "revision", "name",
//! "aliases"}` written atomically with mode 0600 in a 0700 directory. A broken file is
//! never silently replaced: reading fails, and so does saving over it.

use std::collections::HashMap;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::pyjson::{self, Map, Value};
use crate::{paths, py};

pub const MAX_NAME_LENGTH: usize = 80;
pub const MAX_ALIASES: usize = 16;
const MAX_FILE_BYTES: u64 = 16384;

/// A profile as `read()` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub version: i64,
    pub revision: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub configured: bool,
}

impl Profile {
    fn new(name: String, aliases: Vec<String>, revision: String) -> Self {
        let configured = !name.is_empty();
        Self { version: 1, revision, name, aliases, configured }
    }

    /// The dict Python returns, in its key order.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("version", Value::from(self.version));
        map.insert("revision", Value::from(self.revision.as_str()));
        map.insert("name", Value::from(self.name.as_str()));
        map.insert("aliases", Value::Array(self.aliases.iter().map(|a| Value::from(a.as_str())).collect()));
        map.insert("configured", Value::Bool(self.configured));
        Value::Object(map)
    }
}

/// A rejected input or file: Python's `ValueError` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ProfileError {}

fn invalid(message: impl Into<String>) -> ProfileError {
    ProfileError::Invalid(message.into())
}

fn io_error(error: &std::io::Error, path: &Path) -> ProfileError {
    ProfileError::Io(paths::os_error_message(error, path))
}

fn label(value: Option<&Value>) -> Result<String, ProfileError> {
    let Some(Value::Str(value)) = value else {
        return Err(invalid("Profile names must be strings"));
    };
    if value.chars().any(py::is_other) {
        return Err(invalid("Profile names cannot contain control characters"));
    }
    let value = py::strip(value);
    if value.chars().count() > MAX_NAME_LENGTH {
        return Err(invalid(format!("Profile names must be at most {MAX_NAME_LENGTH} characters")));
    }
    Ok(value.to_owned())
}

fn is_name(value: &str) -> bool {
    !value.is_empty() && py::casefold(value) != "me"
}

fn aliases(values: Option<&Value>) -> Result<Vec<String>, ProfileError> {
    let message = || invalid(format!("Profile aliases must be a list of at most {MAX_ALIASES} names"));
    let Some(Value::Array(values)) = values else { return Err(message()) };
    if values.len() > MAX_ALIASES {
        return Err(message());
    }
    let labels = values.iter().map(|v| label(Some(v))).collect::<Result<Vec<_>, _>>()?;
    Ok(unique_names(labels))
}

fn unique_names<I: IntoIterator<Item = String>>(values: I) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = Vec::new();
    for value in values {
        let key = py::casefold(&value);
        if is_name(&value) && !seen.contains(&key) {
            result.push(value);
            seen.push(key);
        }
    }
    result
}

fn writer_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Mutex::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(locks.entry(paths::resolve(path)).or_default())
}

/// `UserProfileStore(data_dir)`.
#[derive(Debug, Clone)]
pub struct UserProfileStore {
    pub path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl UserProfileStore {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        let path = data_dir.join("user-profile.json");
        let lock = writer_lock(&path);
        Self { path, lock }
    }

    /// `read()`: the saved profile, or the unconfigured one when there is no file.
    ///
    /// # Errors
    /// A file that is too large, not JSON, or not a valid profile.
    pub fn read(&self) -> Result<Profile, ProfileError> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Profile::new(String::new(), Vec::new(), "unconfigured".into()));
            }
            Err(e) => return Err(io_error(&e, &self.path)),
        };
        let mut raw = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut raw).map_err(|e| io_error(&e, &self.path))?;
        if raw.len() > usize::try_from(MAX_FILE_BYTES).unwrap_or(usize::MAX) {
            return Err(invalid("Profile file is too large"));
        }
        let saved = pyjson::loads_bytes(&raw).map_err(|_| invalid("Profile file is not valid JSON"))?;
        let saved = match saved {
            Value::Object(map) if matches!(map.get("version"), Some(Value::Int(1))) => map,
            _ => return Err(invalid("Unsupported profile version")),
        };
        let revision = match saved.get("revision") {
            Some(Value::Str(r))
                if !py::strip(r).is_empty() && r.chars().count() <= 128 && !r.chars().any(py::is_other) =>
            {
                r.clone()
            }
            _ => return Err(invalid("Invalid profile revision")),
        };
        let name = label(saved.get("name"))?;
        let aliases = aliases(saved.get("aliases"))?;
        if !name.is_empty() && !is_name(&name) {
            return Err(invalid("Profile name cannot be Me"));
        }
        if name.is_empty() && !aliases.is_empty() {
            return Err(invalid("An empty profile cannot have aliases"));
        }
        Ok(Profile::new(name, aliases, revision))
    }

    /// `write(name, aliases)`.
    ///
    /// # Errors
    /// An invalid name or alias, a broken existing file, or a failed write.
    pub fn write(&self, name: &str, aliases_in: &[String]) -> Result<Profile, ProfileError> {
        let name = label(Some(&Value::from(name)))?;
        if !is_name(&name) {
            return Err(invalid("Profile name must be nonempty and cannot be Me"));
        }
        let list = Value::Array(aliases_in.iter().map(|a| Value::from(a.as_str())).collect());
        let aliases = aliases(Some(&list))?;
        self.save(name, aliases)
    }

    /// `clear()`: an unconfigured profile with a fresh revision, kept as a tombstone.
    ///
    /// # Errors
    /// A broken existing file, or a failed write.
    pub fn clear(&self) -> Result<Profile, ProfileError> {
        self.save(String::new(), Vec::new())
    }

    fn save(&self, name: String, aliases: Vec<String>) -> Result<Profile, ProfileError> {
        let _guard = self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A broken existing profile needs an explicit repair, never silent replacement.
        self.read()?;
        let result = Profile::new(name, aliases, uuid::Uuid::new_v4().hyphenated().to_string());
        let mut saved = result.to_json();
        if let Some(map) = saved.as_object_mut() {
            map.remove("configured");
        }
        let dir = self.path.parent().unwrap_or(Path::new("."));
        paths::mkdir_parents(dir, 0o700).map_err(|e| io_error(&e, dir))?;
        let temp = paths::create_unique(dir, ".user-profile-", ".tmp", 0o600).map_err(|e| io_error(&e, dir))?;
        let contents = format!("{}\n", pyjson::dumps(&saved, None));
        paths::replace_with(&self.path, temp, contents.as_bytes(), true, Some(0o600))
            .map_err(|e| io_error(&e, &self.path))?;
        Ok(result)
    }
}

/// `resolve_identity(profile, legacy_name=, override=)`: CLI labels win for this
/// run; saving a profile retires the legacy alias.
#[must_use]
pub fn resolve_identity(profile: &Profile, legacy_name: Option<&str>, override_name: Option<&str>) -> Value {
    let explicit = override_name.map_or("", py::strip);
    let legacy = legacy_name.map_or("", py::strip);
    let effective = [explicit, profile.name.as_str(), legacy, "Me"].into_iter().find(|s| !s.is_empty()).unwrap_or("Me");
    let names = [profile.name.clone(), effective.to_owned()].into_iter().chain(profile.aliases.iter().cloned());
    let aliases = unique_names(names);
    let mut map = Map::new();
    map.insert("local_user_name", Value::from(effective));
    map.insert("local_user_aliases", Value::Array(aliases.iter().map(|a| Value::from(a.as_str())).collect()));
    map.insert("local_user_profile_revision", Value::from(profile.revision.as_str()));
    map.insert("local_user_identity_known", Value::Bool(!aliases.is_empty()));
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn missing_profile_when_read_then_unconfigured_without_creating_directory() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let store = UserProfileStore::new(&dir.path().join("data"));
        // when
        let profile = store.read().unwrap();
        // then
        assert_eq!(profile, Profile::new(String::new(), Vec::new(), "unconfigured".into()));
        assert!(!dir.path().join("data").exists());
    }

    #[test]
    fn unicode_names_when_saved_then_normalized_private_and_round_trip() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = UserProfileStore::new(&data);
        // when
        let saved = store.write("  Анна  ", &strings(&["Аня", "аня", "Me", "  "])).unwrap();
        // then
        assert_eq!((saved.name.as_str(), saved.aliases.clone()), ("Анна", strings(&["Аня"])));
        assert_eq!(store.read().unwrap(), saved);
        assert_eq!(fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&data).unwrap().permissions().mode() & 0o777, 0o700);
        let text = fs::read_to_string(&store.path).unwrap();
        assert!(text.starts_with("{\"version\": 1, \"revision\": ") && text.ends_with("\"aliases\": [\"Аня\"]}\n"));
    }

    #[test]
    fn invalid_input_when_written_then_rejected_without_file() {
        for (name, aliases) in [
            ("", vec![]),
            ("me", vec![]),
            ("ME", vec![]),
            ("a\nb", vec![]),
            (&"x".repeat(81) as &str, vec![]),
            ("Anna", vec!["x\u{200b}"]),
            ("Anna", vec!["a"; 17]),
        ] {
            // given
            let dir = tempfile::tempdir().unwrap();
            let store = UserProfileStore::new(dir.path());
            // when
            let result = store.write(name, &strings(&aliases));
            // then
            assert!(result.is_err(), "{name:?} {aliases:?}");
            assert!(!store.path.exists());
        }
    }

    #[test]
    fn configured_profile_when_cleared_then_revision_tombstone_persists() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let store = UserProfileStore::new(dir.path());
        let saved = store.write("Anna", &[]).unwrap();
        // when
        let cleared = store.clear().unwrap();
        // then
        assert!(!cleared.configured && cleared.revision != saved.revision);
        assert_eq!(store.read().unwrap(), cleared);
    }

    #[test]
    fn corrupt_profile_when_read_or_saved_then_error_preserves_bytes() {
        for payload in [
            &b"{"[..],
            b"[]",
            b"{\"version\": 2}",
            b"{\"version\": true, \"revision\": \"r\", \"name\": \"\", \"aliases\": []}",
            b"{\"version\": 1, \"revision\": \" \", \"name\": \"\", \"aliases\": []}",
            b"{\"version\": 1, \"revision\": \"r\", \"name\": \"Me\", \"aliases\": []}",
            b"{\"version\": 1, \"revision\": \"r\", \"name\": \"\", \"aliases\": [\"A\"]}",
            b"\xff",
        ] {
            // given
            let dir = tempfile::tempdir().unwrap();
            let store = UserProfileStore::new(dir.path());
            fs::write(&store.path, payload).unwrap();
            // when / then
            assert!(store.read().is_err());
            assert!(store.write("Anna", &[]).is_err());
            assert_eq!(fs::read(&store.path).unwrap(), payload);
        }
    }

    #[test]
    fn concurrent_stores_when_saving_then_unique_revisions_and_complete_reads() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        // when
        let revisions: Vec<String> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    UserProfileStore::new(&path).write(&format!("User {i}"), &[]).unwrap().revision
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        // then
        let unique: std::collections::BTreeSet<_> = revisions.iter().collect();
        assert_eq!(unique.len(), 8);
        assert!(UserProfileStore::new(&path).read().is_ok());
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
    }

    #[test]
    fn identity_inputs_when_resolved_then_precedence_and_aliases_match() {
        // given
        let configured = Profile::new("Anna".into(), strings(&["Аня"]), "r1".into());
        let empty = Profile::new(String::new(), Vec::new(), "unconfigured".into());
        // when / then
        for (profile, legacy, override_name, name, aliases) in [
            (&configured, Some("Legacy"), Some(" Cli "), "Cli", vec!["Anna", "Cli", "Аня"]),
            (&configured, Some("Legacy"), None, "Anna", vec!["Anna", "Аня"]),
            (&empty, Some("Legacy"), None, "Legacy", vec!["Legacy"]),
            (&empty, Some("Me"), None, "Me", vec![]),
            (&empty, None, Some("  "), "Me", vec![]),
        ] {
            let identity = resolve_identity(profile, legacy, override_name);
            let map = identity.as_object().unwrap();
            assert_eq!(map.get("local_user_name"), Some(&Value::from(name)));
            let expected = Value::Array(aliases.iter().map(|a| Value::from(*a)).collect());
            assert_eq!(map.get("local_user_aliases"), Some(&expected));
            assert_eq!(map.get("local_user_identity_known"), Some(&Value::Bool(!aliases.is_empty())));
        }
    }
}
