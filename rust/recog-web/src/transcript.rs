//! The transcript helpers of `server.py`: segments as AI text, the recent window of a
//! transcript, the debug AI context's selections, local participant names.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use recog_core::pyjson::{self, Map, Value};
use recog_core::time::{self as pytime, PyDateTime};

/// A Python exception's text, for the 500 of an unhandled one or `str(e)`.
pub type PyError = String;

/// `value.get(key)` of a dict; another type raises `AttributeError`.
///
/// # Errors
/// The value is not a dict.
pub fn get<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, PyError> {
    match value {
        Value::Object(map) => Ok(map.get(key)),
        other => Err(format!("'{}' object has no attribute 'get'", other.type_name())),
    }
}

/// `value.get(key) or ""` formatted by an f-string.
fn text_or_empty(segment: &Value, key: &str) -> Result<String, PyError> {
    Ok(get(segment, key)?.filter(|v| v.truthy()).map(Value::py_str).unwrap_or_default())
}

/// `_segments_to_transcript`: `[ts] Speaker: text` of every segment with text, event
/// rows left out.
///
/// # Errors
/// A segment that is not a dict.
pub fn segments_to_transcript(segments: &[Value]) -> Result<String, PyError> {
    let mut lines = Vec::new();
    for segment in segments {
        if get(segment, "event_type")?.is_some_and(Value::truthy) {
            continue;
        }
        let ts = text_or_empty(segment, "ts_display")?;
        let speaker = text_or_empty(segment, "speaker")?;
        let text = text_or_empty(segment, "text")?;
        if text.is_empty() {
            continue;
        }
        lines.push(format!("[{ts}] {speaker}: {text}"));
    }
    Ok(lines.join("\n"))
}

/// `_build_ai_transcript`: the transcript, its last `max_chars` characters behind a
/// `...(truncated)...` line when longer.
///
/// # Errors
/// As [`segments_to_transcript`].
pub fn build_ai_transcript(segments: &[Value], max_chars: usize) -> Result<String, PyError> {
    let transcript = segments_to_transcript(segments)?;
    let count = transcript.chars().count();
    if count > max_chars {
        let tail: String = transcript.chars().skip(count - max_chars).collect();
        return Ok(format!("...(truncated)...\n{tail}"));
    }
    Ok(transcript)
}

/// `", ".join(names)`: a name that is not text raises `TypeError`.
///
/// # Errors
/// A name that is not a string.
pub fn join_names(names: &[Value]) -> Result<String, PyError> {
    let mut parts = Vec::new();
    for (i, name) in names.iter().enumerate() {
        match name {
            Value::Str(s) => parts.push(s.as_str()),
            other => return Err(format!("sequence item {i}: expected str instance, {} found", other.type_name())),
        }
    }
    Ok(parts.join(", "))
}

/// `_analysis_prompt_metadata`: the meeting title and every participant's name.
///
/// # Errors
/// Metadata of the wrong shape.
pub fn analysis_prompt_metadata(meeting: Option<&Value>) -> Result<(String, String), PyError> {
    let empty = Value::Object(Map::new());
    let meta = match meeting {
        Some(m) => get(m, "meta")?.unwrap_or(&empty),
        None => &empty,
    };
    let title = get(meta, "title")?.filter(|v| v.truthy()).cloned().unwrap_or_else(|| Value::from(""));
    let participants = get(meta, "participants")?.unwrap_or(&empty);
    let Value::Object(participants) = participants else {
        return Err(format!("'{}' object has no attribute 'items'", participants.type_name()));
    };
    let mut names = Vec::new();
    for (key, info) in participants.iter() {
        let name = get(info, "name")?.filter(|v| v.truthy()).cloned().unwrap_or_else(|| Value::from(key.as_str()));
        names.push(name);
    }
    let title = match title {
        Value::Str(s) => s,
        other => other.py_str(),
    };
    Ok((title, join_names(&names)?))
}

/// `_tail_lines(path, max_lines)`: the last lines of a file, read backwards in 8 KiB
/// chunks, decoded with undecodable bytes dropped.
#[must_use]
pub fn tail_lines(path: &Path, max_lines: usize) -> Vec<String> {
    if max_lines == 0 || !path.exists() {
        return Vec::new();
    }
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(mut position) = file.seek(SeekFrom::End(0)) else { return Vec::new() };
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    while position > 0 && lines.len() < max_lines {
        let read_size = position.min(8192);
        position -= read_size;
        let mut chunk = vec![0; usize::try_from(read_size).unwrap_or(0)];
        if file.seek(SeekFrom::Start(position)).is_err() || file.read_exact(&mut chunk).is_err() {
            break;
        }
        chunk.extend_from_slice(&buffer);
        buffer = chunk;
        while lines.len() < max_lines {
            let Some(idx) = buffer.iter().rposition(|b| *b == b'\n') else { break };
            let line = buffer[idx + 1..].to_vec();
            buffer.truncate(idx);
            if !line.is_empty() {
                lines.push(line);
            }
        }
    }
    if !buffer.is_empty() && lines.len() < max_lines {
        lines.push(buffer);
    }
    lines.reverse();
    lines.into_iter().map(|l| decode_ignore(&l)).collect()
}

/// `bytes.decode("utf-8", errors="ignore")`.
fn decode_ignore(raw: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = raw;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(good).unwrap_or_default());
                let skip = e.error_len().unwrap_or(bad.len());
                rest = &bad[skip..];
            }
        }
    }
}

/// `_parse_iso_ts`: `datetime.fromisoformat`, `None` for anything unreadable.
#[must_use]
pub fn parse_iso_ts(value: Option<&Value>) -> Option<PyDateTime> {
    match value {
        Some(Value::Str(s)) if !s.is_empty() => pytime::fromisoformat(s).ok(),
        _ => None,
    }
}

/// `_select_recent_segments`: the last `max_segments` rows within `window_seconds`
/// of the last row's time.
///
/// # Errors
/// Times that cannot be compared (naive against aware) or rows that are not dicts.
pub fn select_recent_segments(
    meetings_dir: &Path,
    meeting_id: &str,
    window_seconds: i128,
    max_segments: usize,
) -> Result<Vec<Value>, PyError> {
    let path = meetings_dir.join(meeting_id).join("transcript.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let scan_limit = (max_segments * 5).max(200);
    let mut segments: Vec<Value> = tail_lines(&path, scan_limit)
        .iter()
        .filter_map(|line| {
            let line = recog_core::py::strip(line);
            if line.is_empty() { None } else { pyjson::loads(line).ok() }
        })
        .collect();
    let Some(last) = segments.last() else { return Ok(Vec::new()) };
    if let Some(last_ts) = parse_iso_ts(get(last, "ts")?) {
        let cutoff_micros = i64::try_from(window_seconds * 1_000_000).unwrap_or(i64::MAX);
        let cutoff =
            PyDateTime { wall_micros: last_ts.wall_micros - cutoff_micros, offset_micros: last_ts.offset_micros };
        let mut windowed = Vec::new();
        for segment in &segments {
            if let Some(ts) = parse_iso_ts(get(segment, "ts")?)
                && ts.minus(&cutoff).map_err(|e| e.to_string())? >= 0
            {
                windowed.push(segment.clone());
            }
        }
        if !windowed.is_empty() {
            segments = windowed;
        }
    }
    if segments.len() > max_segments {
        segments = segments.split_off(segments.len() - max_segments);
    }
    Ok(segments)
}

/// A number for `<` against an integer bound; other types raise `TypeError`.
fn as_number(value: &Value, bound: i128) -> Result<f64, PyError> {
    match value {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(x) => Ok(*x),
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        other => Err(format!("'<' not supported between instances of '{}' and 'int' ({bound})", other.type_name())),
    }
}

/// `_slice_segments_by_id`.
///
/// # Errors
/// An ID that does not compare with an integer.
pub fn slice_segments_by_id(
    segments: &[Value],
    from_id: Option<i128>,
    to_id: Option<i128>,
    max_segments: usize,
) -> Result<Vec<Value>, PyError> {
    let mut filtered = Vec::new();
    for segment in segments {
        let Some(id) = get(segment, "id")?.filter(|v| !v.is_null()) else { continue };
        if let Some(from) = from_id
            && as_number(id, from)? < from as f64
        {
            continue;
        }
        if let Some(to) = to_id
            && as_number(id, to)? > to as f64
        {
            continue;
        }
        filtered.push(segment.clone());
    }
    filtered.truncate(max_segments);
    Ok(filtered)
}

/// `_slice_segments_by_ts_display`.
///
/// # Errors
/// A row that is not a dict.
pub fn slice_segments_by_ts_display(
    segments: &[Value],
    start: &str,
    max_segments: usize,
) -> Result<Vec<Value>, PyError> {
    if segments.is_empty() || start.is_empty() {
        return Ok(Vec::new());
    }
    for (index, segment) in segments.iter().enumerate() {
        if get(segment, "ts_display")? == Some(&Value::from(start)) {
            return Ok(segments[index..(index + max_segments).min(segments.len())].to_vec());
        }
    }
    Ok(Vec::new())
}

/// `compute_segment_duration_ms`: the slice length of a segment in its track's WAV.
/// Mic and system WAVs keep separate timelines (silence is skipped per track), so only
/// segments with the same `is_local` bound each other, unless the file holds both
/// sides (`shared_timeline`); event rows carry no offset; continuations of the same
/// speaker extend the slice. Clamped to 1–30 s.
#[must_use]
pub fn compute_segment_duration_ms(segments: &[Value], segment_index: usize, shared_timeline: bool) -> f64 {
    let field = |segment: &Value, key: &str| segment.as_object().and_then(|m| m.get(key)).cloned();
    let number = |value: Option<Value>| match value {
        Some(Value::Int(n)) => Some(n as f64),
        Some(Value::Float(x)) => Some(x),
        Some(Value::Bool(b)) => Some(f64::from(u8::from(b))),
        _ => None,
    };
    let segment = &segments[segment_index];
    let offset = number(field(segment, "audio_offset_ms").filter(Value::truthy)).unwrap_or(0.0);
    let track_local = field(segment, "is_local").unwrap_or(Value::Bool(false));
    let mut last_valid = offset;
    let mut next_offset = None;
    let mut index = segment_index + 1;
    while index < segments.len() {
        let next = &segments[index];
        let other_track = !field(next, "is_local").unwrap_or(Value::Bool(false)).py_eq(&track_local);
        if field(next, "event_type").is_some_and(|v| v.truthy()) || (!shared_timeline && other_track) {
            index += 1;
            continue;
        }
        let same_speaker = field(next, "speaker_id")
            .unwrap_or(Value::Null)
            .py_eq(&field(segment, "speaker_id").unwrap_or(Value::Null));
        if field(next, "cont").is_some_and(|v| v.truthy()) && same_speaker {
            if let Some(value) = number(field(next, "audio_offset_ms").filter(|v| !v.is_null())) {
                last_valid = value;
            }
            index += 1;
        } else {
            next_offset = number(field(next, "audio_offset_ms").filter(|v| !v.is_null()));
            break;
        }
    }
    let duration = match next_offset {
        Some(next) => next - offset,
        None => (last_valid - offset) + 3000.0,
    };
    duration.clamp(1000.0, 30000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<Value> {
        match pyjson::loads(text).unwrap() {
            Value::Array(rows) => rows,
            _ => unreachable!(),
        }
    }

    fn interleaved() -> Vec<Value> {
        rows(
            r#"[{"speaker_id": "Unknown", "is_local": false, "audio_offset_ms": 0},
                {"speaker_id": "local", "is_local": true, "audio_offset_ms": 3000},
                {"speaker_id": "Unknown", "is_local": false, "audio_offset_ms": 3000},
                {"speaker_id": "local", "is_local": true, "audio_offset_ms": 5500},
                {"speaker_id": "Unknown", "is_local": false, "audio_offset_ms": 8000}]"#,
        )
    }

    #[test]
    fn mic_segment_followed_by_system_twin_when_sliced_then_bounded_by_the_next_mic_segment() {
        assert_eq!(compute_segment_duration_ms(&interleaved(), 1, false), 2500.0);
    }

    #[test]
    fn system_segment_when_sliced_then_bounded_by_the_next_system_segment() {
        assert_eq!(compute_segment_duration_ms(&interleaved(), 2, false), 5000.0);
    }

    #[test]
    fn last_segment_of_its_track_when_sliced_then_default_chunk() {
        assert_eq!(compute_segment_duration_ms(&interleaved(), 3, false), 3000.0);
    }

    #[test]
    fn continuations_across_other_track_rows_when_sliced_then_group_plus_default_tail() {
        let rows = rows(
            r#"[{"speaker_id": "A", "is_local": false, "audio_offset_ms": 1000},
                {"speaker_id": "local", "is_local": true, "audio_offset_ms": 500},
                {"speaker_id": "A", "is_local": false, "audio_offset_ms": 4000, "cont": true},
                {"speaker_id": "A", "is_local": false, "audio_offset_ms": 7000, "cont": true}]"#,
        );
        assert_eq!(compute_segment_duration_ms(&rows, 0, false), (7000.0 - 1000.0) + 3000.0);
    }

    #[test]
    fn continuation_without_offset_when_sliced_then_last_known_end_is_kept() {
        let rows = rows(
            r#"[{"speaker_id": "A", "is_local": false, "audio_offset_ms": 1000},
                {"speaker_id": "A", "is_local": false, "audio_offset_ms": 4000, "cont": true},
                {"speaker_id": "A", "is_local": false, "audio_offset_ms": null, "cont": true}]"#,
        );
        assert_eq!(compute_segment_duration_ms(&rows, 0, false), (4000.0 - 1000.0) + 3000.0);
    }

    #[test]
    fn event_row_between_segments_when_sliced_then_it_does_not_bound_the_slice() {
        let rows = rows(
            r#"[{"speaker_id": "A", "is_local": false, "audio_offset_ms": 10000},
                {"speaker_id": "system", "is_local": false, "audio_offset_ms": 0, "event_type": "pcm_degraded"},
                {"speaker_id": "A", "is_local": false, "audio_offset_ms": 12000}]"#,
        );
        assert_eq!(compute_segment_duration_ms(&rows, 0, false), 2000.0);
    }

    #[test]
    fn very_short_and_very_long_slices_when_sliced_then_clamped_to_one_and_thirty_seconds() {
        let rows = rows(
            r#"[{"speaker_id": "A", "is_local": false, "audio_offset_ms": 0},
                {"speaker_id": "A", "is_local": false, "audio_offset_ms": 100},
                {"speaker_id": "B", "is_local": false, "audio_offset_ms": 999000}]"#,
        );
        assert_eq!(compute_segment_duration_ms(&rows, 0, false), 1000.0);
        assert_eq!(compute_segment_duration_ms(&rows, 1, false), 30000.0);
    }

    #[test]
    fn reply_on_the_other_side_when_the_file_is_a_mix_then_it_bounds_the_slice() {
        let rows = rows(
            r#"[{"text": "line", "audio_offset_ms": 0, "is_local": true},
                {"text": "line", "audio_offset_ms": 2000, "is_local": false},
                {"text": "line", "audio_offset_ms": 12000, "is_local": true}]"#,
        );
        assert_eq!(compute_segment_duration_ms(&rows, 0, true), 2000.0);
        assert_eq!(compute_segment_duration_ms(&rows, 0, false), 12000.0);
    }

    fn meta(text: &str) -> Value {
        pyjson::loads(text).unwrap()
    }

    #[test]
    fn meeting_with_title_and_participants_when_described_then_title_and_names_or_keys() {
        let meeting =
            meta(r#"{"meta": {"title": "Sync", "participants": {"a": {"name": "Ann"}, "b": {}, "c": {"name": ""}}}}"#);

        assert_eq!(analysis_prompt_metadata(Some(&meeting)).unwrap(), ("Sync".to_owned(), "Ann, b, c".to_owned()));
        assert_eq!(analysis_prompt_metadata(None).unwrap(), (String::new(), String::new()));
        assert_eq!(analysis_prompt_metadata(Some(&meta("{}"))).unwrap(), (String::new(), String::new()));
        assert_eq!(analysis_prompt_metadata(Some(&meta(r#"{"meta": {"title": 5}}"#))).unwrap().0, "5");
    }

    #[test]
    fn metadata_of_the_wrong_shape_when_described_then_the_python_error() {
        for (meeting, error) in [
            (r#"{"meta": []}"#, "'list' object has no attribute 'get'"),
            (r#"{"meta": {"participants": []}}"#, "'list' object has no attribute 'items'"),
            (r#"{"meta": {"participants": {"a": 1}}}"#, "'int' object has no attribute 'get'"),
            (r#"{"meta": {"participants": {"a": {"name": 2}}}}"#, "sequence item 0: expected str instance, int found"),
            ("[]", "'list' object has no attribute 'get'"),
        ] {
            assert_eq!(analysis_prompt_metadata(Some(&meta(meeting))).unwrap_err(), error, "{meeting}");
        }
    }

    #[test]
    fn file_longer_than_a_chunk_when_its_tail_is_read_then_the_last_lines_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let text: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &text).unwrap();

        assert_eq!(tail_lines(&path, 3), ["line 1997", "line 1998", "line 1999"]);
        assert_eq!(tail_lines(&path, 5000).len(), 2000);
        assert_eq!(tail_lines(&path, 5000)[0], "line 0");
        assert!(tail_lines(&path, 0).is_empty());
        assert!(tail_lines(&dir.path().join("absent"), 3).is_empty());
    }

    #[test]
    fn last_line_without_newline_and_blank_lines_when_the_tail_is_read_then_blanks_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "a\n\n\nb\nc").unwrap();

        assert_eq!(tail_lines(&path, 10), ["a", "b", "c"]);
        assert_eq!(tail_lines(&path, 2), ["b", "c"]);
        assert!(tail_lines(dir.path(), 2).is_empty());
    }

    #[test]
    fn undecodable_bytes_when_the_tail_is_read_then_they_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, b"ok\xff\xfeyes\n\xd0\x9f\xd1\n\xe2\x82").unwrap();

        assert_eq!(tail_lines(&path, 10), ["okyes", "П", ""]);
    }

    #[test]
    fn rows_with_and_without_times_when_the_recent_window_is_chosen_then_window_applies_to_timed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let meeting = dir.path().join("m");
        std::fs::create_dir_all(&meeting).unwrap();
        std::fs::write(
            meeting.join("transcript.jsonl"),
            concat!(
                "{\"id\": 1, \"ts\": \"2026-01-01T10:00:00+00:00\"}\n",
                "not json\n",
                "{\"id\": 2}\n",
                "{\"id\": 3, \"ts\": \"2026-01-01T10:04:00+00:00\"}\n",
                "{\"id\": 4, \"ts\": \"2026-01-01T10:05:00+00:00\"}\n",
            ),
        )
        .unwrap();
        let ids = |rows: Vec<Value>| -> Vec<String> {
            rows.iter().map(|r| get(r, "id").unwrap().unwrap().py_str()).collect()
        };

        assert_eq!(ids(select_recent_segments(dir.path(), "m", 120, 10).unwrap()), ["3", "4"]);
        assert_eq!(ids(select_recent_segments(dir.path(), "m", 900, 10).unwrap()), ["1", "3", "4"]);
        assert_eq!(ids(select_recent_segments(dir.path(), "m", 900, 2).unwrap()), ["3", "4"]);
        assert!(select_recent_segments(dir.path(), "absent", 900, 2).unwrap().is_empty());
    }

    #[test]
    fn last_row_without_a_time_or_rows_not_dicts_when_the_recent_window_is_chosen_then_last_rows_or_error() {
        let dir = tempfile::tempdir().unwrap();
        let meeting = dir.path().join("m");
        std::fs::create_dir_all(&meeting).unwrap();
        std::fs::write(
            meeting.join("transcript.jsonl"),
            "{\"id\": 1, \"ts\": \"2026-01-01T10:00:00+00:00\"}\n{\"id\": 2}\n",
        )
        .unwrap();
        assert_eq!(select_recent_segments(dir.path(), "m", 30, 10).unwrap().len(), 2);

        std::fs::write(meeting.join("transcript.jsonl"), "\n  \n").unwrap();
        assert!(select_recent_segments(dir.path(), "m", 30, 10).unwrap().is_empty());

        std::fs::write(meeting.join("transcript.jsonl"), "[1]\n").unwrap();
        assert_eq!(
            select_recent_segments(dir.path(), "m", 30, 10).unwrap_err(),
            "'list' object has no attribute 'get'"
        );

        std::fs::write(
            meeting.join("transcript.jsonl"),
            "{\"ts\": \"2026-01-01T10:00:00\"}\n{\"ts\": \"2026-01-01T10:00:00+00:00\"}\n",
        )
        .unwrap();
        assert!(select_recent_segments(dir.path(), "m", 30, 10).is_err());
    }

    #[test]
    fn ids_of_other_numeric_types_when_sliced_by_id_then_compared_as_numbers() {
        let rows = rows(r#"[{"id": 1.5}, {"id": true}, {"id": null}, {"text": "no id"}, {"id": 3}]"#);

        assert_eq!(slice_segments_by_id(&rows, Some(1), None, 10).unwrap().len(), 3);
        assert_eq!(slice_segments_by_id(&rows, None, Some(1), 10).unwrap().len(), 1);
        assert_eq!(slice_segments_by_id(&rows, Some(0), Some(5), 2).unwrap().len(), 2);
        let error = slice_segments_by_id(&self::rows(r#"[{"id": "x"}]"#), Some(1), None, 10).unwrap_err();
        assert_eq!(error, "'<' not supported between instances of 'str' and 'int' (1)");
    }

    #[test]
    fn start_time_when_sliced_by_time_then_rows_from_its_first_match() {
        let rows = rows(r#"[{"ts_display": "1"}, {"ts_display": "2"}, {"ts_display": "3"}]"#);

        assert_eq!(slice_segments_by_ts_display(&rows, "2", 10).unwrap().len(), 2);
        assert_eq!(slice_segments_by_ts_display(&rows, "1", 2).unwrap().len(), 2);
        assert!(slice_segments_by_ts_display(&rows, "", 2).unwrap().is_empty());
        assert!(slice_segments_by_ts_display(&[], "1", 2).unwrap().is_empty());
        assert!(slice_segments_by_ts_display(&rows, "9", 2).unwrap().is_empty());
        assert!(slice_segments_by_ts_display(&self::rows("[1]"), "9", 2).is_err());
    }

    #[test]
    fn rows_with_events_and_empty_texts_when_joined_then_only_spoken_lines() {
        let rows = rows(
            r#"[{"ts_display": "1", "speaker": "A", "text": "hi"}, {"text": ""}, {"event_type": "gap", "text": "x"}, {"text": "bare"}]"#,
        );

        assert_eq!(segments_to_transcript(&rows).unwrap(), "[1] A: hi\n[] : bare");
        assert!(segments_to_transcript(&self::rows("[1]")).is_err());
    }

    #[test]
    fn long_transcript_when_built_for_ai_then_tail_kept_behind_truncation_marker() {
        // given
        let rows = rows(
            r#"[{"ts_display": "10:00:00", "speaker": "A", "text": "hello world"}, {"event_type": "x", "text": "skip"}]"#,
        );
        // when / then
        assert_eq!(build_ai_transcript(&rows, 100).unwrap(), "[10:00:00] A: hello world");
        assert_eq!(build_ai_transcript(&rows, 5).unwrap(), "...(truncated)...\nworld");
    }
}
