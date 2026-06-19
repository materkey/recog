"""
Title generator for orphan live sessions using Codex CLI.

Generates short descriptive titles (3-7 words) based on transcript content.
"""

import asyncio
import json
import os
from typing import Optional

from recog.ai_provider import build_codex_exec_command, mark_ccs_automation, resolve_cli
from recog.node_env import get_node_env


TITLE_PROMPT = """Придумай короткое название (3-7 слов) для этого разговора.

Требования:
- Название должно отражать основную тему разговора
- Без кавычек, без слова "Разговор" в начале
- На русском языке
- Только название, без пояснений

Транскрипт:
{transcript}

Название:"""


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


async def generate_title_async(transcript: str, model: str = None) -> Optional[str]:
    """
    Generate a title for transcript using Codex CLI.

    Args:
        transcript: Meeting transcript text
        model: Codex model to use (default from CODEX_MODEL env or gpt-5.5)

    Returns:
        Generated title or None if generation failed
    """
    if not transcript or len(transcript.strip()) < 50:
        return None

    # Sample evenly across the whole meeting instead of only the head: a long
    # call's first lines are usual warm-up/small-talk, so head-only truncation
    # produced titles about the opening topic, not the meeting.
    transcript = _sample_transcript(transcript)

    model = model or os.getenv("CODEX_MODEL", "gpt-5.5")
    prompt = mark_ccs_automation(TITLE_PROMPT.format(transcript=transcript), "title")
    cmd = build_codex_exec_command(resolve_cli("codex"), model)

    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env()
        )
        # Codex CLI cold start + inference routinely runs ~30s (the increment
        # provider has no timeout and takes ~28s); 30s here intermittently
        # timed out -> 500 "Failed to generate title". Give it real headroom.
        stdout, stderr = await asyncio.wait_for(
            proc.communicate(input=prompt.encode()),
            timeout=90
        )

        if proc.returncode != 0:
            error_msg = stderr.decode().strip() if stderr else "Unknown error"
            print(f"Codex CLI error: {error_msg}")
            return None

        # Parse JSON lines output
        for line in stdout.decode().strip().split('\n'):
            if not line:
                continue
            try:
                event = json.loads(line)
                if event.get("type") == "item.completed":
                    item = event.get("item", {})
                    if item.get("type") == "agent_message":
                        title = item.get("text", "").strip()
                        # Clean up: remove quotes if present
                        title = title.strip('"\'')
                        # Limit length
                        if len(title) > 100:
                            title = title[:100]
                        return title if title else None
            except json.JSONDecodeError:
                continue

        return None

    except asyncio.TimeoutError:
        print("Codex CLI timeout")
        return None
    except FileNotFoundError:
        print("Codex CLI not found")
        return None
    except Exception as e:
        print(f"Title generation error: {e}")
        return None


def generate_title(transcript: str, model: str = None) -> Optional[str]:
    """
    Synchronous wrapper for generate_title_async.

    Args:
        transcript: Meeting transcript text
        model: Codex model to use

    Returns:
        Generated title or None if generation failed
    """
    try:
        return asyncio.run(generate_title_async(transcript, model))
    except RuntimeError:
        # Already in async context - create new event loop
        loop = asyncio.new_event_loop()
        try:
            return loop.run_until_complete(generate_title_async(transcript, model))
        finally:
            loop.close()
