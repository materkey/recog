//! Prompt definitions in `crates/recog-ai/prompts/*.md` (`recog/prompt_registry.py`).
//!
//! Each file has a frontmatter with `name` and `filter` (a regular expression for the
//! meeting title, or null for every meeting) and a template body with `{user_name}`,
//! `{transcript}`, `{participants}` and `{meeting_title}`.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::pytext;

/// A single prompt definition loaded from a `.md` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptConfig {
    /// File name without `.md`.
    pub id: String,
    /// Display name from the frontmatter.
    pub name: String,
    /// Regular expression for the meeting title; `None` matches every meeting.
    pub filter: Option<String>,
    /// Prompt body with `{variable}` placeholders.
    pub template: String,
}

/// `_parse_frontmatter`: `(fields, body)`. The closing `---` is the first one at the
/// start of a line followed only by whitespace up to a newline or the end; fields are
/// `key: value` lines, `null`/`none`/`~`/empty meaning `None` and one pair of matching
/// quotes being removed.
pub fn parse_frontmatter(text: &str) -> (IndexMap<String, Option<String>>, String) {
    let Some(rest) = text.strip_prefix("---") else {
        return (IndexMap::new(), text.to_owned());
    };
    let Some((start, marker_end)) = closing_marker(rest) else {
        return (IndexMap::new(), text.to_owned());
    };
    let header = pytext::strip(&rest[..start]);
    // The marker is `---` and whitespace; the body is stripped, so it starts after it.
    let body = pytext::strip(&rest[marker_end..]).to_owned();

    let mut fields = IndexMap::new();
    for line in header.split('\n') {
        let line = pytext::strip(line);
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = pytext::strip(key).to_owned();
        let value = pytext::strip(value);
        let parsed = if matches!(value.to_lowercase().as_str(), "null" | "none" | "~" | "") {
            None
        } else if (value.starts_with('"') && value.ends_with('"')) || (value.starts_with('\'') && value.ends_with('\''))
        {
            // `value[1:-1]`: a lone quote gives an empty string.
            let mut chars = value.chars();
            chars.next();
            chars.next_back();
            Some(chars.as_str().to_owned())
        } else {
            Some(value.to_owned())
        };
        fields.insert(key, parsed);
    }
    (fields, body)
}

/// The start of `(?:^|\n)---\s*(?:\n|$)` in `rest`, as `re.search` finds it, and the
/// end of its `---`.
fn closing_marker(rest: &str) -> Option<(usize, usize)> {
    let bytes = rest.as_bytes();
    let closes = |after: usize| {
        // Greedy `\s*`, then `\n` or `$`: the end of the text, or a newline inside the
        // whitespace run.
        let tail = &rest[after..];
        let run = tail.len() - pytext::lstrip(tail).len();
        run == tail.len() || tail[..run].contains('\n')
    };
    if rest.starts_with("---") && closes(3) {
        return Some((0, 3));
    }
    (0..bytes.len())
        .find(|&index| bytes[index] == b'\n' && rest[index + 1..].starts_with("---") && closes(index + 4))
        .map(|index| (index, index + 4))
}

/// Loads prompt definitions from a directory of `.md` files.
#[derive(Clone, Debug)]
pub struct PromptRegistry {
    dir: PathBuf,
}

impl PromptRegistry {
    pub fn new(dir: impl Into<PathBuf>) -> PromptRegistry {
        PromptRegistry { dir: dir.into() }
    }

    /// Every `*.md` entry, sorted by name. Like `Path.glob`, hidden files count; a file
    /// that cannot be read or has no `name` is reported on stderr and left out.
    pub fn load_all(&self) -> Vec<PromptConfig> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<_> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .filter(|name| name.as_encoded_bytes().ends_with(b".md"))
            .collect();
        names.sort();
        let mut prompts = Vec::new();
        for name in names {
            let path = self.dir.join(&name);
            match load_file(&path) {
                Ok(Some(config)) => prompts.push(config),
                Ok(None) => {}
                Err(error) => eprintln!("[prompt_registry] error loading {}: {error}", name.to_string_lossy()),
            }
        }
        prompts
    }

    /// One prompt by ID (file name without `.md`).
    pub fn get(&self, prompt_id: &str) -> Option<PromptConfig> {
        let path = self.dir.join(format!("{prompt_id}.md"));
        if !path.exists() {
            return None;
        }
        match load_file(&path) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("[prompt_registry] error loading {}: {error}", file_name(&path));
                None
            }
        }
    }

    /// Prompts whose filter matches the title case-insensitively (`re.search`), and
    /// those without a filter. A filter that does not compile is reported and skipped.
    pub fn matching(&self, meeting_title: &str) -> Vec<PromptConfig> {
        let mut result = Vec::new();
        for prompt in self.load_all() {
            let Some(filter) = &prompt.filter else {
                result.push(prompt);
                continue;
            };
            let matched = fancy_regex::RegexBuilder::new(filter)
                .case_insensitive(true)
                .build()
                .map_err(|error| error.to_string())
                .and_then(|regex| regex.is_match(meeting_title).map_err(|error| error.to_string()));
            match matched {
                Ok(true) => result.push(prompt),
                Ok(false) => {}
                Err(error) => eprintln!("[prompt_registry] bad regex in {}: {error}", prompt.id),
            }
        }
        result
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn load_file(path: &Path) -> std::io::Result<Option<PromptConfig>> {
    let text = pytext::read_text(path)?;
    let (mut fields, template) = parse_frontmatter(&text);
    let name = fields.get("name").cloned().flatten().filter(|name| !name.is_empty());
    let Some(name) = name else {
        eprintln!("[prompt_registry] {}: missing 'name' in frontmatter", file_name(path));
        return Ok(None);
    };
    let id = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let filter = fields.shift_remove("filter").flatten();
    Ok(Some(PromptConfig { id, name, filter, template }))
}
