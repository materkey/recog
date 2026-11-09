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
    MEDIA_TYPES.iter().find(|(known, _)| *known == ext).map_or("text/plain", |(_, media)| media)
}

const MEDIA_TYPES: [(&str, &str); 18] = [
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("css", "text/css"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("svg", "image/svg+xml"),
    ("json", "application/json"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("ico", "image/x-icon"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("ttf", "font/ttf"),
    ("wav", "audio/x-wav"),
    ("md", "text/markdown"),
];

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
    // Negative starts are kept for the satisfiability check of the caller.
    range
        .split(',')
        .filter_map(|part| parse_range(part.trim(), i128::from(size)))
        .map(|(s, e)| (u64::try_from(s).unwrap_or(u64::MAX), u64::try_from(e).unwrap_or(0)))
        .collect()
}

fn parse_int(s: &str) -> Option<i128> {
    recog_core::py::parse_int(s).ok().map(i128::from)
}

/// One `start-end`, `start-` or `-suffix` range as a half-open interval; `None` for
/// a part Starlette skips.
fn parse_range(part: &str, size: i128) -> Option<(i128, i128)> {
    if part.is_empty() || part == "-" {
        return None;
    }
    let (start, end) = part.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() {
        return Some((size - parse_int(end)?, size));
    }
    let start = parse_int(start)?;
    if end.is_empty() {
        return Some((start, size));
    }
    Some((start, parse_int(end)?.saturating_add(1).min(size)))
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
    if how.conditional && not_modified(text(header::IF_NONE_MATCH), text(header::IF_MODIFIED_SINCE), &stat) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        set(response.headers_mut(), header::ETAG, &stat.etag);
        set(response.headers_mut(), header::CACHE_CONTROL, CACHE_CONTROL);
        return Some(response);
    }
    let Some(range) = text(header::RANGE).filter(|_| range_applies(text(header::IF_RANGE), &stat)) else {
        let body = if how.head { Body::empty() } else { Body::from(std::fs::read(path).unwrap_or_default()) };
        return Some(with_headers(Response::new(body), StatusCode::OK, headers));
    };
    let ranges = match parse_range_header(range, stat.size) {
        Ok(ranges) => ranges,
        Err(error) => return Some(range_error(error, stat.size)),
    };
    let file = RangedFile { path, media: &media, size: stat.size, head: how.head };
    Some(match ranges[..] {
        [(start, end)] => file.single(start, end, headers),
        _ => file.multipart(&ranges, headers),
    })
}

/// `StaticFiles.is_not_modified`: a matching `If-None-Match` tag, else an
/// `If-Modified-Since` not before `Last-Modified`.
fn not_modified(if_none_match: Option<&str>, if_modified_since: Option<&str>, stat: &Stat) -> bool {
    match if_none_match.filter(|v| !v.is_empty()) {
        Some(tags) => tags.split(',').any(|tag| tag.trim_matches(|c| c == ' ' || c == 'W' || c == '/') == stat.etag),
        None => if_modified_since.is_some_and(|since| not_modified_since(since, &stat.last_modified)),
    }
}

/// Whether a `Range` is honoured: without `If-Range`, or when it names the current
/// `Last-Modified` or ETag.
fn range_applies(if_range: Option<&str>, stat: &Stat) -> bool {
    if_range.is_none_or(|tag| tag == stat.last_modified || tag == stat.etag)
}

fn range_error(error: RangeError, size: u64) -> Response {
    match error {
        RangeError::Malformed(message) => responses::plain(StatusCode::BAD_REQUEST, message, &[]),
        RangeError::NotSatisfiable => responses::plain(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "",
            &[(header::CONTENT_RANGE, format!("bytes */{size}"))],
        ),
    }
}

fn with_headers(mut response: Response, status: StatusCode, headers: HeaderMap) -> Response {
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

/// A file answered with byte ranges.
struct RangedFile<'a> {
    path: &'a Path,
    media: &'a str,
    size: u64,
    head: bool,
}

impl RangedFile<'_> {
    fn body(&self, data: Vec<u8>) -> Body {
        if self.head { Body::empty() } else { Body::from(data) }
    }

    /// One range: `206` with its `Content-Range`.
    fn single(&self, start: u64, end: u64, mut headers: HeaderMap) -> Response {
        set(&mut headers, header::CONTENT_RANGE, &format!("bytes {start}-{}/{}", end - 1, self.size));
        set(&mut headers, header::CONTENT_LENGTH, &(end - start).to_string());
        let body = self.body(read_range(self.path, start, end).unwrap_or_default());
        with_headers(Response::new(body), StatusCode::PARTIAL_CONTENT, headers)
    }

    /// Several ranges: `206` with a `multipart/byteranges` body.
    fn multipart(&self, ranges: &[(u64, u64)], mut headers: HeaderMap) -> Response {
        let boundary = uuid::Uuid::new_v4().simple().to_string()[..26].to_owned();
        let mut body = Vec::new();
        for (start, end) in ranges {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Type: {}\r\nContent-Range: bytes {start}-{}/{}\r\n\r\n",
                    self.media,
                    end - 1,
                    self.size
                )
                .as_bytes(),
            );
            body.extend(read_range(self.path, *start, *end).unwrap_or_default());
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--").as_bytes());
        set(&mut headers, header::CONTENT_TYPE, &format!("multipart/byteranges; boundary={boundary}"));
        set(&mut headers, header::CONTENT_LENGTH, &body.len().to_string());
        with_headers(Response::new(self.body(body)), StatusCode::PARTIAL_CONTENT, headers)
    }
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

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt as _;

    use super::*;

    const CONTENT: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

    fn file(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, CONTENT).unwrap();
        (dir, path)
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(header::HeaderName::from_bytes(name.as_bytes()).unwrap(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    const GET: Serve = Serve { head: false, conditional: true };
    const HEAD: Serve = Serve { head: true, conditional: true };

    async fn answer(path: &Path, request: &[(&str, &str)], how: Serve) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = serve(path, &headers(request), how).expect("a file");
        let (parts, body) = response.into_parts();
        (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
    }

    fn value<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
        headers.get(name).map_or("", |v| v.to_str().unwrap())
    }

    #[tokio::test]
    async fn plain_request_when_served_then_the_whole_file_with_its_validators() {
        let (_dir, path) = file("app.js");

        let (status, headers, body) = answer(&path, &[], GET).await;

        assert_eq!((status, body.as_slice()), (StatusCode::OK, CONTENT));
        assert_eq!(value(&headers, "content-type"), "text/javascript; charset=utf-8");
        assert_eq!(value(&headers, "content-length"), "36");
        assert_eq!(value(&headers, "accept-ranges"), "bytes");
        assert_eq!(value(&headers, "cache-control"), "no-cache");
        assert!(value(&headers, "etag").starts_with('"') && value(&headers, "etag").len() == 34);
        assert!(value(&headers, "last-modified").ends_with(" GMT"));
    }

    #[tokio::test]
    async fn head_request_when_served_then_headers_without_a_body() {
        let (_dir, path) = file("app.js");

        let (status, headers, body) = answer(&path, &[], HEAD).await;
        assert_eq!((status, value(&headers, "content-length"), body.len()), (StatusCode::OK, "36", 0));
        let (status, _, body) = answer(&path, &[("range", "bytes=0-9")], HEAD).await;
        assert_eq!((status, body.len()), (StatusCode::PARTIAL_CONTENT, 0));
        let (status, _, body) = answer(&path, &[("range", "bytes=0-1,5-6")], HEAD).await;
        assert_eq!((status, body.len()), (StatusCode::PARTIAL_CONTENT, 0));
    }

    #[tokio::test]
    async fn matching_validators_when_revalidated_then_304_unless_not_conditional() {
        let (_dir, path) = file("app.js");
        let (_, first, _) = answer(&path, &[], GET).await;
        let (etag, modified) = (value(&first, "etag").to_owned(), value(&first, "last-modified").to_owned());
        let weak = format!("W/\"other\", W/{etag}");

        for request in [
            vec![("if-none-match", etag.as_str())],
            vec![("if-none-match", weak.as_str())],
            vec![("if-modified-since", modified.as_str())],
        ] {
            let (status, headers, body) = answer(&path, &request, GET).await;

            assert_eq!((status, body.len()), (StatusCode::NOT_MODIFIED, 0), "{request:?}");
            assert_eq!((value(&headers, "etag"), value(&headers, "cache-control")), (etag.as_str(), "no-cache"));
        }
        let (status, _, _) =
            answer(&path, &[("if-none-match", &etag)], Serve { head: false, conditional: false }).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn stale_validators_when_revalidated_then_the_file() {
        let (_dir, path) = file("app.js");
        for request in [
            vec![("if-none-match", "W/\"other\", \"x\"")],
            vec![("if-modified-since", "Thu, 01 Jan 1970 00:00:00 GMT")],
            vec![("if-modified-since", "not a date")],
            vec![("if-none-match", ""), ("if-modified-since", "Thu, 01 Jan 1970 00:00:00 GMT")],
        ] {
            assert_eq!(answer(&path, &request, GET).await.0, StatusCode::OK, "{request:?}");
        }
    }

    #[tokio::test]
    async fn one_range_when_served_then_206_with_that_slice() {
        let (_dir, path) = file("app.js");
        for (range, content_range, slice) in [
            ("bytes=0-9", "bytes 0-9/36", &CONTENT[..10]),
            ("bytes=30-", "bytes 30-35/36", &CONTENT[30..]),
            ("bytes=-6", "bytes 30-35/36", &CONTENT[30..]),
            ("bytes=30-999", "bytes 30-35/36", &CONTENT[30..]),
            ("BYTES = 1-2, , -, x", "bytes 1-2/36", &CONTENT[1..3]),
            ("bytes=0-4,3-8", "bytes 0-8/36", &CONTENT[..9]),
        ] {
            let (status, headers, body) = answer(&path, &[("range", range)], GET).await;

            assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{range}");
            assert_eq!(value(&headers, "content-range"), content_range, "{range}");
            assert_eq!(value(&headers, "content-length"), slice.len().to_string(), "{range}");
            assert_eq!(body, slice, "{range}");
        }
    }

    #[tokio::test]
    async fn several_ranges_when_served_then_multipart_byteranges_in_order() {
        let (_dir, path) = file("app.js");

        let (status, headers, body) = answer(&path, &[("range", "bytes=10-14,0-4")], GET).await;

        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        let content_type = value(&headers, "content-type");
        let boundary = content_type.strip_prefix("multipart/byteranges; boundary=").unwrap();
        assert_eq!(boundary.len(), 26);
        let expected = format!(
            "--{boundary}\r\nContent-Type: text/javascript; charset=utf-8\r\nContent-Range: bytes 0-4/36\r\n\r\n01234\r\n\
             --{boundary}\r\nContent-Type: text/javascript; charset=utf-8\r\nContent-Range: bytes 10-14/36\r\n\r\nabcde\r\n\
             --{boundary}--"
        );
        assert_eq!(String::from_utf8(body).unwrap(), expected);
        assert_eq!(value(&headers, "content-length"), expected.len().to_string());
    }

    #[tokio::test]
    async fn malformed_or_unsatisfiable_range_when_served_then_400_or_416() {
        let (_dir, path) = file("app.js");
        for (range, status, text) in [
            ("bytes", StatusCode::BAD_REQUEST, "Malformed range header."),
            ("items=0-1", StatusCode::BAD_REQUEST, "Only support bytes range"),
            ("bytes=x-y", StatusCode::BAD_REQUEST, "Range header: range must be requested"),
            ("bytes=-x", StatusCode::BAD_REQUEST, "Range header: range must be requested"),
            ("bytes=1-x", StatusCode::BAD_REQUEST, "Range header: range must be requested"),
            ("bytes=5-2", StatusCode::BAD_REQUEST, "Range header: start must be less than end"),
            ("bytes=99999999-", StatusCode::RANGE_NOT_SATISFIABLE, ""),
            ("bytes=-99", StatusCode::RANGE_NOT_SATISFIABLE, ""),
        ] {
            let (answer_status, headers, body) = answer(&path, &[("range", range)], GET).await;

            assert_eq!((answer_status, String::from_utf8(body).unwrap().as_str()), (status, text), "{range}");
            if status == StatusCode::RANGE_NOT_SATISFIABLE {
                assert_eq!(value(&headers, "content-range"), "bytes */36");
            }
        }
    }

    #[tokio::test]
    async fn if_range_when_served_then_the_range_only_for_current_validators() {
        let (_dir, path) = file("app.js");
        let (_, first, _) = answer(&path, &[], GET).await;
        let (etag, modified) = (value(&first, "etag").to_owned(), value(&first, "last-modified").to_owned());

        for (if_range, status) in [
            (etag.as_str(), StatusCode::PARTIAL_CONTENT),
            (modified.as_str(), StatusCode::PARTIAL_CONTENT),
            ("\"stale\"", StatusCode::OK),
        ] {
            let (answer_status, _, _) = answer(&path, &[("range", "bytes=0-9"), ("if-range", if_range)], GET).await;

            assert_eq!(answer_status, status, "{if_range}");
        }
    }

    #[test]
    fn directory_or_missing_path_when_served_then_none() {
        let dir = tempfile::tempdir().unwrap();

        assert!(serve(dir.path(), &HeaderMap::new(), GET).is_none());
        assert!(serve(&dir.path().join("absent"), &HeaderMap::new(), GET).is_none());
    }

    #[test]
    fn extensions_of_a_web_ui_when_typed_then_their_media_type_else_text_plain() {
        for (name, media) in [
            ("a.JS", "text/javascript"),
            ("a.mjs", "text/javascript"),
            ("a.css", "text/css"),
            ("a.htm", "text/html"),
            ("a.html", "text/html"),
            ("a.svg", "image/svg+xml"),
            ("a.json", "application/json"),
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.ico", "image/x-icon"),
            ("a.woff", "font/woff"),
            ("a.woff2", "font/woff2"),
            ("a.ttf", "font/ttf"),
            ("a.wav", "audio/x-wav"),
            ("a.md", "text/markdown"),
            ("a.bin", "text/plain"),
            ("Makefile", "text/plain"),
        ] {
            assert_eq!(guess_type(Path::new(name)), media, "{name}");
        }
        assert_eq!(content_type("text/css"), "text/css; charset=utf-8");
        assert_eq!(content_type("text/css; charset=latin-1"), "text/css; charset=latin-1");
        assert_eq!(content_type("image/png"), "image/png");
    }
}
