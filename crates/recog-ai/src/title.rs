//! Short meeting titles from the configured provider and prompt
//! (`recog/title_generator.py`), and the AI half of
//! `MeetingStorage._generate_title_for_meeting` (`recog/meeting_storage.py`): the
//! title of a meeting with the provenance to store in its `meta.json`, which stays with
//! the storage.

use std::path::Path;
use std::time::Duration;

use crate::error::{AiError, Result};
use crate::install;
use crate::json::{self, Json};
use crate::provider::ProviderFactory;
use crate::pytext;
use crate::settings::{AiSettingsStore, Fallback, ResolvedTask, Variables, render_prompt};

/// CLI cold start and inference routinely take about 30 s; every provider gets this.
pub const TITLE_TIMEOUT: Duration = Duration::from_secs(90);

/// `_sample_transcript`: a long transcript as evenly spaced blocks of whole lines over
/// the whole meeting (beginning, middle, end), joined by `[...]`; a short one as it is.
pub fn sample_transcript(transcript: &str, max_chars: usize, blocks: usize) -> String {
    if pytext::char_len(transcript) <= max_chars {
        return transcript.to_owned();
    }
    let lines: Vec<&str> = transcript.split('\n').filter(|line| !pytext::strip(line).is_empty()).collect();
    if lines.is_empty() {
        return pytext::prefix_chars(transcript, max_chars).to_owned();
    }
    let budget = (max_chars / blocks).max(1);
    let count = lines.len();
    let mut out = Vec::new();
    for block in 0..blocks {
        let mut index = count * block / blocks;
        let (mut taken, mut size) = (Vec::new(), 0);
        while index < count && size < budget {
            taken.push(lines[index]);
            size += pytext::char_len(lines[index]) + 1;
            index += 1;
        }
        if !taken.is_empty() {
            out.push(taken.join("\n"));
        }
    }
    out.join("\n[...]\n")
}

/// The given snapshot of the `title` scenario, else the one saved in the data directory.
fn title_settings(settings: Option<&ResolvedTask>) -> Result<ResolvedTask> {
    match settings {
        Some(settings) => Ok(settings.clone()),
        None => AiSettingsStore::new(install::data_dir()?, None).resolve_task("title", None, Fallback::default()),
    }
}

async fn try_generate(
    transcript: &str,
    model: Option<&str>,
    settings: Option<&ResolvedTask>,
    variables: &Variables,
    factory: &dyn ProviderFactory,
) -> Result<Option<String>> {
    let mut settings = title_settings(settings)?;
    if let Some(model) = model {
        let provider = settings.provider().to_owned();
        settings.models.insert(provider, model.to_owned());
    }
    // The whole meeting is sampled: the warm-up at its beginning often does not reflect
    // its subject. Caller variables never replace this text.
    let mut variables = variables.clone();
    variables.insert("transcript".into(), sample_transcript(transcript, 6000, 6));
    let prompt = render_prompt(&settings.prompt, &variables)?;
    let provider = factory.create(&settings, "title", None)?;
    let result =
        tokio::time::timeout(TITLE_TIMEOUT, provider.complete(&prompt)).await.map_err(|_| AiError::Timeout)??;
    let title = pytext::strip(&result).trim_matches(['"', '\'']);
    let title = pytext::prefix_chars(title, 100);
    Ok((!title.is_empty()).then(|| title.to_owned()))
}

/// `generate_title_async`: a title from one resolved settings snapshot (the saved
/// `title` scenario when `None`); `model` overrides the selected provider's model for
/// this call. Any failure is printed and gives `None`, as does a transcript shorter than
/// 50 characters, for which no settings are read.
pub async fn generate_title(
    transcript: &str,
    model: Option<&str>,
    settings: Option<&ResolvedTask>,
    variables: &Variables,
    factory: &dyn ProviderFactory,
) -> Option<String> {
    if transcript.is_empty() || pytext::char_len(pytext::strip(transcript)) < 50 {
        return None;
    }
    match try_generate(transcript, model, settings, variables, factory).await {
        Ok(title) => title,
        Err(AiError::Timeout) => {
            println!("Title generation timeout");
            None
        }
        Err(error) => {
            println!("Title generation error: {error}");
            None
        }
    }
}

/// `generate_title`: the same from synchronous code, inside or outside a runtime.
pub fn generate_title_blocking(
    transcript: &str,
    model: Option<&str>,
    settings: Option<&ResolvedTask>,
    variables: &Variables,
    factory: &dyn ProviderFactory,
) -> Option<String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
                runtime.block_on(generate_title(transcript, model, settings, variables, factory))
            })
            .join()
            .ok()
            .flatten()
    })
}

/// What a generated title adds to a meeting's `meta.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleUpdate {
    pub title: String,
    /// The provider that produced it.
    pub title_source: String,
    pub title_model: String,
    pub title_settings_revision: String,
}

impl TitleUpdate {
    pub fn to_json(&self) -> Json {
        json::object([
            ("title", Json::from(self.title.as_str())),
            ("title_source", Json::from(self.title_source.as_str())),
            ("title_model", Json::from(self.title_model.as_str())),
            ("title_settings_revision", Json::from(self.title_settings_revision.as_str())),
        ])
    }
}

/// `MeetingStorage._get_transcript_as_text`: `[ts] Speaker: text` of every segment but
/// system events; unreadable lines are skipped.
pub fn transcript_as_text(meeting_path: &Path) -> Result<String> {
    let path = meeting_path.join("transcript.jsonl");
    if !path.exists() {
        return Ok(String::new());
    }
    let content = pytext::read_text(&path)?;
    let mut lines = Vec::new();
    for line in pytext::strip(&content).split('\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(segment) = Json::parse(line) else {
            continue;
        };
        if segment.as_object().is_none() {
            return Err(AiError::no_get(segment.type_name()));
        }
        if segment.get("speaker_id").and_then(Json::as_str) == Some("__system__") {
            continue;
        }
        let field = |key: &str, default: &str| segment.get(key).map_or_else(|| default.to_owned(), Json::py_str);
        lines.push(format!("[{}] {}: {}", field("ts_display", ""), field("speaker", "Unknown"), field("text", "")));
    }
    Ok(lines.join("\n"))
}

fn names(meta: &Json, local_only: bool) -> Result<Vec<String>> {
    let Some(participants) = meta.get("participants") else {
        return Ok(Vec::new());
    };
    let participants = participants.as_object().ok_or_else(|| AiError::no_get(participants.type_name()))?;
    let mut names = Vec::new();
    for (pid, info) in participants {
        if info.as_object().is_none() {
            return Err(AiError::no_get(info.type_name()));
        }
        if local_only && !info.get("is_local").is_some_and(Json::truthy) {
            continue;
        }
        match info.get("name") {
            None => names.push(pid.clone()),
            Some(Json::Str(name)) => names.push(name.clone()),
            Some(other) => return Err(AiError::Type(format!("expected str instance, {} found", other.type_name()))),
        }
    }
    Ok(names)
}

/// The meeting's `meta.json`, which must hold an object.
fn read_meta(meeting_path: &Path) -> Result<Json> {
    let meta = Json::parse(&pytext::read_text(&meeting_path.join("meta.json"))?)
        .map_err(|error| AiError::Value(format!("Invalid meta.json: {error}")))?;
    if meta.as_object().is_none() {
        return Err(AiError::no_get(meta.type_name()));
    }
    Ok(meta)
}

/// The title prompt's variables from a meeting's metadata: the local participants, all
/// participants and the current title.
fn meeting_variables(meta: &Json) -> Result<Variables> {
    let title = match meta.get("title").filter(|title| title.truthy()) {
        None => String::new(),
        Some(Json::Str(title)) => title.clone(),
        Some(other) => return Err(AiError::Type(format!("meeting title is a {}", other.type_name()))),
    };
    Ok([
        ("user_name".to_owned(), names(meta, true)?.join(", ")),
        ("participants".to_owned(), names(meta, false)?.join(", ")),
        ("meeting_title".to_owned(), title),
    ]
    .into_iter()
    .collect())
}

async fn try_meeting_title(
    data_dir: &Path,
    meeting_path: &Path,
    factory: &dyn ProviderFactory,
) -> Result<Option<TitleUpdate>> {
    let transcript = transcript_as_text(meeting_path)?;
    if transcript.is_empty() {
        return Ok(None);
    }
    let settings = AiSettingsStore::new(data_dir, None).resolve_task("title", None, Fallback::default())?;
    let meta = read_meta(meeting_path)?;
    let variables = meeting_variables(&meta)?;
    let Some(title) = generate_title(&transcript, None, Some(&settings), &variables, factory).await else {
        return Ok(None);
    };
    println!("Generated title: {title}");
    let provider = settings.provider().to_owned();
    Ok(Some(TitleUpdate {
        title,
        title_model: settings.models.get(&provider).cloned().unwrap_or_default(),
        title_source: provider,
        title_settings_revision: settings.revision.clone(),
    }))
}

/// `_generate_title_for_meeting`: a title for the meeting at `meeting_path` from the
/// settings of `data_dir` (the parent of the meetings directory), with its provenance.
/// A failure, including unreadable settings, is printed and gives `None`.
pub async fn meeting_title_update(
    data_dir: &Path,
    meeting_path: &Path,
    factory: &dyn ProviderFactory,
) -> Option<TitleUpdate> {
    match try_meeting_title(data_dir, meeting_path, factory).await {
        Ok(update) => update,
        Err(error) => {
            println!("Warning: Title generation failed: {error}");
            None
        }
    }
}
