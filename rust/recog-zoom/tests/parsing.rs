//! Reading a tile, the account and the menu bar; the wording of the installed Zoom.
//! Ported from the parsing part of `tests/test_zoom_app.py`.
#![forbid(unsafe_code)]

mod common;

use std::collections::BTreeSet;

use common::*;
use recog_zoom::{
    Tile, Vocabulary, ZoomAppSession, account_name, load_vocabulary, menu_account_name, own_mic_muted, parse_tile,
    tile_name,
};

fn set(words: &[&str]) -> BTreeSet<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn given_tile_description_when_parsed_then_only_known_wording_counts() {
    let cases = [
        ("Alice, Computer audio unmuted, Video off, active speaker", Tile::new("Alice", Some(false), true)),
        ("Alice, Computer audio muted, Video off", Tile::new("Alice", Some(true), false)),
        ("Alice, Video off", Tile::new("Alice", None, false)),
        ("Alice", Tile::new("Alice", None, false)),
        ("Alice, Звук компьютера включён, активный докладчик", Tile::new("Alice", None, false)),
        ("Alice, No audio connected, Video off", Tile::new("Alice", Some(true), false)),
        (
            "Alice, Computer audio unmuted, Video on, active speaker, Local time 11:00, Host",
            Tile::new("Alice", Some(false), true),
        ),
        // A room with name tags for the people in it words only the speaking one.
        ("Alice, Bob, active speaker", Tile::new("Alice", None, true)),
        ("Someone else, Computer audio muted, active speaker", Tile::new("Alice", None, false)),
    ];
    for (description, expected) in cases {
        assert_eq!(parse_tile("Alice", description, &Vocabulary::english()), expected, "{description}");
    }
}

#[test]
fn given_tab_group_when_named_then_a_button_must_name_the_same_participant() {
    let cases: [(&str, &[&str], Option<&str>); 11] = [
        ("Alice, Computer audio muted, Video off", &["Alice"], Some("Alice")),
        ("Alice, Computer audio muted, Video off", &["View Alice's profile"], Some("Alice")),
        ("Alice", &["Alice"], Some("Alice")),
        ("Doe, Jane, Computer audio muted", &["View Doe, Jane's profile"], Some("Doe, Jane")),
        ("O'Neil, Video on", &["View O'Neil's profile"], Some("O'Neil")),
        // A display name that reads like the wrapper is still a display name.
        ("View Bob's profile, Video on", &["View Bob's profile"], Some("View Bob's profile")),
        ("Alice, Video on", &["Pin", "View Alice's profile"], Some("Alice")),
        ("Alice, Video on", &["View Bob's profile"], None),
        ("Alice, Video on", &["Al"], None),
        ("Alice, Video on", &["Open Alice's card"], None),
        ("Alice, Video on", &[], None),
    ];
    for (description, buttons, expected) in cases {
        let buttons: Vec<String> = buttons.iter().map(|label| (*label).to_owned()).collect();
        assert_eq!(tile_name(description, &buttons).as_deref(), expected, "{description} {buttons:?}");
    }
}

#[test]
fn given_installed_zoom_when_vocabulary_is_loaded_then_it_has_its_mute_item_wording() {
    let root = tempfile::tempdir().unwrap();
    let russian: &[(&str, &str)] =
        &[("Mute Audio", "Выключить звук"), ("Unmute Audio", "Включить звук"), ("Join Audio", "Подключить звук")];
    let vocabulary = load_vocabulary(&[zoom_app(root.path(), &[("ru", russian)])]);

    assert_eq!(vocabulary.mic_open_item, set(&["выключить звук", "mute audio", "mute telephone"]));
    assert_eq!(
        vocabulary.mic_closed_item,
        set(&["включить звук", "подключить звук", "unmute audio", "unmute telephone", "join audio"])
    );
    let item = |title: &str| vec![vec!["Конференция".to_owned(), title.to_owned()]];
    assert_eq!(own_mic_muted(Some(&item("Выключить звук")), &vocabulary), Some(false));
    assert_eq!(own_mic_muted(Some(&item("Включить звук")), &vocabulary), Some(true));
}

#[test]
fn given_installed_zoom_when_vocabulary_is_loaded_then_it_has_its_mute_wording_and_english() {
    let root = tempfile::tempdir().unwrap();
    let vocabulary = load_vocabulary(&[zoom_app(root.path(), &[("ru", RUSSIAN)])]);

    let muted = set(&[
        "звук компьютера выключен",
        "звук телефона выключен",
        "звук не подключен",
        "computer audio muted",
        "no audio connected",
    ]);
    assert!(muted.is_subset(&vocabulary.muted));
    assert!(set(&["звук компьютера включен", "звук включен", "computer audio unmuted"]).is_subset(&vocabulary.unmuted));
    for video in ["видео выкл.", "видео не подключено"] {
        assert!(!vocabulary.muted.contains(video) && !vocabulary.unmuted.contains(video));
    }
}

#[test]
fn given_string_ids_of_another_zoom_version_when_loaded_then_keys_still_match() {
    let root = tempfile::tempdir().unwrap();
    let renumbered: &[(&str, &str)] = &[("LN_Plist_Accessibility_VoIP_Muted_99999", "Ton aus")];

    assert!(load_vocabulary(&[zoom_app(root.path(), &[("de", renumbered)])]).muted.contains("ton aus"));
}

#[test]
fn given_no_zoom_or_an_unreadable_file_when_loaded_then_english_wording_remains() {
    let root = tempfile::tempdir().unwrap();
    let app = zoom_app(root.path(), &[("ru", RUSSIAN)]);
    std::fs::write(app.join("Contents/Resources/ru.lproj/Localizable.strings"), b"\xff\xfe\x00\xd8").unwrap();

    assert_eq!(load_vocabulary(&[root.path().join("absent.app"), app]), Vocabulary::english());
}

#[test]
fn given_russian_interface_when_a_tile_is_parsed_with_loaded_wording_then_mute_and_speaker_are_read() {
    let root = tempfile::tempdir().unwrap();
    let vocabulary = load_vocabulary(&[zoom_app(root.path(), &[("ru", RUSSIAN)])]);

    let muted = parse_tile("Иван", "Иван, Звук компьютера выключен, Видео выкл., active speaker", &vocabulary);
    let speaking = parse_tile("Иван", "Иван, Звук компьютера включен, Видео выкл., active speaker", &vocabulary);

    assert_eq!(muted, Tile::new("Иван", Some(true), true));
    assert_eq!(speaking, Tile::new("Иван", Some(false), true));
}

#[test]
fn given_russian_interface_when_session_runs_with_loaded_wording_then_a_muted_speaker_is_not_reported() {
    let root = tempfile::tempdir().unwrap();
    let mut session = ZoomAppSession::new(load_vocabulary(&[zoom_app(root.path(), &[("ru", RUSSIAN)])]));
    frames(&mut session, &meeting(&[tile("Host", &[])]), 100.0);

    let sent = frames(
        &mut session,
        &meeting(&[
            tile("Иван", &["Звук компьютера выключен", "active speaker"]),
            tile("Анна", &["Звук компьютера включен"]),
        ]),
        101.0,
    );

    let muted: Vec<_> = participants(&sent).iter().map(|p| p["micMuted"].clone()).collect();
    assert_eq!(muted, [true, false]);
    assert!(speaker_of(&sent).is_null());
}

#[test]
fn given_name_with_a_comma_when_parsed_then_the_name_is_not_read_as_state() {
    let parsed = parse_tile(
        "Doe, Jane muted",
        "Doe, Jane muted, Computer audio unmuted, active speaker",
        &Vocabulary::english(),
    );
    assert_eq!(parsed, Tile::new("Doe, Jane muted", Some(false), true));
}

#[test]
fn given_account_button_description_when_read_then_display_name_or_none() {
    let cases = [
        (Some(ACCOUNT), Some("Me")),
        (Some("Zoom, Иван Петров, Available, Basic account"), Some("Иван Петров")),
        (Some("Zoom, Me"), None),
        (Some("Search, Me, something"), None),
        (Some(""), None),
        (None, None),
    ];
    for (description, expected) in cases {
        assert_eq!(account_name(description).as_deref(), expected, "{description:?}");
    }
}

fn paths(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter().map(|row| row.iter().map(|item| (*item).to_owned()).collect()).collect()
}

#[test]
fn given_account_items_in_the_menu_when_read_then_the_single_account_or_none() {
    let mut two_accounts = menu_account();
    two_accounts.extend(paths(&[&["Zoom Workplace", "Switch account", "Bob(bob@example.org)"]]));
    let cases = [
        (Some(menu_account()), Some("Me")),
        (Some(paths(&[&["Zoom Workplace", "Switch account", "Иван Петров (ivan@example.org)"]])), Some("Иван Петров")),
        (Some(two_accounts), None),
        (Some(paths(&[&["Zoom Workplace", "Switch account", "Add account"]])), None),
        // Not a submenu item: a window title in the Window menu, say.
        (Some(paths(&[&["Window", "Chat(team@example.org)"]])), None),
        (None, None),
    ];
    for (menu, expected) in cases {
        assert_eq!(menu_account_name(menu.as_deref()).as_deref(), expected, "{menu:?}");
    }
}

#[test]
fn given_meeting_menu_when_read_then_own_microphone_follows_known_wording_only() {
    let cases = [
        (Some(menu(Some("Mute audio"))), Some(false)),
        (Some(menu(Some("Unmute audio"))), Some(true)),
        (Some(menu(Some("Join audio"))), Some(true)),
        (Some(menu(Some("Mute telephone"))), Some(false)),
        (Some(menu(Some("Unmute telephone"))), Some(true)),
        // A webinar attendee who cannot unmute, or wording this version does not know.
        (Some(menu(None)), None),
        (Some(menu(Some("Mute my microphone"))), None),
        // The item only, not a deeper one under another menu.
        (Some(paths(&[&["Meeting", "Audio", "Mute audio"]])), None),
        (None, None),
    ];
    for (menu, expected) in cases {
        assert_eq!(own_mic_muted(menu.as_deref(), &Vocabulary::english()), expected, "{menu:?}");
    }
}

#[test]
fn given_strings_file_big_endian_or_without_a_byte_order_mark_when_loaded_then_only_the_marked_one_is_read() {
    // Python reads the files as a text stream, whose UTF-16 decoder needs the mark.
    let root = tempfile::tempdir().unwrap();
    let app = zoom_app(root.path(), &[("de", &[]), ("fr", &[])]);
    let entry = "\"LN_Plist_Accessibility_VoIP_Muted_1\" = \"  Ton AUS \";\r\n";
    let little: Vec<u8> = entry.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut big = vec![0xfe, 0xff];
    big.extend("\"LN_Audio_Unmuted_Accessibility_2\" = \"Son\\\"activé\";".encode_utf16().flat_map(u16::to_be_bytes));
    std::fs::write(app.join("Contents/Resources/de.lproj/Localizable.strings"), little).unwrap();
    std::fs::write(app.join("Contents/Resources/fr.lproj/Localizable.strings"), big).unwrap();

    let vocabulary = load_vocabulary(&[app]);

    assert!(!vocabulary.muted.contains("ton aus"));
    assert!(vocabulary.unmuted.contains("son\\\"activé"));
}
