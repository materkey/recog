"""Tests for MeetingStorage JSONL schema + update_segments semantics (Task 10)."""
import json
import sys
from dataclasses import asdict
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from meeting_storage import MeetingStorage, TranscriptSegment


def _read_segments(storage: MeetingStorage) -> list[dict]:
    transcript_path = storage.current_meeting_path / "transcript.jsonl"
    content = transcript_path.read_text(encoding="utf-8").strip()
    if not content:
        return []
    return [json.loads(line) for line in content.split("\n") if line]


def _read_meta(storage: MeetingStorage) -> dict:
    return json.loads(
        (storage.current_meeting_path / "meta.json").read_text(encoding="utf-8")
    )


def test_transcript_segment_default_refined_false():
    """
    given: a freshly constructed TranscriptSegment with no explicit refined kwarg
    when: inspecting the field
    then: refined defaults to False (backwards-compatible with pre-§5 rows)
    """
    seg = TranscriptSegment(
        id=1,
        ts="2026-04-24T10:00:00+00:00",
        ts_display="10:00:00",
        speaker="Alice",
        speaker_id="Alice",
        text="hello",
        is_local=False,
        cont=False,
        uncertain=False,
    )
    assert seg.refined is False
    assert asdict(seg)["refined"] is False


def test_update_segments_rewrites_text_and_sets_refined_true(tmp_path):
    """
    given: meeting with one segment, live text
    when: update_segments called with {text: <refined>} (no speaker key)
    then: JSONL row has new text AND refined=True
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)
    seg_id = storage.append_segment(
        speaker="Alice", speaker_id="Alice", text="live raw text"
    )

    storage.update_segments([
        {"segment_id": seg_id, "text": "refined and tidy text"}
    ])

    segments = _read_segments(storage)
    assert len(segments) == 1
    assert segments[0]["text"] == "refined and tidy text"
    assert segments[0]["refined"] is True


def test_update_segments_text_rewrite_updates_words_count(tmp_path):
    """
    given: meta.stats.words_count was populated by append_segment
    when: refine rewrites one segment with a different number of words
    then: words_count is adjusted by the text delta
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)
    seg_id = storage.append_segment(
        speaker="Alice", speaker_id="Alice", text="one two three"
    )
    storage.append_segment(
        speaker="Bob", speaker_id="Bob", text="four five"
    )
    assert _read_meta(storage)["stats"]["words_count"] == 5

    storage.update_segments([
        {"segment_id": seg_id, "text": "one"}
    ])

    assert _read_meta(storage)["stats"]["words_count"] == 3


def test_update_segments_preserves_other_fields(tmp_path):
    """
    given: meeting segment carrying speaker, uncertain=True, audio_offset_ms
    when: update_segments writes a text-only refine
    then: speaker, uncertain, uncertain_reason, audio_offset_ms stay untouched
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)
    seg_id = storage.append_segment(
        speaker="Bob",
        speaker_id="Bob",
        text="live",
        is_local=False,
        is_continuation=False,
        uncertain=True,
        uncertain_reason="no_speaker",
        audio_offset_ms=12345,
    )

    storage.update_segments([
        {"segment_id": seg_id, "text": "refined"}
    ])

    segments = _read_segments(storage)
    assert segments[0]["text"] == "refined"
    assert segments[0]["refined"] is True
    assert segments[0]["speaker"] == "Bob"
    assert segments[0]["speaker_id"] == "Bob"
    assert segments[0]["uncertain"] is True
    assert segments[0]["uncertain_reason"] == "no_speaker"
    assert segments[0]["audio_offset_ms"] == 12345


def test_update_segments_without_text_key_still_works(tmp_path):
    """
    given: meeting with Unknown segments
    when: update_segments called with speaker/uncertain keys (legacy retroactive path)
    then: speaker corrected, refined flag NOT set (text untouched)
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)
    seg_id = storage.append_segment(
        speaker="Unknown",
        speaker_id="Unknown",
        text="hello",
        uncertain=True,
        uncertain_reason="no_speaker",
    )

    storage.update_segments([
        {
            "segment_id": seg_id,
            "speaker": "Alice",
            "uncertain": False,
            "uncertain_reason": "",
        }
    ])

    segments = _read_segments(storage)
    assert segments[0]["speaker"] == "Alice"
    assert segments[0]["speaker_id"] == "Alice"
    assert segments[0]["uncertain"] is False
    assert segments[0]["uncertain_reason"] == ""
    assert segments[0]["text"] == "hello"
    # refined must NOT flip when only speaker was corrected
    assert segments[0].get("refined", False) is False


def test_update_segments_speaker_and_text_combined(tmp_path):
    """
    given: a segment
    when: update_segments passes both speaker and text in one row
    then: both are applied; refined=True is set (because text is present)
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)
    seg_id = storage.append_segment(
        speaker="Unknown", speaker_id="Unknown", text="live"
    )

    storage.update_segments([
        {
            "segment_id": seg_id,
            "speaker": "Carol",
            "uncertain": False,
            "uncertain_reason": "",
            "text": "refined",
        }
    ])

    segments = _read_segments(storage)
    assert segments[0]["speaker"] == "Carol"
    assert segments[0]["speaker_id"] == "Carol"
    assert segments[0]["text"] == "refined"
    assert segments[0]["refined"] is True


def test_reload_of_row_with_refined_true_roundtrips(tmp_path):
    """
    given: JSONL row was refined (refined=True written on disk)
    when: reload via get_meeting()
    then: segment dict round-trips with refined=True
    """
    storage = MeetingStorage(base_path=tmp_path)
    meeting_id = storage.start_session(ktalk=True)
    seg_id = storage.append_segment(
        speaker="Alice", speaker_id="Alice", text="live"
    )
    storage.update_segments([
        {"segment_id": seg_id, "text": "refined"}
    ])

    data = storage.get_meeting(meeting_id)
    assert data is not None
    segments = data["segments"]
    assert len(segments) == 1
    assert segments[0]["text"] == "refined"
    assert segments[0]["refined"] is True


def test_reload_of_legacy_row_without_refined_key(tmp_path):
    """
    given: a pre-§5 JSONL row on disk (no refined key at all)
    when: reload via get_meeting()
    then: segment dict still loads; refined absent from row is handled by readers
    """
    storage = MeetingStorage(base_path=tmp_path)
    meeting_id = storage.start_session(ktalk=True)

    # Hand-craft a legacy row (no `refined` key) — simulates pre-§5 transcripts.
    transcript_path = storage.current_meeting_path / "transcript.jsonl"
    legacy_row = {
        "id": 1,
        "ts": "2026-04-24T10:00:00+00:00",
        "ts_display": "10:00:00",
        "speaker": "Legacy",
        "speaker_id": "Legacy",
        "text": "old school",
        "is_local": False,
        "cont": False,
        "uncertain": False,
        "uncertain_reason": "",
        "audio_offset_ms": 0,
    }
    transcript_path.write_text(
        json.dumps(legacy_row, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )

    data = storage.get_meeting(meeting_id)
    assert data is not None
    segments = data["segments"]
    assert len(segments) == 1
    # Dict deserialisation tolerates missing key; readers default to False.
    assert segments[0].get("refined", False) is False
    assert segments[0]["text"] == "old school"


def _make_ended_meeting_dir(base: Path, meeting_id: str, prev=None, next_=None) -> Path:
    """Create a minimal ENDED meeting dir with prev/next links (for delete tests)."""
    d = base / meeting_id
    d.mkdir(parents=True)
    meta = {
        "id": meeting_id,
        "ended_at": "2026-07-04T10:00:00+00:00",
        "prev_meeting_id": prev,
        "next_meeting_id": next_,
    }
    (d / "meta.json").write_text(json.dumps(meta, ensure_ascii=False), encoding="utf-8")
    return d


def test_delete_removes_directory_and_index_entry_for_an_ended_meeting(tmp_path):
    """
    given: an ended meeting with at least one segment (so it is indexed)
    when: delete_meeting is called
    then: returns True, its directory is gone and its index.json entry removed
    """
    storage = MeetingStorage(base_path=tmp_path)
    meeting_id = storage.start_session(ktalk=True)
    storage.append_segment(speaker="Alice", speaker_id="Alice", text="hi there")
    storage.update_title("Test Meeting")  # avoid external title generation on end
    storage.end_session()

    meeting_dir = tmp_path / meeting_id
    index_path = tmp_path / "index.json"
    assert meeting_dir.is_dir()
    index = json.loads(index_path.read_text(encoding="utf-8"))
    assert any(m["id"] == meeting_id for m in index["meetings"])

    assert storage.delete_meeting(meeting_id) is True

    assert not meeting_dir.exists()
    index = json.loads(index_path.read_text(encoding="utf-8"))
    assert not any(m["id"] == meeting_id for m in index["meetings"])


def test_delete_returns_false_for_unknown_meeting_id(tmp_path):
    """
    given: a storage with no such meeting
    when: delete_meeting is called with an unknown id
    then: returns False (maps to 404 at the HTTP layer)
    """
    storage = MeetingStorage(base_path=tmp_path)
    assert storage.delete_meeting("2099-01-01_000000_deadbeef") is False


def test_delete_raises_value_error_for_an_active_meeting(tmp_path):
    """
    given: an active (still recording, ended_at is None) meeting
    when: delete_meeting is called on it
    then: raises ValueError and the directory is left intact
    """
    storage = MeetingStorage(base_path=tmp_path)
    meeting_id = storage.start_session(ktalk=True)

    with pytest.raises(ValueError):
        storage.delete_meeting(meeting_id)

    assert (tmp_path / meeting_id).is_dir()


def test_delete_relinks_prev_and_next_neighbors(tmp_path):
    """
    given: an ended chain A -> B -> C linked via prev/next meta fields
    when: the middle meeting B is deleted
    then: B's dir is gone and A.next == C, C.prev == A
    """
    storage = MeetingStorage(base_path=tmp_path)
    a, b, c = "2026-07-04_090000_aaaaaaaa", "2026-07-04_100000_bbbbbbbb", "2026-07-04_110000_cccccccc"
    _make_ended_meeting_dir(tmp_path, a, prev=None, next_=b)
    _make_ended_meeting_dir(tmp_path, b, prev=a, next_=c)
    _make_ended_meeting_dir(tmp_path, c, prev=b, next_=None)

    assert storage.delete_meeting(b) is True

    assert not (tmp_path / b).exists()
    a_meta = json.loads((tmp_path / a / "meta.json").read_text(encoding="utf-8"))
    c_meta = json.loads((tmp_path / c / "meta.json").read_text(encoding="utf-8"))
    assert a_meta["next_meeting_id"] == c
    assert c_meta["prev_meeting_id"] == a
