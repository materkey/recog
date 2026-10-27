"""
Pytest fixtures for recog tests.

Following datasette-enrichments patterns:
- pytest_asyncio.fixture for async fixtures
- autouse fixtures for setup/teardown
- tmpdir for temporary test data
"""
import json
import os
import pytest
import pytest_asyncio
from pathlib import Path

import sys
sys.path.insert(0, str(Path(__file__).parent.parent))

# The suite must not depend on the developer's installation config: point both
# the config file and the legacy hosts file at nothing before recog is imported.
os.environ["RECOG_CONFIG_FILE"] = "/nonexistent/recog/config.toml"
os.environ["RECOG_CONFERENCE_HOSTS_FILE"] = "/nonexistent/recog/conference-hosts.json"

import recog.privacy as recog_privacy
from recog.install_config import INSTALL_CONFIG
from recog.models import Segment


@pytest.fixture(autouse=True)
def isolated_privacy_settings(tmp_path, monkeypatch):
    """MCP tests must never read the installed user's privacy preference."""
    monkeypatch.setattr(recog_privacy, "get_data_dir", lambda: tmp_path / "privacy-data")


@pytest.fixture(autouse=True)
def local_asr_backend(monkeypatch):
    """Pin the ASR backend to "local" for every unit test.

    `recog.install_config` reads the developer's `.env` at import, so a machine
    running the remote offload would otherwise make every test that
    constructs a LiveTranscriber take the gateway path (and now fail
    outright when RECOG_ASR_REMOTE_URL is unset). Tests that exercise the
    remote path override this with their own monkeypatch.
    """
    monkeypatch.setattr(INSTALL_CONFIG.asr, "backend", "local")


@pytest.fixture
def asr_segments() -> list[Segment]:
    """Returns sample ASR segments for alignment tests."""
    return [
        Segment(text="Привет", start=0.0, end=1.0),
        Segment(text="Как дела", start=1.5, end=2.5),
        Segment(text="Отлично", start=3.0, end=4.0),
    ]


@pytest_asyncio.fixture
async def mock_websocket():
    """
    Async fixture for WebSocket testing.

    Following datasette-enrichments pattern of using
    pytest_asyncio.fixture for async setup/teardown.
    """
    class MockWebSocket:
        def __init__(self):
            self.messages_sent = []
            self.messages_received = []
            self.closed = False

        async def send(self, message):
            self.messages_sent.append(message)

        async def recv(self):
            if not self.messages_received:
                raise StopAsyncIteration
            return self.messages_received.pop(0)

        async def close(self):
            self.closed = True

        def add_message(self, msg):
            self.messages_received.append(msg)

    ws = MockWebSocket()
    yield ws
    if not ws.closed:
        await ws.close()
