//! The Copilot state the bridge keeps: the recent-transcript buffer behind
//! `get_recent_transcript`, the proactive `transcript_tick` trigger, and the merge of
//! the supervisor's screen status with the channel's session count into one chip
//! status (D4 of docs/plans/completed/2026-07-02-auto-copilot-session.md).
//!
//! Pure state; [`crate::Bridge`] holds it under its lock and does the sending.

use std::collections::{HashMap, VecDeque};

use recog_core::protocol;
use recog_core::pyjson::{Map, Value};

/// A `transcript_tick` fires after this many new segments…
pub const COPILOT_TICK_NEW_SEGMENTS: i64 = 5;
/// …or this many seconds after the previous tick.
pub const COPILOT_TICK_INTERVAL_SECONDS: f64 = 15.0;
/// Trailing segments a tick carries as context.
pub const COPILOT_TICK_RECENT_SEGMENTS: usize = 10;
/// Segments the recent-transcript buffer holds.
pub const TRANSCRIPT_BUFFER_CAP: usize = 300;
/// Two missed supervisor heartbeats (~5 s each) mean it is gone.
pub const AGENT_SILENCE_THRESHOLD_S: f64 = 10.0;
/// A confident zero of MCP sessions must last this long before the chip goes offline.
pub const AGENT_SESSIONS_ZERO_DEBOUNCE_S: f64 = 10.0;
/// A TUI without its MCP session this long is replaced (`copilot_channel_unavailable`).
pub const AGENT_CHANNEL_RECOVERY_S: f64 = 60.0;
/// The lifecycle id of a manual Copilot session.
pub const MANUAL_MEETING_ID: &str = protocol::MANUAL_MEETING_ID;

/// Recent segments by id, in insertion order, bounded.
#[derive(Debug, Default)]
pub struct TranscriptBuffer {
    entries: HashMap<i64, Map>,
    order: VecDeque<i64>,
}

impl TranscriptBuffer {
    /// `_record_copilot_transcript`: a new id is appended (the oldest evicted at the
    /// cap); a known one keeps its `start_ms` and `refined` and takes the rest.
    pub fn record(&mut self, segment_id: i64, speaker: &str, text: &str, meeting_id: Option<&str>, now_ms: f64) {
        if segment_id <= 0 {
            return;
        }
        if let Some(entry) = self.entries.get_mut(&segment_id) {
            entry.insert("speaker", Value::from(speaker));
            entry.insert("text", Value::from(text));
            entry.insert("end_ms", Value::Float(now_ms));
            entry.insert("meeting_id", Value::from(meeting_id));
            return;
        }
        if self.order.len() == TRANSCRIPT_BUFFER_CAP
            && let Some(evicted) = self.order.pop_front()
        {
            self.entries.remove(&evicted);
        }
        self.order.push_back(segment_id);
        let mut entry = Map::new();
        entry.insert("segment_id", Value::from(segment_id));
        entry.insert("speaker", Value::from(speaker));
        entry.insert("text", Value::from(text));
        entry.insert("start_ms", Value::Float(now_ms));
        entry.insert("end_ms", Value::Float(now_ms));
        entry.insert("refined", Value::Bool(false));
        entry.insert("meeting_id", Value::from(meeting_id));
        self.entries.insert(segment_id, entry);
    }

    /// `send_refine`'s rewrite: entries of the given meeting (any, when None) take the
    /// refined text.
    pub fn refine(&mut self, segment_id: i64, text: &str, meeting_id: Option<&str>) {
        if segment_id <= 0 {
            return;
        }
        let Some(entry) = self.entries.get_mut(&segment_id) else { return };
        if let Some(mid) = meeting_id
            && entry.get("meeting_id") != Some(&Value::from(mid))
        {
            return;
        }
        entry.insert("text", Value::from(text));
        entry.insert("refined", Value::Bool(true));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Copies of the entries in order.
    #[must_use]
    pub fn ordered(&self) -> Vec<Map> {
        self.order.iter().filter_map(|sid| self.entries.get(sid).cloned()).collect()
    }

    /// Copies of the entries among the last `n` ids that belong to `meeting_id`.
    #[must_use]
    pub fn tail(&self, n: usize, meeting_id: Option<&str>) -> Vec<Map> {
        let skip = self.order.len().saturating_sub(n);
        self.order
            .iter()
            .skip(skip)
            .filter_map(|sid| self.entries.get(sid))
            .filter(|e| e.get("meeting_id") == Some(&Value::from(meeting_id)))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    #[must_use]
    pub fn get(&self, segment_id: i64) -> Option<&Map> {
        self.entries.get(&segment_id)
    }
}

/// `_copilot_tick_state`: set while a conference meeting runs.
#[derive(Debug, Clone, PartialEq)]
pub struct TickState {
    pub meeting_id: Option<String>,
    /// The join frame's `title`, any JSON value.
    pub meeting_title: Value,
    /// Monotonic count of segments sent, surviving buffer eviction.
    pub seg_count: i64,
    pub last_seg_count: i64,
    pub last_emit_ts: f64,
}

impl TickState {
    #[must_use]
    pub fn new(meeting_id: Option<String>, meeting_title: Value, now: f64) -> Self {
        // Seeding the last emit at the join makes the time trigger measure from it.
        Self { meeting_id, meeting_title, seg_count: 0, last_seg_count: 0, last_emit_ts: now }
    }
}

/// The supervisor and channel reports and what was last broadcast.
#[derive(Debug)]
pub struct AgentMerge {
    /// None: no supervisor report yet.
    pub supervisor_status: Option<String>,
    /// `time.monotonic()` of the last report.
    pub supervisor_status_ts: f64,
    pub channel_recovery_sent_at: Option<f64>,
    /// None: unknown, never a confident zero. A bool counts as 0 or 1, as in Python.
    pub claude_sessions: Option<i128>,
    /// When the sessions-zero offline condition began to hold.
    pub sessions_zero_since: Option<f64>,
    /// Seeded "offline": the chip's default, so only real changes broadcast.
    pub last_effective: String,
}

impl Default for AgentMerge {
    fn default() -> Self {
        Self {
            supervisor_status: None,
            supervisor_status_ts: 0.0,
            channel_recovery_sent_at: None,
            claude_sessions: None,
            sessions_zero_since: None,
            last_effective: protocol::AGENT_OFFLINE.to_owned(),
        }
    }
}

impl AgentMerge {
    /// `_update_sessions_zero_anchor`: the debounce starts when a confident zero meets
    /// a supervisor past `spawning` and a session that should be alive.
    pub fn update_zero_anchor(&mut self, now: f64, should_be_alive: bool) {
        let eligible = self.claude_sessions == Some(0)
            && self
                .supervisor_status
                .as_deref()
                .is_some_and(|s| s != protocol::AGENT_OFFLINE && s != protocol::AGENT_SPAWNING)
            && should_be_alive;
        if !eligible {
            self.sessions_zero_since = None;
        } else if self.sessions_zero_since.is_none() {
            self.sessions_zero_since = Some(now);
        }
    }

    /// `_effective_agent_state`. `should_be_alive` is asked last, as Python evaluates
    /// it lazily after the cheaper conditions (it reads meta.json and may log).
    #[must_use]
    pub fn effective(&self, now: f64, should_be_alive: impl FnOnce() -> bool) -> String {
        let Some(status) = self.supervisor_status.as_deref() else { return protocol::AGENT_OFFLINE.to_owned() };
        if status == protocol::AGENT_OFFLINE || now - self.supervisor_status_ts > AGENT_SILENCE_THRESHOLD_S {
            return protocol::AGENT_OFFLINE.to_owned();
        }
        if self.claude_sessions == Some(0)
            && status != protocol::AGENT_SPAWNING
            && self.sessions_zero_since.is_some_and(|since| now - since >= AGENT_SESSIONS_ZERO_DEBOUNCE_S)
            && should_be_alive()
        {
            return protocol::AGENT_OFFLINE.to_owned();
        }
        status.to_owned()
    }

    /// `_recover_missing_channel_session`'s condition; when true the attempt is
    /// recorded (throttled even if delivery fails).
    pub fn take_recovery(&mut self, now: f64) -> bool {
        let Some(since) = self.sessions_zero_since else { return false };
        if now - since < AGENT_CHANNEL_RECOVERY_S
            || !matches!(self.supervisor_status.as_deref(), Some(protocol::AGENT_ONLINE | protocol::AGENT_WORKING))
            || now - self.supervisor_status_ts > AGENT_SILENCE_THRESHOLD_S
        {
            return false;
        }
        if self.channel_recovery_sent_at.is_some_and(|at| now - at < AGENT_CHANNEL_RECOVERY_S) {
            return false;
        }
        self.channel_recovery_sent_at = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_full_buffer_when_new_segment_recorded_then_oldest_is_evicted() {
        // given
        let mut buffer = TranscriptBuffer::default();
        for sid in 1..=300 {
            buffer.record(sid, "A", "t", Some("m"), 1.0);
        }
        // when
        buffer.record(301, "A", "t", Some("m"), 2.0);
        // then
        assert_eq!(buffer.len(), 300);
        assert!(buffer.get(1).is_none() && buffer.get(301).is_some());
    }
}
