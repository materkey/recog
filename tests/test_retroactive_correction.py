"""Tests for retroactive correction of Unknown speaker runs."""
import json
import time
import threading
import pytest
from pathlib import Path
from collections import deque
from unittest.mock import Mock, MagicMock, patch, call

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.live import RecentSegment, RETROACTIVE_MAX_SEGMENTS, RETROACTIVE_MAX_SECONDS
from recog.meeting_storage import MeetingStorage


# --- MeetingStorage.update_segments tests ---


def test_update_segments_corrects_unknown_speaker(tmp_path):
    """
    given: meeting with 5 segments, segments 2-4 are Unknown
    when: update_segments called to change them to "Alice"
    then: JSONL file has segments 2-4 with speaker=Alice
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)

    # Append 5 segments: Alice, Unknown, Unknown, Unknown, Alice
    speakers = ["Alice", "Unknown", "Unknown", "Unknown", "Alice"]
    seg_ids = []
    for s in speakers:
        seg_id = storage.append_segment(
            speaker=s, speaker_id=s, text=f"text by {s}",
            is_local=False, uncertain=(s == "Unknown"),
            uncertain_reason="no_speaker" if s == "Unknown" else ""
        )
        seg_ids.append(seg_id)

    # Correct segments 2-4 (Unknown) to Alice
    updates = [
        {"segment_id": seg_ids[1], "speaker": "Alice", "uncertain": False, "uncertain_reason": ""},
        {"segment_id": seg_ids[2], "speaker": "Alice", "uncertain": False, "uncertain_reason": ""},
        {"segment_id": seg_ids[3], "speaker": "Alice", "uncertain": False, "uncertain_reason": ""},
    ]
    storage.update_segments(updates)

    # Verify JSONL
    transcript_path = storage.current_meeting_path / "transcript.jsonl"
    lines = transcript_path.read_text(encoding="utf-8").strip().split("\n")
    segments = [json.loads(line) for line in lines]

    assert len(segments) == 5
    for seg in segments:
        assert seg["speaker"] == "Alice"
        assert seg["uncertain"] is False
        assert seg["uncertain_reason"] == ""


def test_update_segments_no_op_when_no_updates(tmp_path):
    """
    given: meeting with segments
    when: update_segments called with empty list
    then: JSONL unchanged
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)
    storage.append_segment(speaker="Alice", speaker_id="Alice", text="hello")

    transcript_path = storage.current_meeting_path / "transcript.jsonl"
    before = transcript_path.read_text(encoding="utf-8")

    storage.update_segments([])

    after = transcript_path.read_text(encoding="utf-8")
    assert before == after


def test_update_segments_only_affects_targeted_ids(tmp_path):
    """
    given: meeting with segments from Alice and Bob
    when: update_segments targets only Bob's segment
    then: Alice's segment unchanged, Bob's updated
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)
    id1 = storage.append_segment(speaker="Alice", speaker_id="Alice", text="hi")
    id2 = storage.append_segment(speaker="Unknown", speaker_id="Unknown", text="hello")

    storage.update_segments([
        {"segment_id": id2, "speaker": "Bob", "uncertain": False, "uncertain_reason": ""}
    ])

    transcript_path = storage.current_meeting_path / "transcript.jsonl"
    lines = transcript_path.read_text(encoding="utf-8").strip().split("\n")
    segments = [json.loads(line) for line in lines]

    assert segments[0]["speaker"] == "Alice"
    assert segments[1]["speaker"] == "Bob"
    assert segments[1]["uncertain"] is False


# --- Retroactive correction logic tests ---


def _make_recent_segment(speaker, chunk_time, line_idx=0, seg_id=0, text="hello"):
    return RecentSegment(
        speaker=speaker,
        chunk_time=chunk_time,
        line_idx=line_idx,
        meeting_segment_id=seg_id,
        is_local=False,
        timestamp_display="12:00:00",
        text=text,
    )


def test_retroactive_correction_fixes_unknown_run():
    """
    given: LiveTranscriber with recent segments [Alice, Unknown, Unknown, Bob]
    when: Bob is identified (retroactive correction triggers)
    then: Unknown segments are corrected to Bob in transcript_lines and meeting storage
    """
    # Build a mock LiveTranscriber with minimal attributes
    transcriber = Mock()
    transcriber._state_lock = threading.Lock()
    transcriber.transcript_lines = [
        "\n**Alice** *[12:00:00]*: hello\n",
        "\n**Unknown** *[12:00:03]*: world\n",
        "\n**Unknown** *[12:00:06]*: test\n",
        "\n**Bob** *[12:00:09]*: done\n",
    ]

    now = time.time()
    transcriber._recent_system_segments = deque([
        _make_recent_segment("Alice", now - 9, line_idx=0, seg_id=1),
        _make_recent_segment("Unknown", now - 6, line_idx=1, seg_id=2),
        _make_recent_segment("Unknown", now - 3, line_idx=2, seg_id=3),
        _make_recent_segment("Bob", now, line_idx=3, seg_id=4),
    ], maxlen=RETROACTIVE_MAX_SEGMENTS)

    mock_storage = Mock()
    transcriber._meeting_storage = mock_storage

    mock_bridge = Mock()
    transcriber._ktalk_bridge = mock_bridge

    # Call the actual method
    from recog.live import LiveTranscriber
    LiveTranscriber._retroactive_correct_unknown_run(transcriber, "Bob", now)

    # Verify transcript_lines corrected
    assert "**Bob**" in transcriber.transcript_lines[1]
    assert "**Unknown**" not in transcriber.transcript_lines[1]
    assert "**Bob**" in transcriber.transcript_lines[2]
    assert "**Unknown**" not in transcriber.transcript_lines[2]

    # Verify MeetingStorage.update_segments called
    mock_storage.update_segments.assert_called_once()
    updates = mock_storage.update_segments.call_args[0][0]
    assert len(updates) == 2
    assert all(u["speaker"] == "Bob" for u in updates)
    assert all(u["uncertain"] is False for u in updates)
    seg_ids = {u["segment_id"] for u in updates}
    assert seg_ids == {2, 3}

    # Verify Chrome extension corrections sent
    assert mock_bridge.send_transcript.call_count == 2
    for c in mock_bridge.send_transcript.call_args_list:
        assert c[0][0] == "Bob"  # speaker
        assert c[1]["correction"] is True


def test_retroactive_correction_stops_at_named_speaker():
    """
    given: segments [Alice, Unknown, Bob, Unknown, Unknown, Carol]
    when: Carol identified
    then: only the 2 Unknown segments after Bob are corrected (not the one before Bob)
    """
    transcriber = Mock()
    transcriber._state_lock = threading.Lock()
    transcriber.transcript_lines = [
        "\n**Alice** *[12:00:00]*: a\n",
        "\n**Unknown** *[12:00:03]*: b\n",
        "\n**Bob** *[12:00:06]*: c\n",
        "\n**Unknown** *[12:00:09]*: d\n",
        "\n**Unknown** *[12:00:12]*: e\n",
        "\n**Carol** *[12:00:15]*: f\n",
    ]

    now = time.time()
    transcriber._recent_system_segments = deque([
        _make_recent_segment("Alice", now - 15, line_idx=0, seg_id=1),
        _make_recent_segment("Unknown", now - 12, line_idx=1, seg_id=2),
        _make_recent_segment("Bob", now - 9, line_idx=2, seg_id=3),
        _make_recent_segment("Unknown", now - 6, line_idx=3, seg_id=4),
        _make_recent_segment("Unknown", now - 3, line_idx=4, seg_id=5),
        _make_recent_segment("Carol", now, line_idx=5, seg_id=6),
    ], maxlen=RETROACTIVE_MAX_SEGMENTS)

    mock_storage = Mock()
    transcriber._meeting_storage = mock_storage
    mock_bridge = Mock()
    transcriber._ktalk_bridge = mock_bridge

    from recog.live import LiveTranscriber
    LiveTranscriber._retroactive_correct_unknown_run(transcriber, "Carol", now)

    # Only segments 4 and 5 should be corrected
    mock_storage.update_segments.assert_called_once()
    updates = mock_storage.update_segments.call_args[0][0]
    assert len(updates) == 2
    seg_ids = {u["segment_id"] for u in updates}
    assert seg_ids == {4, 5}

    # Original Unknown at index 1 should NOT be corrected
    assert "**Unknown**" in transcriber.transcript_lines[1]


def test_retroactive_correction_respects_max_time_window():
    """
    given: Unknown segments older than RETROACTIVE_MAX_SECONDS
    when: speaker identified
    then: old Unknown segments are NOT corrected
    """
    transcriber = Mock()
    transcriber._state_lock = threading.Lock()
    transcriber.transcript_lines = [
        "\n**Unknown** *[11:58:00]*: old\n",
        "\n**Unknown** *[12:00:55]*: recent\n",
        "\n**Alice** *[12:00:58]*: new\n",
    ]

    now = time.time()
    transcriber._recent_system_segments = deque([
        # This one is older than RETROACTIVE_MAX_SECONDS (>60s ago)
        _make_recent_segment("Unknown", now - 120, line_idx=0, seg_id=1),
        # This one is within the window
        _make_recent_segment("Unknown", now - 3, line_idx=1, seg_id=2),
        _make_recent_segment("Alice", now, line_idx=2, seg_id=3),
    ], maxlen=RETROACTIVE_MAX_SEGMENTS)

    mock_storage = Mock()
    transcriber._meeting_storage = mock_storage
    mock_bridge = Mock()
    transcriber._ktalk_bridge = mock_bridge

    from recog.live import LiveTranscriber
    LiveTranscriber._retroactive_correct_unknown_run(transcriber, "Alice", now)

    # Only segment 2 should be corrected (segment 1 is too old)
    mock_storage.update_segments.assert_called_once()
    updates = mock_storage.update_segments.call_args[0][0]
    assert len(updates) == 1
    assert updates[0]["segment_id"] == 2

    # Old segment should remain Unknown
    assert "**Unknown**" in transcriber.transcript_lines[0]
    # Recent segment corrected
    assert "**Alice**" in transcriber.transcript_lines[1]


def test_retroactive_correction_no_op_when_no_unknown_run():
    """
    given: segments [Alice, Bob, Carol] with no Unknown
    when: Carol identified
    then: no corrections made
    """
    transcriber = Mock()
    transcriber._state_lock = threading.Lock()
    transcriber.transcript_lines = [
        "\n**Alice** *[12:00:00]*: a\n",
        "\n**Bob** *[12:00:03]*: b\n",
        "\n**Carol** *[12:00:06]*: c\n",
    ]

    now = time.time()
    transcriber._recent_system_segments = deque([
        _make_recent_segment("Alice", now - 6, line_idx=0, seg_id=1),
        _make_recent_segment("Bob", now - 3, line_idx=1, seg_id=2),
        _make_recent_segment("Carol", now, line_idx=2, seg_id=3),
    ], maxlen=RETROACTIVE_MAX_SEGMENTS)

    mock_storage = Mock()
    transcriber._meeting_storage = mock_storage
    mock_bridge = Mock()
    transcriber._ktalk_bridge = mock_bridge

    from recog.live import LiveTranscriber
    LiveTranscriber._retroactive_correct_unknown_run(transcriber, "Carol", now)

    # No updates should be made
    mock_storage.update_segments.assert_not_called()
    mock_bridge.send_transcript.assert_not_called()


def test_retroactive_correction_clears_on_meeting_change():
    """
    given: LiveTranscriber with _recent_system_segments populated
    when: meeting changes (simulated by checking deque is cleared in _invalidate)
    then: _recent_system_segments is empty (correction doesn't cross meetings)
    """
    # Verify that the source code clears _recent_system_segments on meeting change
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()

    # The clearing must happen where _meeting_generation is incremented
    assert "_recent_system_segments.clear()" in source, \
        "_recent_system_segments must be cleared on meeting change"

    # Verify it's near _meeting_generation increment
    gen_pos = source.index("_meeting_generation += 1")
    clear_pos = source.index("_recent_system_segments.clear()")
    # The clear should be within a few lines of the generation increment
    assert abs(clear_pos - gen_pos) < 200, \
        "_recent_system_segments.clear() should be near _meeting_generation increment"
