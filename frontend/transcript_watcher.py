"""Per-meeting transcript watcher for the meeting-watch WebSocket endpoint.

Extracted from `server.py` so the rewrite-detection logic (D10 parity)
can be unit-tested without importing the full FastAPI stack.

Mirrors the LiveWatcher rewrite semantics: `MeetingStorage.update_segments`
rewrites `transcript.jsonl` atomically via
`tmp.write_text(...) + os.replace(tmp, target)`. Without inode / size-shrink
detection and a seen-id watermark, this tailer would silently stall after
the first refine or retroactive correction.
"""
from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path
from typing import Any, Optional

try:
    from watchdog.events import FileSystemEventHandler
except ImportError:  # pragma: no cover
    class FileSystemEventHandler:  # type: ignore[no-redef]
        pass


class TranscriptWatcher(FileSystemEventHandler):
    """Watch transcript files for changes and notify WebSocket clients."""

    def __init__(self):
        self.clients: dict[str, set[Any]] = {}  # meeting_id -> set of websockets
        self.file_positions: dict[str, int] = {}  # file_path -> last read position
        # Mirror of D10 rewrite handling from LiveWatcher: refine /
        # retroactive-correction rewrites JSONL via os.replace, which
        # changes the inode and usually shrinks the file. Without this
        # detection the tailer goes blind to everything written after the
        # rewrite.
        self.file_inodes: dict[str, int] = {}
        self.seen_segment_ids: dict[str, set[int]] = {}
        self._loop: Optional[asyncio.AbstractEventLoop] = None

    def set_loop(self, loop: asyncio.AbstractEventLoop):
        self._loop = loop

    def register(self, meeting_id: str, websocket: Any):
        if meeting_id not in self.clients:
            self.clients[meeting_id] = set()
        self.clients[meeting_id].add(websocket)

    def unregister(self, meeting_id: str, websocket: Any):
        if meeting_id in self.clients:
            self.clients[meeting_id].discard(websocket)
            if not self.clients[meeting_id]:
                del self.clients[meeting_id]
                # Drop per-file tail state when no clients remain for this
                # meeting — otherwise `file_positions`, `file_inodes`, and
                # `seen_segment_ids` grow unbounded across months of
                # meetings (entries keyed by full transcript path).
                suffix = os.path.join(meeting_id, "transcript.jsonl")
                dead_keys = [k for k in self.file_positions if k.endswith(suffix)]
                for k in dead_keys:
                    self.file_positions.pop(k, None)
                    self.file_inodes.pop(k, None)
                    self.seen_segment_ids.pop(k, None)

    def on_modified(self, event):
        if event.is_directory:
            return

        path = Path(event.src_path)
        if path.name == "transcript.jsonl":
            meeting_id = path.parent.name
            if meeting_id in self.clients and self._loop:
                asyncio.run_coroutine_threadsafe(
                    self._notify_clients(meeting_id, path),
                    self._loop
                )

    async def _notify_clients(self, meeting_id: str, transcript_path: Path):
        """Read new lines and send to connected clients."""
        if not transcript_path.exists():
            return

        file_key = str(transcript_path)

        try:
            st = os.stat(transcript_path)
            last_pos = self.file_positions.get(file_key, 0)
            prev_inode = self.file_inodes.get(file_key)
            rewrite_detected = (
                (prev_inode is not None and st.st_ino != prev_inode)
                or st.st_size < last_pos
            )
            if rewrite_detected:
                last_pos = 0
            self.file_inodes[file_key] = st.st_ino
            seen_ids = self.seen_segment_ids.setdefault(file_key, set())

            with open(transcript_path, 'r', encoding='utf-8') as f:
                f.seek(last_pos)
                new_lines = f.readlines()
                self.file_positions[file_key] = f.tell()

            if new_lines and meeting_id in self.clients:
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
                    if isinstance(seg_id, int) and seg_id in seen_ids:
                        continue
                    segments.append(seg)
                    if isinstance(seg_id, int):
                        seen_ids.add(seg_id)

                if segments:
                    message = json.dumps({"type": "segments", "data": segments})
                    # Snapshot before iterating: concurrent `unregister` on
                    # another task would mutate `self.clients[meeting_id]`
                    # during the inner `await` and raise
                    # `RuntimeError: Set changed size during iteration`.
                    dead_clients = []
                    for ws in list(self.clients[meeting_id]):
                        try:
                            await ws.send_text(message)
                        except Exception:
                            dead_clients.append(ws)

                    for ws in dead_clients:
                        self.clients[meeting_id].discard(ws)
        except Exception as e:
            print(f"Error reading transcript: {e}")
