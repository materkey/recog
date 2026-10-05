"""Native Zoom application as a conference signal source (macOS).

The Zoom application is not a web page, so the Chrome extension cannot see it.
A Swift helper (`recog/native/macos/zoom_ax_probe.swift`) reads its
Accessibility tree and writes one JSON snapshot per line; this module turns
the snapshots into the frames the extension sends to the bridge
(`meeting_join`, `participants_update`, `speaker_update`, `meeting_leave`), so
the bridge, attribution, Copilot and increments need no Zoom-specific code.

What the application exposes (Zoom Workplace 7.1.9): the participant tiles
with a name and a description such as ``Alice, Computer audio unmuted, Video
off, active speaker``, worded in the Zoom interface language. It does not expose the meeting number, so every
application meeting is reported under one URL, and no reliable topic, so the
recording has no title.

The helper runs as its own application bundle, started through LaunchServices,
so the Accessibility permission belongs to "Recog Zoom Helper" and not to
whichever Python interpreter or terminal started this module.

Run: ``python -m recog.zoom_app``, or inside the live process with
``live --zoom-app`` / ``RECOG_ZOOM_APP=1``.
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
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
# Background mode: wait this long before starting over after a failure.
RESTART_S = 30.0
HELPER_CONNECT_S = 15.0

# The helper application: the owner of the Accessibility permission. Its bundle
# identifier is what the permission is recorded under, so it must not change.
HELPER_APP = "RecogZoomHelper.app"
HELPER_BUNDLE_ID = "com.recog.zoom-helper"
HELPER_NAME = "Recog Zoom Helper"
HELPER_SOURCE = "recog/native/macos/zoom_ax_probe.swift"

# The speaker mark is a literal in the Zoom binary with no entry in its
# localization files, so it reads the same in every interface language.
_ACTIVE_TOKEN = "active speaker"

ZOOM_APP_PATHS = (Path("/Applications/zoom.us.app"), Path.home() / "Applications/zoom.us.app")
# Localizable.strings keys of the mute wording on a tile; the numeric suffix is
# a string id that may differ between Zoom versions.
_MUTE_ENTRY = re.compile(
    r'"(?:LN_Plist_Accessibility_(?:VoIP|Tele)_(Muted|Unmuted)_\d+'
    r'|LN_Audio_(Muted|Unmuted)_Accessibility_\d+)"\s*=\s*"((?:[^"\\]|\\.)*)"\s*;'
)

_UNSENT = object()


@dataclass(frozen=True)
class Vocabulary:
    """Lower-cased tile wording for a muted and an unmuted microphone."""

    muted: frozenset[str]
    unmuted: frozenset[str]


# What Zoom Workplace 7.1.9 says in English; used when the installed
# application's own wording cannot be read.
ENGLISH = Vocabulary(
    muted=frozenset({"computer audio muted", "telephone muted", "audio muted"}),
    unmuted=frozenset({"computer audio unmuted", "telephone unmuted", "audio unmuted"}),
)


def load_vocabulary(app_paths: tuple[Path, ...] = ZOOM_APP_PATHS) -> Vocabulary:
    """Mute wording in every language the installed Zoom ships, plus English.

    The tile description is in the Zoom interface language. Reading the wording
    from the application's own localization files covers every language it
    ships without a table kept here, and follows its updates.
    """
    muted, unmuted = set(ENGLISH.muted), set(ENGLISH.unmuted)
    for app in app_paths:
        for strings in sorted(app.glob("Contents/Resources/*.lproj/Localizable.strings")):
            try:
                text = strings.read_text(encoding="utf-16")
            except (OSError, UnicodeError):
                continue
            for first, second, wording in _MUTE_ENTRY.findall(text):
                (muted if (first or second) == "Muted" else unmuted).add(wording.strip().lower())
    return Vocabulary(muted=frozenset(muted), unmuted=frozenset(unmuted))


@dataclass(frozen=True)
class Tile:
    name: str
    muted: bool | None
    active: bool


def parse_tile(name: str, description: str, vocabulary: Vocabulary = ENGLISH) -> Tile:
    """Split ``<name>, <state>, <state>…`` into mute and speaker state.

    Positive evidence only: wording outside the vocabulary yields "mute
    unknown, not speaking", never a guess.
    """
    rest = description[len(name) :] if description.startswith(name) else ""
    tokens = {token.strip().lower() for token in rest.split(",") if token.strip()}
    muted = None
    if tokens & vocabulary.unmuted:
        muted = False
    elif tokens & vocabulary.muted:
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


def _tiles(snapshot: dict, vocabulary: Vocabulary) -> dict[str, Tile]:
    tiles: dict[str, Tile] = {}
    for raw in snapshot.get("tiles", []):
        name = raw.get("name")
        if not name:
            continue
        tile = parse_tile(name, raw.get("description", ""), vocabulary)
        tiles[name] = _merge(tiles[name], tile) if name in tiles else tile
    return tiles


class ZoomAppSession:
    """Turns helper snapshots into bridge frames. Pure: no I/O; `now` is epoch seconds."""

    def __init__(self, vocabulary: Vocabulary = ENGLISH) -> None:
        self.joined = False
        self._vocabulary = vocabulary
        self._last_meeting_at: float | None = None
        self._rejected_at: float | None = None
        self._roster: dict[str, Tile] = {}
        self._self_name: str | None = None
        self._sent_participants: object = _UNSENT
        self._sent_speaker: object = _UNSENT

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
        return [{"type": "meeting_join", "url": MEETING_URL, "platform": PLATFORM, "title": ""}]

    def _state_frames(self, snapshot: dict, now: float) -> list[dict]:
        tiles = _tiles(snapshot, self._vocabulary)
        minimal = snapshot.get("view") == "minimal"
        self._self_name = account_name(snapshot.get("account")) or self._self_name
        if minimal:
            # Minimal view shows one tile; the roster seen before it still holds.
            self._roster.update(tiles)
        else:
            self._roster = tiles

        # The helper also reports a "topic" (the description of the toolbar's
        # info button). It is ignored: on a live meeting that description read
        # "Meeting  40-minutes" — the free-plan timer — instead of the topic, and
        # a wrong title is worse than none.
        frames: list[dict] = []
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


def _helper_info() -> dict:
    return {
        "CFBundleIdentifier": HELPER_BUNDLE_ID,
        "CFBundleName": HELPER_NAME,
        "CFBundleDisplayName": HELPER_NAME,
        "CFBundleExecutable": "zoom_ax_probe",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "1.0",
        # No Dock icon, no menu bar: it only reads another application's tree.
        "LSBackgroundOnly": True,
    }


def _build_helper(app: Path, source: Path, digest: str) -> None:
    contents = app / "Contents"
    (contents / "MacOS").mkdir(parents=True, exist_ok=True)
    (contents / "Resources").mkdir(exist_ok=True)
    print(f"[zoom-app] Building {app.name} from {source}...", file=sys.stderr)
    compile_command = ["swiftc", "-O", "-o", str(contents / "MacOS/zoom_ax_probe"), str(source)]
    compile_command += ["-framework", "ApplicationServices", "-framework", "AppKit"]
    with (contents / "Info.plist").open("wb") as handle:
        plistlib.dump(_helper_info(), handle)
    # Sealed by the signature below, so it is written before signing.
    stamp = contents / "Resources/source.sha256"
    stamp.write_text(digest)
    # macOS recognizes the permission holder by its signature. "-" is an ad-hoc
    # signature: it changes with every build, and the permission must then be
    # granted again. A real identity keeps the grant across builds.
    identity = os.environ.get("RECOG_CODESIGN_IDENTITY") or "-"
    sign_command = [
        "codesign",
        "--force",
        "--sign",
        identity,
        "--identifier",
        HELPER_BUNDLE_ID,
        str(app),
    ]
    for command in (compile_command, sign_command):
        result = subprocess.run(command, capture_output=True, text=True)
        if result.returncode != 0:
            stamp.unlink(missing_ok=True)  # not fresh: the next start builds again
            raise RuntimeError(
                f"Failed to build {app.name} ({command[0]}): {result.stderr.strip()}"
            )
    print(
        f'[zoom-app] Built {app.name}: allow "{HELPER_NAME}" in System Settings → '
        "Privacy & Security → Accessibility",
        file=sys.stderr,
    )


def find_or_build_helper(repo_root: Path = REPO_ROOT) -> Path:
    """The helper application; built when missing or when its source changed.

    Freshness is the source's content hash, not its mtime: a rebuild changes an
    ad-hoc signature and with it loses the granted permission, so a checkout
    that merely touches the file must not trigger one.
    """
    app = repo_root / HELPER_APP
    source = repo_root / HELPER_SOURCE
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    stamp = app / "Contents/Resources/source.sha256"
    built = (app / "Contents/_CodeSignature/CodeResources").exists()
    if built and stamp.exists() and stamp.read_text().strip() == digest:
        return app
    if not shutil.which("swiftc"):
        if built:
            return app  # a stale helper is better than none
        raise RuntimeError(f"swiftc not found — cannot build {HELPER_APP} (xcode-select --install)")
    _build_helper(app, source, digest)
    return app


@contextlib.asynccontextmanager
async def helper_lines(app: Path):
    """Start the helper application and yield the stream of its snapshots.

    The helper is launched by LaunchServices and therefore shares no pipe with
    this process; it connects back to a Unix socket in a private temporary
    directory and exits when that socket goes away.
    """
    directory = Path(tempfile.mkdtemp(prefix="recog-zoom-"))
    socket_path = directory / "probe.sock"
    connected: asyncio.Future = asyncio.get_running_loop().create_future()

    def on_connect(reader, writer) -> None:
        if connected.done():
            writer.close()
        else:
            connected.set_result((reader, writer))

    server = await asyncio.start_unix_server(on_connect, path=str(socket_path))
    try:
        # -n: a fresh instance bound to this socket; -g: never comes to the front.
        launch = ["open", "-g", "-n", "-a", str(app), "--args", "--socket", str(socket_path)]
        result = subprocess.run(launch, capture_output=True, text=True)
        if result.returncode != 0:
            raise RuntimeError(f"cannot launch {app.name}: {result.stderr.strip()}")
        try:
            reader, writer = await asyncio.wait_for(connected, HELPER_CONNECT_S)
        except asyncio.TimeoutError:
            raise RuntimeError(
                f"{app.name} did not connect within {HELPER_CONNECT_S:.0f} s"
            ) from None
        try:
            yield reader
        finally:
            writer.close()
    finally:
        server.close()
        shutil.rmtree(directory, ignore_errors=True)


class _StatusLog:
    """One stderr line per change of the helper state."""

    MESSAGES = {
        "not_trusted": f'"{HELPER_NAME}" has no Accessibility permission — Zoom meetings are not '
        "detected (System Settings → Privacy & Security → Accessibility)",
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
                    raise RuntimeError("the Zoom helper disconnected")
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

    session = ZoomAppSession(load_vocabulary())
    log = _StatusLog()
    async with helper_lines(find_or_build_helper()) as lines:
        while True:
            try:
                # No Origin header: the bridge treats this as a local client.
                async with websockets.connect(bridge_url) as websocket:
                    await _pump(websocket, lines, session, log)
            except (OSError, websockets.WebSocketException) as exc:
                print(f"[zoom-app] bridge link lost: {exc}", file=sys.stderr)
            session.on_disconnect()
            await asyncio.sleep(RECONNECT_S)


def main() -> None:
    if sys.platform != "darwin":
        raise SystemExit("recog.zoom_app supports macOS only")
    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        pass


def _supervise(stop: threading.Event) -> None:
    """Keep the source running; a failure must not take live down with it."""
    while not stop.is_set():
        try:
            asyncio.run(run())
        except Exception as exc:
            print(f"[zoom-app] stopped: {exc}; starting over in {RESTART_S:.0f} s", file=sys.stderr)
        stop.wait(RESTART_S)


def start_in_background() -> threading.Thread | None:
    """Run the source in a daemon thread of the calling process; None off macOS."""
    if sys.platform != "darwin":
        print(
            "[zoom-app] the native Zoom source supports macOS only — not started", file=sys.stderr
        )
        return None
    thread = threading.Thread(
        target=_supervise, args=(threading.Event(),), name="zoom-app", daemon=True
    )
    thread.start()
    return thread


if __name__ == "__main__":
    main()
