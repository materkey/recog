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

Ответь кратко (3-5 предложений) на вопрос:
Какой инкремент ожидается от {user_name} к следующему синку?

Выдели:
- Конкретные задачи
- Дедлайны (если упоминались)
- Ожидаемые результаты

Если конкретных задач для {user_name} не упоминалось, так и напиши.

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
    def name(self) -> str:
        """Provider name for display."""
        pass

    @abstractmethod
    async def complete(self, prompt: str) -> str:
        """Send prompt and get completion."""
        pass


class ClaudeCliProvider(AIProvider):
    """Claude CLI - uses Max/Pro subscription."""

    @property
    def name(self) -> str:
        return "claude-cli"

    async def complete(self, prompt: str) -> str:
        proc = await asyncio.create_subprocess_exec(
            "claude", "-p", prompt,
            "--output-format", "text",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await proc.communicate()

        if proc.returncode != 0:
            error_msg = stderr.decode().strip() if stderr else "Unknown error"
            raise Exception(f"Claude CLI error: {error_msg}")

        return stdout.decode().strip()


class CodexCliProvider(AIProvider):
    """Codex CLI - uses ChatGPT Pro subscription."""

    @property
    def name(self) -> str:
        return "codex-cli"

    async def complete(self, prompt: str) -> str:
        proc = await asyncio.create_subprocess_exec(
            "codex", "exec",
            "--prompt", prompt,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await proc.communicate()

        if proc.returncode != 0:
            error_msg = stderr.decode().strip() if stderr else "Unknown error"
            raise Exception(f"Codex CLI error: {error_msg}")

        return stdout.decode().strip()


class OpenRouterProvider(AIProvider):
    """OpenRouter API - pay-per-token."""

    def __init__(self):
        self.api_key = os.getenv("OPENROUTER_API_KEY")
        self.model = os.getenv("OPENROUTER_MODEL", "anthropic/claude-3-haiku")

        if not self.api_key:
            raise ValueError("OPENROUTER_API_KEY environment variable is required")

    @property
    def name(self) -> str:
        return f"openrouter/{self.model}"

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
