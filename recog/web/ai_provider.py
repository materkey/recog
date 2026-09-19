"""Configured AI providers and prompt rendering for the web application."""

from recog.ai_provider import (
    AIProvider, ClaudeCliProvider, CodexCliProvider,
    provider_for_settings,
)
from recog.ai_defaults import (
    INCREMENT_PROMPT,
)
from recog.ai_settings import AiSettingsStore, render_prompt
from recog.config import get_data_dir


def get_task_settings(task: str = "analysis") -> dict:
    task_id = {"analysis": "analysis.increment", "increment": "analysis.increment"}.get(task, task)
    return AiSettingsStore(get_data_dir()).resolve_task(task_id)


def get_provider(task: str = "analysis", *, settings: dict | None = None) -> AIProvider:
    return provider_for_settings(settings or get_task_settings(task), task=task)


def get_all_providers(task: str = "analysis", *, settings: dict | None = None) -> list[AIProvider]:
    settings = settings or get_task_settings(task)
    providers = []
    for key in settings["providers"]:
        try:
            providers.append(provider_for_settings(settings, task=task, provider=key))
        except FileNotFoundError:
            # Default comparison uses the installed CLIs; an explicit scenario
            # selection must fail visibly instead of silently changing its set.
            if settings["customized"]:
                raise
    return providers


def build_prompt(question: str, transcript: str, user_name: str,
                 *, settings: dict | None = None,
                 participants: str = "", meeting_title: str = "") -> str:
    settings = settings or get_task_settings(question)
    template = settings["prompt"]
    result = render_prompt(template, {"transcript": transcript, "user_name": user_name,
                                     "participants": participants, "meeting_title": meeting_title})
    return result
