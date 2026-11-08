"""The Python side of tests/refine_parity.rs: RefineWorker driven step by step.

Run as `python refine.py <repo root>` with `{"scripts": [...]}` on stdin. No worker
thread runs: the script submits, pauses, resumes, bumps the generation, stops, and
processes the queue's head with `_process_task`, telling the transcribe callback what
to do on each call. The answer lists what the worker did after every step.
"""

import contextlib
import io
import json
import queue
import sys
import threading

sys.path.insert(0, sys.argv[1])

import numpy as np

from recog import refine
from recog.refine import RefineAborted, RefineTask, RefineWorker
from recog.remote_asr import ASRTemporarilyUnavailable


class Result:
    def __init__(self, text):
        self.text = text
        self.words = []


def run(script):
    gen = {"v": 0}
    events = []
    outcomes = []
    sleeps = []
    refine.time.sleep = sleeps.append
    holder = {}

    def transcribe(pcm, should_abort=None):
        outcome = outcomes.pop(0)
        events.append(
            [
                "transcribe",
                int(pcm[0]),
                should_abort is not None,
                should_abort() if should_abort else None,
            ]
        )
        worker = holder["worker"]
        if outcome == "pause_during":
            worker.pause()
        elif outcome == "gen_during":
            gen["v"] += 1
        elif outcome == "unavailable":
            raise ASRTemporarilyUnavailable("down")
        elif outcome == "aborted":
            raise RefineAborted()
        elif outcome == "error":
            raise RuntimeError("boom")
        return Result(f"text {int(pcm[0])}")

    def apply(result):
        events.append(["apply", result.task.pid, result.text])
        if result.text.endswith("7"):
            raise RuntimeError("disk full")

    stderr = io.StringIO()
    with contextlib.redirect_stderr(stderr):
        worker = RefineWorker(
            model_lock=threading.RLock(),
            transcribe_fn=transcribe,
            apply_fn=apply,
            abort_mode=script["mode"],
            transcribe_manages_lock=script["manages_lock"],
            generation_getter=(lambda: gen["v"]) if script["generation"] else None,
        )
        holder["worker"] = worker
        for op in script["ops"]:
            kind = op["op"]
            if kind == "submit":
                worker.submit(
                    RefineTask(
                        pid=op["id"],
                        pcm_samples=np.array([op["n"]], dtype="int16"),
                        offset_base_ms=0.0,
                        submitted_at=0.0,
                        meeting_generation=op["gen"],
                    )
                )
            elif kind == "pause":
                worker.pause()
            elif kind == "resume":
                worker.resume()
            elif kind == "gen":
                gen["v"] = op["v"]
            elif kind == "stop":
                worker.stop(timeout=0.1)
            elif kind == "process":
                outcomes.append(op["outcome"])
                try:
                    task = worker._queue.get_nowait()
                except queue.Empty:
                    task = None
                    outcomes.pop()
                if task is not None:
                    try:
                        worker._process_task(task)
                    except RuntimeError as e:
                        events.append(["raised", type(e).__name__, str(e)])
                    if outcomes:
                        outcomes.clear()
            else:
                raise ValueError(kind)
            queued = [t.pid for t in list(worker._queue.queue) if t is not None]
            events.append(["state", queued, worker.is_paused(), len(sleeps)])
    # The traceback print_exc adds after a logged error is not ported.
    log = [line for line in stderr.getvalue().splitlines() if line.startswith("[")]
    return {"events": events, "log": log}


request = json.load(sys.stdin)
print(json.dumps([run(s) for s in request["scripts"]]))
