"""Tests for recog/web/server.py LiveWatcher rewrite detection (D10).

`MeetingStorage.update_segments` rewrites `transcript.jsonl` via
`tmp.write_text(...) + os.replace(tmp, target)`. That atomic swap changes
the inode; the tailer's old `file_position` points past the new EOF.
Without rewrite detection the tailer goes blind to all subsequent
appends. With detection we reset to 0 and filter duplicates via a
`_seen_segment_ids` watermark so already-broadcast rows are not resent.
"""

import asyncio
import json
import os
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent

from recog.web.live_watcher import LiveWatcher  # noqa: E402


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
async def test_set_current_meeting_resets_watermark_on_switch(tmp_path):
    """given: watcher has watermark from meeting A
    when: set_current_meeting(B) is called (as live_stream ws connect does)
    then: watermark, file_position, and inode are cleared so stale ids
    from A cannot silently drop B's rows (ids restart at 1 per meeting)
    """
    path_a = tmp_path / "a" / "transcript.jsonl"
    path_a.parent.mkdir()
    _write_jsonl(path_a, [{"id": 1, "text": "a1"}, {"id": 2, "text": "a2"}])

    watcher, _broadcasts = _make_watcher()
    watcher.current_meeting_id = "meeting-a"
    await watcher._notify_new_segments(path_a)
    assert watcher._seen_segment_ids == {1, 2}
    assert watcher.file_position > 0
    assert watcher._file_inode is not None

    watcher.set_current_meeting("meeting-b")

    assert watcher.current_meeting_id == "meeting-b"
    assert watcher._seen_segment_ids == set()
    assert watcher.file_position == 0
    assert watcher._file_inode is None


@pytest.mark.asyncio
async def test_set_current_meeting_same_id_keeps_watermark(tmp_path):
    """given: watcher has watermark from meeting A
    when: set_current_meeting(A) — same id, as happens on repeat WS connect
    then: watermark / position are preserved (no unnecessary reset)
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [{"id": 1, "text": "one"}])

    watcher, _broadcasts = _make_watcher()
    watcher.current_meeting_id = "meeting-a"
    await watcher._notify_new_segments(path)
    position = watcher.file_position
    seen = set(watcher._seen_segment_ids)
    inode = watcher._file_inode

    watcher.set_current_meeting("meeting-a")

    assert watcher.file_position == position
    assert watcher._seen_segment_ids == seen
    assert watcher._file_inode == inode


@pytest.mark.asyncio
async def test_seed_post_init_watermark_prevents_duplicate_broadcast(tmp_path):
    """given: fresh WS connect to meeting with existing segments; watcher
    state reset by set_current_meeting; init payload already sent via WS
    carrying ids 1-3
    when: the watchdog fires on_modified after a new append (id 4) — the
    first time the watcher has read this file post-reset
    then: only id 4 is broadcast; ids 1-3 are filtered out so the webapp
    does not render duplicates of the init payload

    Regression for the duplicate-on-connect bug introduced when
    set_current_meeting started resetting file_position to 0 without a
    matching seed of already-delivered segment_ids.
    """
    path = tmp_path / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "a"},
        {"id": 2, "text": "b"},
        {"id": 3, "text": "c"},
    ])

    watcher, broadcasts = _make_watcher()
    # Simulate live_stream endpoint: reset + seed watermark from init payload.
    watcher.set_current_meeting("meeting-x")
    watcher.seed_post_init_watermark([1, 2, 3])

    # Now transcription appends id 4.
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": 4, "text": "d"}, ensure_ascii=False) + "\n")

    await watcher._notify_new_segments(path)

    assert len(broadcasts) == 1
    assert [s["id"] for s in broadcasts[0]["data"]] == [4]
    assert watcher._seen_segment_ids == {1, 2, 3, 4}


@pytest.mark.asyncio
async def test_seed_post_init_watermark_ignores_non_int_ids(tmp_path):
    """given: init payload with event rows that have no `id` field
    when: seed_post_init_watermark is called with a mix of ints and None
    then: only int ids are added; None/str/missing are silently dropped
    and do not poison the set with un-comparable values
    """
    watcher, _ = _make_watcher()
    watcher.seed_post_init_watermark([1, None, 2, "x", 3])
    assert watcher._seen_segment_ids == {1, 2, 3}


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


class _Storage:
    def __init__(self, base_path: Path, active: list[dict] | None = None):
        self.base_path = base_path
        self._active = active or []

    def list_active_meetings(self):
        return self._active


class _Event:
    def __init__(self, path: Path, is_directory: bool = False):
        self.src_path = str(path)
        self.is_directory = is_directory


def _meeting_meta(base: Path, meeting_id: str, source: str) -> Path:
    path = base / meeting_id / "meta.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"source": source}), encoding="utf-8")
    return path


@pytest.fixture
def storage(tmp_path, monkeypatch):
    from recog.web import server

    storage = _Storage(tmp_path)
    monkeypatch.setattr(server, "get_storage", lambda: storage)
    return storage


async def _drain() -> None:
    for _ in range(5):
        await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_current_orphan_promoted_to_ktalk_when_checking_source_then_refine_flag_is_resent(
    storage,
):
    _meeting_meta(storage.base_path, "m1", "ktalk")
    watcher, broadcasts = _make_watcher()
    watcher.set_current_meeting("m1", "orphan")

    await watcher._check_source_change("m1")
    await watcher._check_source_change("m1")

    assert broadcasts == [{"type": "refine_state", "meeting_id": "m1", "refine_enabled": True}]
    assert watcher.current_meeting_source == "ktalk"


@pytest.mark.asyncio
async def test_other_or_missing_meeting_when_checking_source_then_nothing_is_sent(storage):
    _meeting_meta(storage.base_path, "other", "ktalk")
    watcher, broadcasts = _make_watcher()
    watcher.set_current_meeting("m1", "orphan")

    await watcher._check_source_change("other")
    await watcher._check_source_change("m1")

    assert broadcasts == []
    assert watcher.current_meeting_source == "orphan"


@pytest.mark.asyncio
async def test_meta_of_current_meeting_modified_when_watching_then_source_change_is_checked(
    storage,
):
    meta = _meeting_meta(storage.base_path, "m1", "ktalk")
    other = _meeting_meta(storage.base_path, "m2", "ktalk")
    watcher, broadcasts = _make_watcher()
    watcher.set_current_meeting("m1", "orphan")
    watcher.set_loop(asyncio.get_running_loop())

    watcher.on_modified(_Event(meta.parent, is_directory=True))
    watcher.on_modified(_Event(other))
    await _drain()
    assert broadcasts == []

    watcher.on_modified(_Event(meta))
    await _drain()
    assert broadcasts == [{"type": "refine_state", "meeting_id": "m1", "refine_enabled": True}]


@pytest.mark.asyncio
async def test_index_modified_when_watching_then_new_current_meeting_is_announced(storage):
    storage._active = [{"id": "m2", "source": "ktalk"}]
    watcher, broadcasts = _make_watcher()
    watcher.set_current_meeting("m1", "orphan")
    watcher.set_loop(asyncio.get_running_loop())

    watcher.on_modified(_Event(storage.base_path / "index.json"))
    await _drain()

    assert broadcasts == [
        {
            "type": "meeting_change",
            "meeting": {"id": "m2", "source": "ktalk"},
            "refine_enabled": True,
        }
    ]
    assert watcher.current_meeting_id == "m2"
