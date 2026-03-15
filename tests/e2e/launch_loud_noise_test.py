#!/usr/bin/env python3
"""Launch guests: quiet speech + LOUD noise to reproduce the misattribution bug.

Rustler's noise is much louder than Speaker's speech, so Jitsi should
mark Rustler as the dominant speaker — reproducing the real-world problem.

Usage:
    python tests/e2e/launch_loud_noise_test.py <room_url>
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
    args = _FAKE_AUDIO_FLAGS + [
        f"--use-file-for-fake-audio-capture={audio_file}",
    ]
    browser = playwright.chromium.launch(headless=False, args=args)
    context = browser.new_context()
    page = context.new_page()

    print(f"[test] Launching '{name}' with: {Path(audio_file).name}")
    page.goto(room_url, wait_until="domcontentloaded", timeout=20_000)

    name_input = page.locator(
        "input[placeholder*='Adam Smith' i], "
        "input[placeholder*='имя' i], "
        "input[placeholder*='name' i], "
        "input[name='displayName']"
    ).first
    name_input.wait_for(state="visible", timeout=15_000)
    name_input.fill(name)

    page.locator("button:has-text('Continue'), button:has-text('Продолжить')").first.click()

    join_btn = page.locator(
        "button:has-text('Join the conference'), "
        "button:has-text('Присоединиться к конференции'), "
        "button:has-text('Присоединиться')"
    ).first
    join_btn.wait_for(state="visible", timeout=15_000)
    join_btn.click()

    page.wait_for_url("**/example.ktalk.ru/**", timeout=15_000)
    print(f"[test] '{name}' joined")
    return browser, context, page


def main():
    if len(sys.argv) < 2:
        print("Usage: python launch_loud_noise_test.py <room_url>")
        sys.exit(1)

    room_url = sys.argv[1]
    speech_file = str(_FIXTURES / "speech_quiet.wav")    # quiet speech (peak 0.15)
    noise_file = str(_FIXTURES / "noise_loud.wav")       # LOUD noise (peak 0.90)

    print(f"[test] Room: {room_url}")
    print(f"[test] Speaker: speech_quiet.wav (peak 0.15)")
    print(f"[test] Rustler: noise_loud.wav (peak 0.90)")
    print(f"[test] Noise is 6x louder — Rustler should become dominant speaker")
    print()

    with sync_playwright() as playwright:
        browsers = []
        try:
            b1, c1, p1 = launch_guest(playwright, speech_file, room_url, "Speaker")
            browsers.append((b1, c1))
            time.sleep(3)
            b2, c2, p2 = launch_guest(playwright, noise_file, room_url, "Rustler")
            browsers.append((b2, c2))

            print()
            print("[test] Both joined. Press Ctrl+C to stop.")
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            print("\n[test] Stopping...")
        finally:
            for b, c in browsers:
                try:
                    c.close()
                    b.close()
                except Exception:
                    pass


if __name__ == "__main__":
    main()
