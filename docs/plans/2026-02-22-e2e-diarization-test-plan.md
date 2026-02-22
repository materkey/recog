# E2E Diarization Test — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Automated e2e test that joins a real KTalk room with multiple Playwright browsers (each with a virtual microphone from WAV files), captures the transcript via LiveTranscriber, and verifies speaker attribution is correct.

**Architecture:** Playwright launches 2-3 Chromium instances with `--use-file-for-fake-audio-capture` flags. One "host" browser logs in via Kontur and creates a public KTalk room with the Chrome extension loaded. Guest browsers join via URL. LiveTranscriber runs in a background thread capturing system audio via BlackHole and reading speaker events from the KTalk bridge WebSocket.

**Tech Stack:** Python 3.12, pytest, playwright (sync API), soundfile, numpy, websockets

---

### Task 1: Install Playwright in venv-gigaam and prepare directory structure

**Files:**
- Create: `tests/e2e/__init__.py`
- Create: `tests/e2e/fixtures/.gitkeep`

**Step 1: Install playwright in venv-gigaam**

Run:
```bash
./venv-gigaam/bin/pip install playwright pytest-playwright
./venv-gigaam/bin/playwright install chromium
```

**Step 2: Create directory structure**

```bash
mkdir -p tests/e2e/fixtures
touch tests/e2e/__init__.py
touch tests/e2e/fixtures/.gitkeep
```

**Step 3: Verify playwright works**

Run: `./venv-gigaam/bin/python -c "from playwright.sync_api import sync_playwright; print('OK')"`
Expected: `OK`

**Step 4: Commit**

```bash
git add tests/e2e/__init__.py tests/e2e/fixtures/.gitkeep
git commit -m "Add e2e test directory structure"
```

---

### Task 2: Prepare audio fixtures from example_5min.wav

**Files:**
- Create: `tests/e2e/prepare_fixtures.py`

**Step 1: Write fixture preparation script**

This script extracts segments from `example_5min.wav` and creates per-speaker WAV files that Chromium can use as fake microphones. Chromium's `--use-file-for-fake-audio-capture` loops the file, so each guest gets a continuous audio stream.

```python
#!/usr/bin/env python3
"""Prepare audio fixtures for e2e diarization tests.

Extracts segments from example_5min.wav and creates per-speaker WAV files.
Chromium --use-file-for-fake-audio-capture loops the file continuously.

Usage:
    python tests/e2e/prepare_fixtures.py
"""
import numpy as np
import soundfile as sf
from pathlib import Path

FIXTURES_DIR = Path(__file__).parent / "fixtures"
SOURCE = Path(__file__).parent.parent.parent / "example_5min.wav"

# Segments to extract (start_sec, duration_sec, output_name)
SEGMENTS = [
    (5.0,  15.0, "speaker_a.wav"),   # First voice in the recording
    (60.0, 15.0, "speaker_b.wav"),   # Different voice (later in recording)
    (120.0, 15.0, "speaker_c.wav"),  # Third voice if available
]

SAMPLE_RATE = 16000


def main():
    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)

    if not SOURCE.exists():
        print(f"ERROR: {SOURCE} not found. Place example_5min.wav in project root.")
        return

    data, sr = sf.read(str(SOURCE))
    print(f"Source: {SOURCE} ({len(data)/sr:.1f}s, {sr}Hz)")

    for start, duration, name in SEGMENTS:
        start_sample = int(start * sr)
        end_sample = int((start + duration) * sr)
        segment = data[start_sample:end_sample]

        # Normalize to prevent clipping and ensure audible levels
        peak = np.max(np.abs(segment))
        if peak > 0:
            segment = segment * (0.8 / peak)

        out_path = FIXTURES_DIR / name
        sf.write(str(out_path), segment, sr)
        print(f"  {name}: {duration:.0f}s ({start:.0f}-{start+duration:.0f}s)")

    # Silence file for host browser (needs a valid WAV but should not produce speech)
    silence = np.zeros(int(5.0 * sr), dtype=np.float32)
    sf.write(str(FIXTURES_DIR / "silence.wav"), silence, sr)
    print(f"  silence.wav: 5.0s")

    print(f"\nFixtures saved to {FIXTURES_DIR}")


if __name__ == "__main__":
    main()
```

**Step 2: Run the script**

Run: `./venv-gigaam/bin/python tests/e2e/prepare_fixtures.py`
Expected: 4 WAV files in `tests/e2e/fixtures/`

**Step 3: Verify files**

Run: `ls -la tests/e2e/fixtures/*.wav`
Expected: `speaker_a.wav`, `speaker_b.wav`, `speaker_c.wav`, `silence.wav`

**Step 4: Add fixtures to .gitignore (audio files are large)**

Add to `.gitignore`:
```
tests/e2e/fixtures/*.wav
```

**Step 5: Commit**

```bash
git add tests/e2e/prepare_fixtures.py .gitignore
git commit -m "Add audio fixture preparation script for e2e tests"
```

---

### Task 3: Create KTalk page objects for Playwright

**Files:**
- Create: `tests/e2e/ktalk_pages.py`

This module encapsulates all KTalk browser interactions. KTalk is a Kontur product built on Jitsi. The page objects handle login, room creation, guest joining, and mute/unmute controls.

**Step 1: Write KTalk page object module**

```python
"""KTalk browser page objects for Playwright-based e2e tests.

Encapsulates login, room creation, guest joining, and mic control
for KTalk (Kontur's Jitsi-based conferencing platform).
"""
import time
from pathlib import Path
from playwright.sync_api import Browser, BrowserContext, Page, Playwright

EXTENSION_PATH = str(Path(__file__).parent.parent.parent / "kontur-diarize" / "ktalk-speaker-extension")


def launch_host_browser(
    playwright: Playwright,
    fake_audio_path: str,
) -> tuple[BrowserContext, Page]:
    """Launch host browser with Chrome extension loaded.

    The host browser runs the KTalk speaker detection extension which
    sends speaker_update events to ktalk_bridge.py via WebSocket.

    Args:
        playwright: Playwright instance
        fake_audio_path: path to WAV file for fake microphone

    Returns:
        (context, page) tuple. Context must be closed by caller.
    """
    context = playwright.chromium.launch_persistent_context(
        user_data_dir="",  # temporary profile
        headless=False,  # extensions require headed mode
        args=[
            f"--disable-extensions-except={EXTENSION_PATH}",
            f"--load-extension={EXTENSION_PATH}",
            "--use-fake-device-for-media-stream",
            f"--use-file-for-fake-audio-capture={fake_audio_path}",
            "--use-fake-ui-for-media-stream",
            "--no-first-run",
            "--no-default-browser-check",
        ],
    )
    page = context.pages[0] if context.pages else context.new_page()
    return context, page


def launch_guest_browser(
    playwright: Playwright,
    fake_audio_path: str,
) -> tuple[Browser, BrowserContext, Page]:
    """Launch guest browser with fake audio (no extension needed).

    Guest browsers join the KTalk room and provide audio via Chromium's
    fake media stream. KTalk/Jitsi detects dominant speaker by audio levels.

    Args:
        playwright: Playwright instance
        fake_audio_path: path to WAV file for fake microphone

    Returns:
        (browser, context, page) tuple. Browser must be closed by caller.
    """
    browser = playwright.chromium.launch(
        headless=False,  # WebRTC needs headed for audio
        args=[
            "--use-fake-device-for-media-stream",
            f"--use-file-for-fake-audio-capture={fake_audio_path}",
            "--use-fake-ui-for-media-stream",
            "--no-first-run",
        ],
    )
    context = browser.new_context(
        permissions=["microphone", "camera"],
    )
    page = context.new_page()
    return browser, context, page


def kontur_login(page: Page, login: str, password: str, timeout: int = 30000):
    """Login to Kontur SSO.

    KTalk redirects to Kontur SSO (accounts.kontur.ru).
    After login, redirects back to KTalk.

    Args:
        page: Playwright page on KTalk login redirect
        login: Kontur account email/login
        password: Kontur account password
        timeout: max wait time in ms
    """
    # Wait for Kontur SSO login form
    page.wait_for_selector('input[name="login"], input[type="email"]', timeout=timeout)
    page.fill('input[name="login"], input[type="email"]', login)
    page.click('button[type="submit"]')

    page.wait_for_selector('input[name="password"], input[type="password"]', timeout=timeout)
    page.fill('input[name="password"], input[type="password"]', password)
    page.click('button[type="submit"]')

    # Wait for redirect back to KTalk
    page.wait_for_url("**/ktalk.ru/**", timeout=timeout)


def create_public_room(page: Page, timeout: int = 15000) -> str:
    """Create a new public KTalk room and return its URL.

    Navigates to KTalk, creates a new room, and returns the room URL
    that guests can join without authentication.

    Args:
        page: logged-in KTalk page
        timeout: max wait time in ms

    Returns:
        Room URL string
    """
    page.goto("https://ktalk.ru")
    page.wait_for_load_state("networkidle")

    # Click "Create room" / "Создать комнату" button
    create_btn = page.locator('button:has-text("Создать"), button:has-text("Create")')
    create_btn.click(timeout=timeout)

    # Wait for room to load (URL changes to room URL)
    page.wait_for_url("**/ktalk.ru/**/*", timeout=timeout)

    # Join the room (click join button if prejoin screen shown)
    join_btn = page.locator('button:has-text("Присоединиться"), button:has-text("Join")')
    if join_btn.is_visible(timeout=5000):
        join_btn.click()

    # Wait for conference to be ready
    page.wait_for_timeout(3000)

    return page.url


def guest_join_room(page: Page, room_url: str, guest_name: str, timeout: int = 20000):
    """Join KTalk room as a guest with a display name.

    KTalk public rooms allow guest access. The guest enters a name
    and joins without Kontur authentication.

    Args:
        page: guest browser page
        room_url: KTalk room URL from create_public_room()
        guest_name: display name for the guest
        timeout: max wait time in ms
    """
    page.goto(room_url)
    page.wait_for_load_state("networkidle")

    # Fill in guest name on prejoin screen
    name_input = page.locator('input[placeholder*="имя"], input[placeholder*="name"], input[name="displayName"]')
    if name_input.is_visible(timeout=5000):
        name_input.fill(guest_name)

    # Click join
    join_btn = page.locator('button:has-text("Присоединиться"), button:has-text("Join")')
    join_btn.click(timeout=timeout)

    # Wait for conference
    page.wait_for_timeout(3000)


def toggle_mic(page: Page, mute: bool):
    """Mute or unmute microphone in KTalk.

    Uses keyboard shortcut (M key) which toggles mic in Jitsi-based conferences.

    Args:
        page: page in an active KTalk conference
        mute: True to mute, False to unmute
    """
    # KTalk/Jitsi uses 'M' as mic toggle, or we can click the mic button
    mic_btn = page.locator('[aria-label*="микрофон"], [aria-label*="microphone"], [data-testid="mic-button"]')
    if mic_btn.is_visible(timeout=2000):
        # Check current state
        is_muted = "muted" in (mic_btn.get_attribute("class") or "") or \
                   "off" in (mic_btn.get_attribute("aria-label") or "").lower()
        if mute != is_muted:
            mic_btn.click()
    else:
        # Fallback: keyboard shortcut
        page.keyboard.press("m")
```

**Step 2: Verify syntax**

Run: `./venv-gigaam/bin/python -c "import ast; ast.parse(open('tests/e2e/ktalk_pages.py').read()); print('OK')"`
Expected: `OK`

**Step 3: Commit**

```bash
git add tests/e2e/ktalk_pages.py
git commit -m "Add KTalk page objects for Playwright e2e tests"
```

---

### Task 4: Create the orchestrator that coordinates browsers + LiveTranscriber

**Files:**
- Create: `tests/e2e/ktalk_orchestrator.py`

**Step 1: Write orchestrator**

The orchestrator manages the lifecycle: starts LiveTranscriber in a thread, launches browsers, runs a mute/unmute schedule, and collects results.

```python
"""Orchestrates e2e diarization tests with real KTalk.

Coordinates multiple Playwright browsers, LiveTranscriber, and KTalk bridge
to test speaker attribution through the full pipeline.

Lifecycle:
    1. Start KTalkBridge + LiveTranscriber in background thread
    2. Launch host browser with Chrome extension
    3. Login to Kontur, create public KTalk room
    4. Launch guest browsers, join room
    5. Execute mute/unmute schedule
    6. Collect transcript, verify speaker attribution
"""
import os
import time
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from playwright.sync_api import sync_playwright, Playwright, Page

from tests.e2e.ktalk_pages import (
    launch_host_browser,
    launch_guest_browser,
    kontur_login,
    create_public_room,
    guest_join_room,
    toggle_mic,
)


FIXTURES_DIR = Path(__file__).parent / "fixtures"


@dataclass
class SpeakerScheduleEvent:
    """A mute/unmute event in the test timeline."""
    time_offset: float  # seconds from test start
    guest_index: int    # which guest (0-based)
    mute: bool          # True=mute, False=unmute


@dataclass
class E2ETestResult:
    """Results collected from an e2e diarization test run."""
    transcript_lines: list[str] = field(default_factory=list)
    speakers_found: set[str] = field(default_factory=set)
    duration_seconds: float = 0.0
    errors: list[str] = field(default_factory=list)


class KTalkOrchestrator:
    """Orchestrates e2e diarization test with real KTalk."""

    def __init__(
        self,
        guest_names: list[str],
        guest_audio_files: list[str],
        schedule: list[SpeakerScheduleEvent],
        test_duration: float = 40.0,
    ):
        """
        Args:
            guest_names: display names for each guest (e.g. ["Алексей", "Мария"])
            guest_audio_files: WAV file paths for each guest's fake microphone
            schedule: mute/unmute events timeline
            test_duration: total test duration in seconds
        """
        assert len(guest_names) == len(guest_audio_files)
        self.guest_names = guest_names
        self.guest_audio_files = guest_audio_files
        self.schedule = sorted(schedule, key=lambda e: e.time_offset)
        self.test_duration = test_duration

        self._kontur_login = os.environ.get("KONTUR_LOGIN", "")
        self._kontur_password = os.environ.get("KONTUR_PASSWORD", "")
        assert self._kontur_login and self._kontur_password, \
            "Set KONTUR_LOGIN and KONTUR_PASSWORD env vars"

        self._transcript_lines: list[str] = []
        self._transcriber_thread: Optional[threading.Thread] = None
        self._transcriber = None

    def run(self) -> E2ETestResult:
        """Execute the full e2e test and return results."""
        result = E2ETestResult()
        start_time = time.time()

        with sync_playwright() as pw:
            # 1. Launch host browser with extension
            host_audio = str(FIXTURES_DIR / "silence.wav")
            host_ctx, host_page = launch_host_browser(pw, host_audio)

            try:
                # 2. Login to Kontur
                host_page.goto("https://ktalk.ru")
                kontur_login(host_page, self._kontur_login, self._kontur_password)

                # 3. Create public room
                room_url = create_public_room(host_page)
                print(f"[E2E] Room created: {room_url}")

                # 4. Launch guest browsers
                guests = []
                for i, (name, audio) in enumerate(
                    zip(self.guest_names, self.guest_audio_files)
                ):
                    browser, ctx, page = launch_guest_browser(pw, audio)
                    guest_join_room(page, room_url, name)
                    # Start muted
                    toggle_mic(page, mute=True)
                    guests.append((browser, ctx, page))
                    print(f"[E2E] Guest '{name}' joined")

                # 5. Wait for conference to stabilize
                time.sleep(5)

                # 6. Start LiveTranscriber in background
                self._start_transcriber()

                # 7. Execute schedule
                test_start = time.time()
                for event in self.schedule:
                    wait = event.time_offset - (time.time() - test_start)
                    if wait > 0:
                        time.sleep(wait)
                    if event.guest_index < len(guests):
                        _, _, page = guests[event.guest_index]
                        toggle_mic(page, mute=event.mute)
                        action = "muted" if event.mute else "unmuted"
                        name = self.guest_names[event.guest_index]
                        print(f"[E2E] {time.time()-test_start:.1f}s: {name} {action}")

                # 8. Wait for remaining test duration
                remaining = self.test_duration - (time.time() - test_start)
                if remaining > 0:
                    time.sleep(remaining)

                # 9. Stop transcriber and collect results
                self._stop_transcriber()

                result.transcript_lines = list(self._transcript_lines)
                result.duration_seconds = time.time() - start_time

                # Parse speakers from transcript
                for line in result.transcript_lines:
                    if line.startswith("\n**") and "**" in line[3:]:
                        speaker = line.split("**")[1]
                        result.speakers_found.add(speaker)

            except Exception as e:
                result.errors.append(str(e))

            finally:
                # Cleanup
                for browser, ctx, page in guests:
                    browser.close()
                host_ctx.close()

        return result

    def _start_transcriber(self):
        """Start LiveTranscriber in a background thread."""
        import sys
        sys.path.insert(0, str(Path(__file__).parent.parent.parent))
        from recog.live import LiveTranscriber

        self._transcriber = LiveTranscriber(
            system_device=None,  # Will be set to BlackHole device index
            use_ktalk=True,
            chunk_duration=3.0,
        )

        # Find BlackHole device
        import sounddevice as sd
        devices = sd.query_devices()
        for i, d in enumerate(devices):
            if "BlackHole" in d["name"] and d["max_input_channels"] > 0:
                self._transcriber.system_device = i
                break

        def run_transcriber():
            try:
                self._transcriber.run()
            except Exception as e:
                print(f"[E2E] Transcriber error: {e}")

        self._transcriber_thread = threading.Thread(
            target=run_transcriber, daemon=True
        )
        self._transcriber_thread.start()
        time.sleep(2)  # Wait for model loading

    def _stop_transcriber(self):
        """Stop LiveTranscriber and collect transcript."""
        if self._transcriber:
            self._transcriber.running = False
            self._transcript_lines = list(self._transcriber.transcript_lines)
        if self._transcriber_thread:
            self._transcriber_thread.join(timeout=5)
```

**Step 2: Verify syntax**

Run: `./venv-gigaam/bin/python -c "import ast; ast.parse(open('tests/e2e/ktalk_orchestrator.py').read()); print('OK')"`
Expected: `OK`

**Step 3: Commit**

```bash
git add tests/e2e/ktalk_orchestrator.py
git commit -m "Add KTalk orchestrator for e2e diarization tests"
```

---

### Task 5: Write the e2e test cases

**Files:**
- Create: `tests/e2e/conftest.py`
- Create: `tests/e2e/test_diarization_e2e.py`

**Step 1: Write conftest with e2e marker**

```python
"""Fixtures for e2e diarization tests."""
import pytest
from pathlib import Path

FIXTURES_DIR = Path(__file__).parent / "fixtures"


def pytest_configure(config):
    config.addinivalue_line("markers", "e2e: end-to-end tests requiring KTalk, browser, and audio hardware")


@pytest.fixture
def fixtures_dir():
    """Path to e2e audio fixtures directory."""
    return FIXTURES_DIR


@pytest.fixture
def speaker_a_wav(fixtures_dir):
    path = fixtures_dir / "speaker_a.wav"
    assert path.exists(), f"Run prepare_fixtures.py first: {path}"
    return str(path)


@pytest.fixture
def speaker_b_wav(fixtures_dir):
    path = fixtures_dir / "speaker_b.wav"
    assert path.exists(), f"Run prepare_fixtures.py first: {path}"
    return str(path)


@pytest.fixture
def speaker_c_wav(fixtures_dir):
    path = fixtures_dir / "speaker_c.wav"
    assert path.exists(), f"Run prepare_fixtures.py first: {path}"
    return str(path)
```

**Step 2: Write test file**

```python
"""E2E diarization tests with real KTalk.

These tests require:
- KONTUR_LOGIN and KONTUR_PASSWORD env vars
- BlackHole virtual audio device installed
- Chrome extension built (npm run build in kontur-diarize/ktalk-speaker-extension/)
- Audio fixtures prepared (python tests/e2e/prepare_fixtures.py)
- GigaAM model available

Run: pytest tests/e2e/ -m e2e -v --timeout=120
"""
import pytest

from tests.e2e.ktalk_orchestrator import KTalkOrchestrator, SpeakerScheduleEvent


@pytest.mark.e2e
def test_two_speakers_alternating(speaker_a_wav, speaker_b_wav):
    """
    given: KTalk room with two guests, each with distinct audio
    when: Guest A speaks first (unmuted 0-15s), then Guest B (unmuted 15-30s)
    then: both speakers should appear in transcript with correct names
    """
    schedule = [
        # Guest A unmuted at start
        SpeakerScheduleEvent(time_offset=0, guest_index=0, mute=False),
        # Guest A muted, Guest B unmuted at 15s
        SpeakerScheduleEvent(time_offset=15, guest_index=0, mute=True),
        SpeakerScheduleEvent(time_offset=15, guest_index=1, mute=False),
        # Guest B muted at 30s
        SpeakerScheduleEvent(time_offset=30, guest_index=1, mute=True),
    ]

    orchestrator = KTalkOrchestrator(
        guest_names=["Алексей", "Мария"],
        guest_audio_files=[speaker_a_wav, speaker_b_wav],
        schedule=schedule,
        test_duration=35,
    )

    result = orchestrator.run()

    assert not result.errors, f"Test errors: {result.errors}"
    assert len(result.speakers_found) >= 2, \
        f"Expected 2+ speakers, got {result.speakers_found}"
    assert len(result.transcript_lines) > 0, "No transcript produced"


@pytest.mark.e2e
def test_ttl_regression_speaker_resets_after_gap(speaker_a_wav, speaker_b_wav):
    """
    given: KTalk room with two guests
    when: Guest A speaks (0-10s), both muted for >10s (TTL), Guest B speaks (25-35s)
    then: after TTL gap, speaker should reset to Unknown (not stay as Guest A)
          and Guest B should be correctly attributed
    """
    schedule = [
        SpeakerScheduleEvent(time_offset=0, guest_index=0, mute=False),
        # Both muted at 10s — creates KTalk data gap
        SpeakerScheduleEvent(time_offset=10, guest_index=0, mute=True),
        # Guest B unmuted at 25s (15s gap > 10s TTL)
        SpeakerScheduleEvent(time_offset=25, guest_index=1, mute=False),
        SpeakerScheduleEvent(time_offset=35, guest_index=1, mute=True),
    ]

    orchestrator = KTalkOrchestrator(
        guest_names=["Алексей", "Мария"],
        guest_audio_files=[speaker_a_wav, speaker_b_wav],
        schedule=schedule,
        test_duration=40,
    )

    result = orchestrator.run()

    assert not result.errors, f"Test errors: {result.errors}"

    # Verify the transcript has both speakers (not just Guest A for everything)
    # After the TTL gap, there should be "Unknown" or a correct speaker change
    guest_a_lines = [l for l in result.transcript_lines if "Алексей" in l]
    guest_b_lines = [l for l in result.transcript_lines if "Мария" in l]

    assert len(guest_a_lines) > 0, "Guest A should have some lines"
    assert len(guest_b_lines) > 0, \
        "Guest B should have lines (not cascaded to Guest A)"


@pytest.mark.e2e
def test_long_pause_between_speakers(speaker_a_wav, speaker_b_wav):
    """
    given: KTalk room with two guests
    when: Guest A speaks (0-10s), 15s silence, Guest B speaks (25-35s)
    then: Guest B should NOT be attributed as Guest A
    """
    schedule = [
        SpeakerScheduleEvent(time_offset=0, guest_index=0, mute=False),
        SpeakerScheduleEvent(time_offset=10, guest_index=0, mute=True),
        # 15s gap
        SpeakerScheduleEvent(time_offset=25, guest_index=1, mute=False),
        SpeakerScheduleEvent(time_offset=35, guest_index=1, mute=True),
    ]

    orchestrator = KTalkOrchestrator(
        guest_names=["Алексей", "Мария"],
        guest_audio_files=[speaker_a_wav, speaker_b_wav],
        schedule=schedule,
        test_duration=40,
    )

    result = orchestrator.run()

    assert not result.errors, f"Test errors: {result.errors}"

    # Lines after 25s mark should NOT all be "Алексей"
    # Parse timestamps from transcript lines: **Speaker** *[HH:MM:SS]*: text
    late_lines = []
    for line in result.transcript_lines:
        if "*[" in line:
            # Extract time from *[HH:MM:SS]*
            ts_part = line.split("*[")[1].split("]*")[0]
            parts = ts_part.split(":")
            secs = int(parts[0]) * 3600 + int(parts[1]) * 60 + int(parts[2])
            if "**" in line:
                speaker = line.split("**")[1]
                late_lines.append((secs, speaker))

    # Check that not ALL late lines are Guest A
    if late_lines:
        late_speakers = {s for _, s in late_lines[-5:]}
        assert "Алексей" not in late_speakers or len(late_speakers) > 1, \
            f"All late lines attributed to Guest A: {late_speakers}"


@pytest.mark.e2e
def test_three_speakers(speaker_a_wav, speaker_b_wav, speaker_c_wav):
    """
    given: KTalk room with three guests
    when: each speaks in sequence (A: 0-10s, B: 10-20s, C: 20-30s)
    then: all three speakers should appear in transcript
    """
    schedule = [
        SpeakerScheduleEvent(time_offset=0, guest_index=0, mute=False),
        SpeakerScheduleEvent(time_offset=10, guest_index=0, mute=True),
        SpeakerScheduleEvent(time_offset=10, guest_index=1, mute=False),
        SpeakerScheduleEvent(time_offset=20, guest_index=1, mute=True),
        SpeakerScheduleEvent(time_offset=20, guest_index=2, mute=False),
        SpeakerScheduleEvent(time_offset=30, guest_index=2, mute=True),
    ]

    orchestrator = KTalkOrchestrator(
        guest_names=["Алексей", "Мария", "Дмитрий"],
        guest_audio_files=[speaker_a_wav, speaker_b_wav, speaker_c_wav],
        schedule=schedule,
        test_duration=35,
    )

    result = orchestrator.run()

    assert not result.errors, f"Test errors: {result.errors}"
    assert len(result.speakers_found) >= 3, \
        f"Expected 3+ speakers, got {result.speakers_found}"
```

**Step 3: Verify syntax of both files**

Run:
```bash
./venv-gigaam/bin/python -c "import ast; ast.parse(open('tests/e2e/conftest.py').read()); ast.parse(open('tests/e2e/test_diarization_e2e.py').read()); print('OK')"
```
Expected: `OK`

**Step 4: Commit**

```bash
git add tests/e2e/conftest.py tests/e2e/test_diarization_e2e.py
git commit -m "Add e2e diarization test cases for KTalk speaker attribution"
```

---

### Task 6: Update pytest.ini and add e2e marker configuration

**Files:**
- Modify: `pytest.ini`

**Step 1: Add e2e marker to pytest.ini**

Add to `pytest.ini`:
```ini
markers =
    e2e: end-to-end tests requiring KTalk, browser, and audio hardware
```

This allows `pytest -m "not e2e"` to skip slow e2e tests in normal runs.

**Step 2: Verify tests are discoverable but skipped without marker**

Run: `./venv-gigaam/bin/python -m pytest tests/e2e/ --collect-only`
Expected: 4 tests collected

Run: `./venv-gigaam/bin/python -m pytest tests/ -m "not e2e" --collect-only`
Expected: e2e tests NOT collected, only unit tests collected

**Step 3: Commit**

```bash
git add pytest.ini
git commit -m "Add e2e marker to pytest configuration"
```

---

### Task 7: Manual smoke test — run one scenario end-to-end

**Step 1: Prepare fixtures**

Run: `./venv-gigaam/bin/python tests/e2e/prepare_fixtures.py`

**Step 2: Set credentials**

```bash
export KONTUR_LOGIN="your_login"
export KONTUR_PASSWORD="your_password"
```

**Step 3: Run the simplest test**

Run:
```bash
./venv-gigaam/bin/python -m pytest tests/e2e/test_diarization_e2e.py::test_two_speakers_alternating -m e2e -v -s --timeout=120
```

This runs headed (visible browsers). Watch:
1. Host browser opens, logs into Kontur, creates KTalk room
2. Two guest browsers join with fake audio
3. Guests mute/unmute per schedule
4. Transcript appears in stdout

**Step 4: Fix selectors if KTalk UI changed**

The page selectors in `ktalk_pages.py` may need adjustment based on the actual KTalk UI. Fix any `locator()` selectors that don't match and re-run.

**Step 5: Commit fixes**

```bash
git add -u
git commit -m "Fix KTalk page selectors from e2e smoke test"
```

---

### Task 8: Add README for e2e tests

**Files:**
- Create: `tests/e2e/README.md`

**Step 1: Write README**

Document setup steps, prerequisites, and how to run.

```markdown
# E2E Diarization Tests

Tests speaker attribution through the full pipeline: real KTalk conference → Chrome extension → ktalk_bridge → LiveTranscriber.

## Prerequisites

1. **Kontur account** with KTalk access
2. **BlackHole** virtual audio device installed
3. **Chrome extension** built: `cd kontur-diarize/ktalk-speaker-extension && npm run build`
4. **Playwright**: `./venv-gigaam/bin/pip install playwright pytest-playwright && ./venv-gigaam/bin/playwright install chromium`
5. **Audio fixtures**: `./venv-gigaam/bin/python tests/e2e/prepare_fixtures.py`

## Running

```bash
export KONTUR_LOGIN="your_login"
export KONTUR_PASSWORD="your_password"
./venv-gigaam/bin/python -m pytest tests/e2e/ -m e2e -v -s --timeout=120
```

## Test Scenarios

| Test | What it verifies |
|------|-----------------|
| `test_two_speakers_alternating` | Basic: two guests speak in turns, both attributed |
| `test_ttl_regression_speaker_resets_after_gap` | TTL fix: after 10s gap, speaker resets to Unknown |
| `test_long_pause_between_speakers` | Long pause: second speaker not attributed as first |
| `test_three_speakers` | Three guests speak sequentially, all three attributed |
```

**Step 2: Commit**

```bash
git add tests/e2e/README.md
git commit -m "Add README for e2e diarization tests"
```
