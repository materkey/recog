"""Pytest configuration and fixtures for e2e diarization tests."""
import subprocess
import sys
import time
from pathlib import Path

import pytest

# Load .env from project root so SSO_* vars are available
_PROJECT_ROOT = Path(__file__).parent.parent.parent
sys.path.insert(0, str(_PROJECT_ROOT))
from recog.config import load_dotenv
load_dotenv(_PROJECT_ROOT / ".env")

_FIXTURES_DIR = Path(__file__).parent / "fixtures"

_LIVE_SERVICE = "com.recog.live"
_FRONTEND_SERVICE = "com.recog.frontend"


def pytest_configure(config):
    config.addinivalue_line(
        "markers",
        "e2e: end-to-end tests requiring KTalk, browser, and audio hardware",
    )


def _service_is_running(service_name: str) -> bool:
    """Check if a launchd service is currently running."""
    try:
        result = subprocess.run(
            ["launchctl", "list", service_name],
            capture_output=True, text=True,
        )
        return result.returncode == 0
    except Exception:
        return False


def _port_in_use(port: int) -> bool:
    """Check if a TCP port is currently in use."""
    result = subprocess.run(
        ["lsof", "-i", f":{port}", "-sTCP:LISTEN"],
        capture_output=True, text=True,
    )
    return bool(result.stdout.strip())


def _wait_ports_free(ports: list[int], timeout: float = 15.0) -> None:
    """Wait until all specified ports are free, killing processes if needed."""
    deadline = time.time() + timeout
    for port in ports:
        while _port_in_use(port) and time.time() < deadline:
            # Kill any process still holding the port
            subprocess.run(
                ["lsof", "-ti", f":{port}", "-sTCP:LISTEN"],
                capture_output=True, text=True,
            )
            result = subprocess.run(
                ["lsof", "-ti", f":{port}", "-sTCP:LISTEN"],
                capture_output=True, text=True,
            )
            for pid in result.stdout.strip().split():
                if pid:
                    print(f"[e2e] Killing PID {pid} holding port {port}")
                    subprocess.run(["kill", "-9", pid], check=False)
            time.sleep(1)


def _get_audio_output() -> str:
    """Get current macOS audio output device name."""
    result = subprocess.run(
        ["SwitchAudioSource", "-c"],
        capture_output=True, text=True,
    )
    return result.stdout.strip()


def _set_audio_output(device_name: str) -> None:
    """Set macOS audio output device."""
    subprocess.run(
        ["SwitchAudioSource", "-s", device_name],
        capture_output=True, text=True, check=True,
    )


@pytest.fixture(scope="session", autouse=True)
def stop_recog_services():
    """Stop recog launchd services and switch audio output before tests."""
    live_was_running = _service_is_running(_LIVE_SERVICE)
    frontend_was_running = _service_is_running(_FRONTEND_SERVICE)

    # Must unload (not just stop) because KeepAlive/SuccessfulExit=false
    # causes launchd to restart the service after a stop
    _live_plist = Path.home() / "Library/LaunchAgents" / f"{_LIVE_SERVICE}.plist"
    _frontend_plist = Path.home() / "Library/LaunchAgents" / f"{_FRONTEND_SERVICE}.plist"

    if live_was_running:
        print(f"\n[e2e] Unloading {_LIVE_SERVICE} (KeepAlive would restart after stop)...")
        subprocess.run(["launchctl", "unload", str(_live_plist)], check=False)

    # Wait for WebSocket ports (8765 ktalk_bridge, 8766 internal) to be free
    print("[e2e] Waiting for ports 8765/8766 to be released...")
    _wait_ports_free([8765, 8766])
    print("[e2e] Ports free, proceeding with tests")

    # Switch audio output to Multi-Output Device so BlackHole receives audio
    original_output = _get_audio_output()
    if original_output != "Multi-Output Device":
        print(f"[e2e] Switching audio output: '{original_output}' -> 'Multi-Output Device'")
        _set_audio_output("Multi-Output Device")
    else:
        print("[e2e] Audio output already set to Multi-Output Device")

    yield

    # Restore original audio output
    if original_output != "Multi-Output Device":
        print(f"\n[e2e] Restoring audio output to '{original_output}'")
        _set_audio_output(original_output)

    if live_was_running:
        print(f"[e2e] Reloading {_LIVE_SERVICE}...")
        subprocess.run(["launchctl", "load", str(_live_plist)], check=False)
        subprocess.run(["launchctl", "start", _LIVE_SERVICE], check=False)
    if frontend_was_running:
        print(f"[e2e] Reloading {_FRONTEND_SERVICE}...")
        subprocess.run(["launchctl", "load", str(_frontend_plist)], check=False)
        subprocess.run(["launchctl", "start", _FRONTEND_SERVICE], check=False)


@pytest.fixture(scope="session")
def fixtures_dir() -> Path:
    return _FIXTURES_DIR


@pytest.fixture(scope="session")
def speaker_a_wav(fixtures_dir: Path) -> Path:
    path = fixtures_dir / "speaker_a.wav"
    assert path.exists(), f"Fixture not found: {path}. Run tests/e2e/prepare_fixtures.py first."
    return path


@pytest.fixture(scope="session")
def speaker_b_wav(fixtures_dir: Path) -> Path:
    path = fixtures_dir / "speaker_b.wav"
    assert path.exists(), f"Fixture not found: {path}. Run tests/e2e/prepare_fixtures.py first."
    return path


@pytest.fixture(scope="session")
def speaker_c_wav(fixtures_dir: Path) -> Path:
    path = fixtures_dir / "speaker_c.wav"
    assert path.exists(), f"Fixture not found: {path}. Run tests/e2e/prepare_fixtures.py first."
    return path
