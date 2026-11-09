//! The web backend of recog in Rust (docs/plans/2026-11-07-rust-migration.md, "Target
//! layout"), in place of `recog/web/`: the HTTP API over the meetings, increments,
//! titles, meeting chat, user profile, privacy and AI settings; the live stream and
//! the per-meeting watch WebSockets; the bidirectional link to the bridge; and the
//! static web UI, served unchanged from `crates/recog-web/static/`.
//!
//! The route list is in [`router`]. Bodies, status codes, the headers that matter
//! (content type and length, ETag, ranges, cache control) and the frames on the
//! WebSockets are Python's, pinned by the tests of each route since the Python server
//! and the differential tests that compared the two were deleted.
//!
//! Known differences from Python, beyond those of the modules below:
//! - FastAPI's generated documentation (`/openapi.json`, `/docs`, `/redoc`) is not
//!   served;
//! - `/api/debug` reports the host and port this server listens on; Python reports
//!   its module constants (127.0.0.1:9080) whatever uvicorn was given;
//! - the desktop integration's worker is `recog-web desktop-worker`, not
//!   `python -m recog.desktop.worker` (see `recog-desktop` for its own differences);
//! - log lines that quote a Python exception quote the Rust error instead;
//! - `RECOG_WEB_KTALK_DEBUG_FILE` moves the bridge's debug file
//!   (`/tmp/ktalk_debug.json`), for tests.
#![forbid(unsafe_code)]
// A route's error is the HTTP response Python raises (`HTTPException`); boxing every one
// would only add noise to the handlers.
#![allow(clippy::result_large_err)]

pub mod ai_provider;
pub mod ai_settings;
pub mod analysis;
pub mod audio;
pub mod bridge_subscriber;
pub mod config;
pub mod desktop;
pub mod json;
pub mod live_stream;
pub mod live_watcher;
pub mod meeting_catalog;
pub mod request;
pub mod responses;
pub mod router;
pub mod routes;
pub mod server;
pub mod static_files;
pub mod tail;
pub mod transcript;
pub mod transcript_watcher;
pub mod user_profile;
pub mod util;
pub mod validation;
pub mod watch;
