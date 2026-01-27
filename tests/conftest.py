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

from recog.speaker_db import SpeakerDatabase
from recog.models import Segment, DiarizationSegment


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


@pytest.fixture
def diarization_segments() -> list[DiarizationSegment]:
    """Returns sample diarization segments for alignment tests."""
    return [
        DiarizationSegment(start=0.0, end=2.0, speaker_id="SPEAKER_00"),
        DiarizationSegment(start=2.0, end=5.0, speaker_id="SPEAKER_01"),
    ]
