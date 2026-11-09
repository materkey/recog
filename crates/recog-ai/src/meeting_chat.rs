//! A saved dialog about one meeting: its file and the prompt of the next question
//! (`recog/meeting_chat.py`).
//!
//! One line of `chat.jsonl` holds a whole exchange, so a question without an answer
//! cannot be stored: a failed model call leaves no trace and can simply be asked again.
//! Each line is one append, which lets several browser tabs ask at the same time.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::error::Result;
use crate::json::{self, Json};
use crate::pytext;

pub const CHAT_FILE: &str = "chat.jsonl";
pub const PROMPT_HISTORY_TURNS: usize = 8;
const FIELDS: [&str; 6] = ["asked_at", "question", "answer", "provider", "settings_revision", "transcript_truncated"];

/// One exchange. Python checks only that the question and the answer are text, so the
/// other fields keep whatever a line holds.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatTurn {
    pub asked_at: Json,
    pub question: String,
    pub answer: String,
    pub provider: Json,
    pub settings_revision: Json,
    pub transcript_truncated: Json,
}

impl ChatTurn {
    pub fn new(
        asked_at: &str,
        question: &str,
        answer: &str,
        provider: &str,
        settings_revision: &str,
        transcript_truncated: bool,
    ) -> ChatTurn {
        ChatTurn {
            asked_at: Json::from(asked_at),
            question: question.to_owned(),
            answer: answer.to_owned(),
            provider: Json::from(provider),
            settings_revision: Json::from(settings_revision),
            transcript_truncated: Json::Bool(transcript_truncated),
        }
    }

    /// `asdict(turn)`, in field order.
    pub fn to_json(&self) -> Json {
        json::object([
            ("asked_at", self.asked_at.clone()),
            ("question", Json::from(self.question.as_str())),
            ("answer", Json::from(self.answer.as_str())),
            ("provider", self.provider.clone()),
            ("settings_revision", self.settings_revision.clone()),
            ("transcript_truncated", self.transcript_truncated.clone()),
        ])
    }
}

/// `ChatTurn(**json.loads(line))` with text question and answer, else `None`.
fn parse_turn(line: &str) -> Option<ChatTurn> {
    let Ok(Json::Object(raw)) = Json::parse(line) else {
        return None;
    };
    if raw.len() != FIELDS.len() || !FIELDS.iter().all(|field| raw.contains_key(*field)) {
        return None;
    }
    let (Json::Str(question), Json::Str(answer)) = (&raw["question"], &raw["answer"]) else {
        return None;
    };
    Some(ChatTurn {
        asked_at: raw["asked_at"].clone(),
        question: question.clone(),
        answer: answer.clone(),
        provider: raw["provider"].clone(),
        settings_revision: raw["settings_revision"].clone(),
        transcript_truncated: raw["transcript_truncated"].clone(),
    })
}

/// The dialog, skipping lines damaged by a crash or by hand. Lines are Python's
/// `splitlines()`, so a raw U+2028, U+2029 or U+0085 inside a stored text splits its
/// line and the exchange is skipped, as in Python.
pub fn read_turns(meeting_path: &Path) -> Result<Vec<ChatTurn>> {
    let text = match pytext::read_text_lossy(&meeting_path.join(CHAT_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(pytext::splitlines(&text).into_iter().filter_map(parse_turn).collect())
}

/// Appends one exchange in one write. An `Io` error of kind `NotFound` means that the
/// meeting has been deleted; its directory is not recreated.
pub fn append_turn(meeting_path: &Path, turn: &ChatTurn) -> Result<()> {
    let line = turn.to_json().dumps(None) + "\n";
    let mut file = OpenOptions::new().append(true).create(true).mode(0o600).open(meeting_path.join(CHAT_FILE))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Starts the dialog anew; the meeting itself stays.
pub fn clear_turns(meeting_path: &Path) -> Result<()> {
    match std::fs::remove_file(meeting_path.join(CHAT_FILE)) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// The editable instructions, then the last exchanges and the question. These are not
/// placeholders of the prompt: a question must reach the model whatever the user has
/// done to the prompt in the settings.
pub fn build_chat_prompt(instructions: &str, history: &[ChatTurn], question: &str) -> String {
    let mut parts = vec![pytext::rstrip(instructions).to_owned()];
    let recent = &history[history.len().saturating_sub(PROMPT_HISTORY_TURNS)..];
    if !recent.is_empty() {
        let exchanges: Vec<String> =
            recent.iter().map(|turn| format!("Вопрос: {}\nОтвет: {}", turn.question, turn.answer)).collect();
        parts.push(format!("Предыдущие вопросы и ответы этого разговора:\n\n{}", exchanges.join("\n\n")));
    }
    parts.push(format!("Вопрос пользователя:\n{question}"));
    parts.join("\n\n")
}
