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
        # Last source broadcast for the current meeting. Tracked so an
        # in-place orphan -> KTalk promotion (same meeting id, see
        # on_meeting_join case 3) can resend the refine flag.
        self.current_meeting_source: Optional[str] = None
        self.file_position: int = 0
        # Watermark so we do not re-broadcast rows after an atomic rewrite
        # (`MeetingStorage.update_segments` -> `os.replace`). See D10.
        self._seen_segment_ids: set[int] = set()
        self._file_inode: Optional[int] = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._meeting_read_lock = asyncio.Lock()

    def set_loop(self, loop: asyncio.AbstractEventLoop):
        self._loop = loop

    def register(self, websocket: Any):
        self.clients.add(websocket)

    def unregister(self, websocket: Any):
        self.clients.discard(websocket)

    def set_current_meeting(self, meeting_id: Optional[str], source: Optional[str] = None):
        """Point the watcher at `meeting_id`, resetting watermark on change.

        Must be used instead of assigning `self.current_meeting_id`
        directly: on a genuine meeting switch we also have to clear
        `file_position`, `_file_inode`, and `_seen_segment_ids`, otherwise
        a watermark leftover from the previous meeting can silently drop
        new-meeting rows whose ids overlap (ids restart at 1 per meeting).
        Without this, a `live_stream` WS connect that races the watchdog's
        `_check_current_meeting` can pre-set the id and make the later
        watchdog call skip the reset.

        `current_meeting_source` is seeded only on a genuine id change.
        A same-id reconnect must NOT overwrite it: otherwise a second
        `live_stream` connect landing between an in-place orphan -> KTalk
        promotion and the queued `_check_source_change` would clobber the
        tracked `orphan`, making that check see no change and suppress the
        `refine_state` broadcast to already-connected clients.
        """
        if meeting_id != self.current_meeting_id:
            self.current_meeting_id = meeting_id
            self.file_position = 0
            self._seen_segment_ids = set()
            self._file_inode = None
            self.current_meeting_source = source

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

        # Watch meta.json of current meeting for an in-place source flip
        # (empty orphan -> KTalk). on_meeting_join rewrites only meta.json
        # for that promotion, so index.json never fires _check_current_meeting.
        if path.name == "meta.json" and self._loop:
            meeting_id = path.parent.name
            if meeting_id == self.current_meeting_id:
                asyncio.run_coroutine_threadsafe(
                    self._check_source_change(meeting_id),
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

    async def read_active_meetings(self, storage):
        """Read off-loop, serializing selections from WS init and the watcher.

        Callers apply the selection before their next await, so an older scan
        cannot finish after a newer one and move the shared watcher backwards.
        """
        async with self._meeting_read_lock:
            return await asyncio.to_thread(storage.list_active_meetings)

    async def _check_current_meeting(self):
        """Check for new/changed current meeting."""
        try:
            # Lazy import to keep this module FastAPI-free for tests.
            from server import get_storage  # type: ignore

            storage = get_storage()
            active_meetings = await self.read_active_meetings(storage)
            current = active_meetings[0] if active_meetings else None

            if current and current["id"] != self.current_meeting_id:
                source = current.get("source")
                self.set_current_meeting(current["id"], source)

                message = json.dumps({
                    "type": "meeting_change",
                    "meeting": current,
                    # Keep refine availability in sync on meeting rotation:
                    # a KTalk call ending into an orphan (or an orphan
                    # promoted to a KTalk conference) must update whether the
                    # preliminary/tier-1 cue applies. Mirrors the init logic
                    # in server.live_stream.
                    "refine_enabled": source == "ktalk",
                })
                await self._broadcast(message)
        except Exception as e:
            print(f"Error checking current meeting: {e}")

    async def _check_source_change(self, meeting_id: str):
        """Resend the refine flag when the current meeting's source flips
        in place (empty orphan -> KTalk via on_meeting_join case 3).

        That promotion rewrites only the meeting's meta.json — not
        index.json — so the index-driven `_check_current_meeting` never
        observes it. We read just the small meta.json (not the heavy
        active-meeting list) since this fires on every meta write.
        """
        if meeting_id != self.current_meeting_id:
            return
        try:
            from server import get_storage  # type: ignore

            meta_path = get_storage().base_path / meeting_id / "meta.json"
            if not meta_path.exists():
                return
            source = json.loads(meta_path.read_text(encoding="utf-8")).get("source")
        except Exception as e:
            print(f"Error checking source change: {e}")
            return

        if source == self.current_meeting_source:
            return

        self.current_meeting_source = source
        # Resend ONLY the refine flag: a meeting_change here would wipe the
        # transcript the client has already rendered.
        message = json.dumps({
            "type": "refine_state",
            "meeting_id": meeting_id,
            "refine_enabled": source == "ktalk",
        })
        await self._broadcast(message)

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
