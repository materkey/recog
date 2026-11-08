//! tests/test_prompt_registry.py.

use std::path::Path;

use recog_ai::install::default_prompts_dir;
use recog_ai::prompt_registry::{PromptRegistry, parse_frontmatter};

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

fn ids(registry: &PromptRegistry, title: &str) -> Vec<String> {
    registry.matching(title).into_iter().map(|prompt| prompt.id).collect()
}

#[test]
fn valid_frontmatter_when_parsed_then_fields_and_body() {
    let (fields, body) = parse_frontmatter("---\nname: My increment\nfilter: null\n---\n\nPrompt body here.");
    assert_eq!(fields["name"].as_deref(), Some("My increment"));
    assert_eq!(fields["filter"], None);
    assert_eq!(body, "Prompt body here.");
}

#[test]
fn quoted_values_when_parsed_then_quotes_are_stripped() {
    let (fields, body) = parse_frontmatter("---\nname: \"Team sync\"\nfilter: \"Sync|синк\"\n---\n\nBody");
    assert_eq!(fields["name"].as_deref(), Some("Team sync"));
    assert_eq!(fields["filter"].as_deref(), Some("Sync|синк"));
    assert_eq!(body, "Body");
}

#[test]
fn plain_text_when_parsed_then_no_fields_and_whole_text() {
    let (fields, body) = parse_frontmatter("Just a plain prompt.");
    assert!(fields.is_empty());
    assert_eq!(body, "Just a plain prompt.");
}

#[test]
fn null_variants_when_parsed_then_all_none() {
    let (fields, _) = parse_frontmatter("---\na: null\nb: none\nc: ~\nd:\ne: NULL\n---\n\nBody");
    for key in ["a", "b", "c", "d", "e"] {
        assert_eq!(fields[key], None, "{key}");
    }
}

#[test]
fn unclosed_or_dashed_marker_when_parsed_then_text_is_kept_whole() {
    let text = "---\nname: x\n----\nbody";
    let (fields, body) = parse_frontmatter(text);
    assert!(fields.is_empty());
    assert_eq!(body, text);
    let (fields, body) = parse_frontmatter("---\nname: x\n---   \n\n  body  \n");
    assert_eq!(fields["name"].as_deref(), Some("x"));
    assert_eq!(body, "body");
}

#[test]
fn two_prompt_files_when_loaded_then_sorted_by_file_name() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "alpha.md", "---\nname: Alpha\nfilter: null\n---\n\nAlpha body");
    write(dir.path(), "beta.md", "---\nname: Beta\nfilter: \"Sync\"\n---\n\nBeta {transcript}");
    let prompts = PromptRegistry::new(dir.path()).load_all();
    assert_eq!(prompts.len(), 2);
    assert_eq!((prompts[0].id.as_str(), prompts[0].name.as_str()), ("alpha", "Alpha"));
    assert_eq!(prompts[0].filter, None);
    assert_eq!(prompts[0].template, "Alpha body");
    assert_eq!((prompts[1].id.as_str(), prompts[1].name.as_str()), ("beta", "Beta"));
    assert_eq!(prompts[1].filter.as_deref(), Some("Sync"));
    assert!(prompts[1].template.contains("{transcript}"));
}

#[test]
fn empty_directory_when_loaded_then_no_prompts() {
    let dir = tempfile::tempdir().unwrap();
    assert!(PromptRegistry::new(dir.path()).load_all().is_empty());
}

#[test]
fn missing_directory_when_loaded_then_no_prompts() {
    let dir = tempfile::tempdir().unwrap();
    assert!(PromptRegistry::new(dir.path().join("nonexistent")).load_all().is_empty());
}

#[test]
fn file_without_name_when_loaded_then_skipped() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "bad.md", "---\nfilter: null\n---\n\nNo name here");
    write(dir.path(), "good.md", "---\nname: Good\nfilter: null\n---\n\nGood body");
    let prompts = PromptRegistry::new(dir.path()).load_all();
    assert_eq!(prompts.iter().map(|prompt| prompt.id.as_str()).collect::<Vec<_>>(), ["good"]);
}

#[test]
fn hidden_crlf_and_directory_entries_when_loaded_then_like_pathlib_glob() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), ".hidden.md", "---\r\nname: Hidden\r\n---\r\nHidden {transcript}\r\n");
    std::fs::create_dir(dir.path().join("folder.md")).unwrap();
    write(dir.path(), "upper.MD", "---\nname: Upper\n---\nx");
    let prompts = PromptRegistry::new(dir.path()).load_all();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].id, ".hidden");
    assert_eq!(prompts[0].template, "Hidden {transcript}");
}

#[test]
fn existing_prompt_when_fetched_then_fields_are_loaded() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "my-prompt.md", "---\nname: My Prompt\nfilter: null\n---\n\nDo something with {user_name}");
    let config = PromptRegistry::new(dir.path()).get("my-prompt").unwrap();
    assert_eq!(config.id, "my-prompt");
    assert_eq!(config.name, "My Prompt");
    assert_eq!(config.filter, None);
    assert!(config.template.contains("{user_name}"));
}

#[test]
fn missing_prompt_when_fetched_then_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(PromptRegistry::new(dir.path()).get("nonexistent").is_none());
}

#[test]
fn null_filters_when_matched_then_every_prompt() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "---\nname: A\nfilter: null\n---\n\nA body");
    write(dir.path(), "b.md", "---\nname: B\nfilter: null\n---\n\nB body");
    assert_eq!(ids(&PromptRegistry::new(dir.path()), "Any Title").len(), 2);
}

#[test]
fn regex_filter_when_title_matches_then_filtered_and_universal_prompts() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "sync-only.md", "---\nname: Sync Only\nfilter: \"Sync|синк\"\n---\n\nSync body");
    write(dir.path(), "always.md", "---\nname: Always\nfilter: null\n---\n\nAlways body");
    let mut found = ids(&PromptRegistry::new(dir.path()), "Daily Sync");
    found.sort();
    assert_eq!(found, ["always", "sync-only"]);
}

#[test]
fn regex_filter_when_title_differs_then_only_universal_prompt() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "retro.md", "---\nname: Retro\nfilter: \"Retro\"\n---\n\nRetro body");
    write(dir.path(), "always.md", "---\nname: Always\nfilter: null\n---\n\nAlways body");
    assert_eq!(ids(&PromptRegistry::new(dir.path()), "Daily Sync"), ["always"]);
}

#[test]
fn lowercase_filter_when_title_is_uppercase_then_matches_case_insensitively() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "sync.md", "---\nname: Sync\nfilter: sync\n---\n\nSync body");
    assert_eq!(ids(&PromptRegistry::new(dir.path()), "Daily SYNC meeting"), ["sync"]);
    write(dir.path(), "cyrillic.md", "---\nname: C\nfilter: синк\n---\n\nC body");
    assert_eq!(ids(&PromptRegistry::new(dir.path()), "Еженедельный СИНК"), ["cyrillic"]);
}

#[test]
fn invalid_regex_when_matched_then_that_prompt_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "bad.md", "---\nname: Bad\nfilter: \"[invalid\"\n---\n\nBad body");
    write(dir.path(), "good.md", "---\nname: Good\nfilter: null\n---\n\nGood body");
    assert_eq!(ids(&PromptRegistry::new(dir.path()), "Some meeting"), ["good"]);
}

#[test]
fn lookahead_filter_when_matched_then_python_syntax_is_supported() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "not-retro.md", "---\nname: N\nfilter: \"^(?!.*retro).*sync\"\n---\n\nN body");
    let registry = PromptRegistry::new(dir.path());
    assert_eq!(ids(&registry, "Daily sync"), ["not-retro"]);
    assert!(ids(&registry, "sync retro").is_empty());
}

#[test]
fn shipped_my_increment_when_loaded_then_expected_name_filter_and_placeholders() {
    let config = PromptRegistry::new(default_prompts_dir()).get("my-increment").unwrap();
    assert_eq!(config.id, "my-increment");
    assert_eq!(config.name, "Инкремент");
    assert_eq!(config.filter, None);
    assert!(config.template.contains("{user_name}") && config.template.contains("{transcript}"));
}
