"""Launch N fake-audio guests into a KTalk meeting room.

Reuses WAV fixtures from ``tests/e2e/fixtures/``. Contains no credentials,
account data, or hard-coded room URLs — everything comes from CLI flags.

Usage:

    # Single guest (baseline / P2P check):
    ./venv-gigaam/bin/python tests/e2e/research/launch_guests.py \\
        --guest ProbeAlice=speech_reference.wav \\
        https://<your-subdomain>.ktalk.ru/<room-id>

    # Two guests (overlap / spectral-signature check):
    ./venv-gigaam/bin/python tests/e2e/research/launch_guests.py \\
        --guest ProbeAlice=speech_reference.wav \\
        --guest ProbeBob=noise_pink.wav \\
        https://<your-subdomain>.ktalk.ru/<room-id>

Guests keep playing until the process is killed (Ctrl+C).

Prerequisites:
    - Extension built: ``cd kontur-diarize/ktalk-speaker-extension && npm run build``
    - Playwright installed: ``./venv-gigaam/bin/playwright install chromium``
    - You have already created the room in your browser (any KTalk workspace
      on app.ktalk.ru or a corporate subdomain works) and enabled public-mode.
"""
import argparse
import sys
import time
from pathlib import Path
from playwright.sync_api import sync_playwright

PROJECT_ROOT = Path(__file__).resolve().parents[3]
FIXTURES_DIR = PROJECT_ROOT / "tests" / "e2e" / "fixtures"

FAKE_AUDIO_FLAGS = [
    "--use-fake-device-for-media-stream",
    "--use-fake-ui-for-media-stream",
]


def parse_guest(spec: str) -> tuple[str, Path]:
    if "=" not in spec:
        raise ValueError(f"--guest expects NAME=WAV_PATH, got {spec!r}")
    name, wav_str = spec.split("=", 1)
    wav = Path(wav_str)
    if not wav.is_absolute():
        # Try fixtures dir first, then repo-relative
        candidate = FIXTURES_DIR / wav.name
        wav = candidate if candidate.exists() else PROJECT_ROOT / wav
    if not wav.exists():
        raise FileNotFoundError(f"WAV not found: {wav}")
    return name, wav


def join_guest(pw, name: str, wav: Path, room_url: str):
    args = FAKE_AUDIO_FLAGS + [f"--use-file-for-fake-audio-capture={wav}"]
    browser = pw.chromium.launch(headless=False, args=args)
    page = browser.new_context().new_page()
    print(f"[{name}] navigate {wav.name}", flush=True)
    page.goto(room_url, wait_until="commit", timeout=60_000)

    name_selector = (
        "input[placeholder*='Adam Smith' i], "
        "input[placeholder*='name' i], "
        "input[placeholder*='имя' i], "
        "input[name='displayName']"
    )
    page.locator(name_selector).first.wait_for(state="visible", timeout=30_000)
    page.locator(name_selector).first.fill(name)

    page.locator(
        "button:has-text('Continue'), button:has-text('Продолжить')"
    ).first.click()

    # Ensure mic is on (some rooms open with mic muted).
    try:
        mic_on = page.locator(
            "button[aria-label*='Turn on microphone' i], "
            "button[aria-label*='Включить микрофон' i]"
        ).first
        mic_on.wait_for(state="visible", timeout=3000)
        mic_on.click()
    except Exception:
        pass  # already on

    page.locator(
        "button:has-text('Join the conference'), "
        "button:has-text('Присоединиться к конференции'), "
        "button:has-text('Присоединиться')"
    ).first.click()

    try:
        page.wait_for_selector(
            "button[aria-label*='Meeting settings' i], "
            "button[aria-label*='microphone' i]",
            timeout=20_000,
            state="visible",
        )
    except Exception as exc:
        print(f"[{name}] meeting UI not detected: {exc}", flush=True)
    print(f"[{name}] joined", flush=True)
    return browser


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("room_url", help="KTalk room URL (with path), e.g. https://sub.ktalk.ru/roomid")
    parser.add_argument(
        "--guest",
        action="append",
        required=True,
        metavar="NAME=WAV_PATH",
        help="Guest definition. WAV_PATH may be a fixture filename (looked up in tests/e2e/fixtures/), "
             "a repo-relative path, or an absolute path. Repeat --guest for more than one guest.",
    )
    args = parser.parse_args()

    guests = [parse_guest(spec) for spec in args.guest]

    with sync_playwright() as pw:
        browsers = []
        for name, wav in guests:
            try:
                browsers.append(join_guest(pw, name, wav, args.room_url))
            except Exception as exc:
                print(f"[{name}] FAILED: {exc}", flush=True)
            time.sleep(3)
        print(
            f"[probe] {len(browsers)}/{len(guests)} guests joined; Ctrl+C to exit.",
            flush=True,
        )
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
        finally:
            for browser in browsers:
                try:
                    browser.close()
                except Exception:
                    pass

    return 0 if browsers else 1


if __name__ == "__main__":
    sys.exit(main())
