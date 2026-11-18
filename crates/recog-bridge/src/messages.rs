//! `_process_message`: one inbound frame, dispatched by `type`, and the extension's
//! frames about the call itself (speakers, participants, PCM, clock sync).
//!
//! A handler's error is the text of the exception Python would raise at the same
//! point; the connection loop prints it as `Error processing message: …`, after the
//! state changes that preceded it, as in Python.

use std::sync::Arc;

use recog_core::protocol;
use recog_core::pyjson::{Map, Value};

use crate::bridge::Bridge;
use crate::client::Client;
use crate::hooks::PcmChunk;
use crate::json;
use crate::speakers::SpeakerOutcome;

/// Python's `int(x)` of a frame field.
pub(crate) enum IntError {
    /// `TypeError`/`ValueError`: the frame is malformed.
    Malformed(String),
    /// `OverflowError`: not caught by the handler.
    Overflow(String),
}

pub(crate) fn py_int(value: &Value) -> Result<i128, IntError> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Bool(b) => Ok(i128::from(*b)),
        Value::Float(x) if x.is_nan() => Err(IntError::Malformed("cannot convert float NaN to integer".into())),
        Value::Float(x) if x.is_infinite() => {
            Err(IntError::Overflow("cannot convert float infinity to integer".into()))
        }
        #[allow(clippy::cast_possible_truncation)]
        Value::Float(x) => Ok(x.trunc() as i128),
        Value::Str(s) => recog_core::py::parse_int(s).map(i128::from).map_err(IntError::Malformed),
        other => Err(IntError::Malformed(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// Python's `float(x)` of a frame field.
pub(crate) fn py_float(value: &Value) -> Result<f64, String> {
    match value {
        Value::Str(s) => recog_core::py::parse_float(s),
        other => json::as_f64(other)
            .ok_or_else(|| format!("float() argument must be a string or a real number, not '{}'", other.type_name())),
    }
}

/// `base64.b64decode(s, validate=True)` of Python 3.12 (`binascii.a2b_base64` in
/// strict mode), with its error texts.
pub(crate) fn b64decode_strict(text: &str) -> Result<Vec<u8>, String> {
    let data = b64_input(text)?;
    let mut decoder = Base64Decoder::with_capacity(data.len() / 4 * 3);
    for (i, &c) in data.iter().enumerate() {
        if c != b'=' {
            decoder.push(c)?;
        } else if decoder.pad()? {
            if i + 1 < data.len() {
                return Err("Excess data after padding".into());
            }
            return Ok(decoder.out);
        }
    }
    decoder.finish()
}

/// The bytes of `text`, refused when it is not ASCII or starts with padding.
fn b64_input(text: &str) -> Result<&[u8], String> {
    if !text.is_ascii() {
        return Err("string argument should contain only ASCII characters".into());
    }
    let data = text.as_bytes();
    if data.first() == Some(&b'=') {
        return Err("Leading padding not allowed".into());
    }
    Ok(data)
}

fn b64_value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// The state of `a2b_base64` between characters: the position in the current quad,
/// the bits left over from its previous character, and the padding seen.
struct Base64Decoder {
    out: Vec<u8>,
    quad_pos: u8,
    left: u8,
    pads: usize,
    padding_started: bool,
}

impl Base64Decoder {
    fn with_capacity(capacity: usize) -> Self {
        Self { out: Vec::with_capacity(capacity), quad_pos: 0, left: 0, pads: 0, padding_started: false }
    }

    /// One `=`; true when it completes the padding of the quad.
    fn pad(&mut self) -> Result<bool, String> {
        self.padding_started = true;
        if self.quad_pos == 0 {
            return Err("Excess padding not allowed".into());
        }
        self.pads += 1;
        Ok(self.quad_pos >= 2 && usize::from(self.quad_pos) + self.pads >= 4)
    }

    /// One data character.
    fn push(&mut self, c: u8) -> Result<(), String> {
        let Some(v) = b64_value(c) else { return Err("Only base64 data is allowed".into()) };
        if self.padding_started {
            return Err("Discontinuous padding not allowed".into());
        }
        self.pads = 0;
        match self.quad_pos {
            0 => {
                self.quad_pos = 1;
                self.left = v;
            }
            1 => {
                self.quad_pos = 2;
                self.out.push((self.left << 2) | (v >> 4));
                self.left = v & 0x0f;
            }
            2 => {
                self.quad_pos = 3;
                self.out.push((self.left << 4) | (v >> 2));
                self.left = v & 0x03;
            }
            _ => {
                self.quad_pos = 0;
                self.out.push((self.left << 6) | v);
                self.left = 0;
            }
        }
        Ok(())
    }

    /// The end of the input without completed padding.
    fn finish(self) -> Result<Vec<u8>, String> {
        match self.quad_pos {
            0 => Ok(self.out),
            1 => Err(format!(
                "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
                self.out.len() / 3 * 4 + 1
            )),
            _ => Err("Incorrect padding".into()),
        }
    }
}

fn key_error(key: &str) -> String {
    recog_core::py::repr(key)
}

/// The fields of a `pcm_chunk` as Python's handler converts them.
struct PcmFields {
    pid: String,
    speaker_name: String,
    seq: i128,
    start_ms: f64,
    end_ms: f64,
    sample_rate: i128,
    pcm_base64: Value,
}

/// Why a `pcm_chunk` was not converted.
enum PcmRefusal {
    /// `KeyError`/`TypeError`/`ValueError`: logged as malformed and skipped.
    Malformed(String),
    /// `OverflowError`: not caught by the handler.
    Raise(String),
}

fn pcm_field<'a>(data: &'a Map, key: &str) -> Result<&'a Value, PcmRefusal> {
    data.get(key).ok_or_else(|| PcmRefusal::Malformed(key_error(key)))
}

fn pcm_int(data: &Map, key: &str) -> Result<i128, PcmRefusal> {
    py_int(pcm_field(data, key)?).map_err(|e| match e {
        IntError::Malformed(e) => PcmRefusal::Malformed(e),
        IntError::Overflow(e) => PcmRefusal::Raise(e),
    })
}

fn pcm_float(data: &Map, key: &str) -> Result<f64, PcmRefusal> {
    py_float(pcm_field(data, key)?).map_err(PcmRefusal::Malformed)
}

/// `str(data["pid"])` (refused when empty), `str(data.get("speaker_name") or "")`,
/// then `int`/`float` of the numbers, in Python's order.
fn parse_pcm_fields(data: &Map) -> Result<PcmFields, PcmRefusal> {
    let pid = json::py_str(pcm_field(data, "pid")?);
    if pid.is_empty() {
        return Err(PcmRefusal::Malformed("pid is empty".into()));
    }
    let speaker_name = data.get("speaker_name").filter(|v| v.truthy()).map_or_else(String::new, json::py_str);
    Ok(PcmFields {
        pid,
        speaker_name,
        seq: pcm_int(data, "seq")?,
        start_ms: pcm_float(data, "chunk_start_ms")?,
        end_ms: pcm_float(data, "chunk_end_ms")?,
        sample_rate: pcm_int(data, "sample_rate")?,
        pcm_base64: pcm_field(data, "pcm_base64")?.clone(),
    })
}

/// `a / 1000` of a frame value, or Python's `TypeError`.
fn div_1000(value: &Value) -> Result<f64, String> {
    json::as_f64(value).map(|x| x / 1000.0).ok_or_else(|| json::operand_error("/", value, "int"))
}

/// The frame's `timestamp` (ms, browser clock) moved onto the bridge's clock by
/// `clock_offset`, or `now` for a frame without one: the bridge's own time is on
/// its clock already.
fn event_time(data: &Map, now: f64, clock_offset: f64) -> Result<f64, String> {
    match data.get("timestamp") {
        Some(ts) => Ok(div_1000(ts)? + clock_offset),
        None => Ok(now),
    }
}

impl Bridge {
    /// `_frame_from_non_owner`: while a meeting has an owner, the call frames of other tabs
    /// (a second conference whose join was refused) do not count.
    pub fn frame_from_non_owner(&self, client: Option<&Arc<Client>>) -> bool {
        let owner = self.st().meeting_owner;
        matches!((client, owner), (Some(c), Some(o)) if c.id != o)
    }

    /// `_process_message(data, websocket)`.
    ///
    /// # Errors
    /// The exception text where Python's handler would raise.
    pub async fn process_message(self: &Arc<Self>, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        let msg_type = data.get("type").and_then(Value::as_str).unwrap_or("");
        if self.refused_while_copilot_off(msg_type, data, client).await {
            return Ok(());
        }
        match msg_type {
            protocol::AI_SETTINGS_REFRESH
            | protocol::USER_PROFILE_REFRESH
            | protocol::COPILOT_SPAWN_REQUEST
            | protocol::COPILOT_STOP_REQUEST
            | protocol::SUPERVISOR_HELLO
            | protocol::COPILOT_AGENT_STATUS
            | protocol::CLAUDE_SESSIONS => self.process_session_frame(msg_type, data, client).await,
            protocol::COPILOT_CHANNEL_HELLO
            | protocol::COPILOT_CHAT_MESSAGE
            | protocol::COPILOT_CHANNEL_REPLY
            | protocol::COPILOT_CHANNEL_STATUS
            | protocol::GET_RECENT_TRANSCRIPT_REQUEST => self.process_channel_frame(msg_type, data, client).await,
            protocol::MEETING_JOIN | protocol::MEETING_LEAVE | "meeting_title_update" | "request_transcript" => {
                self.process_meeting_frame(msg_type, data, client).await
            }
            _ => self.process_call_frame(msg_type, data, client).await,
        }
    }

    /// Copilot frames while the settings switch the Copilot off: a request with an id
    /// gets the error, the channel's answers are dropped. True when the frame is done.
    async fn refused_while_copilot_off(&self, msg_type: &str, data: &Map, client: Option<&Arc<Client>>) -> bool {
        match msg_type {
            protocol::COPILOT_SPAWN_REQUEST | protocol::COPILOT_CHAT_MESSAGE if !self.policy_enabled() => {
                if let Some(request_id) = json::nonempty_str(data.get("request_id")) {
                    self.reply_copilot_status(client, request_id, "error", "Copilot выключен в настройках.").await;
                }
                true
            }
            protocol::COPILOT_CHANNEL_REPLY | protocol::COPILOT_CHANNEL_STATUS => !self.policy_enabled(),
            _ => false,
        }
    }

    /// The webapp's refreshes and the Copilot session's lifecycle (spawn/stop, the
    /// supervisor and the channel's session count).
    async fn process_session_frame(
        self: &Arc<Self>,
        msg_type: &str,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        match msg_type {
            protocol::AI_SETTINGS_REFRESH => self.refresh_ai_settings(client).await,
            protocol::USER_PROFILE_REFRESH => self.refresh_user_profile(client).await,
            protocol::COPILOT_SPAWN_REQUEST => self.on_spawn_request(data, client).await,
            protocol::COPILOT_STOP_REQUEST => self.on_stop_request(data, client).await,
            protocol::SUPERVISOR_HELLO => self.on_supervisor_hello(data, client).await,
            protocol::COPILOT_AGENT_STATUS => self.on_agent_status(data).await,
            protocol::CLAUDE_SESSIONS => self.on_claude_sessions(data).await,
            // Only the types above are routed here.
            _ => Ok(()),
        }
    }

    /// The Copilot channel's frames and the chat it relays.
    async fn process_channel_frame(
        self: &Arc<Self>,
        msg_type: &str,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        match msg_type {
            protocol::COPILOT_CHANNEL_HELLO => self.on_channel_hello(client).await,
            protocol::COPILOT_CHAT_MESSAGE => self.on_chat_message(data, client).await,
            protocol::COPILOT_CHANNEL_REPLY => self.on_channel_reply(data).await,
            protocol::COPILOT_CHANNEL_STATUS => self.on_channel_status(data).await,
            protocol::GET_RECENT_TRANSCRIPT_REQUEST => self.on_recent_transcript_request(data, client).await,
            // Only the types above are routed here.
            _ => Ok(()),
        }
    }

    /// The extension's meeting lifecycle and its transcript history.
    async fn process_meeting_frame(
        self: &Arc<Self>,
        msg_type: &str,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        match msg_type {
            protocol::MEETING_JOIN => self.on_meeting_join(data, client).await,
            protocol::MEETING_LEAVE => self.on_meeting_leave(data, client).await,
            "meeting_title_update" => self.on_meeting_title_update(data),
            "request_transcript" => self.send_transcript_history(client).await,
            // Only the types above are routed here.
            _ => Ok(()),
        }
    }

    /// The extension's frames about the call itself; `ping` (heartbeat) and unknown
    /// types do nothing.
    async fn process_call_frame(
        self: &Arc<Self>,
        msg_type: &str,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        match msg_type {
            protocol::SPEAKER_UPDATE => self.on_speaker_update(data, client),
            protocol::PARTICIPANTS_UPDATE => self.on_participants_update(data, client),
            "pcm_transport_stats" => {
                self.on_pcm_transport_stats(data);
                Ok(())
            }
            "connection_status" => self.on_connection_status(data, client).await,
            "clock_sync" => self.on_clock_sync(data),
            "pcm_chunk" => self.on_pcm_chunk(data, client),
            _ => Ok(()),
        }
    }

    fn on_speaker_update(&self, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        if self.frame_from_non_owner(client) {
            return Ok(());
        }
        if let Some(seq) = data.get("seq").filter(|s| !s.is_null()) {
            self.check_speaker_seq(seq)?;
        }
        let speaker = data.get("speaker");
        let now = self.now();
        let outcome = {
            let mut st = self.st();
            let offset = st.clock_offset;
            let event_time = event_time(data, now, offset)?;
            st.speakers.speaker_update(speaker, event_time, now, offset)?
        };
        self.report_speaker_outcome(&outcome);
        Ok(())
    }

    /// `seq <= self._last_seq` (logged as out of order) and
    /// `self._last_seq = max(self._last_seq, seq)`.
    fn check_speaker_seq(&self, seq: &Value) -> Result<(), String> {
        let mut st = self.st();
        let (Some(new), Some(last)) = (json::as_f64(seq), json::as_f64(&st.last_seq)) else {
            return Err(format!(
                "'<=' not supported between instances of '{}' and '{}'",
                seq.type_name(),
                st.last_seq.type_name()
            ));
        };
        if new <= last {
            let line = format!(
                "[KTalk Bridge] Out-of-order speaker_update: seq={}, last={}",
                seq.py_str(),
                st.last_seq.py_str()
            );
            drop(st);
            self.log(&line);
            st = self.st();
        }
        if new > last {
            st.last_seq = seq.clone();
        }
        Ok(())
    }

    /// The log lines, the debug file and the live hooks of a `speaker_update`, outside
    /// the lock.
    fn report_speaker_outcome(&self, outcome: &SpeakerOutcome) {
        for line in &outcome.logs {
            self.log(line);
        }
        self.write_debug_state();
        if let Some(live) = self.wired().live {
            live.speaker_changed(outcome.speaker.as_deref(), outcome.is_speaking);
            for segment in &outcome.completed {
                live.segment_completed(segment);
            }
        }
    }

    fn on_participants_update(&self, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        if self.frame_from_non_owner(client) {
            return Ok(());
        }
        let empty = Value::Array(Vec::new());
        let participants = data.get("participants").unwrap_or(&empty);
        self.st().speakers.participants_update(Some(participants), data.get("localMicMuted"))?;
        self.write_debug_state();
        Ok(())
    }

    fn on_pcm_transport_stats(&self, data: &Map) {
        let empty = Value::Object(Map::new());
        match data.get("stats").unwrap_or(&empty) {
            Value::Object(stats) => {
                let mut stats = stats.clone();
                stats.insert("received_at", Value::Float(self.now()));
                self.st().speakers.pcm_transport_stats = stats;
                self.write_debug_state();
            }
            _ => self.log("[KTalk Bridge] Malformed pcm_transport_stats frame, skipped"),
        }
    }

    async fn on_connection_status(self: &Arc<Self>, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        if data.get("connected").is_some_and(Value::truthy) {
            self.log("[KTalk Bridge] Conference connected");
            return Ok(());
        }
        let is_owner = client.map(|c| c.id) == self.st().meeting_owner;
        if self.config.recording_policy.gated() && is_owner {
            self.end_meeting_on_extension_gone().await?;
            self.st().meeting_owner = None;
        }
        self.clear_speaker_state();
        self.log("[KTalk Bridge] Conference disconnected");
        Ok(())
    }

    fn on_meeting_title_update(&self, data: &Map) -> Result<(), String> {
        let Some(title) = data.get("title").filter(|t| t.truthy()) else { return Ok(()) };
        let Some(storage) = self.storage() else { return Ok(()) };
        let title = json::py_str(title);
        storage.update_title(&title)?;
        self.log(&format!("[KTalk Bridge] Updated meeting title: {title}"));
        Ok(())
    }

    fn on_clock_sync(&self, data: &Map) -> Result<(), String> {
        let Some(browser_time) = data.get("browser_time").filter(|t| t.truthy()) else { return Ok(()) };
        let offset = self.now() - div_1000(browser_time)?;
        self.st().clock_offset = offset;
        self.log(&format!("[KTalk Bridge] Clock sync: offset={offset:.3}s"));
        Ok(())
    }

    /// `pcm_chunk`: per-participant Int16 PCM from the extension's worklet, checked at
    /// ingress (one bad chunk must not drop the batch it would join) and handed to
    /// the live process.
    fn on_pcm_chunk(&self, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        if !self.pcm_gate_open(client) {
            return Ok(());
        }
        let fields = match parse_pcm_fields(data) {
            Ok(fields) => fields,
            Err(PcmRefusal::Malformed(e)) => {
                self.log(&format!("[KTalk Bridge] Malformed pcm_chunk frame, skipped: {e}"));
                return Ok(());
            }
            Err(PcmRefusal::Raise(e)) => return Err(e),
        };
        let Some(pcm) = self.pcm_payload(&fields)? else { return Ok(()) };
        let PcmFields { pid, speaker_name, seq, start_ms, end_ms, sample_rate, .. } = fields;
        let offset = self.st().clock_offset;
        let chunk = PcmChunk {
            pid: pid.clone(),
            speaker_name,
            // D9: PCM is for remote participants only, whatever the frame claims.
            is_local: false,
            seq,
            chunk_start_ts: start_ms / 1000.0 + offset,
            chunk_end_ts: end_ms / 1000.0 + offset,
            sample_rate,
            pcm,
        };
        if self.pcm_for_current_meeting(data, &pid, seq) {
            self.hand_pcm_to_live(chunk);
        }
        Ok(())
    }

    /// A `ktalk_only` installation takes PCM only from the owner's tab while it records.
    fn pcm_gate_open(&self, client: Option<&Arc<Client>>) -> bool {
        if !self.config.recording_policy.gated() {
            return true;
        }
        let owner = self.st().meeting_owner;
        self.is_recording_active() && client.map(|c| c.id) == owner
    }

    /// The decoded samples, or None (logged) for a rate, an encoding or a length the
    /// ingest cannot take.
    ///
    /// # Errors
    /// Python's `TypeError` for a payload that is not a string.
    fn pcm_payload(&self, fields: &PcmFields) -> Result<Option<Vec<u8>>, String> {
        let PcmFields { pid, seq, sample_rate, .. } = fields;
        if *sample_rate != 16000 {
            self.log(&format!(
                "[KTalk Bridge] pcm_chunk unsupported sample_rate={sample_rate} (pid={pid}, seq={seq}); expected 16000"
            ));
            return Ok(None);
        }
        let Value::Str(text) = &fields.pcm_base64 else {
            return Err(format!(
                "argument should be a bytes-like object or ASCII string, not '{}'",
                fields.pcm_base64.type_name()
            ));
        };
        let pcm = match b64decode_strict(text) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.log(&format!("[KTalk Bridge] pcm_chunk base64 decode failed (pid={pid}, seq={seq}): {e}"));
                return Ok(None);
            }
        };
        if pcm.is_empty() {
            self.log(&format!("[KTalk Bridge] pcm_chunk empty payload skipped (pid={pid}, seq={seq})"));
            return Ok(None);
        }
        if pcm.len() % 2 != 0 {
            self.log(&format!(
                "[KTalk Bridge] pcm_chunk odd-length payload skipped (pid={pid}, seq={seq}, bytes={})",
                pcm.len()
            ));
            return Ok(None);
        }
        Ok(Some(pcm))
    }

    /// A conference meeting is on and the chunk is not a sibling tab's, tagged with
    /// its own meeting; a refusal is logged (a sibling's once per 10 s and pid).
    fn pcm_for_current_meeting(&self, data: &Map, pid: &str, seq: i128) -> bool {
        if !self.has_active_conference_meeting() {
            self.log(&format!("[KTalk Bridge] pcm_chunk outside active ktalk meeting skipped (pid={pid}, seq={seq})"));
            return false;
        }
        if let Some(chunk_meeting) = json::nonempty_str(data.get("meeting_id"))
            && let Some(current) = self.current_meeting_id().filter(|c| !c.is_empty())
            && chunk_meeting != current
        {
            if self.pcm_drop_log_due(pid) {
                self.log(&format!(
                    "[KTalk Bridge] pcm_chunk cross-meeting dropped (pid={pid}, seq={seq}, chunk={chunk_meeting}, current={current})"
                ));
            }
            return false;
        }
        true
    }

    /// True (and the time kept) when `pid`'s last cross-meeting drop was logged more
    /// than 10 s ago.
    fn pcm_drop_log_due(&self, pid: &str) -> bool {
        let now = self.now();
        let mut st = self.st();
        let last = st.pcm_drop_log_ts.get(pid).copied().unwrap_or(0.0);
        let due = now - last > 10.0;
        if due {
            st.pcm_drop_log_ts.insert(pid.to_owned(), now);
        }
        due
    }

    fn hand_pcm_to_live(&self, chunk: PcmChunk) {
        if let Some(live) = self.wired().live.filter(|l| l.wants_pcm())
            && let Err(e) = live.pcm_chunk(chunk)
        {
            self.log(&format!("[KTalk Bridge] pcm_chunk callback raised: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int_of(value: Value) -> Result<i128, (&'static str, String)> {
        py_int(&value).map_err(|e| match e {
            IntError::Malformed(e) => ("malformed", e),
            IntError::Overflow(e) => ("overflow", e),
        })
    }

    #[test]
    fn given_frame_values_when_converted_with_int_then_python_values_and_errors() {
        assert_eq!(int_of(Value::Int(-7)), Ok(-7));
        assert_eq!(int_of(Value::Bool(true)), Ok(1));
        assert_eq!(int_of(Value::Float(16000.9)), Ok(16000));
        assert_eq!(int_of(Value::Float(-2.7)), Ok(-2));
        assert_eq!(int_of(Value::from(" 3 ")), Ok(3));
        assert_eq!(int_of(Value::Float(f64::NAN)), Err(("malformed", "cannot convert float NaN to integer".into())));
        assert_eq!(
            int_of(Value::Float(f64::NEG_INFINITY)),
            Err(("overflow", "cannot convert float infinity to integer".into()))
        );
        assert_eq!(int_of(Value::from("x")), Err(("malformed", "invalid literal for int() with base 10: 'x'".into())));
        assert_eq!(
            int_of(Value::Array(Vec::new())),
            Err((
                "malformed",
                "int() argument must be a string, a bytes-like object or a real number, not 'list'".into()
            ))
        );
    }

    #[test]
    fn given_python_cases_when_decoded_then_same_bytes_and_errors() {
        let cases: &[(&str, Result<&[u8], &str>)] = &[
            ("YQ==", Ok(b"a")),
            ("YQ", Err("Incorrect padding")),
            ("YQ=", Err("Incorrect padding")),
            ("YR==", Ok(b"a")),
            ("YQ==YQ==", Err("Excess data after padding")),
            (
                "Y",
                Err(
                    "Invalid base64-encoded string: number of data characters (1) cannot be 1 more than a multiple of 4",
                ),
            ),
            ("YQ== ", Err("Excess data after padding")),
            ("!!!!", Err("Only base64 data is allowed")),
            ("", Ok(b"")),
            ("YWJj", Ok(b"abc")),
            ("YW=j", Err("Discontinuous padding not allowed")),
            ("=YWJ", Err("Leading padding not allowed")),
            ("YWJjZA", Err("Incorrect padding")),
            ("YWJjZA=", Err("Incorrect padding")),
            (
                "Y===",
                Err(
                    "Invalid base64-encoded string: number of data characters (1) cannot be 1 more than a multiple of 4",
                ),
            ),
            ("YQ\n==", Err("Only base64 data is allowed")),
            ("AAA=", Ok(b"\x00\x00")),
            ("+/8=", Ok(b"\xfb\xff")),
            ("YQ===", Err("Excess data after padding")),
            ("YWJj=", Err("Excess padding not allowed")),
            ("ЖЖЖЖ", Err("string argument should contain only ASCII characters")),
        ];
        for (input, expected) in cases {
            let got = b64decode_strict(input);
            match expected {
                Ok(bytes) => assert_eq!(got.as_deref(), Ok(*bytes), "{input:?}"),
                Err(e) => assert_eq!(got.as_ref().map_err(String::as_str), Err(*e), "{input:?}"),
            }
        }
    }
}
