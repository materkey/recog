"""Tests for recog.ai_provider module."""
import os
import sys
from pathlib import Path
from unittest.mock import AsyncMock, patch, MagicMock

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.ai_provider import (
    CCS_AUTOMATION_MARKER_PREFIX,
    DEFAULT_CODEX_MODEL,
    AIProvider,
    CodexCliProvider,
    build_codex_exec_command,
    mark_ccs_automation,
    resolve_cli,
)


# ---------------------------------------------------------------------------
# resolve_cli
# ---------------------------------------------------------------------------

class TestResolveCli:

    def test_returns_path_from_shutil_which(self):
        """given: binary on PATH - when: resolve_cli - then: returns shutil.which result"""
        with patch("shutil.which", return_value="/usr/local/bin/codex"):
            result = resolve_cli("codex")
        assert result == "/usr/local/bin/codex"

    def test_falls_back_to_common_locations(self, tmp_path):
        """given: binary not on PATH but in ~/.local/bin - when: resolve_cli - then: returns that path"""
        fake_bin = tmp_path / "codex"
        fake_bin.write_text("#!/bin/sh\n")
        fake_bin.chmod(0o755)

        with patch("shutil.which", return_value=None), \
             patch("os.path.expanduser", return_value=str(fake_bin)):
            result = resolve_cli("codex")
        assert result == str(fake_bin)

    def test_falls_back_to_homebrew_bin(self):
        """given: binary only in /opt/homebrew/bin (Apple Silicon Homebrew), not on
        PATH - when: resolve_cli - then: returns the homebrew path"""
        homebrew_path = "/opt/homebrew/bin/codex"

        with patch("shutil.which", return_value=None), \
             patch("glob.glob", return_value=[]), \
             patch("os.path.isfile", side_effect=lambda p: p == homebrew_path), \
             patch("os.access", side_effect=lambda p, _mode: p == homebrew_path):
            result = resolve_cli("codex")
        assert result == homebrew_path

    def test_nonexistent_binary_raises_file_not_found(self):
        """given: binary not found anywhere - when: resolve_cli - then: raises FileNotFoundError"""
        with patch("shutil.which", return_value=None), \
             patch("os.path.expanduser", side_effect=lambda p: p.replace("~", "/nonexistent_home_xyz")):
            with pytest.raises(FileNotFoundError, match="nonexistent_binary_xyz_12345"):
                resolve_cli("nonexistent_binary_xyz_12345")

    def test_nvm_candidates_sorted_by_version(self, tmp_path):
        """given: multiple nvm versions - when: resolve_cli - then: picks highest version"""
        # Create fake nvm structure: v18.0.0/bin/codex and v20.0.0/bin/codex
        nvm_base = tmp_path / ".nvm" / "versions" / "node"
        for ver in ["v18.0.0", "v20.0.0"]:
            bin_dir = nvm_base / ver / "bin"
            bin_dir.mkdir(parents=True)
            codex = bin_dir / "codex"
            codex.write_text("#!/bin/sh\n")
            codex.chmod(0o755)

        # Treat only files inside tmp_path as existing, so fixed system paths
        # (/opt/homebrew/bin, /usr/local/bin) that may hold a real codex on the
        # dev machine don't shadow the nvm candidates under test.
        real_isfile = os.path.isfile
        with patch("shutil.which", return_value=None), \
             patch("os.path.expanduser", side_effect=lambda p: str(tmp_path / p.lstrip("~/"))), \
             patch("os.path.isfile", side_effect=lambda p: real_isfile(p) and str(tmp_path) in p):
            import glob as glob_module
            original_glob = glob_module.glob

            def patched_glob(pattern):
                # Redirect nvm glob to tmp_path
                if ".nvm" in pattern:
                    return original_glob(str(nvm_base / "*/bin/codex"))
                return []

            with patch("glob.glob", side_effect=patched_glob):
                result = resolve_cli("codex")

        # Should pick v20 (higher version)
        assert "v20.0.0" in result


# ---------------------------------------------------------------------------
# CodexCliProvider
# ---------------------------------------------------------------------------

_AGENT_MESSAGE = '{"type":"item.completed","item":{"type":"agent_message","text":"%s"}}'
_TURN_FAILED = '{"type":"turn.failed","error":{"message":"stream disconnected"}}'


def _codex_process(stdout_lines, returncode, stderr):
    """A finished codex exec process that printed these JSON lines."""
    encoded = [line.encode() + b"\n" for line in stdout_lines]

    async def lines():
        for line in encoded:
            yield line

    proc = MagicMock(returncode=returncode)
    proc.communicate = AsyncMock(return_value=(b"".join(encoded), stderr))
    proc.stdout = lines()
    proc.stdin.drain = AsyncMock()
    proc.wait = AsyncMock()
    proc.stderr.read = AsyncMock(return_value=stderr)
    return proc


async def _codex_complete(stdout_lines, returncode=0, stderr=b""):
    proc = _codex_process(stdout_lines, returncode, stderr)
    with (
        patch("recog.ai_provider.resolve_cli", return_value="/mock/codex"),
        patch("asyncio.create_subprocess_exec", new=AsyncMock(return_value=proc)),
    ):
        return await CodexCliProvider().complete("hello")


async def _codex_stream(stdout_lines, returncode=0, stderr=b""):
    proc = _codex_process(stdout_lines, returncode, stderr)
    with (
        patch("recog.ai_provider.resolve_cli", return_value="/mock/codex"),
        patch("asyncio.create_subprocess_exec", new=AsyncMock(return_value=proc)),
    ):
        return [item async for item in CodexCliProvider().stream("hello")]


class TestCodexCliProvider:

    def test_importable_from_recog(self):
        """given: recog.ai_provider - when: import CodexCliProvider - then: no error"""
        from recog.ai_provider import CodexCliProvider
        assert CodexCliProvider is not None

    def test_key_is_codex(self):
        """given: CodexCliProvider - when: .key - then: returns 'codex'"""
        with patch("recog.ai_provider.resolve_cli", return_value="/usr/bin/codex"):
            provider = CodexCliProvider()
        assert provider.key == "codex"

    def test_no_model_override_when_creating_codex_provider_then_astra_is_used(self):
        with patch("recog.ai_provider.resolve_cli", return_value="/usr/bin/codex"), \
             patch.dict(os.environ, {}, clear=True):
            provider = CodexCliProvider()
        assert provider.model == DEFAULT_CODEX_MODEL == "gpt-6-astra"

    def test_name_includes_model(self):
        """given: CodexCliProvider - when: .name - then: includes model name"""
        with patch("recog.ai_provider.resolve_cli", return_value="/usr/bin/codex"), \
             patch.dict(os.environ, {"CODEX_MODEL": "gpt-5.4", "CODEX_REASONING_EFFORT": "xhigh"}):
            provider = CodexCliProvider()
        assert "gpt-5.4" in provider.name
        assert "xhigh" in provider.name

    def test_name_spark_model_no_reasoning(self):
        """given: spark model - when: .name - then: no reasoning effort shown"""
        with patch("recog.ai_provider.resolve_cli", return_value="/usr/bin/codex"), \
             patch.dict(os.environ, {"CODEX_MODEL": "o4-mini-spark"}):
            provider = CodexCliProvider()
        assert "spark" in provider.name
        assert "xhigh" not in provider.name

    def test_is_subclass_of_ai_provider(self):
        """given: CodexCliProvider - when: check inheritance - then: is AIProvider subclass"""
        assert issubclass(CodexCliProvider, AIProvider)

    def test_resolve_cli_called_on_init(self):
        """given: CodexCliProvider() - when: init - then: resolve_cli("codex") is called"""
        with patch("recog.ai_provider.resolve_cli", return_value="/mock/codex") as mock_resolve:
            provider = CodexCliProvider()
        mock_resolve.assert_called_once_with("codex")
        assert provider._cli == "/mock/codex"

    def test_mark_ccs_automation_prepends_marker_once(self):
        marked = mark_ccs_automation("hello", "Increment Prompt")

        assert marked.startswith("<!-- ccs-automation:recog task=increment-prompt -->\n")
        assert mark_ccs_automation(marked, "title") == marked

    def test_build_codex_exec_command_uses_ephemeral(self):
        cmd = build_codex_exec_command("/usr/bin/codex", "gpt-5.4", "xhigh")

        assert cmd == [
            "/usr/bin/codex",
            "exec",
            "-m",
            "gpt-5.4",
            "-c",
            'model_reasoning_effort="xhigh"',
            "--ephemeral",
            "--json",
            "-",
        ]

    def test_isolated_analysis_when_building_command_then_custom_context_is_disabled(self):
        cmd = build_codex_exec_command("/mock/codex", "test-model", "xhigh", isolated=True)
        assert "--ignore-user-config" in cmd
        overrides = {cmd[index + 1] for index, value in enumerate(cmd) if value == "-c"}
        assert overrides == {
            'model_reasoning_effort="xhigh"',
            "skills.include_instructions=false", "skills.bundled.enabled=false",
            "features.plugins=false", "features.hooks=false", "features.memories=false",
            "features.skill_search=false", "project_doc_max_bytes=0",
        }
        assert "--ephemeral" in cmd
        assert cmd[-2:] == ["--json", "-"]

    @pytest.mark.asyncio
    async def test_complete_marks_prompt_and_runs_ephemeral_codex(self):
        """given: CodexCliProvider - when: complete - then: prompt is marked and command is ephemeral"""
        created = {}

        class FakeProcess:
            returncode = 0

            async def communicate(self, input=None):
                created["input"] = input
                return (
                    b'{"type":"item.completed","item":{"type":"agent_message","text":"answer"}}\n',
                    b"",
                )

        async def fake_create_subprocess_exec(*cmd, **kwargs):
            created["cmd"] = cmd
            created["kwargs"] = kwargs
            return FakeProcess()

        with patch("recog.ai_provider.resolve_cli", return_value="/mock/codex"), \
             patch("asyncio.create_subprocess_exec", new=AsyncMock(side_effect=fake_create_subprocess_exec)):
            provider = CodexCliProvider(task="increment")
            answer = await provider.complete("hello")

        assert answer == "answer"
        assert "--ephemeral" in created["cmd"]
        assert "--ignore-user-config" in created["cmd"]
        assert "skills.include_instructions=false" in created["cmd"]
        assert created["cmd"][-2:] == ("--json", "-")
        assert created["input"].decode().startswith(
            f"{CCS_AUTOMATION_MARKER_PREFIX} task=increment -->\nhello"
        )

    @pytest.mark.asyncio
    async def test_reconnect_notice_then_answer_when_completing_then_answer_is_returned(self):
        # codex exec --json emits retry notices (will_retry) as "error" events too
        answer = await _codex_complete(
            [
                '{"type":"error","message":"Reconnecting... 2/5 (request timed out)"}',
                _AGENT_MESSAGE % "answer",
                '{"type":"turn.completed"}',
            ]
        )

        assert answer == "answer"

    @pytest.mark.asyncio
    async def test_turn_failed_when_completing_then_its_error_is_raised(self):
        with pytest.raises(Exception, match="^stream disconnected$"):
            await _codex_complete(
                [
                    '{"type":"error","message":"Reconnecting... 5/5 (request timed out)"}',
                    _TURN_FAILED,
                ]
            )

    @pytest.mark.asyncio
    async def test_codex_exits_nonzero_when_completing_then_stderr_is_raised(self):
        with pytest.raises(Exception, match="^Codex CLI error: auth expired$"):
            await _codex_complete([], returncode=1, stderr=b"auth expired\n")

    @pytest.mark.asyncio
    async def test_no_agent_message_when_completing_then_no_response_is_raised(self):
        reasoning = '{"type":"item.completed","item":{"type":"reasoning","text":"x"}}'
        with pytest.raises(Exception, match="^No response from Codex$"):
            await _codex_complete(["not json", "", reasoning])

    @pytest.mark.asyncio
    async def test_two_agent_messages_when_completing_then_they_are_joined(self):
        answer = await _codex_complete([_AGENT_MESSAGE % "one", "garbage", _AGENT_MESSAGE % "two"])

        assert answer == "one\ntwo"

    @pytest.mark.asyncio
    async def test_stream_ends_without_turn_completed_then_last_answer_is_final(self):
        result = await _codex_stream(
            ["", "garbage", _AGENT_MESSAGE % "", _AGENT_MESSAGE % "answer"]
        )

        assert result == [("answer", False), ("answer", True)]

    @pytest.mark.asyncio
    async def test_codex_exits_nonzero_when_streaming_then_stderr_is_raised(self):
        with pytest.raises(
            Exception, match="^Codex streaming error: Codex CLI error: auth expired$"
        ):
            await _codex_stream([], returncode=1, stderr=b"auth expired\n")

    @pytest.mark.asyncio
    async def test_reconnect_notice_then_answer_when_streaming_then_answer_is_returned(self):
        result = await _codex_stream(
            [
                '{"type":"error","message":"Reconnecting... 2/5 (request timed out)"}',
                _AGENT_MESSAGE % "answer",
                '{"type":"turn.completed"}',
                _AGENT_MESSAGE % "after the turn",
            ]
        )

        assert result == [("answer", False), ("answer", True)]

    @pytest.mark.asyncio
    async def test_turn_failed_when_streaming_then_its_error_is_raised(self):
        with pytest.raises(Exception, match="^Codex streaming error: stream disconnected$"):
            await _codex_stream([_TURN_FAILED])

    @pytest.mark.asyncio
    async def test_streaming_analysis_when_spawning_codex_then_context_is_isolated(self):
        async def lines():
            yield b'{"type":"item.completed","item":{"type":"agent_message","text":"answer"}}\n'
            yield b'{"type":"turn.completed"}\n'

        proc = MagicMock(returncode=0)
        proc.stdout = lines()
        proc.stdin.drain = AsyncMock()
        spawn = AsyncMock(return_value=proc)
        with patch("recog.ai_provider.resolve_cli", return_value="/mock/codex"), \
             patch("asyncio.create_subprocess_exec", new=spawn):
            result = [item async for item in CodexCliProvider().stream("hello")]

        assert result == [("answer", False), ("answer", True)]
        cmd = spawn.call_args.args
        assert "--ignore-user-config" in cmd
        assert "skills.include_instructions=false" in cmd
        assert "project_doc_max_bytes=0" in cmd
        assert cmd[-2:] == ("--json", "-")


def test_codex_auto_given_command_when_built_then_no_reasoning_override():
    from recog.ai_provider import build_codex_exec_command
    command = build_codex_exec_command('/mock/codex', 'gpt-6-astra', '', isolated=True)
    assert not any('model_reasoning_effort=' in arg for arg in command)
    assert '--ignore-user-config' in command


class TestFrontendClaudeProvider:

    def test_no_model_override_when_creating_claude_provider_then_opus_5_5_is_used(self):
        from recog.web.ai_provider import ClaudeCliProvider

        with patch("recog.ai_provider.resolve_cli", return_value="/mock/claude"), \
             patch.dict(os.environ, {}, clear=True):
            assert ClaudeCliProvider().model == "claude-opus-5-5"

    @pytest.mark.asyncio
    async def test_complete_marks_prompt_for_claude_cli(self):
        from recog.web.ai_provider import ClaudeCliProvider

        created = {}

        class FakeProcess:
            returncode = 0

            async def communicate(self, input=None):
                created["stdin"] = input
                return b"answer\n", b""

        async def fake_create_subprocess_exec(*cmd, **kwargs):
            created["cmd"] = cmd
            return FakeProcess()

        with patch("recog.ai_provider.resolve_cli", return_value="/mock/claude"), \
             patch("asyncio.create_subprocess_exec", new=AsyncMock(side_effect=fake_create_subprocess_exec)):
            provider = ClaudeCliProvider(task="anchor")
            answer = await provider.complete("hello")

        assert answer == "answer"
        assert created["cmd"][:2] == ("/mock/claude", "-p")
        assert (
            created["stdin"].decode().startswith("<!-- ccs-automation:recog task=anchor -->\nhello")
        )
        assert not any("hello" in str(arg) for arg in created["cmd"])
        assert "--safe-mode" in created["cmd"]
        assert "--bare" not in created["cmd"]
        assert "--disable-slash-commands" in created["cmd"]

    @pytest.mark.asyncio
    async def test_streaming_analysis_when_spawning_claude_then_safe_mode_preserves_auth_and_disables_skills(self):
        from recog.web.ai_provider import ClaudeCliProvider

        created = {}

        written = bytearray()

        class FakeStdin:
            write = staticmethod(written.extend)
            drain = AsyncMock()
            close = staticmethod(lambda: None)

        class FakeProcess:
            returncode = 0
            stdin = FakeStdin()

            @property
            def stdout(self):
                async def lines():
                    yield b'{"type":"assistant","message":{"content":[{"type":"text","text":"answer"}]}}\n'
                    yield b'{"type":"result","result":"answer"}\n'
                return lines()

        async def spawn(*cmd, **kwargs):
            created["cmd"] = cmd
            return FakeProcess()

        with patch("recog.ai_provider.resolve_cli", return_value="/mock/claude"), \
             patch("asyncio.create_subprocess_exec", new=AsyncMock(side_effect=spawn)):
            result = [item async for item in ClaudeCliProvider().stream("hello")]

        assert result == [("answer", False), ("answer", True)]
        assert written.decode().endswith("hello")
        assert not any("hello" in str(arg) for arg in created["cmd"])
        assert "--safe-mode" in created["cmd"]
        assert "--bare" not in created["cmd"]
        assert "--disable-slash-commands" in created["cmd"]
        assert created["cmd"][created["cmd"].index("--output-format") + 1] == "stream-json"
