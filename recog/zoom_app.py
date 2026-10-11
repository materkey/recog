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

The helper reports what the tree says and nothing more; what a described tab
group means is decided here. The rules follow the code that words the tree in
the ``zVideoUI`` binary of that version (``-[VideoUI accessibilityLabel]`` and
the name tag of ``ZMMTVRInfoTagView``), see ``docs/zoom-app.md``.

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
# How often a helper is started afresh while the permission is missing. A
# running helper cannot notice the grant: macOS caches the answer per process.
PERMISSION_RETRY_S = 15.0

# The helper application: the owner of the Accessibility permission. Its bundle
# identifier is what the permission is recorded under, so it must not change.
HELPER_APP = "RecogZoomHelper.app"
HELPER_BUNDLE_ID = "com.recog.zoom-helper"
HELPER_NAME = "Recog Zoom Helper"
HELPER_SOURCE = "recog/native/macos/zoom_ax_probe.swift"
# Hash of the source the bundle was built from. Kept next to the bundle, not in
# it: everything inside is sealed by the signature, so a stamp there would make
# any edit of the source, even a comment, change the signature and lose the
# permission, although the same code compiles to the same binary.
HELPER_STAMP = "RecogZoomHelper.source-sha256"

# The speaker mark is a literal in the Zoom binary with no entry in its
# localization files, so it reads the same in every interface language. So do
# the video state and the wrapper around the name on a tile's name tag.
_ACTIVE_TOKEN = "active speaker"
_VIDEO_TOKENS = frozenset({"video on", "video off"})
# The name tag is a button described by the display name, or by this when Zoom
# can show the participant's profile card.
_PROFILE_BUTTON = re.compile(r"View (.+)'s profile", re.DOTALL)

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


def _tokens(text: str) -> set[str]:
    return {token.strip().lower() for token in text.split(",") if token.strip()}


def tile_name(description: str, buttons: list[str]) -> str | None:
    """Display name of the participant a described tab group shows, else None.

    The description starts with the name, and so a button must name the same
    participant: bare, or as "View <name>'s profile". A name may contain a
    comma, so it cannot be cut out of the description alone.
    """
    for label in buttons:
        wrapped = _PROFILE_BUTTON.fullmatch(label)
        for name in (wrapped.group(1), label) if wrapped else (label,):
            if description == name or description.startswith(name + ","):
                return name
    return None


def parse_tile(name: str, description: str, vocabulary: Vocabulary = ENGLISH) -> Tile:
    """Split ``<name>, <state>, <state>…`` into mute and speaker state.

    Positive evidence only: wording outside the vocabulary yields "mute
    unknown, not speaking", never a guess.
    """
    rest = description[len(name) :] if description.startswith(name) else ""
    tokens = _tokens(rest)
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


def _buttons(raw: dict) -> list[str]:
    if "children" not in raw:
        # A helper built before the tree was reported raw (kept when it cannot
        # be rebuilt) names the tile itself.
        return [raw["name"]] if raw.get("name") else []
    return [
        child["description"]
        for child in raw["children"]
        if child.get("role") == "AXButton" and child.get("description")
    ]


def _tiles(snapshot: dict, vocabulary: Vocabulary) -> tuple[dict[str, Tile], list[dict]]:
    """Participant tiles by name, and the tab groups that look like one but name nobody."""
    tiles: dict[str, Tile] = {}
    unread: list[dict] = []
    states = vocabulary.muted | vocabulary.unmuted | _VIDEO_TOKENS | {_ACTIVE_TOKEN}
    for raw in snapshot.get("tiles", []):
        description = raw.get("description", "")
        name = tile_name(description, _buttons(raw))
        if not name:
            # Any other described tab group is not a tile; one that words a
            # participant's state is a tile Zoom now names differently.
            if _tokens(description) & states:
                unread.append(raw)
            continue
        tile = parse_tile(name, description, vocabulary)
        tiles[name] = _merge(tiles[name], tile) if name in tiles else tile
    return tiles, unread


class ZoomAppSession:
    """Turns helper snapshots into bridge frames. Pure: no I/O; `now` is epoch seconds."""

    def __init__(self, vocabulary: Vocabulary = ENGLISH) -> None:
        self.joined = False
        self._vocabulary = vocabulary
        self._last_meeting_at: float | None = None
        self._rejected_at: float | None = None
        self._roster: dict[str, Tile] = {}
        self._self_name: str | None = None
        # Of the last snapshot: the helper state as understood here, and the
        # tab groups that look like a participant tile but name nobody.
        self.state: str | None = None
        self.unread: list[dict] = []
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
        # Reported outside a meeting too; inside one the main window that
        # carries it is not always reachable.
        self._self_name = account_name(snapshot.get("account")) or self._self_name
        self.state = snapshot.get("state")
        self.unread = []
        if self.state != "meeting":
            return self._leave_frames(now)
        tiles, self.unread = _tiles(snapshot, self._vocabulary)
        minimal = snapshot.get("view") == "minimal"
        if minimal and not tiles:
            # Without the meeting window the helper reports the tab groups of
            # every dialog; they are the minimal view only when one of them
            # shows a participant.
            self.state = "idle"
            return self._leave_frames(now)
        self._last_meeting_at = now
        if not self.joined:
            # The join preview is already the meeting window, with no tile in
            # it yet. A meeting starts with the first participant tile.
            if not tiles:
                return []
            return self._join_frames(now)
        return self._state_frames(tiles, minimal, now)

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

    def _state_frames(self, tiles: dict[str, Tile], minimal: bool, now: float) -> list[dict]:
        # The tree holds only the tiles on screen: one in minimal view, a page of
        # them in a large meeting or a small window. The roster therefore grows
        # over the meeting and is never trimmed — a participant who left stays
        # listed, which is a smaller error than dropping everyone off-screen.
        self._roster.update(tiles)

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


def _build_helper(app: Path, source: Path) -> None:
    # Built aside and swapped in on success: a failed build must not take a
    # working helper away.
    staging = app.with_name(app.name + ".build")
    shutil.rmtree(staging, ignore_errors=True)
    contents = staging / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    print(f"[zoom-app] Building {app.name} from {source}...", file=sys.stderr)
    compile_command = ["swiftc", "-O", "-o", str(contents / "MacOS/zoom_ax_probe"), str(source)]
    compile_command += ["-framework", "ApplicationServices", "-framework", "AppKit"]
    with (contents / "Info.plist").open("wb") as handle:
        plistlib.dump(_helper_info(), handle)
    # macOS recognizes the permission holder by its signature. "-" is an ad-hoc
    # signature, which is the hash of the bundle: it stays the same while the
    # code does, and the permission must be granted again once the code
    # changes. A real identity keeps the grant across such changes too.
    identity = os.environ.get("RECOG_CODESIGN_IDENTITY") or "-"
    sign_command = [
        "codesign",
        "--force",
        "--sign",
        identity,
        "--identifier",
        HELPER_BUNDLE_ID,
        str(staging),
    ]
    for command in (compile_command, sign_command):
        result = subprocess.run(command, capture_output=True, text=True)
        if result.returncode != 0:
            raise RuntimeError(
                f"Failed to build {app.name} ({command[0]}): {result.stderr.strip()}"
            )
    shutil.rmtree(app, ignore_errors=True)
    staging.rename(app)
    print(
        f'[zoom-app] Built {app.name}: allow "{HELPER_NAME}" in System Settings → '
        "Privacy & Security → Accessibility",
        file=sys.stderr,
    )


def find_or_build_helper(repo_root: Path = REPO_ROOT) -> Path:
    """The helper application; built when missing or when its source changed.

    Freshness is the source's content hash, not its mtime, so a checkout that
    merely touches the file does not rebuild. The stamp is written last: a
    build that failed half-way is repeated on the next start.
    """
    app = repo_root / HELPER_APP
    source = repo_root / HELPER_SOURCE
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    stamp = repo_root / HELPER_STAMP
    built = (app / "Contents/_CodeSignature/CodeResources").exists()
    if built and stamp.exists() and stamp.read_text().strip() == digest:
        return app
    if not shutil.which("swiftc"):
        if built:
            return app  # a stale helper is better than none
        raise RuntimeError(f"swiftc not found — cannot build {HELPER_APP} (xcode-select --install)")
    _build_helper(app, source)
    stamp.write_text(digest)
    return app


@contextlib.asynccontextmanager
async def helper_lines(app: Path, prompt: bool = True):
    """Start the helper application and yield the stream of its snapshots.

    `prompt=False` keeps a helper without the Accessibility permission from
    showing the system dialog again.

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
        if not prompt:
            launch.append("--no-prompt")
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


class HelperGone(RuntimeError):
    """The helper closed its stream."""


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
        self.state: str | None = None
        self._unread_noted = False

    def note(self, state: str | None) -> None:
        if state == self.state:
            return
        self.state = state
        self._unread_noted = False
        print(
            f"[zoom-app] {self.MESSAGES.get(state or '', f'unknown helper state: {state}')}",
            file=sys.stderr,
        )

    def note_unread(self, unread: list[dict]) -> None:
        """Once per meeting: tiles that are left out because their naming is not understood."""
        if not unread or self._unread_noted:
            return
        self._unread_noted = True
        print(
            f"[zoom-app] {len(unread)} participant tile(s) ignored: Zoom names them in a way "
            f"this version does not know, e.g. {json.dumps(unread[0], ensure_ascii=False)}",
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
                    raise HelperGone("the Zoom helper disconnected")
                frames = session.frames(json.loads(raw), time.time())
                log.note(session.state)
                log.note_unread(session.unread)
                for frame in frames:
                    await websocket.send(json.dumps(frame, ensure_ascii=False))
                next_line = asyncio.ensure_future(lines.readline())
    finally:
        next_line.cancel()
        next_message.cancel()


async def _serve(bridge_url: str, lines, session: ZoomAppSession, log: _StatusLog) -> None:
    """Forward the helper's snapshots, reconnecting to the bridge, until the helper is gone."""
    import websockets

    while True:
        try:
            # No Origin header: the bridge treats this as a local client.
            async with websockets.connect(bridge_url) as websocket:
                await _pump(websocket, lines, session, log)
        except (OSError, websockets.WebSocketException) as exc:
            print(f"[zoom-app] bridge link lost: {exc}", file=sys.stderr)
        session.on_disconnect()
        await asyncio.sleep(RECONNECT_S)


async def run(bridge_url: str = BRIDGE_URL) -> None:
    session = ZoomAppSession(load_vocabulary())
    log = _StatusLog()
    app = find_or_build_helper()
    prompt = True
    while True:
        try:
            async with helper_lines(app, prompt) as lines:
                await _serve(bridge_url, lines, session, log)
        except HelperGone:
            # A helper without the permission exits after reporting it; anything
            # else that ends the stream is a failure.
            if log.state != "not_trusted":
                raise
        prompt = False
        await asyncio.sleep(PERMISSION_RETRY_S)


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
