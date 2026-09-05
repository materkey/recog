"""Tests for MeetingStorage JSONL schema + update_segments semantics (Task 10)."""
import json
import os
import stat
import sys
import time
from dataclasses import asdict
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.meeting_storage import MeetingStorage, TranscriptSegment


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


def test_on_meeting_join_element_url_uses_fragment_room_id(tmp_path):
    """
    given: an Element Call meeting URL carrying the room id in the fragment
           (empty path) — https://element.example.org/#/room/!abc
    when: on_meeting_join is called with platform="matrix"
    then: conference.live_id is the bang room id "!abc", not the empty path id
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)

    storage.on_meeting_join(
        "https://element.example.org/#/room/!abc", platform="matrix"
    )

    meta = _read_meta(storage)
    assert meta["conference"]["live_id"] == "!abc"


def test_on_meeting_join_element_alias_form_keeps_leading_hash(tmp_path):
    """
    given: the alias room form #/room/#recog-spike:... where the room id itself
           starts with '#' (parsed.fragment would drop that leading '#')
    when: on_meeting_join is called with platform="matrix"
    then: live_id is "#recog-spike:matrix.example.org" — the regex runs on
          the full URL string, preserving the leading '#'
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)

    storage.on_meeting_join(
        "https://element.example.org/#/room/#recog-spike:matrix.example.org",
        platform="matrix",
    )

    meta = _read_meta(storage)
    assert meta["conference"]["live_id"] == "#recog-spike:matrix.example.org"


def test_on_meeting_join_ktalk_path_id_unchanged(tmp_path):
    """
    given: a KTalk meeting URL with the id in the path
    when: on_meeting_join is called with platform="ktalk"
    then: live_id is still the path-derived id (fragment fallback must not fire
          when the path already yields an id)
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)

    storage.on_meeting_join(
        "https://example.ktalk.ru/abcd1234wxyz", platform="ktalk"
    )

    meta = _read_meta(storage)
    assert meta["conference"]["live_id"] == "abcd1234wxyz"


def test_on_meeting_join_no_path_or_fragment_id_leaves_live_id_empty(tmp_path):
    """
    given: a URL with neither a path id nor a #/room/ fragment
    when: on_meeting_join is called
    then: live_id is falsy (empty string / None) — no spurious fallback match
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)

    storage.on_meeting_join("https://element.example.org/", platform="matrix")

    meta = _read_meta(storage)
    assert not meta["conference"]["live_id"]


# --- DW-7: _save_json atomicity (temp file + os.replace) ---


def test_save_json_overwrites_existing_file_without_tmp_leftovers(tmp_path):
    """
    given: a target file that already contains old JSON
    when: _save_json writes new data to the same path
    then: the file contains exactly the new JSON and no *.tmp files remain
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    target.write_text(json.dumps({"old": True}), encoding="utf-8")

    storage._save_json(target, {"new": "данные"})

    assert json.loads(target.read_text(encoding="utf-8")) == {"new": "данные"}
    assert list(tmp_path.glob("*.tmp")) == []


def test_save_json_first_write_creates_target(tmp_path):
    """
    given: the target file does not exist yet
    when: _save_json is called
    then: the target is created with the new JSON and no *.tmp files remain
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"

    storage._save_json(target, {"id": 1})

    assert json.loads(target.read_text(encoding="utf-8")) == {"id": 1}
    assert list(tmp_path.glob("*.tmp")) == []


def test_save_json_preserves_format(tmp_path):
    """
    given: data with non-ASCII content
    when: _save_json writes it
    then: output matches json.dumps(..., ensure_ascii=False, indent=2) exactly
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    data = {"title": "Встреча", "nested": {"a": 1}}

    storage._save_json(target, data)

    expected = json.dumps(data, ensure_ascii=False, indent=2)
    assert target.read_text(encoding="utf-8") == expected


def test_save_json_concurrent_writers_use_unique_same_directory_temps(
    tmp_path, monkeypatch
):
    """
    given: two writers targeting the same existing JSON file
    when: both reach os.replace concurrently
    then: each uses a different temporary file in the target directory
    """
    import threading

    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    target.write_text("{}", encoding="utf-8")
    real_replace = os.replace
    replace_barrier = threading.Barrier(2)
    source_paths: list[Path] = []
    source_lock = threading.Lock()
    errors: list[BaseException] = []

    def synchronized_replace(src, dst):
        with source_lock:
            source_paths.append(Path(src))
        replace_barrier.wait(timeout=10)
        real_replace(src, dst)

    def writer(payload):
        try:
            storage._save_json(target, payload)
        except BaseException as exc:  # pragma: no cover - failure path
            errors.append(exc)

    monkeypatch.setattr("recog.meeting_storage.os.replace", synchronized_replace)
    threads = [
        threading.Thread(target=writer, args=({"writer": 1},), daemon=True),
        threading.Thread(target=writer, args=({"writer": 2},), daemon=True),
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=30)

    assert all(not thread.is_alive() for thread in threads), "writer thread hung"
    assert not errors
    assert len(source_paths) == 2
    assert len(set(source_paths)) == 2
    assert all(source.parent == target.parent for source in source_paths)
    assert json.loads(target.read_text(encoding="utf-8")) in (
        {"writer": 1},
        {"writer": 2},
    )
    assert list(tmp_path.glob("*.tmp")) == []


def test_save_json_failure_leaves_original_intact_and_no_tmp(tmp_path, monkeypatch):
    """
    given: a target file with valid old JSON and os.replace that raises
    when: _save_json fails partway
    then: the original content is untouched, no temp file remains,
          and the exception propagates to the caller
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    old = {"still": "here"}
    target.write_text(json.dumps(old, ensure_ascii=False, indent=2), encoding="utf-8")

    def boom(src, dst):
        raise OSError("simulated replace failure")

    monkeypatch.setattr("recog.meeting_storage.os.replace", boom)

    with pytest.raises(OSError, match="simulated replace failure"):
        storage._save_json(target, {"new": "data"})

    assert json.loads(target.read_text(encoding="utf-8")) == old
    assert list(tmp_path.glob("*.tmp")) == []


def test_save_json_first_write_replace_failure_leaves_target_absent(
    tmp_path, monkeypatch
):
    """
    given: no target file and os.replace that raises
    when: _save_json attempts the first write
    then: the target stays absent, the temp is removed, and the error propagates
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"

    def boom(src, dst):
        raise OSError("simulated replace failure")

    monkeypatch.setattr("recog.meeting_storage.os.replace", boom)

    with pytest.raises(OSError, match="simulated replace failure"):
        storage._save_json(target, {"new": "data"})

    assert not target.exists()
    assert list(tmp_path.glob("*.tmp")) == []


def test_save_json_write_failure_leaves_original_intact_and_no_tmp(
    tmp_path, monkeypatch
):
    """
    given: an existing target and a temporary file whose write raises
    when: _save_json fails before os.replace
    then: the original survives, the temp is removed, and the error propagates
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    old = {"still": "here"}
    target.write_text(json.dumps(old), encoding="utf-8")
    real_fdopen = os.fdopen

    class FailingWriter:
        def __init__(self, file_obj):
            self.file_obj = file_obj

        def __enter__(self):
            return self

        def __exit__(self, exc_type, exc, tb):
            self.file_obj.close()

        def write(self, text):
            raise OSError("simulated write failure")

    def failing_fdopen(fd, *args, **kwargs):
        return FailingWriter(real_fdopen(fd, *args, **kwargs))

    monkeypatch.setattr("recog.meeting_storage.os.fdopen", failing_fdopen)

    with pytest.raises(OSError, match="simulated write failure"):
        storage._save_json(target, {"new": "data"})

    assert json.loads(target.read_text(encoding="utf-8")) == old
    assert list(tmp_path.glob("*.tmp")) == []


def test_save_json_concurrent_writer_reader_never_torn(tmp_path):
    """
    given: one thread looping _save_json on a path while another loops
           json.loads(path.read_text())
    when: they run for many iterations
    then: every read parses valid JSON equal to one of the written payloads —
          never empty/partial (mirrors _is_meeting_active never seeing a torn
          meta.json) — and the reader demonstrably performed reads
    """
    import threading

    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    payload_a = {"state": "a", "pad": "x" * 2048}
    payload_b = {"state": "b", "pad": "y" * 2048}
    storage._save_json(target, payload_a)

    iterations = 300
    errors: list[BaseException] = []
    done = threading.Event()
    start = threading.Barrier(2)
    writer_active = threading.Event()
    observed_overlap = threading.Event()
    reads = 0

    def writer():
        try:
            writer_active.set()
            start.wait(timeout=10)
            for i in range(iterations):
                storage._save_json(
                    target, payload_a if i % 2 == 0 else payload_b
                )
                time.sleep(0)  # yield so the reader interleaves
        except BaseException as exc:  # pragma: no cover - failure path
            errors.append(exc)
        finally:
            writer_active.clear()
            done.set()

    def reader():
        nonlocal reads
        try:
            start.wait(timeout=10)
            while True:
                data = json.loads(target.read_text(encoding="utf-8"))
                assert data in (payload_a, payload_b)
                reads += 1
                if writer_active.is_set():
                    observed_overlap.set()
                if done.is_set():
                    break
        except BaseException as exc:  # pragma: no cover - failure path
            errors.append(exc)

    threads = [
        threading.Thread(target=writer, daemon=True),
        threading.Thread(target=reader, daemon=True),
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=30)

    assert all(not t.is_alive() for t in threads), "thread did not finish in time"
    assert not errors, f"torn read or write failure: {errors!r}"
    assert reads > 0, "reader performed no reads — test would be vacuous"
    assert observed_overlap.is_set(), "reader never overlapped the active writer"
    assert list(tmp_path.glob("*.tmp")) == []


def test_save_json_preserves_existing_file_mode(tmp_path):
    """
    given: an existing target file chmod'ed to a non-default mode with sticky bit
    when: _save_json overwrites it
    then: the file keeps mode 0o1640 (old truncate-in-place preserved the
          inode's permissions; the atomic rewrite must match)
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    target.write_text("{}", encoding="utf-8")
    os.chmod(target, 0o1640)

    storage._save_json(target, {"v": 2})

    assert stat.S_IMODE(target.stat().st_mode) == 0o1640
    assert json.loads(target.read_text(encoding="utf-8")) == {"v": 2}


def test_save_json_restrictive_mode_protects_temp_before_write(
    tmp_path, monkeypatch
):
    """
    given: an existing target restricted to its owner
    when: _save_json opens its temporary file for writing
    then: the temporary file is already restricted before JSON is written
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    target.write_text("{}", encoding="utf-8")
    os.chmod(target, 0o600)
    real_fdopen = os.fdopen
    observed_modes = []

    def recording_fdopen(fd, *args, **kwargs):
        observed_modes.append(stat.S_IMODE(os.fstat(fd).st_mode))
        return real_fdopen(fd, *args, **kwargs)

    monkeypatch.setattr("recog.meeting_storage.os.fdopen", recording_fdopen)

    storage._save_json(target, {"private": True})

    assert observed_modes == [0o600]
    assert stat.S_IMODE(target.stat().st_mode) == 0o600


def test_save_json_first_write_mode_matches_umask_default(tmp_path):
    """
    given: no existing target file
    when: _save_json creates it under a controlled 0o027 umask
    then: mode is 0o666 & ~umask — what the old path.write_text (open "w")
          produced
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"

    previous_umask = os.umask(0o027)
    try:
        storage._save_json(target, {"fresh": True})
    finally:
        os.umask(previous_umask)

    assert stat.S_IMODE(target.stat().st_mode) == 0o640


def test_save_json_existing_target_stat_error_propagates(tmp_path, monkeypatch):
    """
    given: an existing target whose stat fails for a reason other than absence
    when: _save_json tries to preserve its mode
    then: the error propagates, the original survives, and the temp is removed
    """
    storage = MeetingStorage(base_path=tmp_path)
    target = tmp_path / "meta.json"
    old = {"still": "here"}
    target.write_text(json.dumps(old), encoding="utf-8")
    real_stat = os.stat

    def failing_target_stat(path, *args, **kwargs):
        if Path(path) == target:
            raise PermissionError("simulated stat failure")
        return real_stat(path, *args, **kwargs)

    monkeypatch.setattr("recog.meeting_storage.os.stat", failing_target_stat)

    with pytest.raises(PermissionError, match="simulated stat failure"):
        storage._save_json(target, {"new": "data"})

    assert json.loads(target.read_text(encoding="utf-8")) == old
    assert list(tmp_path.glob("*.tmp")) == []
