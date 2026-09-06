"""Configured title generation and orphan metadata without network calls."""

import os
import asyncio
from unittest.mock import AsyncMock, patch

import pytest

from recog.ai_provider import DEFAULT_CODEX_MODEL
from recog.title_generator import generate_title_async, generate_title, _sample_transcript
from recog.ai_settings import AiSettingsStore
from types import SimpleNamespace
import json


@pytest.mark.asyncio
async def test_generate_title_marks_prompt_and_runs_ephemeral_codex(tmp_path):
    created = {}

    class FakeProcess:
        returncode = 0

        async def communicate(self, input=None):
            created["input"] = input
            return (
                b'{"type":"item.completed","item":{"type":"agent_message","text":"Test Title"}}\n',
                b"",
            )

    async def fake_create_subprocess_exec(*cmd, **kwargs):
        created["cmd"] = cmd
        return FakeProcess()

    transcript = "[12:00:00] User: " + ("обсуждаем архитектуру " * 4)
    with patch("recog.ai_provider.resolve_cli", return_value="/mock/codex"), \
         patch.dict(os.environ, {}, clear=True), \
         patch("asyncio.create_subprocess_exec", new=AsyncMock(side_effect=fake_create_subprocess_exec)):
        title = await generate_title_async(transcript, settings=AiSettingsStore(tmp_path).resolve_task("title"))

    assert title == "Test Title"
    assert created["cmd"][3] == DEFAULT_CODEX_MODEL
    assert "--ephemeral" in created["cmd"]
    assert created["cmd"][-2:] == ("--json", "-")
    assert created["input"].decode().startswith("<!-- ccs-automation:recog task=title -->")


@pytest.mark.asyncio
async def test_saved_claude_title_when_generated_then_selected_model_and_single_pass_prompt(tmp_path):
    store = AiSettingsStore(tmp_path)
    store.write_models({"claude": "claude-custom"})
    store.write_task("title", {"providers": ["claude"], "prompt": '{user_name}: {transcript} / {"outer":{"inner":1}}'})
    settings = store.resolve_task("title")
    transcript = "literal {user_name} {{braces}} " * 4
    provider = SimpleNamespace(complete=AsyncMock(return_value='  "Название"  '))
    with patch("recog.title_generator.provider_for_settings", return_value=provider) as factory:
        title = await generate_title_async(transcript, settings=settings, variables={"user_name": "User"})
    assert title == "Название"
    assert factory.call_args.args[0]["models"]["claude"] == "claude-custom"
    assert factory.call_args.kwargs == {"task": "title"}
    assert provider.complete.call_args.args[0] == 'User: ' + transcript + ' / {"outer":{"inner":1}}'


@pytest.mark.asyncio
async def test_explicit_model_when_title_generated_then_snapshot_and_saved_settings_unchanged(tmp_path):
    store = AiSettingsStore(tmp_path)
    store.write_task("title", {"providers": ["claude"], "models": {"claude": "saved"}})
    settings = store.resolve_task("title")
    before = store.path.read_bytes()
    provider = SimpleNamespace(complete=AsyncMock(return_value="x" * 150))
    with patch("recog.title_generator.provider_for_settings", return_value=provider) as factory:
        title = await generate_title_async("speech " * 20, "one-call", settings=settings)
    assert title == "x" * 100
    assert factory.call_args.args[0]["models"]["claude"] == "one-call"
    assert settings["models"]["claude"] == "saved"
    assert store.path.read_bytes() == before


@pytest.mark.asyncio
@pytest.mark.parametrize("error", [ValueError("bad settings"), FileNotFoundError("no CLI"), asyncio.TimeoutError()])
async def test_title_failure_when_generated_then_none_and_bounded_timeout(tmp_path, error):
    provider = SimpleNamespace(complete=AsyncMock(side_effect=error))
    real_wait_for = asyncio.wait_for
    with patch("recog.title_generator.provider_for_settings", return_value=provider), \
         patch("recog.title_generator.asyncio.wait_for", wraps=real_wait_for) as wait:
        assert await generate_title_async("speech " * 20, settings=AiSettingsStore(tmp_path).resolve_task("title")) is None
    assert wait.call_args.kwargs["timeout"] == 90


@pytest.mark.asyncio
async def test_short_transcript_when_title_requested_then_no_provider_or_settings_read():
    with patch("recog.title_generator.AiSettingsStore") as store, \
         patch("recog.title_generator.provider_for_settings") as factory:
        assert await generate_title_async("short") is None
    store.assert_not_called()
    factory.assert_not_called()


def test_long_transcript_when_sampled_then_beginning_middle_and_end_are_represented():
    transcript = "\n".join(f"[{index:04d}] " + ("words " * 20) for index in range(120))
    sampled = _sample_transcript(transcript)
    assert "[0000]" in sampled and "[0060]" in sampled and "[0100]" in sampled
    assert len(sampled) < len(transcript)


def test_sync_title_when_given_snapshot_then_forwards_settings_and_variables(tmp_path):
    settings = AiSettingsStore(tmp_path).resolve_task("title")
    with patch("recog.title_generator.generate_title_async", new=AsyncMock(return_value="Title")) as generate:
        assert generate_title("speech", settings=settings, variables={"user_name": "User"}) == "Title"
    generate.assert_awaited_once_with("speech", None, settings=settings, variables={"user_name": "User"})


def test_orphan_title_when_generated_then_actual_provider_model_revision_persist(tmp_path):
    from recog.meeting_storage import MeetingStorage
    store = AiSettingsStore(tmp_path)
    saved = store.write_task("title", {"providers": ["claude"], "models": {"claude": "meeting-model"}})
    storage = MeetingStorage(tmp_path / "meetings")
    storage.start_session(ktalk=True)
    storage.append_segment(speaker="User", speaker_id="local", text="Обсуждаем архитектуру системы и все необходимые изменения.")
    path = storage.current_meeting_path
    with patch("recog.title_generator.generate_title", return_value="Архитектура системы") as generate:
        storage.end_session()
    meta = json.loads((path / "meta.json").read_text())
    assert meta["title"] == "Архитектура системы"
    assert meta["title_source"] == "claude"
    assert meta["title_model"] == "meeting-model"
    assert meta["title_settings_revision"] == saved["revision"]
    assert generate.call_args.kwargs["settings"]["revision"] == saved["revision"]
    assert meta["ended_at"] and meta["end_reason"] == "session_end"


def test_corrupt_title_settings_when_orphan_ends_then_session_still_closes(tmp_path):
    from recog.meeting_storage import MeetingStorage
    store = AiSettingsStore(tmp_path)
    store.path.write_text("broken settings")
    storage = MeetingStorage(tmp_path / "meetings")
    storage.start_session(ktalk=True)
    storage.append_segment(speaker="User", speaker_id="local", text="Обсуждаем архитектуру системы и все необходимые изменения.")
    path = storage.current_meeting_path
    with patch("recog.title_generator.generate_title") as generate:
        storage.end_session()
    generate.assert_not_called()
    meta = json.loads((path / "meta.json").read_text())
    assert meta["title"] is None
    assert meta["ended_at"] and meta["end_reason"] == "session_end"
