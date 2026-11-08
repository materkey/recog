"""The Python side of tests/capture_decoders_parity.rs: `_decode_read` of the captures.

Run as `python capture_decoders.py <repo root>` with {kind, sample_rate, stage, reads}
on stdin. `kind` is pcm (SubprocessPCMCapture), mic (ChildMicCapture) or reference
(FramedSystemCapture); `stage` the parameters of the deterministic fake echo stage
below (or null); each read is {data (base64), wall, mono} or {new_child: true}.
time.time() and time.monotonic() of recog.system_audio read the read's values, and
the capture's last signal is at 0, so digital_silence_s() is `mono`.
"""

import base64
import contextlib
import hashlib
import io
import json
import sys
import types

sys.path.insert(0, sys.argv[1])

import numpy as np  # noqa: E402

from recog import system_audio  # noqa: E402
from recog.mic_capture import ChildMicCapture, FramedSystemCapture  # noqa: E402


def digest(samples):
    return {
        "len": len(samples),
        "sha256": hashlib.sha256(samples.astype("<f4").tobytes()).hexdigest(),
    }


class Stage:
    """Halves the microphone and holds back a cycling number of samples."""

    def __init__(self, hold_cycle, fail_at, active_period):
        self.hold_cycle, self.fail_at, self.active_period = hold_cycle, fail_at, active_period
        self.held = np.zeros(0, dtype=np.float32)
        self.calls = 0
        self.active = False
        self.record = []

    def add_mic(self, samples, time_s):
        self.calls += 1
        if self.calls == self.fail_at:
            raise RuntimeError(f"stage failed at call {self.calls}")
        self.record.append(["mic", time_s, digest(samples)])
        joined = np.r_[self.held, samples * np.float32(0.5)]
        hold = min(self.hold_cycle[self.calls % len(self.hold_cycle)], len(joined))
        out, self.held = joined[: len(joined) - hold], joined[len(joined) - hold :]
        self.active = (self.calls // self.active_period) % 2 == 1
        return out

    def flush_raw(self):
        out, self.held = self.held, np.zeros(0, dtype=np.float32)
        return out

    def add_reference(self, samples, time_s, echo_possible):
        self.calls += 1
        if self.calls == self.fail_at:
            raise RuntimeError(f"stage failed at call {self.calls}")
        self.record.append(["reference", time_s, echo_possible, digest(samples)])


request = json.load(sys.stdin)
now = {"wall": 0.0, "mono": 0.0}
system_audio.time = types.SimpleNamespace(
    time=lambda: now["wall"], monotonic=lambda: now["mono"], sleep=lambda s: None
)
spec = request["stage"]
stage = Stage(spec["hold_cycle"], spec["fail_at"], spec["active_period"]) if spec else None
sr = request["sample_rate"]
kind = request["kind"]
if kind == "pcm":
    capture = system_audio.SubprocessPCMCapture(
        ["unused"], "pcm", sample_rate=sr, max_buffer_s=10.0
    )
elif kind == "mic":
    capture = ChildMicCapture(None, sr, argv=["unused"], max_buffer_s=10.0, echo_stage=stage)
else:
    capture = FramedSystemCapture(["unused"], stage, sample_rate=sr, max_buffer_s=10.0)
capture._last_signal_at = 0.0

out = []
err = io.StringIO()
with contextlib.redirect_stderr(err):
    for read in request["reads"]:
        if read.get("new_child"):
            capture._on_new_child()
            out.append(None)
            continue
        now["wall"], now["mono"] = read["wall"], read["mono"]
        batches = capture._decode_read(base64.b64decode(read["data"]))
        out.append([{**digest(samples), "at": at} for samples, at in batches])
print(
    json.dumps({"reads": out, "stage": stage.record if stage else None, "stderr": err.getvalue()})
)
