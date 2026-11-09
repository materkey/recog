//! One error type standing for the Python exceptions the ported modules raise. The
//! message is what `str(exc)` gives in Python wherever recog wrote the message itself;
//! errors from the operating system or a JSON parser carry Rust's wording.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum AiError {
    /// `ValueError`: invalid input or settings.
    Value(String),
    /// `SettingsConflictError`: the caller's draft was based on an older revision.
    Conflict(String),
    /// `SettingsReadError`: the persisted settings are malformed.
    SettingsRead(String),
    /// `FileNotFoundError` raised by recog: a CLI that is not installed, or a meeting
    /// directory deleted while its dialog was being answered.
    NotFound(String),
    /// `OSError` from the file system or a subprocess.
    Io(io::Error),
    /// `RuntimeError`: no increment version could be claimed.
    Runtime(String),
    /// `KeyError`.
    Key(String),
    /// `AttributeError`/`TypeError` from data of the wrong shape.
    Type(String),
    /// A plain `Exception` from a provider (CLI exit code, `turn.failed`, no answer).
    Provider(String),
    /// `asyncio.TimeoutError`.
    Timeout,
}

impl AiError {
    /// Whether Python would catch this as `ValueError` (the settings errors subclass it).
    pub fn is_value_error(&self) -> bool {
        matches!(self, AiError::Value(_) | AiError::Conflict(_) | AiError::SettingsRead(_))
    }

    /// `AttributeError: 'list' object has no attribute 'get'`.
    pub fn no_get(type_name: &str) -> AiError {
        AiError::Type(format!("'{type_name}' object has no attribute 'get'"))
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiError::Value(message)
            | AiError::Conflict(message)
            | AiError::SettingsRead(message)
            | AiError::NotFound(message)
            | AiError::Runtime(message)
            | AiError::Type(message)
            | AiError::Provider(message) => formatter.write_str(message),
            AiError::Io(error) => write!(formatter, "{error}"),
            AiError::Key(key) => write!(formatter, "'{key}'"),
            AiError::Timeout => Ok(()),
        }
    }
}

impl std::error::Error for AiError {}

impl From<io::Error> for AiError {
    fn from(error: io::Error) -> AiError {
        AiError::Io(error)
    }
}

pub type Result<T, E = AiError> = std::result::Result<T, E>;
