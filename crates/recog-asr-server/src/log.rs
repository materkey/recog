//! Log lines on stderr in the format of the Python modules' `logging.basicConfig`:
//! `%(asctime)s %(levelname)s %(message)s`.

fn line(level: &str, message: &str) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S,%3f");
    eprintln!("{now} {level} {message}");
}

/// `log.info(...)`.
pub fn info(message: &str) {
    line("INFO", message);
}

/// `log.warning(...)`.
pub fn warning(message: &str) {
    line("WARNING", message);
}

/// `log.error(...)` and `log.exception(...)`.
pub fn error(message: &str) {
    line("ERROR", message);
}

/// Python's `repr` of an optional string in a log line: `None` or the text as `%s`.
#[must_use]
pub fn or_none(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_owned(), str::to_owned)
}

/// `%s` of a bool.
#[must_use]
pub fn py_bool(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}
