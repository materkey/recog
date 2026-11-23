//! A client of `codex app-server` over its stdio: one JSON object per line, requests
//! `{id, method, params}` answered by `{id, result}` or `{id, error}`, notifications
//! without an id (docs/plans/completed/2026-11-23-codex-copilot.md).
//!
//! Only what the Copilot harness acts on becomes an [`Event`]; the rest of the stream
//! (deltas, token usage, rate limits, MCP startup) is read and dropped. A request the
//! server sends (an approval, an elicitation) is refused at once: the harness runs
//! with approval policy `never` and must never wait on a human.

use std::collections::VecDeque;
use std::fmt;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader, Lines};

#[derive(Debug)]
pub enum Error {
    /// The server's stream ended, or writing to it failed.
    Closed,
    /// A line that is not a JSON object of the protocol.
    Protocol(String),
    /// The server's error answer to a request.
    Rpc { code: i64, message: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Closed => f.write_str("codex app-server closed"),
            Error::Protocol(line) => write!(f, "unreadable app-server line: {line}"),
            Error::Rpc { code, message } => write!(f, "app-server error {code}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
}

/// A turn's error: `codexErrorInfo` (its name when it is an object such as
/// `{"httpConnectionFailed": {...}}`) and the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub info: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    TurnStarted {
        turn_id: String,
    },
    TurnCompleted {
        turn_id: String,
        status: TurnStatus,
        error: Option<Failure>,
    },
    /// A finished message of the agent, commentary or final answer.
    AgentMessage {
        text: String,
    },
    Error {
        failure: Failure,
        will_retry: bool,
    },
    /// A server request that was refused.
    Refused {
        method: String,
    },
}

pub struct AppServer<R, W> {
    lines: Lines<BufReader<R>>,
    writer: W,
    next_id: u64,
    queued: VecDeque<Event>,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> AppServer<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        AppServer { lines: BufReader::new(reader).lines(), writer, next_id: 0, queued: VecDeque::new() }
    }

    /// `initialize`, which the server requires before anything else.
    ///
    /// # Errors
    /// The server's refusal or a closed stream.
    pub async fn initialize(&mut self) -> Result<(), Error> {
        let client = json!({"name": "recog-copilot", "title": "Recog Copilot", "version": env!("CARGO_PKG_VERSION")});
        self.request("initialize", json!({"clientInfo": client})).await.map(drop)
    }

    /// Whether Codex has an account to run turns with (`account/read`).
    ///
    /// # Errors
    /// The server's refusal or a closed stream.
    pub async fn logged_in(&mut self) -> Result<bool, Error> {
        let answer = self.request("account/read", json!({})).await?;
        Ok(answer.get("account").is_some_and(|account| !account.is_null()))
    }

    /// `thread/start` with `params`; the new thread's id.
    ///
    /// # Errors
    /// The server's refusal, a closed stream or an answer without a thread id.
    pub async fn start_thread(&mut self, params: Value) -> Result<String, Error> {
        let answer = self.request("thread/start", params).await?;
        text_at(&answer, &["thread", "id"])
    }

    /// `turn/start` with one text input; the id of the turn it started or steered.
    ///
    /// # Errors
    /// The server's refusal, a closed stream or an answer without a turn id.
    pub async fn start_turn(&mut self, thread_id: &str, text: &str) -> Result<String, Error> {
        let params = json!({"threadId": thread_id, "input": [{"type": "text", "text": text}]});
        let answer = self.request("turn/start", params).await?;
        text_at(&answer, &["turn", "id"])
    }

    /// The next event, `None` once the server's stream has ended. Cancel safe: an
    /// interrupted wait loses no line.
    ///
    /// # Errors
    /// An unreadable line.
    pub async fn next_event(&mut self) -> Result<Option<Event>, Error> {
        loop {
            if let Some(event) = self.queued.pop_front() {
                return Ok(Some(event));
            }
            let Some(message) = self.read().await? else { return Ok(None) };
            if message.get("id").is_some() && message.get("method").is_none() {
                continue; // the answer to a request nobody waits for any more
            }
            self.take(message).await?;
        }
    }

    /// Sends `method` and waits for its answer; events arriving meanwhile are kept
    /// for [`next_event`](Self::next_event).
    ///
    /// # Errors
    /// The server's error answer, a closed stream or an unreadable line.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"id": id, "method": method, "params": params})).await?;
        loop {
            let message = self.read().await?.ok_or(Error::Closed)?;
            if message.get("method").is_some() {
                self.take(message).await?;
            } else if message.get("id").and_then(Value::as_u64) == Some(id) {
                return answer(message);
            }
        }
    }

    /// A notification or a server request: queued as an event when it is one.
    async fn take(&mut self, message: Value) -> Result<(), Error> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default().to_owned();
        if let Some(id) = message.get("id") {
            let refusal = json!({"id": id, "error": {"code": -32000, "message": "recog Copilot approves nothing"}});
            self.write(&refusal).await?;
            self.queued.push_back(Event::Refused { method });
        } else if let Some(event) = event(&method, message.get("params").unwrap_or(&Value::Null)) {
            self.queued.push_back(event);
        }
        Ok(())
    }

    async fn read(&mut self) -> Result<Option<Value>, Error> {
        let Some(line) = self.lines.next_line().await.map_err(|_| Error::Closed)? else { return Ok(None) };
        match serde_json::from_str::<Value>(&line) {
            Ok(message) if message.is_object() => Ok(Some(message)),
            _ => Err(Error::Protocol(line)),
        }
    }

    async fn write(&mut self, message: &Value) -> Result<(), Error> {
        let line = format!("{message}\n");
        self.writer.write_all(line.as_bytes()).await.map_err(|_| Error::Closed)?;
        self.writer.flush().await.map_err(|_| Error::Closed)
    }
}

fn answer(message: Value) -> Result<Value, Error> {
    if let Some(error) = message.get("error") {
        return Err(Error::Rpc {
            code: error.get("code").and_then(Value::as_i64).unwrap_or_default(),
            message: error.get("message").and_then(Value::as_str).unwrap_or_default().to_owned(),
        });
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

fn text_at(value: &Value, path: &[&str]) -> Result<String, Error> {
    path.iter()
        .try_fold(value, |value, key| value.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Protocol(format!("no {} in {value}", path.join("."))))
}

/// The event a notification carries, if the harness acts on it.
fn event(method: &str, params: &Value) -> Option<Event> {
    let turn_id = || params.pointer("/turn/id").and_then(Value::as_str).unwrap_or_default().to_owned();
    match method {
        "turn/started" => Some(Event::TurnStarted { turn_id: turn_id() }),
        "turn/completed" => Some(Event::TurnCompleted {
            turn_id: turn_id(),
            status: match params.pointer("/turn/status").and_then(Value::as_str) {
                Some("completed") => TurnStatus::Completed,
                Some("interrupted") => TurnStatus::Interrupted,
                _ => TurnStatus::Failed,
            },
            error: params.pointer("/turn/error").and_then(failure),
        }),
        "item/completed" if params.pointer("/item/type").and_then(Value::as_str) == Some("agentMessage") => {
            Some(Event::AgentMessage { text: params.pointer("/item/text")?.as_str()?.to_owned() })
        }
        "error" => Some(Event::Error {
            failure: params.get("error").and_then(failure)?,
            will_retry: params.get("willRetry").and_then(Value::as_bool).unwrap_or(false),
        }),
        _ => None,
    }
}

fn failure(error: &Value) -> Option<Failure> {
    let error = error.as_object()?;
    let info = match error.get("codexErrorInfo") {
        Some(Value::String(name)) => Some(name.clone()),
        Some(Value::Object(variant)) => variant.keys().next().cloned(),
        _ => None,
    };
    let message = error.get("message").and_then(Value::as_str).unwrap_or_default().to_owned();
    Some(Failure { info, message })
}
