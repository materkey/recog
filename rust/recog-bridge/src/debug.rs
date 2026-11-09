//! `_write_debug_state`: the bridge's state as JSON in `/tmp/ktalk_debug.json`, at
//! most once a second, for `/api/pcm-diagnostics` and people debugging a call.

use recog_core::pyjson::{self, Map, Value};

use crate::bridge::{Bridge, State};
use crate::hooks::local_isoformat;
use crate::obj;
use crate::speakers::Speakers;

impl Bridge {
    /// Writes the debug file when a second has passed since the last write.
    pub fn write_debug_state(&self) {
        let Some(path) = self.config.debug_file.clone() else { return };
        let now = self.now();
        {
            let mut st = self.st();
            if now - st.last_debug_update < 1.0 {
                return;
            }
            st.last_debug_update = now;
        }
        let recording_active = self.is_recording_active();
        let meeting_id = self.current_meeting_id();
        let mut state = {
            let st = self.st();
            let mut state = Map::new();
            state.insert("timestamp", Value::Float(now));
            state.insert("timestamp_iso", Value::from(local_isoformat(self.clock.as_ref(), now)));
            state.insert("bridge", self.bridge_section(&st));
            state.insert("conference", conference_section(&st.speakers));
            state.insert("speaker", speaker_section(&st.speakers));
            state.insert("segments", segments_section(&st.speakers));
            state.insert("meeting", obj! {"id" => meeting_id.as_deref()});
            state.insert("recording", self.recording_section(recording_active));
            state.insert("pcm", Value::Object(Map::new()));
            state.insert("pcm_transport", Value::Object(st.speakers.pcm_transport_stats.clone()));
            state
        };
        if let Some(stats) = self.wired().live.and_then(|l| l.pcm_stats()) {
            merge_pcm_stats(&mut state, stats);
        }
        if let Err(e) = std::fs::write(&path, pyjson::dumps(&Value::Object(state), Some(2))) {
            self.log(&format!("[KTalk Bridge] Debug write error: {}", recog_core::paths::os_error_message(&e, &path)));
        }
    }

    fn bridge_section(&self, st: &State) -> Value {
        obj! {
            "running" => st.running,
            "connected_clients" => count(st.clients.connected.len()),
            "port" => i64::from(self.config.port),
        }
    }

    fn recording_section(&self, active: bool) -> Value {
        obj! {
            "policy" => if self.config.ktalk_only { "ktalk_only" } else { "continuous" },
            "active" => active,
        }
    }
}

/// A length as the JSON integer Python writes.
fn count(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

fn conference_section(sp: &Speakers) -> Value {
    let participants: Vec<Value> = sp
        .participants
        .values()
        .map(|p| obj! {"id" => p.id.as_str(), "name" => p.name.as_str(), "is_local" => p.is_local, "mic_muted" => p.mic_muted})
        .collect();
    let participant_count = count(participants.len());
    obj! {
        "participants" => Value::Array(participants),
        "participant_count" => participant_count,
        "local_mic_muted" => sp.local_mic_muted.map_or(Value::Null, Value::Bool),
        "all_mics_muted" => sp.are_all_mics_muted(),
    }
}

fn speaker_section(sp: &Speakers) -> Value {
    obj! {
        "current" => sp.current_speaker.as_deref(),
        "current_id" => sp.current_speaker_id.as_deref(),
        "is_speaking" => sp.is_speaking,
        "is_local" => sp.current_is_local,
        "last_speaker" => sp.last_speaker.as_deref(),
        "last_speaker_time" => sp.last_speaker_time.map_or(Value::Int(0), Value::Float),
    }
}

fn segments_section(sp: &Speakers) -> Value {
    obj! {
        "active_start" => sp.segment_start_time.map_or(Value::Null, Value::Float),
        "active_speaker" => sp.segment_speaker_name.as_deref(),
        "completed_queue" => count(sp.completed_segments.len()),
        "history_count" => count(sp.history.len()),
    }
}

/// The live side's PCM statistics, or the error it answered, into `pcm`.
fn merge_pcm_stats(state: &mut Map, stats: Result<Map, String>) {
    let Some(pcm) = state.get_mut("pcm").and_then(Value::as_object_mut) else { return };
    match stats {
        Ok(stats) => {
            for (k, v) in stats.iter() {
                pcm.insert(k.clone(), v.clone());
            }
        }
        Err(e) => pcm.insert("error", Value::from(e)),
    }
}
