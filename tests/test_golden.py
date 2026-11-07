"""Golden session recording (docs/plans/2026-11-07-golden-sessions-design.md).

The recorder writes one live process's inputs and ASR answers; read_session gives
them back in order. A replay depends on getting back exactly what was recorded.
"""

import json
import threading
from types import SimpleNamespace

import numpy as np

from recog.golden import GoldenRecorder, audio_key, read_session


class Ticks:
    """A clock that advances 0.5 s per reading."""

    def __init__(self):
        self.now = 100.0

    def __call__(self):
        self.now += 0.5
        return self.now


def test_recorded_session_read_session_returns_every_input_in_order_exactly(tmp_path):
    """given: a session with a connect, a frame, captured audio, an ASR answer and a disconnect
    when: it is read back
    then: the same events in order, times from the start, and the audio bit for bit"""
    mic = np.random.default_rng(1).normal(0, 0.1, 48000).astype(np.float32)
    recorder = GoldenRecorder(tmp_path, clock=Ticks(), wall=lambda: 1_700_000_000.0)
    recorder.client_connected(1)
    recorder.frame(1, {"type": "speaker_update", "speaker": {"id": "a", "name": "Alice"}})
    recorder.capture("mic", mic, 1_699_999_997.0)
    recorder.asr("transcribe_chunk", mic, "привет")
    recorder.client_disconnected(1)
    recorder.close()

    events = list(read_session(tmp_path))

    assert [e["kind"] for e in events] == ["connect", "frame", "capture", "asr", "disconnect"]
    assert [e["t"] for e in events] == [0.5, 1.0, 1.5, 2.0, 2.5]
    assert events[1]["data"]["speaker"]["name"] == "Alice"
    assert events[2]["source"] == "mic" and events[2]["start_ts"] == 1_699_999_997.0
    np.testing.assert_array_equal(events[2]["audio"], mic)
    assert events[3]["inputs"] == [audio_key(mic)] and events[3]["output"] == "привет"


def test_asr_outputs_of_every_shape_are_kept_as_plain_data(tmp_path):
    """given: a batch answer and a refine answer with words
    when: they are recorded and read back
    then: the texts and the words (text, start ms, end ms), with a key per input clip"""
    clips = [np.zeros(10, dtype=np.int16), np.ones(10, dtype=np.int16)]
    refine = SimpleNamespace(
        text="Привет мир", words=[SimpleNamespace(text="Привет", start=0, end=80)]
    )
    recorder = GoldenRecorder(tmp_path)
    recorder.asr("transcribe_batch", clips, ["а", "б"])
    recorder.asr("transcribe_with_timestamps", clips[0], refine)
    recorder.close()

    batch, timestamps = read_session(tmp_path)

    assert batch["inputs"] == [audio_key(c) for c in clips] and batch["output"] == ["а", "б"]
    assert timestamps["output"] == {"text": "Привет мир", "words": [["Привет", 0, 80]]}


def test_same_samples_other_type_audio_key_differs():
    """given: the same numbers as int16 and as float32
    when: they are keyed
    then: different keys: the replay must hand ASR exactly what was recorded"""
    assert audio_key(np.zeros(4, dtype=np.int16)) != audio_key(np.zeros(4, dtype=np.float32))


def test_threads_recording_at_once_every_line_is_whole(tmp_path):
    """given: the bridge thread, the ingest worker and the main loop recording at once
    when: the session is read
    then: every event is there and every line parses"""
    recorder = GoldenRecorder(tmp_path)

    def record(n):
        for i in range(200):
            recorder.capture(f"s{n}", np.full(160, i, dtype=np.float32), float(i))

    threads = [threading.Thread(target=record, args=(n,)) for n in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    recorder.close()

    lines = (tmp_path / "events.jsonl").read_text().splitlines()
    assert len(lines) == 800 and all(json.loads(line) for line in lines)
    for event in read_session(tmp_path):
        assert (event["audio"] == event["start_ts"]).all()


def test_disk_error_recording_stops_quietly_and_the_caller_goes_on(tmp_path, capsys):
    """given: a write that fails (a full disk)
    when: more events come
    then: no exception reaches the live process; recording stops and says so once"""
    recorder = GoldenRecorder(tmp_path)

    def full(_data):
        raise OSError(28, "No space left on device")

    recorder._audio.write = full
    recorder.capture("mic", np.zeros(10, dtype=np.float32), 0.0)
    recorder.capture("mic", np.zeros(10, dtype=np.float32), 0.0)
    recorder.frame(1, {"type": "x"})

    assert capsys.readouterr().err.count("golden recording stopped") == 1


def test_live_asr_calls_with_recording_on_their_answers_are_recorded(tmp_path):
    """given: a live process recording a golden session during a conference
    when: a live chunk goes through ASR
    then: each call is recorded with its input keys and answer, and the answer still returned"""
    from unittest.mock import Mock

    from recog.live import LiveTranscriber

    t = LiveTranscriber.__new__(LiveTranscriber)
    t._golden = GoldenRecorder(tmp_path)
    t._refine_worker = None
    t._ktalk_bridge = Mock()
    t._ktalk_bridge.is_conference_active.return_value = True
    t.asr = Mock()
    t.asr.transcribe_chunk.return_value = "раз"
    chunk = np.zeros(16000, dtype=np.float32)

    assert t._transcribe_direct(chunk) == "раз"
    t._golden.close()

    [event] = read_session(tmp_path)
    assert (event["call"], event["inputs"], event["output"]) == (
        "transcribe_chunk",
        [audio_key(chunk)],
        "раз",
    )


def test_golden_record_setting_start_golden_recording_opens_a_session_under_the_data_dir(
    tmp_path, monkeypatch
):
    """given: [live] golden_record on, and off
    when: the live process starts recording
    then: a session directory under <data dir>/golden only when on"""
    from recog import live as live_mod
    from recog.install_config import INSTALL_CONFIG

    monkeypatch.setattr(live_mod, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(INSTALL_CONFIG.live, "golden_record", False)
    assert live_mod.LiveTranscriber._start_golden_recording() is None

    monkeypatch.setattr(INSTALL_CONFIG.live, "golden_record", True)
    recorder = live_mod.LiveTranscriber._start_golden_recording()
    recorder.close()

    assert recorder.directory.parent == tmp_path / "golden"
    assert (recorder.directory / "events.jsonl").exists()


def test_no_conference_live_audio_and_asr_are_not_recorded(tmp_path):
    """given: recording on, the service listening between meetings (it does all day)
    when: a chunk goes through ASR with no conference, then with one
    then: only the conference's call is recorded: a day of idle microphone would be ~11 GB"""
    from unittest.mock import Mock

    from recog.live import LiveTranscriber

    t = LiveTranscriber.__new__(LiveTranscriber)
    t._golden = GoldenRecorder(tmp_path)
    t._refine_worker = None
    t._ktalk_bridge = Mock()
    t.asr = Mock()
    t.asr.transcribe_chunk.side_effect = ["между встречами", "на встрече"]
    chunk = np.zeros(16000, dtype=np.float32)

    t._ktalk_bridge.is_conference_active.return_value = False
    t._transcribe_direct(chunk)
    t._ktalk_bridge.is_conference_active.return_value = True
    t._transcribe_direct(chunk)
    t._golden.close()

    assert [e["output"] for e in read_session(tmp_path)] == ["на встрече"]


def test_sessions_of_every_age_remove_old_sessions_keeps_recent_and_marked_ones(tmp_path):
    """given: sessions 20 and 2 days old, one 20 days old marked -keep, and a stray file
    when: old sessions are removed with a 14-day limit
    then: only the unmarked 20-day-old session goes"""
    import os
    import time as time_mod

    from recog.golden import remove_old_sessions

    now = time_mod.time()
    for name, days in [("old", 20), ("recent", 2), ("old-keep", 20)]:
        recorder = GoldenRecorder(tmp_path / name)
        recorder.close()
        for path in [tmp_path / name, *(tmp_path / name).iterdir()]:
            os.utime(path, (now - days * 86400, now - days * 86400))
    (tmp_path / "notes.txt").write_text("x")

    removed = remove_old_sessions(tmp_path, max_age_days=14, now=now)

    assert sorted(p.name for p in tmp_path.iterdir()) == ["notes.txt", "old-keep", "recent"]
    assert [p.name for p in removed] == ["old"]


def test_open_session_read_session_sees_every_event_so_far(tmp_path):
    """given: a session still being recorded (the live process runs for days)
    when: it is read, or the process is killed now
    then: every event so far is on disk, with its audio"""
    recorder = GoldenRecorder(tmp_path)
    recorder.frame(1, {"type": "clock_sync"})
    recorder.capture("mic", np.ones(160, dtype=np.float32), 0.0)

    events = list(read_session(tmp_path))

    assert [e["kind"] for e in events] == ["frame", "capture"]
    assert len(events[1]["audio"]) == 160
    recorder.close()
