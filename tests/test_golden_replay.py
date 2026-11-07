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
