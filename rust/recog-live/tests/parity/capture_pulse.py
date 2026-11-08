"""The Python side of tests/capture_pulse_parity.rs: PulseAudioCapture on scripted pactl.

Run as `python capture_pulse.py <repo root>` with a list of {pinned, answers, ops} on
stdin, one scenario each, answered in a list (the rate limit is reset between them).
Each answer is ["sink", name], ["exit", stdout, code] or ["error", text]; each op is
`pin`, `poll` or `switch` (take the pending source and apply it). The child is a fake
that never dies and time.sleep is a no-op, so `_restart_process` runs for real and
logs as it does. Answers one record per op and everything printed.
"""

import contextlib
import io
import json
import sys
import types

sys.path.insert(0, sys.argv[1])

from recog import system_audio  # noqa: E402
from recog.log_utils import _reset_for_tests  # noqa: E402

answers = []


class Result:
    def __init__(self, stdout, returncode):
        self.stdout, self.returncode = stdout, returncode


def run(argv, **kwargs):
    assert argv == ["pactl", "get-default-sink"] and kwargs["timeout"] == 2.0
    kind, *rest = answers.pop(0) if answers else ["sink", ""]
    if kind == "error":
        raise RuntimeError(rest[0])
    if kind == "exit":
        return Result(rest[0], rest[1])
    return Result(rest[0] + "\n", 0)


class Child:
    stdout = stderr = None
    pid = 7

    def poll(self):
        return None

    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0

    def kill(self):
        pass


system_audio.time = types.SimpleNamespace(
    time=lambda: 0.0, monotonic=lambda: 0.0, sleep=lambda s: None
)


def scenario(request):
    answers[:] = request["answers"]
    _reset_for_tests()
    spawned = []
    err = io.StringIO()
    out = []
    with contextlib.redirect_stderr(err):
        cap = system_audio.PulseAudioCapture(sample_rate=16000, pinned=request["pinned"], run=run)

        def spawn():
            spawned.append(list(cap.argv))
            return Child()

        cap._spawn = spawn
        for op in request["ops"]:
            record = {}
            if op == "pin":
                cap._pin_initial_source()
            elif op == "poll":
                cap._poll_default_sink_once()
            else:
                pending = cap._take_pending_source()
                record["pending"] = pending
                if pending is not None:
                    cap._apply_source_switch(pending)
            record.update(
                argv=cap.argv,
                current=cap._current_source,
                last_sink=cap._last_default_sink,
                epoch=cap.source_epoch,
                follow=cap._follow_default_sink,
            )
            out.append(record)
    return {"ops": out, "spawned": spawned, "stderr": err.getvalue()}


print(json.dumps([scenario(request) for request in json.load(sys.stdin)]))
