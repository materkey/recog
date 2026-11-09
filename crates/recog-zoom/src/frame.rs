//! The frames the session sends to the bridge, the ones the Chrome extension sends.

use recog_core::protocol;
use serde::Serialize;

/// The application exposes no meeting number. One URL for all of them keeps a rejoin
/// inside the storage reopen window in the same recording.
pub const MEETING_URL: &str = "https://zoom.us/app";
pub const PLATFORM: &str = "zoom";

/// A participant as `participants_update` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Participant {
    pub id: String,
    pub name: String,
    #[serde(rename = "isLocal")]
    pub is_local: bool,
    #[serde(rename = "micMuted")]
    pub mic_muted: bool,
}

/// The active speaker as `speaker_update` names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Speaker {
    pub id: String,
    pub name: String,
    #[serde(rename = "isLocal")]
    pub is_local: bool,
}

/// One bridge frame; serialized with `type` first, as the Python dicts are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    MeetingJoin {
        url: String,
        platform: String,
        title: String,
    },
    ParticipantsUpdate {
        participants: Vec<Participant>,
        /// The source's own word on the user's microphone; absent leaves it to the
        /// user's tile.
        #[serde(rename = "localMicMuted", skip_serializing_if = "Option::is_none")]
        local_mic_muted: Option<bool>,
    },
    SpeakerUpdate {
        speaker: Option<Speaker>,
        /// Milliseconds since the epoch, `int(now * 1000)`.
        timestamp: i64,
    },
    MeetingLeave {
        reason: String,
    },
}

impl Frame {
    /// The join of the application meeting: no title, the application offers none.
    pub fn meeting_join() -> Self {
        Self::MeetingJoin { url: MEETING_URL.into(), platform: PLATFORM.into(), title: String::new() }
    }

    pub fn meeting_leave() -> Self {
        Self::MeetingLeave { reason: "user_left".into() }
    }

    /// The `type` field.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MeetingJoin { .. } => protocol::MEETING_JOIN,
            Self::ParticipantsUpdate { .. } => protocol::PARTICIPANTS_UPDATE,
            Self::SpeakerUpdate { .. } => protocol::SPEAKER_UPDATE,
            Self::MeetingLeave { .. } => protocol::MEETING_LEAVE,
        }
    }

    /// The frame as the bridge receives it.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a frame always serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_when_serialized_then_its_type_is_the_bridge_protocol_word() {
        let frames = [
            (Frame::meeting_join(), protocol::MEETING_JOIN),
            (Frame::ParticipantsUpdate { participants: vec![], local_mic_muted: None }, protocol::PARTICIPANTS_UPDATE),
            (Frame::SpeakerUpdate { speaker: None, timestamp: 0 }, protocol::SPEAKER_UPDATE),
            (Frame::meeting_leave(), protocol::MEETING_LEAVE),
        ];
        for (frame, word) in frames {
            let value: serde_json::Value = serde_json::from_str(&frame.to_json()).unwrap();
            assert_eq!(value["type"], word, "{frame:?}");
            assert_eq!(frame.kind(), word, "{frame:?}");
        }
    }
}
