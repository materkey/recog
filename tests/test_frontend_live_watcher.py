"""Tests for frontend/server.py LiveWatcher rewrite detection (D10).

`MeetingStorage.update_segments` rewrites `transcript.jsonl` via
`tmp.write_text(...) + os.replace(tmp, target)`. That atomic swap changes
the inode; the tailer's old `file_position` points past the new EOF.
Without rewrite detection the tailer goes blind to all subsequent
appends. With detection we reset to 0 and filter duplicates via a
`_seen_segment_ids` watermark so already-broadcast rows are not resent.
"""
import json
import os
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_ROOT / "frontend"))

from live_watcher import LiveWatcher  # noqa: E402


def _write_jsonl(path: Path, segments: list[dict]) -> None:
    lines = [json.dumps(seg, ensure_ascii=False) for seg in segments]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def _atomic_rewrite(path: Path, segments: list[dict]) -> None:
    tmp = path.with_suffix(".jsonl.tmp")
    lines = [json.dumps(seg, ensure_ascii=False) for seg in segments]
    tmp.write_text("\n".join(lines) + "\n", encoding="utf-8")
    os.replace(str(tmp), str(path))


def _make_watcher() -> tuple[LiveWatcher, list[dict]]:
    watcher = LiveWatcher()
    broadcasts: list[dict] = []

    async def _capture(message: str) -> None:
        broadcasts.append(json.loads(message))

    watcher._broadcast = _capture  # type: ignore[assignment]
    return watcher, broadcasts


@pytest.mark.asyncio
async def test_initial_read_broadcasts_segments(tmp_path):
    """given: fresh transcript.jsonl with 2 segments
    when: _notify_new_segments runs the first time
    then: both segments are broadcast and ids recorded in watermark
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "hello", "speaker": "A"},
        {"id": 2, "text": "world", "speaker": "B"},
    ])

    watcher, broadcasts = _make_watcher()
    await watcher._notify_new_segments(path)

    assert len(broadcasts) == 1
    assert broadcasts[0]["type"] == "segments"
    assert [s["id"] for s in broadcasts[0]["data"]] == [1, 2]
    assert watcher._seen_segment_ids == {1, 2}
    assert watcher.file_position > 0
    assert watcher._file_inode is not None


@pytest.mark.asyncio
async def test_append_only_new_segments_broadcast(tmp_path):
    """given: watcher already saw segments 1-2 via pure append
    when: the file gets a new segment appended (no rewrite)
    then: only the appended segment is broadcast
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "a"},
        {"id": 2, "text": "b"},
    ])

    watcher, broadcasts = _make_watcher()
    await watcher._notify_new_segments(path)

    # Append a new line without rewriting (simulates MeetingStorage.append_segment).
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": 3, "text": "c"}, ensure_ascii=False) + "\n")

    await watcher._notify_new_segments(path)

    assert len(broadcasts) == 2
    assert [s["id"] for s in broadcasts[1]["data"]] == [3]
    assert watcher._seen_segment_ids == {1, 2, 3}


@pytest.mark.asyncio
async def test_atomic_rewrite_does_not_rebroadcast(tmp_path):
    """given: watcher saw segments 1-3, then update_segments rewrites in place
    when: rewrite shrinks size OR changes inode
    then: watcher detects rewrite, rewinds, but watermark blocks duplicates
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "one", "refined": False},
        {"id": 2, "text": "two", "refined": False},
        {"id": 3, "text": "three", "refined": False},
    ])

    watcher, broadcasts = _make_watcher()
    await watcher._notify_new_segments(path)
    assert len(broadcasts) == 1
    assert watcher.file_position > 0
    original_inode = watcher._file_inode

    # Simulate MeetingStorage.update_segments: rewrite with shorter text
    # for id=2 (shrinks size). Atomic os.replace changes inode.
    _atomic_rewrite(path, [
        {"id": 1, "text": "one", "refined": False},
        {"id": 2, "text": "2", "refined": True},
        {"id": 3, "text": "three", "refined": False},
    ])

    await watcher._notify_new_segments(path)

    # No second broadcast: all three ids are already in the watermark.
    assert len(broadcasts) == 1
    # File position advanced to the new EOF, inode updated to the new one.
    assert watcher._file_inode != original_inode
    assert watcher.file_position > 0


@pytest.mark.asyncio
async def test_atomic_rewrite_then_append_broadcasts_new_only(tmp_path):
    """given: rewrite happened; watcher has correctly rewound
    when: a new segment 4 is appended after the rewrite
    then: only segment 4 is broadcast (1-3 blocked by watermark)
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "one"},
        {"id": 2, "text": "two"},
    ])

    watcher, broadcasts = _make_watcher()
    await watcher._notify_new_segments(path)

    # Rewrite (update_segments-style): refined text, same ids.
    _atomic_rewrite(path, [
        {"id": 1, "text": "one refined", "refined": True},
        {"id": 2, "text": "two refined", "refined": True},
    ])
    await watcher._notify_new_segments(path)

    # Now append a new segment AFTER the rewrite.
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": 3, "text": "three"}, ensure_ascii=False) + "\n")

    await watcher._notify_new_segments(path)

    broadcast_ids = [
        [s["id"] for s in b["data"]] for b in broadcasts
    ]
    assert broadcast_ids == [[1, 2], [3]]
    assert watcher._seen_segment_ids == {1, 2, 3}


@pytest.mark.asyncio
async def test_rewrite_same_size_but_inode_changed(tmp_path):
    """given: rewrite preserves length but changes inode (text swap same len)
    when: _notify_new_segments runs after the rewrite
    then: inode-change detection triggers rewind + watermark filtering
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "aaa"},
        {"id": 2, "text": "bbb"},
    ])

    watcher, broadcasts = _make_watcher()
    await watcher._notify_new_segments(path)
    old_inode = watcher._file_inode

    # Same-length rewrite: "aaa" -> "AAA", "bbb" -> "BBB".
    _atomic_rewrite(path, [
        {"id": 1, "text": "AAA"},
        {"id": 2, "text": "BBB"},
    ])

    await watcher._notify_new_segments(path)

    assert len(broadcasts) == 1  # still just the first broadcast
    assert watcher._file_inode != old_inode


@pytest.mark.asyncio
async def test_meeting_change_clears_watermark(tmp_path):
    """given: watcher in one meeting accumulated watermark ids
    when: simulated meeting switch resets state (as _check_current_meeting does)
    then: the next notify treats new file as fresh (no residual watermark)
    """
    path_a = tmp_path / "a" / "transcript.jsonl"
    path_a.parent.mkdir()
    _write_jsonl(path_a, [{"id": 1, "text": "meeting-a"}])

    watcher, broadcasts = _make_watcher()
    await watcher._notify_new_segments(path_a)
    assert watcher._seen_segment_ids == {1}

    # Simulate _check_current_meeting picking up a new meeting.
    watcher.current_meeting_id = "meeting-b"
    watcher.file_position = 0
    watcher._seen_segment_ids = set()
    watcher._file_inode = None

    path_b = tmp_path / "b" / "transcript.jsonl"
    path_b.parent.mkdir()
    _write_jsonl(path_b, [
        {"id": 1, "text": "meeting-b-row-1"},
        {"id": 2, "text": "meeting-b-row-2"},
    ])

    await watcher._notify_new_segments(path_b)

    assert len(broadcasts) == 2
    assert [s["id"] for s in broadcasts[1]["data"]] == [1, 2]
    assert watcher._seen_segment_ids == {1, 2}


@pytest.mark.asyncio
async def test_missing_id_segments_still_broadcast(tmp_path):
    """given: a line without id field (rare but valid in some event rows)
    when: watcher processes the file
    then: the row is broadcast but not added to the watermark (no dedup possible)
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "has-id"},
        {"event_type": "session_pause", "message": "paused"},
    ])

    watcher, broadcasts = _make_watcher()
    await watcher._notify_new_segments(path)

    assert len(broadcasts) == 1
    assert len(broadcasts[0]["data"]) == 2
    assert watcher._seen_segment_ids == {1}
