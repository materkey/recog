"""
AI Provider base classes and CLI resolution for recog package.

Provides:
- AIProvider ABC
- CodexCliProvider (ChatGPT Pro via Codex CLI)
- resolve_cli() for finding CLI binaries under launchd
"""

import os
import json
import shutil
import asyncio
from abc import ABC, abstractmethod
from pathlib import Path

from recog.node_env import get_node_env, version_key
from recog.ai_defaults import DEFAULT_MODELS

CCS_AUTOMATION_MARKER_PREFIX = "<!-- ccs-automation:recog"
DEFAULT_CODEX_MODEL = DEFAULT_MODELS["codex"]


def _sanitize_ccs_task(task: str) -> str:
    normalized = (task or "analysis").strip().lower()
    safe = "".join(
        ch if ch.isalnum() or ch in "._:-" else "-"
        for ch in normalized
    ).strip("-")
    return safe or "analysis"


def mark_ccs_automation(prompt: str, task: str = "analysis") -> str:
    """Mark a Recog-generated prompt so session search can classify it as automation."""
    if prompt.lstrip().startswith(CCS_AUTOMATION_MARKER_PREFIX):
        return prompt
    return f"{CCS_AUTOMATION_MARKER_PREFIX} task={_sanitize_ccs_task(task)} -->\n{prompt}"


def build_codex_exec_command(
    cli: str,
    model: str,
    reasoning_effort: str | None = None,
    *,
    ephemeral: bool = True,
    isolated: bool = False,
) -> list[str]:
    """Build a Codex CLI command for Recog automation calls."""
    cmd = [cli, "exec", "-m", model]
    if isolated:
        # Keep CODEX_HOME/auth while excluding customization from analysis.
        cmd.append("--ignore-user-config")
        for setting in (
            "skills.include_instructions=false",
            "skills.bundled.enabled=false",
            "features.plugins=false",
            "features.hooks=false",
            "features.memories=false",
            "features.skill_search=false",
            "project_doc_max_bytes=0",
        ):
            cmd += ["-c", setting]
    if "spark" not in model and reasoning_effort:
        cmd += ["-c", f"model_reasoning_effort=\"{reasoning_effort}\""]
    if ephemeral:
        cmd.append("--ephemeral")
    cmd += ["--json", "-"]
    return cmd


def _codex_turn_error(event: dict) -> str:
    """Message of a codex exec ``turn.failed`` event."""
    error = event.get("error") or {}
    return error.get("message") or "turn failed"


def _codex_event(line: str) -> tuple[str, str] | None:
    """Read one ``codex exec --json`` line.

    Returns ``("message", text)`` for an agent message, ``("done", "")`` for
    ``turn.completed`` and None for anything else; raises on ``turn.failed``.
    "error" events are not fatal: codex exec also emits its retry notices
    ("Reconnecting... 2/5") that way, and a failure that does end the turn
    arrives as ``turn.failed``.
    """
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        return None
    kind = event.get("type")
    if kind == "turn.failed":
        raise Exception(_codex_turn_error(event))
    if kind == "turn.completed":
        return "done", ""
    item = event.get("item", {}) if kind == "item.completed" else {}
    if item.get("type") == "agent_message":
        return "message", item.get("text", "")
    return None


def _nvm_version_key(path: str) -> tuple:
    """Extract semver tuple from nvm path for correct sorting (not lexicographic)."""
    return version_key(Path(path).parent.parent)


def resolve_cli(name: str) -> str:
    """Resolve full path to CLI binary (needed for launchd which has limited PATH)."""
    path = shutil.which(name)
    if path:
        return path
    # Fallback: check common locations
    import glob as glob_module
    candidates = [
        os.path.expanduser(f"~/.local/bin/{name}"),
        f"/opt/homebrew/bin/{name}",  # Apple Silicon Homebrew (not in launchd PATH)
        f"/usr/local/bin/{name}",
    ]
    # nvm-installed binaries (codex, etc.)
    nvm_globs = [
        os.path.expanduser(f"~/.local/share/nvm/*/bin/{name}"),
        os.path.expanduser(f"~/.nvm/versions/node/*/bin/{name}"),
    ]
    for pattern in nvm_globs:
        candidates.extend(
            sorted(glob_module.glob(pattern), key=_nvm_version_key, reverse=True)
        )
    for candidate in candidates:
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    raise FileNotFoundError(f"{name} not found in PATH, ~/.local/bin, or nvm")


class AIProvider(ABC):
    """Abstract base class for AI providers."""

    @property
    @abstractmethod
    def key(self) -> str:
        """Provider key for API responses (e.g., 'claude', 'codex')."""
        pass

    @property
    @abstractmethod
    def name(self) -> str:
        """Provider name for display (e.g., 'claude-cli (haiku)')."""
        pass

    @abstractmethod
    async def complete(self, prompt: str) -> str:
        """Send prompt and get completion."""
        pass

    async def stream(self, prompt: str):
        """Stream completion chunks. Yields (chunk_text, is_done) tuples."""
        # Default implementation: just yield the full result
        result = await self.complete(prompt)
        yield result, True


class CodexCliProvider(AIProvider):
    """Isolated automation via Codex CLI with existing subscription auth."""

    def __init__(self, task: str = "analysis", *, model: str | None = None, reasoning_effort: str | None = None):
        self.model = model if model is not None else os.getenv("CODEX_MODEL", DEFAULT_CODEX_MODEL)
        self.reasoning_effort = reasoning_effort if reasoning_effort is not None else os.getenv("CODEX_REASONING_EFFORT", "xhigh")
        self.task = task
        self._cli = resolve_cli("codex")

    @property
    def key(self) -> str:
        return "codex"

    @property
    def name(self) -> str:
        if "spark" in self.model:
            return f"Codex ({self.model})"
        return f"Codex ({self.model} {self.reasoning_effort})"

    async def complete(self, prompt: str) -> str:
        # Codex exec expects prompt via stdin when using "-"
        cmd = build_codex_exec_command(self._cli, self.model, self.reasoning_effort, isolated=True)
        marked_prompt = mark_ccs_automation(prompt, self.task)
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env()
        )
        stdout, stderr = await proc.communicate(input=marked_prompt.encode())

        if proc.returncode != 0:
            error_msg = stderr.decode().strip() if stderr else "Unknown error"
            raise Exception(f"Codex CLI error: {error_msg}")

        # Parse JSON lines output to find the agent message
        answer_parts = []
        for line in stdout.decode().splitlines():
            event = _codex_event(line)
            if event and event[0] == "message":
                answer_parts.append(event[1])

        if not answer_parts:
            raise Exception("No response from Codex")

        return "\n".join(answer_parts)

    async def stream(self, prompt: str):
        """Stream Codex CLI response."""
        cmd = build_codex_exec_command(self._cli, self.model, self.reasoning_effort, isolated=True)
        marked_prompt = mark_ccs_automation(prompt, self.task)
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env()
        )

        # Write prompt to stdin
        proc.stdin.write(marked_prompt.encode())
        await proc.stdin.drain()
        proc.stdin.close()

        full_text = ""
        try:
            async for line in proc.stdout:
                event = _codex_event(line.decode())
                if event is None:
                    continue
                kind, text = event
                if kind == "done":
                    yield full_text, True
                    return
                if text:
                    full_text = text
                    yield text, False

            await proc.wait()
            if proc.returncode != 0:
                stderr = await proc.stderr.read()
                raise Exception(f"Codex CLI error: {stderr.decode().strip()}")

            if full_text:
                yield full_text, True
        except Exception as e:
            raise Exception(f"Codex streaming error: {str(e)}") from e


class ClaudeCliProvider(AIProvider):
    """Claude CLI - keeps subscription auth while isolating analysis context."""

    def __init__(self, task: str = "analysis", *, model: str | None = None,
                 reasoning_effort: str | None = None):
        self.model = model if model is not None else os.getenv("CLAUDE_MODEL", DEFAULT_MODELS["claude"])
        self.reasoning_effort = reasoning_effort
        self.task = task
        self._cli = resolve_cli("claude")

    @property
    def key(self) -> str:
        return "claude"

    @property
    def name(self) -> str:
        effort = f" {self.reasoning_effort}" if self.reasoning_effort else ""
        return f"Claude ({self.model}{effort})"

    async def complete(self, prompt: str) -> str:
        marked_prompt = mark_ccs_automation(prompt, self.task)
        # On stdin: Linux limits one argv string to 128 KiB, under 70k Cyrillic characters.
        proc = await asyncio.create_subprocess_exec(
            self._cli,
            "-p",
            "--safe-mode",
            "--disable-slash-commands",
            "--model",
            self.model,
            *(["--effort", self.reasoning_effort] if self.reasoning_effort else []),
            "--output-format",
            "text",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env(),
        )
        stdout, stderr = await proc.communicate(input=marked_prompt.encode())

        if proc.returncode != 0:
            error_msg = stderr.decode().strip() if stderr else "Unknown error"
            raise Exception(f"Claude CLI error: {error_msg}")

        return stdout.decode().strip()

    async def stream(self, prompt: str):
        """Stream Claude CLI response."""
        import json as json_module

        marked_prompt = mark_ccs_automation(prompt, self.task)
        proc = await asyncio.create_subprocess_exec(
            self._cli,
            "-p",
            "--safe-mode",
            "--disable-slash-commands",
            "--model",
            self.model,
            *(["--effort", self.reasoning_effort] if self.reasoning_effort else []),
            "--output-format",
            "stream-json",
            "--verbose",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env(),
        )
        proc.stdin.write(marked_prompt.encode())
        await proc.stdin.drain()
        proc.stdin.close()

        full_text = ""
        try:
            async for line in proc.stdout:
                line = line.decode().strip()
                if not line:
                    continue
                try:
                    event = json_module.loads(line)
                    if event.get("type") == "assistant":
                        msg = event.get("message", {})
                        content = msg.get("content", [])
                        for block in content:
                            if block.get("type") == "text":
                                text = block.get("text", "")
                                if text:
                                    full_text = text  # Claude sends full text each time
                                    yield text, False
                    elif event.get("type") == "result":
                        result = event.get("result", full_text)
                        yield result, True
                        return
                except json_module.JSONDecodeError:
                    continue

            await proc.wait()
            if proc.returncode != 0:
                stderr = await proc.stderr.read()
                raise Exception(f"Claude CLI error: {stderr.decode().strip()}")

            # If we didn't get a result event, yield what we have
            if full_text:
                yield full_text, True
        except Exception as e:
            raise Exception(f"Claude streaming error: {str(e)}")


def create_provider(provider: str, *, task: str = "analysis", model: str | None = None,
                    reasoning_effort: str | None = None) -> AIProvider:
    """Create the explicitly selected backend without changing authentication."""
    if provider == "claude":
        return ClaudeCliProvider(task=task, model=model, reasoning_effort=reasoning_effort)
    if provider == "codex":
        return CodexCliProvider(task=task, model=model, reasoning_effort=reasoning_effort)
    raise ValueError(f"Unknown AI provider: {provider}")


def provider_for_settings(settings: dict, *, task: str, provider: str | None = None) -> AIProvider:
    key = provider or settings["providers"][0]
    return create_provider(key, task=task, model=settings["models"][key],
                           reasoning_effort=settings.get("claude_effort" if key == "claude" else "reasoning_effort"))
