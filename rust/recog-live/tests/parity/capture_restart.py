"""The Python side of tests/capture_restart_parity.rs: SubprocessPCMCapture on a stub child.

Run as `python capture_restart.py <repo root>` with a JSON request on stdin:
{argv, policy: {backoff_s, max_restarts, lost_retry_s}, spawn_log, spawns, settle_s,
restart_after_s}. The policy constants are patched into recog.system_audio, the
capture is started, and once the spawn log holds `spawns` lines (or 20 s passed) it
waits `settle_s` and stops. Answers {stderr, error}: everything it printed.
"""

import contextlib
import io
import json
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, sys.argv[1])

from recog import system_audio  # noqa: E402

request = json.load(sys.stdin)
policy = request["policy"]
system_audio.RESTART_BACKOFF_S = tuple(policy["backoff_s"])
system_audio.MAX_RESTARTS = policy["max_restarts"]
system_audio.LOST_RETRY_S = policy["lost_retry_s"]


class Tee(io.StringIO):
    """Collects what the capture prints, from every thread."""

    def __init__(self):
        super().__init__()
        self.lock = threading.Lock()

    def write(self, text):
        with self.lock:
            return super().write(text)


def spawned():
    path = Path(request["spawn_log"])
    return len(path.read_text().splitlines()) if path.exists() else 0


out = Tee()
error = None
with contextlib.redirect_stderr(out):
    capture = system_audio.SubprocessPCMCapture(request["argv"], "stub", max_buffer_s=10.0)
    started = time.monotonic()
    try:
        capture.start()
    except Exception as e:  # noqa: BLE001 - the kind is the answer
        error = type(e).__name__
    if error is None:
        restart_after = request.get("restart_after_s")
        deadline = started + 20.0
        while spawned() < request["spawns"] and time.monotonic() < deadline:
            if restart_after is not None and time.monotonic() - started >= restart_after:
                capture.request_restart("silence")
                restart_after = None
            time.sleep(0.01)
        time.sleep(request["settle_s"])
        capture.stop()

print(json.dumps({"stderr": out.getvalue(), "error": error}))
