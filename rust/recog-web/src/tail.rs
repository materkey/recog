//! Tailing `transcript.jsonl` across atomic rewrites, shared by the live watcher and
//! the per-meeting watcher (D10 of `openspec/changes/per-speaker-stream-refine`).
//!
//! `MeetingStorage.update_segments` replaces the file (`os.replace`), which changes
//! the inode and usually shrinks it. A changed inode or a file shorter than the read
//! position rewinds to the start, and the IDs already sent are skipped.

use std::collections::HashSet;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use recog_core::py;
use recog_core::pyjson::{self, Value};

/// Where a tailer stands in one file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TailState {
    pub position: u64,
    pub inode: Option<u64>,
    pub seen: HashSet<i128>,
}

/// `isinstance(seg.get("id"), int)`: an integer ID (a bool counts, as in Python).
#[must_use]
pub fn segment_id(segment: &Value) -> Option<i128> {
    match segment.as_object()?.get("id")? {
        Value::Int(n) => Some(*n),
        Value::Bool(b) => Some(i128::from(*b)),
        _ => None,
    }
}

/// The segments appended since the last read, IDs already seen left out. An error is
/// the text Python's `except Exception as e` would print; the state is then as far as
/// Python got before raising.
///
/// # Errors
/// The file cannot be read or decoded, or a line holds JSON that is not an object.
pub fn read_new_segments(path: &Path, state: &mut TailState) -> Result<Vec<Value>, String> {
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let rewrite = state.inode.is_some_and(|inode| inode != metadata.ino()) || metadata.len() < state.position;
    if rewrite {
        state.position = 0;
    }
    state.inode = Some(metadata.ino());
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(state.position)).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&raw).map_err(|e| pyjson::utf8_error_message(&raw, &e))?;
    state.position += raw.len() as u64;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut segments = Vec::new();
    for line in text.split_inclusive('\n') {
        let line = py::strip(line);
        if line.is_empty() {
            continue;
        }
        let Ok(segment) = pyjson::loads(line) else { continue };
        if segment.as_object().is_none() {
            return Err(format!("'{}' object has no attribute 'get'", segment.type_name()));
        }
        let id = segment_id(&segment);
        if id.is_some_and(|id| state.seen.contains(&id)) {
            continue;
        }
        segments.push(segment);
        if let Some(id) = id {
            state.seen.insert(id);
        }
    }
    Ok(segments)
}

/// `json.dumps({"type": "segments", "data": segments})`.
#[must_use]
pub fn segments_frame(segments: Vec<Value>) -> String {
    crate::json::ascii(&crate::json::object([("type", Value::from("segments")), ("data", Value::Array(segments))]))
}
