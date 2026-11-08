//! The web UI files as Starlette's `FileResponse` and `StaticFiles` serve them: the
//! same ETag (`md5("<st_mtime>-<st_size>")`), `Last-Modified`, byte ranges (one range,
//! several as `multipart/byteranges`, 400 for a malformed header, 416 beyond the end),
//! and for `/static/` the 304 revalidation. Every UI file carries `Cache-Control:
//! no-cache`, so an update never mixes old and new code.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use md5::{Digest as _, Md5};

use crate::responses;

pub const CACHE_CONTROL: &str = "no-cache";

/// `mimetypes.guess_type` on this Mac for the extensions a web UI uses; Starlette
/// falls back to `text/plain`.
#[must_use]
pub fn guess_type(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wav" => "audio/x-wav",
        "md" => "text/markdown",
        _ => "text/plain",
    }
}

/// The `Content-Type` of a media type: text types get `charset=utf-8`.
#[must_use]
pub fn content_type(media: &str) -> String {
    if media.starts_with("text/") && !media.to_ascii_lowercase().contains("charset=") {
        format!("{media}; charset=utf-8")
    } else {
        media.to_owned()
    }
}

struct Stat {
    size: u64,
    etag: String,
    last_modified: String,
}

fn stat(path: &Path) -> std::io::Result<Option<Stat>> {
    let m = std::fs::metadata(path)?;
    if !m.is_file() {
        return Ok(None);
    }
    // CPython's st_mtime: `sec + nsec * 1e-9` in double precision, printed by repr.
    let mtime = m.mtime() as f64 + m.mtime_nsec() as f64 * 1e-9;
    let base = format!("{}-{}", recog_core::py::float_repr(mtime), m.size());
    let etag = format!("\"{:x}\"", Md5::digest(base.as_bytes()));
    let seconds = u64::try_from(m.mtime()).unwrap_or(0);
    let last_modified = httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(seconds));
    Ok(Some(Stat { size: m.size(), etag, last_modified }))
}

enum RangeError {
    Malformed(&'static str),
    NotSatisfiable,
}

fn parse_ranges(range: &str, size: u64) -> Vec<(u64, u64)> {
    let mut ranges = Vec::new();
    for part in range.split(',') {
        let part = part.trim();
        if part.is_empty() || part == "-" || !part.contains('-') {
            continue;
        }
        let (start, end) = part.split_once('-').unwrap_or_default();
        let (start, end) = (start.trim(), end.trim());
        let parse = |s: &str| -> Option<i128> { recog_core::py::parse_int(s).ok().map(i128::from) };
        let size_i = i128::from(size);
        let start_v = if start.is_empty() {
            match parse(end) {
                Some(e) => size_i - e,
                None => continue,
            }
        } else {
            match parse(start) {
                Some(s) => s,
                None => continue,
            }
        };
        let end_v = if !start.is_empty() && !end.is_empty() {
            match parse(end) {
                Some(e) if e < size_i => e + 1,
                Some(_) => size_i,
                None => continue,
            }
        } else {
            size_i
        };
        // Negative starts are kept for the satisfiability check below.
        ranges.push((start_v, end_v));
    }
    ranges.into_iter().map(|(s, e)| (u64::try_from(s).unwrap_or(u64::MAX), u64::try_from(e).unwrap_or(0))).collect()
}

fn parse_range_header(header: &str, size: u64) -> Result<Vec<(u64, u64)>, RangeError> {
    let Some((units, range)) = header.split_once('=') else {
        return Err(RangeError::Malformed("Malformed range header."));
    };
    if units.trim().to_lowercase() != "bytes" {
        return Err(RangeError::Malformed("Only support bytes range"));
    }
    let mut ranges = parse_ranges(range, size);
    if ranges.is_empty() {
        return Err(RangeError::Malformed("Range header: range must be requested"));
    }
    if ranges.iter().any(|(start, _)| *start >= size) {
        return Err(RangeError::NotSatisfiable);
    }
    if ranges.iter().any(|(start, end)| start > end) {
        return Err(RangeError::Malformed("Range header: start must be less than end"));
    }
    if ranges.len() == 1 {
        return Ok(ranges);
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = vec![ranges[0]];
    for (start, end) in ranges.into_iter().skip(1) {
        let last = merged.last_mut().unwrap_or_else(|| unreachable!("merged starts non-empty"));
        if start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    Ok(merged)
}

fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut data = Vec::new();
    file.take(end.saturating_sub(start)).read_to_end(&mut data)?;
    Ok(data)
}

fn set(headers: &mut HeaderMap, name: header::HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

/// How a file is served.
#[derive(Clone, Copy)]
pub struct Serve {
    /// `HEAD`: headers only.
    pub head: bool,
    /// `StaticFiles` answers 304 to a matching `If-None-Match` / `If-Modified-Since`.
    pub conditional: bool,
}

/// `parsedate(a) >= parsedate(b)` of `If-Modified-Since` and `Last-Modified`.
fn not_modified_since(since: &str, last_modified: &str) -> bool {
    match (httpdate::parse_http_date(since), httpdate::parse_http_date(last_modified)) {
        (Ok(since), Ok(last)) => since >= last,
        _ => false,
    }
}

/// A regular file as Starlette serves it; `None` when the path is not one.
#[must_use]
pub fn serve(path: &Path, request: &HeaderMap, how: Serve) -> Option<Response> {
    let stat = match stat(path) {
        Ok(Some(stat)) => stat,
        _ => return None,
    };
    let media = content_type(guess_type(path));
    let mut headers = HeaderMap::new();
    set(&mut headers, header::CACHE_CONTROL, CACHE_CONTROL);
    set(&mut headers, header::CONTENT_TYPE, &media);
    set(&mut headers, header::ACCEPT_RANGES, "bytes");
    set(&mut headers, header::CONTENT_LENGTH, &stat.size.to_string());
    set(&mut headers, header::LAST_MODIFIED, &stat.last_modified);
    set(&mut headers, header::ETAG, &stat.etag);
    let text = |name: header::HeaderName| request.get(name).and_then(|v| v.to_str().ok());
    if how.conditional {
        let not_modified = match text(header::IF_NONE_MATCH).filter(|v| !v.is_empty()) {
            Some(tags) => {
                tags.split(',').any(|tag| tag.trim_matches(|c| c == ' ' || c == 'W' || c == '/') == stat.etag)
            }
            None => text(header::IF_MODIFIED_SINCE).is_some_and(|since| not_modified_since(since, &stat.last_modified)),
        };
        if not_modified {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::NOT_MODIFIED;
            set(response.headers_mut(), header::ETAG, &stat.etag);
            set(response.headers_mut(), header::CACHE_CONTROL, CACHE_CONTROL);
            return Some(response);
        }
    }
    let range = text(header::RANGE);
    let if_range = text(header::IF_RANGE);
    let use_range = match (range, if_range) {
        (None, _) => false,
        (Some(_), Some(tag)) => tag == stat.last_modified || tag == stat.etag,
        (Some(_), None) => true,
    };
    let body_or_empty =
        |data: std::io::Result<Vec<u8>>| if how.head { Body::empty() } else { Body::from(data.unwrap_or_default()) };
    if !use_range {
        let mut response = Response::new(body_or_empty(if how.head { Ok(Vec::new()) } else { std::fs::read(path) }));
        *response.headers_mut() = headers;
        return Some(response);
    }
    let ranges = match parse_range_header(range.unwrap_or_default(), stat.size) {
        Ok(ranges) => ranges,
        Err(RangeError::Malformed(message)) => {
            return Some(responses::plain(StatusCode::BAD_REQUEST, message, &[]));
        }
        Err(RangeError::NotSatisfiable) => {
            return Some(responses::plain(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "",
                &[(header::CONTENT_RANGE, format!("bytes */{}", stat.size))],
            ));
        }
    };
    if let [(start, end)] = ranges[..] {
        set(&mut headers, header::CONTENT_RANGE, &format!("bytes {start}-{}/{}", end - 1, stat.size));
        set(&mut headers, header::CONTENT_LENGTH, &(end - start).to_string());
        let mut response = Response::new(body_or_empty(read_range(path, start, end)));
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        *response.headers_mut() = headers;
        return Some(response);
    }
    let boundary = uuid::Uuid::new_v4().simple().to_string()[..26].to_owned();
    let mut body = Vec::new();
    for (start, end) in &ranges {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: {media}\r\nContent-Range: bytes {start}-{}/{}\r\n\r\n",
                end - 1,
                stat.size
            )
            .as_bytes(),
        );
        body.extend(read_range(path, *start, *end).unwrap_or_default());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--").as_bytes());
    set(&mut headers, header::CONTENT_TYPE, &format!("multipart/byteranges; boundary={boundary}"));
    set(&mut headers, header::CONTENT_LENGTH, &body.len().to_string());
    let mut response = Response::new(if how.head { Body::empty() } else { Body::from(body) });
    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    *response.headers_mut() = headers;
    Some(response)
}

/// `StaticFiles.lookup_path`: the file under `directory`, or `None` for anything that
/// leaves it (symlinks resolved) or does not exist.
#[must_use]
pub fn lookup(directory: &Path, relative: &str) -> Option<PathBuf> {
    if relative.contains('\0') {
        return None;
    }
    let joined = directory.join(relative.trim_start_matches('/'));
    let full = std::fs::canonicalize(&joined).ok()?;
    let root = std::fs::canonicalize(directory).ok()?;
    if !full.starts_with(&root) || full.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(full)
}
