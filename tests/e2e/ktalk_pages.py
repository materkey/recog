"""KTalk page objects for Playwright e2e tests.

Provides helper functions to launch browsers, authenticate via Kontur SSO,
create/join KTalk rooms, and control microphone state using the sync Playwright API.
"""
from pathlib import Path

from playwright.sync_api import Browser, BrowserContext, Page, Playwright

EXTENSION_PATH = str(Path(__file__).parent.parent.parent / "kontur-diarize" / "ktalk-speaker-extension")

_FAKE_AUDIO_FLAGS = [
    "--use-fake-device-for-media-stream",
    "--use-fake-ui-for-media-stream",
]


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
    args = _FAKE_AUDIO_FLAGS + [
        f"--use-file-for-fake-audio-capture={fake_audio_path}",
        f"--disable-extensions-except={EXTENSION_PATH}",
        f"--load-extension={EXTENSION_PATH}",
    ]
    context = playwright.chromium.launch_persistent_context(
        user_data_dir="",
        headless=False,
        args=args,
    )
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
    args = _FAKE_AUDIO_FLAGS + [
        f"--use-file-for-fake-audio-capture={fake_audio_path}",
    ]
    browser = playwright.chromium.launch(headless=False, args=args)
    context = browser.new_context()
    page = context.new_page()
    return browser, context, page


def kontur_login(page: Page, login: str, password: str, timeout: int = 30_000) -> None:
    """Authenticate via the conference SSO (Keycloak).

    example.ktalk.ru redirects to sso.example.org which shows a single-page
    login form with username + password fields and a "Sign In" submit input.

    Args:
        page: Playwright Page that has been redirected to SSO login
              (typically after navigating to example.ktalk.ru).
        login: conference SSO account login (email or username).
        password: Account password.
        timeout: Maximum wait time in ms for each step.
    """
    # Wait for SSO Keycloak login form
    page.wait_for_selector("input[name='username']", timeout=timeout)
    page.locator("input[name='username']").fill(login)
    page.locator("input[name='password']").fill(password)

    # Submit — SSO uses input[type=submit][name=login], not a button
    page.locator("input[name='login'][type='submit']").click()

    # Wait until we land back on example.ktalk.ru
    page.wait_for_url("**/example.ktalk.ru/**", timeout=timeout)


def create_public_room(page: Page, timeout: int = 15_000) -> str:
    """Create a new public KTalk room and return its URL.

    Navigates to example.ktalk.ru and clicks "Start quick call" to create a room.
    Handles a pre-join screen if one appears.

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
    return page.url


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

    # Guest name input may appear on a pre-join / lobby screen
    name_input = page.locator(
        "input[placeholder*='имя' i], "
        "input[placeholder*='name' i], "
        "input[name='displayName'], "
        "input[data-testid='prejoin-display-name']"
    ).first
    name_input.wait_for(state="visible", timeout=timeout)
    name_input.fill(guest_name)

    join_btn = page.locator(
        "button:has-text('Войти'), "
        "button:has-text('Присоединиться'), "
        "button:has-text('Join'), "
        "[data-testid='prejoin-join-button']"
    ).first
    join_btn.click()

    # Confirm we are inside the room
    page.wait_for_url("**/example.ktalk.ru/**", timeout=timeout)


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
