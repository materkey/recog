//! Inputs for the differential test and the Rust side of it.

use recog_zoom::bridge;
use recog_zoom::log::{Logger, StatusLog};
use recog_zoom::vocabulary::Vocabulary;
use recog_zoom::{
    JOIN_RETRY_S, LEAVE_DEBOUNCE_S, ZoomAppSession, account_name, menu_account_name, own_mic_muted, parse_tile,
    tile_name,
};
use serde_json::{Value, json};

use crate::common::*;

/// One run of a session: an optional vocabulary and its events, as the driver reads them.
pub struct Scenario {
    pub name: String,
    pub vocabulary: Option<Value>,
    pub events: Vec<Value>,
}

impl Scenario {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), vocabulary: None, events: Vec::new() }
    }

    /// A helper line carrying `snapshot`, read at `at`.
    pub fn line(mut self, at: f64, snapshot: &Value) -> Self {
        self.events.push(json!({"line": snapshot.to_string(), "at": at}));
        self
    }

    pub fn raw_line(mut self, at: f64, line: &str) -> Self {
        self.events.push(json!({"line": line, "at": at}));
        self
    }

    pub fn rejected(mut self, at: f64) -> Self {
        let message = json!({"type": "meeting_join_rejected", "reason": "another_tab_active"});
        self.events.push(json!({"message": message.to_string(), "at": at}));
        self
    }

    pub fn disconnect(mut self) -> Self {
        self.events.push(json!({"disconnect": true}));
        self
    }

    pub fn vocabulary(mut self, vocabulary: &Vocabulary) -> Self {
        self.vocabulary = Some(vocabulary_json(vocabulary));
        self
    }

    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "vocabulary": self.vocabulary, "events": self.events})
    }

    pub fn from_json(value: &Value) -> Self {
        Self {
            name: value["name"].as_str().unwrap().into(),
            vocabulary: value.get("vocabulary").filter(|v| !v.is_null()).cloned(),
            events: value["events"].as_array().unwrap().clone(),
        }
    }
}

fn vocabulary_json(vocabulary: &Vocabulary) -> Value {
    json!({
        "muted": vocabulary.muted,
        "unmuted": vocabulary.unmuted,
        "mic_open_item": vocabulary.mic_open_item,
        "mic_closed_item": vocabulary.mic_closed_item,
    })
}

fn vocabulary_of(spec: Option<&Value>) -> Vocabulary {
    let Some(spec) = spec.filter(|spec| !spec.is_null()) else { return Vocabulary::english() };
    let words = |key: &str| spec[key].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_owned()).collect();
    Vocabulary {
        muted: words("muted"),
        unmuted: words("unmuted"),
        mic_open_item: words("mic_open_item"),
        mic_closed_item: words("mic_closed_item"),
    }
}

/// What the Rust session does with a scenario, in the driver's answer format.
pub fn rust_session(scenario: &Scenario) -> Vec<Value> {
    let (logger, buffer) = Logger::buffer();
    let mut session = ZoomAppSession::new(vocabulary_of(scenario.vocabulary.as_ref()));
    let mut log = StatusLog::new(logger.clone());
    let mut results = Vec::new();
    for event in &scenario.events {
        let mut sent = Vec::new();
        let at = event["at"].as_f64().unwrap_or(0.0);
        let outcome = if let Some(line) = event["line"].as_str() {
            bridge::on_line(line.as_bytes(), &mut session, &mut log, at).map(|frames| sent = frames)
        } else if let Some(message) = event["message"].as_str() {
            bridge::on_message(message.as_bytes(), &mut session, &logger, at)
        } else {
            session.on_disconnect();
            Ok(())
        };
        let logged: Vec<String> = String::from_utf8(std::mem::take(&mut *buffer.lock().unwrap()))
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        results.push(json!({
            "sent": sent,
            "log": logged,
            "error": outcome.is_err(),
            "state": session.state,
            "unread": session.unread,
            "own_mic_unknown": session.own_mic_unknown,
            "joined": session.joined,
        }));
        if outcome.is_err() {
            break;
        }
    }
    results
}

fn host() -> Value {
    meeting(&[tile("Host", &[])])
}

fn bob_speaking() -> Value {
    meeting(&[tile("Bob", &["active speaker"])])
}

fn menu_meeting(tiles: &[Value], audio_item: Option<&str>, account: bool) -> Value {
    let mut extra = vec![("menu", menu_value(audio_item))];
    if account {
        extra.push(("account", json!(ACCOUNT)));
    }
    meeting_in("window", tiles, &extra)
}

/// The flows of `tests/test_zoom_app.py`, plus the shapes it checks one by one.
pub fn ported_scenarios() -> Vec<Scenario> {
    let foreign = meeting_in("minimal", &[group("Reactions", &["Clap"])], &[]);
    let renamed = group("Alice, Computer audio unmuted, Video on, active speaker", &["Open Alice's card"]);
    let profile_meeting = meeting_in(
        "window",
        &[
            group("Me, Computer audio muted, Video off", &["View Me's profile"]),
            tile("Guest", &["Computer audio unmuted", "Video on"]),
            group("Carol, Computer audio muted, Video on", &["View Carol's profile"]),
            group("Dana, Computer audio unmuted, Video on, active speaker", &["View Dana's profile"]),
        ],
        &[("account", json!(ACCOUNT))],
    );
    let root = tempfile::tempdir().unwrap();
    let russian = recog_zoom::load_vocabulary(&[zoom_app(root.path(), &[("ru", RUSSIAN)])]);
    let mut scenarios = vec![
        Scenario::new("no meeting")
            .line(100.0, &idle())
            .line(101.0, &json!({"state": "zoom_not_running"}))
            .line(102.0, &json!({"state": "not_trusted"})),
        Scenario::new("first meeting snapshot")
            .line(100.0, &meeting(&[tile("Bob", &["Computer audio unmuted", "active speaker"])])),
        Scenario::new("preview then first tile").line(100.0, &meeting(&[])).line(104.0, &meeting(&[tile("Me", &[])])),
        Scenario::new("preview cancelled").line(100.0, &meeting(&[])).line(100.0 + LEAVE_DEBOUNCE_S + 1.0, &idle()),
        Scenario::new("tiles vanish").line(100.0, &host()).line(101.0, &meeting(&[])),
        Scenario::new("participants and speaker follow").line(100.0, &host()).line(
            101.0,
            &meeting_in(
                "window",
                &[
                    tile("Me", &["Computer audio muted"]),
                    tile("Bob", &["Computer audio unmuted", "Video off", "active speaker"]),
                ],
                &[("account", json!(ACCOUNT))],
            ),
        ),
        Scenario::new("heartbeat").line(100.0, &host()).line(101.0, &bob_speaking()).line(103.0, &bob_speaking()),
        Scenario::new("window gone briefly").line(100.0, &host()).line(100.0 + LEAVE_DEBOUNCE_S - 0.1, &idle()),
        Scenario::new("window gone for the debounce")
            .line(100.0, &host())
            .line(100.0 + LEAVE_DEBOUNCE_S, &idle())
            .line(200.0, &idle()),
        Scenario::new("new meeting after leaving")
            .line(100.0, &host())
            .line(101.0, &bob_speaking())
            .line(200.0, &idle())
            .line(300.0, &bob_speaking())
            .line(301.0, &bob_speaking()),
        Scenario::new("join rejected")
            .line(100.0, &host())
            .rejected(100.5)
            .line(101.0, &bob_speaking())
            .line(100.5 + JOIN_RETRY_S - 0.1, &bob_speaking())
            .line(100.5 + JOIN_RETRY_S, &bob_speaking()),
        Scenario::new("rejected then ended")
            .line(100.0, &host())
            .rejected(100.5)
            .line(101.0, &idle())
            .line(102.0, &host()),
        Scenario::new("link dropped")
            .line(100.0, &host())
            .line(101.0, &bob_speaking())
            .disconnect()
            .line(105.0, &bob_speaking())
            .line(106.0, &bob_speaking()),
        Scenario::new("active mark moves")
            .line(100.0, &host())
            .line(101.0, &meeting(&[tile("Alice", &["active speaker"]), tile("Bob", &[])]))
            .line(102.0, &meeting(&[tile("Alice", &[]), tile("Bob", &["active speaker"])])),
        Scenario::new("nobody marked")
            .line(100.0, &host())
            .line(101.0, &meeting(&[tile("Alice", &[]), tile("Bob", &[])])),
        Scenario::new("muted speaker")
            .line(100.0, &host())
            .line(101.0, &meeting(&[tile("Alice", &["Computer audio muted", "active speaker"])])),
        Scenario::new("own tile marked")
            .line(100.0, &host())
            .line(101.0, &meeting_in("window", &[tile("Me", &["active speaker"])], &[("account", json!(ACCOUNT))])),
        Scenario::new("account seen before")
            .line(90.0, &json!({"state": "idle", "account": ACCOUNT}))
            .line(100.0, &meeting(&[tile("Me", &[])]))
            .line(101.0, &meeting(&[tile("Me", &["active speaker"]), tile("Bob", &[])])),
        Scenario::new("menu wins over the tile")
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Me", &["Computer audio unmuted"])], Some("Unmute audio"), true)),
        Scenario::new("menu says open")
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Me", &["Computer audio muted"])], Some("Mute audio"), true)),
        Scenario::new("menu names the account").line(100.0, &host()).line(
            101.0,
            &menu_meeting(&[tile("Me", &["Computer audio muted"]), tile("Bob", &[])], Some("Unmute audio"), false),
        ),
        Scenario::new("webinar attendee")
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Host", &["Computer audio unmuted", "active speaker"])], None, false)),
        Scenario::new("attendee promoted")
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Host", &[])], None, false))
            .line(102.0, &menu_meeting(&[tile("Host", &[])], Some("Mute audio"), false)),
        Scenario::new("unknown menu wording, own tile")
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Me", &["Computer audio unmuted"])], Some("Mute my microphone"), false)),
        Scenario::new("unknown menu wording, unknown name").line(100.0, &host()).line(
            101.0,
            &meeting_in(
                "window",
                &[tile("Me", &["Computer audio unmuted"])],
                &[(
                    "menu",
                    json!(
                        menu(Some("Mute my microphone")).into_iter().filter(|p| p[0] == "Meeting").collect::<Vec<_>>()
                    ),
                )],
            ),
        ),
        Scenario::new("menu missing once")
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Host", &[])], Some("Unmute audio"), false))
            .line(102.0, &meeting(&[tile("Host", &[]), tile("Bob", &[])])),
        Scenario::new("window gone briefly, menu kept")
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Host", &[])], Some("Unmute audio"), false))
            .line(102.0, &idle())
            .line(102.0 + LEAVE_DEBOUNCE_S / 2.0, &meeting(&[tile("Host", &[]), tile("Bob", &[])])),
        Scenario::new("helper without menu").line(100.0, &host()).line(
            101.0,
            &meeting_in("window", &[tile("Me", &["Computer audio unmuted"])], &[("account", json!(ACCOUNT))]),
        ),
        Scenario::new("minimal view keeps the roster")
            .line(100.0, &host())
            .line(101.0, &meeting(&[tile("Alice", &["active speaker"]), tile("Bob", &[])]))
            .line(102.0, &meeting_in("minimal", &[tile("Bob", &["Computer audio unmuted"])], &[])),
        Scenario::new("minimal view muted")
            .line(100.0, &host())
            .line(101.0, &meeting_in("minimal", &[tile("Bob", &["Computer audio muted"])], &[])),
        Scenario::new("tiles scroll off")
            .line(100.0, &host())
            .line(101.0, &meeting(&[tile("Alice", &["active speaker"]), tile("Bob", &[]), tile("Carol", &[])]))
            .line(102.0, &bob_speaking())
            .line(103.0, &meeting(&[tile("Dave", &["Computer audio muted"])])),
        Scenario::new("roster starts empty")
            .line(100.0, &host())
            .line(101.0, &meeting(&[tile("Alice", &[])]))
            .line(200.0, &idle())
            .line(300.0, &host())
            .line(301.0, &meeting(&[tile("Bob", &[])])),
        Scenario::new("namesakes").line(100.0, &host()).line(
            101.0,
            &meeting(&[tile("Alex", &["Computer audio muted"]), tile("Alex", &["Computer audio unmuted"])]),
        ),
        Scenario::new("tile without a name")
            .line(100.0, &host())
            .line(101.0, &meeting(&[json!({"name": "", "description": "x"}), tile("Bob", &[])])),
        Scenario::new("profile cards").line(100.0, &profile_meeting).line(101.0, &profile_meeting),
        Scenario::new("no audio")
            .line(100.0, &host())
            .line(101.0, &meeting(&[tile("Viewer", &["No audio connected", "Video off", "active speaker"])])),
        Scenario::new("early helper")
            .line(
                100.0,
                &meeting(&[json!({"name": "Bob", "description": "Bob, Computer audio unmuted, active speaker"})]),
            )
            .line(
                101.0,
                &meeting(&[json!({"name": "Bob", "description": "Bob, Computer audio unmuted, active speaker"})]),
            ),
        Scenario::new("other tab group")
            .line(100.0, &host())
            .line(101.0, &meeting(&[group("Reactions", &["Clap", "Like"]), tile("Bob", &[])])),
        Scenario::new("renamed tile")
            .line(100.0, &host())
            .line(101.0, &meeting(&[renamed.clone(), tile("Bob", &[])]))
            .line(102.0, &meeting(&[renamed])),
        Scenario::new("only unreadable tiles")
            .line(100.0, &meeting(&[group("Alice, Computer audio unmuted, Video on", &["Open Alice's card"])])),
        Scenario::new("foreign dialog only").line(100.0, &foreign),
        Scenario::new("foreign dialog after joining")
            .line(100.0, &host())
            .line(101.0, &foreign)
            .line(100.0 + LEAVE_DEBOUNCE_S + 0.1, &foreign),
        Scenario::new("pump: dialog then unreadable tile")
            .line(100.0, &foreign)
            .line(101.0, &meeting(&[group("Alice, Video on", &["Open Alice's card"])])),
        Scenario::new("russian interface").vocabulary(&russian).line(100.0, &host()).line(
            101.0,
            &meeting(&[
                tile("Иван", &["Звук компьютера выключен", "active speaker"]),
                tile("Анна", &["Звук компьютера включен"]),
            ]),
        ),
        Scenario::new("states logged")
            .line(1.0, &json!({"state": "not_trusted"}))
            .line(2.0, &json!({"state": "not_trusted"}))
            .line(3.0, &idle())
            .line(4.0, &host())
            .line(5.0, &json!({"state": "weird"}))
            .line(6.0, &json!({}))
            .line(7.0, &json!({"state": 5}))
            .line(8.0, &json!({"state": []}))
            .line(9.0, &json!({"state": ["x"]})),
        Scenario::new("bad line").line(100.0, &host()).raw_line(101.0, "not json"),
    ];
    for rejected in [false, true] {
        let mut scenario = Scenario::new(&format!("nothing carries over (rejected: {rejected})"))
            .line(100.0, &host())
            .line(101.0, &menu_meeting(&[tile("Me", &[])], Some("Unmute audio"), true));
        if rejected {
            scenario = scenario.rejected(101.0);
        }
        let later = 102.0 + LEAVE_DEBOUNCE_S + JOIN_RETRY_S;
        scenarios.push(
            scenario
                .line(102.0, &idle())
                .line(102.0 + LEAVE_DEBOUNCE_S, &idle())
                .line(later, &meeting(&[tile("Me", &["Computer audio unmuted"])]))
                .line(later + 1.0, &meeting(&[tile("Host", &["Computer audio unmuted"])])),
        );
    }
    scenarios
}

/// xorshift64*: the same sequence on every host.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const NAMES: &[&str] = &[
    "Alice",
    "Bob",
    "Me",
    "Doe, Jane",
    "O'Neil",
    "View Bob's profile",
    "Иван Петров",
    "Al",
    "Alex",
    " Spaced ",
    "x",
    "Tab\tName",
];
const STATES: &[&str] = &[
    "Computer audio muted",
    "Computer audio unmuted",
    "Telephone muted",
    "Telephone unmuted",
    "Audio muted",
    "No audio connected",
    "Video on",
    "Video off",
    "active speaker",
    "ACTIVE SPEAKER",
    "Local time 11:00",
    "Host",
    "Звук компьютера выключен",
    "Звук компьютера включен",
    " Computer audio muted ",
    "",
    "Bob",
];
const AUDIO_ITEMS: &[&str] = &[
    "Mute audio",
    "Unmute audio",
    "Join audio",
    "Mute telephone",
    "Unmute telephone",
    "MUTE AUDIO ",
    "Mute my microphone",
    "Выключить звук",
    "Включить звук",
];
const ACCOUNT_ITEMS: &[&str] = &[
    "Me(me@example.org)",
    "Иван Петров (ivan@example.org)",
    "Bob(bob@example.org)",
    "Add account",
    " Me (a@b) ",
    "Me(a@b)(c@d)",
    "Me\n(a@b)",
    "(a@b)",
    "Me(@b)",
    "Me(a@)",
    "Me(a b@c)",
    "Me (Work)(me@x.org)",
];
const ACCOUNTS: &[&str] = &[
    "Zoom, Me, In a Zoom Meeting, Basic account",
    "Zoom, Иван Петров, Available, Basic account",
    "Zoom, , x, y",
    "Zoom, Me",
    "Zoom,Alice,Busy",
    "",
];
const STEPS: &[f64] = &[0.0, 0.1, 0.5, 1.0, 2.0, 5.9, 6.0, 6.1, 29.9, 30.0, 31.0, 100.0];

fn random_tile(rng: &mut Rng, hostile: bool) -> Value {
    let name = *rng.pick(NAMES);
    let mut parts = vec![name];
    for _ in 0..rng.below(4) {
        parts.push(rng.pick(STATES));
    }
    let description = parts.join(", ");
    match rng.below(if hostile { 20 } else { 19 }) {
        0..=8 => group(&description, &[name]),
        9..=11 => group(&description, &[&format!("View {name}'s profile")]),
        12 => group(&description, &["Pin", name]),
        13 => group(&description, &[&format!("Open {name}'s card")]),
        14 => group("Reactions", &["Clap"]),
        15 => json!({"name": name, "description": description}),
        16 => json!({"name": "", "description": description}),
        17 => {
            json!({"description": description, "children": [{"role": "AXStaticText", "value": name}, {"role": "AXButton"}, {"role": "AXButton", "description": name}]})
        }
        18 => group(&format!("Someone, {}", rng.pick(STATES)), &[name]),
        // Shapes Python raises on, or reads in its own way.
        _ => rng
            .pick(&[
                json!({"description": null, "children": [{"role": "AXButton", "description": name}]}),
                json!({"description": 5}),
                json!({"description": description, "children": null}),
                json!({"description": description, "children": "x"}),
                json!({"description": description, "children": ""}),
                json!({"description": description, "children": [{"role": "AXButton", "description": 5}]}),
                json!("tile"),
                json!({"children": [{"role": "AXButton", "description": name}]}),
            ])
            .clone(),
    }
}

fn random_menu(rng: &mut Rng, hostile: bool) -> Option<Value> {
    match rng.below(10) {
        0..=2 => None,
        3 => Some(Value::Null),
        // Python iterates a string by character and a dict by key; a number raises.
        4 if hostile => Some(rng.pick(&[json!("abc"), json!(0), json!([]), json!(7), json!({"Meeting": 1})]).clone()),
        4 => Some(rng.pick(&[json!("abc"), json!(0), json!([]), json!({"Meeting": 1})]).clone()),
        _ => {
            let mut paths = Vec::new();
            for _ in 0..rng.below(3) {
                paths.push(json!(["Zoom Workplace", "Switch account", rng.pick(ACCOUNT_ITEMS)]));
            }
            for _ in 0..rng.below(3) {
                paths.push(json!(["Meeting", rng.pick(AUDIO_ITEMS)]));
            }
            if rng.chance(10) {
                paths.push(json!(["Meeting", "Audio", "Mute audio"]));
            }
            if hostile && rng.chance(3) {
                paths.push(rng.pick(&[json!("abc"), json!([1, 2]), json!(["a", "b", 3])]).clone());
            }
            Some(Value::from(paths))
        }
    }
}

fn random_snapshot(rng: &mut Rng, hostile: bool) -> Value {
    let mut snapshot = serde_json::Map::new();
    let state = match rng.below(40) {
        0..=27 => json!("meeting"),
        28..=31 => json!("idle"),
        32 => json!("zoom_not_running"),
        33 => json!("not_trusted"),
        34 => json!("weird"),
        35 => Value::Null,
        36 => json!(5),
        37 => json!([]),
        38 if hostile => json!(["x"]),
        _ => json!(true),
    };
    if !(state.is_null() && rng.chance(50)) {
        snapshot.insert("state".into(), state);
    }
    match rng.below(10) {
        0..=6 => {
            snapshot.insert("view".into(), json!("window"));
        }
        7 | 8 => {
            snapshot.insert("view".into(), json!("minimal"));
        }
        _ => {}
    }
    let tiles: Vec<Value> = (0..rng.below(6)).map(|_| random_tile(rng, hostile)).collect();
    if !hostile || rng.chance(98) {
        snapshot.insert("tiles".into(), Value::from(tiles));
    } else {
        snapshot.insert("tiles".into(), rng.pick(&[Value::Null, json!("s"), json!(5)]).clone());
    }
    if rng.chance(30) {
        let account = if !hostile || rng.chance(95) {
            json!(rng.pick(ACCOUNTS))
        } else {
            rng.pick(&[Value::Null, json!(1)]).clone()
        };
        snapshot.insert("account".into(), account);
    }
    if let Some(menu) = random_menu(rng, hostile) {
        snapshot.insert("menu".into(), menu);
    }
    Value::Object(snapshot)
}

/// A seeded random run of 40 events, mostly snapshots of a few slowly changing tiles.
/// One seed in four is hostile: it also carries lines and messages Python raises on,
/// and the run ends at the first of them.
pub fn random_scenario(seed: u64) -> Scenario {
    let mut rng = Rng::new(seed);
    let hostile = seed % 4 == 3;
    let mut scenario = Scenario::new(&format!("random seed {seed}"));
    if seed.is_multiple_of(5) {
        let root = tempfile::tempdir().unwrap();
        scenario = scenario.vocabulary(&recog_zoom::load_vocabulary(&[zoom_app(root.path(), &[("ru", RUSSIAN)])]));
    }
    let mut at = 1_000.0;
    let mut last = random_snapshot(&mut rng, hostile);
    for _ in 0..40 {
        at += rng.pick(STEPS);
        match rng.below(100) {
            0..=79 => {
                if rng.chance(60) {
                    last = random_snapshot(&mut rng, hostile);
                }
                scenario = scenario.line(at, &last);
            }
            80..=86 => {
                let reason =
                    rng.pick(&[json!("another_tab_active"), Value::Null, json!(5), json!({"a": [1, "b"]})]).clone();
                let mut message = json!({"type": "meeting_join_rejected"});
                if !reason.is_null() || rng.chance(50) {
                    message["reason"] = reason;
                }
                scenario.events.push(json!({"message": message.to_string(), "at": at}));
            }
            87..=89 if hostile => {
                let message = rng.pick(&["{\"type\": \"copilot_status\"}", "[1]", "{"]);
                scenario.events.push(json!({"message": message, "at": at}));
            }
            87..=96 => scenario = scenario.disconnect(),
            _ if hostile => {
                scenario = scenario.raw_line(
                    at,
                    rng.pick(&["not json", "[1, 2]", "\"text\"", "{\"state\": \"meeting\", \"tiles\": [{}]}"]),
                )
            }
            _ => scenario = scenario.raw_line(at, "{\"state\": \"meeting\", \"tiles\": [{}]}"),
        }
    }
    scenario
}

const FRAGMENTS: &[&str] = &[
    "Alice",
    "Bob",
    "View ",
    "'s profile",
    ",",
    ", ",
    " ",
    "(",
    ")",
    "@",
    "a",
    "\n",
    "\t",
    "\u{1c}",
    "Σ",
    "İ",
    "x@y",
    "Zoom",
    "Computer audio muted",
    "active speaker",
    "Mute audio",
    "Unmute audio",
    "Иван",
    "'",
];

fn random_text(rng: &mut Rng) -> String {
    (0..rng.below(7)).map(|_| *rng.pick(FRAGMENTS)).collect()
}

/// Padding Python's `str.strip()` and Rust's `trim()` disagree on, or both keep.
const PADS: &[&str] = &["", " ", "\u{1c}", "\u{1f}", "\t", "\u{a0}", "\u{200b}", "\u{3000}", "\n"];

fn padded(rng: &mut Rng, text: &str) -> String {
    format!("{}{text}{}", rng.pick(PADS), rng.pick(PADS))
}

/// Text shaped like what the function reads, padded, or random fragments.
fn shaped(rng: &mut Rng, shapes: &[&str]) -> String {
    if rng.chance(60) {
        let shape = *rng.pick(shapes);
        let parts: Vec<String> = shape.split(',').map(|part| padded(rng, part)).collect();
        parts.join(",")
    } else {
        random_text(rng)
    }
}

const ACCOUNT_SHAPES: &[&str] = &["Zoom,Me,Busy,Basic", "Zoom,Иван Петров,x", "Zoom,,x", "zoom,Me,x", "Zoom,Me"];

fn random_paths(rng: &mut Rng) -> Value {
    let mut items: Vec<&str> = ACCOUNT_ITEMS.to_vec();
    items.extend_from_slice(AUDIO_ITEMS);
    Value::from(
        (0..rng.below(5))
            .map(|_| {
                let length = 1 + rng.below(3);
                Value::from((0..length).map(|_| shaped(rng, &items)).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>(),
    )
}

/// Random calls of the pure functions with arguments of the right types.
pub fn random_calls(count: usize) -> Vec<Value> {
    let mut rng = Rng::new(7);
    (0..count)
        .map(|_| match rng.below(5) {
            0 => {
                let buttons: Vec<String> = (0..rng.below(3)).map(|_| random_text(&mut rng)).collect();
                json!({"fn": "tile_name", "args": [random_text(&mut rng), buttons]})
            }
            1 => {
                let name = *rng.pick(NAMES);
                let states: Vec<String> = (0..rng.below(4)).map(|_| {
                    let state = STATES[rng.below(STATES.len())];
                    padded(&mut rng, state)
                }).collect();
                let description = if rng.chance(70) { format!("{name},{}", states.join(",")) } else { random_text(&mut rng) };
                json!({"fn": "parse_tile", "args": [name, description, null]})
            }
            2 => json!({"fn": "account_name", "args": [if rng.chance(10) { Value::Null } else { json!(shaped(&mut rng, ACCOUNT_SHAPES)) }]}),
            3 => json!({"fn": "menu_account_name", "args": [if rng.chance(10) { Value::Null } else { random_paths(&mut rng) }]}),
            _ => json!({"fn": "own_mic_muted", "args": [if rng.chance(10) { Value::Null } else { random_paths(&mut rng) }, null]}),
        })
        .collect()
}

fn paths_of(value: &Value) -> Option<Vec<Vec<String>>> {
    value.as_array().map(|paths| {
        paths
            .iter()
            .map(|path| path.as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_owned()).collect())
            .collect()
    })
}

/// What the Rust functions answer, in the driver's format.
pub fn rust_calls(calls: &[Value]) -> Vec<Value> {
    let english = Vocabulary::english();
    calls
        .iter()
        .map(|call| {
            let args = &call["args"];
            let answer = match call["fn"].as_str().unwrap() {
                "tile_name" => {
                    let buttons: Vec<String> =
                        args[1].as_array().unwrap().iter().map(|b| b.as_str().unwrap().to_owned()).collect();
                    json!(tile_name(args[0].as_str().unwrap(), &buttons))
                }
                "parse_tile" => {
                    let tile = parse_tile(args[0].as_str().unwrap(), args[1].as_str().unwrap(), &english);
                    json!([tile.name, tile.muted, tile.active])
                }
                "account_name" => json!(account_name(args[0].as_str())),
                "menu_account_name" => json!(menu_account_name(paths_of(&args[0]).as_deref())),
                _ => json!(own_mic_muted(paths_of(&args[0]).as_deref(), &english)),
            };
            json!({"ok": answer})
        })
        .collect()
}
