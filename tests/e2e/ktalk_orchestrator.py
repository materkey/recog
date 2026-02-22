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
    ):
        if len(guest_names) != len(guest_audio_files):
            raise ValueError("guest_names and guest_audio_files must have the same length")

        self.guest_names = guest_names
        self.guest_audio_files = guest_audio_files
        self.speaker_windows = sorted(speaker_windows, key=lambda w: w.start)
        self.record_duration = record_duration

        self._login = os.environ["SSO_LOGIN"]
        self._password = os.environ["SSO_PASSWORD"]

        self._transcriber = None
        self._transcriber_thread: Optional[threading.Thread] = None
        self._transcript_lines: list[str] = []

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def run(self) -> E2ETestResult:
        """Run the full e2e test and return the collected result."""
        errors: list[str] = []
        start_time = time.time()

        with sync_playwright() as playwright:
            # --- Host browser (extension + silence) ---
            host_context, host_page = launch_host_browser(playwright, _SILENCE_WAV)

            # --- Login and create room ---
            try:
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
            self._speaker_injector_thread = threading.Thread(
                target=self._run_speaker_injector, daemon=True,
            )
            self._speaker_injector_thread.start()

            print(f"[e2e] All guests joined. Recording for {self.record_duration}s...")
            time.sleep(self.record_duration)

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
        )

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _run_speaker_injector(self) -> None:
        """Send speaker_update messages to the bridge on a schedule.

        Connects to the bridge WebSocket and sends the same messages the Chrome
        extension would send, but with deterministic timing based on
        ``self.speaker_windows``.
        """
        import websocket  # websocket-client (sync)

        time.sleep(2)  # let bridge WebSocket accept connections

        try:
            ws = websocket.create_connection(_BRIDGE_WS_URL, timeout=5)
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
