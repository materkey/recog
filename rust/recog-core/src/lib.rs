//! The core of recog in Rust: the installation settings, the data directory, the
//! local user profile, the WebSocket origin allowlist, the privacy setting and the
//! meeting storage (docs/plans/2026-11-07-rust-migration.md, "Target layout").
//!
//! Each module ports one Python module and is held to it by the parity tests under
//! `tests/`, which run the Python implementation on the same inputs and compare what
//! both produce: files byte for byte, values as JSON, errors by message. Where Python
//! and Rust legitimately differ, the module's documentation says so.
//!
//! No caller uses the crate yet: Python stays the default until a setting switches a
//! part over.
#![forbid(unsafe_code)]

pub mod data_dir;
pub mod install_config;
pub mod paths;
pub mod privacy;
pub mod py;
pub mod pyjson;
pub mod time;
#[rustfmt::skip]
mod unicode_tables;
pub mod url;
pub mod user_profile;
pub mod ws_origin;
