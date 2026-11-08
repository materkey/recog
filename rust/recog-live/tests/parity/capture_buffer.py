"""The Python side of tests/capture_buffer_parity.rs: BufferedAudioCapture on a script.

Run as `python capture_buffer.py <repo root>` with {sample_rate, max_buffer_s, ops} on
stdin. Each op is `append` {k, n, at} (n samples made from k, the formula the Rust
test uses), `get` {d}, `clear`, `boundary` or `running` {on}. Answers one record per op.
"""

import hashlib
import json
import sys

sys.path.insert(0, sys.argv[1])

import numpy as np  # noqa: E402

from recog.audio_capture import BufferedAudioCapture  # noqa: E402


def samples(k, n):
    return ((k * 7919 + np.arange(n)) % 2001 - 1000).astype(np.float32) / np.float32(1024)


request = json.load(sys.stdin)
cap = BufferedAudioCapture(request["sample_rate"], "parity", max_buffer_s=request["max_buffer_s"])
cap.running = True
out = []
for op in request["ops"]:
    kind = op["op"]
    if kind == "append":
        dropped = cap._append_locked_for_tests(samples(op["k"], op["n"]), op["at"])
        out.append({"dropped": dropped})
    elif kind == "get":
        result = cap.get_chunk(op["d"])
        if result is None:
            out.append(None)
        else:
            chunk, ts = result
            assert chunk.dtype == np.float32 and chunk.ndim == 1
            out.append(
                {
                    "len": len(chunk),
                    "sha256": hashlib.sha256(chunk.astype("<f4").tobytes()).hexdigest(),
                    "ts": ts,
                    "chunk_epoch": cap.chunk_epoch,
                }
            )
    elif kind == "clear":
        cap.clear_buffer()
        out.append(None)
    elif kind == "boundary":
        cap.mark_source_boundary()
        out.append(None)
    elif kind == "running":
        cap.running = op["on"]
        out.append(None)
    out[-1] = {
        "result": out[-1],
        "buffered": cap._buffered_samples,
        "start": cap._buffer_start_ts,
        "source_epoch": cap.source_epoch,
    }
print(json.dumps(out))
