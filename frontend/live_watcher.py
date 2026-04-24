"""Live transcription file watcher for the current meeting.

Extracted from `server.py` so the rewrite-detection logic (D10) can be
unit-tested without importing the full FastAPI stack.

`MeetingStorage.update_segments` rewrites `transcript.jsonl` atomically
via `tmp.write_text(...) + os.replace(tmp, target)`. That swap changes
the inode and often shrinks size; a naive tailer holding the previous
`file_position` goes blind to everything after the rewrite. This module
detects rewrite (inode change or size shrink) and rewinds, while a
`_seen_segment_ids` watermark prevents re-broadcasting rows that were
already pushed to clients before the rewrite.
"""
from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
from typing import Any, Optional

# Keep the module importable in environments without `watchdog` (the CLI
# venv and test envs that only want the tailer logic). Falls back to a
# stub base class; at runtime in `server.py` the real watchdog is always
# present.
try:
    from watchdog.events import FileSystemEventHandler
except ImportError:  # pragma: no cover
    class FileSystemEventHandler:  # type: ignore[no-redef]
        pass


class LiveWatcher(FileSystemEventHandler):
    """Watch for live transcription updates (current meeting)."""

    def __init__(self):
        self.clients: set[Any] = set()
        self.current_meeting_id: Optional[str] = None
        self.file_position: int = 0
        # Watermark so we do not re-broadcast rows after an atomic rewrite
        # (`MeetingStorage.update_segments` -> `os.replace`). See D10.
        self._seen_segment_ids: set[int] = set()
        self._file_inode: Optional[int] = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None

    def set_loop(self, loop: asyncio.AbstractEventLoop):
        self._loop = loop

    def register(self, websocket: Any):
        self.clients.add(websocket)

    def unregister(self, websocket: Any):
        self.clients.discard(websocket)

    def set_current_meeting(self, meeting_id: Optional[str]):
        """Point the watcher at `meeting_id`, resetting watermark on change.

        Must be used instead of assigning `self.current_meeting_id`
        directly: on a genuine meeting switch we also have to clear
        `file_position`, `_file_inode`, and `_seen_segment_ids`, otherwise
        a watermark leftover from the previous meeting can silently drop
        new-meeting rows whose ids overlap (ids restart at 1 per meeting).
        Without this, a `live_stream` WS connect that races the watchdog's
        `_check_current_meeting` can pre-set the id and make the later
        watchdog call skip the reset.
        """
        if meeting_id != self.current_meeting_id:
            self.current_meeting_id = meeting_id
            self.file_position = 0
            self._seen_segment_ids = set()
            self._file_inode = None

    def seed_post_init_watermark(self, known_segment_ids):
        """Pre-populate the seen-id watermark with segment_ids already
        delivered in an `init` WS frame, so the first `on_modified` after
        a fresh `live_stream` connect does not re-broadcast rows that the
        webapp has already rendered.

        Called between `set_current_meeting` (which resets the watermark
        on meeting switch) and the init `send_text`. `file_position`
        stays at 0 on purpose: any segment appended between
        `storage.get_meeting` and this call is not in `known_segment_ids`
        and must still be picked up on the next read.
        """
        for sid in known_segment_ids:
            if isinstance(sid, int):
                self._seen_segment_ids.add(sid)

    def on_modified(self, event):
        if event.is_directory:
            return

        path = Path(event.src_path)

        # Watch index.json for new meetings
        if path.name == "index.json" and self._loop:
            asyncio.run_coroutine_threadsafe(
                self._check_current_meeting(),
                self._loop
            )

        # Watch transcript of current meeting
        if path.name == "transcript.jsonl" and self._loop:
            meeting_id = path.parent.name
            if meeting_id == self.current_meeting_id:
                asyncio.run_coroutine_threadsafe(
                    self._notify_new_segments(path),
                    self._loop
                )

    async def _check_current_meeting(self):
        """Check for new/changed current meeting."""
        try:
            # Lazy import to keep this module FastAPI-free for tests.
            from server import get_storage  # type: ignore

            storage = get_storage()
            active_meetings = storage.list_active_meetings()
            current = active_meetings[0] if active_meetings else None

            if current and current["id"] != self.current_meeting_id:
                self.set_current_meeting(current["id"])

                message = json.dumps({
                    "type": "meeting_change",
                    "meeting": current
                })
                await self._broadcast(message)
        except Exception as e:
            print(f"Error checking current meeting: {e}")

    async def _notify_new_segments(self, transcript_path: Path):
        """Read new lines and send to connected clients.

        Handles atomic rewrite: detects rewrite by inode change or size
        shrink, rewinds to 0, and filters already-broadcast ids via the
        seen-id watermark. Refined text itself is delivered via the
        bridge channel (`transcript_refine`), not here — this handler
        only keeps the tailer consistent for later appends.
        """
        if not transcript_path.exists():
            return

        try:
            st = os.stat(transcript_path)
            rewrite_detected = (
                (self._file_inode is not None and st.st_ino != self._file_inode)
                or st.st_size < self.file_position
            )
            if rewrite_detected:
                self.file_position = 0
            self._file_inode = st.st_ino

            with open(transcript_path, 'r', encoding='utf-8') as f:
                f.seek(self.file_position)
                new_lines = f.readlines()
                self.file_position = f.tell()

            if new_lines:
                segments = []
                for line in new_lines:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        seg = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    seg_id = seg.get("id")
                    if isinstance(seg_id, int) and seg_id in self._seen_segment_ids:
                        continue
                    segments.append(seg)
                    if isinstance(seg_id, int):
                        self._seen_segment_ids.add(seg_id)

                if segments:
                    message = json.dumps({"type": "segments", "data": segments})
                    await self._broadcast(message)
        except Exception as e:
            print(f"Error reading transcript: {e}")

    async def _broadcast(self, message: str):
        # Snapshot before iterating: concurrent `unregister` on another
        # task would mutate `self.clients` during the inner `await` and
        # raise `RuntimeError: Set changed size during iteration`.
        dead_clients = []
        for ws in list(self.clients):
            try:
                await ws.send_text(message)
            except Exception:
                dead_clients.append(ws)

        for ws in dead_clients:
            self.clients.discard(ws)
