//! The native Zoom application as a conference signal source (macOS), in place of
//! `recog/zoom_app.py`.
//!
//! The Zoom application is not a web page, so the Chrome extension cannot see it. The
//! Swift helper `recog/native/macos/zoom_ax_probe.swift` reads its Accessibility tree
//! and writes one JSON snapshot per line; this crate turns the snapshots into the
//! frames the extension sends to the bridge (`meeting_join`, `participants_update`,
//! `speaker_update`, `meeting_leave`), so the bridge, attribution, Copilot and
//! increments need no Zoom-specific code. See `docs/zoom-app.md`.
//!
//! The helper only reports the tree; what a described tab group means is decided here
//! ([`tile`], [`session`]). Its code must not change for this crate: any change of
//! the helper changes its signature and costs the user the Accessibility grant.
//!
//! - [`session::ZoomAppSession`]: snapshots to frames, pure.
//! - [`vocabulary::load_vocabulary`]: the mute wording of every language the installed
//!   Zoom ships.
#![forbid(unsafe_code)]

pub mod frame;
pub mod py;
pub mod session;
pub mod tile;
pub mod vocabulary;

pub use frame::{Frame, MEETING_URL, PLATFORM};
pub use session::{JOIN_RETRY_S, LEAVE_DEBOUNCE_S, ZoomAppSession};
pub use tile::{Tile, account_name, menu_account_name, own_mic_muted, parse_tile, tile_name};
pub use vocabulary::{Vocabulary, load_vocabulary};
