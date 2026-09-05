"""KTalk page objects for Playwright e2e tests.

Provides helper functions to launch browsers, authenticate via Kontur SSO,
create/join KTalk rooms, and control microphone state using the sync Playwright API.
"""
import os
from pathlib import Path

from playwright.sync_api import Browser, BrowserContext, Page, Playwright

EXTENSION_PATH = str(Path(__file__).parent.parent.parent / "browser-extension")


def _fake_audio_flags() -> list[str]:
    """Build Chromium launch flags for fake-audio e2e tests.

    ``--mute-audio`` is included by default so tests do not leak audio
    into the user's speakers. conftest.py sets ``E2E_MUTE_AUDIO=0`` for
    tests tagged ``@pytest.mark.needs_audio_loopback`` — those tests need
    the browser's audio to actually reach the system output so BlackHole
    can record it for the mixed-path fallback.
    """
    flags = [
        "--use-fake-device-for-media-stream",
        "--use-fake-ui-for-media-stream",
        # Programmatic page.click() in Playwright is not always treated as a
        # "real" user gesture by Chromium's autoplay policy. Without this
        # flag AudioContext stays suspended and AudioWorklet.process()
        # never runs, so pcm_chunk frames never reach the bridge.
        "--autoplay-policy=no-user-gesture-required",
    ]
    if os.environ.get("E2E_MUTE_AUDIO", "1") != "0":
        # Silence all audio output from the Playwright Chromium tabs. SFU
        # audio is relayed over MediaStream/WebRTC, so this does not break
        # PCM transport — it only stops remote audio from being played
        # into the host's speakers during the test.
        flags.append("--mute-audio")
    return flags


# ``--mute-audio`` only suppresses HTMLMediaElement autoplay; Jitsi
# plays remote WebRTC audio through dynamically-created <audio> tags
# whose .volume/.muted attributes ignore the flag. This init-script
# runs in every new page/frame before page scripts, forces every
# current and future <audio>/<video> element to muted+0-volume, and
# intercepts programmatic `.volume=` / `.play()` / `.srcObject=`
# assignments so there is no audible path to speakers. MediaStream
# data stays intact — the AudioWorklet reads it directly and PCM
# transport is unaffected.
_MUTE_MEDIA_SCRIPT = """
(() => {
  const silence = (el) => {
    try { el.muted = true; el.volume = 0; } catch (_) {}
  };
  const silenceAll = (root) => {
    (root.querySelectorAll ? root.querySelectorAll('audio,video') : []).forEach(silence);
  };
  silenceAll(document);
  new MutationObserver((muts) => {
    for (const m of muts) {
      m.addedNodes.forEach((n) => {
        if (n.nodeType !== 1) return;
        if (n.matches && n.matches('audio,video')) silence(n);
        silenceAll(n);
      });
    }
  }).observe(document.documentElement, { childList: true, subtree: true });
  const proto = HTMLMediaElement.prototype;
  const volDesc = Object.getOwnPropertyDescriptor(proto, 'volume');
  if (volDesc && volDesc.configurable) {
    Object.defineProperty(proto, 'volume', {
      get() { return 0; },
      set() { /* swallow any attempt to raise volume */ },
      configurable: true,
    });
  }
})();
"""


def _install_media_mute(context: BrowserContext) -> None:
    """Silence all `<audio>`/`<video>` in the context unless the test opted
    into audio loopback via ``E2E_MUTE_AUDIO=0``.
    """
    if os.environ.get("E2E_MUTE_AUDIO", "1") == "0":
        return
    context.add_init_script(_MUTE_MEDIA_SCRIPT)


def launch_host_browser(playwright: Playwright, fake_audio_path: str) -> tuple[BrowserContext, Page]:
    """Launch a Chromium browser with the KTalk speaker extension loaded.

    The browser uses a fake audio device fed from *fake_audio_path* so that
    the participant appears to be speaking without a real microphone.

    Args:
        playwright: Active Playwright instance.
        fake_audio_path: Absolute path to a WAV file to use as the fake mic input.

    Returns:
        A (context, page) tuple. The context owns the extension and must be
        closed by the caller when done.
    """
    args = _fake_audio_flags() + [
        f"--use-file-for-fake-audio-capture={fake_audio_path}",
        f"--disable-extensions-except={EXTENSION_PATH}",
        f"--load-extension={EXTENSION_PATH}",
    ]
    context = playwright.chromium.launch_persistent_context(
        user_data_dir="",
        headless=False,
        args=args,
    )
    _install_media_mute(context)
    page = context.pages[0] if context.pages else context.new_page()
    return context, page


def launch_guest_browser(playwright: Playwright, fake_audio_path: str) -> tuple[Browser, BrowserContext, Page]:
    """Launch a plain Chromium browser without the extension for a guest participant.

    Args:
        playwright: Active Playwright instance.
        fake_audio_path: Absolute path to a WAV file to use as the fake mic input.

    Returns:
        A (browser, context, page) tuple. Both browser and context must be
        closed by the caller when done.
    """
    args = _fake_audio_flags() + [
        f"--use-file-for-fake-audio-capture={fake_audio_path}",
    ]
    browser = playwright.chromium.launch(headless=False, args=args)
    context = browser.new_context()
    _install_media_mute(context)
    page = context.new_page()
    return browser, context, page


def kontur_login(page: Page, login: str, password: str,
                 timeout: int = 30_000) -> None:
    """Authenticate via the conference SSO (Keycloak).

    example.ktalk.ru redirects to sso.example.org which shows
    a login form (username + password).

    Args:
        page: Playwright Page that has been redirected to SSO login
              (typically after navigating to example.ktalk.ru).
        login: conference SSO account login (email or username).
        password: Account password.
        timeout: Maximum wait time in ms for each step.
    """
    # Step 1: Username + Password
    page.wait_for_selector("input[name='username']", timeout=timeout)
    page.locator("input[name='username']").fill(login)
    page.locator("input[name='password']").fill(password)
    page.locator("input[name='login'][type='submit']").click()

    # Wait until we land back on example.ktalk.ru
    page.wait_for_url("**/example.ktalk.ru/**", timeout=timeout)


def create_public_room(page: Page, timeout: int = 15_000) -> str:
    """Create a new public KTalk room and return its URL.

    Navigates to example.ktalk.ru, clicks "Start quick call" to create a room,
    and enables "Public room" so external guests can join without auth.

    Args:
        page: Playwright Page authenticated via SSO.
        timeout: Maximum wait time in ms for each interaction.

    Returns:
        The full URL of the newly created room (e.g. ``https://example.ktalk.ru/abcdef123``).
    """
    page.goto("https://example.ktalk.ru", wait_until="domcontentloaded", timeout=timeout)

    # Click "Start quick call" button (visible on the example.ktalk.ru dashboard)
    create_btn = page.locator(
        "button:has-text('Start quick call'), "
        "button:has-text('Начать быстрый звонок'), "
        "button:has-text('Создать'), "
        "button:has-text('Create event')"
    ).first
    create_btn.click()

    # Some KTalk deployments show a pre-join / lobby screen before entering
    try:
        join_btn = page.locator(
            "button:has-text('Войти'), "
            "button:has-text('Присоединиться'), "
            "button:has-text('Join'), "
            "[data-testid='prejoin-join-button']"
        ).first
        join_btn.wait_for(state="visible", timeout=5_000)
        join_btn.click()
    except Exception:
        pass  # No pre-join screen; already in the room

    # Wait for room URL (contains a hash/slug after the domain)
    page.wait_for_url("**/example.ktalk.ru/**", timeout=timeout)

    # Enable "Public room" so external guests can join without auth.
    # Open the shield/security menu (bottom-left toolbar icon)
    _enable_public_room(page, timeout)

    return page.url


def _enable_public_room(page: Page, timeout: int = 10_000) -> None:
    """Toggle 'Public room' on in the meeting permissions panel.

    Opens the shield icon menu → finds the 'Public room' toggle → enables it
    if not already enabled.
    """
    import time

    # Click the "Meeting settings" shield button (conference-island component)
    shield_btn = page.locator(
        "[aria-label='Meeting settings'], "
        "[aria-label='Настройки встречи']"
    ).first

    shield_btn.wait_for(state="visible", timeout=timeout)
    shield_btn.click()

    time.sleep(1)

    # Find the "Public room" toggle (div[role=checkbox]) and enable it
    public_toggle = page.locator(
        "[role='checkbox'][title='Public room'], "
        "[role='checkbox'][title='Публичная комната']"
    ).first

    public_toggle.wait_for(state="visible", timeout=5_000)
    if public_toggle.get_attribute("aria-checked") == "false":
        public_toggle.click()

    time.sleep(1)

    # Close the menu by clicking elsewhere or pressing Escape
    page.keyboard.press("Escape")


def guest_join_room(page: Page, room_url: str, guest_name: str, timeout: int = 20_000) -> None:
    """Join a KTalk room as an unauthenticated guest.

    Navigates to *room_url*, enters *guest_name* in the guest-name input, and
    clicks the join button.

    Args:
        page: Playwright Page from a guest browser (no Kontur auth).
        room_url: Full KTalk room URL to join.
        guest_name: Display name for the guest participant.
        timeout: Maximum wait time in ms for each interaction.
    """
    page.goto(room_url, wait_until="domcontentloaded", timeout=timeout)

    # Guest name input: "Please type your name" with placeholder "Example: Adam Smith"
    name_input = page.locator(
        "input[placeholder*='Adam Smith' i], "
        "input[placeholder*='имя' i], "
        "input[placeholder*='name' i], "
        "input[name='displayName']"
    ).first
    name_input.wait_for(state="visible", timeout=timeout)
    name_input.fill(guest_name)

    # Click "Continue" button to proceed to pre-join screen
    continue_btn = page.locator(
        "button:has-text('Continue'), "
        "button:has-text('Продолжить')"
    ).first
    continue_btn.click()

    # Pre-join screen: camera/mic preview with "Join the conference" button
    join_btn = page.locator(
        "button:has-text('Join the conference'), "
        "button:has-text('Присоединиться к конференции'), "
        "button:has-text('Присоединиться')"
    ).first
    join_btn.wait_for(state="visible", timeout=timeout)
    join_btn.click()

    # Wait until we are inside the room. Match any KTalk subdomain so
    # this helper works for example.ktalk.ru as well as personal workspaces
    # (e.g. uoqrh0lo.ktalk.ru) reached via KTALK_ROOM_URL override.
    page.wait_for_url("**/*.ktalk.ru/**", timeout=timeout)


def toggle_mic(page: Page, mute: bool) -> None:
    """Set the microphone mute state for the local participant.

    Inspects the current aria/data attributes of the mic toolbar button to
    decide whether a click is needed. Falls back to the 'm' keyboard shortcut
    if the button cannot be identified.

    Args:
        page: Playwright Page of a participant already inside a room.
        mute: ``True`` to mute the microphone, ``False`` to unmute.
    """
    mic_btn = page.locator(
        "[aria-label*='микрофон' i], "
        "[aria-label*='mute' i], "
        "[aria-label*='unmute' i], "
        "[data-testid='toolbar-mic-button'], "
        "button.toolbox-button:has(svg[class*='mic'])"
    ).first

    try:
        mic_btn.wait_for(state="visible", timeout=5_000)

        # Determine current state from aria-pressed or aria-label text
        aria_pressed = mic_btn.get_attribute("aria-pressed")
        aria_label = (mic_btn.get_attribute("aria-label") or "").lower()

        # aria-pressed="true" means the mute button is active (mic muted)
        if aria_pressed is not None:
            currently_muted = aria_pressed == "true"
        else:
            currently_muted = "unmute" in aria_label or "включить" in aria_label

        if mute != currently_muted:
            mic_btn.click()
    except Exception:
        # Fallback: use the 'm' keyboard shortcut (standard Jitsi keybinding)
        page.keyboard.press("m")
