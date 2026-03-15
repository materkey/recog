#!/usr/bin/env python3
"""Launch guest browsers for speaker-noise detection research.

Launches 2 guest Chromium browsers with fake audio:
  - Guest "Speaker" plays speech WAV
  - Guest "Rustler" plays noise WAV (pink noise / rustling)

Both join the specified KTalk room URL. Keep this script running while
probing APIs from the host browser.

Usage:
    python tests/e2e/launch_research_guests.py <room_url> [noise_type]

    noise_type: pink (default), clicks, rustling
"""
import sys
import time
from pathlib import Path
from playwright.sync_api import sync_playwright

_FIXTURES = Path(__file__).parent / "fixtures"

_FAKE_AUDIO_FLAGS = [
    "--use-fake-device-for-media-stream",
    "--use-fake-ui-for-media-stream",
]


def launch_guest(playwright, audio_file: str, room_url: str, name: str):
    """Launch a guest browser with fake audio and join the room."""
    args = _FAKE_AUDIO_FLAGS + [
        f"--use-file-for-fake-audio-capture={audio_file}",
    ]
    browser = playwright.chromium.launch(headless=False, args=args)
    context = browser.new_context()
    page = context.new_page()

    print(f"[research] Launching guest '{name}' with audio: {Path(audio_file).name}")
    page.goto(room_url, wait_until="domcontentloaded", timeout=20_000)

    # Enter guest name
    name_input = page.locator(
        "input[placeholder*='Adam Smith' i], "
        "input[placeholder*='имя' i], "
        "input[placeholder*='name' i], "
        "input[name='displayName']"
    ).first
    name_input.wait_for(state="visible", timeout=15_000)
    name_input.fill(name)

    # Click Continue
    continue_btn = page.locator(
        "button:has-text('Continue'), "
        "button:has-text('Продолжить')"
    ).first
    continue_btn.click()

    # Click Join
    join_btn = page.locator(
        "button:has-text('Join the conference'), "
        "button:has-text('Присоединиться к конференции'), "
        "button:has-text('Присоединиться')"
    ).first
    join_btn.wait_for(state="visible", timeout=15_000)
    join_btn.click()

    page.wait_for_url("**/example.ktalk.ru/**", timeout=15_000)
    print(f"[research] Guest '{name}' joined the room")

    return browser, context, page


def main():
    if len(sys.argv) < 2:
        print("Usage: python launch_research_guests.py <room_url> [noise_type]")
        print("  noise_type: pink (default), clicks, rustling")
        sys.exit(1)

    room_url = sys.argv[1]
    noise_type = sys.argv[2] if len(sys.argv) > 2 else "pink"

    noise_files = {
        "pink": str(_FIXTURES / "noise_pink.wav"),
        "clicks": str(_FIXTURES / "noise_clicks.wav"),
        "rustling": str(_FIXTURES / "noise_rustling.wav"),
    }
    noise_file = noise_files.get(noise_type)
    if not noise_file:
        print(f"Unknown noise type: {noise_type}. Choose: {list(noise_files.keys())}")
        sys.exit(1)

    speech_file = str(_FIXTURES / "speech_reference.wav")

    print(f"[research] Room URL: {room_url}")
    print(f"[research] Speech: speech_reference.wav")
    print(f"[research] Noise: noise_{noise_type}.wav")
    print()

    with sync_playwright() as playwright:
        browsers = []
        try:
            # Launch speech guest
            b1, c1, p1 = launch_guest(playwright, speech_file, room_url, "Speaker")
            browsers.append((b1, c1))

            time.sleep(3)  # Give first guest time to settle

            # Launch noise guest
            b2, c2, p2 = launch_guest(playwright, noise_file, room_url, "Rustler")
            browsers.append((b2, c2))

            print()
            print("[research] Both guests joined. Press Ctrl+C to stop...")
            print("[research] Now probe APIs from the host browser.")
            print()

            # Keep running until interrupted
            while True:
                time.sleep(1)

        except KeyboardInterrupt:
            print("\n[research] Shutting down guests...")
        finally:
            for browser, context in browsers:
                try:
                    context.close()
                    browser.close()
                except Exception:
                    pass
            print("[research] Done")


if __name__ == "__main__":
    main()
