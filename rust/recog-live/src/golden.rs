//! Golden sessions: one live process's inputs and ASR answers, recorded for replay, in
//! place of `recog/golden.py` and in its format, so `recog/golden_replay.py` reads a
//! Rust recording as it reads a Python one.
//!
//! Design: docs/plans/2026-11-07-golden-sessions-design.md. A session directory holds
//!
//! - `events.jsonl`: one event per line, in the order they happened, each with `t`
//!   (seconds since the recording started, monotonic, rounded to 6 decimals as Python's
//!   `round`) and `wall` (`time.time()`), written as `json.dumps(event,
//!   ensure_ascii=False)` writes it ([`recog_core::pyjson`]);
//! - `audio.bin`: the samples of `capture` events (and nothing else), appended as they
//!   come, each event naming its dtype (numpy's `dtype.str`: `<f4`, `<i2`, `<f8`), offset
//!   and length in bytes.
//!
//! The recording holds personal audio and text: it stays under the data directory.
//!
//! Known difference: an OS error is reported as Python's `str(OSError)` (`[Errno 28]
//! No space left on device`) only when it carries an errno; other I/O errors keep
//! Rust's wording.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use recog_core::pyjson::{self, Map, Value};
use sha1::{Digest as _, Sha1};

use crate::asr::{Clip, Transcription};
use crate::clock::{Clock as _, SystemClock};
use crate::log::Logger;

pub const EVENTS: &str = "events.jsonl";
pub const AUDIO: &str = "audio.bin";
/// A session whose directory name ends with this is never removed.
pub const KEEP_SUFFIX: &str = "-keep";

/// Samples as numpy holds them: the dtypes the live process records.
#[derive(Debug, Clone, Copy)]
pub enum Audio<'a> {
    F32(&'a [f32]),
    I16(&'a [i16]),
    F64(&'a [f64]),
}

impl Audio<'_> {
    /// numpy's `dtype.str` of the samples.
    #[must_use]
    pub fn dtype(&self) -> &'static str {
        match self {
            Audio::F32(_) => "<f4",
            Audio::I16(_) => "<i2",
            Audio::F64(_) => "<f8",
        }
    }

    /// The samples' bytes, `tobytes()` on a little-endian host.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Audio::F32(s) => s.iter().flat_map(|v| v.to_le_bytes()).collect(),
            Audio::I16(s) => s.iter().flat_map(|v| v.to_le_bytes()).collect(),
            Audio::F64(s) => s.iter().flat_map(|v| v.to_le_bytes()).collect(),
        }
    }
}

impl<'a> From<Clip<'a>> for Audio<'a> {
    fn from(clip: Clip<'a>) -> Self {
        match clip {
            Clip::F32(s) => Audio::F32(s),
            Clip::I16(s) => Audio::I16(s),
        }
    }
}

/// Samples read back from `audio.bin`.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioData {
    F32(Vec<f32>),
    I16(Vec<i16>),
    F64(Vec<f64>),
}

impl AudioData {
    /// The samples as an [`Audio`] view, e.g. to key them with [`audio_key`].
    #[must_use]
    pub fn as_audio(&self) -> Audio<'_> {
        match self {
            AudioData::F32(s) => Audio::F32(s),
            AudioData::I16(s) => Audio::I16(s),
            AudioData::F64(s) => Audio::F64(s),
        }
    }
}

/// What an ASR call was given: its dtype and samples, hashed (sha1 hex).
#[must_use]
pub fn audio_key(audio: Audio<'_>) -> String {
    let mut hash = Sha1::new();
    hash.update(audio.dtype().as_bytes());
    hash.update(audio.bytes());
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// An ASR answer as the recorder keeps it (`_plain`).
#[derive(Debug, Clone, Copy)]
pub enum AsrOutput<'a> {
    /// `None`: no answer.
    None,
    /// A live chunk's text.
    Text(&'a str),
    /// A batch's texts.
    Texts(&'a [String]),
    /// A refine's text with words in ms.
    Timed(&'a Transcription),
}

impl AsrOutput<'_> {
    fn plain(&self) -> Value {
        match self {
            AsrOutput::None => Value::Null,
            AsrOutput::Text(t) => Value::Str((*t).to_owned()),
            AsrOutput::Texts(ts) => Value::Array(ts.iter().map(|t| Value::Str(t.clone())).collect()),
            AsrOutput::Timed(r) => {
                let mut map = Map::new();
                map.insert("text", Value::Str(r.text.clone()));
                let words = r
                    .words
                    .iter()
                    .map(|w| {
                        Value::Array(vec![
                            Value::Str(w.text.clone()),
                            Value::Int(i128::from(w.start)),
                            Value::Int(i128::from(w.end)),
                        ])
                    })
                    .collect();
                map.insert("words", Value::Array(words));
                Value::Object(map)
            }
        }
    }
}

/// Python's `round(x, 6)`: the exact binary value rounded half to even at the sixth
/// decimal, read back as the nearest float.
#[must_use]
pub fn round6(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    // An f64 has at most 1074 fractional decimal digits: this is its exact expansion.
    let exact = format!("{:.1100}", x.abs());
    let (int_part, frac) = exact.split_once('.').unwrap_or((&exact, ""));
    let (kept, rest) = frac.split_at(6);
    let mut digits: Vec<u8> = int_part.bytes().chain(kept.bytes()).map(|b| b - b'0').collect();
    let rest = rest.as_bytes();
    let up = match rest.first() {
        Some(d) if *d > b'5' => true,
        Some(b'5') => rest[1..].iter().any(|&d| d != b'0') || digits.last().is_some_and(|d| d % 2 == 1),
        _ => false,
    };
    if up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let split = digits.len() - 6;
    let text: String = digits[..split].iter().map(|d| char::from(b'0' + d)).collect::<String>()
        + "."
        + &digits[split..].iter().map(|d| char::from(b'0' + d)).collect::<String>();
    let value: f64 = text.parse().unwrap_or(0.0);
    if x.is_sign_negative() { -value } else { value }
}

/// `str(OSError)`: `[Errno n] message` when the error has an errno.
fn os_error_text(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(errno) => {
            let text = error.to_string();
            let message = text.strip_suffix(&format!(" (os error {errno})")).unwrap_or(&text);
            format!("[Errno {errno}] {message}")
        }
        None => error.to_string(),
    }
}

type Writer = Box<dyn Write + Send>;
type ClockFn = Box<dyn Fn() -> f64 + Send + Sync>;

struct Files {
    events: Option<Writer>,
    audio: Option<Writer>,
    /// `audio.bin`'s length so far: Python's `tell()` on its append-mode file.
    audio_offset: u64,
}

/// Appends events to a session directory; safe to call from any thread.
pub struct GoldenRecorder {
    directory: PathBuf,
    clock: ClockFn,
    wall: ClockFn,
    start: f64,
    files: Mutex<Files>,
    logger: Logger,
}

impl std::fmt::Debug for GoldenRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoldenRecorder").field("directory", &self.directory).finish_non_exhaustive()
    }
}

impl GoldenRecorder {
    /// A session in `directory` (created with its parents), on the system clocks.
    ///
    /// # Errors
    /// Creating the directory or opening its files.
    pub fn new(directory: &Path) -> io::Result<Self> {
        let system = SystemClock::new();
        Self::with_clocks(
            directory,
            Box::new(move || system.monotonic()),
            Box::new(move || system.wall()),
            Logger::stderr(),
        )
    }

    /// A session whose `t` comes from `clock` (read once now as the start) and `wall`
    /// from `wall`; lines go to `logger`.
    ///
    /// # Errors
    /// Creating the directory or opening its files.
    pub fn with_clocks(directory: &Path, clock: ClockFn, wall: ClockFn, logger: Logger) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let start = clock();
        let events = OpenOptions::new().create(true).append(true).open(directory.join(EVENTS))?;
        let audio = OpenOptions::new().create(true).append(true).open(directory.join(AUDIO))?;
        let audio_offset = audio.metadata()?.len();
        Ok(Self {
            directory: directory.to_path_buf(),
            clock,
            wall,
            start,
            files: Mutex::new(Files { events: Some(Box::new(events)), audio: Some(Box::new(audio)), audio_offset }),
            logger,
        })
    }

    /// The session directory.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Puts `writer` in place of `audio.bin`: a test's failing disk.
    #[doc(hidden)]
    pub fn replace_audio_writer(&self, writer: Writer) {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if files.audio.is_some() {
            files.audio = Some(writer);
        }
    }

    fn write(&self, kind: &str, fields: Vec<(&str, Value)>, audio: Option<Audio<'_>>) {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        if files.events.is_none() {
            return;
        }
        let mut event = Map::new();
        event.insert("t", Value::Float(round6((self.clock)() - self.start)));
        event.insert("wall", Value::Float((self.wall)()));
        event.insert("kind", Value::Str(kind.to_owned()));
        if let Err(error) = Self::write_locked(&mut files, event, fields, audio) {
            // A recording must never cost the live process a frame or a chunk.
            self.logger.line(&format!("[golden] golden recording stopped: {}", os_error_text(&error)));
            files.events = None;
            files.audio = None;
        }
    }

    fn write_locked(
        files: &mut Files,
        mut event: Map,
        fields: Vec<(&str, Value)>,
        audio: Option<Audio<'_>>,
    ) -> io::Result<()> {
        if let Some(audio) = audio {
            let data = audio.bytes();
            event.insert("dtype", Value::Str(audio.dtype().to_owned()));
            event.insert("offset", Value::Int(i128::from(files.audio_offset)));
            event.insert("nbytes", Value::Int(data.len() as i128));
            let writer = files.audio.as_mut().ok_or_else(|| io::Error::other("audio.bin is closed"))?;
            writer.write_all(&data)?;
            writer.flush()?;
            files.audio_offset += data.len() as u64;
        }
        for (key, value) in fields {
            event.insert(key, value);
        }
        let line = pyjson::dumps(&Value::Object(event), None) + "\n";
        let writer = files.events.as_mut().ok_or_else(|| io::Error::other("events.jsonl is closed"))?;
        writer.write_all(line.as_bytes())?;
        writer.flush()
    }

    pub fn client_connected(&self, client: i64) {
        self.write("connect", vec![("client", Value::Int(i128::from(client)))], None);
    }

    pub fn client_disconnected(&self, client: i64) {
        self.write("disconnect", vec![("client", Value::Int(i128::from(client)))], None);
    }

    /// A parsed bridge message as received.
    pub fn frame(&self, client: i64, data: &Value) {
        self.write("frame", vec![("client", Value::Int(i128::from(client))), ("data", data.clone())], None);
    }

    /// A microphone or system-audio chunk as `get_chunk` gave it, with its capture time.
    pub fn capture(&self, source: &str, chunk: Audio<'_>, start_ts: f64) {
        self.write(
            "capture",
            vec![("source", Value::Str(source.to_owned())), ("start_ts", Value::Float(start_ts))],
            Some(chunk),
        );
    }

    /// An ASR call: its name, the keys of its input clips, its answer.
    pub fn asr(&self, call: &str, clips: &[Audio<'_>], output: AsrOutput<'_>) {
        let inputs = clips.iter().map(|c| Value::Str(audio_key(*c))).collect();
        self.write(
            "asr",
            vec![("call", Value::Str(call.to_owned())), ("inputs", Value::Array(inputs)), ("output", output.plain())],
            None,
        );
    }

    /// Closes both files; later events are dropped.
    pub fn close(&self) {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        files.events = None;
        files.audio = None;
    }
}

/// Deletes the sessions under `root` whose `events.jsonl` was last written more than
/// `max_age_days` ago (`now` defaults to the wall clock), in name order.
///
/// A session whose directory name ends with `-keep` is kept whatever its age, and only
/// directories holding an `events.jsonl` are sessions.
///
/// # Errors
/// Listing `root` or removing a session.
pub fn remove_old_sessions(root: &Path, max_age_days: f64, now: Option<f64>) -> io::Result<Vec<PathBuf>> {
    let now = now.unwrap_or_else(|| SystemClock::new().wall());
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(root)?.map(|e| e.map(|e| e.path())).collect::<io::Result<_>>()?;
    entries.sort();
    let mut removed = Vec::new();
    for directory in entries {
        let events = directory.join(EVENTS);
        let keep = directory.file_name().is_some_and(|n| n.to_string_lossy().ends_with(KEEP_SUFFIX));
        if keep || !events.is_file() {
            continue;
        }
        let modified = fs::metadata(&events)?.modified()?;
        let mtime = match modified.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        };
        if now - mtime > max_age_days * 86400.0 {
            fs::remove_dir_all(&directory)?;
            removed.push(directory);
        }
    }
    Ok(removed)
}

/// An event read back: its JSON object, and for a `capture` its samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub fields: Map,
    pub audio: Option<AudioData>,
}

impl Event {
    /// A field of the event.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }
}

fn decode(dtype: &str, bytes: &[u8]) -> io::Result<AudioData> {
    match dtype {
        "<f4" => Ok(AudioData::F32(bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect())),
        "<i2" => Ok(AudioData::I16(bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect())),
        "<f8" => Ok(AudioData::F64(bytes.as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b)).collect())),
        other => Err(io::Error::new(io::ErrorKind::InvalidData, format!("unknown dtype {other:?}"))),
    }
}

fn int_field(event: &Map, key: &str) -> io::Result<u64> {
    match event.get(key) {
        Some(Value::Int(n)) => u64::try_from(*n).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("event without an integer {key}"))),
    }
}

/// The events of a session in order; `capture` events carry their samples. A session
/// still being written gives every whole line so far.
///
/// # Errors
/// Reading the files, a line that is not JSON, or audio outside `audio.bin`.
pub fn read_session(directory: &Path) -> io::Result<Vec<Event>> {
    let mut audio = File::open(directory.join(AUDIO))?;
    let text = fs::read_to_string(directory.join(EVENTS))?;
    let mut events = Vec::new();
    for line in text.split_inclusive('\n') {
        let value = pyjson::loads(line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let Value::Object(fields) = value else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "an event that is not an object"));
        };
        let samples = if fields.contains_key("nbytes") {
            let offset = int_field(&fields, "offset")?;
            let nbytes = usize::try_from(int_field(&fields, "nbytes")?).unwrap_or(usize::MAX);
            let dtype = fields.get("dtype").and_then(Value::as_str).unwrap_or("");
            audio.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; nbytes];
            audio.read_exact(&mut bytes)?;
            Some(decode(dtype, &bytes)?)
        } else {
            None
        };
        events.push(Event { fields, audio: samples });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_at_the_sixth_decimal_round6_rounds_half_to_even_like_python() {
        // 1/128 = 0.0078125 exactly: a tie, the sixth digit 2 is even.
        assert_eq!(round6(0.007_812_5), 0.007_812);
        // 3/128 = 0.0234375: a tie, the sixth digit 7 is odd.
        assert_eq!(round6(0.023_437_5), 0.023_438);
        assert_eq!(round6(0.5), 0.5);
        assert_eq!(round6(1.999_999_9), 2.0);
        assert_eq!(round6(0.1 + 0.2), 0.3);
    }

    #[test]
    fn same_numbers_other_dtype_audio_key_differs() {
        assert_ne!(audio_key(Audio::I16(&[0; 4])), audio_key(Audio::F32(&[0.0; 4])));
    }

    #[test]
    fn errno_error_os_error_text_reads_as_python_oserror() {
        let error = io::Error::from_raw_os_error(28);
        assert_eq!(os_error_text(&error), "[Errno 28] No space left on device");
    }
}
