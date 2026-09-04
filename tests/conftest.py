"""
Pytest fixtures for recog tests.

Following datasette-enrichments patterns:
- pytest_asyncio.fixture for async fixtures
- autouse fixtures for setup/teardown
- tmpdir for temporary test data
"""
import json
import pytest
import pytest_asyncio
import numpy as np
from pathlib import Path

import sys
sys.path.insert(0, str(Path(__file__).parent.parent))

import recog.config as recog_config
from recog.speaker_db import SpeakerDatabase
from recog.models import Segment


@pytest.fixture(autouse=True)
def local_asr_backend(monkeypatch):
    """Pin the ASR backend to "local" for every unit test.

    `recog.config` reads the developer's `.env` at import, so a machine
    running the remote offload would otherwise make every test that
    constructs a LiveTranscriber take the gateway path (and now fail
    outright when RECOG_ASR_REMOTE_URL is unset). Tests that exercise the
    remote path override this with their own monkeypatch.
    """
    monkeypatch.setattr(recog_config, "RECOG_ASR_BACKEND", "local", raising=False)


@pytest.fixture
def sample_embedding() -> np.ndarray:
    """Returns a normalized 192-dim embedding (ECAPA-TDNN size)."""
    np.random.seed(42)
    emb = np.random.randn(192)
    return emb / np.linalg.norm(emb)


@pytest.fixture
def sample_embedding_similar(sample_embedding) -> np.ndarray:
    """Returns an embedding similar to sample_embedding (cosine > 0.9)."""
    noise = np.random.randn(192) * 0.1
    similar = sample_embedding + noise
    return similar / np.linalg.norm(similar)


@pytest.fixture
def sample_embedding_different() -> np.ndarray:
    """Returns an embedding very different from sample_embedding."""
    np.random.seed(999)
    emb = np.random.randn(192)
    return emb / np.linalg.norm(emb)


@pytest.fixture
def speaker_db_path(tmp_path) -> Path:
    """Returns path to a temporary speaker database file."""
    return tmp_path / "test_speakers.json"


@pytest.fixture
def speaker_db_with_data(speaker_db_path, sample_embedding) -> SpeakerDatabase:
    """
    Creates a SpeakerDatabase with one pre-loaded speaker.

    Speaker: "vlad" with display_name "Vladimir"
    """
    db = SpeakerDatabase(str(speaker_db_path))
    db.add_speaker(
        speaker_id="vlad",
        display_name="Vladimir",
        embedding=sample_embedding,
        source_file="test_audio.wav",
        transcript="test transcript"
    )
    db.save()
    return db


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
