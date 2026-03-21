"""
IncrementGenerator — auto-generate versioned AI task summaries on ktalk_leave.

Stores outputs in meetings/<id>/increments/v1.md, v2.md with YAML frontmatter.
Delta-generation: vN uses only segments after v(N-1), no duplication.
"""

import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional, Callable

from frontend.ai_provider import CodexCliProvider, INCREMENT_PROMPT


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

    def __init__(self, meetings_base: Path):
        self._meetings_base = meetings_base
        self._callback: Optional[Callable] = None

    def set_callback(self, callback: Callable):
        """Set callback invoked after successful generation: callback(meeting_id, version)."""
        self._callback = callback

    def _increments_dir(self, meeting_id: str) -> Path:
        return self._meetings_base / meeting_id / "increments"

    def _next_version(self, meeting_id: str) -> tuple[int, Optional[int]]:
        """Scan increments/v*_meta.json, return (next_version, last_segment_id).

        last_segment_id is the upper bound of segment_range from the latest
        *completed* version, or None if no completed versions exist.
        Version counter increments past failed versions to avoid reuse.
        """
        inc_dir = self._increments_dir(meeting_id)
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

    def _build_frontmatter(
        self, meta: dict, meeting_id: str, version: int,
        seg_start: int, seg_end: int, participants: list[str]
    ) -> str:
        title = (meta.get("title") or "Untitled meeting").replace('\\', '\\\\').replace('"', '\\"').replace('\n', ' ')
        date = meta.get("created_at", "")[:10]
        quoted_participants = ', '.join(f'"{p}"' for p in participants)
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

    async def generate(self, meeting_id: str, meeting_path: Path) -> Optional[int]:
        """Generate next increment version for a meeting.

        Returns version number on success, None if skipped (zero delta).
        Raises nothing — on Codex failure writes failed meta.
        """

        # Load meta
        meta_path = meeting_path / "meta.json"
        if not meta_path.exists():
            return None
        meta = json.loads(meta_path.read_text(encoding="utf-8"))

        # Load segments
        transcript_path = meeting_path / "transcript.jsonl"
        if not transcript_path.exists():
            return None
        segments = []
        content = transcript_path.read_text(encoding="utf-8").strip()
        if content:
            for line in content.split("\n"):
                if line.strip():
                    segments.append(json.loads(line))

        if not segments:
            return None

        # Determine version and delta range
        version, last_segment_id = self._next_version(meeting_id)

        # Filter to delta segments
        if last_segment_id is not None:
            delta_segments = [s for s in segments if s.get("id", 0) > last_segment_id]
        else:
            delta_segments = segments

        # Skip if zero delta
        if not delta_segments:
            print(f"[increment] v{version} skipped: 0 delta segments", file=sys.stderr)
            return None

        # Determine segment range
        seg_ids = [s.get("id", 0) for s in delta_segments]
        seg_start = min(seg_ids)
        seg_end = max(seg_ids)

        # Build transcript text (filters out event_type segments and empty text)
        transcript_text = _segments_to_transcript(delta_segments)
        if not transcript_text.strip():
            print(f"[increment] v{version} skipped: no text in delta segments", file=sys.stderr)
            return None

        # Determine user name
        local_names = _local_participant_names(meta)
        user_name = ", ".join(local_names) if local_names else "User"

        # All participant names
        participants = [
            info.get("name", pid)
            for pid, info in meta.get("participants", {}).items()
        ]

        # Build prompt
        prompt = INCREMENT_PROMPT.format(user_name=user_name, transcript=transcript_text)

        # Ensure increments directory exists
        inc_dir = self._increments_dir(meeting_id)
        inc_dir.mkdir(parents=True, exist_ok=True)

        # Generate
        t0 = time.monotonic()
        provider = CodexCliProvider()
        version_meta = {
            "version": version,
            "provider": provider.key,
            "model": provider.model,
            "reasoning_effort": provider.reasoning_effort,
            "segment_range": [seg_start, seg_end],
            "segment_count": len(delta_segments),
        }
        try:
            result = await provider.complete(prompt)
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
                f"[increment] v{version} generated ({len(delta_segments)} segments, "
                f"{duration:.1f}s)",
                file=sys.stderr,
            )

            if self._callback:
                self._callback(meeting_id, version)

            return version

        except Exception as exc:
            duration = time.monotonic() - t0
            # Write failed meta, no .md
            version_meta.update(
                generated_at=datetime.now(timezone.utc).isoformat(),
                status="failed",
                duration_seconds=round(duration, 1),
                error=str(exc),
            )
            self._save_json(inc_dir / f"v{version}_meta.json", version_meta)

            print(f"[increment] v{version} FAILED: {exc}", file=sys.stderr)
            return None

    def list_increments(self, meeting_id: str) -> list[dict]:
        """Return sorted list of increment meta dicts for a meeting."""
        inc_dir = self._increments_dir(meeting_id)
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

    def get_increment(self, meeting_id: str, version: int) -> Optional[dict]:
        """Return {meta, content} for a specific version, or None if not found."""
        inc_dir = self._increments_dir(meeting_id)
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
