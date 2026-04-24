"""Tests for TranscriptWatcher rewrite detection (D10 parity).

`MeetingStorage.update_segments` rewrites `transcript.jsonl` atomically
via `tmp.write_text(...) + os.replace(tmp, target)`. Like LiveWatcher,
the per-meeting TranscriptWatcher must detect the rewrite (inode change
or size shrink) and rewind so that subsequent appends continue to reach
`/api/meetings/{id}/watch` clients. Without detection the tailer goes
blind after the first refine / retroactive-correction rewrite.
"""
import asyncio
import json
import os
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_ROOT / "frontend"))

from transcript_watcher import TranscriptWatcher  # noqa: E402


class _FakeWebSocket:
    def __init__(self) -> None:
        self.sent: list[dict] = []

    async def send_text(self, message: str) -> None:
        self.sent.append(json.loads(message))


def _write_jsonl(path: Path, segments: list[dict]) -> None:
    lines = [json.dumps(seg, ensure_ascii=False) for seg in segments]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def _atomic_rewrite(path: Path, segments: list[dict]) -> None:
    tmp = path.with_suffix(".jsonl.tmp")
    lines = [json.dumps(seg, ensure_ascii=False) for seg in segments]
    tmp.write_text("\n".join(lines) + "\n", encoding="utf-8")
    os.replace(str(tmp), str(path))


@pytest.mark.asyncio
async def test_atomic_rewrite_does_not_rebroadcast(tmp_path):
    """given: watcher saw segments 1-2; update_segments rewrites in place
    when: inode changes after os.replace
    then: rewind happens but watermark blocks rebroadcasting already-seen ids
    """
    meeting_id = "m-2026-04-24"
    meeting_dir = tmp_path / meeting_id
    meeting_dir.mkdir()
    path = meeting_dir / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "one"},
        {"id": 2, "text": "two"},
    ])

    watcher = TranscriptWatcher()
    ws = _FakeWebSocket()
    watcher.register(meeting_id, ws)

    await watcher._notify_clients(meeting_id, path)
    assert len(ws.sent) == 1
    assert [s["id"] for s in ws.sent[0]["data"]] == [1, 2]

    _atomic_rewrite(path, [
        {"id": 1, "text": "one refined", "refined": True},
        {"id": 2, "text": "two refined", "refined": True},
    ])
    await watcher._notify_clients(meeting_id, path)

    # Watermark prevents re-broadcast of already-seen ids.
    assert len(ws.sent) == 1


@pytest.mark.asyncio
async def test_rewrite_then_append_broadcasts_new_only(tmp_path):
    """given: rewrite happened; new segment appended afterwards
    when: watcher runs after the append
    then: only the new segment is broadcast — 1-2 blocked by watermark
    """
    meeting_id = "m-rewrite-append"
    meeting_dir = tmp_path / meeting_id
    meeting_dir.mkdir()
    path = meeting_dir / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "one"},
        {"id": 2, "text": "two"},
    ])

    watcher = TranscriptWatcher()
    ws = _FakeWebSocket()
    watcher.register(meeting_id, ws)

    await watcher._notify_clients(meeting_id, path)
    _atomic_rewrite(path, [
        {"id": 1, "text": "one R", "refined": True},
        {"id": 2, "text": "two R", "refined": True},
    ])
    await watcher._notify_clients(meeting_id, path)

    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps({"id": 3, "text": "three"}, ensure_ascii=False) + "\n")

    await watcher._notify_clients(meeting_id, path)

    assert [s["id"] for s in ws.sent[0]["data"]] == [1, 2]
    # Only the second broadcast (the newly-appended id=3) follows.
    assert len(ws.sent) == 2
    assert [s["id"] for s in ws.sent[1]["data"]] == [3]


@pytest.mark.asyncio
async def test_same_size_inode_change_still_detected(tmp_path):
    """given: rewrite preserves length but changes inode
    when: _notify_clients runs after the rewrite
    then: the tailer does not treat mid-file content as brand-new
    """
    meeting_id = "m-same-size"
    meeting_dir = tmp_path / meeting_id
    meeting_dir.mkdir()
    path = meeting_dir / "transcript.jsonl"
    _write_jsonl(path, [
        {"id": 1, "text": "aaa"},
        {"id": 2, "text": "bbb"},
    ])

    watcher = TranscriptWatcher()
    ws = _FakeWebSocket()
    watcher.register(meeting_id, ws)

    await watcher._notify_clients(meeting_id, path)
    old_inode = watcher.file_inodes[str(path)]

    # Same-length rewrite: "aaa" -> "AAA", "bbb" -> "BBB".
    _atomic_rewrite(path, [
        {"id": 1, "text": "AAA"},
        {"id": 2, "text": "BBB"},
    ])

    await watcher._notify_clients(meeting_id, path)

    assert len(ws.sent) == 1  # no rebroadcast
    assert watcher.file_inodes[str(path)] != old_inode


@pytest.mark.asyncio
async def test_unregister_last_client_clears_per_file_state(tmp_path):
    """given: watcher accumulated per-file state for meetings A and B
    when: unregister the only client of meeting A
    then: A's file_positions / file_inodes / seen_segment_ids entries are
    dropped, but B's survive (prevents slow unbounded growth)
    """
    meeting_a = "m-A"
    meeting_b = "m-B"
    path_a = tmp_path / meeting_a / "transcript.jsonl"
    path_a.parent.mkdir()
    _write_jsonl(path_a, [{"id": 1, "text": "a"}])
    path_b = tmp_path / meeting_b / "transcript.jsonl"
    path_b.parent.mkdir()
    _write_jsonl(path_b, [{"id": 1, "text": "b"}])

    watcher = TranscriptWatcher()
    ws_a = _FakeWebSocket()
    ws_b = _FakeWebSocket()
    watcher.register(meeting_a, ws_a)
    watcher.register(meeting_b, ws_b)

    await watcher._notify_clients(meeting_a, path_a)
    await watcher._notify_clients(meeting_b, path_b)
    assert str(path_a) in watcher.file_positions
    assert str(path_b) in watcher.file_positions

    watcher.unregister(meeting_a, ws_a)

    assert str(path_a) not in watcher.file_positions
    assert str(path_a) not in watcher.file_inodes
    assert str(path_a) not in watcher.seen_segment_ids
    # B untouched — still has a registered client.
    assert str(path_b) in watcher.file_positions
    assert str(path_b) in watcher.file_inodes
    assert str(path_b) in watcher.seen_segment_ids


@pytest.mark.asyncio
async def test_unregister_non_last_client_keeps_state(tmp_path):
    """given: two clients watching the same meeting
    when: only one disconnects
    then: per-file state is preserved (the other client still needs it)
    """
    meeting_id = "m-two-clients"
    path = tmp_path / meeting_id / "transcript.jsonl"
    path.parent.mkdir()
    _write_jsonl(path, [{"id": 1, "text": "a"}])

    watcher = TranscriptWatcher()
    ws1 = _FakeWebSocket()
    ws2 = _FakeWebSocket()
    watcher.register(meeting_id, ws1)
    watcher.register(meeting_id, ws2)

    await watcher._notify_clients(meeting_id, path)
    assert str(path) in watcher.file_positions

    watcher.unregister(meeting_id, ws1)

    # State retained for the still-connected ws2.
    assert str(path) in watcher.file_positions
    assert str(path) in watcher.file_inodes
    assert str(path) in watcher.seen_segment_ids
