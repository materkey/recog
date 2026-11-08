//! `_write_debug_state`: the bridge's state as JSON in `/tmp/ktalk_debug.json`, at
//! most once a second, for `/api/pcm-diagnostics` and people debugging a call.

use recog_core::pyjson::{self, Map, Value};

use crate::bridge::Bridge;
use crate::hooks::local_isoformat;
use crate::obj;

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
            let sp = &st.speakers;
            let participants: Vec<Value> = sp
                .participants
                .values()
                .map(|p| obj! {"id" => p.id.as_str(), "name" => p.name.as_str(), "is_local" => p.is_local, "mic_muted" => p.mic_muted})
                .collect();
            let count = i64::try_from(participants.len()).unwrap_or(i64::MAX);
            let local_mic = sp.local_mic_muted.map_or(Value::Null, Value::Bool);
            let opt = |v: Option<f64>| v.map_or(Value::Null, Value::Float);
            obj! {
                "timestamp" => Value::Float(now),
                "timestamp_iso" => local_isoformat(self.clock.as_ref(), now),
                "bridge" => obj! {
                    "running" => st.running,
                    "connected_clients" => i64::try_from(st.clients.connected.len()).unwrap_or(i64::MAX),
                    "port" => i64::from(self.config.port),
                },
                "conference" => obj! {
                    "participants" => Value::Array(participants),
                    "participant_count" => count,
                    "local_mic_muted" => local_mic,
                    "all_mics_muted" => sp.are_all_mics_muted(),
                },
                "speaker" => obj! {
                    "current" => sp.current_speaker.as_deref(),
                    "current_id" => sp.current_speaker_id.as_deref(),
                    "is_speaking" => sp.is_speaking,
                    "is_local" => sp.current_is_local,
                    "last_speaker" => sp.last_speaker.as_deref(),
                    "last_speaker_time" => sp.last_speaker_time.map_or(Value::Int(0), Value::Float),
                },
                "segments" => obj! {
                    "active_start" => opt(sp.segment_start_time),
                    "active_speaker" => sp.segment_speaker_name.as_deref(),
                    "completed_queue" => i64::try_from(sp.completed_segments.len()).unwrap_or(i64::MAX),
                    "history_count" => i64::try_from(sp.history.len()).unwrap_or(i64::MAX),
                },
                "meeting" => obj! {"id" => meeting_id.as_deref()},
                "recording" => obj! {
                    "policy" => if self.config.ktalk_only { "ktalk_only" } else { "continuous" },
                    "active" => recording_active,
                },
                "pcm" => Value::Object(Map::new()),
                "pcm_transport" => Value::Object(sp.pcm_transport_stats.clone()),
            }
        };
        if let Some(stats) = self.wired().live.and_then(|l| l.pcm_stats())
            && let Some(pcm) = state.as_object_mut().and_then(|m| m.get_mut("pcm")).and_then(Value::as_object_mut)
        {
            match stats {
                Ok(stats) => {
                    for (k, v) in stats.iter() {
                        pcm.insert(k.clone(), v.clone());
                    }
                }
                Err(e) => pcm.insert("error", Value::from(e)),
            }
        }
        if let Err(e) = std::fs::write(&path, pyjson::dumps(&state, Some(2))) {
            self.log(&format!("[KTalk Bridge] Debug write error: {}", recog_core::paths::os_error_message(&e, &path)));
        }
    }
}
