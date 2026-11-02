"""Disposable, event-driven read model for the meeting list.

Only the refresh worker touches files. Requests read a published snapshot;
watchdog notifications are hints, with periodic stat reconciliation as a backstop.
"""

import asyncio
import json
import logging
import os
from datetime import datetime, timezone
from pathlib import Path

from recog.install_config import canonical_source

from watchdog.events import FileSystemEventHandler


logger = logging.getLogger(__name__)


class MeetingCatalog(FileSystemEventHandler):
    def __init__(self, base_path: Path, external_paths=(), *,
                 reconcile_seconds: float = 30, debounce_seconds: float = 0.05,
                 retry_seconds: float = 1):
        self.base_path = base_path.resolve()
        self.external_paths = [(p.resolve(), source) for p, source in external_paths]
        self.roots = tuple(dict.fromkeys([self.base_path, *[p for p, _ in self.external_paths]]))
        self.reconcile_seconds = reconcile_seconds
        self.debounce_seconds = debounce_seconds
        self.retry_seconds = retry_seconds
        self._files = {}
        self._retry_paths: set[Path] = set()
        self._meetings: tuple[dict, ...] = ()
        self._dirty: set[Path] = set()
        self._wake = asyncio.Event()
        self._lock = asyncio.Lock()
        self._loop = None
        self._task = None
        self._stopping = False
        self._needs_retry = False
        self._rescan_needed = False

    async def start(self):
        # The observer is already running: changes during this scan are queued.
        self._loop = asyncio.get_running_loop()
        await self.refresh()
        self._task = asyncio.create_task(self._run())

    async def stop(self):
        self._stopping = True
        self._wake.set()
        if self._task:
            await self._task
        self._loop = None

    def on_any_event(self, event):
        if event.event_type not in {"created", "modified", "deleted", "moved"}:
            return
        paths = set()
        for value in (event.src_path, getattr(event, "dest_path", "")):
            if not value:
                continue
            path = Path(os.fsdecode(value))
            if event.is_directory:
                if event.event_type != "modified" and path.parent in self.roots:
                    paths.add(path / "meta.json")
            elif (path == self.base_path / "index.json" or
                  (path.name == "meta.json" and path.parent.parent in self.roots)):
                paths.add(path)
        if paths and self._loop and not self._stopping:
            try:
                self._loop.call_soon_threadsafe(self._invalidate, paths)
            except RuntimeError:
                pass  # The event loop closed while watchdog was dispatching.

    def _invalidate(self, paths):
        self._dirty.update(paths)
        self._wake.set()

    async def _run(self):
        deadline = self._loop.time() + self.reconcile_seconds
        while not self._stopping:
            timeout = max(0, deadline - self._loop.time())
            if self._needs_retry:
                timeout = min(timeout, self.retry_seconds)
            try:
                await asyncio.wait_for(self._wake.wait(), timeout=timeout)
            except TimeoutError:
                pass
            if self._stopping:
                break
            await asyncio.sleep(self.debounce_seconds)
            dirty, self._dirty = self._dirty, set()
            self._wake.clear()
            full = self._loop.time() >= deadline
            try:
                await self.refresh(None if full else dirty)
            except Exception:
                logger.exception("Meeting catalog refresh failed; retaining previous snapshot")
                self._dirty.update(dirty)
                self._needs_retry = True
                self._rescan_needed = True
            if full:
                deadline = self._loop.time() + self.reconcile_seconds

    async def refresh(self, paths: set[Path] | None = None):
        """Serialize refreshes, including read-after-write refreshes from the API."""
        async with self._lock:
            worker = asyncio.create_task(asyncio.to_thread(self._refresh_files, paths))
            cancelled = False
            # Cancelling to_thread's await does not stop the thread. Keep the
            # lock until it exits so no second worker can mutate the file cache.
            while not worker.done():
                try:
                    await asyncio.shield(worker)
                except asyncio.CancelledError:
                    cancelled = True
            meetings, needs_retry = worker.result()
            self._meetings = tuple(meetings)
            self._needs_retry = needs_retry
            if self._task and asyncio.current_task() is not self._task:
                self._wake.set()
            if cancelled:
                raise asyncio.CancelledError

    def list_meetings(self) -> list[dict]:
        now = datetime.now(timezone.utc)
        meetings = []
        for entry in self._meetings:
            row = dict(entry)
            if "participants" in row:
                row["participants"] = list(row["participants"])
            if row.get("is_active"):
                try:
                    row["duration_seconds"] = int(
                        (now - datetime.fromisoformat(row["created_at"])).total_seconds()
                    )
                except (ValueError, TypeError):
                    row["duration_seconds"] = 0
            meetings.append(row)
        return meetings

    @staticmethod
    def _signature(path):
        stat = path.stat()
        return stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns

    @staticmethod
    def _validate_entry(row):
        if not isinstance(row, dict) or not isinstance(row.get("id"), str):
            raise ValueError("entry requires an id")
        for field in ("created_at", "title"):
            if row.get(field) is not None and not isinstance(row[field], str):
                raise ValueError(f"invalid {field}")
        if "participants" in row and (not isinstance(row["participants"], list) or
                                      any(not isinstance(p, str) for p in row["participants"])):
            raise ValueError("invalid participants")
        if row.get("is_active") and not isinstance(row.get("created_at"), str):
            raise ValueError("active entry requires created_at")

    def _read_file(self, path):
        try:
            signature = self._signature(path)
            if path in self._files and self._files[path][0] == signature:
                self._retry_paths.discard(path)
                return
            data = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(data, dict):
                raise ValueError("expected a JSON object")
            if path == self.base_path / "index.json":
                rows = data.get("meetings", [])
                if not isinstance(rows, list):
                    raise ValueError("invalid meetings index")
                for row in rows:
                    self._validate_entry(row)
                entry = rows
            else:
                entry = self._metadata_entry(path, data)
                self._validate_entry(entry)
            if self._signature(path) != signature:
                raise ValueError("file changed during read")
        except FileNotFoundError:
            self._files.pop(path, None)
            self._retry_paths.discard(path)
            return
        except (OSError, ValueError, KeyError, TypeError, AttributeError) as exc:
            if path not in self._retry_paths:
                logger.warning("Cannot refresh %s; retaining last valid metadata: %s", path, exc)
            self._retry_paths.add(path)
            return
        self._files[path] = (signature, entry)
        self._retry_paths.discard(path)

    def _metadata_entry(self, path, meta):
        if not isinstance(meta.get("id"), str) or not isinstance(meta.get("created_at"), str):
            raise ValueError("metadata requires id and created_at")
        source = meta.get("source")
        external = path.parent.parent != self.base_path
        if external:
            source = next(override or source or "unknown" for root, override in self.external_paths
                          if root == path.parent.parent)
        active = not external and meta.get("ended_at") is None
        entry = {
            "id": meta["id"], "created_at": meta["created_at"],
            "ended_at": meta.get("ended_at"),
            "duration_seconds": meta.get("stats", {}).get("duration_seconds", 0),
            "title": meta.get("title"), "source": canonical_source(source),
            "end_reason": None if active else meta.get("end_reason"),
            "conference_url" if external else "ktalk_live_url":
                meta.get("conference", meta.get("ktalk", {})).get("live_url"),
            "segment_count": meta.get("stats", {}).get("segment_count", 0),
            "participants": [p.get("name", key) for key, p in meta.get("participants", {}).items()],
            "recorder": meta.get("callrec", {}).get("recorder"),
        }
        if active:
            entry["is_active"] = True
        if external:
            entry.update(_external=True, _external_path=str(path.parent.parent))
        return entry

    def _refresh_files(self, paths):
        scan_failed = False
        if paths is None or self._rescan_needed:
            paths = {self.base_path / "index.json"}
            for root in self.roots:
                try:
                    with os.scandir(root) as children:
                        paths.update(Path(child.path) / "meta.json" for child in children if child.is_dir())
                except FileNotFoundError:
                    pass
                except OSError as exc:
                    logger.warning("Cannot scan %s; retaining catalog entries: %s", root, exc)
                    paths.update(path for path in self._files if path.parent.parent == root)
                    scan_failed = True
            for path in self._files.keys() - paths:
                del self._files[path]
            self._retry_paths.intersection_update(paths)
            self._rescan_needed = scan_failed
        for path in paths | self._retry_paths.copy():
            self._read_file(path)

        indexed = self._files.get(self.base_path / "index.json", (None, []))[1]
        local = {row["id"]: row for row in indexed}
        external = []
        for path, (_, entry) in self._files.items():
            if path.name != "meta.json":
                continue
            if entry.get("_external"):
                external.append(entry)
            elif entry.get("is_active") or entry["segment_count"]:
                # Metadata is authoritative across the recorder's two-file
                # end/reopen transition; index-only archives remain accessible.
                local[entry["id"]] = entry
            else:
                local.pop(entry["id"], None)
        meetings = [*local.values(), *external]
        meetings.sort(key=lambda row: row.get("created_at") or "", reverse=True)
        return meetings, bool(self._retry_paths) or scan_failed
