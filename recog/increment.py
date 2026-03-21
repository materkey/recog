"""
IncrementGenerator — auto-generate versioned AI task summaries on ktalk_leave.

Stores outputs in meetings/<id>/increments/<prompt_id>/v1.md, v2.md with YAML frontmatter.
Delta-generation: vN uses only segments after v(N-1), no duplication.
Multiple prompts run in parallel via generate_all().
"""

import asyncio
import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional, Callable

from frontend.ai_provider import CodexCliProvider
from recog.prompt_registry import PromptConfig, PromptRegistry


def _segments_to_transcript(segments: list[dict]) -> str:
    lines = []
    for seg in segments:
        if seg.get("event_type"):
            continue
        ts_display = seg.get("ts_display") or ""
        speaker = seg.get("speaker") or ""
        text = seg.get("text") or ""
        if not text:
            continue
        lines.append(f"[{ts_display}] {speaker}: {text}")
    return "\n".join(lines)


def _local_participant_names(meta: dict) -> list[str]:
    names = []
    for pid, info in meta.get("participants", {}).items():
        if info.get("is_local"):
            names.append(info.get("name", pid))
    return names


class IncrementGenerator:
    """Generate and store versioned AI increment summaries for meetings."""

    def __init__(self, meetings_base: Path, prompts_dir: Path = Path("prompts")):
        self._meetings_base = meetings_base
        self._prompts_dir = prompts_dir
        self._callback: Optional[Callable] = None

    def set_callback(self, callback: Callable):
        """Set callback invoked after successful generation: callback(meeting_id, version, prompt_id)."""
        self._callback = callback

    def migrate_flat_increments(self, meeting_path: Path, default_prompt_id: str = "my-increment"):
        """Move old flat increments/v*.md|json to increments/<default_prompt_id>/v*.md|json.

        Idempotent: skips if subdirectory already exists or no flat files found.
        """
        inc_dir = meeting_path / "increments"
        if not inc_dir.exists():
            return

        # Detect flat v*.md or v*_meta.json files directly in increments/
        flat_files = list(inc_dir.glob("v*.md")) + list(inc_dir.glob("v*_meta.json"))
        if not flat_files:
            return

        target_dir = inc_dir / default_prompt_id
        target_dir.mkdir(parents=True, exist_ok=True)

        for f in flat_files:
            dest = target_dir / f.name
            if not dest.exists():
                f.rename(dest)
            # If dest already exists, leave the flat file (shouldn't happen in practice)

    def _increments_dir(self, meeting_id: str, prompt_id: str) -> Path:
        return self._meetings_base / meeting_id / "increments" / prompt_id

    def _next_version(self, meeting_id: str, prompt_id: str) -> tuple[int, Optional[int]]:
        """Scan increments/<prompt_id>/v*_meta.json, return (next_version, last_segment_id).

        last_segment_id is the upper bound of segment_range from the latest
        *completed* version, or None if no completed versions exist.
        Version counter increments past failed versions to avoid reuse.
        """
        inc_dir = self._increments_dir(meeting_id, prompt_id)
        if not inc_dir.exists():
            return 1, None

        max_version = 0
        last_segment_id = None
        for meta_file in inc_dir.glob("v*_meta.json"):
            try:
                meta = json.loads(meta_file.read_text(encoding="utf-8"))
                v = meta.get("version", 0)
                if v > max_version:
                    max_version = v
                # Only use segment_range from completed versions for delta base,
                # so failed segments are retried in the next generation.
                if meta.get("status") == "completed":
                    sr = meta.get("segment_range")
                    if sr and len(sr) == 2 and (last_segment_id is None or sr[1] > last_segment_id):
                        last_segment_id = sr[1]
            except (json.JSONDecodeError, OSError):
                continue

        return max_version + 1, last_segment_id

    @staticmethod
    def _escape_yaml_str(s: str) -> str:
        return s.replace('\\', '\\\\').replace('"', '\\"').replace('\n', ' ')

    def _build_frontmatter(
        self, meta: dict, meeting_id: str, version: int,
        seg_start: int, seg_end: int, participants: list[str]
    ) -> str:
        title = self._escape_yaml_str(meta.get("title") or "Untitled meeting")
        date = meta.get("created_at", "")[:10]
        quoted_participants = ', '.join(
            f'"{self._escape_yaml_str(p)}"' for p in participants
        )
        return (
            "---\n"
            f'meeting: "{title}"\n'
            f"meeting_id: {meeting_id}\n"
            f'date: "{date}"\n'
            f"participants: [{quoted_participants}]\n"
            f"version: {version}\n"
            f"segment_range: [{seg_start}, {seg_end}]\n"
            "---\n\n"
        )

    async def generate(self, meeting_id: str, meeting_path: Path, prompt: PromptConfig) -> Optional[int]:
        """Generate next increment version for a meeting using the given prompt.

        Returns version number on success, None if skipped (zero delta).
        Raises nothing — on any failure writes failed meta.
        """
        prompt_id = prompt.id

        # File existence checks — legitimate skips, not failures
        meta_path = meeting_path / "meta.json"
        if not meta_path.exists():
            return None
        transcript_path = meeting_path / "transcript.jsonl"
        if not transcript_path.exists():
            return None

        # Determine version early so we can write failed meta on any error
        # (including malformed JSON in meta.json / transcript.jsonl).
        version, last_segment_id = self._next_version(meeting_id, prompt_id)
        inc_dir = self._increments_dir(meeting_id, prompt_id)

        t0 = time.monotonic()
        try:
            # Load meta
            meta = json.loads(meta_path.read_text(encoding="utf-8"))

            # Load segments
            segments = []
            content = transcript_path.read_text(encoding="utf-8").strip()
            if content:
                for line in content.split("\n"):
                    if line.strip():
                        segments.append(json.loads(line))

            if not segments:
                return None

            # Filter to delta segments
            if last_segment_id is not None:
                delta_segments = [s for s in segments if s.get("id", 0) > last_segment_id]
            else:
                delta_segments = segments

            # Skip if zero delta
            if not delta_segments:
                print(f"[increment] {prompt_id} v{version} skipped: 0 delta segments", file=sys.stderr)
                return None

            # Determine segment range
            seg_ids = [s.get("id", 0) for s in delta_segments]
            seg_start = min(seg_ids)
            seg_end = max(seg_ids)

            # Build transcript text (filters out event_type segments and empty text)
            transcript_text = _segments_to_transcript(delta_segments)
            if not transcript_text.strip():
                print(f"[increment] {prompt_id} v{version} skipped: no text in delta segments", file=sys.stderr)
                return None

            # Determine user name
            local_names = _local_participant_names(meta)
            user_name = ", ".join(local_names) if local_names else "User"

            # All participant names
            participants = [
                info.get("name", pid)
                for pid, info in meta.get("participants", {}).items()
            ]

            # Build prompt from template (use replace instead of str.format
            # to avoid KeyError on literal curly braces in transcript text)
            rendered_prompt = (
                prompt.template
                .replace("{user_name}", user_name)
                .replace("{transcript}", transcript_text)
                .replace("{participants}", ", ".join(participants))
                .replace("{meeting_title}", meta.get("title", ""))
            )

            # Ensure increments directory exists
            inc_dir.mkdir(parents=True, exist_ok=True)

            # Generate
            provider = CodexCliProvider()
            version_meta = {
                "version": version,
                "prompt_id": prompt_id,
                "prompt_name": prompt.name,
                "provider": provider.key,
                "model": provider.model,
                "reasoning_effort": provider.reasoning_effort,
                "segment_range": [seg_start, seg_end],
                "segment_count": len(delta_segments),
            }

            result = await provider.complete(rendered_prompt)
            duration = time.monotonic() - t0

            # Build frontmatter + content
            frontmatter = self._build_frontmatter(
                meta, meeting_id, version, seg_start, seg_end, participants
            )
            md_content = frontmatter + result

            # Write vN.md
            (inc_dir / f"v{version}.md").write_text(md_content, encoding="utf-8")

            # Write vN_meta.json
            version_meta.update(
                generated_at=datetime.now(timezone.utc).isoformat(),
                status="completed",
                duration_seconds=round(duration, 1),
                error=None,
            )
            self._save_json(inc_dir / f"v{version}_meta.json", version_meta)

            print(
                f"[increment] {prompt_id} v{version} generated ({len(delta_segments)} segments, "
                f"{duration:.1f}s)",
                file=sys.stderr,
            )

            if self._callback:
                self._callback(meeting_id, version, prompt_id)

            return version

        except Exception as exc:
            duration = time.monotonic() - t0
            # Write failed meta, no .md
            inc_dir.mkdir(parents=True, exist_ok=True)
            version_meta = {
                "version": version,
                "prompt_id": prompt_id,
                "prompt_name": prompt.name,
                "generated_at": datetime.now(timezone.utc).isoformat(),
                "status": "failed",
                "duration_seconds": round(duration, 1),
                "error": str(exc),
            }
            self._save_json(inc_dir / f"v{version}_meta.json", version_meta)

            print(f"[increment] {prompt_id} v{version} FAILED: {exc}", file=sys.stderr)
            return None

    async def generate_all(self, meeting_id: str, meeting_path: Path):
        """Load matching prompts, run generate() for each in parallel."""
        # Migrate old flat increments layout before generating
        self.migrate_flat_increments(meeting_path)

        registry = PromptRegistry(self._prompts_dir)

        meta_path = meeting_path / "meta.json"
        if not meta_path.exists():
            return

        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        title = meta.get("title", "")
        prompts = registry.matching(title)

        if not prompts:
            print("[increment] no matching prompts found", file=sys.stderr)
            return

        tasks = [self.generate(meeting_id, meeting_path, p) for p in prompts]
        await asyncio.gather(*tasks, return_exceptions=True)

    def list_all_increments(self, meeting_id: str) -> dict[str, list[dict]]:
        """Return {prompt_id: [metas]} for all prompts that have increments."""
        meeting_path = self._meetings_base / meeting_id
        # Migrate old flat layout so reads see them in subdirectories
        self.migrate_flat_increments(meeting_path)

        base = meeting_path / "increments"
        if not base.exists():
            return {}

        result: dict[str, list[dict]] = {}
        for subdir in sorted(base.iterdir()):
            if not subdir.is_dir():
                continue
            prompt_id = subdir.name
            metas = self.list_increments(meeting_id, prompt_id)
            if metas:
                result[prompt_id] = metas
        return result

    def list_increments(self, meeting_id: str, prompt_id: str) -> list[dict]:
        """Return sorted list of increment meta dicts for a meeting prompt."""
        inc_dir = self._increments_dir(meeting_id, prompt_id)
        if not inc_dir.exists():
            return []

        result = []
        for meta_file in inc_dir.glob("v*_meta.json"):
            try:
                meta = json.loads(meta_file.read_text(encoding="utf-8"))
                result.append(meta)
            except (json.JSONDecodeError, OSError):
                continue

        result.sort(key=lambda m: m.get("version", 0))
        return result

    def get_increment(self, meeting_id: str, prompt_id: str, version: int) -> Optional[dict]:
        """Return {meta, content} for a specific version, or None if not found."""
        inc_dir = self._increments_dir(meeting_id, prompt_id)
        meta_path = inc_dir / f"v{version}_meta.json"
        if not meta_path.exists():
            return None

        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return None

        md_path = inc_dir / f"v{version}.md"
        content = None
        if md_path.exists():
            content = md_path.read_text(encoding="utf-8")

        return {"meta": meta, "content": content}

    def _save_json(self, path: Path, data: dict):
        path.write_text(
            json.dumps(data, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
