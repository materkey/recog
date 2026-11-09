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
    fn value(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    if !text.is_ascii() {
        return Err("string argument should contain only ASCII characters".into());
    }
    let data = text.as_bytes();
    if data.first() == Some(&b'=') {
        return Err("Leading padding not allowed".into());
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let (mut quad_pos, mut left, mut pads, mut padding_started) = (0u8, 0u8, 0usize, false);
    for (i, &c) in data.iter().enumerate() {
        if c == b'=' {
            padding_started = true;
            if quad_pos == 0 {
                return Err("Excess padding not allowed".into());
            }
            pads += 1;
            if quad_pos >= 2 && usize::from(quad_pos) + pads >= 4 {
                if i + 1 < data.len() {
                    return Err("Excess data after padding".into());
                }
                return Ok(out);
            }
            continue;
        }
        let Some(v) = value(c) else { return Err("Only base64 data is allowed".into()) };
        if padding_started {
            return Err("Discontinuous padding not allowed".into());
        }
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                left = v;
            }
            1 => {
                quad_pos = 2;
                out.push((left << 2) | (v >> 4));
                left = v & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push((left << 4) | (v >> 2));
                left = v & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push((left << 6) | v);
                left = 0;
            }
        }
    }
    match quad_pos {
        0 => Ok(out),
        1 => Err(format!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
            out.len() / 3 * 4 + 1
        )),
        _ => Err("Incorrect padding".into()),
    }
}

fn key_error(key: &str) -> String {
    recog_core::py::repr(key)
}

/// `a / 1000` of a frame value, or Python's `TypeError`.
fn div_1000(value: &Value) -> Result<f64, String> {
    json::as_f64(value).map(|x| x / 1000.0).ok_or_else(|| json::operand_error("/", value, "int"))
}

/// `data.get("timestamp", time.time() * 1000) / 1000`.
fn raw_timestamp(data: &Map, now: f64) -> Result<f64, String> {
    match data.get("timestamp") {
        Some(ts) => div_1000(ts),
        None => Ok(now * 1000.0 / 1000.0),
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
        if matches!(msg_type, protocol::COPILOT_SPAWN_REQUEST | protocol::COPILOT_CHAT_MESSAGE)
            && !self.policy_enabled()
        {
            if let Some(request_id) = json::nonempty_str(data.get("request_id")) {
                self.reply_copilot_status(client, request_id, "error", "Copilot выключен в настройках.").await;
            }
            return Ok(());
        }
        if matches!(msg_type, protocol::COPILOT_CHANNEL_REPLY | protocol::COPILOT_CHANNEL_STATUS)
            && !self.policy_enabled()
        {
            return Ok(());
        }
        match msg_type {
            protocol::AI_SETTINGS_REFRESH => self.refresh_ai_settings(client).await,
            protocol::USER_PROFILE_REFRESH => self.refresh_user_profile(client).await,
            protocol::SPEAKER_UPDATE => self.on_speaker_update(data, client),
            protocol::PARTICIPANTS_UPDATE => self.on_participants_update(data, client),
            "pcm_transport_stats" => {
                self.on_pcm_transport_stats(data);
                Ok(())
            }
            "connection_status" => self.on_connection_status(data, client).await,
            protocol::MEETING_JOIN => self.on_meeting_join(data, client).await,
            protocol::MEETING_LEAVE => self.on_meeting_leave(data, client).await,
            "meeting_title_update" => self.on_meeting_title_update(data),
            "request_transcript" => self.send_transcript_history(client).await,
            "clock_sync" => self.on_clock_sync(data),
            "pcm_chunk" => self.on_pcm_chunk(data, client),
            protocol::COPILOT_CHANNEL_HELLO => self.on_channel_hello(client).await,
            protocol::COPILOT_CHAT_MESSAGE => self.on_chat_message(data, client).await,
            protocol::COPILOT_CHANNEL_REPLY => self.on_channel_reply(data).await,
            protocol::COPILOT_CHANNEL_STATUS => self.on_channel_status(data).await,
            protocol::GET_RECENT_TRANSCRIPT_REQUEST => self.on_recent_transcript_request(data, client).await,
            protocol::COPILOT_SPAWN_REQUEST => self.on_spawn_request(data, client).await,
            protocol::COPILOT_STOP_REQUEST => self.on_stop_request(data, client).await,
            protocol::SUPERVISOR_HELLO => self.on_supervisor_hello(data, client).await,
            protocol::COPILOT_AGENT_STATUS => self.on_agent_status(data).await,
            protocol::CLAUDE_SESSIONS => self.on_claude_sessions(data).await,
            // `ping` (heartbeat) and unknown types do nothing.
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
        let raw_ts = raw_timestamp(data, now)?;
        let outcome = {
            let mut st = self.st();
            let offset = st.clock_offset;
            let event_time = if offset == 0.0 { raw_ts } else { raw_ts + offset };
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
        if self.config.ktalk_only && is_owner {
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
        if self.config.ktalk_only {
            let owner = self.st().meeting_owner;
            if !self.is_recording_active() || client.map(|c| c.id) != owner {
                return Ok(());
            }
        }
        let malformed = |e: String| format!("[KTalk Bridge] Malformed pcm_chunk frame, skipped: {e}");
        let field = |key: &str| data.get(key).ok_or_else(|| key_error(key));
        let parsed = (|| -> Result<Result<_, String>, String> {
            let pid = match field("pid") {
                Ok(v) => json::py_str(v),
                Err(e) => return Ok(Err(e)),
            };
            if pid.is_empty() {
                return Ok(Err("pid is empty".into()));
            }
            let speaker_name = data.get("speaker_name").filter(|v| v.truthy()).map_or_else(String::new, json::py_str);
            let int = |key: &str| -> Result<Result<i128, String>, String> {
                match field(key) {
                    Err(e) => Ok(Err(e)),
                    Ok(v) => match py_int(v) {
                        Ok(n) => Ok(Ok(n)),
                        Err(IntError::Malformed(e)) => Ok(Err(e)),
                        Err(IntError::Overflow(e)) => Err(e),
                    },
                }
            };
            let seq = match int("seq")? {
                Ok(n) => n,
                Err(e) => return Ok(Err(e)),
            };
            let float = |key: &str| field(key).and_then(py_float);
            let start_ms = match float("chunk_start_ms") {
                Ok(x) => x,
                Err(e) => return Ok(Err(e)),
            };
            let end_ms = match float("chunk_end_ms") {
                Ok(x) => x,
                Err(e) => return Ok(Err(e)),
            };
            let sample_rate = match int("sample_rate")? {
                Ok(n) => n,
                Err(e) => return Ok(Err(e)),
            };
            let pcm = match field("pcm_base64") {
                Ok(v) => v.clone(),
                Err(e) => return Ok(Err(e)),
            };
            Ok(Ok((pid, speaker_name, seq, start_ms, end_ms, sample_rate, pcm)))
        })()?;
        let (pid, speaker_name, seq, start_ms, end_ms, sample_rate, pcm_base64) = match parsed {
            Ok(fields) => fields,
            Err(e) => {
                self.log(&malformed(e));
                return Ok(());
            }
        };
        if sample_rate != 16000 {
            self.log(&format!(
                "[KTalk Bridge] pcm_chunk unsupported sample_rate={sample_rate} (pid={pid}, seq={seq}); expected 16000"
            ));
            return Ok(());
        }
        let Value::Str(text) = &pcm_base64 else {
            return Err(format!(
                "argument should be a bytes-like object or ASCII string, not '{}'",
                pcm_base64.type_name()
            ));
        };
        let pcm = match b64decode_strict(text) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.log(&format!("[KTalk Bridge] pcm_chunk base64 decode failed (pid={pid}, seq={seq}): {e}"));
                return Ok(());
            }
        };
        if pcm.is_empty() {
            self.log(&format!("[KTalk Bridge] pcm_chunk empty payload skipped (pid={pid}, seq={seq})"));
            return Ok(());
        }
        if pcm.len() % 2 != 0 {
            self.log(&format!(
                "[KTalk Bridge] pcm_chunk odd-length payload skipped (pid={pid}, seq={seq}, bytes={})",
                pcm.len()
            ));
            return Ok(());
        }
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
        if !self.has_active_conference_meeting() {
            self.log(&format!("[KTalk Bridge] pcm_chunk outside active ktalk meeting skipped (pid={pid}, seq={seq})"));
            return Ok(());
        }
        // A sibling tab's PCM, tagged with its own meeting, must not enter this one.
        if let Some(chunk_meeting) = json::nonempty_str(data.get("meeting_id"))
            && let Some(current) = self.current_meeting_id().filter(|c| !c.is_empty())
            && chunk_meeting != current
        {
            let now = self.now();
            let log_it = {
                let mut st = self.st();
                let last = st.pcm_drop_log_ts.get(&pid).copied().unwrap_or(0.0);
                let due = now - last > 10.0;
                if due {
                    st.pcm_drop_log_ts.insert(pid.clone(), now);
                }
                due
            };
            if log_it {
                self.log(&format!(
                    "[KTalk Bridge] pcm_chunk cross-meeting dropped (pid={pid}, seq={seq}, chunk={chunk_meeting}, current={current})"
                ));
            }
            return Ok(());
        }
        if let Some(live) = self.wired().live.filter(|l| l.wants_pcm())
            && let Err(e) = live.pcm_chunk(chunk)
        {
            self.log(&format!("[KTalk Bridge] pcm_chunk callback raised: {e}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
