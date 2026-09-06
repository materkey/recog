"""Tests for recog/prompt_registry — PromptRegistry and PromptConfig."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.prompt_registry import PromptConfig, PromptRegistry, _parse_frontmatter


# --- _parse_frontmatter ---


def test_parse_frontmatter_basic():
    """
    given: markdown text with valid frontmatter
    when: _parse_frontmatter called
    then: returns parsed dict and body
    """
    text = '---\nname: My increment\nfilter: null\n---\n\nPrompt body here.'
    fm, body = _parse_frontmatter(text)
    assert fm["name"] == "My increment"
    assert fm["filter"] is None
    assert body == "Prompt body here."


def test_parse_frontmatter_quoted_values():
    """
    given: frontmatter with quoted string values
    when: _parse_frontmatter called
    then: quotes are stripped from values
    """
    text = '---\nname: "Team sync"\nfilter: "Sync|синк"\n---\n\nBody'
    fm, body = _parse_frontmatter(text)
    assert fm["name"] == "Team sync"
    assert fm["filter"] == "Sync|синк"
    assert body == "Body"


def test_parse_frontmatter_no_frontmatter():
    """
    given: plain text without frontmatter markers
    when: _parse_frontmatter called
    then: returns empty dict and full text as body
    """
    text = "Just a plain prompt."
    fm, body = _parse_frontmatter(text)
    assert fm == {}
    assert body == "Just a plain prompt."


def test_parse_frontmatter_null_variants():
    """
    given: frontmatter with various null representations
    when: _parse_frontmatter called
    then: all parsed as None
    """
    text = '---\na: null\nb: none\nc: ~\nd:\n---\n\nBody'
    fm, body = _parse_frontmatter(text)
    assert fm["a"] is None
    assert fm["b"] is None
    assert fm["c"] is None
    assert fm["d"] is None


# --- PromptRegistry.load_all ---


def test_load_all_returns_prompts(tmp_path):
    """
    given: directory with two valid prompt files
    when: load_all() called
    then: returns list of PromptConfig sorted by filename
    """
    (tmp_path / "alpha.md").write_text(
        '---\nname: Alpha\nfilter: null\n---\n\nAlpha body',
        encoding="utf-8",
    )
    (tmp_path / "beta.md").write_text(
        '---\nname: Beta\nfilter: "Sync"\n---\n\nBeta {transcript}',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    prompts = registry.load_all()

    assert len(prompts) == 2
    assert prompts[0].id == "alpha"
    assert prompts[0].name == "Alpha"
    assert prompts[0].filter is None
    assert prompts[0].template == "Alpha body"
    assert prompts[1].id == "beta"
    assert prompts[1].name == "Beta"
    assert prompts[1].filter == "Sync"
    assert "{transcript}" in prompts[1].template


def test_load_all_empty_directory(tmp_path):
    """
    given: empty prompts directory
    when: load_all() called
    then: returns empty list
    """
    registry = PromptRegistry(tmp_path)
    assert registry.load_all() == []


def test_load_all_missing_directory(tmp_path):
    """
    given: non-existent prompts directory
    when: load_all() called
    then: returns empty list
    """
    registry = PromptRegistry(tmp_path / "nonexistent")
    assert registry.load_all() == []


def test_load_all_skips_missing_name(tmp_path):
    """
    given: prompt file without name in frontmatter
    when: load_all() called
    then: skips that file, returns others
    """
    (tmp_path / "bad.md").write_text(
        '---\nfilter: null\n---\n\nNo name here',
        encoding="utf-8",
    )
    (tmp_path / "good.md").write_text(
        '---\nname: Good\nfilter: null\n---\n\nGood body',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    prompts = registry.load_all()
    assert len(prompts) == 1
    assert prompts[0].id == "good"


# --- PromptRegistry.get ---


def test_get_returns_prompt(tmp_path):
    """
    given: valid prompt file exists
    when: get("my-prompt") called
    then: returns PromptConfig with correct fields
    """
    (tmp_path / "my-prompt.md").write_text(
        '---\nname: My Prompt\nfilter: null\n---\n\nDo something with {user_name}',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    config = registry.get("my-prompt")

    assert config is not None
    assert config.id == "my-prompt"
    assert config.name == "My Prompt"
    assert config.filter is None
    assert "{user_name}" in config.template


def test_get_returns_none_for_missing(tmp_path):
    """
    given: no file for requested prompt_id
    when: get("nonexistent") called
    then: returns None
    """
    registry = PromptRegistry(tmp_path)
    assert registry.get("nonexistent") is None


# --- PromptRegistry.matching ---


def test_matching_returns_all_null_filter_prompts(tmp_path):
    """
    given: two prompts with filter=null
    when: matching("Any Title") called
    then: returns both prompts
    """
    (tmp_path / "a.md").write_text(
        '---\nname: A\nfilter: null\n---\n\nA body',
        encoding="utf-8",
    )
    (tmp_path / "b.md").write_text(
        '---\nname: B\nfilter: null\n---\n\nB body',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    result = registry.matching("Any Title")
    assert len(result) == 2


def test_matching_filters_by_regex(tmp_path):
    """
    given: prompt with filter "Sync|синк", another with filter=null
    when: matching("Daily Sync") called
    then: returns both (regex matches + null filter)
    """
    (tmp_path / "sync-only.md").write_text(
        '---\nname: Sync Only\nfilter: "Sync|синк"\n---\n\nSync body',
        encoding="utf-8",
    )
    (tmp_path / "always.md").write_text(
        '---\nname: Always\nfilter: null\n---\n\nAlways body',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    result = registry.matching("Daily Sync")
    assert len(result) == 2
    ids = {p.id for p in result}
    assert "sync-only" in ids
    assert "always" in ids


def test_matching_excludes_non_matching_filter(tmp_path):
    """
    given: prompt with filter "Retro", another with filter=null
    when: matching("Daily Sync") called
    then: returns only the null-filter prompt
    """
    (tmp_path / "retro.md").write_text(
        '---\nname: Retro\nfilter: "Retro"\n---\n\nRetro body',
        encoding="utf-8",
    )
    (tmp_path / "always.md").write_text(
        '---\nname: Always\nfilter: null\n---\n\nAlways body',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    result = registry.matching("Daily Sync")
    assert len(result) == 1
    assert result[0].id == "always"


def test_matching_case_insensitive(tmp_path):
    """
    given: prompt with filter "sync"
    when: matching("Daily SYNC meeting") called
    then: filter matches case-insensitively
    """
    (tmp_path / "sync.md").write_text(
        '---\nname: Sync\nfilter: sync\n---\n\nSync body',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    result = registry.matching("Daily SYNC meeting")
    assert len(result) == 1
    assert result[0].id == "sync"


def test_matching_bad_regex_skipped(tmp_path):
    """
    given: prompt with invalid regex in filter
    when: matching() called
    then: that prompt is skipped, no crash
    """
    (tmp_path / "bad.md").write_text(
        '---\nname: Bad\nfilter: "[invalid"\n---\n\nBad body',
        encoding="utf-8",
    )
    (tmp_path / "good.md").write_text(
        '---\nname: Good\nfilter: null\n---\n\nGood body',
        encoding="utf-8",
    )

    registry = PromptRegistry(tmp_path)
    result = registry.matching("Some meeting")
    assert len(result) == 1
    assert result[0].id == "good"


# --- Default prompt file ---


def test_default_my_increment_prompt_loads():
    """
    given: the shipped prompts/my-increment.md file
    when: loaded via PromptRegistry
    then: has correct id, name, null filter, and contains expected placeholders
    """
    prompts_dir = Path(__file__).resolve().parent.parent / "recog/prompts"
    registry = PromptRegistry(prompts_dir)
    config = registry.get("my-increment")

    assert config is not None
    assert config.id == "my-increment"
    assert config.name == "Инкремент"
    assert config.filter is None
    assert "{user_name}" in config.template
    assert "{transcript}" in config.template
