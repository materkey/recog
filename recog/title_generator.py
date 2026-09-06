"""
Title generator using the configured AI provider and prompt.

Generates short descriptive titles (3-7 words) based on transcript content.
"""

import asyncio
from typing import Optional

from recog.ai_defaults import TITLE_PROMPT
from recog.ai_provider import provider_for_settings
from recog.ai_settings import AiSettingsStore, render_prompt
from recog.config import get_data_dir


def _sample_transcript(transcript: str, max_chars: int = 6000, blocks: int = 6) -> str:
    """Sample a long transcript into evenly-spaced blocks covering the whole
    meeting (beginning, middle, end), so the title reflects the overall content
    rather than just the opening lines.

    Short transcripts (<= max_chars) are returned unchanged. Sampling is done at
    line granularity to avoid cutting through "[ts] Speaker: text" lines.
    """
    if len(transcript) <= max_chars:
        return transcript

    lines = [ln for ln in transcript.split("\n") if ln.strip()]
    if not lines:
        return transcript[:max_chars]

    budget_per_block = max(1, max_chars // blocks)
    n = len(lines)
    out_blocks = []
    for b in range(blocks):
        start = (n * b) // blocks
        block, size, i = [], 0, start
        while i < n and size < budget_per_block:
            block.append(lines[i])
            size += len(lines[i]) + 1
            i += 1
        if block:
            out_blocks.append("\n".join(block))

    return "\n[...]\n".join(out_blocks)


async def generate_title_async(
    transcript: str, model: str = None, *, settings: dict = None, variables: dict = None,
) -> Optional[str]:
    """Generate a title from one resolved settings snapshot; failures return None.

    settings is a resolved title task from AiSettingsStore.resolve_task("title").
    The optional model overrides that task's selected provider for this call.
    """
    if not transcript or len(transcript.strip()) < 50:
        return None

    try:
        settings = settings if settings is not None else AiSettingsStore(get_data_dir()).resolve_task("title")
        if model is not None:
            settings = {**settings, "models": {**settings["models"], settings["providers"][0]: model}}
        # Sample the whole meeting: warm-up at the beginning often does not
        # reflect its main subject. Never let caller variables replace this text.
        prompt = render_prompt(settings["prompt"], {
            **(variables or {}), "transcript": _sample_transcript(transcript),
        })
        provider = provider_for_settings(settings, task="title")
        # CLI cold start + inference routinely takes ~30s. Keep enough headroom
        # while bounding every supported provider with the same timeout.
        result = await asyncio.wait_for(provider.complete(prompt), timeout=90)
        title = result.strip().strip('"\'')[:100]
        return title or None
    except asyncio.TimeoutError:
        print("Title generation timeout")
        return None
    except Exception as exc:
        print(f"Title generation error: {exc}")
        return None


def generate_title(
    transcript: str, model: str = None, *, settings: dict = None, variables: dict = None,
) -> Optional[str]:
    """Synchronous wrapper with the same settings snapshot and prompt variables."""
    try:
        return asyncio.run(generate_title_async(transcript, model, settings=settings, variables=variables))
    except RuntimeError:
        # Already in async context - create new event loop
        loop = asyncio.new_event_loop()
        try:
            return loop.run_until_complete(generate_title_async(transcript, model, settings=settings, variables=variables))
        finally:
            loop.close()
