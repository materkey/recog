//! What a described tab group, the account button and the menu bar say.
//!
//! The rules follow the code that words the tree in Zoom's `zVideoUI` binary
//! (`docs/zoom-app.md`, "How the tree is read"). Positive evidence only: wording
//! outside the vocabulary means "mute unknown, not speaking", never a guess.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::py::{self, PyResult};
use crate::vocabulary::Vocabulary;

/// The speaker mark, a literal in the Zoom binary with no entry in its localization
/// files: it reads the same in every interface language. So do the video states.
pub const ACTIVE_TOKEN: &str = "active speaker";
pub const VIDEO_TOKENS: [&str; 2] = ["video on", "video off"];

/// A participant tile: the display name, the mute state if worded, the speaker mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub name: String,
    pub muted: Option<bool>,
    pub active: bool,
}

impl Tile {
    pub fn new(name: &str, muted: Option<bool>, active: bool) -> Self {
        Self { name: name.to_owned(), muted, active }
    }
}

/// The comma-separated parts of a description, stripped and lower-cased.
pub fn tokens(text: &str) -> BTreeSet<String> {
    text.split(',').map(py::strip).filter(|token| !token.is_empty()).map(str::to_lowercase).collect()
}

/// The name inside `View <name>'s profile`, the name tag of a participant whose
/// profile card Zoom can show (not localized).
fn profile_name(label: &str) -> Option<&str> {
    let name = label.strip_prefix("View ")?.strip_suffix("'s profile")?;
    (!name.is_empty()).then_some(name)
}

fn names_participant(description: &str, name: &str) -> bool {
    description == name || description.strip_prefix(name).is_some_and(|rest| rest.starts_with(','))
}

/// Display name of the participant a described tab group shows, else `None`.
///
/// The description starts with the name, and so a button must name the same
/// participant: bare, or as "View <name>'s profile". A name may contain a comma, so
/// it cannot be cut out of the description alone.
pub fn tile_name(description: &str, buttons: &[String]) -> Option<String> {
    for label in buttons {
        let candidates = profile_name(label).into_iter().chain(std::iter::once(label.as_str()));
        if let Some(name) = candidates.into_iter().find(|name| names_participant(description, name)) {
            return Some(name.to_owned());
        }
    }
    None
}

/// `tile_name` over the values a snapshot carries: a non-string label or, once a
/// label is compared, a non-string description fails as it does in Python.
pub(crate) fn tile_name_value(description: &Value, buttons: &[Value]) -> PyResult<Option<String>> {
    for label in buttons {
        let label = match label {
            Value::String(label) => label,
            _ => return Err(py::PyError("expected string or bytes-like object".into())),
        };
        let description = py::text(description, "startswith")?;
        if let Some(name) = tile_name(description, std::slice::from_ref(label)) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Split `<name>, <state>, <state>…` into mute and speaker state.
pub fn parse_tile(name: &str, description: &str, vocabulary: &Vocabulary) -> Tile {
    let rest = description.strip_prefix(name).unwrap_or("");
    let tokens = tokens(rest);
    let muted = if tokens.iter().any(|token| vocabulary.unmuted.contains(token)) {
        Some(false)
    } else if tokens.iter().any(|token| vocabulary.muted.contains(token)) {
        Some(true)
    } else {
        None
    };
    Tile { name: name.to_owned(), muted, active: tokens.contains(ACTIVE_TOKEN) }
}

/// Display name from the account button, `Zoom, <name>, <status>, <plan>`, else `None`.
pub fn account_name(description: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = description.unwrap_or("").split(',').map(py::strip).collect();
    (parts.len() >= 3 && parts[0] == "Zoom" && !parts[1].is_empty()).then(|| parts[1].to_owned())
}

pub(crate) fn account_name_value(description: Option<&Value>) -> PyResult<Option<String>> {
    match description {
        Some(value) if py::truthy(value) => Ok(account_name(Some(py::text(value, "split")?))),
        _ => Ok(None),
    }
}

/// A menu item that names an account: `Ivan Petrov(ivan@example.org)` in the
/// "Switch account" and "Sign out" submenus of the application menu.
static ACCOUNT_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(.+?)[\s\x1c-\x1f]*\(([^()\s\x1c-\x1f]+@[^()\s\x1c-\x1f]+)\))$").expect("valid pattern")
});

/// The paths of a menu given as a value, with their length and the item at `at`.
fn menu_items(menu: Option<&Value>, length: usize, at: usize) -> PyResult<Vec<String>> {
    let mut found = Vec::new();
    for path in py::items_or_empty(menu)? {
        if py::len(&path)? == length {
            found.push(py::strip(py::text(&py::index(&path, at)?, "strip")?).to_owned());
        }
    }
    Ok(found)
}

fn typed_menu(menu: Option<&[Vec<String>]>) -> Option<Value> {
    menu.map(|paths| Value::from(paths.iter().map(|path| Value::from(path.clone())).collect::<Vec<_>>()))
}

/// Display name from the account items of the menu bar, else `None`.
///
/// The menu bar is there when the main window, whose account button names the user,
/// is closed. Only items of a submenu count (menu > submenu > item). With several
/// accounts signed in it cannot tell which one is in the meeting, so it names nobody.
pub fn menu_account_name(menu: Option<&[Vec<String>]>) -> Option<String> {
    menu_account_name_value(typed_menu(menu).as_ref()).expect("a list of string lists")
}

pub(crate) fn menu_account_name_value(menu: Option<&Value>) -> PyResult<Option<String>> {
    let names: BTreeSet<String> = menu_items(menu, 3, 2)?
        .iter()
        .filter_map(|item| ACCOUNT_ITEM.captures(item).map(|found| found[1].to_owned()))
        .collect();
    Ok(if names.len() == 1 { names.into_iter().next() } else { None })
}

/// Whether the user's own microphone is closed, from the meeting menu's audio item.
///
/// `None` without a menu bar, as from a helper built before it was reported, and
/// when no item carries known wording.
pub fn own_mic_muted(menu: Option<&[Vec<String>]>, vocabulary: &Vocabulary) -> Option<bool> {
    own_mic_muted_value(typed_menu(menu).as_ref(), vocabulary).expect("a list of string lists")
}

pub(crate) fn own_mic_muted_value(menu: Option<&Value>, vocabulary: &Vocabulary) -> PyResult<Option<bool>> {
    let titles: BTreeSet<String> = menu_items(menu, 2, 1)?.iter().map(|title| title.to_lowercase()).collect();
    Ok(if titles.iter().any(|title| vocabulary.mic_open_item.contains(title)) {
        Some(false)
    } else if titles.iter().any(|title| vocabulary.mic_closed_item.contains(title)) {
        Some(true)
    } else {
        None
    })
}
