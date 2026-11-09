//! Helper snapshots in, bridge frames out: `ZoomAppSession` of `recog/zoom_app.py`.
//!
//! Pure, no I/O; `now` is epoch seconds. A snapshot is the helper's JSON line as a
//! value and is read the way Python reads it, so a line Python would fail on fails
//! here too (`PyError`), and the run that read it ends.

use serde_json::Value;

use crate::frame::{Frame, Participant, Speaker};
use crate::py::{self, PyError, PyResult};
use crate::tile::{self, ACTIVE_TOKEN, Tile, VIDEO_TOKENS};
use crate::vocabulary::Vocabulary;

/// A helper restart or a view switch must not end the meeting.
pub const LEAVE_DEBOUNCE_S: f64 = 6.0;
/// After the bridge rejects a join (another source owns the meeting), stay silent
/// this long before asking again.
pub const JOIN_RETRY_S: f64 = 30.0;

/// Tiles by display name, in the order they first appeared.
type Tiles = Vec<(String, Tile)>;

fn upsert(tiles: &mut Tiles, tile: Tile) {
    match tiles.iter_mut().find(|(name, _)| *name == tile.name) {
        Some((_, existing)) => *existing = tile,
        None => tiles.push((tile.name.clone(), tile)),
    }
}

/// Namesakes share an entry: muted only when both are, speaking when either is.
fn merge(first: &Tile, second: &Tile) -> Tile {
    // Python's `first.muted and second.muted` on True / False / None.
    let muted = if first.muted == second.muted {
        first.muted
    } else if first.muted == Some(true) {
        second.muted
    } else {
        first.muted
    };
    Tile { name: first.name.clone(), muted, active: first.active || second.active }
}

/// The labels of a tile's buttons: what may name the participant.
fn buttons(raw: &serde_json::Map<String, Value>) -> PyResult<Vec<Value>> {
    let Some(children) = raw.get("children") else {
        // A helper built before the tree was reported raw (kept when it cannot be
        // rebuilt) names the tile itself.
        return Ok(raw.get("name").filter(|name| py::truthy(name)).cloned().into_iter().collect());
    };
    let mut labels = Vec::new();
    for child in py::items(children)? {
        let child = object(&child)?;
        let description = child.get("description");
        if child.get("role").and_then(Value::as_str) == Some("AXButton") && description.is_some_and(py::truthy) {
            labels.extend(description.cloned());
        }
    }
    Ok(labels)
}

fn object(value: &Value) -> PyResult<&serde_json::Map<String, Value>> {
    value.as_object().ok_or_else(|| PyError("object has no attribute 'get'".into()))
}

/// `snapshot.get(key)`: a JSON null reads as absent.
fn get<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).filter(|value| !value.is_null())
}

/// Participant tiles by name, and the tab groups that look like one but name nobody.
fn read_tiles(snapshot: &serde_json::Map<String, Value>, vocabulary: &Vocabulary) -> PyResult<(Tiles, Vec<Value>)> {
    let mut tiles = Tiles::new();
    let mut unread = Vec::new();
    let raws = match snapshot.get("tiles") {
        None => Vec::new(),
        Some(raws) => py::items(raws)?,
    };
    for raw in raws {
        read_tile(&raw, vocabulary, &mut tiles, &mut unread)?;
    }
    Ok((tiles, unread))
}

/// One described tab group: a participant tile into `tiles`, or into `unread` when it
/// words a participant's state but names nobody.
fn read_tile(raw: &Value, vocabulary: &Vocabulary, tiles: &mut Tiles, unread: &mut Vec<Value>) -> PyResult<()> {
    let fields = object(raw)?;
    let description = fields.get("description").cloned().unwrap_or_else(|| Value::from(""));
    let Some(name) = tile::tile_name_value(&description, &buttons(fields)?)? else {
        // Any other described tab group is not a tile; one that words a
        // participant's state is a tile Zoom now names differently.
        if words_a_state(&description, vocabulary)? {
            unread.push(raw.clone());
        }
        return Ok(());
    };
    let description = py::text(&description, "startswith")?;
    add_tile(tiles, tile::parse_tile(&name, description, vocabulary));
    Ok(())
}

/// Whether the description of a tab group that names nobody words a participant's
/// state: a mute word, a video word or the speaker mark.
fn words_a_state(description: &Value, vocabulary: &Vocabulary) -> PyResult<bool> {
    let words = tile::tokens(py::text(description, "split")?);
    let state_word = |word: &String| {
        vocabulary.muted.contains(word)
            || vocabulary.unmuted.contains(word)
            || VIDEO_TOKENS.contains(&word.as_str())
            || word == ACTIVE_TOKEN
    };
    Ok(words.iter().any(state_word))
}

/// Add a parsed tile, merged with a namesake already read from the same snapshot.
fn add_tile(tiles: &mut Tiles, parsed: Tile) {
    let merged = match tiles.iter().find(|(known, _)| *known == parsed.name) {
        Some((_, known)) => merge(known, &parsed),
        None => parsed,
    };
    upsert(tiles, merged);
}

/// Turns helper snapshots into bridge frames.
#[derive(Debug, Clone)]
pub struct ZoomAppSession {
    /// Whether the bridge was sent a join it has not refused.
    pub joined: bool,
    /// Of the last snapshot: the helper state as understood here (`"idle"` for a
    /// minimal view that shows nobody), the tab groups that look like a participant
    /// tile but name nobody, and whether a menu bar was read whose audio item carries
    /// no known wording.
    pub state: Option<Value>,
    pub unread: Vec<Value>,
    pub own_mic_unknown: bool,
    vocabulary: Vocabulary,
    last_meeting_at: Option<f64>,
    rejected_at: Option<f64>,
    roster: Tiles,
    self_name: Option<String>,
    // The meeting menu's word on the user's own microphone, kept while a snapshot
    // comes without a menu bar.
    own_mic_muted: Option<bool>,
    menu_seen: bool,
    sent_participants: Option<Frame>,
    sent_speaker: Option<Option<Speaker>>,
}

impl Default for ZoomAppSession {
    fn default() -> Self {
        Self::new(Vocabulary::english())
    }
}

impl ZoomAppSession {
    pub fn new(vocabulary: Vocabulary) -> Self {
        Self {
            joined: false,
            state: None,
            unread: Vec::new(),
            own_mic_unknown: false,
            vocabulary,
            last_meeting_at: None,
            rejected_at: None,
            roster: Tiles::new(),
            self_name: None,
            own_mic_muted: None,
            menu_seen: false,
            sent_participants: None,
            sent_speaker: None,
        }
    }

    /// The bridge refused the join: another source owns the meeting.
    pub fn on_rejected(&mut self, now: f64) {
        self.joined = false;
        self.rejected_at = Some(now);
    }

    /// The bridge link dropped: the bridge has already ended the meeting.
    pub fn on_disconnect(&mut self) {
        self.joined = false;
        self.rejected_at = None;
    }

    /// The frames one snapshot calls for.
    pub fn frames(&mut self, snapshot: &Value, now: f64) -> PyResult<Vec<Frame>> {
        let snapshot = object(snapshot)?;
        let menu = self.read_account_and_menu(snapshot)?;
        self.state = get(snapshot, "state").cloned();
        self.unread = Vec::new();
        self.own_mic_unknown = menu && self.own_mic_muted.is_none();
        if self.state.as_ref().and_then(Value::as_str) != Some("meeting") {
            return Ok(self.leave_frames(now));
        }
        self.meeting_frames(snapshot, now)
    }

    /// The user's name and the meeting menu's word on their microphone; whether the
    /// snapshot carried a menu bar.
    fn read_account_and_menu(&mut self, snapshot: &serde_json::Map<String, Value>) -> PyResult<bool> {
        // Reported outside a meeting too; inside one the main window that carries
        // it is not always reachable.
        let menu = get(snapshot, "menu");
        let name = match tile::account_name_value(get(snapshot, "account"))? {
            Some(name) => Some(name),
            None => tile::menu_account_name_value(menu)?,
        };
        self.self_name = name.or(self.self_name.take());
        if menu.is_some() {
            self.menu_seen = true;
            self.own_mic_muted = tile::own_mic_muted_value(menu, &self.vocabulary)?;
        }
        Ok(menu.is_some())
    }

    /// The frames of a snapshot the helper reports as a meeting.
    fn meeting_frames(&mut self, snapshot: &serde_json::Map<String, Value>, now: f64) -> PyResult<Vec<Frame>> {
        let (tiles, unread) = read_tiles(snapshot, &self.vocabulary)?;
        self.unread = unread;
        let minimal = snapshot.get("view").and_then(Value::as_str) == Some("minimal");
        if minimal && tiles.is_empty() {
            // Without the meeting window the helper reports the tab groups of every
            // dialog; they are the minimal view only when one of them shows a
            // participant.
            self.state = Some(Value::from("idle"));
            return Ok(self.leave_frames(now));
        }
        self.last_meeting_at = Some(now);
        if !self.joined {
            // The join preview is already the meeting window, with no tile in it
            // yet. A meeting starts with the first participant tile.
            if tiles.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(self.join_frames(now));
        }
        Ok(self.state_frames(tiles, minimal, now))
    }

    fn leave_frames(&mut self, now: f64) -> Vec<Frame> {
        let within_debounce = self.last_meeting_at.is_some_and(|at| now - at < LEAVE_DEBOUNCE_S);
        if !within_debounce {
            // The meeting is over, joined or not: what its menu said about the user's
            // microphone must not speak for the next one.
            self.own_mic_muted = None;
            self.menu_seen = false;
        }
        if !self.joined || self.last_meeting_at.is_none() {
            self.rejected_at = None;
            return Vec::new();
        }
        if within_debounce {
            return Vec::new();
        }
        self.joined = false;
        self.roster.clear();
        vec![Frame::meeting_leave()]
    }

    fn join_frames(&mut self, now: f64) -> Vec<Frame> {
        if self.rejected_at.is_some_and(|at| now - at < JOIN_RETRY_S) {
            return Vec::new();
        }
        // Only the join: participants and the speaker follow with the next snapshot,
        // after a rejection had time to arrive.
        self.joined = true;
        self.rejected_at = None;
        self.roster.clear();
        self.sent_participants = None;
        self.sent_speaker = None;
        vec![Frame::meeting_join()]
    }

    fn state_frames(&mut self, tiles: Tiles, minimal: bool, now: f64) -> Vec<Frame> {
        // The tree holds only the tiles on screen: one in minimal view, a page of
        // them in a large meeting or a small window. The roster therefore grows over
        // the meeting and is never trimmed: a participant who left stays listed,
        // which is a smaller error than dropping everyone off-screen.
        for (_, tile) in &tiles {
            upsert(&mut self.roster, tile.clone());
        }
        let mut frames = Vec::new();
        let participants = self
            .roster
            .iter()
            .map(|(name, tile)| Participant {
                id: name.clone(),
                name: name.clone(),
                is_local: Some(name) == self.self_name.as_ref(),
                mic_muted: tile.muted == Some(true),
            })
            .collect();
        let update = Frame::ParticipantsUpdate { participants, local_mic_muted: self.local_mic_muted() };
        if self.sent_participants.as_ref() != Some(&update) {
            self.sent_participants = Some(update.clone());
            frames.push(update);
        }
        let speaker = self.speaker(&tiles, minimal);
        if self.sent_speaker.as_ref() != Some(&speaker) {
            self.sent_speaker = Some(speaker.clone());
            frames.push(Frame::SpeakerUpdate { speaker, timestamp: (now * 1000.0) as i64 });
        }
        frames
    }

    /// The user's own microphone for the bridge, `None` to leave it to their tile.
    ///
    /// The meeting menu's word wins. A menu whose audio item says nothing known leaves
    /// it to the user's tile; with no tile of theirs, as for a webinar attendee, who
    /// has neither a tile nor an audio item, it counts as closed. That needs the
    /// user's name: without it their own tile cannot be told apart.
    fn local_mic_muted(&self) -> Option<bool> {
        if self.own_mic_muted.is_some() {
            return self.own_mic_muted;
        }
        let own_tile_absent =
            self.self_name.as_ref().is_some_and(|own| !self.roster.iter().any(|(name, _)| name == own));
        (self.menu_seen && own_tile_absent).then_some(true)
    }

    fn speaker(&self, tiles: &Tiles, minimal: bool) -> Option<Speaker> {
        // Minimal view has no "active speaker" wording: its only tile is the speaker.
        let single = minimal && tiles.len() == 1;
        tiles
            .iter()
            .map(|(_, tile)| tile)
            .filter(|tile| single || tile.active)
            .find(|tile| tile.muted != Some(true))
            .map(|tile| Speaker {
                id: tile.name.clone(),
                name: tile.name.clone(),
                is_local: Some(&tile.name) == self.self_name.as_ref(),
            })
    }
}
