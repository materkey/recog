"""
AI Provider abstraction for transcript analysis.

Supports:
- Claude CLI (uses Max/Pro subscription)
- Codex CLI (uses ChatGPT Pro subscription)
- OpenRouter API (pay-per-token)
"""

import os
import asyncio
from abc import ABC, abstractmethod
from typing import Optional


# Prompts
INCREMENT_PROMPT = """Проанализируй транскрипт рабочей встречи.

Участник: {user_name}
(это один и тот же человек — имя может отличаться из-за разных источников записи)

Ответь на вопрос: Какой инкремент ожидается от этого участника к следующему синку?

Формат ответа — markdown список:

## Задачи
- Задача 1
- Задача 2

## Дедлайны
- Дедлайн (если упоминался)

## Ожидаемые результаты
- Результат 1

Если конкретных задач не упоминалось, напиши "Конкретных задач не назначено".

Транскрипт:
{transcript}
"""

SUMMARY_PROMPT = """Проанализируй транскрипт рабочей встречи.

Дай краткое summary (5-7 предложений):
- Основные темы обсуждения
- Ключевые решения
- Action items с ответственными

Транскрипт:
{transcript}
"""


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


class ClaudeCliProvider(AIProvider):
    """Claude CLI - uses Max/Pro subscription."""

    def __init__(self):
        self.model = os.getenv("CLAUDE_MODEL", "haiku")

    @property
    def key(self) -> str:
        return "claude"

    @property
    def name(self) -> str:
        return f"Claude ({self.model})"

    async def complete(self, prompt: str) -> str:
        proc = await asyncio.create_subprocess_exec(
            "claude", "-p", prompt,
            "--model", self.model,
            "--output-format", "text",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await proc.communicate()

        if proc.returncode != 0:
            error_msg = stderr.decode().strip() if stderr else "Unknown error"
            raise Exception(f"Claude CLI error: {error_msg}")

        return stdout.decode().strip()

    async def stream(self, prompt: str):
        """Stream Claude CLI response."""
        import json as json_module

        proc = await asyncio.create_subprocess_exec(
            "claude", "-p", prompt,
            "--model", self.model,
            "--output-format", "stream-json",
            "--verbose",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )

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


class CodexCliProvider(AIProvider):
    """Codex CLI - uses ChatGPT Pro subscription."""

    def __init__(self):
        self.model = os.getenv("CODEX_MODEL", "gpt-5.2")

    @property
    def key(self) -> str:
        return "codex"

    @property
    def name(self) -> str:
        return f"Codex ({self.model})"

    async def complete(self, prompt: str) -> str:
        import json as json_module

        # Codex exec expects prompt via stdin when using "-"
        proc = await asyncio.create_subprocess_exec(
            "codex", "exec",
            "-m", self.model,
            "--json",
            "-",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await proc.communicate(input=prompt.encode())

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

        proc = await asyncio.create_subprocess_exec(
            "codex", "exec",
            "-m", self.model,
            "--json",
            "-",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )

        # Write prompt to stdin
        proc.stdin.write(prompt.encode())
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
            raise Exception(f"Codex streaming error: {str(e)}")


class OpenRouterProvider(AIProvider):
    """OpenRouter API - pay-per-token."""

    def __init__(self):
        self.api_key = os.getenv("OPENROUTER_API_KEY")
        self.model = os.getenv("OPENROUTER_MODEL", "anthropic/claude-3-haiku")

        if not self.api_key:
            raise ValueError("OPENROUTER_API_KEY environment variable is required")

    @property
    def key(self) -> str:
        return "openrouter"

    @property
    def name(self) -> str:
        return f"OpenRouter ({self.model})"

    async def complete(self, prompt: str) -> str:
        import httpx

        async with httpx.AsyncClient() as client:
            response = await client.post(
                "https://openrouter.ai/api/v1/chat/completions",
                headers={
                    "Authorization": f"Bearer {self.api_key}",
                    "HTTP-Referer": "http://localhost:8080",
                    "X-Title": "Recog Transcripts"
                },
                json={
                    "model": self.model,
                    "messages": [{"role": "user", "content": prompt}]
                },
                timeout=60.0
            )
            response.raise_for_status()
            data = response.json()
            return data["choices"][0]["message"]["content"]


def get_provider() -> AIProvider:
    """Get configured AI provider."""
    provider_name = os.getenv("AI_PROVIDER", "claude")

    if provider_name == "claude":
        return ClaudeCliProvider()
    elif provider_name == "codex":
        return CodexCliProvider()
    elif provider_name == "openrouter":
        return OpenRouterProvider()
    else:
        raise ValueError(f"Unknown AI provider: {provider_name}. Use: claude, codex, openrouter")


def get_all_providers() -> list[AIProvider]:
    """Get all CLI-based providers (Claude and Codex)."""
    return [ClaudeCliProvider(), CodexCliProvider()]


def build_prompt(
    question: str,
    transcript: str,
    user_name: str,
    custom_prompt: Optional[str] = None
) -> str:
    """Build prompt for AI analysis."""
    if question == "increment":
        return INCREMENT_PROMPT.format(user_name=user_name, transcript=transcript)
    elif question == "summary":
        return SUMMARY_PROMPT.format(transcript=transcript)
    elif question == "custom" and custom_prompt:
        return custom_prompt.format(user_name=user_name, transcript=transcript)
    else:
        raise ValueError(f"Unknown question type: {question}")
