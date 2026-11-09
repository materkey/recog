//! JSON the way Python's `json` module reads and writes it.
//!
//! The storage files are written by `json.dumps`; for the Rust port to write the same
//! bytes the document keeps what Python's dict keeps (insertion order, a repeated key
//! taking the first key's place and the last value), integers apart from floats, and
//! floats printed by `repr`. `loads` accepts what `json.loads` accepts, `NaN` and
//! `Infinity` included, and fails with its messages.
//!
//! Known differences: integers beyond i128 fail to parse (Python keeps them), and a
//! lone surrogate escape (`"\ud800"`) becomes U+FFFD, since a Rust string cannot hold
//! it.

use std::fmt::Write as _;

use crate::py;

/// A JSON value as Python's `json.loads` returns it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Map),
}

/// A dict: keys in insertion order, each once.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Map(Vec<(String, Value)>);

impl Map {
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// `d[key] = value`: an existing key keeps its place.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        match self.get_mut(&key) {
            Some(slot) => *slot = value,
            None => self.0.push((key, value)),
        }
    }

    /// `d.pop(key, None)`.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let at = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(at).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Value)> {
        self.0.iter_mut().map(|(k, v)| (&*k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.0.iter().map(|(_, v)| v)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `d.setdefault(key, default)`.
    pub fn setdefault(&mut self, key: &str, default: Value) -> &mut Value {
        if !self.contains_key(key) {
            self.0.push((key.to_owned(), default));
        }
        self.get_mut(key).unwrap_or_else(|| unreachable!("the key was just inserted"))
    }

    /// Move a key to the end, as `OrderedDict.move_to_end`.
    pub fn move_to_end(&mut self, key: &str) {
        if let Some(at) = self.0.iter().position(|(k, _)| k == key) {
            let entry = self.0.remove(at);
            self.0.push(entry);
        }
    }

    /// Remove the first entry, as `OrderedDict.popitem(last=False)`.
    pub fn pop_first(&mut self) -> Option<(String, Value)> {
        if self.0.is_empty() { None } else { Some(self.0.remove(0)) }
    }
}

impl FromIterator<(String, Value)> for Map {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::Str(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::Str(s)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Self::Int(i128::from(n))
    }
}

impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Self::Object(m)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(o: Option<T>) -> Self {
        o.map_or(Self::Null, Into::into)
    }
}

impl Value {
    #[must_use]
    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Self::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Self::Object(m) => Some(m),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Python's truth value.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Int(n) => *n != 0,
            Self::Float(x) => *x != 0.0,
            Self::Str(s) => !s.is_empty(),
            Self::Array(a) => !a.is_empty(),
            Self::Object(m) => !m.is_empty(),
        }
    }

    /// The name of the Python type, for error messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "NoneType",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Str(_) => "str",
            Self::Array(_) => "list",
            Self::Object(_) => "dict",
        }
    }

    /// `==` between Python values: numbers compare by value across int, float and
    /// bool.
    #[must_use]
    pub fn py_eq(&self, other: &Self) -> bool {
        match (self.as_number(), other.as_number()) {
            (Some(a), Some(b)) => a == b,
            _ => match (self, other) {
                (Self::Array(a), Self::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.py_eq(y)),
                (Self::Object(a), Self::Object(b)) => {
                    a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.py_eq(w)))
                }
                _ => self == other,
            },
        }
    }

    fn as_number(&self) -> Option<f64> {
        match self {
            Self::Bool(b) => Some(f64::from(u8::from(*b))),
            #[allow(clippy::cast_precision_loss)]
            Self::Int(n) => Some(*n as f64),
            Self::Float(x) => Some(*x),
            _ => None,
        }
    }

    /// `str(value)`.
    #[must_use]
    pub fn py_str(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            other => other.py_repr(),
        }
    }

    /// `repr(value)`.
    #[must_use]
    pub fn py_repr(&self) -> String {
        match self {
            Self::Null => "None".into(),
            Self::Bool(true) => "True".into(),
            Self::Bool(false) => "False".into(),
            Self::Int(n) => n.to_string(),
            Self::Float(x) => py::float_repr(*x),
            Self::Str(s) => py::repr(s),
            Self::Array(a) => format!("[{}]", a.iter().map(Self::py_repr).collect::<Vec<_>>().join(", ")),
            Self::Object(m) => format!(
                "{{{}}}",
                m.iter().map(|(k, v)| format!("{}: {}", py::repr(k), v.py_repr())).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// A `json.JSONDecodeError`: its message, as `str(error)` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

/// `json.loads(text)` of a `str`.
///
/// # Errors
/// What `json.loads` raises, with its message.
pub fn loads(text: &str) -> Result<Value, DecodeError> {
    let chars: Vec<char> = text.chars().collect();
    if chars.first() == Some(&'\u{feff}') {
        // json.loads refuses a str that starts with a BOM, with no position.
        return Err(DecodeError("Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)".into()));
    }
    let mut parser = Parser { chars: &chars, at: 0 };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.at != chars.len() {
        return Err(parser.error("Extra data", parser.at));
    }
    Ok(value)
}

/// `json.loads(raw)` of `bytes`, which strips a UTF-8 BOM.
///
/// # Errors
/// A decoding failure (`UnicodeDecodeError`, which Python's callers handle as a
/// `ValueError`) or what [`loads`] raises.
pub fn loads_bytes(raw: &[u8]) -> Result<Value, DecodeError> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    let text = std::str::from_utf8(raw).map_err(|e| DecodeError(utf8_error_message(raw, &e)))?;
    loads(text)
}

/// `str(UnicodeDecodeError)` of decoding `raw` as UTF-8.
#[must_use]
pub fn utf8_error_message(raw: &[u8], error: &std::str::Utf8Error) -> String {
    let at = error.valid_up_to();
    let (count, reason) = match error.error_len() {
        None => (raw.len() - at, "unexpected end of data"),
        Some(n) => {
            let start = raw[at];
            let invalid_start = matches!(start, 0x80..=0xC1 | 0xF5..=0xFF);
            (n, if invalid_start { "invalid start byte" } else { "invalid continuation byte" })
        }
    };
    if count == 1 {
        format!("'utf-8' codec can't decode byte 0x{:02x} in position {at}: {reason}", raw[at])
    } else {
        format!("'utf-8' codec can't decode bytes in position {at}-{}: {reason}", at + count - 1)
    }
}

/// The character of a one-letter JSON escape.
fn simple_escape(e: char) -> Option<char> {
    [('"', '"'), ('\\', '\\'), ('/', '/'), ('b', '\u{8}'), ('f', '\u{c}'), ('n', '\n'), ('r', '\r'), ('t', '\t')]
        .into_iter()
        .find(|(letter, _)| *letter == e)
        .map(|(_, c)| c)
}

struct Parser<'a> {
    chars: &'a [char],
    at: usize,
}

impl Parser<'_> {
    fn error(&self, message: &str, pos: usize) -> DecodeError {
        let line = self.chars[..pos].iter().filter(|&&c| c == '\n').count() + 1;
        let column = match self.chars[..pos].iter().rposition(|&c| c == '\n') {
            Some(newline) => pos - newline,
            None => pos + 1,
        };
        DecodeError(format!("{message}: line {line} column {column} (char {pos})"))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.at += 1;
        }
    }

    fn starts_with(&self, word: &str) -> bool {
        (self.at..).zip(word.chars()).all(|(i, c)| self.chars.get(i) == Some(&c))
    }

    fn value(&mut self) -> Result<Value, DecodeError> {
        let start = self.at;
        if let Some((length, value)) = self.literal() {
            self.at += length;
            return Ok(value);
        }
        match self.peek() {
            Some('"') => {
                self.at += 1;
                self.string(start).map(Value::Str)
            }
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('-' | '0'..='9') => self.number(start),
            _ => Err(self.error("Expecting value", start)),
        }
    }

    /// `null`, `true`, `false` and Python's `NaN`, `Infinity` and `-Infinity` at the
    /// position, with their length.
    fn literal(&self) -> Option<(usize, Value)> {
        [
            ("null", Value::Null),
            ("true", Value::Bool(true)),
            ("false", Value::Bool(false)),
            ("NaN", Value::Float(f64::NAN)),
            ("Infinity", Value::Float(f64::INFINITY)),
            ("-Infinity", Value::Float(f64::NEG_INFINITY)),
        ]
        .into_iter()
        .find(|(word, _)| self.starts_with(word))
        .map(|(word, value)| (word.len(), value))
    }

    fn number(&mut self, start: usize) -> Result<Value, DecodeError> {
        let (end, float) = self.number_extent(start)?;
        let text: String = self.chars[start..end].iter().collect();
        self.at = end;
        if float {
            Ok(Value::Float(text.parse().unwrap_or(f64::NAN)))
        } else {
            text.parse::<i128>().map(Value::Int).map_err(|_| self.error("Integer out of range", start))
        }
    }

    /// Where the number at `start` ends, and whether it is a float.
    fn number_extent(&self, start: usize) -> Result<(usize, bool), DecodeError> {
        // NUMBER_RE: (-?(?:0|[1-9]\d*))(\.\d+)?([eE][-+]?\d+)?
        let mut i = start;
        if self.chars[i] == '-' {
            i += 1;
        }
        match self.chars.get(i) {
            Some('0') => i += 1,
            Some('1'..='9') => i = self.digits_end(i),
            _ => return Err(self.error("Expecting value", start)),
        }
        let (i, fraction) = self.fraction_end(i);
        let (i, exponent) = self.exponent_end(i);
        Ok((i, fraction || exponent))
    }

    fn digits_end(&self, mut i: usize) -> usize {
        while self.chars.get(i).is_some_and(char::is_ascii_digit) {
            i += 1;
        }
        i
    }

    /// `(\.\d+)?` at `i`: where it ends, and whether it is there.
    fn fraction_end(&self, i: usize) -> (usize, bool) {
        if self.chars.get(i) == Some(&'.') && self.chars.get(i + 1).is_some_and(char::is_ascii_digit) {
            (self.digits_end(i + 1), true)
        } else {
            (i, false)
        }
    }

    /// `([eE][-+]?\d+)?` at `i`: where it ends, and whether it is there.
    fn exponent_end(&self, i: usize) -> (usize, bool) {
        if !matches!(self.chars.get(i), Some('e' | 'E')) {
            return (i, false);
        }
        let mut j = i + 1;
        if matches!(self.chars.get(j), Some('+' | '-')) {
            j += 1;
        }
        if self.chars.get(j).is_some_and(char::is_ascii_digit) { (self.digits_end(j), true) } else { (i, false) }
    }

    fn string(&mut self, start: usize) -> Result<String, DecodeError> {
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("Unterminated string starting at", start));
            };
            match c {
                '"' => {
                    self.at += 1;
                    return Ok(out);
                }
                '\\' => out.push(self.escape(start)?),
                c if u32::from(c) < 0x20 => return Err(self.error("Invalid control character at", self.at)),
                c => {
                    out.push(c);
                    self.at += 1;
                }
            }
        }
    }

    /// The character the backslash escape at the position stands for.
    fn escape(&mut self, start: usize) -> Result<char, DecodeError> {
        let escape_at = self.at;
        self.at += 1;
        let Some(e) = self.peek() else {
            return Err(self.error("Unterminated string starting at", start));
        };
        self.at += 1;
        match e {
            'u' => self.unicode_escape(escape_at),
            _ => simple_escape(e).ok_or_else(|| self.error("Invalid \\escape", escape_at)),
        }
    }

    /// `\uXXXX`, a high surrogate joined with the low one after it; a lone surrogate
    /// becomes U+FFFD.
    fn unicode_escape(&mut self, escape_at: usize) -> Result<char, DecodeError> {
        let first = self.hex4(escape_at)?;
        let code = if (0xD800..0xDC00).contains(&first)
            && self.chars.get(self.at) == Some(&'\\')
            && self.chars.get(self.at + 1) == Some(&'u')
        {
            self.low_surrogate(first)?
        } else {
            first
        };
        Ok(char::from_u32(code).unwrap_or('\u{fffd}'))
    }

    /// The code point of `first` and the `\uXXXX` after it, or `first` alone (the
    /// escape after it left unread) when that is not a low surrogate.
    fn low_surrogate(&mut self, first: u32) -> Result<u32, DecodeError> {
        let saved = self.at;
        self.at += 2;
        let second = self.hex4(saved)?;
        if (0xDC00..0xE000).contains(&second) {
            Ok(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00))
        } else {
            self.at = saved;
            Ok(first)
        }
    }

    fn hex4(&mut self, escape_at: usize) -> Result<u32, DecodeError> {
        let digits: String = self.chars.iter().skip(self.at).take(4).collect();
        let valid = digits.len() == 4 && digits.chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return Err(self.error("Invalid \\uXXXX escape", escape_at + 1));
        }
        self.at += 4;
        u32::from_str_radix(&digits, 16).map_err(|_| self.error("Invalid \\uXXXX escape", escape_at))
    }

    fn object(&mut self) -> Result<Value, DecodeError> {
        self.at += 1;
        let mut map = Map::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.at += 1;
            return Ok(Value::Object(map));
        }
        loop {
            if self.peek() != Some('"') {
                return Err(self.error("Expecting property name enclosed in double quotes", self.at));
            }
            let key_at = self.at;
            self.at += 1;
            let key = self.string(key_at)?;
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err(self.error("Expecting ':' delimiter", self.at));
            }
            self.at += 1;
            self.skip_ws();
            let value = self.value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.at += 1;
                    return Ok(Value::Object(map));
                }
                Some(',') => {
                    self.at += 1;
                    self.skip_ws();
                }
                _ => return Err(self.error("Expecting ',' delimiter", self.at)),
            }
        }
    }

    fn array(&mut self) -> Result<Value, DecodeError> {
        self.at += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.at += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(']') => {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                Some(',') => {
                    self.at += 1;
                    self.skip_ws();
                }
                _ => return Err(self.error("Expecting ',' delimiter", self.at)),
            }
        }
    }
}

/// `json.dumps(value, ensure_ascii=False)`, or with `indent` as `indent=n`.
#[must_use]
pub fn dumps(value: &Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    write_value(&mut out, value, indent, 0);
    out
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        if let Some(escaped) = short_escape(c) {
            out.push_str(escaped);
        } else if u32::from(c) < 0x20 {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out.push('"');
}

/// The two-character escape `json.dumps` writes for `c`, if it has one.
fn short_escape(c: char) -> Option<&'static str> {
    [('"', "\\\""), ('\\', "\\\\"), ('\n', "\\n"), ('\r', "\\r"), ('\t', "\\t"), ('\u{8}', "\\b"), ('\u{c}', "\\f")]
        .into_iter()
        .find(|(plain, _)| *plain == c)
        .map(|(_, escaped)| escaped)
}

fn write_float(out: &mut String, x: f64) {
    if x.is_nan() {
        out.push_str("NaN");
    } else if x.is_infinite() {
        out.push_str(if x > 0.0 { "Infinity" } else { "-Infinity" });
    } else {
        out.push_str(&py::float_repr(x));
    }
}

fn newline(out: &mut String, indent: usize, level: usize) {
    out.push('\n');
    out.push_str(&" ".repeat(indent * level));
}

fn write_value(out: &mut String, value: &Value, indent: Option<usize>, level: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(x) => write_float(out, *x),
        Value::Str(s) => write_string(out, s),
        Value::Array(items) => write_array(out, items, indent, level),
        Value::Object(map) => write_object(out, map, indent, level),
    }
}

fn write_array(out: &mut String, items: &[Value], indent: Option<usize>, level: usize) {
    if items.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        write_separator(out, i, indent, level);
        write_value(out, item, indent, level + 1);
    }
    write_close(out, ']', indent, level);
}

fn write_object(out: &mut String, map: &Map, indent: Option<usize>, level: usize) {
    if map.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push('{');
    for (i, (k, v)) in map.iter().enumerate() {
        write_separator(out, i, indent, level);
        write_string(out, k);
        out.push_str(": ");
        write_value(out, v, indent, level + 1);
    }
    write_close(out, '}', indent, level);
}

/// Before the `i`th item of a container: the comma after the one before, then the
/// item's own line when indenting.
fn write_separator(out: &mut String, i: usize, indent: Option<usize>, level: usize) {
    if i > 0 {
        out.push_str(if indent.is_some() { "," } else { ", " });
    }
    if let Some(n) = indent {
        newline(out, n, level + 1);
    }
}

fn write_close(out: &mut String, bracket: char, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        newline(out, n, level);
    }
    out.push(bracket);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_read_and_written_again_keeps_order_numbers_and_unicode() {
        // given
        let text = r#"{"b": 1, "a": [1.0, 2.5e-7, true, null], "c": "Привет \"q\"\n", "d": {}}"#;
        // when
        let value = loads(text).unwrap();
        // then
        assert_eq!(dumps(&value, None), r#"{"b": 1, "a": [1.0, 2.5e-07, true, null], "c": "Привет \"q\"\n", "d": {}}"#);
        assert_eq!(
            dumps(&value, Some(2)),
            "{\n  \"b\": 1,\n  \"a\": [\n    1.0,\n    2.5e-07,\n    true,\n    null\n  ],\n  \"c\": \"Привет \\\"q\\\"\\n\",\n  \"d\": {}\n}"
        );
    }

    #[test]
    fn a_repeated_key_keeps_the_first_place_and_the_last_value() {
        // given / when
        let value = loads(r#"{"a": 1, "b": 2, "a": 3}"#).unwrap();
        // then
        assert_eq!(dumps(&value, None), r#"{"a": 3, "b": 2}"#);
    }

    #[test]
    fn malformed_documents_fail_with_python_messages() {
        // given / when / then
        for (text, message) in [
            ("", "Expecting value: line 1 column 1 (char 0)"),
            ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
            ("{\"a\": 1,}", "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"),
            ("\"\\u12\"", "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ("{\"a\": 1\n x", "Expecting ',' delimiter: line 2 column 2 (char 9)"),
            ("\"abc", "Unterminated string starting at: line 1 column 1 (char 0)"),
            ("{} x", "Extra data: line 1 column 4 (char 3)"),
            ("\"a\u{1}\"", "Invalid control character at: line 1 column 3 (char 2)"),
        ] {
            assert_eq!(loads(text).unwrap_err().0, message, "{text:?}");
        }
    }

    #[test]
    fn every_character_json_escapes_when_written_then_escaped_as_python_does() {
        // given
        let value = Value::from("\"\\\n\r\t\u{8}\u{c}\u{1f}é/");
        // when / then
        assert_eq!(dumps(&value, None), "\"\\\"\\\\\\n\\r\\t\\b\\f\\u001fé/\"");
    }

    #[test]
    fn escapes_when_read_then_decoded_and_broken_ones_fail_with_python_messages() {
        // given / when / then
        assert_eq!(loads(r#""\"\\\/\b\f\n\r\t\u00e9""#), Ok(Value::from("\"\\/\u{8}\u{c}\n\r\té")));
        assert_eq!(loads(r#""\ud83d\u0041""#), Ok(Value::from("\u{fffd}A")));
        assert_eq!(loads(r#""\ud83d""#), Ok(Value::from("\u{fffd}")));
        for (text, message) in [
            (r#""\x""#, "Invalid \\escape: line 1 column 2 (char 1)"),
            ("\"\\", "Unterminated string starting at: line 1 column 1 (char 0)"),
            (r#""\ud83d\u12""#, "Invalid \\uXXXX escape: line 1 column 9 (char 8)"),
        ] {
            assert_eq!(loads(text).unwrap_err().0, message, "{text:?}");
        }
    }

    #[test]
    fn numbers_and_literals_when_read_then_ints_floats_and_python_constants() {
        // given
        let text = "[NaN, Infinity, -Infinity, -0, 0.5e+3, 1E2, 2e-1, 12, -7, 0]";
        // when
        let value = loads(text).unwrap();
        // then
        assert_eq!(dumps(&value, None), "[NaN, Infinity, -Infinity, 0, 500.0, 100.0, 0.2, 12, -7, 0]");
        for (text, message) in [
            ("[1.]", "Expecting ',' delimiter: line 1 column 3 (char 2)"),
            ("1e", "Extra data: line 1 column 2 (char 1)"),
            ("-", "Expecting value: line 1 column 1 (char 0)"),
            ("nul", "Expecting value: line 1 column 1 (char 0)"),
            ("-Inf", "Expecting value: line 1 column 1 (char 0)"),
            ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
            ("123456789012345678901234567890123456789012", "Integer out of range: line 1 column 1 (char 0)"),
        ] {
            assert_eq!(loads(text).unwrap_err().0, message, "{text:?}");
        }
    }

    #[test]
    fn values_when_shown_with_repr_then_python_literals() {
        // given
        let value = loads(r#"{"a": [null, true, false, 1, 2.5, "it's"], "b": {}}"#).unwrap();
        // when / then
        assert_eq!(value.py_repr(), "{'a': [None, True, False, 1, 2.5, \"it's\"], 'b': {}}");
        assert_eq!(value.py_str(), value.py_repr());
        assert_eq!(Value::from("x").py_str(), "x");
    }

    #[test]
    fn values_when_compared_then_numbers_by_value_and_containers_item_by_item() {
        // given
        let v = |text: &str| loads(text).unwrap();
        // when / then
        for (a, b, equal) in [
            ("1", "1.0", true),
            ("true", "1", true),
            ("[1, 2]", "[1.0, 2]", true),
            ("[1]", "[1, 2]", false),
            (r#"{"a": 1}"#, r#"{"a": 1.0}"#, true),
            (r#"{"a": 1}"#, r#"{"b": 1}"#, false),
            (r#"{"a": 1}"#, r#"{"a": 1, "b": 2}"#, false),
            (r#""1""#, "1", false),
            ("null", "null", true),
            (r#""a""#, r#""a""#, true),
        ] {
            assert_eq!(v(a).py_eq(&v(b)), equal, "{a} == {b}");
        }
    }

    #[test]
    fn surrogate_pairs_and_control_characters_round_trip() {
        // given / when
        let value = loads(r#""\ud83d\ude00\u0001""#).unwrap();
        // then
        assert_eq!(value, Value::Str("😀\u{1}".into()));
        assert_eq!(dumps(&value, None), "\"😀\\u0001\"");
    }
}
