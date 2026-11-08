"""Replay of a golden session into the Python live process (recog/golden_replay.py).

The player's own acceptance test: a synthetic session, recorded with the recorder,
replays to the transcript its recorded ASR answers and speaker signals make.
"""

import json
import subprocess
import sys
import time

import numpy as np

from recog.golden import GoldenRecorder


class Clock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t


def _session(directory):
    """A KTalk call: Alice speaks on the system audio, then the user on the microphone."""
    clock = Clock()
    wall0 = time.time()
    rec = GoldenRecorder(directory, clock=clock, wall=lambda: wall0 + clock.t)
    noise = np.random.default_rng(0).normal(0, 0.1, 48000).astype(np.float32)
    voice = np.random.default_rng(1).normal(0, 0.1, 48000).astype(np.float32)
    me = {"id": "me", "name": "Tester", "isLocal": True, "micMuted": False}
    alice = {"id": "a", "name": "Alice", "isLocal": False, "micMuted": False}
    rec.client_connected(1)
    clock.t = 0.1
    rec.frame(1, {"type": "clock_sync", "browser_time": (wall0 + 0.1) * 1000})
    rec.frame(
        1, {"type": "meeting_join", "url": "https://test.ktalk.ru/room1", "platform": "ktalk"}
    )
    rec.frame(1, {"type": "participants_update", "participants": [me, alice]})
    clock.t = 0.3
    rec.frame(1, {"type": "speaker_update", "speaker": alice, "timestamp": (wall0 + 0.3) * 1000})
    clock.t = 3.5
    rec.capture("system", noise, wall0 + 0.4)
    rec.asr("transcribe_chunk", noise, "Добрый день, коллеги")
    clock.t = 4.0
    rec.frame(1, {"type": "speaker_update", "speaker": None, "timestamp": (wall0 + 4.0) * 1000})
    clock.t = 4.5
    rec.capture("mic", voice, wall0 + 1.5)
    rec.asr("transcribe_chunk", voice, "Привет, Алиса")
    rec.close()


def test_synthetic_call_replay_gives_its_transcript(tmp_path):
    """given: a recorded KTalk call with one remote and one local phrase
    when: it is replayed into the live process
    then: the meeting's transcript holds both phrases under their speakers,
          and every ASR call was answered from the recording"""
    session, out = tmp_path / "session", tmp_path / "out"
    _session(session)
    code = (
        "import sys, recog.golden_replay as r\n"
        "r.TAIL_S = 3.0\n"
        "sys.exit(r.main(['replay', sys.argv[1], sys.argv[2]]))\n"
    )

    result = subprocess.run(
        [sys.executable, "-c", code, str(session), str(out)],
        capture_output=True,
        text=True,
        timeout=120,
    )

    assert result.returncode == 0, result.stderr[-3000:]
    summary = json.loads((out / "replay.json").read_text())
    assert summary["asr_misses"] == 0
    rows = [
        json.loads(line)
        for path in (out / "data" / "meetings").glob("*/transcript.jsonl")
        for line in path.read_text().splitlines()
    ]
    said = {(r["speaker"], r["text"]) for r in rows}
    assert ("Alice", "Добрый день, коллеги") in said
    assert any(text == "Привет, Алиса" for _speaker, text in said), said


def _rows(meetings):
    return [
        (path, json.loads(line))
        for path in sorted(meetings.glob("*/transcript.jsonl"))
        for line in path.read_text().splitlines()
    ]


def test_replay_against_its_original_compare_matches_rows_and_names_a_changed_one(tmp_path):
    """given: a replay, and the meeting as the live service wrote it at recording time
    when: they are compared, then once more with one phrase changed in the original
    then: every row matches the first time; the second time the two versions of the
          changed phrase are reported, one on each side"""
    from datetime import datetime, timedelta

    from recog.golden_replay import compare

    session, out = tmp_path / "session", tmp_path / "out"
    _session(session)
    code = "import sys, recog.golden_replay as r\nr.TAIL_S = 3.0\nsys.exit(r.main(['replay', sys.argv[1], sys.argv[2]]))\n"
    result = subprocess.run(
        [sys.executable, "-c", code, str(session), str(out)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert result.returncode == 0, result.stderr[-3000:]
    summary = json.loads((out / "replay.json").read_text())
    shift = timedelta(seconds=summary["start_wall"] - summary["recorded_start_wall"])
    original = tmp_path / "original"
    for path, row in _rows(out / "data" / "meetings"):
        row["ts"] = (datetime.fromisoformat(row["ts"]) - shift).isoformat()
        target = original / path.parent.name / "transcript.jsonl"
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("a") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

    same = compare(session, original, out)

    assert same["only_original"] == [] and same["only_replay"] == [] and same["matched"] >= 2
    target = next(original.glob("*/transcript.jsonl"))
    target.write_text(target.read_text().replace("Добрый день, коллеги", "Добрый вечер, коллеги"))
    changed = compare(session, original, out)
    assert [r["text"] for r in changed["only_original"]] == ["Добрый вечер, коллеги"]
    assert [r["text"] for r in changed["only_replay"]] == ["Добрый день, коллеги"]


def test_rows_outside_the_recorded_audio_compare_leaves_them_out(tmp_path):
    """given: the service's rows of a recorded call, and a row from after the call,
          when the service went on listening with no conference (its audio is not recorded)
    when: a replay with the call's rows is compared with them
    then: everything matches: the replay cannot reproduce audio that was never recorded"""
    from datetime import datetime, timezone

    from recog.golden import read_session
    from recog.golden_replay import compare

    session, out = tmp_path / "session", tmp_path / "out"
    _session(session)
    wall0 = next(iter(read_session(session)))["wall"]

    def row(t, speaker, is_local, text):
        ts = datetime.fromtimestamp(wall0 + t, timezone.utc).isoformat()
        return {"ts": ts, "speaker": speaker, "is_local": is_local, "text": text}

    call = [
        row(0.4, "Alice", False, "Добрый день, коллеги"),
        row(1.5, "Tester", True, "Привет, Алиса"),
    ]
    for meetings, rows in [
        (out / "data" / "meetings", call),
        (tmp_path / "original", call + [row(10.0, "Tester", True, "Да.")]),
    ]:
        (meetings / "m").mkdir(parents=True)
        (meetings / "m" / "transcript.jsonl").write_text(
            "".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows)
        )
    (out / "replay.json").write_text(json.dumps({"start_wall": 0, "recorded_start_wall": 0}))

    result = compare(session, tmp_path / "original", out)

    assert result == {"matched": 2, "only_original": [], "only_replay": []}


MARKER = {"speaker_id": "__system__", "uncertain_reason": "pcm_watchdog_transition"}


def _replay_out(out, wall0, shift, rows, meta=None):
    """A replay's output as compare_replays reads it: replay.json and one meeting whose
    rows are given at seconds from the recorded start."""
    from datetime import datetime, timezone

    meeting = out / "data" / "meetings" / "m"
    meeting.mkdir(parents=True)
    (out / "replay.json").write_text(
        json.dumps({"start_wall": wall0 + shift, "recorded_start_wall": wall0, "asr_misses": 0})
    )
    lines = []
    for t, speaker, text, extra in rows:
        ts = datetime.fromtimestamp(wall0 + shift + t, timezone.utc).isoformat()
        lines.append(json.dumps({"ts": ts, "speaker": speaker, "is_local": False, "text": text, **extra}))
    (meeting / "transcript.jsonl").write_text("".join(line + "\n" for line in lines))
    base = {
        "created_at": "2026-01-01T00:00:00+00:00",
        "source": "ktalk",
        "participants": {"a": {"name": "Alice", "first_seen": str(wall0 + shift)}},
        "stats": {"segment_count": len(rows), "words_count": 3},
    }
    (meeting / "meta.json").write_text(json.dumps({**base, **(meta or {})}))


def test_two_replays_compare_replays_agrees_on_rows_markers_and_meta_apart_from_times(tmp_path):
    """given: two replays of a session started 100 s apart, with the same row, the same
          marker and their participants first seen at their own times
    when: they are compared
    then: they agree"""
    from recog.golden import read_session
    from recog.golden_replay import compare_replays, replays_agree

    session = tmp_path / "session"
    _session(session)
    wall0 = next(iter(read_session(session)))["wall"]
    rows = [(0.4, "Alice", "Добрый день, коллеги", {}), (2.0, "Система", "[PCM recovered]", MARKER)]
    _replay_out(tmp_path / "a", wall0, 0.0, rows)
    _replay_out(tmp_path / "b", wall0, 100.0, rows)

    result = compare_replays(session, tmp_path / "a", tmp_path / "b")

    assert replays_agree(result), result
    assert (result["matched"], result["markers_matched"]) == (1, 1)


def test_a_changed_row_a_missing_marker_and_another_source_compare_replays_names_each(tmp_path):
    """given: two replays differing in a row's text, a marker one of them lacks and the
          meeting's source
    when: they are compared
    then: each difference is reported on its side and the replays disagree"""
    from recog.golden import read_session
    from recog.golden_replay import compare_replays, replays_agree

    session = tmp_path / "session"
    _session(session)
    wall0 = next(iter(read_session(session)))["wall"]
    _replay_out(
        tmp_path / "a",
        wall0,
        0.0,
        [(0.4, "Alice", "Добрый день", {}), (1.0, "Система", "[lost]", MARKER)],
    )
    _replay_out(tmp_path / "b", wall0, 0.0, [(0.4, "Alice", "Добрый вечер", {})], {"source": "orphan"})

    result = compare_replays(session, tmp_path / "a", tmp_path / "b")

    assert not replays_agree(result)
    assert [r["text"] for r in result["only_first"]] == ["Добрый день"]
    assert [r["text"] for r in result["only_second"]] == ["Добрый вечер"]
    assert [r["text"] for r in result["markers_only_first"]] == ["[lost]"]
    assert result["meta_equal"] is False
