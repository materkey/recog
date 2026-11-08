//! Who speaks and who is in the call: the state behind `speaker_update` and
//! `participants_update`, and the queries the live process attributes audio with.
//!
//! Pure state, no I/O, every "now" passed in; [`crate::Bridge`] holds it under its
//! lock, as `KTalkBridge` holds these fields under `self._lock`.
//!
//! Known differences from Python, all about frames the extension never sends: a
//! speaker or participant name or id that is not a string is kept as its `str()`
//! (Python keeps the value; `0` is falsy there and `"0"` is not here), `isLocal` and
//! `micMuted` are kept as their truth value, and a participant id that is not a
//! string is a key equal to its `str()` (`5` and `"5"` are one participant).

use indexmap::IndexMap;
use recog_core::pyjson::{Map, Value};

use crate::hooks::{Participant, SpeakerEvent, SpeakerSegment};
use crate::json;

/// How long (s) the last speaker is remembered after falling silent; at least one
/// capture chunk, to cover the audio's lag behind the frames.
pub const SPEAKER_MEMORY_SECONDS: f64 = 0.8;
/// A segment longer than this (s) is cut into parts.
pub const MAX_SEGMENT_DURATION: f64 = 20.0;
/// How far (s) a speaker change may be moved back by `speaker.since`.
pub const SPEAKER_ONSET_MAX_BACKDATE_S: f64 = 3.0;
/// History kept, in seconds.
const HISTORY_SECONDS: f64 = 300.0;

/// The speaker and participant fields of `KTalkBridge`.
#[derive(Debug, Default)]
pub struct Speakers {
    pub current_speaker: Option<String>,
    pub current_speaker_id: Option<String>,
    pub current_is_local: bool,
    pub is_speaking: bool,
    pub last_speaker: Option<String>,
    /// None: Python's initial (and cleared) int `0`, which the debug file shows as `0`.
    pub last_speaker_time: Option<f64>,
    pub last_is_local: bool,
    pub participants: IndexMap<String, Participant>,
    /// `localMicMuted` of the last `participants_update`: the source's own word on the
    /// user's microphone, None when it gave none.
    pub local_mic_muted: Option<bool>,
    pub history: Vec<SpeakerEvent>,
    pub segment_start_time: Option<f64>,
    pub segment_speaker_name: Option<String>,
    pub segment_speaker_id: Option<String>,
    pub segment_is_local: bool,
    pub completed_segments: Vec<SpeakerSegment>,
    pub first_speaker_logged: bool,
    /// The extension's last `pcm_transport_stats`, with `received_at`.
    pub pcm_transport_stats: Map,
}

/// What a `speaker_update` produced for the callbacks, outside the lock.
#[derive(Debug, Default)]
pub struct SpeakerOutcome {
    pub speaker: Option<String>,
    pub is_speaking: bool,
    pub completed: Vec<SpeakerSegment>,
    pub logs: Vec<String>,
}

/// `str(v)` of a present, non-null field.
fn text(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(v) => Some(json::py_str(v)),
    }
}

/// `x or ""` of an optional string.
fn or_empty(value: Option<&String>) -> String {
    value.cloned().unwrap_or_default()
}

fn truthy(value: Option<&Value>, default: bool) -> bool {
    value.map_or(default, Value::truthy)
}

#[allow(clippy::cast_possible_truncation)]
fn millis(seconds: f64) -> i64 {
    (seconds * 1000.0).trunc() as i64
}

impl Speakers {
    /// `_clear_speaker_state()`: everything a new or ended meeting must not inherit.
    pub fn clear(&mut self) {
        self.participants.clear();
        self.local_mic_muted = None;
        self.is_speaking = false;
        self.current_speaker = None;
        self.current_speaker_id = None;
        self.last_speaker = None;
        self.last_speaker_time = None;
        self.last_is_local = false;
        self.history.clear();
        self.segment_start_time = None;
        self.segment_speaker_name = None;
        self.segment_speaker_id = None;
        self.pcm_transport_stats = Map::new();
    }

    fn trim_history(&mut self, now: f64) {
        let cutoff = now - HISTORY_SECONDS;
        let keep_from = self.history.iter().position(|e| e.timestamp >= cutoff).unwrap_or(self.history.len());
        self.history.drain(..keep_from);
    }

    /// The `speaker_update` handler under the lock: `speaker` is the frame's field,
    /// `event_time` its timestamp on the bridge's clock, `now` the wall clock.
    ///
    /// # Errors
    /// Python's `AttributeError` for a speaker that is truthy but not a dict, before
    /// anything changed.
    pub fn speaker_update(
        &mut self,
        speaker: Option<&Value>,
        event_time: f64,
        now: f64,
        clock_offset: f64,
    ) -> Result<SpeakerOutcome, String> {
        let mut out = SpeakerOutcome::default();
        if let Some(speaker) = speaker.filter(|s| s.truthy()) {
            let Some(fields) = speaker.as_object() else { return Err(json::attribute_error(speaker, "get")) };
            let new_id = text(fields.get("id"));
            let new_name = text(fields.get("name"));
            let new_is_local = truthy(fields.get("isLocal"), false);
            if !self.first_speaker_logged {
                out.logs
                    .push(format!("[KTalk Bridge] First speaker received: {}", new_name.as_deref().unwrap_or("None")));
                self.first_speaker_logged = true;
            }
            if let Some(start) = self.segment_start_time {
                let complete = self.segment_speaker_id != new_id || (event_time - start) >= MAX_SEGMENT_DURATION;
                if complete {
                    let duration_ms = millis(event_time - start);
                    if duration_ms > 100 {
                        let segment = SpeakerSegment {
                            speaker_name: or_empty(self.segment_speaker_name.as_ref()),
                            speaker_id: or_empty(self.segment_speaker_id.as_ref()),
                            start_time: start,
                            end_time: event_time,
                            duration_ms,
                            is_local: self.segment_is_local,
                        };
                        self.completed_segments.push(segment.clone());
                        out.completed.push(segment);
                    }
                    self.segment_start_time = None;
                }
            }
            if self.segment_start_time.is_none() {
                self.segment_start_time = Some(event_time);
                self.segment_speaker_name.clone_from(&new_name);
                self.segment_speaker_id.clone_from(&new_id);
                self.segment_is_local = new_is_local;
            }
            self.current_speaker = new_name;
            self.current_speaker_id = new_id;
            self.current_is_local = new_is_local;
            self.is_speaking = true;
            self.last_speaker.clone_from(&self.current_speaker);
            self.last_speaker_time = Some(now);
            self.last_is_local = new_is_local;
            let timestamp = self.switch_time(fields, now, clock_offset);
            self.history.push(SpeakerEvent {
                speaker_name: self.current_speaker.clone(),
                speaker_id: self.current_speaker_id.clone().filter(|s| !s.is_empty()).unwrap_or_default(),
                timestamp,
                is_local: new_is_local,
            });
            self.trim_history(now);
        } else {
            self.history.push(SpeakerEvent {
                speaker_name: None,
                speaker_id: String::new(),
                timestamp: now,
                is_local: false,
            });
            self.trim_history(now);
            if let Some(start) =
                self.segment_start_time.filter(|_| self.segment_speaker_name.as_ref().is_some_and(|n| !n.is_empty()))
            {
                let duration_ms = millis(event_time - start);
                if duration_ms > 100 {
                    let segment = SpeakerSegment {
                        speaker_name: or_empty(self.segment_speaker_name.as_ref()),
                        speaker_id: or_empty(self.segment_speaker_id.as_ref()),
                        start_time: start,
                        end_time: event_time,
                        duration_ms,
                        is_local: self.segment_is_local,
                    };
                    self.completed_segments.push(segment.clone());
                    out.completed.push(segment);
                }
                self.segment_start_time = None;
                self.segment_speaker_name = None;
                self.segment_speaker_id = None;
            }
            self.is_speaking = false;
            self.current_speaker = None;
            self.current_speaker_id = None;
        }
        out.speaker.clone_from(&self.current_speaker);
        out.is_speaking = self.is_speaking;
        Ok(out)
    }

    /// `_speaker_switch_time_locked`: an adapter that reports a change late gives the
    /// new speaker's onset in `since` (ms, browser clock); the change is moved there,
    /// within `SPEAKER_ONSET_MAX_BACKDATE_S`, and repeats of the outgoing speaker after
    /// that moment are dropped.
    fn switch_time(&mut self, speaker: &Map, now: f64, clock_offset: f64) -> f64 {
        let since = match speaker.get("since") {
            Some(v @ (Value::Int(_) | Value::Float(_))) => json::as_f64(v).unwrap_or(0.0),
            _ => return now,
        };
        let incoming = text(speaker.get("id")).filter(|s| !s.is_empty()).unwrap_or_default();
        match self.history.last() {
            Some(last) if last.speaker_name.is_some() && last.speaker_id != incoming => {}
            _ => return now,
        }
        let onset = (since / 1000.0 + clock_offset).max(now - SPEAKER_ONSET_MAX_BACKDATE_S).min(now);
        let outgoing = self.history.last().map(|e| e.speaker_id.clone()).unwrap_or_default();
        while self.history.len() >= 2 {
            let n = self.history.len();
            let (prev, last) = (&self.history[n - 2], &self.history[n - 1]);
            if last.timestamp > onset && last.speaker_id == outgoing && prev.speaker_id == outgoing {
                self.history.pop();
            } else {
                break;
            }
        }
        onset.max(self.history.last().map_or(onset, |e| e.timestamp))
    }

    /// `participants_update` under the lock. Python clears the list first and fills it
    /// as it goes, so a bad entry leaves the ones before it.
    ///
    /// # Errors
    /// Python's `TypeError`/`AttributeError` text for a list that is not iterable or an
    /// entry that is not a dict.
    pub fn participants_update(
        &mut self,
        participants: Option<&Value>,
        local_mic_muted: Option<&Value>,
    ) -> Result<(), String> {
        self.local_mic_muted = match local_mic_muted {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        };
        self.participants.clear();
        let items = match participants {
            None => Vec::new(),
            Some(v) => json::iterate(v)?,
        };
        for p in &items {
            let Some(fields) = p.as_object() else { return Err(json::attribute_error(p, "get")) };
            let id = fields.get("id").map_or_else(String::new, json::py_str);
            let name = fields.get("name").map_or_else(|| "Unknown".to_owned(), json::py_str);
            let participant = Participant {
                id: id.clone(),
                name,
                is_local: truthy(fields.get("isLocal"), false),
                mic_muted: truthy(fields.get("micMuted"), true),
            };
            self.participants.insert(id, participant);
        }
        Ok(())
    }

    /// `_get_current_speaker_unlocked`: the remote speaker now, or the last one within
    /// `SPEAKER_MEMORY_SECONDS`. The user's own voice never plays from the speakers, so
    /// a local speaker answers None.
    #[must_use]
    pub fn current_speaker(&self, now: f64) -> Option<String> {
        if self.is_speaking && self.current_speaker.as_ref().is_some_and(|s| !s.is_empty()) {
            return if self.current_is_local { None } else { self.current_speaker.clone() };
        }
        if let Some(last) = self.last_speaker.as_ref().filter(|s| !s.is_empty())
            && !self.last_is_local
            && now - self.last_speaker_time.unwrap_or(0.0) < SPEAKER_MEMORY_SECONDS
        {
            return Some(last.clone());
        }
        None
    }

    /// `get_dominant_speaker_for_period(start, end)`: the remote speaker with the most
    /// time in the window (the first of equals), local turns counted as silence; the
    /// current speaker when the history says nothing.
    #[must_use]
    pub fn dominant_speaker(&self, start: f64, end: f64, now: f64) -> Option<String> {
        let relevant: Vec<&SpeakerEvent> = self.history.iter().filter(|e| e.timestamp <= end).collect();
        if relevant.is_empty() {
            return self.current_speaker(now);
        }
        let mut durations: IndexMap<&str, f64> = IndexMap::new();
        for (i, event) in relevant.iter().enumerate() {
            let Some(name) = event.speaker_name.as_deref().filter(|_| !event.is_local) else { continue };
            let event_start = event.timestamp.max(start);
            let event_end = relevant.get(i + 1).map_or(end, |next| next.timestamp.min(end));
            if event_end > event_start {
                *durations.entry(name).or_insert(0.0) += event_end - event_start;
            }
        }
        let mut best: Option<(&str, f64)> = None;
        for (name, d) in &durations {
            if best.is_none_or(|(_, b)| *d > b) {
                best = Some((name, *d));
            }
        }
        match best {
            Some((name, _)) => Some(name.to_owned()),
            None => self.current_speaker(now),
        }
    }

    /// `is_current_speaker_local()`.
    #[must_use]
    pub fn is_current_speaker_local(&self, now: f64) -> bool {
        if self.is_speaking {
            return self.current_is_local;
        }
        if self.last_speaker.as_ref().is_some_and(|s| !s.is_empty())
            && now - self.last_speaker_time.unwrap_or(0.0) < SPEAKER_MEMORY_SECONDS
        {
            return self.last_is_local;
        }
        false
    }

    /// `are_all_mics_muted()`: false with nobody in the list.
    #[must_use]
    pub fn are_all_mics_muted(&self) -> bool {
        !self.participants.is_empty() && self.participants.values().all(|p| p.mic_muted)
    }

    /// `is_local_mic_muted()`: the source's `localMicMuted`, else the local
    /// participant's `micMuted`, else open.
    #[must_use]
    pub fn is_local_mic_muted(&self) -> bool {
        if let Some(muted) = self.local_mic_muted {
            return muted;
        }
        self.participants.values().find(|p| p.is_local).is_some_and(|p| p.mic_muted)
    }

    /// `is_speaker_muted(name)`: the first participant of that name decides; unknown
    /// names count as unmuted.
    #[must_use]
    pub fn is_speaker_muted(&self, name: &str) -> bool {
        self.participants.values().find(|p| p.name == name).is_some_and(|p| p.mic_muted)
    }

    /// `has_remote_participants()`.
    #[must_use]
    pub fn has_remote_participants(&self) -> bool {
        self.participants.values().any(|p| !p.is_local)
    }

    /// `get_sole_unmuted_remote_participant()`: the name when exactly one remote
    /// participant has the microphone open.
    #[must_use]
    pub fn sole_unmuted_remote_participant(&self) -> Option<String> {
        let mut unmuted = self.participants.values().filter(|p| !p.is_local && !p.mic_muted);
        match (unmuted.next(), unmuted.next()) {
            (Some(p), None) => Some(p.name.clone()),
            _ => None,
        }
    }

    /// `_check_segment_timeout()`: a segment past `MAX_SEGMENT_DURATION` is completed
    /// and a new one starts with the same speaker.
    pub fn check_segment_timeout(&mut self, now: f64) -> Option<SpeakerSegment> {
        let start = self.segment_start_time?;
        let name = self.segment_speaker_name.as_ref().filter(|n| !n.is_empty())?;
        if now - start < MAX_SEGMENT_DURATION {
            return None;
        }
        let duration_ms = millis(now - start);
        let segment = (duration_ms > 100).then(|| SpeakerSegment {
            speaker_name: name.clone(),
            speaker_id: or_empty(self.segment_speaker_id.as_ref()),
            start_time: start,
            end_time: now,
            duration_ms,
            is_local: self.segment_is_local,
        });
        if let Some(segment) = &segment {
            self.completed_segments.push(segment.clone());
        }
        self.segment_start_time = Some(now);
        segment
    }
}
