//! What the Python modules lean on implicitly: exceptions by class (so that each
//! `except (OSError, ValueError, ...)` lets through what Python lets through), their
//! messages as `str(exc)` prints them, and the conversions and lookups on loaded JSON
//! (`dict.get`, `x[key]`, `int()`, `float()`, `{**a, **b}`) with the errors Python
//! raises for the wrong type.

use std::fmt;
use std::io;
use std::path::Path;

use nix::errno::Errno;
use recog_core::py;
use recog_core::pyjson::{self, Map, Value};

/// A Python exception: its class name and `str(exc)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyErr {
    pub kind: &'static str,
    pub message: String,
}

impl fmt::Display for PyErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PyErr {}

const OS_ERRORS: [&str; 15] = [
    "OSError",
    "FileNotFoundError",
    "PermissionError",
    "NotADirectoryError",
    "IsADirectoryError",
    "FileExistsError",
    "ProcessLookupError",
    "BlockingIOError",
    "TimeoutError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "InterruptedError",
    "ChildProcessError",
    "BrokenPipeError",
    "ConnectionAbortedError",
];
const VALUE_ERRORS: [&str; 5] =
    ["ValueError", "JSONDecodeError", "UnicodeDecodeError", "UnicodeEncodeError", "InvalidFileException"];
const PSUTIL_ERRORS: [&str; 3] = ["NoSuchProcess", "AccessDenied", "ZombieProcess"];

/// The `OSError` subclass of each errno Python maps to one; any other is `OSError`.
const ERRNO_CLASSES: [(Errno, &str); 18] = [
    (Errno::ENOENT, "FileNotFoundError"),
    (Errno::EACCES, "PermissionError"),
    (Errno::EPERM, "PermissionError"),
    (Errno::ENOTDIR, "NotADirectoryError"),
    (Errno::EISDIR, "IsADirectoryError"),
    (Errno::EEXIST, "FileExistsError"),
    (Errno::ESRCH, "ProcessLookupError"),
    (Errno::EAGAIN, "BlockingIOError"),
    (Errno::EALREADY, "BlockingIOError"),
    (Errno::EINPROGRESS, "BlockingIOError"),
    (Errno::ETIMEDOUT, "TimeoutError"),
    (Errno::ECONNREFUSED, "ConnectionRefusedError"),
    (Errno::ECONNRESET, "ConnectionResetError"),
    (Errno::ECONNABORTED, "ConnectionAbortedError"),
    (Errno::EINTR, "InterruptedError"),
    (Errno::ECHILD, "ChildProcessError"),
    (Errno::EPIPE, "BrokenPipeError"),
    (Errno::ESHUTDOWN, "BrokenPipeError"),
];

fn os_error_class(code: i32) -> &'static str {
    let errno = Errno::from_raw(code);
    ERRNO_CLASSES.iter().find(|(known, _)| *known == errno).map_or("OSError", |(_, class)| class)
}

/// `str(exc)`: `[Errno N] text`, with `: 'path'` when the error is about one.
fn os_error_text(error: &io::Error, code: i32, path: Option<&Path>) -> String {
    match path {
        Some(path) => recog_core::paths::os_error_message(error, path),
        None => {
            let text = error.to_string();
            let strerror = text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text).to_owned();
            format!("[Errno {code}] {strerror}")
        }
    }
}

impl PyErr {
    #[must_use]
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    #[must_use]
    pub fn value(message: impl Into<String>) -> Self {
        Self::new("ValueError", message)
    }

    #[must_use]
    pub fn type_error(message: impl Into<String>) -> Self {
        Self::new("TypeError", message)
    }

    /// `KeyError(key)`: `str()` is the key's repr.
    #[must_use]
    pub fn key(key: &str) -> Self {
        Self::new("KeyError", py::repr(key))
    }

    /// `AttributeError` of `value.name` on a JSON value without it.
    #[must_use]
    pub fn attribute(value: &Value, name: &str) -> Self {
        Self::new("AttributeError", format!("'{}' object has no attribute '{name}'", value.type_name()))
    }

    /// `psutil.NoSuchProcess`.
    #[must_use]
    pub fn no_such_process(pid: i64) -> Self {
        Self::new("NoSuchProcess", format!("process no longer exists (pid={pid})"))
    }

    /// `psutil.ZombieProcess`.
    #[must_use]
    pub fn zombie(pid: i64) -> Self {
        Self::new("ZombieProcess", format!("PID still exists but it's a zombie (pid={pid})"))
    }

    /// `psutil.AccessDenied`.
    #[must_use]
    pub fn access_denied(pid: i64) -> Self {
        Self::new("AccessDenied", format!("(pid={pid})"))
    }

    /// The `OSError` subclass Python raises for `error`, about `path` when given.
    #[must_use]
    pub fn os(error: &io::Error, path: Option<&Path>) -> Self {
        let Some(code) = error.raw_os_error() else {
            return Self::new("OSError", error.to_string());
        };
        Self::new(os_error_class(code), os_error_text(error, code, path))
    }

    /// `isinstance(exc, OSError)`.
    #[must_use]
    pub fn is_os(&self) -> bool {
        OS_ERRORS.contains(&self.kind)
    }

    /// `isinstance(exc, ValueError)`.
    #[must_use]
    pub fn is_value(&self) -> bool {
        VALUE_ERRORS.contains(&self.kind)
    }

    /// `isinstance(exc, psutil.Error)`.
    #[must_use]
    pub fn is_psutil(&self) -> bool {
        PSUTIL_ERRORS.contains(&self.kind)
    }

    #[must_use]
    pub fn is(&self, kind: &str) -> bool {
        self.kind == kind
    }

    /// `type(exc).__name__ + ": " + str(exc)`, as a log line quotes it.
    #[must_use]
    pub fn described(&self) -> String {
        format!("{}: {}", self.kind, self.message)
    }
}

/// `Errno`'s error for a `nix` call about `path`.
#[must_use]
pub fn errno_error(errno: Errno, path: Option<&Path>) -> PyErr {
    PyErr::os(&io::Error::from_raw_os_error(errno as i32), path)
}

/// `value.get(key)` of a dict; any other value has no `get`.
///
/// # Errors
/// `AttributeError` for a value that is not a dict.
pub fn get<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, PyErr> {
    value.as_object().map(|map| map.get(key)).ok_or_else(|| PyErr::attribute(value, "get"))
}

/// `value[key]` with a string key.
///
/// # Errors
/// `KeyError` for a missing key, `TypeError` for a value that cannot be indexed so.
pub fn index<'a>(value: &'a Value, key: &str) -> Result<&'a Value, PyErr> {
    match value {
        Value::Object(map) => map.get(key).ok_or_else(|| PyErr::key(key)),
        Value::Array(_) => Err(PyErr::type_error("list indices must be integers or slices, not str")),
        Value::Str(_) => Err(PyErr::type_error("string indices must be integers, not 'str'")),
        other => Err(PyErr::type_error(format!("'{}' object is not subscriptable", other.type_name()))),
    }
}

/// `value.get(key) == expected` for a string.
#[must_use]
pub fn str_at(value: &Value, key: &str) -> Option<String> {
    value.as_object().and_then(|map| map.get(key)).and_then(Value::as_str).map(str::to_owned)
}

/// `int(value)`.
///
/// # Errors
/// What `int()` raises for the value.
pub fn to_int(value: &Value) -> Result<i128, PyErr> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Bool(b) => Ok(i128::from(*b)),
        Value::Float(x) if x.is_nan() => Err(PyErr::value("cannot convert float NaN to integer")),
        Value::Float(x) if x.is_infinite() => {
            Err(PyErr::new("OverflowError", "cannot convert float infinity to integer"))
        }
        #[allow(clippy::cast_possible_truncation)]
        Value::Float(x) => Ok(x.trunc() as i128),
        Value::Str(s) => py::parse_int(s).map(i128::from).map_err(PyErr::value),
        other => Err(PyErr::type_error(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// `float(value)`.
///
/// # Errors
/// What `float()` raises for the value.
pub fn to_float(value: &Value) -> Result<f64, PyErr> {
    match value {
        Value::Float(x) => Ok(*x),
        #[allow(clippy::cast_precision_loss)]
        Value::Int(n) => Ok(*n as f64),
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        Value::Str(s) => py::parse_float(s).map_err(PyErr::value),
        other => Err(PyErr::type_error(format!(
            "float() argument must be a string or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// A number for arithmetic (`time.time() - value`): bool, int or float.
///
/// # Errors
/// `TypeError` for anything else.
pub fn number(value: &Value, operation: &str) -> Result<f64, PyErr> {
    match value {
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        #[allow(clippy::cast_precision_loss)]
        Value::Int(n) => Ok(*n as f64),
        Value::Float(x) => Ok(*x),
        other => Err(PyErr::type_error(format!(
            "unsupported operand type(s) for {operation}: 'float' and '{}'",
            other.type_name()
        ))),
    }
}

/// `{**base, **extra}`.
///
/// # Errors
/// `TypeError` when either is not a mapping.
pub fn spread(base: &Value, extra: &Value) -> Result<Value, PyErr> {
    let not_mapping = |v: &Value| PyErr::type_error(format!("'{}' object is not a mapping", v.type_name()));
    let mut map = base.as_object().ok_or_else(|| not_mapping(base))?.clone();
    for (key, value) in extra.as_object().ok_or_else(|| not_mapping(extra))?.iter() {
        map.insert(key.clone(), value.clone());
    }
    Ok(Value::Object(map))
}

/// A dict literal.
#[must_use]
pub fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect::<Map>())
}

/// `json.dumps(value, ensure_ascii=False)`.
#[must_use]
pub fn dumps(value: &Value) -> String {
    pyjson::dumps(value, None)
}

/// `json.dumps(value)`: everything outside printable ASCII as `\uXXXX`. Outside
/// strings the text is ASCII already, so escaping the whole text is escaping the
/// strings.
#[must_use]
pub fn dumps_ascii(value: &Value) -> String {
    use std::fmt::Write as _;
    let text = pyjson::dumps(value, None);
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if u32::from(c) > 0x7e {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `json.loads(path.read_text())`: the text decoded as UTF-8 with universal newlines.
///
/// # Errors
/// `UnicodeDecodeError` or `JSONDecodeError`.
pub fn loads_text(raw: &[u8]) -> Result<Value, PyErr> {
    let text =
        std::str::from_utf8(raw).map_err(|e| PyErr::new("UnicodeDecodeError", pyjson::utf8_error_message(raw, &e)))?;
    let text = if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.to_owned() };
    pyjson::loads(&text).map_err(|e| PyErr::new("JSONDecodeError", e.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_ascii_text_when_dumped_for_the_wire_then_escaped_like_json_dumps() {
        // given
        let value = obj([("path", Value::from("/Applications/Толк.app")), ("emoji", Value::from("\u{1f600}\u{7f}"))]);
        // when
        let text = dumps_ascii(&value);
        // then
        assert_eq!(
            text,
            "{\"path\": \"/Applications/\\u0422\\u043e\\u043b\\u043a.app\", \"emoji\": \"\\ud83d\\ude00\\u007f\"}"
        );
    }

    #[test]
    fn wrong_types_when_looked_up_then_python_errors() {
        // given
        let list = Value::Array(vec![]);
        // when / then
        assert_eq!(get(&list, "a").unwrap_err().described(), "AttributeError: 'list' object has no attribute 'get'");
        assert_eq!(index(&obj([]), "app").unwrap_err().described(), "KeyError: 'app'");
        assert_eq!(
            index(&Value::from("x"), "app").unwrap_err().described(),
            "TypeError: string indices must be integers, not 'str'"
        );
        assert_eq!(to_int(&Value::Float(12.9)).unwrap(), 12);
        assert!(to_int(&Value::Float(f64::INFINITY)).unwrap_err().is("OverflowError"));
        assert_eq!(to_float(&Value::from(" 1_0.5 ")).unwrap(), 10.5);
        assert!(spread(&list, &obj([])).unwrap_err().message.contains("'list' object is not a mapping"));
    }

    #[test]
    fn io_errors_when_converted_then_python_classes_and_messages() {
        // given
        let missing = io::Error::from_raw_os_error(2);
        // when
        let error = PyErr::os(&missing, Some(Path::new("/x")));
        // then
        assert_eq!(error.described(), "FileNotFoundError: [Errno 2] No such file or directory: '/x'");
        assert!(error.is_os());
        assert!(PyErr::os(&io::Error::from_raw_os_error(35), None).is("BlockingIOError") || cfg!(target_os = "linux"));
    }

    #[test]
    fn errno_codes_when_converted_then_python_os_error_subclasses() {
        // given
        let cases = [
            (Errno::ENOENT, "FileNotFoundError"),
            (Errno::EACCES, "PermissionError"),
            (Errno::EPERM, "PermissionError"),
            (Errno::ENOTDIR, "NotADirectoryError"),
            (Errno::EISDIR, "IsADirectoryError"),
            (Errno::EEXIST, "FileExistsError"),
            (Errno::ESRCH, "ProcessLookupError"),
            (Errno::EAGAIN, "BlockingIOError"),
            (Errno::EALREADY, "BlockingIOError"),
            (Errno::EINPROGRESS, "BlockingIOError"),
            (Errno::ETIMEDOUT, "TimeoutError"),
            (Errno::ECONNREFUSED, "ConnectionRefusedError"),
            (Errno::ECONNRESET, "ConnectionResetError"),
            (Errno::ECONNABORTED, "ConnectionAbortedError"),
            (Errno::EINTR, "InterruptedError"),
            (Errno::ECHILD, "ChildProcessError"),
            (Errno::EPIPE, "BrokenPipeError"),
            (Errno::ESHUTDOWN, "BrokenPipeError"),
            (Errno::EINVAL, "OSError"),
        ];
        for (errno, kind) in cases {
            // when
            let error = errno_error(errno, None);
            // then
            assert_eq!(error.kind, kind, "{errno:?}");
            assert!(error.is_os(), "{errno:?}");
        }
    }

    #[test]
    fn error_without_a_path_or_a_code_when_converted_then_errno_text_or_plain_message() {
        // given
        let missing = io::Error::from_raw_os_error(2);
        let plain = io::Error::other("no command");
        // when / then
        assert_eq!(PyErr::os(&missing, None).message, "[Errno 2] No such file or directory");
        assert_eq!(PyErr::os(&plain, None).described(), "OSError: no command");
    }

    #[test]
    fn json_values_when_converted_to_numbers_then_like_int_float_and_arithmetic() {
        // given / when / then
        assert_eq!(to_int(&Value::Bool(true)).unwrap(), 1);
        assert_eq!(to_int(&Value::from(" 42 ")).unwrap(), 42);
        assert_eq!(
            to_int(&Value::Float(f64::NAN)).unwrap_err().described(),
            "ValueError: cannot convert float NaN to integer"
        );
        assert_eq!(
            to_int(&Value::from("x")).unwrap_err().described(),
            "ValueError: invalid literal for int() with base 10: 'x'"
        );
        assert_eq!(
            to_int(&Value::Null).unwrap_err().described(),
            "TypeError: int() argument must be a string, a bytes-like object or a real number, not 'NoneType'"
        );
        assert_eq!(to_float(&Value::Int(3)).unwrap(), 3.0);
        assert_eq!(to_float(&Value::Bool(true)).unwrap(), 1.0);
        assert!(to_float(&Value::from("nan")).unwrap().is_nan());
        assert_eq!(
            to_float(&Value::from("x")).unwrap_err().described(),
            "ValueError: could not convert string to float: 'x'"
        );
        assert_eq!(
            to_float(&Value::Array(vec![])).unwrap_err().described(),
            "TypeError: float() argument must be a string or a real number, not 'list'"
        );
        assert_eq!(number(&Value::Bool(true), "-").unwrap(), 1.0);
        assert_eq!(number(&Value::Int(7), "-").unwrap(), 7.0);
        assert_eq!(
            number(&Value::from("7"), "-").unwrap_err().described(),
            "TypeError: unsupported operand type(s) for -: 'float' and 'str'"
        );
    }
}
