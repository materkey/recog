"""The Python side of tests/asr_remote_parity.rs: FallbackASR over a fake gateway.

Run as `python remote_asr.py <repo root>` with `{"url": ..., "scenarios": [...]}` on
stdin. Each scenario builds a `FallbackASR` over a `RemoteASRClient` of
`<url>/s<index>/` (the test's gateway answers each scenario from its own plan) and a
fake local model whose weight load blocks until the scenario says how it ends. The
breakers and the background load read a clock the scenario moves itself. The answer is
what every step returned, whether ASR was available after it, and the `[FallbackASR]`
lines of the log, the gateway's error text cut off (httpx's, which ureq cannot match).
"""

import base64
import contextlib
import io
import json
import queue
import sys
import types

sys.path.insert(0, sys.argv[1])

import numpy as np
from gigaam.types import TranscriptionResult

from recog import log_utils, remote_asr
from recog.remote_asr import ASRTemporarilyUnavailable, FallbackASR, RemoteASRClient

DETAIL_MARK = "RemoteASRError: "


def normalize(msg):
    """The gateway's error text after `<call> failed: ` replaced by a placeholder."""
    head, mark, tail = msg.partition(DETAIL_MARK)
    if not mark:
        return msg
    call, failed, _detail = tail.partition(" failed: ")
    return f"{head}{mark}{call}{failed}<detail>" if failed else msg


class Clock:
    def __init__(self):
        self.t = 1000.0

    def monotonic(self):
        return self.t


def first_id(samples):
    return round(float(samples[0]) * 32768) if samples.dtype != np.int16 else int(samples[0])


class FakeLocal:
    """The local model: answers `local <id>`; its load waits for the scenario."""

    def __init__(self, gate):
        self.gate = gate

    def load_model(self, quiet=False):
        if self.gate.get() == "fail":
            raise RuntimeError("weights missing")

    def transcribe_chunk(self, audio, sample_rate=16000):
        return f"local {first_id(audio)}"

    def transcribe_batch(self, pcm_list):
        return [f"local {first_id(p)} " for p in pcm_list]

    def transcribe_with_timestamps(self, audio, sample_rate=16000):
        return TranscriptionResult(text=f"local {first_id(audio)} ", words=[])


def outcome(call, *args):
    try:
        return {"ok": call(*args)}
    except ASRTemporarilyUnavailable:
        return "unavailable"
    except Exception:  # noqa: BLE001 - the category is what is compared
        return "error"


def batch_texts(fb, clips):
    # The live caller reads a null text as an empty one: `(raw_text or "")`.
    return [t or "" for t in fb.transcribe_batch(clips)]


def refine(fb, pcm):
    r = fb.transcribe_with_timestamps(pcm)
    return {"text": r.text, "words": [[w.text, w.start, w.end] for w in r.words]}


def run(url, index, scenario):
    config = scenario["config"]
    clock = Clock()
    remote_asr.time = types.SimpleNamespace(monotonic=clock.monotonic)
    original_log = log_utils.log_rate_limited
    remote_asr.log_rate_limited = lambda key, msg: original_log(
        key, normalize(msg), _now_fn=clock.monotonic
    )
    log_utils._reset_for_tests()
    gate = queue.Queue()
    stderr = io.StringIO()
    steps = []
    with contextlib.redirect_stderr(stderr):
        client = RemoteASRClient(
            f"{url}/s{index}/",
            config["model"],
            config["timeout_s"],
            live_timeout_s=config["live_timeout_s"],
        )
        fb = FallbackASR(
            client,
            local_factory=lambda: FakeLocal(gate),
            fail_threshold=config["fails"],
            open_seconds=config["open_s"],
        )
        for op in scenario["ops"]:
            kind = op["op"]
            if kind == "load":
                result = outcome(fb.load_model, True)
            elif kind == "healthz":
                result = fb.remote.healthz()
            elif kind == "chunk":
                audio = np.frombuffer(base64.b64decode(op["pcm"]), dtype="<f4").copy()
                result = outcome(fb.transcribe_chunk, audio)
            elif kind == "batch":
                clips = [
                    np.frombuffer(base64.b64decode(c), dtype="<i2").copy() for c in op["clips"]
                ]
                result = outcome(batch_texts, fb, clips)
            elif kind == "refine":
                pcm = np.frombuffer(base64.b64decode(op["pcm"]), dtype="<i2").copy()
                result = outcome(refine, fb, pcm)
            elif kind == "advance":
                clock.t += op["s"]
                result = None
            elif kind == "local":
                thread = fb._local_load_thread
                loading = thread is not None and thread.is_alive()
                if loading:
                    gate.put(op["outcome"])
                    fb.wait_local_ready(timeout=10)
                result = loading
            else:
                raise ValueError(kind)
            steps.append({"op": kind, "result": result, "available": fb.available})
    lines = [line for line in stderr.getvalue().splitlines() if line.startswith("[FallbackASR]")]
    return {"steps": steps, "log": lines}


def main():
    request = json.load(sys.stdin)
    out = [run(request["url"], i, s) for i, s in enumerate(request["scenarios"])]
    json.dump(out, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
