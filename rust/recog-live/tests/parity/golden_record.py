"""The Python side of tests/golden_parity.rs: recog/golden.py on a scripted session.

Request:
    {"record_dir": dir Python records into, "read_dir": dir Rust recorded into,
     "clock": [readings of the monotonic clock, the first at construction],
     "wall": [readings of the wall clock], "ops": [...], "arrays": [[dtype, b64], ...]}
Ops: ["connect", client], ["disconnect", client], ["frame", client, json text],
["capture", source, [dtype, b64], start_ts], ["asr", call, single, [[dtype, b64], ...],
output] (output: null, a text, a list of texts, or {"text", "words"}), ["close"].

Answer: {"read": events of read_dir by read_session, each as [json text of the
event without audio, [dtype, b64] or null]; "replay": ReplayASR's answers by key for
read_dir; "keys": audio_key of each of "arrays"}.
"""

import base64
import json
import sys
from types import SimpleNamespace

sys.path.insert(0, sys.argv[1])

import numpy as np  # noqa: E402

from recog.golden import GoldenRecorder, audio_key, read_session  # noqa: E402
from recog.golden_replay import ReplayASR  # noqa: E402


def array(spec):
    dtype, data = spec
    return np.frombuffer(base64.b64decode(data), dtype=np.dtype(dtype))


def output(value):
    if isinstance(value, dict):
        words = [SimpleNamespace(text=t, start=s, end=e) for t, s, e in value["words"]]
        return SimpleNamespace(text=value["text"], words=words)
    return value


request = json.load(sys.stdin)
clock = iter(request["clock"])
wall = iter(request["wall"])
recorder = GoldenRecorder(request["record_dir"], clock=lambda: next(clock), wall=lambda: next(wall))
for op in request["ops"]:
    kind = op[0]
    if kind == "connect":
        recorder.client_connected(op[1])
    elif kind == "disconnect":
        recorder.client_disconnected(op[1])
    elif kind == "frame":
        recorder.frame(op[1], json.loads(op[2]))
    elif kind == "capture":
        recorder.capture(op[1], array(op[2]), op[3])
    elif kind == "asr":
        clips = [array(spec) for spec in op[3]]
        recorder.asr(op[1], clips[0] if op[2] else clips, output(op[4]))
    elif kind == "close":
        recorder.close()
recorder.close()

events = list(read_session(request["read_dir"]))
read = []
for event in events:
    audio = event.pop("audio", None)
    spec = None if audio is None else [audio.dtype.str, base64.b64encode(audio.tobytes()).decode()]
    read.append([json.dumps(event, ensure_ascii=False), spec])
replay = ReplayASR(events)
print(
    json.dumps(
        {
            "read": read,
            "replay": {k: json.dumps(v, ensure_ascii=False) for k, v in replay.answers.items()},
            "keys": [audio_key(array(spec)) for spec in request["arrays"]],
        }
    )
)
