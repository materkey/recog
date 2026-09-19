"""AI settings contracts use isolated data directories, never installed settings."""

from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import uuid

import pytest

from recog.ai_defaults import COPILOT_PROMPT, DEFAULT_MODELS
from recog.ai_settings import AiSettingsStore, SettingsConflictError, SettingsReadError, render_prompt


@pytest.fixture(autouse=True)
def clean_legacy_settings(monkeypatch):
    for key in ("CLAUDE_MODEL", "CODEX_MODEL", "OPENROUTER_MODEL", "CODEX_REASONING_EFFORT", "AI_PROVIDER"):
        monkeypatch.delenv(key, raising=False)


@pytest.fixture
def store(tmp_path):
    return AiSettingsStore(tmp_path / "data", prompts_dir=tmp_path / "prompts")


def test_missing_settings_when_read_then_defaults_without_creating_data(store):
    assert store.read() == {"version": 1, "revision": "defaults", "models": {}, "tasks": {}}
    view = store.view()
    assert view["models"] == DEFAULT_MODELS
    assert {task["id"] for task in view["tasks"]} == {
        "analysis.increment", "title", "copilot",
    }
    assert not store.path.parent.exists()


def test_increment_registry_given_catalog_when_configured_then_one_entry_controls_both_paths(store):
    store.prompts_dir.mkdir()
    (store.prompts_dir / "my-increment.md").write_text('---\nname: My increment\n---\nShared {transcript}')
    tasks = store.view()["tasks"]
    assert [task["id"] for task in tasks].count("analysis.increment") == 1
    assert "saved:my-increment" not in {task["id"] for task in tasks}
    assert store.resolve_task("analysis.increment")["prompt"] == "Shared {transcript}"
    assert store.resolve_task("saved:my-increment")["prompt"] == "Shared {transcript}"
    store.write_task("analysis.increment", {"prompt": "Edited {transcript}",
                     "providers": ["claude"], "models": {"claude": "chosen"}})
    for task_id in ("analysis.increment", "saved:my-increment"):
        task = store.resolve_task(task_id)
        assert task["prompt"] == "Edited {transcript}"
        assert task["providers"] == ["claude"]
        assert task["models"]["claude"] == "chosen"
    store.write_task("analysis.increment", {"providers": ["claude", "codex"]})
    assert store.resolve_task("saved:my-increment")["providers"] == ["codex"]


def test_legacy_saved_increment_given_read_and_reset_when_aliased_then_preferences_survive_until_reset(store):
    store.path.parent.mkdir()
    raw = {"version": 1, "revision": str(uuid.uuid4()), "models": {}, "tasks": {
        "saved:my-increment": {"prompt": "Legacy {transcript}", "claude_effort": "max"},
    }}
    store.path.write_text(json.dumps(raw))
    original = store.path.read_bytes()
    assert store.resolve_task("analysis.increment")["prompt"] == "Legacy {transcript}"
    assert store.resolve_task("analysis.increment")["claude_effort"] == "max"
    assert store.path.read_bytes() == original
    store.reset_task("analysis.increment", raw["revision"])
    assert not store.resolve_task("saved:my-increment")["customized"]


def test_retired_features_given_legacy_file_when_loaded_then_active_settings_remain_usable(store):
    store.path.parent.mkdir()
    raw = {"version": 1, "revision": str(uuid.uuid4()), "models": {"openrouter": "old", "codex": "kept"}, "tasks": {
        "anchor": {"prompt": "Removed {transcript}"}, "analysis.custom": {"prompt": "Removed {transcript}"},
        "analysis.summary": {"prompt": "Retired summary {transcript}"},
        "title": {"providers": ["openrouter"], "models": {"openrouter": "old"}, "prompt": "Keep {transcript}"},
    }}
    store.path.write_text(json.dumps(raw))
    original = store.path.read_bytes()
    view = store.view()
    assert set(view["models"]) == {"claude", "codex"}
    assert view["models"]["codex"] == "kept"
    assert {task["id"] for task in view["tasks"]} == {"analysis.increment", "title", "copilot"}
    assert store.resolve_task("title")["providers"] == ["codex"]
    assert store.resolve_task("title")["prompt"] == "Keep {transcript}"
    assert store.path.read_bytes() == original
    with pytest.raises(ValueError):
        store.write_models({"openrouter": "removed"})
    with pytest.raises(ValueError):
        store.write_task("anchor", {"prompt": "Removed {transcript}"})
    with pytest.raises(ValueError):
        store.resolve_task("analysis.summary")
    with pytest.raises(ValueError):
        store.write_task("analysis.summary", {"prompt": "Retired {transcript}"})


def test_codex_auto_given_scenario_when_saved_then_empty_effort_roundtrips(store):
    store.write_task("analysis.increment", {"reasoning_effort": ""})
    assert store.resolve_task("analysis.increment")["reasoning_effort"] == ""
    assert store.resolve_task("saved:my-increment")["reasoning_effort"] == ""


def test_global_and_task_overrides_when_resolved_then_precedence_and_blank_inheritance(store, monkeypatch):
    monkeypatch.setenv("CLAUDE_MODEL", "legacy-claude")
    monkeypatch.setenv("CODEX_MODEL", "legacy-codex")
    assert store.view()["models"]["claude"] == "legacy-claude"
    first = store.write_models({"claude": "global-claude", "codex": "global-codex"}, "defaults")
    store.write_task("title", {"models": {"codex": "title-model"}}, first["revision"])
    assert store.resolve_task("title")["models"]["codex"] == "title-model"
    assert store.resolve_task("analysis.increment")["models"]["codex"] == "global-codex"
    store.write_task("title", {"models": {"codex": " "}})
    assert store.resolve_task("title")["models"]["codex"] == "global-codex"
    store.write_models({"codex": ""})
    assert store.resolve_task("title")["models"]["codex"] == "legacy-codex"
    assert store.view()["models"]["claude"] == "global-claude"


def test_saved_models_when_legacy_value_is_invalid_then_saved_value_wins(store, monkeypatch):
    store.write_models({"codex": "saved"})
    monkeypatch.setenv("CODEX_MODEL", "invalid legacy value")
    assert store.view()["models"]["codex"] == "saved"


def test_task_override_when_replaced_and_reset_then_only_selected_task_changes(store):
    store.write_models({"codex": "global"})
    store.write_task("title", {"prompt": "Title {transcript}", "providers": ["claude"], "reasoning_effort": "low"})
    store.write_task("analysis.increment", {"prompt": "Anchor {transcript}"})
    view = store.write_task("title", {"providers": ["codex"]})
    title = store.resolve_task("title")
    assert title["prompt"] == title["default_prompt"]
    assert title["reasoning_effort"] == "xhigh"
    reset = store.reset_task("title", view["revision"])
    assert reset["revision"] != view["revision"]
    assert not store.resolve_task("title")["customized"]
    assert store.resolve_task("analysis.increment")["prompt"] == "Anchor {transcript}"
    assert reset["models"]["codex"] == "global"


def test_legacy_providers_and_effort_when_resolved_then_defaults_retained(store, monkeypatch):
    monkeypatch.setenv("AI_PROVIDER", "openrouter")
    monkeypatch.setenv("CODEX_REASONING_EFFORT", "high")
    assert store.resolve_task("title")["reasoning_effort"] == "high"
    assert store.resolve_task("analysis.increment")["providers"] == ["claude", "codex"]
    assert store.resolve_task("copilot")["allowed_providers"] == ["claude"]


def test_snapshot_when_global_changes_then_all_task_values_keep_snapshot(store):
    store.write_models({"codex": "old"})
    snapshot = store.read()
    store.write_models({"codex": "new"})
    resolved = store.resolve_task("title", snapshot=snapshot)
    assert resolved["models"]["codex"] == "old"
    assert resolved["revision"] == snapshot["revision"]


def test_saved_registry_when_edited_then_defaults_refresh_without_writing_registry(store):
    store.prompts_dir.mkdir()
    path = store.prompts_dir / "notes.md"
    path.write_text('---\nname: Notes\n---\nSummarize {transcript}')
    assert store.resolve_task("saved:notes")["label"] == "Notes"
    assert store.resolve_task("saved:notes")["prompt"] == "Summarize {transcript}"
    store.write_task("saved:notes", {"prompt": "Custom {transcript}"})
    assert path.read_text().endswith("Summarize {transcript}")
    path.write_text('---\nname: Updated\n---\nUpdated {transcript}')
    assert store.resolve_task("saved:notes")["default_prompt"] == "Updated {transcript}"
    store.reset_task("saved:notes")
    assert store.resolve_task("saved:notes")["prompt"] == "Updated {transcript}"
    assert "saved:notes" in {task["id"] for task in store.view()["tasks"]}
    with pytest.raises(ValueError, match="Unknown saved"):
        store.resolve_task("saved:missing")
    assert store.resolve_task("saved:generated", fallback_prompt="Fallback {transcript}", fallback_label="Generated")["label"] == "Generated"
    with pytest.raises(ValueError):
        store.resolve_task("saved:../outside")


@pytest.mark.parametrize("values", [
    {"unknown": True}, {"prompt": ""}, {"prompt": "No transcript"},
    {"prompt": "{transcript} {unknown}"}, {"prompt": "{transcript!r}"},
    {"prompt": "{transcript.__class__}"}, {"prompt": "x" * 32001 + "{transcript}"},
    {"providers": []}, {"providers": ["claude", "claude"]}, {"providers": "claude"},
    {"providers": ["unknown"]}, {"providers": [None]}, {"models": {"api_key": "secret"}},
    {"models": {"codex": None}}, {"models": {"codex": "two words"}},
    {"models": {"codex": "model\n"}}, {"models": {"codex": "--option"}},
    {"models": {"codex": "x" * 161}}, {"models": {"codex": "a\u200bb"}},
    {"reasoning_effort": "max"}, {"reasoning_effort": None},
])
def test_invalid_task_input_when_saved_then_no_settings_file(store, values):
    with pytest.raises(ValueError) as error:
        store.write_task("analysis.increment", values)
    assert not isinstance(error.value, SettingsReadError)
    assert not store.path.exists()


@pytest.mark.parametrize("task_id,values", [
    ("title", {"providers": ["claude", "codex"]}),
    ("copilot", {"providers": ["codex"]}),
    ("copilot", {"models": {"codex": "model"}}),
    ("copilot", {"reasoning_effort": "high"}),
    ("missing", {"prompt": "{transcript}"}),
])
def test_task_capabilities_when_unsupported_then_rejected(store, task_id, values):
    with pytest.raises(ValueError):
        store.write_task(task_id, values)


def test_copilot_literal_policy_when_saved_then_braces_are_not_template_variables(store):
    prompt = COPILOT_PROMPT + '\nExample: {unknown} {"json": true}'
    store.write_task("copilot", {"prompt": prompt})
    assert store.resolve_task("copilot")["prompt"] == prompt
    assert store.resolve_task("copilot")["required_placeholders"] == []


def test_json_template_and_transcript_braces_when_rendered_then_single_pass_preserves_data():
    transcript = '{unknown} {user_name} {{nested}} {"json": 1}'
    template = 'JSON: {"literal": true}, escaped {{unknown}}, user {user_name}, text {transcript}'
    assert render_prompt(template, {"transcript": transcript, "user_name": "Alex"}) == (
        'JSON: {"literal": true}, escaped {unknown}, user Alex, text ' + transcript
    )
    assert render_prompt('{user_name}/{meeting_title}/{participants}/{transcript}', {"transcript": "speech"}) == "///speech"
    with pytest.raises(ValueError, match="Missing"):
        render_prompt("{transcript}", {})
    with pytest.raises(ValueError, match="Unknown"):
        render_prompt("{unknown}", {"unknown": "text"})


def test_nested_literal_json_when_rendered_then_adjacent_closing_braces_remain():
    literal = '{"outer":{"inner":{"value":1}}}'
    assert render_prompt(literal + " {transcript}", {"transcript": "speech"}) == literal + " speech"
    escaped = '{{"outer":{{"inner":{{"value":1}}}}}}'
    assert render_prompt(escaped, {}) == literal


def test_revision_conflict_when_parallel_drafts_saved_then_one_wins(store):
    store.write_models({"codex": "initial"})
    revision = store.read()["revision"]

    def write(model):
        try:
            return AiSettingsStore(store.path.parent, store.prompts_dir).write_models({"codex": model}, revision)
        except SettingsConflictError:
            return None

    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(write, ["first", "second"]))
    assert sum(result is not None for result in results) == 1
    assert store.read()["models"]["codex"] in {"first", "second"}
    with pytest.raises(SettingsConflictError):
        store.reset_task("title", revision)


def test_private_atomic_write_when_successful_then_uuid_and_permissions_persist(store):
    saved = store.write_models({"codex": "configured"})
    assert str(uuid.UUID(saved["revision"])) == saved["revision"]
    assert store.path.stat().st_mode & 0o777 == 0o600
    assert store.path.with_suffix(".lock").stat().st_mode & 0o777 == 0o600
    assert AiSettingsStore(store.path.parent, store.prompts_dir).view() == saved


@pytest.mark.parametrize("failure", ["replace", "fsync"])
def test_atomic_failure_when_saving_then_previous_bytes_and_no_temp_files(store, monkeypatch, failure):
    store.write_models({"codex": "old"})
    before = store.path.read_bytes()

    def fail(*args):
        if failure == "replace":
            assert os.stat(args[0]).st_mode & 0o777 == 0o600
            assert store.path.read_bytes() == before
        raise OSError("simulated failure")

    monkeypatch.setattr(f"recog.ai_settings.os.{failure}", fail)
    with pytest.raises(OSError, match="simulated"):
        store.write_models({"codex": "new"})
    assert store.path.read_bytes() == before
    assert sorted(path.name for path in store.path.parent.iterdir()) == ["ai-settings.json", "ai-settings.lock"]


@pytest.mark.parametrize("payload", [b"not json", b"\xff", b"[]", b"{}", b"x" * (256 * 1024 + 1)])
def test_corrupt_settings_when_read_or_written_then_error_preserves_file(store, payload):
    store.path.parent.mkdir()
    store.path.write_bytes(payload)
    for operation in (store.read, store.view, lambda: store.write_models({"codex": "new"}), lambda: store.reset_task("title")):
        with pytest.raises(SettingsReadError):
            operation()
        assert store.path.read_bytes() == payload


@pytest.mark.parametrize("mutation", [
    lambda raw: raw.update(secret="do not accept"),
    lambda raw: raw.update(version=True),
    lambda raw: raw.update(revision="defaults"),
    lambda raw: raw.update(tasks={"title": {"unknown": True}}),
    lambda raw: raw.update(models={"api_key": "secret"}),
    lambda raw: raw.update(tasks={"title": {"prompt": "{unknown}"}}),
])
def test_invalid_file_schema_when_read_then_visible_failure(store, mutation):
    store.write_models({})
    raw = store.read()
    mutation(raw)
    store.path.write_text(json.dumps(raw))
    before = store.path.read_bytes()
    with pytest.raises(SettingsReadError):
        store.write_models({"codex": "new"})
    assert store.path.read_bytes() == before


def test_total_limit_when_many_large_prompts_saved_then_previous_file_preserved(store):
    store.prompts_dir.mkdir()
    for number in range(10):
        (store.prompts_dir / f"p{number}.md").write_text('---\nname: Large\n---\n{transcript}')
    for number in range(8):
        store.write_task(f"saved:p{number}", {"prompt": "x" * 31900 + "{transcript}"})
    before = store.path.read_bytes()
    with pytest.raises(ValueError, match="size limit"):
        store.write_task("saved:p8", {"prompt": "x" * 31900 + "{transcript}"})
    assert store.path.read_bytes() == before
