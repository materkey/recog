//! The wording a tile and the meeting menu use, read from the installed Zoom.
//!
//! A tile's description is in the Zoom interface language. The mute wording of every
//! language Zoom ships is in its own `*.lproj/Localizable.strings`: UTF-16 with a
//! byte-order mark (little-endian in Zoom Workplace 7.1.9), lines of
//! `"key" = "value";`. English is built in for when they cannot be read.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::py;

/// Lower-cased tile wording for a participant who cannot be heard and one who can,
/// and the titles of the meeting menu's audio item while the user's own microphone
/// is open and closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vocabulary {
    pub muted: BTreeSet<String>,
    pub unmuted: BTreeSet<String>,
    pub mic_open_item: BTreeSet<String>,
    pub mic_closed_item: BTreeSet<String>,
}

fn set(words: &[&str]) -> BTreeSet<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

impl Vocabulary {
    /// What Zoom Workplace 7.1.9 says in English; used when the installed
    /// application's own wording cannot be read.
    pub fn english() -> Self {
        Self {
            muted: set(&["computer audio muted", "telephone muted", "audio muted", "no audio connected"]),
            unmuted: set(&["computer audio unmuted", "telephone unmuted", "audio unmuted"]),
            mic_open_item: set(&["mute audio", "mute telephone"]),
            mic_closed_item: set(&["unmute audio", "unmute telephone", "join audio"]),
        }
    }
}

impl Default for Vocabulary {
    fn default() -> Self {
        Self::english()
    }
}

/// Where Zoom is installed: `/Applications` and the user's own `~/Applications`.
pub fn default_app_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/Applications/zoom.us.app")];
    if let Some(home) = std::env::home_dir() {
        paths.push(home.join("Applications/zoom.us.app"));
    }
    paths
}

// Python's `\s` is `str.isspace()`, which also covers U+001C..U+001F.
const SPACE: &str = r"[\s\x1c-\x1f]";

/// Keys of the audio wording on a tile; the numeric suffix is a string id that may
/// differ between Zoom versions. A participant with no audio connected cannot be
/// heard either.
static MUTE_ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#""(?:LN_Plist_Accessibility_(?:VoIP|Tele)_(Muted|Unmuted)_\d+|LN_Audio_(Muted|Unmuted)_Accessibility_\d+|LN_Plist_Accessibility_(No_Audio)_\d+)"{SPACE}*={SPACE}*"((?:[^"\\]|\\.)*)"{SPACE}*;"#
    ))
    .expect("valid pattern")
});

/// The meeting menu's audio item: "Mute Audio" while the user's own microphone is
/// open, "Unmute Audio" while it is muted; telephone audio has its own pair, and
/// "Join Audio" counts as muted, since without audio nobody hears the user.
static MIC_ITEM_ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#""(Mute Audio|Unmute Audio|Mute Telephone|Unmute Telephone|Join Audio)"{SPACE}*={SPACE}*"((?:[^"\\]|\\.)*)"{SPACE}*;"#
    ))
    .expect("valid pattern")
});

const MIC_OPEN_KEYS: [&str; 2] = ["Mute Audio", "Mute Telephone"];

/// Strict UTF-16 as `Path.read_text(encoding="utf-16")` reads it: the byte-order mark
/// picks the order and is dropped, and a file without one is an error ("UTF-16 stream
/// does not start with BOM"; unlike `bytes.decode`, the stream decoder does not fall
/// back to little-endian), as is an odd length or an unpaired surrogate. An empty file
/// is empty text.
pub fn decode_utf16(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let (big_endian, body) = match bytes {
        [] => return Some(String::new()),
        [0xfe, 0xff, rest @ ..] => (true, rest),
        [0xff, 0xfe, rest @ ..] => (false, rest),
        _ => return None,
    };
    let units = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| if big_endian { u16::from_be_bytes(pair) } else { u16::from_le_bytes(pair) });
    char::decode_utf16(units).collect::<Result<String, _>>().ok()
}

/// `Path.read_text()` translates every line ending to `\n`.
fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn strings_files(app: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(app.join("Contents/Resources")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".lproj"))
        .map(|entry| entry.path().join("Localizable.strings"))
        .collect();
    files.sort();
    files
}

fn wording(raw: &str) -> String {
    py::strip(raw).to_lowercase()
}

/// Mute wording in every language the installed Zoom ships, plus English.
///
/// A file that cannot be read or decoded is skipped, as an absent application is.
pub fn load_vocabulary(app_paths: &[PathBuf]) -> Vocabulary {
    let mut vocabulary = Vocabulary::english();
    for app in app_paths {
        for path in strings_files(app) {
            let Some(text) = std::fs::read(&path).ok().and_then(|bytes| decode_utf16(&bytes)) else {
                continue;
            };
            let text = universal_newlines(&text);
            for entry in MUTE_ENTRY.captures_iter(&text) {
                let unmuted = entry.get(1).or(entry.get(2)).is_some_and(|kind| kind.as_str() == "Unmuted");
                let target = if unmuted { &mut vocabulary.unmuted } else { &mut vocabulary.muted };
                target.insert(wording(&entry[4]));
            }
            for entry in MIC_ITEM_ENTRY.captures_iter(&text) {
                let open = MIC_OPEN_KEYS.contains(&&entry[1]);
                let target = if open { &mut vocabulary.mic_open_item } else { &mut vocabulary.mic_closed_item };
                target.insert(wording(&entry[2]));
            }
        }
    }
    vocabulary
}
