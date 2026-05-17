"""
AI Provider base classes and CLI resolution for recog package.

Provides:
- AIProvider ABC
- CodexCliProvider (ChatGPT Pro via Codex CLI)
- resolve_cli() for finding CLI binaries under launchd
"""

import os
import shutil
import asyncio
from abc import ABC, abstractmethod
from pathlib import Path

from recog.node_env import get_node_env, version_key

CCS_AUTOMATION_MARKER_PREFIX = "<!-- ccs-automation:recog"


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
) -> list[str]:
    """Build a Codex CLI command for Recog automation calls."""
    cmd = [cli, "exec", "-m", model]
    if "spark" not in model and reasoning_effort:
        cmd += ["-c", f"model_reasoning_effort=\"{reasoning_effort}\""]
    if ephemeral:
        cmd.append("--ephemeral")
    cmd += ["--json", "-"]
    return cmd


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
    """Codex CLI - uses ChatGPT Pro subscription."""

    def __init__(self, task: str = "analysis"):
        self.model = os.getenv("CODEX_MODEL", "gpt-5.5")
        self.reasoning_effort = os.getenv("CODEX_REASONING_EFFORT", "xhigh")
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
        import json as json_module

        # Codex exec expects prompt via stdin when using "-"
        cmd = build_codex_exec_command(self._cli, self.model, self.reasoning_effort)
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
        for line in stdout.decode().strip().split('\n'):
            if not line:
                continue
            try:
                event = json_module.loads(line)
                if event.get("type") == "item.completed":
                    item = event.get("item", {})
                    if item.get("type") == "agent_message":
                        answer_parts.append(item.get("text", ""))
                elif event.get("type") == "error":
                    raise Exception(event.get("message", "Unknown error"))
            except json_module.JSONDecodeError:
                continue

        if not answer_parts:
            raise Exception("No response from Codex")

        return "\n".join(answer_parts)

    async def stream(self, prompt: str):
        """Stream Codex CLI response."""
        import json as json_module

        cmd = build_codex_exec_command(self._cli, self.model, self.reasoning_effort)
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
                line = line.decode().strip()
                if not line:
                    continue
                try:
                    event = json_module.loads(line)
                    if event.get("type") == "item.completed":
                        item = event.get("item", {})
                        if item.get("type") == "agent_message":
                            text = item.get("text", "")
                            if text:
                                full_text = text
                                yield text, False
                    elif event.get("type") == "turn.completed":
                        yield full_text, True
                        return
                    elif event.get("type") == "error":
                        raise Exception(event.get("message", "Unknown error"))
                except json_module.JSONDecodeError:
                    continue

            await proc.wait()
            if proc.returncode != 0:
                stderr = await proc.stderr.read()
                raise Exception(f"Codex CLI error: {stderr.decode().strip()}")

            if full_text:
                yield full_text, True
        except Exception as e:
            raise Exception(f"Codex streaming error: {str(e)}") from e
