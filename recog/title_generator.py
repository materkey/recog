"""
Title generator for orphan live sessions using Codex CLI.

Generates short descriptive titles (3-7 words) based on transcript content.
"""

import asyncio
import json
import os
from typing import Optional


TITLE_PROMPT = """Придумай короткое название (3-7 слов) для этого разговора.

Требования:
- Название должно отражать основную тему разговора
- Без кавычек, без слова "Разговор" в начале
- На русском языке
- Только название, без пояснений

Транскрипт:
{transcript}

Название:"""


async def generate_title_async(transcript: str, model: str = None) -> Optional[str]:
    """
    Generate a title for transcript using Codex CLI.

    Args:
        transcript: Meeting transcript text
        model: Codex model to use (default from CODEX_MODEL env or gpt-5.2)

    Returns:
        Generated title or None if generation failed
    """
    if not transcript or len(transcript.strip()) < 50:
        return None

    # Truncate very long transcripts (keep first ~2000 chars for title generation)
    if len(transcript) > 2000:
        transcript = transcript[:2000] + "\n..."

    model = model or os.getenv("CODEX_MODEL", "gpt-5.2")
    prompt = TITLE_PROMPT.format(transcript=transcript)

    try:
        proc = await asyncio.create_subprocess_exec(
            "codex", "exec",
            "-m", model,
            "--json",
            "-",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await asyncio.wait_for(
            proc.communicate(input=prompt.encode()),
            timeout=30
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
