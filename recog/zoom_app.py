"""Native Zoom application as a conference signal source (macOS).

The Zoom application is not a web page, so the Chrome extension cannot see it.
A Swift helper (`recog/native/macos/zoom_ax_probe.swift`) reads its
Accessibility tree and prints one JSON snapshot per line; this module turns
the snapshots into the frames the extension sends to the bridge
(`meeting_join`, `participants_update`, `speaker_update`, `meeting_leave`), so
the bridge, attribution, Copilot and increments need no Zoom-specific code.

What the application exposes (Zoom Workplace 7.1.9): the participant tiles
with a name and a description such as ``Alice, Computer audio unmuted, Video
off, active speaker``. It does not expose the meeting number, so every
application meeting is reported under one URL; the topic is readable only
while the toolbar is shown.

Run: ``python -m recog.zoom_app``, or inside the live process with
``live --zoom-app`` / ``RECOG_ZOOM_APP=1``. The process that runs it needs the
Accessibility permission.
"""

from __future__ import annotations

import asyncio
import json
import shutil
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BRIDGE_URL = "ws://127.0.0.1:8766"
PLATFORM = "zoom"
# The application exposes no meeting number. One URL for all of them keeps a
# rejoin inside the storage reopen window in the same recording.
MEETING_URL = "https://zoom.us/app"

# A helper restart or a view switch must not end the meeting.
LEAVE_DEBOUNCE_S = 6.0
# After the bridge rejects a join (another source owns the meeting), stay
# silent this long before asking again.
JOIN_RETRY_S = 30.0
RECONNECT_S = 3.0

# Tile description vocabulary, English Zoom UI. Positive evidence only: an
# unknown wording yields "not muted, not speaking", never a guess.
_ACTIVE_TOKEN = "active speaker"
_UNMUTED_SUFFIX = " unmuted"
_MUTED_SUFFIX = " muted"

_UNSENT = object()


@dataclass(frozen=True)
class Tile:
    name: str
    muted: bool | None
    active: bool


def parse_tile(name: str, description: str) -> Tile:
    """Split ``<name>, <state>, <state>…`` into mute and speaker state."""
    rest = description[len(name) :] if description.startswith(name) else ""
    tokens = [token.strip().lower() for token in rest.split(",") if token.strip()]
    muted = None
    for token in tokens:
        if token.endswith(_UNMUTED_SUFFIX):
            muted = False
        elif token.endswith(_MUTED_SUFFIX):
            muted = True
    return Tile(name=name, muted=muted, active=_ACTIVE_TOKEN in tokens)


def account_name(description: str | None) -> str | None:
    """Display name from ``Zoom, <name>, <status>, <plan>``, else None."""
    parts = [part.strip() for part in (description or "").split(",")]
    if len(parts) >= 3 and parts[0] == "Zoom" and parts[1]:
        return parts[1]
    return None


def _merge(first: Tile, second: Tile) -> Tile:
    """Namesakes share an entry: muted only when both are, speaking when either is."""
    muted = first.muted if first.muted == second.muted else (first.muted and second.muted)
    return Tile(name=first.name, muted=muted, active=first.active or second.active)


def _tiles(snapshot: dict) -> dict[str, Tile]:
    tiles: dict[str, Tile] = {}
    for raw in snapshot.get("tiles", []):
        name = raw.get("name")
        if not name:
            continue
        tile = parse_tile(name, raw.get("description", ""))
        tiles[name] = _merge(tiles[name], tile) if name in tiles else tile
    return tiles


class ZoomAppSession:
    """Turns helper snapshots into bridge frames. Pure: no I/O; `now` is epoch seconds."""

    def __init__(self) -> None:
        self.joined = False
        self._last_meeting_at: float | None = None
        self._rejected_at: float | None = None
        self._roster: dict[str, Tile] = {}
        self._self_name: str | None = None
        self._sent_participants: object = _UNSENT
        self._sent_speaker: object = _UNSENT
        self._sent_title: str | None = None

    def on_rejected(self, now: float) -> None:
        """The bridge refused the join: another source owns the meeting."""
        self.joined = False
        self._rejected_at = now

    def on_disconnect(self) -> None:
        """The bridge link dropped: the bridge has already ended the meeting."""
        self.joined = False
        self._rejected_at = None

    def frames(self, snapshot: dict, now: float) -> list[dict]:
        if snapshot.get("state") != "meeting":
            return self._leave_frames(now)
        self._last_meeting_at = now
        if not self.joined:
            return self._join_frames(now)
        return self._state_frames(snapshot, now)

    def _leave_frames(self, now: float) -> list[dict]:
        if not self.joined or self._last_meeting_at is None:
            self._rejected_at = None
            return []
        if now - self._last_meeting_at < LEAVE_DEBOUNCE_S:
            return []
        self.joined = False
        self._roster = {}
        return [{"type": "meeting_leave", "reason": "user_left"}]

    def _join_frames(self, now: float) -> list[dict]:
        if self._rejected_at is not None and now - self._rejected_at < JOIN_RETRY_S:
            return []
        # Only the join: participants and the speaker follow with the next
        # snapshot, after a rejection had time to arrive.
        self.joined = True
        self._rejected_at = None
        self._roster = {}
        self._sent_participants = _UNSENT
        self._sent_speaker = _UNSENT
        self._sent_title = None
        return [{"type": "meeting_join", "url": MEETING_URL, "platform": PLATFORM, "title": ""}]

    def _state_frames(self, snapshot: dict, now: float) -> list[dict]:
        tiles = _tiles(snapshot)
        minimal = snapshot.get("view") == "minimal"
        self._self_name = account_name(snapshot.get("account")) or self._self_name
        if minimal:
            # Minimal view shows one tile; the roster seen before it still holds.
            self._roster.update(tiles)
        else:
            self._roster = tiles

        frames: list[dict] = []
        topic = snapshot.get("topic")
        if topic and topic != self._sent_title:
            self._sent_title = topic
            frames.append({"type": "meeting_title_update", "title": topic})

        participants = [
            {
                "id": name,
                "name": name,
                "isLocal": name == self._self_name,
                "micMuted": tile.muted is True,
            }
            for name, tile in self._roster.items()
        ]
        if participants != self._sent_participants:
            self._sent_participants = participants
            frames.append({"type": "participants_update", "participants": participants})

        speaker = self._speaker(tiles, minimal)
        if speaker != self._sent_speaker:
            self._sent_speaker = speaker
            frames.append(
                {"type": "speaker_update", "speaker": speaker, "timestamp": int(now * 1000)}
            )
        return frames

    def _speaker(self, tiles: dict[str, Tile], minimal: bool) -> dict | None:
        # Minimal view has no "active speaker" wording: its only tile is the speaker.
        candidates = (
            list(tiles.values())
            if minimal and len(tiles) == 1
            else [t for t in tiles.values() if t.active]
        )
        for tile in candidates:
            if tile.muted is not True:
                return {"id": tile.name, "name": tile.name, "isLocal": tile.name == self._self_name}
        return None


def find_or_build_probe(repo_root: Path = REPO_ROOT) -> str:
    """Path to the zoom_ax_probe helper; compiles it when missing or stale."""
    binary = repo_root / "zoom_ax_probe"
    source = repo_root / "recog/native/macos/zoom_ax_probe.swift"
    fresh = binary.exists() and binary.stat().st_mtime >= source.stat().st_mtime
    if fresh:
        return str(binary)
    if not shutil.which("swiftc"):
        if binary.exists():
            return str(binary)
        raise RuntimeError("swiftc not found — cannot build zoom_ax_probe (xcode-select --install)")
    print(f"[zoom-app] Compiling {source}...", file=sys.stderr)
    command = ["swiftc", "-O", "-o", str(binary), str(source)]
    command += ["-framework", "ApplicationServices", "-framework", "AppKit"]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"Failed to compile zoom_ax_probe: {result.stderr}")
    return str(binary)


class _StatusLog:
    """One stderr line per change of the helper state."""

    MESSAGES = {
        "not_trusted": "no Accessibility permission for this process — Zoom meetings are not detected",
        "zoom_not_running": "Zoom is not running",
        "idle": "Zoom is running, no meeting",
        "meeting": "Zoom meeting detected",
    }

    def __init__(self) -> None:
        self._state: str | None = None

    def note(self, state: str | None) -> None:
        if state == self._state:
            return
        self._state = state
        print(
            f"[zoom-app] {self.MESSAGES.get(state or '', f'unknown helper state: {state}')}",
            file=sys.stderr,
        )


async def _pump(
    websocket, lines: asyncio.StreamReader, session: ZoomAppSession, log: _StatusLog
) -> None:
    """Forward helper snapshots to the bridge until either side closes."""
    next_line = asyncio.ensure_future(lines.readline())
    next_message = asyncio.ensure_future(websocket.recv())
    try:
        while True:
            await asyncio.wait({next_line, next_message}, return_when=asyncio.FIRST_COMPLETED)
            if next_message.done():
                message = json.loads(next_message.result())
                if message.get("type") == "meeting_join_rejected":
                    print(f"[zoom-app] join rejected: {message.get('reason')}", file=sys.stderr)
                    session.on_rejected(time.time())
                next_message = asyncio.ensure_future(websocket.recv())
            if next_line.done():
                raw = next_line.result()
                if not raw:
                    raise RuntimeError("zoom_ax_probe exited")
                snapshot = json.loads(raw)
                log.note(snapshot.get("state"))
                for frame in session.frames(snapshot, time.time()):
                    await websocket.send(json.dumps(frame, ensure_ascii=False))
                next_line = asyncio.ensure_future(lines.readline())
    finally:
        next_line.cancel()
        next_message.cancel()


async def run(bridge_url: str = BRIDGE_URL) -> None:
    import websockets

    helper = await asyncio.create_subprocess_exec(
        find_or_build_probe(), stdout=asyncio.subprocess.PIPE
    )
    session = ZoomAppSession()
    log = _StatusLog()
    try:
        while helper.returncode is None:
            try:
                # No Origin header: the bridge treats this as a local client.
                async with websockets.connect(bridge_url) as websocket:
                    await _pump(websocket, helper.stdout, session, log)
            except (OSError, websockets.WebSocketException) as exc:
                print(f"[zoom-app] bridge link lost: {exc}", file=sys.stderr)
            session.on_disconnect()
            await asyncio.sleep(RECONNECT_S)
    finally:
        if helper.returncode is None:
            helper.terminate()


def main() -> None:
    if sys.platform != "darwin":
        raise SystemExit("recog.zoom_app supports macOS only")
    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        pass


def _run_until_failure() -> None:
    try:
        asyncio.run(run())
    except Exception as exc:  # a dead source must not take live down with it
        print(f"[zoom-app] stopped: {exc}", file=sys.stderr)


def start_in_background() -> threading.Thread | None:
    """Run the source in a daemon thread of the calling process; None off macOS."""
    if sys.platform != "darwin":
        print(
            "[zoom-app] the native Zoom source supports macOS only — not started", file=sys.stderr
        )
        return None
    thread = threading.Thread(target=_run_until_failure, name="zoom-app", daemon=True)
    thread.start()
    return thread


if __name__ == "__main__":
    main()
