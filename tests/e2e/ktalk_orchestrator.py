"""KTalk E2E test orchestrator.

Coordinates browser participants and live transcription for end-to-end
diarization tests.

Speaker timing is controlled by the WAV files themselves (speech at scheduled
offsets, silence elsewhere) because KTalk mute/unmute does NOT stop Chromium's
fake audio stream.

Additionally, speaker identity signals are injected directly into the KTalk
bridge via WebSocket (same protocol as the Chrome extension) because Jitsi's
server-side dominant speaker detection doesn't work reliably with Chromium's
fake audio capture.
"""
import json
import os
import re
import sys
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from playwright.sync_api import sync_playwright

from tests.e2e.ktalk_pages import (
    launch_host_browser,
    launch_guest_browser,
    kontur_login,
    create_public_room,
    guest_join_room,
)

_FIXTURES_DIR = Path(__file__).parent / "fixtures"
_SILENCE_WAV = str(_FIXTURES_DIR / "silence.wav")
_BRIDGE_WS_URL = "ws://localhost:8766"


@dataclass
class SpeakerWindow:
    """A time window during which a guest is speaking."""
    guest_name: str
    start: float   # seconds from recording start
    end: float     # seconds from recording start


@dataclass
class E2ETestResult:
    """Result collected after a KTalkOrchestrator.run() call."""

    transcript_lines: list[str] = field(default_factory=list)
    speakers_found: set[str] = field(default_factory=set)
    duration_seconds: float = 0.0
    errors: list[str] = field(default_factory=list)
    debug_state: dict = field(default_factory=dict)
    meeting_path: Optional[Path] = None
    # True iff at least one real PCM chunk reached PerSpeakerIngest before
    # the test finished. If False in a test that requires PCM flow (e.g.
    # degraded-mode), the caller should fail with a clear diagnostic
    # instead of asserting downstream markers that depend on pcm_ever_seen.
    first_pcm_seen: bool = False
    # Captured `console.*` messages from the host page (extension log
    # surface). Used to diagnose extension-side failures that would
    # otherwise be invisible to pytest — AudioContext state, worklet load
    # errors, attach-summary counts. Tagged with message type prefix.
    browser_console_lines: list[str] = field(default_factory=list)
    # Preflight snapshot of `conf._participantsMap` taken shortly after
    # guests join. Distinguishes "SFU not relaying fake audio to host"
    # from "extension worklet fails to start" when first_pcm_seen=False.
    participants_preflight: list[dict] = field(default_factory=list)


class KTalkOrchestrator:
    """Orchestrates a full KTalk e2e diarization test.

    Launches a host browser (with the KTalk speaker extension) and one or
    more guest browsers (with per-speaker timed audio files), joins them all
    into a single room, waits for the audio to play through, and collects the
    live transcript produced by LiveTranscriber.

    Speaker timing is baked into the WAV files: each file contains silence
    where the guest should be quiet and speech where they should talk.
    """

    def __init__(
        self,
        guest_names: list[str],
        guest_audio_files: list[str],
        speaker_windows: list[SpeakerWindow],
        record_duration: float = 45.0,
        inject_speakers: bool = True,
        use_pcm_transport: bool = False,
        host_reload_at_s: Optional[float] = None,
        pcm_disable_at_s: Optional[float] = None,
        pcm_warmup_timeout_s: float = 25.0,
    ):
        if len(guest_names) != len(guest_audio_files):
            raise ValueError("guest_names and guest_audio_files must have the same length")

        self.guest_names = guest_names
        self.guest_audio_files = guest_audio_files
        self.speaker_windows = sorted(speaker_windows, key=lambda w: w.start)
        self.record_duration = record_duration
        self.inject_speakers = inject_speakers
        self.use_pcm_transport = use_pcm_transport
        # Schedule a host_page.reload() at ``host_reload_at_s`` seconds from
        # the moment guests finish joining. Used by
        # test_per_speaker_degraded_mode_on_extension_reload to simulate MV3
        # service-worker eviction (extension stops sending PCM; watchdog
        # fires; live transcriber falls back to mixed-path).
        self.host_reload_at_s = host_reload_at_s
        # Schedule a ``bridge.set_pcm_transport_enabled(False)`` broadcast at
        # ``pcm_disable_at_s`` seconds from the moment PCM flow is confirmed.
        # Cleaner than host_reload: extension disposes its AudioWorklet but
        # the WebSocket + Jitsi conference stay up, so the meeting is NOT
        # torn down and the watchdog fires on a genuine PCM-active→silent
        # transition (pcm_ever_seen=True → transition marker is emitted).
        self.pcm_disable_at_s = pcm_disable_at_s
        # How long to poll PerSpeakerIngest.pcm_ever_seen before giving up
        # when ``use_pcm_transport=True``. If no PCM arrives in this window,
        # the result's ``first_pcm_seen`` stays False and degraded-mode
        # simulation is skipped (nothing to degrade from).
        self.pcm_warmup_timeout_s = pcm_warmup_timeout_s

        # Pre-made room URL (skips SSO login + create_public_room). Supply
        # via env when you want to reuse an existing public room on any
        # KTalk subdomain — e.g. `app.ktalk.ru` or a user's personal
        # workspace. Creds are optional in this mode.
        self._room_url_override = os.environ.get("KTALK_ROOM_URL", "").strip()
        if self._room_url_override:
            self._login = ""
            self._password = ""
        else:
            self._login = os.environ["SSO_LOGIN"]
            self._password = os.environ["SSO_PASSWORD"]

        self._transcriber = None
        self._transcriber_thread: Optional[threading.Thread] = None
        self._transcript_lines: list[str] = []
        self._debug_state: dict = {}
        self._meeting_path: Optional[Path] = None
        # Ring-buffer the host page's console output so long-running tests
        # do not accumulate unbounded diagnostic lines.
        self._browser_console_lines: deque[str] = deque(maxlen=500)
        self._participants_preflight: list[dict] = []

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def run(self) -> E2ETestResult:
        """Run the full e2e test and return the collected result."""
        errors: list[str] = []
        start_time = time.time()
        first_pcm_seen = False

        with sync_playwright() as playwright:
            # --- Host browser (extension + silence) ---
            host_context, host_page = launch_host_browser(playwright, _SILENCE_WAV)

            # Capture all console.* from the host page so extension-side
            # lifecycle events (AudioContext state, worklet load, attach
            # counts) surface in pytest -s output when things go wrong.
            def _on_console(msg):
                try:
                    line = f"[{msg.type}] {msg.text}"
                except Exception:
                    line = "[?] <unreadable console msg>"
                self._browser_console_lines.append(line)
            host_page.on("console", _on_console)

            # --- Room URL: either pre-made (public) or create via SSO ---
            try:
                if self._room_url_override:
                    room_url = self._room_url_override
                    print(f"[e2e] Using pre-made room URL: {room_url}")
                    # Host still needs to be IN the room for the extension's
                    # $conf to populate and PCM transport to attach to the
                    # remote participants' streams. Join as "Host" via the
                    # same public-room prejoin flow that guests use.
                    guest_join_room(host_page, room_url, "Host")
                    print("[e2e] Host joined pre-made room")
                else:
                    host_page.goto("https://example.ktalk.ru", wait_until="domcontentloaded", timeout=20_000)
                    kontur_login(host_page, self._login, self._password)
                    room_url = create_public_room(host_page)
                    print(f"[e2e] Room created: {room_url}")
            except Exception as exc:
                errors.append(f"Host setup failed: {exc}")
                host_context.close()
                return E2ETestResult(errors=errors)

            # --- Start transcriber BEFORE guests join ---
            # This ensures the bridge is ready when the extension connects.
            self._start_transcriber()

            # --- Guest browsers (timed audio files handle who speaks when) ---
            guest_browsers = []
            for name, audio_file in zip(self.guest_names, self.guest_audio_files):
                try:
                    browser, context, page = launch_guest_browser(playwright, audio_file)
                    guest_join_room(page, room_url, name)
                    guest_browsers.append((browser, context))
                    print(f"[e2e] Guest '{name}' joined")
                except Exception as exc:
                    errors.append(f"Guest '{name}' setup failed: {exc}")

            # --- Inject speaker signals via WebSocket ---
            # Jitsi dominant speaker detection doesn't work with Chromium fake
            # audio, so we inject speaker identity directly into the bridge.
            if self.inject_speakers and self.speaker_windows:
                self._speaker_injector_thread = threading.Thread(
                    target=self._run_speaker_injector, daemon=True,
                )
                self._speaker_injector_thread.start()

            print(f"[e2e] All guests joined. Recording for {self.record_duration}s...")

            # --- Preflight + wait for first PCM chunk (if PCM path is on) ---
            # Preflight distinguishes "Jitsi SFU isn't relaying fake audio
            # to host as remote tracks" from "worklet never starts" — run
            # it concurrently with the PCM poll so we don't add 5 s of
            # serial sleep to every run.
            if self.use_pcm_transport:
                first_pcm_seen = self._wait_with_preflight(
                    host_page, self.pcm_warmup_timeout_s
                )

            if self.pcm_disable_at_s is not None and first_pcm_seen:
                # The mid-meeting PCM disable scenario depended on a
                # server-authoritative gate broadcast that no longer
                # exists; per-speaker is the only ingest path now.
                # Skip the disable phase and just record straight through.
                remainder = self.record_duration
                if remainder > 0:
                    time.sleep(remainder)
            # --- Optional mid-meeting host reload (legacy degraded-mode) ---
            # Kept for backwards compatibility. host_page.reload() tears down
            # the Jitsi conference as a side effect, which creates a meeting
            # boundary and resets pcm_ever_seen — prefer pcm_disable_at_s.
            elif self.host_reload_at_s is not None:
                pre = min(self.host_reload_at_s, self.record_duration)
                time.sleep(pre)
                print(f"[e2e] Reloading host page at t={pre:.0f}s "
                      f"(simulates extension PCM stop)")
                try:
                    host_page.reload(wait_until="domcontentloaded", timeout=20_000)
                    print("[e2e] Host page reloaded")
                except Exception as exc:
                    errors.append(f"Host reload failed: {exc}")
                remainder = self.record_duration - pre
                if remainder > 0:
                    time.sleep(remainder)
            else:
                time.sleep(self.record_duration)

            # --- Collect extension debug state from host page ---
            self._debug_state = self._collect_debug_state(host_page)

            # --- Stop transcriber ---
            self._stop_transcriber()

            # --- Cleanup ---
            for browser, context in guest_browsers:
                try:
                    context.close()
                    browser.close()
                except Exception:
                    pass
            try:
                host_context.close()
            except Exception:
                pass

        duration_seconds = time.time() - start_time
        speakers_found = _parse_speakers(self._transcript_lines)

        return E2ETestResult(
            transcript_lines=self._transcript_lines,
            speakers_found=speakers_found,
            duration_seconds=duration_seconds,
            errors=errors,
            debug_state=self._debug_state,
            meeting_path=self._meeting_path,
            first_pcm_seen=first_pcm_seen,
            browser_console_lines=list(self._browser_console_lines),
            participants_preflight=self._participants_preflight,
        )

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    @staticmethod
    def _safe_evaluate(page, js: str, tag: str, default):
        """Run ``page.evaluate(js)`` and return its result, or ``default``
        on any navigation/connection error. Prints a one-line failure
        note so the caller sees why the snapshot is empty.
        """
        try:
            return page.evaluate(js)
        except Exception as exc:
            print(f"[e2e] {tag} evaluate failed: {exc}", flush=True)
            return default

    def _snapshot_participants(self, host_page) -> list[dict]:
        """One-shot snapshot of ``conf._participantsMap`` shaped as a
        list of ``{pid, local, displayName, hasStream, streamActive}``.
        """
        snapshot = self._safe_evaluate(
            host_page,
            """() => {
                const pm = window.$conf && window.$conf._participantsMap;
                if (!pm || !pm.forEach) return [];
                const out = [];
                pm.forEach((p, key) => {
                    const stream = p && p._audioTrack$ && p._audioTrack$.value
                        && p._audioTrack$.value.jitsiTrack
                        && p._audioTrack$.value.jitsiTrack.stream;
                    out.push({
                        key,
                        pid: p && p._id,
                        local: !!(p && p.local),
                        displayName: (p && (p._displayName || p.name)) || null,
                        hasStream: !!stream,
                        streamActive: !!(stream && stream.active),
                    });
                });
                return out;
            }""",
            tag="participants preflight",
            default=[],
        )
        return snapshot if isinstance(snapshot, list) else []

    def _dump_browser_console(self, tag: str, n: int = 60) -> None:
        """Print the last N captured browser console lines to stderr."""
        if not self._browser_console_lines:
            print(f"[e2e][{tag}] no browser console lines captured", flush=True)
            return
        print(f"[e2e][{tag}] last {n} browser console lines:", flush=True)
        tail = list(self._browser_console_lines)[-n:]
        for line in tail:
            print(f"  {line}", flush=True)

    def _wait_with_preflight(self, host_page, timeout_s: float) -> bool:
        """Poll PerSpeakerIngest.pcm_ever_seen and take a one-shot
        preflight snapshot of remote tracks during the same window —
        no extra serial sleep before the poll starts.

        Preflight is taken ~2 s into the wait (Jitsi has already had the
        guest-join time on top). Failing tests get both the snapshot and
        the browser console tail for diagnosis.
        """
        if self._transcriber is None:
            return False
        ingest = getattr(self._transcriber, "_per_speaker_ingest", None)
        if ingest is None:
            print("[e2e] PCM wait skipped — no PerSpeakerIngest on transcriber", flush=True)
            return False

        preflight_deadline = time.time() + 2.0
        warmup_deadline = time.time() + timeout_s
        took_preflight = False

        while time.time() < warmup_deadline:
            if not took_preflight and time.time() >= preflight_deadline:
                self._participants_preflight = self._snapshot_participants(host_page)
                print(
                    f"[e2e] participants preflight: {self._participants_preflight}",
                    flush=True,
                )
                took_preflight = True
            if ingest.pcm_ever_seen:
                waited = timeout_s - (warmup_deadline - time.time())
                print(f"[e2e] PCM first-chunk observed after {waited:.1f}s", flush=True)
                if not took_preflight:
                    self._participants_preflight = self._snapshot_participants(host_page)
                return True
            time.sleep(0.2)

        print(
            f"[e2e] WARNING: no PCM chunk arrived within {timeout_s:.0f}s — "
            "degraded-mode simulation will be skipped",
            flush=True,
        )
        if self._participants_preflight:
            remote_with_stream = [
                p for p in self._participants_preflight
                if not p.get("local") and p.get("hasStream")
            ]
            print(
                f"[e2e] preflight: {len(self._participants_preflight)} participants, "
                f"{len(remote_with_stream)} remote with stream",
                flush=True,
            )
        self._dump_browser_console("pcm-wait-timeout")
        return False

    def _run_speaker_injector(self) -> None:
        """Send speaker_update messages to the bridge on a schedule.

        Connects to the bridge WebSocket and sends the same messages the Chrome
        extension would send, but with deterministic timing based on
        ``self.speaker_windows``.
        """
        import websocket  # websocket-client (sync)

        time.sleep(2)  # let bridge WebSocket accept connections

        try:
            # Without an Origin header, as the Python daemons connect: the
            # bridge admits only conference hosts, the webapp, and no Origin,
            # and websocket-client would send http://localhost:8766.
            ws = websocket.create_connection(_BRIDGE_WS_URL, timeout=5, suppress_origin=True)
        except Exception as exc:
            print(f"[e2e] Speaker injector failed to connect: {exc}", file=sys.stderr)
            return

        print("[e2e] Speaker injector connected to bridge")
        # Start seq at a high base so our injector always takes precedence
        # over the Chrome extension's own speaker_update messages
        seq = 1_000_000
        t0 = time.time()

        try:
            for window in self.speaker_windows:
                # Wait until the window starts
                wait = window.start - (time.time() - t0)
                if wait > 0:
                    time.sleep(wait)

                print(f"[e2e] Injecting speaker: {window.guest_name} (t={window.start:.0f}-{window.end:.0f}s)")

                # Send speaker updates every 500ms for the duration of the window
                while (time.time() - t0) < window.end:
                    seq += 1
                    msg = json.dumps({
                        "type": "speaker_update",
                        "speaker": {"name": window.guest_name, "id": f"fake_{window.guest_name}", "isLocal": False},
                        "timestamp": time.time() * 1000,
                        "seq": seq,
                    })
                    ws.send(msg)
                    time.sleep(0.5)

                # Send null speaker (nobody speaking) at end of window
                seq += 1
                ws.send(json.dumps({
                    "type": "speaker_update",
                    "speaker": None,
                    "timestamp": time.time() * 1000,
                    "seq": seq,
                }))
        except Exception as exc:
            print(f"[e2e] Speaker injector error: {exc}", file=sys.stderr)
        finally:
            ws.close()
            print("[e2e] Speaker injector done")

    def _collect_debug_state(self, host_page) -> dict:
        """Read noise gate debug state from the extension's debug panel."""
        state = self._safe_evaluate(
            host_page,
            """() => {
                const panel = document.querySelector('.ktalk-debug-panel');
                if (!panel) return {};
                const rows = {};
                panel.querySelectorAll('.ktalk-debug-row').forEach(row => {
                    const key = row.querySelector('.ktalk-debug-key')?.textContent;
                    const value = row.querySelector('.ktalk-debug-value')?.textContent;
                    if (key) rows[key] = value;
                });
                return rows;
            }""",
            tag="debug state",
            default={},
        ) or {}
        if state:
            print(f"[e2e] Debug state: {state}")
        return state

    def _start_transcriber(self) -> None:
        """Import LiveTranscriber, find BlackHole device, start in daemon thread."""
        import traceback
        sys.path.insert(0, str(Path(__file__).parent.parent.parent))

        from recog.live import LiveTranscriber, SingleDeviceCapture

        blackhole_device = SingleDeviceCapture.find_blackhole()
        print(f"[e2e] BlackHole device: {blackhole_device}")

        self._transcriber = LiveTranscriber(
            system_device=blackhole_device,
            use_ktalk=True,
        )

        self._transcriber_error: Optional[str] = None

        def _run():
            try:
                self._transcriber.run()
            except Exception as exc:
                self._transcriber_error = f"{exc}\n{traceback.format_exc()}"
                print(f"[e2e] Transcriber error: {self._transcriber_error}", file=sys.stderr)

        self._transcriber_thread = threading.Thread(target=_run, daemon=True)
        self._transcriber_thread.start()

        # Wait for model to load and transcriber to start
        for i in range(30):
            time.sleep(1)
            if self._transcriber.running:
                print(f"[e2e] Transcriber ready after {i+1}s")
                break
        else:
            print("[e2e] WARNING: Transcriber not ready after 30s")

    def _stop_transcriber(self) -> None:
        """Signal the transcriber to stop and collect transcript_lines."""
        if self._transcriber is not None:
            print(f"[e2e] Stopping transcriber. Lines so far: {len(self._transcriber.transcript_lines)}")
            # Capture meeting_path before stop_session clears it.
            storage = getattr(self._transcriber, "_meeting_storage", None)
            if storage is not None:
                self._meeting_path = getattr(storage, "current_meeting_path", None)
            self._transcriber.running = False

        if self._transcriber_thread is not None:
            self._transcriber_thread.join(timeout=10)

        if self._transcriber is not None:
            self._transcript_lines = list(self._transcriber.transcript_lines)
            print(f"[e2e] Final transcript lines: {len(self._transcript_lines)}")
            if hasattr(self, '_transcriber_error') and self._transcriber_error:
                print(f"[e2e] Transcriber had error: {self._transcriber_error}")


# ------------------------------------------------------------------
# Module-level helpers
# ------------------------------------------------------------------

_SPEAKER_RE = re.compile(r"\*\*(.+?)\*\*")


def _parse_speakers(lines: list[str]) -> set[str]:
    """Extract speaker names from markdown-formatted transcript lines.

    Expected line format::

        \\n**Speaker** *[HH:MM:SS]*: text\\n
    """
    speakers: set[str] = set()
    for line in lines:
        for match in _SPEAKER_RE.finditer(line):
            name = match.group(1).strip()
            if name and name != "...":
                speakers.add(name)
    return speakers
