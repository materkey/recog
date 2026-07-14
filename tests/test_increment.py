"""Tests for recog/increment.py — IncrementGenerator."""
import asyncio
import json
import pytest
from pathlib import Path
from unittest.mock import AsyncMock, patch, MagicMock

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.increment import IncrementGenerator
from recog.prompt_registry import PromptConfig


# Default prompt config used across tests
DEFAULT_PROMPT = PromptConfig(
    id="my-increment",
    name="My increment",
    filter=None,
    template="Участник: {user_name}\nТранскрипт:\n{transcript}",
)


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
    when: generate() called with PromptConfig
    then: v1.md with YAML frontmatter and v1_meta.json in prompt_id subdirectory
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)
    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="## Задачи\n- Сделать ревью\n")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider) as mock_cls:
        version = await gen.generate(meeting_id, meeting_path, DEFAULT_PROMPT)

    assert version == 1
    mock_cls.assert_called_with(task="increment")

    # Files are now in increments/<prompt_id>/ subdirectory
    inc_dir = meeting_path / "increments" / "my-increment"
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
    assert v1_meta["prompt_id"] == "my-increment"
    assert v1_meta["prompt_name"] == "My increment"
    assert v1_meta["participant_id"] is None
    assert v1_meta["participant_name"] == "Вася"

    # Verify prompt contained all segments
    call_args = mock_provider.complete.call_args[0][0]
    assert "Привет" in call_args
    assert "Начинаем" in call_args


@pytest.mark.asyncio
async def test_other_participant_when_generated_then_uses_their_name_and_metadata(tmp_path):
    """
    given: meeting with a local and a remote participant
    when: increment generation targets the remote participant
    then: the prompt and stored metadata identify that participant
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)
    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.6-sol"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="## Задачи\n- Сделать ревью\n")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(
            meeting_id,
            meeting_path,
            DEFAULT_PROMPT,
            participant_id="speaker_2",
        )

    assert version == 1
    rendered_prompt = mock_provider.complete.call_args[0][0]
    assert "Участник: Петя" in rendered_prompt

    meta = json.loads(
        (meeting_path / "increments" / "my-increment" / "v1_meta.json").read_text(
            encoding="utf-8"
        )
    )
    assert meta["participant_id"] == "speaker_2"
    assert meta["participant_name"] == "Петя"
    assert meta["model"] == "gpt-5.6-sol"


@pytest.mark.asyncio
async def test_local_increment_exists_when_other_participant_generated_then_full_transcript_is_used(tmp_path):
    """
    given: a local participant increment already covers the whole transcript
    when: the first increment is generated for another participant
    then: participant delta tracking starts independently from the full transcript
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)
    inc_dir = meeting_path / "increments" / "my-increment"
    _make_v1_meta(inc_dir, [1, 3])
    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.6-sol"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="## Задачи\n- Сделать ревью\n")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(
            meeting_id,
            meeting_path,
            DEFAULT_PROMPT,
            participant_id="speaker_2",
        )

    assert version == 2
    rendered_prompt = mock_provider.complete.call_args[0][0]
    assert "Привет" in rendered_prompt
    assert "Начинаем" in rendered_prompt


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

    # Pre-create v1 covering segments 1-5 in prompt_id subdirectory
    inc_dir = meeting_path / "increments" / "my-increment"
    _make_v1_meta(inc_dir, [1, 5])

    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="## Задачи\n- Delta task\n")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(meeting_id, meeting_path, DEFAULT_PROMPT)

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

    # Pre-create v1 covering ALL segments in prompt_id subdirectory
    inc_dir = meeting_path / "increments" / "my-increment"
    _make_v1_meta(inc_dir, [1, 5])

    gen = IncrementGenerator(tmp_path)

    mock_provider = MagicMock()
    mock_provider.complete = AsyncMock()

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(meeting_id, meeting_path, DEFAULT_PROMPT)

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
        version = await gen.generate(meeting_id, meeting_path, DEFAULT_PROMPT)

    assert version is None

    inc_dir = meeting_path / "increments" / "my-increment"
    # Meta should exist with failed status
    v1_meta = json.loads((inc_dir / "v1_meta.json").read_text(encoding="utf-8"))
    assert v1_meta["status"] == "failed"
    assert "timeout" in v1_meta["error"]
    assert v1_meta["prompt_id"] == "my-increment"

    # No .md file
    assert not (inc_dir / "v1.md").exists()


def test_list_increments(tmp_path):
    """
    given: meeting with two increment versions for a prompt
    when: list_increments() called with prompt_id
    then: returns sorted list of meta dicts
    """
    meeting_id = "2026-03-21_test"
    inc_dir = tmp_path / meeting_id / "increments" / "my-increment"
    inc_dir.mkdir(parents=True)

    for v in [1, 2]:
        meta = {"version": v, "status": "completed", "segment_range": [1, v * 3]}
        (inc_dir / f"v{v}_meta.json").write_text(
            json.dumps(meta), encoding="utf-8"
        )

    gen = IncrementGenerator(tmp_path)
    result = gen.list_increments(meeting_id, "my-increment")

    assert len(result) == 2
    assert result[0]["version"] == 1
    assert result[1]["version"] == 2


def test_get_increment(tmp_path):
    """
    given: meeting with v1 increment (meta + md) for a prompt
    when: get_increment(meeting_id, prompt_id, 1) called
    then: returns dict with meta and content
    """
    meeting_id = "2026-03-21_test"
    inc_dir = tmp_path / meeting_id / "increments" / "my-increment"
    inc_dir.mkdir(parents=True)

    meta = {"version": 1, "status": "completed"}
    (inc_dir / "v1_meta.json").write_text(json.dumps(meta), encoding="utf-8")
    (inc_dir / "v1.md").write_text("---\nversion: 1\n---\n\n## Задачи\n- Task\n", encoding="utf-8")

    gen = IncrementGenerator(tmp_path)
    result = gen.get_increment(meeting_id, "my-increment", 1)

    assert result is not None
    assert result["meta"]["version"] == 1
    assert result["meta"]["status"] == "completed"
    assert "## Задачи" in result["content"]


def test_get_increment_returns_none_for_missing(tmp_path):
    """
    given: meeting with no increments
    when: get_increment(meeting_id, prompt_id, 1) called
    then: returns None
    """
    meeting_id = "2026-03-21_test"
    (tmp_path / meeting_id).mkdir(parents=True)

    gen = IncrementGenerator(tmp_path)
    result = gen.get_increment(meeting_id, "my-increment", 1)
    assert result is None


@pytest.mark.asyncio
async def test_generate_uses_prompt_template(tmp_path):
    """
    given: meeting with segments and a custom PromptConfig
    when: generate() called with that prompt
    then: AI provider receives the rendered template with {user_name}, {transcript},
          {participants}, {meeting_title} substituted
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)
    gen = IncrementGenerator(tmp_path)

    custom_prompt = PromptConfig(
        id="team-facts",
        name="Team facts",
        filter=None,
        template="Title: {meeting_title}\nParticipants: {participants}\nUser: {user_name}\n{transcript}",
    )

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="Facts here")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        version = await gen.generate(meeting_id, meeting_path, custom_prompt)

    assert version == 1

    # Verify rendered prompt has all variables substituted
    call_args = mock_provider.complete.call_args[0][0]
    assert "Title: Daily sync" in call_args
    assert "Вася" in call_args
    assert "Петя" in call_args
    assert "User: Вася" in call_args

    # Files stored in team-facts subdirectory
    inc_dir = meeting_path / "increments" / "team-facts"
    assert (inc_dir / "v1.md").exists()
    assert (inc_dir / "v1_meta.json").exists()


@pytest.mark.asyncio
async def test_generate_all_runs_matching_prompts_in_parallel(tmp_path):
    """
    given: prompts dir with 2 prompts (one filtered, one universal), meeting title matches filter
    when: generate_all() called
    then: both prompts generate increments in their respective subdirectories
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)

    # Create prompts directory with two prompts
    prompts_dir = tmp_path / "prompts"
    prompts_dir.mkdir()
    (prompts_dir / "my-increment.md").write_text(
        "---\nname: My increment\nfilter: null\n---\n\nUser: {user_name}\n{transcript}",
        encoding="utf-8",
    )
    (prompts_dir / "sync-only.md").write_text(
        "---\nname: Sync only\nfilter: sync\n---\n\nSync report for {user_name}\n{transcript}",
        encoding="utf-8",
    )

    gen = IncrementGenerator(tmp_path, prompts_dir=prompts_dir)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="Generated content")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        await gen.generate_all(meeting_id, meeting_path)

    # Both prompts should have generated v1
    assert (meeting_path / "increments" / "my-increment" / "v1.md").exists()
    assert (meeting_path / "increments" / "sync-only" / "v1.md").exists()

    # Provider should have been called twice (once per prompt)
    assert mock_provider.complete.call_count == 2


@pytest.mark.asyncio
async def test_generate_all_skips_non_matching_prompts(tmp_path):
    """
    given: prompts dir with a filtered prompt that doesn't match meeting title
    when: generate_all() called
    then: non-matching prompt does not generate
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)  # title is "Daily sync"

    prompts_dir = tmp_path / "prompts"
    prompts_dir.mkdir()
    (prompts_dir / "retro-only.md").write_text(
        "---\nname: Retro only\nfilter: retro\n---\n\n{transcript}",
        encoding="utf-8",
    )

    gen = IncrementGenerator(tmp_path, prompts_dir=prompts_dir)

    mock_provider = MagicMock()
    mock_provider.complete = AsyncMock()

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        await gen.generate_all(meeting_id, meeting_path)

    # No prompts matched, so no generation
    mock_provider.complete.assert_not_called()
    assert not (meeting_path / "increments" / "retro-only").exists()


@pytest.mark.asyncio
async def test_callback_receives_prompt_id(tmp_path):
    """
    given: IncrementGenerator with callback set
    when: generate() succeeds
    then: callback receives (meeting_id, version, prompt_id)
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)
    gen = IncrementGenerator(tmp_path)

    callback = MagicMock()
    gen.set_callback(callback)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="Result")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        await gen.generate(meeting_id, meeting_path, DEFAULT_PROMPT)

    callback.assert_called_once_with(meeting_id, 1, "my-increment")


# --- Migration tests ---


def test_migrate_flat_increments_moves_files(tmp_path):
    """
    given: meeting with old flat increments/v1.md and v1_meta.json
    when: migrate_flat_increments() called
    then: files moved to increments/my-increment/v1.md and v1_meta.json
    """
    meeting_id = "2026-03-21_test"
    meeting_path = tmp_path / meeting_id
    inc_dir = meeting_path / "increments"
    inc_dir.mkdir(parents=True)

    # Create old flat files
    (inc_dir / "v1.md").write_text("# Old v1", encoding="utf-8")
    (inc_dir / "v1_meta.json").write_text('{"version": 1, "status": "completed"}', encoding="utf-8")
    (inc_dir / "v2.md").write_text("# Old v2", encoding="utf-8")
    (inc_dir / "v2_meta.json").write_text('{"version": 2, "status": "completed"}', encoding="utf-8")

    gen = IncrementGenerator(tmp_path)
    gen.migrate_flat_increments(meeting_path)

    # Old flat files should be gone
    assert not (inc_dir / "v1.md").exists()
    assert not (inc_dir / "v1_meta.json").exists()
    assert not (inc_dir / "v2.md").exists()
    assert not (inc_dir / "v2_meta.json").exists()

    # Files should be in my-increment subdirectory
    target = inc_dir / "my-increment"
    assert target.exists()
    assert (target / "v1.md").read_text(encoding="utf-8") == "# Old v1"
    assert (target / "v1_meta.json").read_text(encoding="utf-8") == '{"version": 1, "status": "completed"}'
    assert (target / "v2.md").read_text(encoding="utf-8") == "# Old v2"
    assert (target / "v2_meta.json").read_text(encoding="utf-8") == '{"version": 2, "status": "completed"}'


def test_migrate_flat_increments_noop_when_no_flat_files(tmp_path):
    """
    given: meeting with increments/ containing only subdirectories (already migrated)
    when: migrate_flat_increments() called
    then: nothing changes
    """
    meeting_id = "2026-03-21_test"
    meeting_path = tmp_path / meeting_id
    inc_dir = meeting_path / "increments" / "my-increment"
    inc_dir.mkdir(parents=True)
    (inc_dir / "v1.md").write_text("# Already migrated", encoding="utf-8")

    gen = IncrementGenerator(tmp_path)
    gen.migrate_flat_increments(meeting_path)

    # File should still be in subdirectory, untouched
    assert (inc_dir / "v1.md").read_text(encoding="utf-8") == "# Already migrated"


def test_migrate_flat_increments_noop_when_no_increments_dir(tmp_path):
    """
    given: meeting with no increments/ directory at all
    when: migrate_flat_increments() called
    then: no error, nothing created
    """
    meeting_path = tmp_path / "2026-03-21_test"
    meeting_path.mkdir(parents=True)

    gen = IncrementGenerator(tmp_path)
    gen.migrate_flat_increments(meeting_path)

    assert not (meeting_path / "increments").exists()


def test_migrate_flat_increments_idempotent(tmp_path):
    """
    given: meeting where migration already ran (files in subdirectory)
    when: migrate_flat_increments() called again
    then: no error, no duplicate files
    """
    meeting_path = tmp_path / "2026-03-21_test"
    inc_dir = meeting_path / "increments"
    target = inc_dir / "my-increment"
    target.mkdir(parents=True)
    (target / "v1.md").write_text("# Migrated", encoding="utf-8")

    gen = IncrementGenerator(tmp_path)
    gen.migrate_flat_increments(meeting_path)

    # Still just the one file in subdirectory
    assert (target / "v1.md").read_text(encoding="utf-8") == "# Migrated"


@pytest.mark.asyncio
async def test_generate_all_runs_migration_before_generating(tmp_path):
    """
    given: meeting with old flat increments/v1.md and a prompts dir
    when: generate_all() called
    then: migration runs first, then generation proceeds with new layout
    """
    meeting_id = "2026-03-21_test"
    meeting_path = _make_meeting(tmp_path, meeting_id)

    # Create old flat increment files
    inc_dir = meeting_path / "increments"
    inc_dir.mkdir(parents=True)
    (inc_dir / "v1.md").write_text("# Old", encoding="utf-8")
    (inc_dir / "v1_meta.json").write_text(
        json.dumps({"version": 1, "status": "completed", "segment_range": [1, 3]}),
        encoding="utf-8",
    )

    # Create prompts
    prompts_dir = tmp_path / "prompts"
    prompts_dir.mkdir()
    (prompts_dir / "my-increment.md").write_text(
        "---\nname: My increment\nfilter: null\n---\n\nUser: {user_name}\n{transcript}",
        encoding="utf-8",
    )

    gen = IncrementGenerator(tmp_path, prompts_dir=prompts_dir)

    mock_provider = MagicMock()
    mock_provider.key = "codex"
    mock_provider.model = "gpt-5.4"
    mock_provider.reasoning_effort = "xhigh"
    mock_provider.complete = AsyncMock(return_value="New content")

    with patch("recog.increment.CodexCliProvider", return_value=mock_provider):
        await gen.generate_all(meeting_id, meeting_path)

    # Old flat files should be migrated
    assert not (inc_dir / "v1.md").exists()
    assert not (inc_dir / "v1_meta.json").exists()

    # Migrated v1 should be in subdirectory
    migrated = inc_dir / "my-increment" / "v1.md"
    assert migrated.exists()
    assert migrated.read_text(encoding="utf-8") == "# Old"

    # Provider should NOT have been called since all segments [1,3] are covered by v1
    # (the meeting has segments 1-3 and v1 covers [1,3])
    mock_provider.complete.assert_not_called()
