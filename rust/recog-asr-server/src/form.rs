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
    Header(HeaderState),
    PartDataStart,
    PartData,
    EndBoundary,
    End,
}

/// The states of one part's headers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderState {
    FieldStart,
    Field,
    ValueStart,
    Value,
    ValueAlmostDone,
    HeadersAlmostDone,
}

/// python-multipart's limits of one part's headers.
const MAX_HEADER_COUNT: usize = 8;
const MAX_HEADER_SIZE: usize = 4096 + 128;
const MAX_BOUNDARY_LENGTH: usize = 256;

/// What a state did with the byte at `i`.
enum Step {
    /// On to the next byte (`i += 1` of the Python loop).
    Next,
    /// From `i` as it is now: the same byte again in the new state (`i -= 1`), or
    /// where the state moved it (`continue`).
    Stay,
    /// The parse is over (`break`).
    Stop,
}

/// `MultipartParseError`, the answer to every byte python-multipart refuses.
fn expect(ok: bool) -> Result<(), Failure> {
    if ok { Ok(()) } else { Err(Failure::Parse) }
}

/// python-multipart's `MultipartParser` over one body: the state, the position and
/// the marks of the data a callback has not had yet.
struct Multipart<'a> {
    /// `\r\n--` and the boundary.
    boundary: Vec<u8>,
    data: &'a [u8],
    i: usize,
    state: State,
    index: usize,
    part_boundary: bool,
    last_boundary: bool,
    header_count: usize,
    header_size: usize,
    header_mark: Option<usize>,
    value_mark: Option<usize>,
    data_mark: Option<usize>,
}

/// python-multipart's `MultipartParser._internal_write` over the whole body, the
/// callbacks going to `parts`.
fn run_multipart(boundary: &[u8], data: &[u8], parts: &mut Parts) -> Result<(), Failure> {
    if boundary.len() > MAX_BOUNDARY_LENGTH {
        return Err(Failure::Parse);
    }
    let mut parser = Multipart::new(boundary, data);
    while parser.i < data.len() {
        match parser.step(parts)? {
            Step::Next => parser.i += 1,
            Step::Stay => {}
            Step::Stop => break,
        }
    }
    parser.flush(parts)
}

impl<'a> Multipart<'a> {
    fn new(boundary: &[u8], data: &'a [u8]) -> Self {
        Self {
            boundary: [b"\r\n--".as_slice(), boundary].concat(),
            data,
            i: 0,
            state: State::Start,
            index: 0,
            part_boundary: false,
            last_boundary: false,
            header_count: 0,
            header_size: 0,
            header_mark: None,
            value_mark: None,
            data_mark: None,
        }
    }

    /// The byte at `i` in the current state.
    fn step(&mut self, parts: &mut Parts) -> Result<Step, Failure> {
        let c = self.data[self.i];
        match self.state {
            State::Start => Ok(self.start(c)),
            State::StartBoundary => self.start_boundary(c, parts),
            State::Header(header) => self.header(header, c, parts),
            State::PartDataStart => Ok(self.part_data_start()),
            State::PartData => self.part_data(parts),
            State::EndBoundary => self.end_boundary(c),
            State::End => Ok(Step::Stop),
        }
    }

    fn header(&mut self, state: HeaderState, c: u8, parts: &mut Parts) -> Result<Step, Failure> {
        match state {
            HeaderState::FieldStart => self.header_field_start(c),
            HeaderState::Field => self.header_field(c, parts),
            HeaderState::ValueStart => self.header_value_start(c),
            HeaderState::Value => self.header_value(parts),
            HeaderState::ValueAlmostDone => self.header_value_almost_done(c),
            HeaderState::HeadersAlmostDone => self.headers_almost_done(c, parts),
        }
    }

    /// Newlines before the first boundary are skipped up to its first hyphen; a body
    /// of newlines only ends the parse.
    fn start(&mut self, c: u8) -> Step {
        if c == CR || c == LF {
            return match self.data[self.i..].iter().position(|&b| b == HYPHEN) {
                Some(at) => {
                    self.i += at;
                    Step::Stay
                }
                None => Step::Stop,
            };
        }
        self.index = 0;
        self.state = State::StartBoundary;
        Step::Stay
    }

    fn start_boundary(&mut self, c: u8, parts: &mut Parts) -> Result<Step, Failure> {
        if self.index == 0 && self.data[self.i..].starts_with(&self.boundary[2..]) {
            self.index = self.boundary.len() - 2;
            self.i += self.index;
            return Ok(Step::Stay);
        }
        self.start_boundary_byte(c, parts)?;
        Ok(Step::Next)
    }

    /// One byte of the first boundary line, matched byte by byte: `--boundary`, then
    /// `\r\n` (the first part's headers follow) or `-` (the closing boundary).
    fn start_boundary_byte(&mut self, c: u8, parts: &mut Parts) -> Result<(), Failure> {
        let blen = self.boundary.len();
        if self.index == blen - 1 {
            expect(c == LF)?;
            self.begin_part(parts);
            return Ok(());
        }
        if self.index == blen - 2 {
            expect(c == HYPHEN || c == CR)?;
            if c == HYPHEN {
                self.state = State::EndBoundary;
            }
        } else {
            expect(c == self.boundary[self.index + 2])?;
        }
        self.index += 1;
        Ok(())
    }

    /// `on_part_begin`, and the headers of the new part to read.
    fn begin_part(&mut self, parts: &mut Parts) {
        parts.part_begin();
        (self.header_count, self.header_size) = (0, 0);
        self.index = 0;
        self.state = State::Header(HeaderState::FieldStart);
    }

    /// Counts `amount` more bytes of the current header against its limit.
    fn advance(&mut self, amount: usize) -> Result<(), Failure> {
        self.header_size += amount;
        expect(self.header_size <= MAX_HEADER_SIZE)
    }

    fn header_field_start(&mut self, c: u8) -> Result<Step, Failure> {
        self.index = 0;
        if c != CR {
            self.header_count += 1;
            expect(self.header_count <= MAX_HEADER_COUNT)?;
            self.header_size = 0;
        }
        self.header_mark = Some(self.i);
        self.state = State::Header(HeaderState::Field);
        Ok(Step::Stay)
    }

    /// A header's name up to its colon, or the blank line that ends the headers.
    fn header_field(&mut self, c: u8, parts: &mut Parts) -> Result<Step, Failure> {
        let (data, i) = (self.data, self.i);
        if c == CR && self.index == 0 {
            self.header_mark = None;
            self.state = State::Header(HeaderState::HeadersAlmostDone);
            return Ok(Step::Next);
        }
        let colon = data[i..].iter().position(|&b| b == b':').map(|p| i + p);
        let end = colon.unwrap_or(data.len());
        self.advance(end - i + usize::from(colon.is_some()))?;
        expect(data[i..end].iter().all(|&b| is_token(b)))?;
        self.index += end - i;
        let Some(colon) = colon else {
            self.i = data.len();
            return Ok(Step::Stay);
        };
        expect(self.index != 0)?;
        self.i = colon;
        if let Some(r) = marked(&mut self.header_mark, colon) {
            parts.header_name.extend_from_slice(&data[r]);
        }
        self.state = State::Header(HeaderState::ValueStart);
        Ok(Step::Next)
    }

    fn header_value_start(&mut self, c: u8) -> Result<Step, Failure> {
        if c == b' ' {
            self.advance(1)?;
            return Ok(Step::Next);
        }
        self.value_mark = Some(self.i);
        self.state = State::Header(HeaderState::Value);
        Ok(Step::Stay)
    }

    /// A header's value up to its CR: `on_header_value` and `on_header_end`.
    fn header_value(&mut self, parts: &mut Parts) -> Result<Step, Failure> {
        let (data, i) = (self.data, self.i);
        let cr = data[i..].iter().position(|&b| b == CR).map(|p| i + p);
        self.advance(cr.unwrap_or(data.len()) - i)?;
        let Some(cr) = cr else {
            self.i = data.len();
            return Ok(Step::Stay);
        };
        self.i = cr;
        if let Some(r) = marked(&mut self.value_mark, cr) {
            parts.header_value.extend_from_slice(&data[r]);
        }
        parts.header_end();
        self.header_size = 0;
        self.state = State::Header(HeaderState::ValueAlmostDone);
        Ok(Step::Next)
    }

    fn header_value_almost_done(&mut self, c: u8) -> Result<Step, Failure> {
        expect(c == LF)?;
        self.state = State::Header(HeaderState::FieldStart);
        Ok(Step::Next)
    }

    fn headers_almost_done(&mut self, c: u8, parts: &mut Parts) -> Result<Step, Failure> {
        expect(c == LF)?;
        parts.headers_finished()?;
        self.state = State::PartDataStart;
        Ok(Step::Next)
    }

    fn part_data_start(&mut self) -> Step {
        self.data_mark = Some(self.i);
        self.state = State::PartData;
        Step::Stay
    }

    /// A part's data: skipped up to the next boundary, whose bytes are then matched
    /// one by one; a mismatch makes them data and reads the byte again.
    fn part_data(&mut self, parts: &mut Parts) -> Result<Step, Failure> {
        let prev_index = self.index;
        if self.index == 0 && !self.skip_to_boundary() {
            return Ok(Step::Stay);
        }
        if self.boundary_byte(self.data[self.i], parts)? {
            return Ok(Step::Next);
        }
        Ok(if self.index == 0 && prev_index > 0 { Step::Stay } else { Step::Next })
    }

    /// Moves `i` to the last byte of the next boundary; without one, to the end of the
    /// body, `index` counting a boundary cut off by it.
    fn skip_to_boundary(&mut self) -> bool {
        let (data, blen) = (self.data, self.boundary.len());
        if let Some(at) = find(data, &self.boundary, self.i) {
            self.index = blen - 1;
            self.i = at + blen - 1;
            return true;
        }
        // A boundary cut off by the end of this chunk.
        let from = self.i.max((data.len() + 1).saturating_sub(blen));
        if let Some(k) = data[from..].iter().rposition(|&b| b == self.boundary[0]).map(|p| from + p)
            && self.boundary.starts_with(&data[k..])
        {
            self.index = data.len() - k;
        }
        self.i = data.len();
        false
    }

    /// One byte at `index` of a boundary in the data; true once it began a new part.
    fn boundary_byte(&mut self, c: u8, parts: &mut Parts) -> Result<bool, Failure> {
        let blen = self.boundary.len();
        if self.index < blen {
            self.index = if self.boundary[self.index] == c { self.index + 1 } else { 0 };
        } else if self.index == blen {
            self.after_boundary(c);
        } else if self.index == blen + 1 {
            return self.boundary_end(c, parts);
        }
        Ok(false)
    }

    /// The byte after a whole boundary: CR for another part, `-` for the last one.
    fn after_boundary(&mut self, c: u8) {
        self.index += 1;
        match c {
            CR => self.part_boundary = true,
            HYPHEN => self.last_boundary = true,
            _ => self.index = 0,
        }
    }

    /// The second byte after a boundary: LF begins the next part, `-` ends the body.
    fn boundary_end(&mut self, c: u8, parts: &mut Parts) -> Result<bool, Failure> {
        if self.part_boundary {
            self.part_boundary = false;
            if c != LF {
                self.index = 0;
                return Ok(false);
            }
            self.end_part(parts)?;
            self.begin_part(parts);
            return Ok(true);
        }
        if self.last_boundary {
            if c == HYPHEN {
                self.end_part(parts)?;
                self.state = State::End;
            } else {
                self.index = 0;
            }
        }
        Ok(false)
    }

    /// The second hyphen of a closing boundary that came first.
    fn end_boundary(&mut self, c: u8) -> Result<Step, Failure> {
        if self.index == self.boundary.len() - 1 {
            expect(c == HYPHEN)?;
            self.index += 1;
            self.state = State::End;
        }
        Ok(Step::Next)
    }

    /// `on_part_data` up to the boundary just read, and `on_part_end`.
    fn end_part(&mut self, parts: &mut Parts) -> Result<(), Failure> {
        if let Some(r) = marked(&mut self.data_mark, self.i.saturating_sub(self.index)) {
            parts.part_data(&self.data[r])?;
        }
        parts.part_end();
        Ok(())
    }

    /// The data of a field, a header or a part cut off by the end of the body.
    fn flush(&mut self, parts: &mut Parts) -> Result<(), Failure> {
        let length = self.data.len();
        if let Some(r) = marked(&mut self.header_mark, length) {
            parts.header_name.extend_from_slice(&self.data[r]);
        }
        if let Some(r) = marked(&mut self.value_mark, length) {
            parts.header_value.extend_from_slice(&self.data[r]);
        }
        if let Some(r) = marked(&mut self.data_mark, length.saturating_sub(self.index)) {
            parts.part_data(&self.data[r])?;
        }
        Ok(())
    }
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

    fn names(form: &FormData) -> Vec<&str> {
        form.items().iter().map(|(name, _)| name.as_str()).collect()
    }

    const PART_A: &str = "--b\r\nContent-Disposition: form-data; name=a\r\n\r\n";

    #[test]
    fn newlines_before_the_first_boundary_when_parsed_then_skipped() {
        let body = format!("\r\n\n\r\n{PART_A}1\r\n--b--");
        assert_eq!(text_of(&multipart("b", &body).unwrap(), "a").as_deref(), Some("1"));
    }

    #[test]
    fn only_newlines_or_nothing_when_parsed_then_an_empty_form() {
        assert_eq!(multipart("b", "\r\n\r\n"), Ok(FormData::default()));
        assert_eq!(multipart("b", ""), Ok(FormData::default()));
    }

    #[test]
    fn a_body_shorter_than_the_boundary_when_parsed_then_an_empty_form() {
        assert_eq!(multipart("b", "--"), Ok(FormData::default()));
        assert_eq!(multipart("b", "-x"), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn a_boundary_of_256_bytes_when_parsed_then_read_and_of_257_refused() {
        let ok = "x".repeat(MAX_BOUNDARY_LENGTH);
        let body = format!("--{ok}\r\nContent-Disposition: form-data; name=a\r\n\r\n1\r\n--{ok}--");
        assert_eq!(text_of(&multipart(&ok, &body).unwrap(), "a").as_deref(), Some("1"));
        let long = "x".repeat(MAX_BOUNDARY_LENGTH + 1);
        let body = format!("--{long}\r\nContent-Disposition: form-data; name=a\r\n\r\n1\r\n--{long}--");
        assert_eq!(multipart(&long, &body), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn the_closing_boundary_first_when_parsed_then_an_empty_form_and_what_follows_ignored() {
        assert_eq!(multipart("b", "--b--\r\nanything"), Ok(FormData::default()));
        assert_eq!(multipart("b", "--b-x"), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn the_first_boundary_followed_by_neither_cr_nor_hyphen_when_parsed_then_the_parse_error() {
        assert_eq!(multipart("b", "--bx\r\n"), Err(PARSE_ERROR.to_owned()));
        assert_eq!(multipart("b", "--b\rx"), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn data_after_the_closing_boundary_when_parsed_then_ignored() {
        let body = format!("{PART_A}1\r\n--b--\r\n--b\r\nContent-Disposition: form-data; name=c\r\n\r\n2\r\n--b--");
        assert_eq!(names(&multipart("b", &body).unwrap()), ["a"]);
    }

    #[test]
    fn a_header_name_with_a_space_or_without_a_name_when_parsed_then_the_parse_error() {
        let space = "--b\r\nContent Disposition: form-data; name=a\r\n\r\n1\r\n--b--";
        assert_eq!(multipart("b", space), Err(PARSE_ERROR.to_owned()));
        let empty = "--b\r\n: form-data; name=a\r\n\r\n1\r\n--b--";
        assert_eq!(multipart("b", empty), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn a_body_cut_inside_a_header_when_parsed_then_an_empty_form() {
        assert_eq!(multipart("b", "--b\r\nContent-Dispo"), Ok(FormData::default()));
        assert_eq!(multipart("b", "--b\r\nContent-Disposition:"), Ok(FormData::default()));
        assert_eq!(multipart("b", "--b\r\nContent-Disposition:  form-data; na"), Ok(FormData::default()));
    }

    #[test]
    fn header_values_with_no_or_several_leading_spaces_when_parsed_then_the_same_parameters() {
        let none = "--b\r\ncontent-DISPOSITION:form-data; name=a\r\n\r\n1\r\n--b--";
        assert_eq!(text_of(&multipart("b", none).unwrap(), "a").as_deref(), Some("1"));
        let three = "--b\r\nContent-Disposition:   form-data; name=a\r\n\r\n1\r\n--b--";
        assert_eq!(text_of(&multipart("b", three).unwrap(), "a").as_deref(), Some("1"));
    }

    #[test]
    fn a_header_line_or_the_header_block_ended_by_cr_without_lf_when_parsed_then_the_parse_error() {
        let line = "--b\r\nContent-Disposition: form-data; name=a\rx";
        assert_eq!(multipart("b", line), Err(PARSE_ERROR.to_owned()));
        let block = PART_A.replacen("\r\n\r\n", "\r\n\rx", 1);
        assert_eq!(multipart("b", &block), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn eight_headers_in_a_part_when_parsed_then_accepted() {
        let headers = "X-A: 1\r\n".repeat(7);
        let body = format!("--b\r\nContent-Disposition: form-data; name=a\r\n{headers}\r\n1\r\n--b--");
        assert_eq!(text_of(&multipart("b", &body).unwrap(), "a").as_deref(), Some("1"));
    }

    #[test]
    fn a_header_of_4224_bytes_when_parsed_then_accepted_and_of_4225_refused() {
        // `X-A:` counts with its colon, then the space and the value.
        let header = |value_len: usize| {
            format!("--b\r\nX-A: {}\r\nContent-Disposition: form-data; name=a\r\n\r\n1\r\n--b--", "v".repeat(value_len))
        };
        assert_eq!(text_of(&multipart("b", &header(MAX_HEADER_SIZE - 5)).unwrap(), "a").as_deref(), Some("1"));
        assert_eq!(multipart("b", &header(MAX_HEADER_SIZE - 4)), Err(PARSE_ERROR.to_owned()));
        let name = format!("--b\r\n{}: 1\r\n\r\n1\r\n--b--", "N".repeat(MAX_HEADER_SIZE));
        assert_eq!(multipart("b", &name), Err(PARSE_ERROR.to_owned()));
    }

    #[test]
    fn an_empty_part_when_parsed_then_an_empty_value() {
        let body = format!("{PART_A}\r\n--b--");
        assert_eq!(text_of(&multipart("b", &body).unwrap(), "a").as_deref(), Some(""));
    }

    #[test]
    fn the_boundary_followed_by_other_bytes_inside_a_part_when_parsed_then_kept_in_the_value() {
        for inside in ["\r\n--bc", "\r\n--b\rx", "\r\n--b-x", "\r\n--x", "\r\n-"] {
            let body = format!("{PART_A}1{inside}2\r\n--b--");
            assert_eq!(text_of(&multipart("b", &body).unwrap(), "a"), Some(format!("1{inside}2")), "{inside:?}");
        }
    }

    #[test]
    fn three_parts_when_parsed_then_in_order() {
        let body = format!(
            "{PART_A}1\r\n--b\r\nContent-Disposition: form-data; name=c\r\n\r\n2\r\n\
             --b\r\nContent-Disposition: form-data; name=a\r\n\r\n3\r\n--b--\r\n"
        );
        let form = multipart("b", &body).unwrap();
        assert_eq!(names(&form), ["a", "c", "a"]);
        assert_eq!(text_of(&form, "a").as_deref(), Some("3"));
    }

    #[test]
    fn a_field_cut_inside_the_closing_boundary_when_at_the_size_limit_then_the_partial_boundary_not_counted() {
        let big = "x".repeat(MAX_PART_SIZE);
        let cut = format!("{PART_A}{big}\r\n--");
        assert_eq!(multipart("b", &cut), Ok(FormData::default()));
        let not_a_boundary = format!("{PART_A}{big}\r\n-x");
        assert_eq!(multipart("b", &not_a_boundary), Err("Part exceeded maximum size of 1024KB.".to_owned()));
    }

    #[test]
    fn a_file_over_one_mebibyte_when_parsed_then_whole() {
        let big = "x".repeat(MAX_PART_SIZE + 1);
        let body = format!("--b\r\nContent-Disposition: form-data; name=f; filename=a.wav\r\n\r\n{big}\r\n--b--");
        let Some(FormValue::File(file)) = multipart("b", &body).unwrap().get("f").cloned() else { panic!() };
        assert_eq!(file.data.len(), MAX_PART_SIZE + 1);
        assert_eq!(file.content_type, None);
    }

    #[test]
    fn a_thousand_and_one_fields_or_files_when_parsed_then_starlettes_limits() {
        let fields = format!("{PART_A}1\r\n").repeat(MAX_FIELDS + 1) + "--b--";
        assert_eq!(multipart("b", &fields), Err("Too many fields. Maximum number of fields is 1000.".to_owned()));
        let file = "--b\r\nContent-Disposition: form-data; name=f; filename=a\r\n\r\n1\r\n";
        let files = file.repeat(MAX_FILES + 1) + "--b--";
        assert_eq!(multipart("b", &files), Err("Too many files. Maximum number of files is 1000.".to_owned()));
        let at_limit = format!("{PART_A}1\r\n").repeat(MAX_FIELDS) + &file.repeat(MAX_FILES) + "--b--";
        assert_eq!(multipart("b", &at_limit).unwrap().items().len(), MAX_FIELDS + MAX_FILES);
    }

    #[test]
    fn values_not_in_utf8_or_a_latin1_charset_when_parsed_then_latin1() {
        let body = b"--b\r\nContent-Disposition: form-data; name=a\r\n\r\n\xd0\xb0\xff\r\n--b--";
        let utf8 = parse(Some(b"multipart/form-data; boundary=b"), body).unwrap();
        assert_eq!(text_of(&utf8, "a").as_deref(), Some("\u{d0}\u{b0}\u{ff}"));
        let body = b"--b\r\nContent-Disposition: form-data; name=a\r\n\r\n\xd0\xb0\r\n--b--";
        let utf8 = parse(Some(b"multipart/form-data; boundary=b"), body).unwrap();
        assert_eq!(text_of(&utf8, "a").as_deref(), Some("а"));
        let latin = parse(Some(b"multipart/form-data; boundary=b; charset=latin-1"), body).unwrap();
        assert_eq!(text_of(&latin, "a").as_deref(), Some("\u{d0}\u{b0}"));
    }

    #[test]
    fn other_content_types_when_parsed_then_an_empty_form() {
        assert_eq!(parse(Some(b"application/json"), b"{\"a\": 1}"), Ok(FormData::default()));
        assert_eq!(parse(None, b"a=1"), Ok(FormData::default()));
    }

    #[test]
    fn urlencoded_edge_cases_when_parsed_then_pythons_pairs() {
        let pairs = |body: &[u8]| -> Vec<(String, String)> {
            let form = parse(Some(b"application/x-www-form-urlencoded"), body).unwrap();
            form.items()
                .iter()
                .map(|(k, v)| match v {
                    FormValue::Text(t) => (k.clone(), t.clone()),
                    FormValue::File(_) => panic!("a file from a query string"),
                })
                .collect()
        };
        let owned = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        assert_eq!(pairs(b""), owned(&[]));
        assert_eq!(pairs(b"&&x=1&tail"), owned(&[("x", "1"), ("tail", "")]));
        assert_eq!(pairs(b"=v&a=1=2&e=&z"), owned(&[("", "v"), ("a", "1=2"), ("e", ""), ("z", "")]));
        assert_eq!(pairs(b"a="), owned(&[("a", "")]));
        assert_eq!(pairs(b"%ff+%41=%zz"), owned(&[("\u{fffd} A", "%zz")]));
    }

    #[test]
    fn parameters_without_a_value_repeated_or_extended_when_split_then_pythons_options() {
        let (ctype, options) = parse_options_header(" Form-Data ; flag; name=a; NAME=\"b\"; name*=x; filename*0=y");
        assert_eq!(ctype, "form-data");
        assert_eq!(options, vec![("flag".to_owned(), String::new()), ("name".to_owned(), "b".to_owned())]);
        assert_eq!(parse_options_header(""), (String::new(), Vec::new()));
        assert_eq!(parse_options_header("  Text/Plain  "), ("text/plain".to_owned(), Vec::new()));
    }

    #[test]
    fn quoted_escapes_and_windows_paths_when_split_then_unescaped_and_the_last_component() {
        let (_, options) = parse_options_header(r#"form-data; name="a\"b\\c"; filename="\\\\server\\x.wav""#);
        assert_eq!(
            options,
            vec![("name".to_owned(), r#"a"b\c"#.to_owned()), ("filename".to_owned(), "x.wav".to_owned())]
        );
        let (_, options) = parse_options_header(r"form-data; filename=dir\a.wav");
        assert_eq!(options, vec![("filename".to_owned(), r"dir\a.wav".to_owned())]);
        let (_, options) = parse_options_header(r#"form-data; filename="""#);
        assert_eq!(options, vec![("filename".to_owned(), String::new())]);
    }

    #[test]
    fn quoted_semicolons_given_parameters_when_split_then_kept_inside_the_value() {
        let (ctype, options) = parse_options_header("Form-Data; name=\"a;b\"; filename=x");
        assert_eq!(ctype, "form-data");
        assert_eq!(options, vec![("name".to_owned(), "a;b".to_owned()), ("filename".to_owned(), "x".to_owned())]);
    }
}
