"""
PromptRegistry — load and filter prompt definitions from .md files.

Each prompt is a .md file in a prompts/ directory with YAML frontmatter:

---
name: Display Name
filter: regex|null
---

Prompt body with {user_name}, {transcript}, {participants}, {meeting_title} placeholders.
"""

import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class PromptConfig:
    """A single prompt definition loaded from a .md file."""

    id: str  # filename without .md
    name: str  # display name from frontmatter
    filter: Optional[str]  # regex for meeting title, None = all meetings
    template: str  # prompt body with {variable} placeholders


def _parse_frontmatter(text: str) -> tuple[dict, str]:
    """Parse YAML frontmatter from markdown text.

    Returns (frontmatter_dict, body) where body is everything after
    the closing --- marker.  Handles the two required fields (name, filter)
    without a YAML library dependency.
    """
    if not text.startswith("---"):
        return {}, text

    # Find closing ---
    end_idx = text.index("---", 3) if "---" in text[3:] else -1
    if end_idx == -1:
        return {}, text

    # Adjust end_idx relative to full string (search started at pos 3)
    end_idx = text.index("---", 3)
    header = text[3:end_idx].strip()
    body = text[end_idx + 3:].strip()

    result: dict = {}
    for line in header.split("\n"):
        line = line.strip()
        if not line or ":" not in line:
            continue
        key, _, value = line.partition(":")
        key = key.strip()
        value = value.strip()
        # Handle null/None
        if value.lower() in ("null", "none", "~", ""):
            result[key] = None
        # Handle quoted strings
        elif (value.startswith('"') and value.endswith('"')) or \
             (value.startswith("'") and value.endswith("'")):
            result[key] = value[1:-1]
        else:
            result[key] = value

    return result, body


class PromptRegistry:
    """Load prompt definitions from a directory of .md files."""

    def __init__(self, prompts_dir: Path):
        self._dir = prompts_dir

    def load_all(self) -> list[PromptConfig]:
        """Load all .md files from prompts/, parse frontmatter."""
        if not self._dir.exists():
            return []

        prompts = []
        for path in sorted(self._dir.glob("*.md")):
            try:
                config = self._load_file(path)
                if config:
                    prompts.append(config)
            except Exception as e:
                print(
                    f"[prompt_registry] error loading {path.name}: {e}",
                    file=sys.stderr,
                )
        return prompts

    def get(self, prompt_id: str) -> Optional[PromptConfig]:
        """Load a single prompt by ID (filename without .md)."""
        path = self._dir / f"{prompt_id}.md"
        if not path.exists():
            return None
        try:
            return self._load_file(path)
        except Exception as e:
            print(
                f"[prompt_registry] error loading {path.name}: {e}",
                file=sys.stderr,
            )
            return None

    def matching(self, meeting_title: str) -> list[PromptConfig]:
        """Return prompts whose filter matches the title (or filter is null)."""
        result = []
        for prompt in self.load_all():
            if prompt.filter is None:
                result.append(prompt)
            else:
                try:
                    if re.search(prompt.filter, meeting_title, re.IGNORECASE):
                        result.append(prompt)
                except re.error as e:
                    print(
                        f"[prompt_registry] bad regex in {prompt.id}: {e}",
                        file=sys.stderr,
                    )
        return result

    def _load_file(self, path: Path) -> Optional[PromptConfig]:
        """Parse a single prompt .md file."""
        text = path.read_text(encoding="utf-8")
        frontmatter, body = _parse_frontmatter(text)

        name = frontmatter.get("name")
        if not name:
            print(
                f"[prompt_registry] {path.name}: missing 'name' in frontmatter",
                file=sys.stderr,
            )
            return None

        return PromptConfig(
            id=path.stem,
            name=name,
            filter=frontmatter.get("filter"),
            template=body,
        )
