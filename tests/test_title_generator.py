"""Tests for Codex-backed meeting title generation."""

from unittest.mock import AsyncMock, patch

import pytest

from recog.title_generator import generate_title_async


@pytest.mark.asyncio
async def test_generate_title_marks_prompt_and_runs_ephemeral_codex():
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
    with patch("recog.title_generator.resolve_cli", return_value="/mock/codex"), \
         patch("asyncio.create_subprocess_exec", new=AsyncMock(side_effect=fake_create_subprocess_exec)):
        title = await generate_title_async(transcript, model="gpt-5.4")

    assert title == "Test Title"
    assert "--ephemeral" in created["cmd"]
    assert created["cmd"][-2:] == ("--json", "-")
    assert created["input"].decode().startswith("<!-- ccs-automation:recog task=title -->")
