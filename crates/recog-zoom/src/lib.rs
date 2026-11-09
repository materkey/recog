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
//! - [`helper_app::find_or_build_helper`]: `RecogZoomHelper.app`, rebuilt only when
//!   its source's content changes.
//! - [`helper_link::helper_lines`], [`bridge::serve`], [`run::run_with`]: the helper
//!   started through LaunchServices, its stream, the bridge link, the restarts.
//!
//! The crate builds and is tested on Linux as well; only the entry points that start
//! the real helper are macOS-only.
#![forbid(unsafe_code)]

pub mod bridge;
pub mod frame;
pub mod helper_app;
pub mod helper_link;
pub mod log;
pub mod py;
pub mod run;
pub mod session;
pub mod settings;
pub mod tile;
pub mod vocabulary;

pub use frame::{Frame, MEETING_URL, PLATFORM};
pub use session::{JOIN_RETRY_S, LEAVE_DEBOUNCE_S, ZoomAppSession};
pub use tile::{Tile, account_name, menu_account_name, own_mic_muted, parse_tile, tile_name};
pub use vocabulary::{Vocabulary, load_vocabulary};
