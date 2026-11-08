//! Snapshots as the helper writes them, and stand-ins for Zoom, the toolchain, the
//! helper and the bridge. The shapes are those of `tests/test_zoom_app.py`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use recog_zoom::{Frame, ZoomAppSession};
use serde_json::{Map, Value, json};

pub const ACCOUNT: &str = "Zoom, Me, In a Zoom Meeting, Basic account";

/// A described tab group as the helper reports it.
pub fn group(description: &str, buttons: &[&str]) -> Value {
    let mut children = vec![json!({"role": "AXImage"})];
    children.extend(buttons.iter().map(|label| json!({"role": "AXButton", "description": label})));
    json!({"description": description, "children": children})
}

/// A participant tile named by a bare name tag.
pub fn tile(name: &str, states: &[&str]) -> Value {
    let mut parts = vec![name];
    parts.extend_from_slice(states);
    group(&parts.join(", "), &[name])
}

/// A meeting snapshot in the window view.
pub fn meeting(tiles: &[Value]) -> Value {
    meeting_in("window", tiles, &[])
}

/// A meeting snapshot with a view and extra keys (`account`, `menu`).
pub fn meeting_in(view: &str, tiles: &[Value], extra: &[(&str, Value)]) -> Value {
    let mut snapshot = Map::new();
    snapshot.insert("state".into(), json!("meeting"));
    snapshot.insert("view".into(), json!(view));
    snapshot.insert("tiles".into(), Value::from(tiles.to_vec()));
    for (key, value) in extra {
        snapshot.insert((*key).into(), value.clone());
    }
    Value::Object(snapshot)
}

pub fn with(snapshot: Value, extra: &[(&str, Value)]) -> Value {
    let mut snapshot = snapshot;
    for (key, value) in extra {
        snapshot[*key] = value.clone();
    }
    snapshot
}

pub fn idle() -> Value {
    json!({"state": "idle"})
}

pub fn menu_account() -> Vec<Vec<String>> {
    vec![
        vec!["Zoom Workplace".into(), "Switch account".into(), "Me(me@example.org)".into()],
        vec!["Zoom Workplace".into(), "Switch account".into(), "Add account".into()],
        vec!["Zoom Workplace".into(), "Sign out".into(), "Me(me@example.org)".into()],
    ]
}

/// The menu bar as the helper reports it, with the meeting menu's audio item.
pub fn menu(audio_item: Option<&str>) -> Vec<Vec<String>> {
    let mut items = vec!["Mute all", "Ask all to unmute"];
    items.extend(audio_item);
    items.push("Start video");
    let mut paths = menu_account();
    paths.extend(items.into_iter().map(|item| vec!["Meeting".to_owned(), item.to_owned()]));
    paths
}

pub fn menu_value(audio_item: Option<&str>) -> Value {
    serde_json::to_value(menu(audio_item)).unwrap()
}

pub fn frames(session: &mut ZoomAppSession, snapshot: &Value, now: f64) -> Vec<Frame> {
    session.frames(snapshot, now).expect("a well-formed snapshot")
}

pub fn joined_session(now: f64) -> ZoomAppSession {
    let mut session = ZoomAppSession::default();
    assert_eq!(kinds(&frames(&mut session, &meeting(&[tile("Host", &[])]), now)), ["meeting_join"]);
    session
}

pub fn kinds(frames: &[Frame]) -> Vec<&'static str> {
    frames.iter().map(Frame::kind).collect()
}

pub fn values(frames: &[Frame]) -> Vec<Value> {
    frames.iter().map(|frame| serde_json::to_value(frame).unwrap()).collect()
}

/// The `speaker` of the first `speaker_update`.
pub fn speaker_of(frames: &[Frame]) -> Value {
    values(frames).into_iter().find(|frame| frame["type"] == "speaker_update").expect("a speaker_update")["speaker"]
        .clone()
}

/// The first frame, which is a `participants_update`.
pub fn participants(frames: &[Frame]) -> Vec<Value> {
    let first = &values(frames)[0];
    assert_eq!(first["type"], "participants_update");
    first["participants"].as_array().unwrap().clone()
}

/// UTF-16 little-endian with a byte-order mark, as Zoom ships its strings files.
pub fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

/// A stand-in Zoom bundle with one `Localizable.strings` per language.
pub fn zoom_app(root: &Path, languages: &[(&str, &[(&str, &str)])]) -> PathBuf {
    let app = root.join("zoom.us.app");
    for (language, entries) in languages {
        let folder = app.join(format!("Contents/Resources/{language}.lproj"));
        std::fs::create_dir_all(&folder).unwrap();
        let body: String = entries.iter().map(|(key, value)| format!("\"{key}\" = \"{value}\";\n")).collect();
        std::fs::write(folder.join("Localizable.strings"), utf16(&body)).unwrap();
    }
    app
}

pub const RUSSIAN: &[(&str, &str)] = &[
    ("LN_Plist_Accessibility_VoIP_Muted_20044", "Звук компьютера выключен"),
    ("LN_Plist_Accessibility_VoIP_Unmuted_20044", "Звук компьютера включен"),
    ("LN_Plist_Accessibility_Tele_Muted_20044", "Звук телефона выключен"),
    ("LN_Audio_Unmuted_Accessibility_1152855", "Звук включен"),
    ("LN_Plist_Accessibility_Video_Off_20044", "Видео выкл."),
    ("LN_Plist_Accessibility_No_Audio_20044", "Звук не подключен"),
    ("LN_Plist_Accessibility_No_Video_20044", "Видео не подключено"),
];
