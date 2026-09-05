# Pytest Async Testing Infrastructure Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add pytest-asyncio testing infrastructure following datasette-enrichments patterns

**Architecture:** Fixtures in `conftest.py` provide reusable test setup (temp speaker DB, mock audio data). Tests use `@pytest.mark.asyncio` for async functions and `@pytest.mark.parametrize` for test variations. Cleanup via try/finally in autouse fixtures.

**Tech Stack:** pytest, pytest-asyncio, numpy (for embeddings)

---

## Task 1: Add test dependencies to requirements

**Files:**
- Modify: `/Users/vkkovalev/projects/recog/requirements.txt`

**Step 1: Read current requirements**

Run: `cat /Users/vkkovalev/projects/recog/requirements.txt`

**Step 2: Add test dependencies**

Add to deploy/python/requirements.txt:
```
# Testing
pytest>=7.0
pytest-asyncio>=0.23
```

**Step 3: Install dependencies**

Run: `source /Users/vkkovalev/projects/recog/venv-gigaam/bin/activate && pip install pytest pytest-asyncio`
Expected: Successfully installed packages

**Step 4: Commit**

```bash
git add deploy/python/requirements.txt
git commit -m "feat(tests): add pytest and pytest-asyncio dependencies"
```

---

## Task 2: Create conftest.py with speaker database fixture

**Files:**
- Create: `/Users/vkkovalev/projects/recog/tests/conftest.py`

**Step 1: Write conftest.py with fixtures**

```python
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
```

**Step 2: Verify syntax**

Run: `python -m py_compile /Users/vkkovalev/projects/recog/tests/conftest.py`
Expected: No output (syntax OK)

**Step 3: Commit**

```bash
git add tests/conftest.py
git commit -m "feat(tests): add conftest.py with speaker DB and alignment fixtures"
```

---

## Task 3: Create test_speaker_db.py for SpeakerDatabase

**Files:**
- Create: `/Users/vkkovalev/projects/recog/tests/test_speaker_db.py`

**Step 1: Write the failing test for speaker matching**

```python
"""Tests for recog/speaker_db.py."""
import pytest
import numpy as np
from pathlib import Path

import sys
sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.speaker_db import SpeakerDatabase


class TestSpeakerDatabaseMatch:
    """Tests for match_speaker functionality."""

    def test_match_speaker_exact_embedding_returns_match(
        self, speaker_db_with_data, sample_embedding
    ):
        """
        given: database with one speaker "vlad"
        when: matching with exact same embedding
        then: should return ("vlad", "Vladimir", similarity ~1.0)
        """
        result = speaker_db_with_data.match_speaker(sample_embedding)

        assert result is not None
        speaker_id, display_name, similarity = result
        assert speaker_id == "vlad"
        assert display_name == "Vladimir"
        assert similarity > 0.99

    def test_match_speaker_similar_embedding_returns_match(
        self, speaker_db_with_data, sample_embedding_similar
    ):
        """
        given: database with one speaker "vlad"
        when: matching with similar embedding (cosine > 0.9)
        then: should return match with high similarity
        """
        result = speaker_db_with_data.match_speaker(sample_embedding_similar)

        assert result is not None
        speaker_id, display_name, similarity = result
        assert speaker_id == "vlad"
        assert similarity > 0.8

    def test_match_speaker_different_embedding_returns_none(
        self, speaker_db_with_data, sample_embedding_different
    ):
        """
        given: database with one speaker "vlad"
        when: matching with very different embedding
        then: should return None (below threshold)
        """
        result = speaker_db_with_data.match_speaker(sample_embedding_different)

        assert result is None

    def test_match_speaker_empty_database_returns_none(
        self, speaker_db_path, sample_embedding
    ):
        """
        given: empty speaker database
        when: matching any embedding
        then: should return None
        """
        db = SpeakerDatabase(str(speaker_db_path))
        result = db.match_speaker(sample_embedding)

        assert result is None


class TestSpeakerDatabaseCRUD:
    """Tests for add/delete/rename operations."""

    def test_add_speaker_creates_new_entry(self, speaker_db_path, sample_embedding):
        """
        given: empty speaker database
        when: adding a new speaker
        then: speaker should be retrievable
        """
        db = SpeakerDatabase(str(speaker_db_path))
        db.add_speaker(
            speaker_id="test_id",
            display_name="Test User",
            embedding=sample_embedding
        )

        speaker = db.get_speaker("test_id")
        assert speaker is not None
        assert speaker.display_name == "Test User"
        assert len(speaker.embeddings) == 1

    def test_add_speaker_appends_to_existing(
        self, speaker_db_with_data, sample_embedding_similar
    ):
        """
        given: database with speaker "vlad" having 1 embedding
        when: adding another embedding to "vlad"
        then: "vlad" should have 2 embeddings
        """
        speaker_db_with_data.add_speaker(
            speaker_id="vlad",
            display_name="Vladimir",
            embedding=sample_embedding_similar,
            source_file="second_sample.wav"
        )

        speaker = speaker_db_with_data.get_speaker("vlad")
        assert len(speaker.embeddings) == 2

    def test_delete_speaker_removes_entry(self, speaker_db_with_data):
        """
        given: database with speaker "vlad"
        when: deleting "vlad"
        then: speaker should not be retrievable
        """
        result = speaker_db_with_data.delete_speaker("vlad")

        assert result is True
        assert speaker_db_with_data.get_speaker("vlad") is None

    def test_rename_speaker_updates_display_name(self, speaker_db_with_data):
        """
        given: database with speaker "vlad" named "Vladimir"
        when: renaming to "Vlad K"
        then: display_name should be updated
        """
        result = speaker_db_with_data.rename_speaker("vlad", "Vlad K")

        assert result is True
        speaker = speaker_db_with_data.get_speaker("vlad")
        assert speaker.display_name == "Vlad K"


class TestSpeakerDatabasePersistence:
    """Tests for save/load functionality."""

    def test_save_and_load_preserves_data(
        self, speaker_db_path, sample_embedding
    ):
        """
        given: database with one speaker
        when: save then load in new instance
        then: data should be preserved
        """
        db1 = SpeakerDatabase(str(speaker_db_path))
        db1.add_speaker(
            speaker_id="persist_test",
            display_name="Persistent User",
            embedding=sample_embedding,
            source_file="persist.wav",
            transcript="hello world"
        )
        db1.save()

        db2 = SpeakerDatabase(str(speaker_db_path))

        speaker = db2.get_speaker("persist_test")
        assert speaker is not None
        assert speaker.display_name == "Persistent User"
        assert speaker.embeddings[0].source_file == "persist.wav"
        assert speaker.embeddings[0].transcript == "hello world"
```

**Step 2: Run tests to verify they pass**

Run: `cd /Users/vkkovalev/projects/recog && source venv-gigaam/bin/activate && python -m pytest tests/test_speaker_db.py -v`
Expected: All tests PASS

**Step 3: Commit**

```bash
git add tests/test_speaker_db.py
git commit -m "feat(tests): add SpeakerDatabase unit tests"
```

---

## Task 4: Create test_alignment.py for segment alignment

**Files:**
- Create: `/Users/vkkovalev/projects/recog/tests/test_alignment.py`

**Step 1: Write alignment tests**

```python
"""Tests for recog/alignment.py."""
import pytest
from pathlib import Path

import sys
sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.alignment import align_segments_with_speakers
from recog.models import Segment, DiarizationSegment


class TestAlignSegmentsWithSpeakers:
    """Tests for align_segments_with_speakers function."""

    def test_segment_fully_in_first_speaker_interval(self):
        """
        given: ASR segment [0.0, 1.0] and diarization [0.0, 2.0] SPEAKER_00
        when: aligning
        then: segment should be assigned to SPEAKER_00
        """
        asr = [Segment(text="Hello", start=0.0, end=1.0)]
        diar = [DiarizationSegment(start=0.0, end=2.0, speaker_id="SPEAKER_00")]

        result = align_segments_with_speakers(asr, diar)

        assert len(result) == 1
        assert result[0].speaker_id == "SPEAKER_00"

    def test_segment_spanning_two_speakers_uses_max_overlap(self):
        """
        given: ASR segment [1.5, 2.5] spanning SPEAKER_00 [0, 2] and SPEAKER_01 [2, 4]
        when: aligning
        then: segment should be assigned to speaker with more overlap
        """
        asr = [Segment(text="Test", start=1.5, end=2.5)]
        diar = [
            DiarizationSegment(start=0.0, end=2.0, speaker_id="SPEAKER_00"),
            DiarizationSegment(start=2.0, end=4.0, speaker_id="SPEAKER_01"),
        ]

        result = align_segments_with_speakers(asr, diar)

        assert len(result) == 1
        # Overlap with SPEAKER_00: 0.5s, with SPEAKER_01: 0.5s
        # Equal overlap - should pick first one found with max
        assert result[0].speaker_id in ["SPEAKER_00", "SPEAKER_01"]

    def test_multiple_segments_assigned_correctly(self, asr_segments, diarization_segments):
        """
        given: 3 ASR segments and 2 diarization segments
        when: aligning
        then: each segment should have appropriate speaker assigned
        """
        result = align_segments_with_speakers(asr_segments, diarization_segments)

        assert len(result) == 3
        # Segment [0, 1] -> SPEAKER_00 (fully in [0, 2])
        assert result[0].speaker_id == "SPEAKER_00"
        # Segment [1.5, 2.5] -> overlaps both, picks max
        assert result[1].speaker_id in ["SPEAKER_00", "SPEAKER_01"]
        # Segment [3, 4] -> SPEAKER_01 (fully in [2, 5])
        assert result[2].speaker_id == "SPEAKER_01"

    def test_preserves_original_text_and_times(self, asr_segments, diarization_segments):
        """
        given: ASR segments with specific text and timestamps
        when: aligning
        then: text and timestamps should be preserved
        """
        result = align_segments_with_speakers(asr_segments, diarization_segments)

        assert result[0].text == "Привет"
        assert result[0].start == 0.0
        assert result[0].end == 1.0

    def test_empty_asr_returns_empty_list(self, diarization_segments):
        """
        given: empty ASR segments
        when: aligning
        then: should return empty list
        """
        result = align_segments_with_speakers([], diarization_segments)

        assert result == []

    def test_no_diarization_uses_default_speaker(self):
        """
        given: ASR segments but no diarization
        when: aligning
        then: should use default speaker_0
        """
        asr = [Segment(text="Hello", start=0.0, end=1.0)]

        result = align_segments_with_speakers(asr, [])

        assert len(result) == 1
        assert result[0].speaker_id == "speaker_0"
```

**Step 2: Run tests**

Run: `cd /Users/vkkovalev/projects/recog && source venv-gigaam/bin/activate && python -m pytest tests/test_alignment.py -v`
Expected: All tests PASS

**Step 3: Commit**

```bash
git add tests/test_alignment.py
git commit -m "feat(tests): add segment alignment unit tests"
```

---

## Task 5: Add pytest.ini configuration

**Files:**
- Create: `/Users/vkkovalev/projects/recog/pytest.ini`

**Step 1: Write pytest configuration**

```ini
[pytest]
asyncio_mode = auto
testpaths = tests
python_files = test_*.py
python_classes = Test*
python_functions = test_*
addopts = -v --tb=short
filterwarnings =
    ignore::DeprecationWarning
    ignore::PendingDeprecationWarning
```

**Step 2: Verify all tests run**

Run: `cd /Users/vkkovalev/projects/recog && source venv-gigaam/bin/activate && python -m pytest`
Expected: All tests PASS

**Step 3: Commit**

```bash
git add pytest.ini
git commit -m "feat(tests): add pytest.ini configuration"
```

---

## Task 6: Add async fixture example for future WebSocket tests

**Files:**
- Modify: `/Users/vkkovalev/projects/recog/tests/conftest.py`

**Step 1: Add pytest_asyncio import and async fixture**

Add to conftest.py after existing fixtures:

```python
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
```

**Step 2: Add async marker to test_ktalk_bridge**

Update `/Users/vkkovalev/projects/recog/tests/test_ktalk_bridge.py` to use shared fixtures by adding import:

```python
# Add at top of file after existing imports
from conftest import *  # noqa - import fixtures
```

**Step 3: Run all tests**

Run: `cd /Users/vkkovalev/projects/recog && source venv-gigaam/bin/activate && python -m pytest -v`
Expected: All tests PASS

**Step 4: Commit**

```bash
git add tests/conftest.py tests/test_ktalk_bridge.py
git commit -m "feat(tests): add async WebSocket fixture for future tests"
```

---

## Task 7: Update .gitignore for pytest artifacts

**Files:**
- Modify: `/Users/vkkovalev/projects/recog/.gitignore`

**Step 1: Add pytest artifacts**

Add to .gitignore:
```
# Pytest
.pytest_cache/
__pycache__/
*.pyc
.coverage
htmlcov/
```

**Step 2: Commit**

```bash
git add .gitignore
git commit -m "chore: add pytest artifacts to gitignore"
```

---

## Summary

This plan adds:

1. **pytest + pytest-asyncio** dependencies
2. **conftest.py** with reusable fixtures:
   - `sample_embedding` / `sample_embedding_similar` / `sample_embedding_different`
   - `speaker_db_path` / `speaker_db_with_data`
   - `asr_segments` / `diarization_segments`
   - `mock_websocket` (async)
3. **test_speaker_db.py** - 9 tests for SpeakerDatabase
4. **test_alignment.py** - 6 tests for segment alignment
5. **pytest.ini** - configuration with asyncio_mode=auto

Key patterns from datasette-enrichments applied:
- `@pytest_asyncio.fixture` for async fixtures
- `@pytest.mark.parametrize` ready for variations
- `tmp_path` fixture for temporary files
- Test naming: `given-when-then` in docstrings
- Cleanup via fixture teardown
