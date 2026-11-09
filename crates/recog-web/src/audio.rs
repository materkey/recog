//! `GET /api/meetings/{id}/audio/{segment_index}`: the slice of a meeting's recording
//! that a transcript segment covers, cut by ffmpeg into a 16 kHz mono WAV.
//!
//! The track follows the speaker (local → `audio_mic.wav`, remote →
//! `audio_system.wav`) unless `track` names one; a phone call's per-side files
//! (`audio_me.m4a`, `audio_them.m4a`) and its single mix (`audio_mix.m4a`) stand in,
//! and the other side's file when this side has none. The slice length is
//! [`crate::transcript::compute_segment_duration_ms`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use recog_core::py::float_repr;
use recog_core::pyjson::{Map, Value};

use crate::request::Req;
use crate::responses::{self, Reply};
use crate::server::AppState;
use crate::transcript::compute_segment_duration_ms;
use crate::util::invalid_id;
use crate::validation::{self, Ty};

fn bad_request(message: &str) -> Response {
    responses::detail(StatusCode::BAD_REQUEST, message)
}

/// `meta.get(f"audio_{track}_path")` when it names an existing file: `Ok(None)` for
/// a missing or falsy value or a path that does not exist, `Err` for a value
/// `Path()` refuses.
fn meta_path(meta: &Map, track: &str) -> Result<Option<PathBuf>, Response> {
    match meta.get(&format!("audio_{track}_path")) {
        None => Ok(None),
        Some(value) if !value.truthy() => Ok(None),
        Some(Value::Str(path)) => Ok(Some(PathBuf::from(path)).filter(|p| p.exists())),
        Some(_) => Err(responses::internal_error()),
    }
}

/// Which file holds the segment's audio, or the 400 Python raises.
fn audio_file(meta: &Map, meeting_path: &Path, track: &str) -> Result<PathBuf, Response> {
    if let Some(path) = track_file(meta, meeting_path, track)? {
        return Ok(path);
    }
    let fallback = if track == "mic" { "system" } else { "mic" };
    if let Some(path) = track_file(meta, meeting_path, fallback)? {
        return Ok(path);
    }
    let mix = meeting_path.join("audio_mix.m4a");
    if mix.exists() {
        return Ok(mix);
    }
    let expired = meta.get("audio_deleted_at").is_some_and(Value::truthy);
    Err(bad_request(if expired {
        "Audio expired (older than 24h retention)"
    } else {
        "No audio file available for this meeting"
    }))
}

/// One track's file: the path in the metadata, the live WAV, or the phone call's
/// file of that side.
fn track_file(meta: &Map, meeting_path: &Path, track: &str) -> Result<Option<PathBuf>, Response> {
    if let Some(path) = meta_path(meta, track)? {
        return Ok(Some(path));
    }
    let folder = meeting_path.join(format!("audio_{track}.wav"));
    if folder.exists() {
        return Ok(Some(folder));
    }
    let side = match track {
        "mic" => Some(meeting_path.join("audio_me.m4a")),
        "system" => Some(meeting_path.join("audio_them.m4a")),
        _ => None,
    };
    Ok(side.filter(|p| p.exists()))
}

/// `str(x / 1000)` of a millisecond value.
fn seconds(ms: f64) -> String {
    float_repr(ms / 1000.0)
}

/// The segment index and the track parameter, or the 422.
fn audio_params(req: &Req) -> Result<(Option<Value>, Option<Value>), Response> {
    let mut errors = Vec::new();
    let index = validation::validate_param(
        "path",
        "segment_index",
        &Ty::Int,
        Some(req.param("segment_index")),
        true,
        &mut errors,
    );
    let track = validation::validate_param("query", "track", &Ty::Str, req.query("track"), false, &mut errors);
    if !errors.is_empty() {
        return Err(responses::invalid(&errors));
    }
    Ok((index, track))
}

/// The meeting's metadata and rows, or the 400 of a path-like ID and the 404.
fn stored_meeting(state: &AppState, meeting_id: &str) -> Result<(Map, Vec<Value>), Response> {
    if invalid_id(meeting_id) {
        return Err(bad_request("Invalid meeting ID"));
    }
    let storage = state.config.storage().map_err(|_| responses::internal_error())?;
    let meeting = match storage.get_meeting(meeting_id) {
        Ok(Some(meeting)) => meeting,
        Ok(None) => return Err(responses::detail(StatusCode::NOT_FOUND, "Meeting not found")),
        Err(_) => return Err(responses::internal_error()),
    };
    let fields = meeting.as_object().cloned().unwrap_or_default();
    let meta = fields.get("meta").and_then(Value::as_object).cloned().unwrap_or_default();
    let segments = match fields.get("segments") {
        Some(Value::Array(rows)) => rows.clone(),
        _ => Vec::new(),
    };
    Ok((meta, segments))
}

/// The index of an existing row, or the 400.
fn segment_index(index: Option<Value>, count: usize) -> Result<usize, Response> {
    match index {
        Some(Value::Int(n)) if n >= 0 && usize::try_from(n).is_ok_and(|n| n < count) => {
            Ok(usize::try_from(n).unwrap_or(0))
        }
        _ => Err(bad_request("Invalid segment index")),
    }
}

/// The segment's offset in its file, or the 400 of a segment recorded without one.
fn segment_offset(segment: &Map) -> Result<f64, Response> {
    match segment.get("audio_offset_ms") {
        None | Some(Value::Null) => Err(bad_request("Segment has no audio data (recorded before audio feature)")),
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(x)) => Ok(*x),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(_) => Err(responses::internal_error()),
    }
}

/// The named track, else the speaker's side.
fn chosen_track(track: Option<Value>, segment: &Map) -> String {
    match track {
        Some(track) => track.py_str(),
        None if segment.get("is_local").is_some_and(Value::truthy) => "mic".to_owned(),
        None => "system".to_owned(),
    }
}

/// The slice cut by ffmpeg into a 16 kHz mono WAV, or the 500 with the head of its
/// error.
async fn extract(ffmpeg: &std::ffi::OsStr, audio_path: &Path, offset: f64, duration: f64) -> Result<Vec<u8>, Response> {
    let temp = tempfile::Builder::new().suffix(".wav").tempfile().map_err(|_| responses::internal_error())?;
    let temp_path = temp.path().to_path_buf();
    let output = tokio::process::Command::new(ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(audio_path)
        .args(["-ss", &seconds(offset), "-t", &seconds(duration), "-ar", "16000", "-ac", "1"])
        .arg(&temp_path)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|_| responses::internal_error())?;
    if !output.status.success() {
        let Ok(stderr) = String::from_utf8(output.stderr) else { return Err(responses::internal_error()) };
        let head: String = stderr.chars().take(200).collect();
        return Err(responses::detail(StatusCode::INTERNAL_SERVER_ERROR, &format!("Audio extraction failed: {head}")));
    }
    let data = std::fs::read(&temp_path).map_err(|_| responses::internal_error())?;
    drop(temp);
    Ok(data)
}

/// The route.
pub async fn segment_audio(state: &Arc<AppState>, req: &Req) -> Reply {
    let (index, track) = audio_params(req)?;
    let meeting_id = req.param("meeting_id");
    let (meta, segments) = stored_meeting(state, meeting_id)?;
    let meeting_path = state.config.meetings_dir.join(meeting_id);
    let index = segment_index(index, segments.len())?;
    let segment = segments[index].as_object().cloned().unwrap_or_default();
    let offset = segment_offset(&segment)?;
    let audio_path = audio_file(&meta, &meeting_path, &chosen_track(track, &segment))?;
    if !audio_path.exists() {
        return Err(bad_request("Audio file not found (may have been cleaned up)"));
    }
    let shared = audio_path.file_name().and_then(|n| n.to_str()) == Some("audio_mix.m4a");
    let duration = compute_segment_duration_ms(&segments, index, shared);
    let data = extract(&state.config.ffmpeg, &audio_path, offset, duration).await?;
    let length = data.len();
    let mut response = Response::new(Body::from(data));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
    if let Ok(value) = HeaderValue::from_str(&format!("inline; filename=\"segment_{index}.wav\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    Ok(response)
}
