"""KTalk E2E test orchestrator.

Coordinates browser participants, mic scheduling, and live transcription
for end-to-end diarization tests.
"""
import os
import re
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
    toggle_mic,
)

_FIXTURES_DIR = Path(__file__).parent / "fixtures"
_SILENCE_WAV = str(_FIXTURES_DIR / "silence.wav")


@dataclass
class SpeakerScheduleEvent:
    """A scheduled mute/unmute event for a guest participant."""

    time_offset: float  # seconds from conference start
    guest_index: int    # 0-based index into KTalkOrchestrator.guest_names
    mute: bool          # True = mute, False = unmute


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
    more guest browsers (with per-speaker fake audio), joins them all into a
    single room, executes a mute/unmute schedule, and collects the live
    transcript produced by LiveTranscriber.
    """

    def __init__(
        self,
        guest_names: list[str],
        guest_audio_files: list[str],
        schedule: list[SpeakerScheduleEvent],
        test_duration: float = 40.0,
    ):
        if len(guest_names) != len(guest_audio_files):
            raise ValueError("guest_names and guest_audio_files must have the same length")

        self.guest_names = guest_names
        self.guest_audio_files = guest_audio_files
        self.schedule = sorted(schedule, key=lambda e: e.time_offset)
        self.test_duration = test_duration

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
            except Exception as exc:
                errors.append(f"Host setup failed: {exc}")
                host_context.close()
                return E2ETestResult(errors=errors)

            # --- Guest browsers ---
            guest_browsers = []
            guest_pages = []
            for name, audio_file in zip(self.guest_names, self.guest_audio_files):
                try:
                    browser, context, page = launch_guest_browser(playwright, audio_file)
                    guest_join_room(page, room_url, name)
                    # Start guests muted
                    toggle_mic(page, mute=True)
                    guest_browsers.append((browser, context))
                    guest_pages.append(page)
                except Exception as exc:
                    errors.append(f"Guest '{name}' setup failed: {exc}")

            # --- Wait for conference to stabilise ---
            time.sleep(5)

            # --- Start transcriber ---
            self._start_transcriber()

            # --- Execute schedule ---
            schedule_start = time.time()
            for event in self.schedule:
                now = time.time() - schedule_start
                wait = event.time_offset - now
                if wait > 0:
                    time.sleep(wait)

                if event.guest_index < len(guest_pages):
                    try:
                        toggle_mic(guest_pages[event.guest_index], mute=event.mute)
                    except Exception as exc:
                        errors.append(f"Schedule event failed (guest {event.guest_index}, t={event.time_offset}): {exc}")

            # --- Wait out remaining test duration ---
            elapsed = time.time() - schedule_start
            remaining = self.test_duration - elapsed
            if remaining > 0:
                time.sleep(remaining)

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

    def _start_transcriber(self) -> None:
        """Import LiveTranscriber, find BlackHole device, start in daemon thread."""
        import sys
        sys.path.insert(0, str(Path(__file__).parent.parent.parent))

        from recog.live import LiveTranscriber, SingleDeviceCapture

        blackhole_device = SingleDeviceCapture.find_blackhole()

        self._transcriber = LiveTranscriber(
            system_device=blackhole_device,
            use_ktalk=True,
        )

        def _run():
            try:
                self._transcriber.run()
            except Exception:
                pass

        self._transcriber_thread = threading.Thread(target=_run, daemon=True)
        self._transcriber_thread.start()

    def _stop_transcriber(self) -> None:
        """Signal the transcriber to stop and collect transcript_lines."""
        if self._transcriber is not None:
            self._transcriber.running = False

        if self._transcriber_thread is not None:
            self._transcriber_thread.join(timeout=10)

        if self._transcriber is not None:
            self._transcript_lines = list(self._transcriber.transcript_lines)


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
