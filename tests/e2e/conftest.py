"""Pytest configuration and fixtures for e2e diarization tests."""
import sys
from pathlib import Path

import pytest

# Load .env from project root so SSO_* vars are available
_PROJECT_ROOT = Path(__file__).parent.parent.parent
sys.path.insert(0, str(_PROJECT_ROOT))
from recog.config import load_dotenv
load_dotenv(_PROJECT_ROOT / ".env")

_FIXTURES_DIR = Path(__file__).parent / "fixtures"


def pytest_configure(config):
    config.addinivalue_line(
        "markers",
        "e2e: end-to-end tests requiring KTalk, browser, and audio hardware",
    )


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
