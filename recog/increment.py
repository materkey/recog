"""
IncrementGenerator — auto-generate versioned AI task summaries on ktalk_leave.

Stores outputs in meetings/<id>/increments/<prompt_id>/v1.md, v2.md with YAML frontmatter.
Delta-generation: vN uses only segments after v(N-1), no duplication.
Multiple prompts run in parallel via generate_all().
"""

import asyncio
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional, Callable

from recog.ai_provider import CodexCliProvider
from recog.prompt_registry import PromptConfig, PromptRegistry

STALE_GENERATING_TIMEOUT_SECONDS = 600  # 10 minutes


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


def _version_from_filename(name: str) -> Optional[int]:
    """Extract version number from v{N}_meta.json filename."""
    if name.startswith("v") and name.endswith("_meta.json"):
        try:
            return int(name[1:-len("_meta.json")])
        except ValueError:
            pass
    return None


class IncrementGenerator:
    """Generate and store versioned AI increment summaries for meetings."""

    # Class-level lock dict shared across instances to prevent concurrent
    # version conflicts even when separate IncrementGenerator instances
    # are created (e.g. server endpoint vs ktalk_bridge).
    _locks: dict[tuple[str, str], asyncio.Lock] = {}

    def __init__(self, meetings_base: Path, prompts_dir: Path = Path("prompts")):
        self._meetings_base = meetings_base
        self._prompts_dir = prompts_dir
        self._callback: Optional[Callable] = None

    def set_callback(self, callback: Callable):
        """Set callback invoked after successful generation: callback(meeting_id, version, prompt_id)."""
        self._callback = callback

    @classmethod
    def _get_lock(cls, meeting_id: str, prompt_id: str) -> asyncio.Lock:
        key = (meeting_id, prompt_id)
        if key not in cls._locks:
            cls._locks[key] = asyncio.Lock()
        return cls._locks[key]

    def _claim_version(
        self, meeting_id: str, prompt_id: str, all_segments: list[dict]
    ) -> Optional[tuple[int, list[dict], int, int]]:
        """Atomically claim the next version number with segment_range in placeholder.

        Computes delta segments from all_segments and includes segment_range in
        the 'generating' placeholder from the start, preventing cross-process
        duplicate delta windows.  Uses O_CREAT|O_EXCL to prevent version collisions.

        Returns (version, delta_segments, seg_start, seg_end) or None if no delta.
        Raises RuntimeError if version claim fails after retries.
        """
        inc_dir = self._increments_dir(meeting_id, prompt_id)
        inc_dir.mkdir(parents=True, exist_ok=True)

        for _ in range(5):
            version, last_segment_id = self._next_version(meeting_id, prompt_id)

            # Compute delta for this version's base
            if last_segment_id is not None:
                delta_segments = [s for s in all_segments if s.get("id", 0) > last_segment_id]
            else:
                delta_segments = all_segments

            if not delta_segments:
                return None

            seg_ids = [s.get("id", 0) for s in delta_segments]
            seg_start = min(seg_ids)
            seg_end = max(seg_ids)

            meta_path = inc_dir / f"v{version}_meta.json"
            placeholder = json.dumps({
                "version": version,
                "prompt_id": prompt_id,
                "status": "generating",
                "generated_at": datetime.now(timezone.utc).isoformat(),
                "segment_range": [seg_start, seg_end],
            }, ensure_ascii=False, indent=2) + "\n"

            try:
                fd = os.open(str(meta_path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
                try:
                    os.write(fd, placeholder.encode("utf-8"))
                finally:
                    os.close(fd)
                return version, delta_segments, seg_start, seg_end
            except FileExistsError:
                continue

        raise RuntimeError(
            f"Could not claim version for {prompt_id} after retries"
        )

    async def _heartbeat(self, meta_path: Path, interval: float = 120.0):
        """Periodically update generated_at to prevent stale detection during long generations."""
        try:
            while True:
                await asyncio.sleep(interval)
                try:
                    data = json.loads(meta_path.read_text(encoding="utf-8"))
                    if data.get("status") == "generating":
                        data["generated_at"] = datetime.now(timezone.utc).isoformat()
                        self._save_json(meta_path, data)
                except (json.JSONDecodeError, OSError):
                    pass
        except asyncio.CancelledError:
            pass

    def _remove_generating_meta(self, inc_dir: Path, version: int):
        """Remove a 'generating' placeholder meta for a skipped version."""
        meta_path = inc_dir / f"v{version}_meta.json"
        try:
            data = json.loads(meta_path.read_text(encoding="utf-8"))
            if data.get("status") == "generating":
                meta_path.unlink()
        except (OSError, json.JSONDecodeError):
            pass

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
        *completed* or *in-progress* version, or None if no such versions exist.
        Version counter increments past failed versions to avoid reuse.
        Stale 'generating' placeholders (older than STALE_GENERATING_TIMEOUT_SECONDS)
        are automatically marked as failed so their segments can be retried.
        """
        inc_dir = self._increments_dir(meeting_id, prompt_id)
        if not inc_dir.exists():
            return 1, None

        max_version = 0
        last_segment_id = None
        now = datetime.now(timezone.utc)
        for meta_file in inc_dir.glob("v*_meta.json"):
            try:
                meta = json.loads(meta_file.read_text(encoding="utf-8"))
                v = meta.get("version", 0)
                if v > max_version:
                    max_version = v

                status = meta.get("status")

                # Detect stale "generating" placeholders (crash recovery).
                # If older than threshold, mark as failed so segments are retried.
                if status == "generating":
                    generated_at_str = meta.get("generated_at", "")
                    try:
                        ts = datetime.fromisoformat(generated_at_str)
                        age = (now - ts).total_seconds()
                        if age > STALE_GENERATING_TIMEOUT_SECONDS:
                            meta["status"] = "failed"
                            meta["error"] = "stale generating placeholder (process crash recovery)"
                            self._save_json(meta_file, meta)
                            status = "failed"
                    except (ValueError, TypeError):
                        pass

                # Use segment_range from completed and in-progress versions:
                # completed results are final; in-progress ranges prevent
                # cross-process duplicate content.  Failed versions lose
                # their segment_range, so those segments are retried.
                if status in ("completed", "generating"):
                    sr = meta.get("segment_range")
                    if sr and len(sr) == 2 and (last_segment_id is None or sr[1] > last_segment_id):
                        last_segment_id = sr[1]
            except (json.JSONDecodeError, OSError):
                # File may be in the process of being written (_claim_version
                # creates file with O_CREAT|O_EXCL, then writes JSON).
                # Retry once after a brief pause before falling back to
                # filename-only version extraction.
                time.sleep(0.05)
                try:
                    meta = json.loads(meta_file.read_text(encoding="utf-8"))
                    v = meta.get("version", 0)
                    if v > max_version:
                        max_version = v
                    status = meta.get("status")
                    if status in ("completed", "generating"):
                        sr = meta.get("segment_range")
                        if sr and len(sr) == 2 and (last_segment_id is None or sr[1] > last_segment_id):
                            last_segment_id = sr[1]
                except (json.JSONDecodeError, OSError):
                    # Still account for the version from the filename to prevent
                    # _claim_version from retrying an already-existing file.
                    v = _version_from_filename(meta_file.name)
                    if v is not None and v > max_version:
                        max_version = v
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

        Returns version number on success, None if skipped (zero delta
        or missing files).
        Raises RuntimeError if version claim fails (filesystem collision).
        Raises ValueError/OSError if transcript exists but cannot be parsed.
        Failures after version claim write failed meta and return None.
        """
        prompt_id = prompt.id

        # File existence checks — legitimate skips, not failures
        meta_path = meeting_path / "meta.json"
        if not meta_path.exists():
            return None
        transcript_path = meeting_path / "transcript.jsonl"
        if not transcript_path.exists():
            return None

        # Load segments before acquiring lock (needed for _claim_version
        # to compute delta and include segment_range in the placeholder).
        # Errors (malformed JSON, I/O failures) propagate to the caller so
        # they surface as real failures instead of being confused with
        # legitimate skips (missing files, zero delta).
        segments = []
        content = transcript_path.read_text(encoding="utf-8").strip()
        if content:
            for i, line in enumerate(content.split("\n"), 1):
                if line.strip():
                    try:
                        segments.append(json.loads(line))
                    except json.JSONDecodeError as exc:
                        raise ValueError(
                            f"Malformed JSONL in {transcript_path.name} line {i}: {exc}"
                        ) from exc

        if not segments:
            return None

        # Serialize concurrent generate() calls for the same (meeting_id, prompt_id)
        # to prevent two coroutines from selecting the same version number.
        async with self._get_lock(meeting_id, prompt_id):
            # Atomically claim version on the filesystem with segment_range
            # in the placeholder from the start, preventing cross-process
            # duplicate delta windows.
            claim = self._claim_version(meeting_id, prompt_id, segments)
            if claim is None:
                print(f"[increment] {prompt_id} skipped: 0 delta segments", file=sys.stderr)
                return None

            version, delta_segments, seg_start, seg_end = claim
            inc_dir = self._increments_dir(meeting_id, prompt_id)

            t0 = time.monotonic()
            try:
                # Load meta
                meta = json.loads(meta_path.read_text(encoding="utf-8"))

                # Build transcript text (filters out event_type segments and empty text)
                transcript_text = _segments_to_transcript(delta_segments)
                if not transcript_text.strip():
                    print(f"[increment] {prompt_id} v{version} skipped: no text in delta segments", file=sys.stderr)
                    self._remove_generating_meta(inc_dir, version)
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
                    # `title` is stored as null for untitled meetings, so the
                    # get-default ("") does not apply — guard the None explicitly.
                    .replace("{meeting_title}", meta.get("title") or "")
                )

                # Ensure increments directory exists
                inc_dir.mkdir(parents=True, exist_ok=True)

                # Generate
                provider = CodexCliProvider(task="increment")
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

                # Heartbeat keeps generated_at fresh to prevent stale
                # detection by other processes during long API calls.
                heartbeat_meta_path = inc_dir / f"v{version}_meta.json"
                heartbeat_task = asyncio.create_task(
                    self._heartbeat(heartbeat_meta_path)
                )
                try:
                    result = await provider.complete(rendered_prompt)
                finally:
                    heartbeat_task.cancel()
                    try:
                        await heartbeat_task
                    except asyncio.CancelledError:
                        pass
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
            except BaseException as exc:
                # CancelledError (BaseException in Python 3.9+), SystemExit,
                # KeyboardInterrupt — write failed meta to prevent stale
                # "generating" placeholder, then re-raise.
                duration = time.monotonic() - t0
                inc_dir.mkdir(parents=True, exist_ok=True)
                version_meta = {
                    "version": version,
                    "prompt_id": prompt_id,
                    "prompt_name": prompt.name,
                    "generated_at": datetime.now(timezone.utc).isoformat(),
                    "status": "failed",
                    "duration_seconds": round(duration, 1),
                    "error": f"cancelled: {exc}",
                }
                self._save_json(inc_dir / f"v{version}_meta.json", version_meta)
                print(f"[increment] {prompt_id} v{version} CANCELLED: {exc}", file=sys.stderr)
                raise
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
        title = meta.get("title") or ""
        prompts = registry.matching(title)

        if not prompts:
            print("[increment] no matching prompts found", file=sys.stderr)
            return

        tasks = [self.generate(meeting_id, meeting_path, p) for p in prompts]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        for i, result in enumerate(results):
            if isinstance(result, BaseException) and not isinstance(result, Exception):
                # CancelledError, KeyboardInterrupt, SystemExit — must propagate
                raise result
            if isinstance(result, Exception):
                print(
                    f"[increment] {prompts[i].id} unexpected error: {result}",
                    file=sys.stderr,
                )

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
