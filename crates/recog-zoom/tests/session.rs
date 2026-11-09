//! Snapshots to bridge frames: the meeting's lifecycle, the speaker, the roster and
//! the user's own microphone. Ported from the session part of `tests/test_zoom_app.py`.
#![forbid(unsafe_code)]

mod common;

use common::*;
use recog_zoom::{JOIN_RETRY_S, LEAVE_DEBOUNCE_S, MEETING_URL, ZoomAppSession};
use serde_json::{Value, json};

// --- lifecycle ------------------------------------------------------------------

#[test]
fn given_no_meeting_when_snapshots_arrive_then_nothing_is_sent() {
    for state in ["idle", "zoom_not_running", "not_trusted"] {
        assert!(frames(&mut ZoomAppSession::default(), &json!({"state": state}), 100.0).is_empty());
    }
}

#[test]
fn given_first_meeting_snapshot_when_processed_then_only_the_join_is_sent() {
    let mut session = ZoomAppSession::default();

    let sent = frames(&mut session, &meeting(&[tile("Bob", &["Computer audio unmuted", "active speaker"])]), 100.0);

    assert_eq!(values(&sent), [json!({"type": "meeting_join", "url": MEETING_URL, "platform": "zoom", "title": ""})]);
}

#[test]
fn given_join_preview_without_tiles_when_processed_then_nothing_until_the_first_tile() {
    // The preview shown before "Start" is already the meeting window.
    let mut session = ZoomAppSession::default();

    assert!(frames(&mut session, &meeting(&[]), 100.0).is_empty());
    assert!(!session.joined);
    assert_eq!(kinds(&frames(&mut session, &meeting(&[tile("Me", &[])]), 104.0)), ["meeting_join"]);
}

#[test]
fn given_preview_cancelled_when_the_window_goes_away_then_no_leave_is_sent() {
    let mut session = ZoomAppSession::default();
    frames(&mut session, &meeting(&[]), 100.0);

    assert!(frames(&mut session, &idle(), 100.0 + LEAVE_DEBOUNCE_S + 1.0).is_empty());
}

#[test]
fn given_joined_meeting_when_tiles_vanish_then_it_stays_joined() {
    let mut session = joined_session(100.0);

    let sent = frames(&mut session, &meeting(&[]), 101.0);

    assert!(session.joined);
    assert!(!kinds(&sent).contains(&"meeting_leave"));
}

#[test]
fn given_joined_when_next_snapshot_arrives_then_participants_and_speaker_follow() {
    let mut session = joined_session(100.0);
    let snapshot = meeting_in(
        "window",
        &[
            tile("Me", &["Computer audio muted"]),
            tile("Bob", &["Computer audio unmuted", "Video off", "active speaker"]),
        ],
        &[("account", json!(ACCOUNT))],
    );

    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(
        values(&sent),
        [
            json!({"type": "participants_update", "participants": [
                {"id": "Me", "name": "Me", "isLocal": true, "micMuted": true},
                {"id": "Bob", "name": "Bob", "isLocal": false, "micMuted": false},
            ]}),
            json!({"type": "speaker_update", "speaker": {"id": "Bob", "name": "Bob", "isLocal": false}, "timestamp": 101000}),
        ]
    );
}

#[test]
fn given_unchanged_snapshot_when_heartbeat_repeats_it_then_nothing_is_resent() {
    let mut session = joined_session(100.0);
    let snapshot = meeting(&[tile("Bob", &["Computer audio unmuted", "active speaker"])]);
    frames(&mut session, &snapshot, 101.0);

    assert!(frames(&mut session, &snapshot, 103.0).is_empty());
}

#[test]
fn given_meeting_window_gone_briefly_when_it_returns_then_no_leave_is_sent() {
    let mut session = joined_session(100.0);

    assert!(frames(&mut session, &idle(), 100.0 + LEAVE_DEBOUNCE_S - 0.1).is_empty());
    assert!(session.joined);
}

#[test]
fn given_meeting_window_gone_for_the_debounce_when_processed_then_leave_once() {
    let mut session = joined_session(100.0);

    assert_eq!(
        values(&frames(&mut session, &idle(), 100.0 + LEAVE_DEBOUNCE_S)),
        [json!({"type": "meeting_leave", "reason": "user_left"})]
    );
    assert!(frames(&mut session, &idle(), 200.0).is_empty());
}

#[test]
fn given_left_meeting_when_a_new_one_starts_then_join_and_full_state_are_sent_again() {
    let mut session = joined_session(100.0);
    let bob = meeting(&[tile("Bob", &["active speaker"])]);
    frames(&mut session, &bob, 101.0);
    frames(&mut session, &idle(), 200.0);

    assert_eq!(kinds(&frames(&mut session, &bob, 300.0)), ["meeting_join"]);
    assert_eq!(kinds(&frames(&mut session, &bob, 301.0)), ["participants_update", "speaker_update"]);
}

#[test]
fn given_join_rejected_when_meeting_continues_then_silent_until_the_retry_window_passes() {
    let mut session = joined_session(100.0);
    session.on_rejected(100.5);
    let snapshot = meeting(&[tile("Bob", &["active speaker"])]);

    assert!(frames(&mut session, &snapshot, 101.0).is_empty());
    assert!(frames(&mut session, &snapshot, 100.5 + JOIN_RETRY_S - 0.1).is_empty());
    assert_eq!(kinds(&frames(&mut session, &snapshot, 100.5 + JOIN_RETRY_S)), ["meeting_join"]);
}

#[test]
fn given_rejected_then_meeting_ended_when_a_new_meeting_starts_then_join_is_immediate() {
    let mut session = joined_session(100.0);
    session.on_rejected(100.5);
    frames(&mut session, &idle(), 101.0);

    assert_eq!(kinds(&frames(&mut session, &meeting(&[tile("Host", &[])]), 102.0)), ["meeting_join"]);
}

#[test]
fn given_bridge_link_dropped_when_meeting_continues_then_join_is_sent_again() {
    let mut session = joined_session(100.0);
    let bob = meeting(&[tile("Bob", &["active speaker"])]);
    frames(&mut session, &bob, 101.0);
    session.on_disconnect();

    assert_eq!(kinds(&frames(&mut session, &bob, 105.0)), ["meeting_join"]);
    assert_eq!(kinds(&frames(&mut session, &bob, 106.0)), ["participants_update", "speaker_update"]);
}

// --- speaker and roster -----------------------------------------------------------

#[test]
fn given_active_mark_moves_when_processed_then_speaker_changes() {
    let mut session = joined_session(100.0);
    frames(&mut session, &meeting(&[tile("Alice", &["active speaker"]), tile("Bob", &[])]), 101.0);

    let sent = frames(&mut session, &meeting(&[tile("Alice", &[]), tile("Bob", &["active speaker"])]), 102.0);

    assert_eq!(kinds(&sent), ["speaker_update"]);
    assert_eq!(speaker_of(&sent)["name"], "Bob");
}

#[test]
fn given_nobody_marked_when_processed_then_speaker_is_null() {
    let mut session = joined_session(100.0);

    let sent = frames(&mut session, &meeting(&[tile("Alice", &[]), tile("Bob", &[])]), 101.0);

    assert!(speaker_of(&sent).is_null());
}

#[test]
fn given_marked_speaker_is_muted_when_processed_then_speaker_is_null() {
    let mut session = joined_session(100.0);

    let sent = frames(&mut session, &meeting(&[tile("Alice", &["Computer audio muted", "active speaker"])]), 101.0);

    assert!(speaker_of(&sent).is_null());
}

#[test]
fn given_own_tile_marked_when_processed_then_speaker_is_local() {
    let mut session = joined_session(100.0);

    let snapshot = meeting_in("window", &[tile("Me", &["active speaker"])], &[("account", json!(ACCOUNT))]);
    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(speaker_of(&sent), json!({"id": "Me", "name": "Me", "isLocal": true}));
}

#[test]
fn given_account_seen_before_the_meeting_when_own_tile_is_marked_then_speaker_is_local() {
    // During a meeting the main window that carries the account is not always
    // reachable; the helper reports it while idle as well.
    let mut session = ZoomAppSession::default();
    frames(&mut session, &json!({"state": "idle", "account": ACCOUNT}), 90.0);
    frames(&mut session, &meeting(&[tile("Me", &[])]), 100.0);

    let sent = frames(&mut session, &meeting(&[tile("Me", &["active speaker"]), tile("Bob", &[])]), 101.0);

    assert_eq!(speaker_of(&sent), json!({"id": "Me", "name": "Me", "isLocal": true}));
    let local: Vec<_> = participants(&sent).iter().map(|p| p["isLocal"].clone()).collect();
    assert_eq!(local, [true, false]);
}

fn in_meeting(tiles: &[Value], extra: &[(&str, Value)]) -> Value {
    meeting_in("window", tiles, extra)
}

#[test]
fn given_own_microphone_muted_in_the_menu_when_own_tile_says_unmuted_then_the_menu_wins() {
    let mut session = joined_session(100.0);

    let snapshot = in_meeting(
        &[tile("Me", &["Computer audio unmuted"])],
        &[("account", json!(ACCOUNT)), ("menu", menu_value(Some("Unmute audio")))],
    );
    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(
        values(&sent)[0],
        json!({
            "type": "participants_update",
            "participants": [{"id": "Me", "name": "Me", "isLocal": true, "micMuted": false}],
            "localMicMuted": true,
        })
    );
}

#[test]
fn given_own_microphone_open_in_the_menu_when_processed_then_reported_open() {
    let mut session = joined_session(100.0);

    let snapshot = in_meeting(
        &[tile("Me", &["Computer audio muted"])],
        &[("account", json!(ACCOUNT)), ("menu", menu_value(Some("Mute audio")))],
    );
    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(values(&sent)[0]["localMicMuted"], false);
}

#[test]
fn given_main_window_closed_when_menu_names_the_account_then_own_tile_is_local() {
    let mut session = joined_session(100.0);

    let snapshot = in_meeting(
        &[tile("Me", &["Computer audio muted"]), tile("Bob", &[])],
        &[("menu", menu_value(Some("Unmute audio")))],
    );
    let sent = frames(&mut session, &snapshot, 101.0);

    let local: Vec<_> = participants(&sent).iter().map(|p| (p["id"].clone(), p["isLocal"].clone())).collect();
    assert_eq!(local, [(json!("Me"), json!(true)), (json!("Bob"), json!(false))]);
}

#[test]
fn given_webinar_attendee_without_tile_or_audio_item_when_processed_then_reported_muted() {
    let mut session = joined_session(100.0);

    let snapshot =
        in_meeting(&[tile("Host", &["Computer audio unmuted", "active speaker"])], &[("menu", menu_value(None))]);
    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(
        values(&sent)[0],
        json!({
            "type": "participants_update",
            "participants": [{"id": "Host", "name": "Host", "isLocal": false, "micMuted": false}],
            "localMicMuted": true,
        })
    );
    assert!(session.own_mic_unknown);
}

#[test]
fn given_attendee_promoted_when_the_menu_offers_mute_then_reported_open() {
    let mut session = joined_session(100.0);
    frames(&mut session, &in_meeting(&[tile("Host", &[])], &[("menu", menu_value(None))]), 101.0);

    let sent =
        frames(&mut session, &in_meeting(&[tile("Host", &[])], &[("menu", menu_value(Some("Mute audio")))]), 102.0);

    assert_eq!(
        values(&sent),
        [json!({
            "type": "participants_update",
            "participants": [{"id": "Host", "name": "Host", "isLocal": false, "micMuted": false}],
            "localMicMuted": false,
        })]
    );
}

#[test]
fn given_own_tile_and_unknown_menu_wording_when_processed_then_left_to_the_tile() {
    let mut session = joined_session(100.0);

    let snapshot =
        in_meeting(&[tile("Me", &["Computer audio unmuted"])], &[("menu", menu_value(Some("Mute my microphone")))]);
    let sent = frames(&mut session, &snapshot, 101.0);

    assert!(values(&sent)[0].get("localMicMuted").is_none());
    assert_eq!(participants(&sent)[0], json!({"id": "Me", "name": "Me", "isLocal": true, "micMuted": false}));
    assert!(session.own_mic_unknown);
}

#[test]
fn given_unknown_menu_wording_and_unknown_own_name_when_processed_then_left_to_the_tiles() {
    // A guest, or several accounts: the user's own tile may be there unrecognised.
    let mut session = joined_session(100.0);
    let without_account: Vec<_> =
        menu(Some("Mute my microphone")).into_iter().filter(|path| path[0] == "Meeting").collect();

    let snapshot = in_meeting(&[tile("Me", &["Computer audio unmuted"])], &[("menu", json!(without_account))]);
    let sent = frames(&mut session, &snapshot, 101.0);

    assert!(values(&sent)[0].get("localMicMuted").is_none());
    assert_eq!(participants(&sent)[0]["isLocal"], false);
    assert!(session.own_mic_unknown);
}

#[test]
fn given_menu_missing_from_one_snapshot_when_processed_then_the_last_word_is_kept() {
    let mut session = joined_session(100.0);
    frames(&mut session, &in_meeting(&[tile("Host", &[])], &[("menu", menu_value(Some("Unmute audio")))]), 101.0);

    let sent = frames(&mut session, &meeting(&[tile("Host", &[]), tile("Bob", &[])]), 102.0);

    assert_eq!(values(&sent)[0]["localMicMuted"], true);
    assert!(!session.own_mic_unknown);
}

#[test]
fn given_meeting_window_gone_briefly_when_it_returns_without_a_menu_then_the_word_is_kept() {
    let mut session = joined_session(100.0);
    frames(&mut session, &in_meeting(&[tile("Host", &[])], &[("menu", menu_value(Some("Unmute audio")))]), 101.0);
    frames(&mut session, &idle(), 102.0);

    let sent = frames(&mut session, &meeting(&[tile("Host", &[]), tile("Bob", &[])]), 102.0 + LEAVE_DEBOUNCE_S / 2.0);

    assert_eq!(values(&sent)[0]["localMicMuted"], true);
}

#[test]
fn given_meeting_ended_muted_when_the_next_one_has_no_menu_then_nothing_carries_over() {
    for rejected in [false, true] {
        let mut session = joined_session(100.0);
        let snapshot =
            in_meeting(&[tile("Me", &[])], &[("account", json!(ACCOUNT)), ("menu", menu_value(Some("Unmute audio")))]);
        frames(&mut session, &snapshot, 101.0);
        if rejected {
            session.on_rejected(101.0);
        }
        frames(&mut session, &idle(), 102.0);
        frames(&mut session, &idle(), 102.0 + LEAVE_DEBOUNCE_S);
        let later = 102.0 + LEAVE_DEBOUNCE_S + JOIN_RETRY_S;
        assert_eq!(
            kinds(&frames(&mut session, &meeting(&[tile("Me", &["Computer audio unmuted"])]), later)),
            ["meeting_join"]
        );

        // Neither the last menu word nor "a menu was seen" (which, with no own tile,
        // would count the microphone as closed) survives into this meeting.
        let sent = frames(&mut session, &meeting(&[tile("Host", &["Computer audio unmuted"])]), later + 1.0);

        assert!(values(&sent)[0].get("localMicMuted").is_none(), "rejected: {rejected}");
    }
}

#[test]
fn given_helper_without_menu_when_processed_then_nothing_is_said_about_own_microphone() {
    let mut session = joined_session(100.0);

    let snapshot = in_meeting(&[tile("Me", &["Computer audio unmuted"])], &[("account", json!(ACCOUNT))]);
    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(
        values(&sent)[0],
        json!({
            "type": "participants_update",
            "participants": [{"id": "Me", "name": "Me", "isLocal": true, "micMuted": false}],
        })
    );
}

#[test]
fn given_minimal_view_when_processed_then_its_single_tile_is_the_speaker_and_roster_is_kept() {
    let mut session = joined_session(100.0);
    frames(&mut session, &meeting(&[tile("Alice", &["active speaker"]), tile("Bob", &[])]), 101.0);

    let sent = frames(&mut session, &meeting_in("minimal", &[tile("Bob", &["Computer audio unmuted"])], &[]), 102.0);

    assert_eq!(kinds(&sent), ["speaker_update"]);
    assert_eq!(speaker_of(&sent)["name"], "Bob");
}

#[test]
fn given_minimal_view_with_a_muted_tile_when_processed_then_speaker_is_null() {
    let mut session = joined_session(100.0);

    let sent = frames(&mut session, &meeting_in("minimal", &[tile("Bob", &["Computer audio muted"])], &[]), 101.0);

    assert!(speaker_of(&sent).is_null());
}

#[test]
fn given_tiles_scroll_off_screen_when_processed_then_the_roster_keeps_everyone_seen() {
    // A small window or a large meeting shows a page of tiles; the speaker is always
    // among them, the rest of the roster is not.
    let mut session = joined_session(100.0);
    frames(&mut session, &meeting(&[tile("Alice", &["active speaker"]), tile("Bob", &[]), tile("Carol", &[])]), 101.0);

    let sent = frames(&mut session, &meeting(&[tile("Bob", &["active speaker"])]), 102.0);

    assert_eq!(kinds(&sent), ["speaker_update"]); // the roster did not shrink
    assert_eq!(speaker_of(&sent)["name"], "Bob");
    let later = frames(&mut session, &meeting(&[tile("Dave", &["Computer audio muted"])]), 103.0);
    let names: Vec<_> = participants(&later).iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, ["Alice", "Bob", "Carol", "Dave"]);
}

#[test]
fn given_a_new_meeting_when_joined_then_the_roster_starts_empty() {
    let mut session = joined_session(100.0);
    frames(&mut session, &meeting(&[tile("Alice", &[])]), 101.0);
    frames(&mut session, &idle(), 200.0);
    frames(&mut session, &meeting(&[tile("Host", &[])]), 300.0);

    let sent = frames(&mut session, &meeting(&[tile("Bob", &[])]), 301.0);

    let names: Vec<_> = participants(&sent).iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, ["Bob"]);
}

#[test]
fn given_namesakes_one_muted_when_processed_then_one_unmuted_participant() {
    let mut session = joined_session(100.0);

    let snapshot = meeting(&[tile("Alex", &["Computer audio muted"]), tile("Alex", &["Computer audio unmuted"])]);
    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(participants(&sent), [json!({"id": "Alex", "name": "Alex", "isLocal": false, "micMuted": false})]);
}

#[test]
fn given_tile_without_a_name_when_processed_then_skipped() {
    let mut session = joined_session(100.0);

    let sent = frames(&mut session, &meeting(&[json!({"name": "", "description": "x"}), tile("Bob", &[])]), 101.0);

    let names: Vec<_> = participants(&sent).iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, ["Bob"]);
}

#[test]
fn given_profile_cards_shown_when_processed_then_every_tile_and_the_speaker_are_read() {
    // The shape of a live meeting: most name tags open a profile card, one
    // participant is a guest, the speaker is among the former.
    let mut session = ZoomAppSession::default();
    let snapshot = in_meeting(
        &[
            group("Me, Computer audio muted, Video off", &["View Me's profile"]),
            tile("Guest", &["Computer audio unmuted", "Video on"]),
            group("Carol, Computer audio muted, Video on", &["View Carol's profile"]),
            group("Dana, Computer audio unmuted, Video on, active speaker", &["View Dana's profile"]),
        ],
        &[("account", json!(ACCOUNT))],
    );
    frames(&mut session, &snapshot, 100.0);

    let sent = frames(&mut session, &snapshot, 101.0);

    let rows: Vec<_> =
        participants(&sent).iter().map(|p| (p["name"].clone(), p["isLocal"].clone(), p["micMuted"].clone())).collect();
    assert_eq!(
        rows,
        [
            (json!("Me"), json!(true), json!(true)),
            (json!("Guest"), json!(false), json!(false)),
            (json!("Carol"), json!(false), json!(true)),
            (json!("Dana"), json!(false), json!(false)),
        ]
    );
    assert_eq!(speaker_of(&sent), json!({"id": "Dana", "name": "Dana", "isLocal": false}));
}

#[test]
fn given_participant_without_audio_when_processed_then_reported_as_unable_to_speak() {
    let mut session = joined_session(100.0);

    let snapshot = meeting(&[tile("Viewer", &["No audio connected", "Video off", "active speaker"])]);
    let sent = frames(&mut session, &snapshot, 101.0);

    assert_eq!(participants(&sent)[0]["micMuted"], true);
    assert!(speaker_of(&sent).is_null());
}

#[test]
fn given_helper_built_before_raw_reporting_when_processed_then_its_tiles_are_still_read() {
    let mut session = ZoomAppSession::default();
    let stale = json!({"name": "Bob", "description": "Bob, Computer audio unmuted, active speaker"});
    frames(&mut session, &meeting(std::slice::from_ref(&stale)), 100.0);

    let sent = frames(&mut session, &meeting(&[stale]), 101.0);

    assert_eq!(speaker_of(&sent)["name"], "Bob");
}

#[test]
fn given_other_described_tab_group_when_processed_then_it_is_neither_a_tile_nor_reported() {
    let mut session = joined_session(100.0);

    let sent = frames(&mut session, &meeting(&[group("Reactions", &["Clap", "Like"]), tile("Bob", &[])]), 101.0);

    let names: Vec<_> = participants(&sent).iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, ["Bob"]);
    assert!(session.unread.is_empty());
}

#[test]
fn given_tile_named_in_an_unknown_way_when_processed_then_left_out_and_reported() {
    let mut session = joined_session(100.0);
    let renamed = group("Alice, Computer audio unmuted, Video on, active speaker", &["Open Alice's card"]);

    let sent = frames(&mut session, &meeting(&[renamed.clone(), tile("Bob", &[])]), 101.0);

    let names: Vec<_> = participants(&sent).iter().map(|p| p["name"].clone()).collect();
    assert_eq!(names, ["Bob"]);
    assert!(speaker_of(&sent).is_null());
    assert_eq!(session.unread, [renamed]);
}

#[test]
fn given_only_unreadable_tiles_when_the_meeting_window_opens_then_no_join_but_reported() {
    let mut session = ZoomAppSession::default();
    let renamed = group("Alice, Computer audio unmuted, Video on", &["Open Alice's card"]);

    assert!(frames(&mut session, &meeting(std::slice::from_ref(&renamed)), 100.0).is_empty());
    assert!(!session.joined);
    assert_eq!(session.unread, [renamed]);
}

#[test]
fn given_dialog_without_a_participant_when_no_meeting_window_then_it_is_not_a_meeting() {
    let mut session = ZoomAppSession::default();

    let sent = frames(&mut session, &meeting_in("minimal", &[group("Reactions", &["Clap"])], &[]), 100.0);

    assert!(sent.is_empty());
    assert_eq!(session.state, Some(json!("idle")));
    assert!(!session.joined);
}

#[test]
fn given_joined_when_only_a_foreign_dialog_remains_then_the_meeting_ends_after_the_debounce() {
    let mut session = joined_session(100.0);
    let foreign = meeting_in("minimal", &[group("Reactions", &["Clap"])], &[]);

    assert!(frames(&mut session, &foreign, 101.0).is_empty());
    let sent = frames(&mut session, &foreign, 100.0 + LEAVE_DEBOUNCE_S + 0.1);

    assert_eq!(kinds(&sent), ["meeting_leave"]);
}

// --- lines Python fails on --------------------------------------------------------

#[test]
fn given_malformed_snapshots_when_processed_then_they_fail_where_python_raises() {
    let failing = [
        json!(["state", "meeting"]),
        json!({"state": "meeting", "tiles": null}),
        json!({"state": "meeting", "tiles": ["Alice"]}),
        json!({"state": "meeting", "tiles": [{"description": null, "children": [{"role": "AXButton", "description": "A"}]}]}),
        json!({"state": "meeting", "tiles": [{"description": 5}]}),
        json!({"state": "idle", "account": ["Zoom", "Me", "x"]}),
        json!({"state": "idle", "menu": [[1, 2, 3]]}),
        json!({"state": "idle", "menu": 7}),
    ];
    for snapshot in failing {
        assert!(ZoomAppSession::default().frames(&snapshot, 100.0).is_err(), "{snapshot}");
    }
    // Shapes Python reads without raising: a string menu is iterated by character,
    // and tiles are not looked at outside a meeting.
    let tolerated = [json!({"state": "idle", "menu": "abc"}), json!({"state": "idle", "tiles": 5})];
    for snapshot in tolerated {
        assert!(ZoomAppSession::default().frames(&snapshot, 100.0).is_ok(), "{snapshot}");
    }
}
