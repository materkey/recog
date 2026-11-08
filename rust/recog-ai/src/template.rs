//! Prompt templates (`render_prompt` and its tokenizer in `recog/ai_settings.py`).
//!
//! A placeholder is `{name}` with a Python identifier, optionally followed by a format
//! suffix (`{name!r}`, `{name.attr}`, `{name:spec}`, `{name[key]}`), which is rejected as
//! an unknown placeholder. `{{` and `}}` are literal braces; a single `{` that starts no
//! placeholder stays literal, and the `}` closing it stays single, so literal JSON in a
//! prompt survives. Substitution is one pass: braces inside substituted text are data.

use std::collections::{BTreeMap, BTreeSet};

use crate::defaults::PLACEHOLDERS;
use crate::error::{AiError, Result};

/// Values for the placeholders, by name.
pub type Variables = BTreeMap<String, String>;

enum Part<'a> {
    Literal(&'a str),
    Placeholder(&'a str),
}

/// `_PLACEHOLDER_TOKEN.match(template, index)`: the name and the end of the token.
fn placeholder_at(template: &str, index: usize) -> Option<(&str, usize)> {
    let bytes = template.as_bytes();
    let mut end = index + 1;
    if !bytes.get(end).is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_') {
        return None;
    }
    while bytes.get(end).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
        end += 1;
    }
    match bytes.get(end) {
        Some(b'}') => Some((&template[index + 1..end], end + 1)),
        Some(b'.' | b'!' | b':' | b'[') => {
            let rest = &template[end + 1..];
            let stop = rest.find(['{', '\n', '}']).unwrap_or(rest.len());
            let close = end + 1 + stop;
            (bytes.get(close) == Some(&b'}')).then(|| (&template[index + 1..close], close + 1))
        }
        _ => None,
    }
}

/// `_template_parts`, which keeps track of literal braces so that the closing braces of
/// literal JSON next to each other stay intact.
fn template_parts(template: &str) -> Vec<Part<'_>> {
    let mut parts = Vec::new();
    let mut opens: Vec<bool> = Vec::new();
    let mut index = 0;
    while index < template.len() {
        let rest = &template[index..];
        if rest.starts_with("{{") {
            opens.push(true);
            parts.push(Part::Literal("{"));
            index += 2;
        } else if rest.starts_with('{') {
            if let Some((name, end)) = placeholder_at(template, index) {
                parts.push(Part::Placeholder(name));
                index = end;
            } else {
                opens.push(false);
                parts.push(Part::Literal("{"));
                index += 1;
            }
        } else if rest.starts_with('}') {
            let escaped = opens.pop().unwrap_or(true);
            index += if escaped && rest.starts_with("}}") { 2 } else { 1 };
            parts.push(Part::Literal("}"));
        } else {
            let width = rest.chars().next().map_or(1, char::len_utf8);
            parts.push(Part::Literal(&rest[..width]));
            index += width;
        }
    }
    parts
}

/// The placeholders of a template; an unknown one is a `ValueError`.
pub fn template_variables(template: &str) -> Result<BTreeSet<String>> {
    let found: BTreeSet<String> = template_parts(template)
        .into_iter()
        .filter_map(|part| match part {
            Part::Placeholder(name) => Some(name.to_owned()),
            Part::Literal(_) => None,
        })
        .collect();
    let unknown: Vec<&str> = found.iter().map(String::as_str).filter(|name| !PLACEHOLDERS.contains(name)).collect();
    if !unknown.is_empty() {
        return Err(AiError::Value(format!("Unknown prompt placeholders: {}", unknown.join(", "))));
    }
    Ok(found)
}

/// `render_prompt`: known placeholders replaced once (a missing one by an empty string,
/// except `{transcript}`, which must be given), literal and substituted braces kept.
pub fn render_prompt(template: &str, variables: &Variables) -> Result<String> {
    let found = template_variables(template)?;
    if found.contains("transcript") && !variables.contains_key("transcript") {
        return Err(AiError::Value("Missing prompt variable: transcript".to_owned()));
    }
    let mut out = String::with_capacity(template.len());
    for part in template_parts(template) {
        match part {
            Part::Literal(text) => out.push_str(text),
            Part::Placeholder(name) => out.push_str(variables.get(name).map_or("", String::as_str)),
        }
    }
    Ok(out)
}

/// [`Variables`] from pairs.
pub fn variables<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Variables {
    pairs.into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect()
}
