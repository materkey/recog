"""
AI Provider abstraction for transcript analysis.

Supports:
- Claude CLI (uses Max/Pro subscription)
- Codex CLI (uses ChatGPT Pro subscription)
- OpenRouter API (pay-per-token)
"""

import os
import sys
import asyncio
from pathlib import Path
from typing import Optional

from recog.node_env import get_node_env
from recog.ai_provider import AIProvider, CodexCliProvider, mark_ccs_automation, resolve_cli


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

ANCHOR_PROMPT = """Ты — помощник для быстрого восстановления контекста встречи.

Участник: {user_name}
(это один и тот же человек — имя может отличаться из-за разных источников записи)

Вход: транскрипт последних минут. Формат строк:
[HH:MM:SS] Speaker: text

Твоя задача:
1) current_topic: 1 короткая фраза, что обсуждают прямо сейчас.
2) topic_tone: одно из "neutral" | "calm" | "heated".
3) summary_bullets: 3 пункта (каждый до 10 слов) + ts_display из транскрипта.
4) user_mentioned: true/false если обращались к участнику или назначали задачи.
5) mention_context: короткий контекст (если user_mentioned=true).
6) mention_ts_display: ts_display (если user_mentioned=true).

Если данных недостаточно — не выдумывай, заполни безопасными значениями.

Верни строго валидный JSON без markdown и комментариев:
{{
  "current_topic": "...",
  "topic_tone": "neutral|calm|heated",
  "summary_bullets": [
    {{"text": "...", "ts_display": "HH:MM:SS"}}
  ],
  "user_mentioned": false,
  "mention_context": "",
  "mention_ts_display": ""
}}

Транскрипт:
{transcript}
"""


class ClaudeCliProvider(AIProvider):
    """Claude CLI - uses Max/Pro subscription."""

    def __init__(self, task: str = "analysis"):
        self.model = os.getenv("CLAUDE_MODEL", "claude-opus-4-6")
        self.task = task
        self._cli = resolve_cli("claude")

    @property
    def key(self) -> str:
        return "claude"

    @property
    def name(self) -> str:
        return f"Claude ({self.model})"

    async def complete(self, prompt: str) -> str:
        marked_prompt = mark_ccs_automation(prompt, self.task)
        proc = await asyncio.create_subprocess_exec(
            self._cli, "-p", marked_prompt,
            "--model", self.model,
            "--output-format", "text",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env()
        )
        stdout, stderr = await proc.communicate()

        if proc.returncode != 0:
            error_msg = stderr.decode().strip() if stderr else "Unknown error"
            raise Exception(f"Claude CLI error: {error_msg}")

        return stdout.decode().strip()

    async def stream(self, prompt: str):
        """Stream Claude CLI response."""
        import json as json_module

        marked_prompt = mark_ccs_automation(prompt, self.task)
        proc = await asyncio.create_subprocess_exec(
            self._cli, "-p", marked_prompt,
            "--model", self.model,
            "--output-format", "stream-json",
            "--verbose",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env()
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


def get_provider(task: str = "analysis") -> AIProvider:
    """Get configured AI provider."""
    provider_name = os.getenv("AI_PROVIDER", "claude")

    if provider_name == "claude":
        return ClaudeCliProvider(task=task)
    elif provider_name == "codex":
        return CodexCliProvider(task=task)
    elif provider_name == "openrouter":
        return OpenRouterProvider()
    else:
        raise ValueError(f"Unknown AI provider: {provider_name}. Use: claude, codex, openrouter")


def get_all_providers(task: str = "analysis") -> list[AIProvider]:
    """Get all CLI-based providers that are installed (Claude and/or Codex)."""
    providers: list[AIProvider] = []
    for cls in (ClaudeCliProvider, CodexCliProvider):
        try:
            providers.append(cls(task=task))
        except FileNotFoundError:
            pass
    return providers


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
    elif question == "anchor":
        return ANCHOR_PROMPT.format(user_name=user_name, transcript=transcript)
    elif question == "custom" and custom_prompt:
        return custom_prompt.format(user_name=user_name, transcript=transcript)
    else:
        raise ValueError(f"Unknown question type: {question}")
