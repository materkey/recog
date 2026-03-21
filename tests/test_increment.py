"""Tests for recog/increment.py — IncrementGenerator."""
import asyncio
import json
import pytest
from pathlib import Path
from unittest.mock import AsyncMock, patch, MagicMock

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.increment import IncrementGenerator


def _make_meeting(tmp_path: Path, meeting_id: str = "2026-03-21_test",
                  segments: list[dict] = None, meta: dict = None) -> Path:
    """Create a meeting directory with meta.json and transcript.jsonl."""
    meeting_dir = tmp_path / meeting_id
    meeting_dir.mkdir(parents=True, exist_ok=True)

    default_meta = {
        "id": meeting_id,
        "title": "Daily sync",
        "created_at": "2026-03-21T14:30:00+00:00",
        "participants": {
            "speaker_1": {"name": "Вася", "is_local": True},
            "speaker_2": {"name": "Петя", "is_local": False},
        },
    }
    meta_data = meta or default_meta
    (meeting_dir / "meta.json").write_text(
        json.dumps(meta_data, ensure_ascii=False, indent=2), encoding="utf-8"
    )

    if segments is None:
        segments = [
            {"id": 1, "ts_display": "14:30:01", "speaker": "Вася", "text": "Привет"},
            {"id": 2, "ts_display": "14:30:05", "speaker": "Петя", "text": "Привет всем"},
            {"id": 3, "ts_display": "14:30:10", "speaker": "Вася", "text": "Начинаем"},
        ]
    lines = [json.dumps(s, ensure_ascii=False) for s in segments]
    (meeting_dir / "transcript.jsonl").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )
    return meeting_dir


def _make_v1_meta(inc_dir: Path, segment_range: list[int]):
    """Create a v1_meta.json in increments dir."""
    inc_dir.mkdir(parents=True, exist_ok=True)
    meta = {
        "version": 1,
        "generated_at": "2026-03-21T14:45:00+00:00",
        "provider": "codex",
        "model": "gpt-5.4",
        "reasoning_effort": "xhigh",
        "segment_range": segment_range,
        "segment_count": segment_range[1] - segment_range[0] + 1,
        "status": "completed",
        "duration_seconds": 5.0,
        "error": None,
    }
    (inc_dir / "v1_meta.json").write_text(
        json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    (inc_dir / "v1.md").write_text(
        "---\nmeeting: Daily sync\nversion: 1\n---\n\n## Задачи\n- Task 1\n",
        encoding="utf-8",
    )


@pytest.mark.asyncio
async def test_generate_v1_creates_increment_files(tmp_path):
    """
    given: meeting with 3 segments, no prior increments
    when: generate() called
    then: v1.md with YAML frontmatter and v1_meta.json with correct fields created
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)
    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="## Задачи\n- Сделать ревью\n")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(meeting_id, meeting_path)

    assert version == 1

    inc_dir = meeting_path / "increments"
    assert inc_dir.exists()

    # Check v1.md
    v1_md = (inc_dir / "v1.md").read_text(encoding="utf-8")
    assert "---" in v1_md
    assert 'meeting: "Daily sync"' in v1_md
    assert f"meeting_id: {meeting_id}" in v1_md
    assert 'date: "2026-03-21"' in v1_md
    assert "version: 1" in v1_md
    assert "segment_range: [1, 3]" in v1_md
    assert "## Задачи" in v1_md
    assert "Сделать ревью" in v1_md

    # Check v1_meta.json
    v1_meta = json.loads((inc_dir / "v1_meta.json").read_text(encoding="utf-8"))
    assert v1_meta["version"] == 1
    assert v1_meta["status"] == "completed"
    assert v1_meta["segment_range"] == [1, 3]
    assert v1_meta["segment_count"] == 3
    assert v1_meta["error"] is None
    assert v1_meta["provider"] == "codex"

    # Verify prompt contained all segments
    call_args = mock_provider.complete.call_args[0][0]
    assert "Привет" in call_args
    assert "Начинаем" in call_args


@pytest.mark.asyncio
async def test_generate_v2_uses_delta_segments(tmp_path):
    """
    given: meeting with 8 segments, v1 covers segments [1, 5]
    when: generate() called
    then: v2 prompt contains only segments 6-8, not 1-5
    """
    meeting_id = "2026-03-21_test"
    segments = [
        {"id": i, "ts_display": f"14:30:{i:02d}", "speaker": "Вася", "text": f"Seg {i}"}
        for i in range(1, 9)
    ]
    meeting_path = _make_meeting(tmp_path, meeting_id, segments=segments)

    # Pre-create v1 covering segments 1-5
    inc_dir = meeting_path / "increments"
    _make_v1_meta(inc_dir, [1, 5])

    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="## Задачи\n- Delta task\n")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(meeting_id, meeting_path)

    assert version == 2

    # Verify prompt contains only segments 6-8
    call_args = mock_provider.complete.call_args[0][0]
    assert "Seg 6" in call_args
    assert "Seg 7" in call_args
    assert "Seg 8" in call_args
    # Should NOT contain segments 1-5
    assert "Seg 1" not in call_args
    assert "Seg 5" not in call_args

    # Check v2_meta.json
    v2_meta = json.loads((inc_dir / "v2_meta.json").read_text(encoding="utf-8"))
    assert v2_meta["version"] == 2
    assert v2_meta["segment_range"] == [6, 8]
    assert v2_meta["segment_count"] == 3


@pytest.mark.asyncio
async def test_skip_generation_when_zero_delta(tmp_path):
    """
    given: meeting with 5 segments, v1 covers all [1, 5]
    when: generate() called
    then: no v2 created, CodexCliProvider not called
    """
    meeting_id = "2026-03-21_test"
    segments = [
        {"id": i, "ts_display": f"14:30:{i:02d}", "speaker": "Вася", "text": f"Seg {i}"}
        for i in range(1, 6)
    ]
    meeting_path = _make_meeting(tmp_path, meeting_id, segments=segments)

    # Pre-create v1 covering ALL segments
    inc_dir = meeting_path / "increments"
    _make_v1_meta(inc_dir, [1, 5])

    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.complete = AsyncMock()

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(meeting_id, meeting_path)

    assert version is None
    mock_provider.complete.assert_not_called()
    assert not (inc_dir / "v2_meta.json").exists()
    assert not (inc_dir / "v2.md").exists()


@pytest.mark.asyncio
async def test_generate_handles_codex_failure(tmp_path):
    """
    given: meeting with segments, CodexCliProvider raises exception
    when: generate() called
    then: vN_meta.json with status="failed" and error message, no vN.md
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)
    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(side_effect=Exception("Codex CLI error: timeout"))

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(meeting_id, meeting_path)

    assert version is None

    inc_dir = meeting_path / "increments"
    # Meta should exist with failed status
    v1_meta = json.loads((inc_dir / "v1_meta.json").read_text(encoding="utf-8"))
    assert v1_meta["status"] == "failed"
    assert "timeout" in v1_meta["error"]

    # No .md file
    assert not (inc_dir / "v1.md").exists()


def test_list_increments(tmp_path):
    """
    given: meeting with two increment versions
    when: list_increments() called
    then: returns sorted list of meta dicts
    """
    meeting_id = "2026-03-21_test"
    inc_dir = tmp_path / meeting_id / "increments"
    inc_dir.mkdir(parents=True)

    for v in [1, 2]:
        meta = {"version": v, "status": "completed", "segment_range": [1, v * 3]}
        (inc_dir / f"v{v}_meta.json").write_text(
            json.dumps(meta), encoding="utf-8"
        )

    gen = IncrementGenerator(tmp_path)
    result = gen.list_increments(meeting_id)

    assert len(result) == 2
    assert result[0]["version"] == 1
    assert result[1]["version"] == 2


def test_get_increment(tmp_path):
    """
    given: meeting with v1 increment (meta + md)
    when: get_increment(meeting_id, 1) called
    then: returns dict with meta and content
    """
    meeting_id = "2026-03-21_test"
    inc_dir = tmp_path / meeting_id / "increments"
    inc_dir.mkdir(parents=True)

    meta = {"version": 1, "status": "completed"}
    (inc_dir / "v1_meta.json").write_text(json.dumps(meta), encoding="utf-8")
    (inc_dir / "v1.md").write_text("---\nversion: 1\n---\n\n## Задачи\n- Task\n", encoding="utf-8")

    gen = IncrementGenerator(tmp_path)
    result = gen.get_increment(meeting_id, 1)

    assert result is not None
    assert result["meta"]["version"] == 1
    assert result["meta"]["status"] == "completed"
    assert "## Задачи" in result["content"]


def test_get_increment_returns_none_for_missing(tmp_path):
    """
    given: meeting with no increments
    when: get_increment(meeting_id, 1) called
    then: returns None
    """
    meeting_id = "2026-03-21_test"
    (tmp_path / meeting_id).mkdir(parents=True)

    gen = IncrementGenerator(tmp_path)
    result = gen.get_increment(meeting_id, 1)
    assert result is None
