//! Form bodies the way FastAPI receives them: Starlette's `request.form()` (1.0) over
//! python-multipart 0.0.33, the version a fresh install of the requirements gets (an
//! older venv, 0.0.26, still reads RFC 2231 parameters and keeps the case of a media
//! type with parameters).
//!
//! [`parse`] reads `multipart/form-data` with a port of python-multipart's
//! `MultipartParser` state machine and Starlette's `MultiPartParser` callbacks (their
//! limits and messages included), `application/x-www-form-urlencoded` with a port of
//! `QuerystringParser` and Starlette's `FormParser`, and anything else as an empty
//! form. The body arrives whole: the Python parsers are written to give the same
//! result whatever the chunking.
//!
//! An error is the `detail` of the 400 answer: Starlette's own message for what it
//! checks (a missing boundary, a part without a name, a field over 1 MiB, more than
//! 1000 fields or files), [`PARSE_ERROR`] for what python-multipart refuses (its parse
//! errors, more than 8 headers in a part, a header over 4224 bytes, a boundary over
//! 256 bytes).
//!
//! Known differences: values are decoded with the request's `charset` only when it
//! names UTF-8; any other charset decodes as Latin-1 (Python would use that codec when
//! it knows it, e.g. cp1251).

/// FastAPI's answer when parsing the body raised anything but Starlette's own error.
pub const PARSE_ERROR: &str = "There was an error parsing the body";

use recog_core::py;

/// Starlette's limits of `request.form()`.
const MAX_FILES: usize = 1000;
const MAX_FIELDS: usize = 1000;
const MAX_PART_SIZE: usize = 1024 * 1024;

/// A file part: Starlette's `UploadFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadFile {
    /// The `filename` parameter of the part's `Content-Disposition`, decoded.
    pub filename: String,
    /// The part's first `Content-Type` header, as Latin-1.
    pub content_type: Option<String>,
    /// The part's content.
    pub data: Vec<u8>,
}

/// One value of a form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormValue {
    Text(String),
    File(UploadFile),
}

/// Starlette's `FormData`: the items in order, a name possibly repeated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormData {
    items: Vec<(String, FormValue)>,
}

impl FormData {
    /// `form.get(name)`: the last value of that name, as `ImmutableMultiDict` keeps it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&FormValue> {
        self.items.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// The items in order.
    #[must_use]
    pub fn items(&self) -> &[(String, FormValue)] {
        &self.items
    }
}

/// `bytes.decode("latin-1")`.
#[must_use]
pub fn latin1(raw: &[u8]) -> String {
    raw.iter().map(|&b| char::from(b)).collect()
}

/// `str.encode("latin-1")` of a string made by [`latin1`] (or of its Latin-1 slices).
fn latin1_bytes(text: &str) -> Vec<u8> {
    text.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect()
}

/// `encodings.normalize_encoding` plus the alias table, for the UTF-8 names only.
fn is_utf8_codec(name: &str) -> bool {
    let mut normalized = String::new();
    let mut pending = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            if pending && !normalized.is_empty() {
                normalized.push('_');
            }
            normalized.push(c.to_ascii_lowercase());
            pending = false;
        } else {
            pending = true;
        }
    }
    matches!(normalized.as_str(), "utf_8" | "utf8" | "u8" | "utf" | "utf8_ucs2" | "utf8_ucs4" | "cp65001")
}

/// Starlette's `_user_safe_decode`: the charset, or Latin-1 when it fails.
fn safe_decode(raw: &[u8], charset: &str) -> String {
    if is_utf8_codec(charset)
        && let Ok(text) = std::str::from_utf8(raw)
    {
        return text.to_owned();
    }
    latin1(raw)
}

// --- python-multipart's parameter parsing (`_parseparam`, vendored from email.message).

/// `s.find(sub, start, end)` with Python's index normalisation; -1 when absent.
fn py_find(s: &[char], sub: &[char], start: isize, end: isize) -> isize {
    let (start, end) = normalize(s.len(), start, end);
    if sub.is_empty() {
        return if start <= end { start as isize } else { -1 };
    }
    let mut i = start;
    while i + sub.len() <= end {
        if &s[i..i + sub.len()] == sub {
            return i as isize;
        }
        i += 1;
    }
    -1
}

/// `s.count(sub, start, end)` with Python's index normalisation.
fn py_count(s: &[char], sub: &[char], start: isize, end: isize) -> isize {
    let (start, end) = normalize(s.len(), start, end);
    let mut count = 0;
    let mut i = start;
    while i + sub.len() <= end {
        if &s[i..i + sub.len()] == sub {
            count += 1;
            i += sub.len();
        } else {
            i += 1;
        }
    }
    count
}

fn normalize(len: usize, start: isize, end: isize) -> (usize, usize) {
    let len = len as isize;
    let clamp = |x: isize| if x < 0 { (x + len).max(0) } else { x.min(len) };
    let (start, end) = (clamp(start), clamp(end));
    (start as usize, end.max(start) as usize)
}

/// `s[start:end]`.
fn slice(s: &[char], start: isize, end: isize) -> String {
    let (start, end) = normalize(s.len(), start, end);
    s[start..end].iter().collect()
}

/// `_parseparam`.
fn parseparam(value: &str) -> Vec<String> {
    let s: Vec<char> = std::iter::once(';').chain(value.chars()).collect();
    let len = s.len() as isize;
    let mut plist = Vec::new();
    let mut start: isize = 0;
    while py_find(&s, &[';'], start, len) == start {
        start += 1;
        let mut end = py_find(&s, &[';'], start, len);
        let (mut ind, mut diff) = (start, 0isize);
        while end > 0 {
            diff += py_count(&s, &['"'], ind, end) - py_count(&s, &['\\', '"'], ind, end);
            if diff % 2 == 0 {
                break;
            }
            let next = py_find(&s, &[';'], end + 1, len);
            end = ind;
            ind = next;
        }
        if end < 0 {
            end = len;
        }
        let i = py_find(&s, &['='], start, end);
        let f = if i == -1 {
            slice(&s, start, end)
        } else {
            let name = slice(&s, start, i);
            let value = slice(&s, i + 1, end);
            format!("{}={}", name.trim_end_matches(py::is_space).to_lowercase(), value.trim_start_matches(py::is_space))
        };
        plist.push(py::strip(&f).to_owned());
        start = end;
    }
    plist
}

/// `urllib.parse.unquote(s)`: runs of `%XX` decoded as UTF-8 with replacement.
pub(crate) fn unquote_utf8(s: &str) -> String {
    percent_decode(s, |bytes| String::from_utf8_lossy(bytes).into_owned())
}

fn percent_decode(s: &str, decode: impl Fn(&[u8]) -> String) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let hex = |c: char| c.to_digit(16).and_then(|d| u8::try_from(d).ok());
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut run: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%'
            && let (Some(&a), Some(&b)) = (chars.get(i + 1), chars.get(i + 2))
            && let (Some(a), Some(b)) = (hex(a), hex(b))
        {
            run.push(a * 16 + b);
            i += 3;
            continue;
        }
        if !run.is_empty() {
            out.push_str(&decode(&run));
            run.clear();
        }
        out.push(chars[i]);
        i += 1;
    }
    if !run.is_empty() {
        out.push_str(&decode(&run));
    }
    out
}

/// python-multipart's `parse_options_header` of a Latin-1 header value: the value,
/// lowercased, and its parameters (a repeated name keeps the last value). Extended
/// parameters (`name*=`, `name*0`) are left out, as RFC 7578 forbids them.
fn parse_options_header(value: &str) -> (String, Vec<(String, String)>) {
    if value.is_empty() {
        return (String::new(), Vec::new());
    }
    if !value.contains(';') {
        return (py::strip(&value.to_lowercase()).to_owned(), Vec::new());
    }
    let mut segments = parseparam(value).into_iter();
    let ctype = segments.next().unwrap_or_default().to_lowercase();
    let mut options: Vec<(String, String)> = Vec::new();
    for segment in segments {
        let (key, mut val) = match segment.split_once('=') {
            Some((key, val)) => (key.to_owned(), val.to_owned()),
            None => (segment.clone(), String::new()),
        };
        if key.contains('*') {
            continue;
        }
        if val.chars().count() >= 2 && val.starts_with('"') && val.ends_with('"') {
            val = val[1..val.len() - 1].replace(r"\\", r"\").replace(r#"\""#, "\"");
        }
        if key == "filename" {
            let chars: Vec<char> = val.chars().collect();
            if chars.get(1..3) == Some(&[':', '\\'][..]) || chars.get(..2) == Some(&['\\', '\\'][..]) {
                val = val.rsplit('\\').next().unwrap_or_default().to_owned();
            }
        }
        match options.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = val,
            None => options.push((key, val)),
        }
    }
    (ctype, options)
}

fn option<'a>(options: &'a [(String, String)], key: &str) -> Option<&'a str> {
    options.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Parses a request body by its `Content-Type` header (the first one, raw bytes).
///
/// # Errors
/// The `detail` of the 400 answer.
pub fn parse(content_type: Option<&[u8]>, body: &[u8]) -> Result<FormData, String> {
    let header = content_type.map(latin1).unwrap_or_default();
    let (ctype, options) = parse_options_header(&header);
    match ctype.as_str() {
        "multipart/form-data" => {
            let charset = option(&options, "charset").unwrap_or("utf-8").to_owned();
            let Some(boundary) = option(&options, "boundary") else {
                return Err("Missing boundary in multipart.".to_owned());
            };
            let mut parts = Parts::new(charset);
            match run_multipart(&latin1_bytes(boundary), body, &mut parts) {
                Ok(()) => Ok(FormData { items: parts.items }),
                Err(Failure::Starlette(message)) => Err(message),
                Err(Failure::Parse) => Err(PARSE_ERROR.to_owned()),
            }
        }
        "application/x-www-form-urlencoded" => Ok(FormData { items: querystring(body) }),
        _ => Ok(FormData::default()),
    }
}

/// How the multipart parse stopped.
enum Failure {
    /// Starlette's `MultiPartException`: its message is the answer's detail.
    Starlette(String),
    /// python-multipart's `MultipartParseError` (or anything else raised).
    Parse,
}

/// Starlette's `MultiPartParser` callbacks and what they collect.
struct Parts {
    charset: String,
    items: Vec<(String, FormValue)>,
    files: usize,
    fields: usize,
    header_name: Vec<u8>,
    header_value: Vec<u8>,
    disposition: Option<Vec<u8>>,
    headers: Vec<(Vec<u8>, Vec<u8>)>,
    name: String,
    data: Vec<u8>,
    file: Option<UploadFile>,
}

impl Parts {
    fn new(charset: String) -> Self {
        Self {
            charset,
            items: Vec::new(),
            files: 0,
            fields: 0,
            header_name: Vec::new(),
            header_value: Vec::new(),
            disposition: None,
            headers: Vec::new(),
            name: String::new(),
            data: Vec::new(),
            file: None,
        }
    }

    fn part_begin(&mut self) {
        self.disposition = None;
        self.headers.clear();
        self.name.clear();
        self.data.clear();
        self.file = None;
    }

    fn part_data(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        match &mut self.file {
            None => {
                if self.data.len() + bytes.len() > MAX_PART_SIZE {
                    return Err(Failure::Starlette(format!(
                        "Part exceeded maximum size of {}KB.",
                        MAX_PART_SIZE / 1024
                    )));
                }
                self.data.extend_from_slice(bytes);
            }
            Some(file) => file.data.extend_from_slice(bytes),
        }
        Ok(())
    }

    fn part_end(&mut self) {
        let name = std::mem::take(&mut self.name);
        match self.file.take() {
            None => {
                let value = safe_decode(&self.data, &self.charset);
                self.items.push((name, FormValue::Text(value)));
            }
            Some(file) => self.items.push((name, FormValue::File(file))),
        }
        self.data.clear();
    }

    fn header_end(&mut self) {
        let field = self.header_name.to_ascii_lowercase();
        let value = std::mem::take(&mut self.header_value);
        if field == b"content-disposition" {
            self.disposition = Some(value.clone());
        }
        self.headers.push((field, value));
        self.header_name.clear();
    }

    fn headers_finished(&mut self) -> Result<(), Failure> {
        let disposition = self.disposition.as_deref().map(latin1).unwrap_or_default();
        let (_, options) = parse_options_header(&disposition);
        let Some(name) = option(&options, "name") else {
            return Err(Failure::Starlette(
                "The Content-Disposition header field \"name\" must be provided.".to_owned(),
            ));
        };
        self.name = safe_decode(&latin1_bytes(name), &self.charset);
        if let Some(filename) = option(&options, "filename") {
            self.files += 1;
            if self.files > MAX_FILES {
                return Err(Failure::Starlette(format!("Too many files. Maximum number of files is {MAX_FILES}.")));
            }
            let content_type = self.headers.iter().find(|(k, _)| k == b"content-type").map(|(_, v)| latin1(v));
            self.file = Some(UploadFile {
                filename: safe_decode(&latin1_bytes(filename), &self.charset),
                content_type,
                data: Vec::new(),
            });
        } else {
            self.fields += 1;
            if self.fields > MAX_FIELDS {
                return Err(Failure::Starlette(format!("Too many fields. Maximum number of fields is {MAX_FIELDS}.")));
            }
            self.file = None;
        }
        Ok(())
    }
}

/// HTTP token characters (RFC 7230 3.2.6), python-multipart's `TOKEN_CHARS_SET`.
fn is_token(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)
}

const CR: u8 = b'\r';
const LF: u8 = b'\n';
const HYPHEN: u8 = b'-';

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    StartBoundary,
    HeaderFieldStart,
    HeaderField,
    HeaderValueStart,
    HeaderValue,
    HeaderValueAlmostDone,
    HeadersAlmostDone,
    PartDataStart,
    PartData,
    EndBoundary,
    End,
}

/// python-multipart's limits of one part's headers.
const MAX_HEADER_COUNT: usize = 8;
const MAX_HEADER_SIZE: usize = 4096 + 128;
const MAX_BOUNDARY_LENGTH: usize = 256;

/// python-multipart's `MultipartParser._internal_write` over the whole body, the
/// callbacks going to `parts`.
fn run_multipart(boundary: &[u8], data: &[u8], parts: &mut Parts) -> Result<(), Failure> {
    if boundary.len() > MAX_BOUNDARY_LENGTH {
        return Err(Failure::Parse);
    }
    let boundary: Vec<u8> = [b"\r\n--".as_slice(), boundary].concat();
    let blen = boundary.len();
    let length = data.len();
    let mut state = State::Start;
    let mut index: usize = 0;
    let (mut part_boundary, mut last_boundary) = (false, false);
    let (mut header_count, mut header_size) = (0usize, 0usize);
    let mut header_mark: Option<usize> = None;
    let mut value_mark: Option<usize> = None;
    let mut data_mark: Option<usize> = None;
    let mut i: usize = 0;
    let advance = |size: &mut usize, amount: usize| -> Result<(), Failure> {
        *size += amount;
        if *size > MAX_HEADER_SIZE { Err(Failure::Parse) } else { Ok(()) }
    };

    while i < length {
        let mut c = data[i];
        // `i -= 1` of the Python loop: the same byte again in the next state.
        let mut again = false;
        match state {
            State::Start => {
                if c == CR || c == LF {
                    match data[i..].iter().position(|&b| b == HYPHEN) {
                        Some(at) => {
                            i += at;
                            continue;
                        }
                        None => break,
                    }
                }
                index = 0;
                state = State::StartBoundary;
                again = true;
            }
            State::StartBoundary => {
                if index == 0 && data[i..].starts_with(&boundary[2..]) {
                    index = blen - 2;
                    i += index;
                    continue;
                }
                if index == blen - 2 {
                    if c == HYPHEN {
                        state = State::EndBoundary;
                    } else if c != CR {
                        return Err(Failure::Parse);
                    }
                    index += 1;
                } else if index == blen - 1 {
                    if c != LF {
                        return Err(Failure::Parse);
                    }
                    index = 0;
                    parts.part_begin();
                    (header_count, header_size) = (0, 0);
                    state = State::HeaderFieldStart;
                } else {
                    if c != boundary[index + 2] {
                        return Err(Failure::Parse);
                    }
                    index += 1;
                }
            }
            State::HeaderFieldStart => {
                index = 0;
                if c != CR {
                    header_count += 1;
                    if header_count > MAX_HEADER_COUNT {
                        return Err(Failure::Parse);
                    }
                    header_size = 0;
                }
                header_mark = Some(i);
                state = State::HeaderField;
                again = true;
            }
            State::HeaderField => {
                if c == CR && index == 0 {
                    header_mark = None;
                    state = State::HeadersAlmostDone;
                    i += 1;
                    continue;
                }
                let colon = data[i..].iter().position(|&b| b == b':').map(|p| i + p);
                let end = colon.unwrap_or(length);
                advance(&mut header_size, if colon.is_some() { end - i + 1 } else { end - i })?;
                if !data[i..end].iter().all(|&b| is_token(b)) {
                    return Err(Failure::Parse);
                }
                index += end - i;
                match colon {
                    None => {
                        i = length;
                        continue;
                    }
                    Some(colon) => {
                        if index == 0 {
                            return Err(Failure::Parse);
                        }
                        i = colon;
                        if let Some(r) = marked(&mut header_mark, i) {
                            parts.header_name.extend_from_slice(&data[r]);
                        }
                        state = State::HeaderValueStart;
                    }
                }
            }
            State::HeaderValueStart => {
                if c == b' ' {
                    advance(&mut header_size, 1)?;
                    i += 1;
                    continue;
                }
                value_mark = Some(i);
                state = State::HeaderValue;
                again = true;
            }
            State::HeaderValue => {
                let cr = data[i..].iter().position(|&b| b == CR).map(|p| i + p);
                let end = cr.unwrap_or(length);
                advance(&mut header_size, end - i)?;
                match cr {
                    Some(cr) => {
                        i = cr;
                        if let Some(r) = marked(&mut value_mark, i) {
                            parts.header_value.extend_from_slice(&data[r]);
                        }
                        parts.header_end();
                        header_size = 0;
                        state = State::HeaderValueAlmostDone;
                    }
                    None => {
                        i = length;
                        continue;
                    }
                }
            }
            State::HeaderValueAlmostDone => {
                if c != LF {
                    return Err(Failure::Parse);
                }
                state = State::HeaderFieldStart;
            }
            State::HeadersAlmostDone => {
                if c != LF {
                    return Err(Failure::Parse);
                }
                parts.headers_finished()?;
                state = State::PartDataStart;
            }
            State::PartDataStart => {
                data_mark = Some(i);
                state = State::PartData;
                again = true;
            }
            State::PartData => {
                let prev_index = index;
                if index == 0 {
                    match find(data, &boundary, i) {
                        Some(i0) => {
                            index = blen - 1;
                            i = i0 + blen - 1;
                            c = data[i];
                        }
                        None => {
                            // A boundary cut off by the end of this chunk.
                            let from = i.max((length + 1).saturating_sub(blen));
                            if let Some(k) = data[from..].iter().rposition(|&b| b == boundary[0]).map(|p| from + p)
                                && boundary.starts_with(&data[k..])
                            {
                                index = length - k;
                            }
                            i = length;
                            continue;
                        }
                    }
                }
                if index < blen {
                    if boundary[index] == c {
                        index += 1;
                    } else {
                        index = 0;
                    }
                } else if index == blen {
                    index += 1;
                    if c == CR {
                        part_boundary = true;
                    } else if c == HYPHEN {
                        last_boundary = true;
                    } else {
                        index = 0;
                    }
                } else if index == blen + 1 {
                    if part_boundary {
                        if c == LF {
                            part_boundary = false;
                            if let Some(r) = marked(&mut data_mark, i.saturating_sub(index)) {
                                parts.part_data(&data[r])?;
                            }
                            parts.part_end();
                            parts.part_begin();
                            (header_count, header_size) = (0, 0);
                            index = 0;
                            state = State::HeaderFieldStart;
                            i += 1;
                            continue;
                        }
                        index = 0;
                        part_boundary = false;
                    } else if last_boundary {
                        if c == HYPHEN {
                            if let Some(r) = marked(&mut data_mark, i.saturating_sub(index)) {
                                parts.part_data(&data[r])?;
                            }
                            parts.part_end();
                            state = State::End;
                        } else {
                            index = 0;
                        }
                    }
                }
                if index == 0 && prev_index > 0 {
                    again = true;
                }
            }
            State::EndBoundary => {
                if index == blen - 1 {
                    if c != HYPHEN {
                        return Err(Failure::Parse);
                    }
                    index += 1;
                    state = State::End;
                }
            }
            State::End => break,
        }
        if !again {
            i += 1;
        }
    }
    // The data of a field, a header or a part cut off by the end of the body.
    if let Some(r) = marked(&mut header_mark, length) {
        parts.header_name.extend_from_slice(&data[r]);
    }
    if let Some(r) = marked(&mut value_mark, length) {
        parts.header_value.extend_from_slice(&data[r]);
    }
    if let Some(r) = marked(&mut data_mark, length.saturating_sub(index)) {
        parts.part_data(&data[r])?;
    }
    Ok(())
}

/// `data_callback(name, end_i)`: the marked bytes up to `end`, the mark dropped.
fn marked(mark: &mut Option<usize>, end: usize) -> Option<std::ops::Range<usize>> {
    let start = mark.take()?;
    (end > start).then_some(start..end)
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() || needle.len() > haystack.len() - from {
        return None;
    }
    haystack[from..].windows(needle.len()).position(|w| w == needle).map(|at| from + at)
}

/// `urllib.parse.unquote_plus` of a Latin-1 string.
fn unquote_plus(text: &str) -> String {
    unquote_utf8(&text.replace('+', " "))
}

/// python-multipart's `QuerystringParser` (not strict) and Starlette's `FormParser`.
fn querystring(data: &[u8]) -> Vec<(String, FormValue)> {
    #[derive(PartialEq)]
    enum Q {
        Before,
        Name,
        Data,
    }
    let mut items = Vec::new();
    let (mut name, mut value): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let mut end_field = |name: &mut Vec<u8>, value: &mut Vec<u8>| {
        items.push((unquote_plus(&latin1(name)), FormValue::Text(unquote_plus(&latin1(value)))));
    };
    let sep = |from: usize| find(data, b"&", from);
    let length = data.len();
    let mut state = Q::Before;
    let mut i = 0;
    while i < length {
        let ch = data[i];
        match state {
            Q::Before => {
                if ch != b'&' {
                    name.clear();
                    value.clear();
                    state = Q::Name;
                    continue;
                }
            }
            Q::Name => {
                let sep_pos = sep(i);
                let equals = match sep_pos {
                    Some(s) => data[i..s].iter().position(|&b| b == b'=').map(|p| i + p),
                    None => data[i..].iter().position(|&b| b == b'=').map(|p| i + p),
                };
                if let Some(eq) = equals {
                    name.extend_from_slice(&data[i..eq]);
                    i = eq;
                    state = Q::Data;
                } else if let Some(s) = sep_pos {
                    name.extend_from_slice(&data[i..s]);
                    end_field(&mut name, &mut value);
                    i = s;
                    state = Q::Before;
                    continue;
                } else {
                    name.extend_from_slice(&data[i..]);
                    i = length;
                    continue;
                }
            }
            Q::Data => {
                if let Some(s) = sep(i) {
                    value.extend_from_slice(&data[i..s]);
                    end_field(&mut name, &mut value);
                    i = s;
                    state = Q::Before;
                    continue;
                }
                value.extend_from_slice(&data[i..]);
                i = length;
                continue;
            }
        }
        i += 1;
    }
    if state != Q::Before {
        end_field(&mut name, &mut value);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multipart(boundary: &str, body: &str) -> Result<FormData, String> {
        parse(Some(format!("multipart/form-data; boundary={boundary}").as_bytes()), body.as_bytes())
    }

    fn text_of(form: &FormData, name: &str) -> Option<String> {
        match form.get(name) {
            Some(FormValue::Text(t)) => Some(t.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_field_and_a_file_given_multipart_when_parsed_then_both_come_back() {
        let body = "--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngigaam\r\n\
                    --b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\n\
                    Content-Type: audio/wav\r\n\r\nRIFF\r\n--b--\r\n";
        let form = multipart("b", body).unwrap();
        assert_eq!(text_of(&form, "model").as_deref(), Some("gigaam"));
        let Some(FormValue::File(file)) = form.get("file") else { panic!("no file") };
        assert_eq!(file.filename, "a.wav");
        assert_eq!(file.content_type.as_deref(), Some("audio/wav"));
        assert_eq!(file.data, b"RIFF");
    }

    #[test]
    fn a_repeated_field_given_multipart_when_read_then_the_last_value_counts() {
        let body = "--b\r\nContent-Disposition: form-data; name=m\r\n\r\none\r\n\
                    --b\r\nContent-Disposition: form-data; name=m\r\n\r\ntwo\r\n--b--";
        assert_eq!(text_of(&multipart("b", body).unwrap(), "m").as_deref(), Some("two"));
    }

    #[test]
    fn no_boundary_given_multipart_when_parsed_then_starlettes_message() {
        assert_eq!(parse(Some(b"multipart/form-data"), b"x"), Err("Missing boundary in multipart.".to_owned()));
    }

    #[test]
    fn garbage_given_a_boundary_when_parsed_then_the_generic_parse_error() {
        assert_eq!(multipart("abc", "xx"), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn a_part_without_a_name_when_parsed_then_starlettes_message() {
        let body = "--b\r\nContent-Type: text/plain\r\n\r\nx\r\n--b--";
        assert_eq!(
            multipart("b", body),
            Err("The Content-Disposition header field \"name\" must be provided.".to_owned())
        );
    }

    #[test]
    fn an_unfinished_last_part_when_parsed_then_it_is_left_out() {
        let body = "--b\r\nContent-Disposition: form-data; name=a\r\n\r\n1\r\n\
                    --b\r\nContent-Disposition: form-data; name=c\r\n\r\n2";
        let form = multipart("b", body).unwrap();
        assert_eq!(text_of(&form, "a").as_deref(), Some("1"));
        assert!(form.get("c").is_none());
    }

    #[test]
    fn a_windows_path_as_filename_when_parsed_then_its_last_component() {
        let body = "--b\r\nContent-Disposition: form-data; name=f; filename=\"C:\\\\dir\\\\a.mp3\"\r\n\r\nx\r\n--b--";
        let Some(FormValue::File(file)) = multipart("b", body).unwrap().get("f").cloned() else { panic!() };
        assert_eq!(file.filename, "a.mp3");
    }

    #[test]
    fn an_rfc2231_filename_when_parsed_then_ignored_and_the_part_is_a_field() {
        let body = "--b\r\nContent-Disposition: form-data; name=f; filename*=UTF-8''%D0%B0.wav\r\n\r\nx\r\n--b--";
        assert_eq!(text_of(&multipart("b", body).unwrap(), "f").as_deref(), Some("x"));
    }

    #[test]
    fn nine_headers_in_a_part_when_parsed_then_python_multiparts_limit() {
        let headers = "X-A: 1\r\n".repeat(8);
        let body = format!("--b\r\nContent-Disposition: form-data; name=a\r\n{headers}\r\n1\r\n--b--");
        assert_eq!(multipart("b", &body), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn a_field_over_one_mebibyte_when_parsed_then_starlettes_limit() {
        let big = "x".repeat(MAX_PART_SIZE + 1);
        let body = format!("--b\r\nContent-Disposition: form-data; name=a\r\n\r\n{big}\r\n--b--");
        assert_eq!(multipart("b", &body), Err("Part exceeded maximum size of 1024KB.".to_owned()));
    }

    #[test]
    fn an_uppercase_type_with_parameters_when_parsed_then_multipart() {
        let body = b"--b\r\nContent-Disposition: form-data; name=a\r\n\r\n1\r\n--b--";
        let form = parse(Some(b"Multipart/Form-Data; boundary=b"), body).unwrap();
        assert_eq!(text_of(&form, "a").as_deref(), Some("1"));
    }

    #[test]
    fn urlencoded_fields_when_parsed_then_unquoted_and_trailing_name_kept() {
        let form = parse(Some(b"application/x-www-form-urlencoded"), b"a=1+2%21&&b&c=%D0%B0").unwrap();
        assert_eq!(text_of(&form, "a").as_deref(), Some("1 2!"));
        assert_eq!(text_of(&form, "b").as_deref(), Some(""));
        assert_eq!(text_of(&form, "c").as_deref(), Some("а"));
    }

    #[test]
    fn quoted_semicolons_given_parameters_when_split_then_kept_inside_the_value() {
        let (ctype, options) = parse_options_header("Form-Data; name=\"a;b\"; filename=x");
        assert_eq!(ctype, "form-data");
        assert_eq!(options, vec![("name".to_owned(), "a;b".to_owned()), ("filename".to_owned(), "x".to_owned())]);
    }
}
