"""The Python side of tests/per_speaker_parity.rs: PerSpeakerIngest on scripted scenarios.

Run as `python per_speaker.py <repo root>` with `{"scenarios": [...]}` on stdin. Each
scenario is a list of operations (see `run`); the answer lists, per scenario, what the
ingest did: callback calls, tick results, refine tasks, candidates, stats, the flags
after every operation, the per-pid refine bookkeeping at the end and the log lines.

Time is a fake clock both sides set explicitly; the rate-limited log reads it too.
Floats travel as their repr ("f:<repr>"), so the comparison covers the formatting.
PCM is generated from a seed on both sides and reported as its SHA-256.
"""

import contextlib
import hashlib
import io
import json
import sys

sys.path.insert(0, sys.argv[1])

import numpy as np

from recog import log_utils, per_speaker
from recog.ktalk_bridge import PcmChunk
from recog.per_speaker import PerSpeakerIngest
from recog.remote_asr import ASRTemporarilyUnavailable


def num(x):
    if isinstance(x, float):
        return "f:" + repr(x)
    return x


def pcm_bytes(n, seed):
    i = np.arange(n, dtype=np.int64)
    return (((seed * 2654435761 + i * 40503) % 65536) - 32768).astype("<i2").tobytes()


def digest(samples):
    return hashlib.sha256(np.ascontiguousarray(samples, dtype="<i2").tobytes()).hexdigest()


def batch_json(batch):
    return [
        [pid, name, local, digest(pcm), int(pcm.size), num(s), num(e), num(a0), num(a1)]
        for pid, name, local, pcm, s, e, a0, a1 in batch
    ]


def task_json(task):
    if task is None:
        return None
    return [
        task.pid,
        digest(task.pcm_samples),
        int(task.pcm_samples.size),
        num(task.offset_base_ms),
        num(task.submitted_at),
        task.meeting_generation,
    ]


def stats_json(stats):
    out = {k: num(v) for k, v in stats.items() if k not in ("per_pid", "pending_buffer_sizes")}
    out["pending_buffer_sizes"] = stats["pending_buffer_sizes"]
    out["per_pid"] = {pid: {k: num(v) for k, v in s.items()} for pid, s in stats["per_pid"].items()}
    return out


def internals(ingest):
    pids = set(ingest._pid_audio_ms) | set(ingest._pcm_rings) | set(ingest._silence_ms)
    pids |= ingest._pids_had_chunk_since_last_tick
    out = {}
    for pid in sorted(pids):

        def opt(d, pid=pid):
            return num(d[pid]) if pid in d else None

        out[pid] = {
            "ring": [[int(s.size), num(end)] for s, end in ingest._pcm_rings.get(pid, [])],
            "audio_ms": num(ingest._pid_audio_ms.get(pid, 0.0)),
            "drained_ms": opt(ingest._pid_drained_ms),
            "watermark_ms": opt(ingest._refine_watermark_ms),
            "silence_ms": opt(ingest._silence_ms),
            "continuous_speech_ms": opt(ingest._continuous_speech_ms),
            "sample_rate": ingest._pid_sample_rate.get(pid),
            "touched": pid in ingest._pids_had_chunk_since_last_tick,
        }
    return out


def run(scenario):
    clock = {"t": 0.0}
    gen = {"v": 0}
    flags = {"active": False, "asr": True}
    fails = []
    events = []

    def callback(batch):
        events.append(["call", batch_json(batch)])
        if fails:
            kind = fails.pop(0)
            if kind == "unavailable":
                raise ASRTemporarilyUnavailable("down")
            raise RuntimeError("boom")

    def submit(task):
        events.append(["refine", task_json(task)])

    original = log_utils.log_rate_limited
    log_utils._reset_for_tests()
    per_speaker.log_rate_limited = lambda key, msg: original(key, msg, _now_fn=lambda: clock["t"])
    options = scenario["options"]
    stderr = io.StringIO()
    with contextlib.redirect_stderr(stderr):
        ingest = PerSpeakerIngest(
            transcribe_callback=callback,
            generation_getter=lambda: gen["v"],
            meeting_active_getter=lambda: flags["active"],
            max_batch_size=options["max_batch_size"],
            max_chunks_per_pid=options["max_chunks_per_pid"],
            silence_watchdog_s=options["silence_watchdog_s"],
            time_fn=lambda: clock["t"],
            refine_submit_callback=submit if options["refine"] else None,
            asr_available_getter=lambda: flags["asr"],
        )
        for op in scenario["ops"]:
            kind = op["op"]
            if kind == "time":
                clock["t"] = op["t"]
            elif kind == "enqueue":
                ingest.enqueue(
                    PcmChunk(
                        pid=op["pid"],
                        speaker_name=op["name"],
                        is_local=op["local"],
                        seq=op["seq"],
                        chunk_start_ts=op["start"],
                        chunk_end_ts=op["end"],
                        sample_rate=op["sr"],
                        pcm=pcm_bytes(op["n"], op["seed"]),
                    )
                )
            elif kind == "tick":
                events.append(["tick", batch_json(ingest.tick(now=op["now"]))])
            elif kind == "gen":
                gen["v"] = op["v"]
            elif kind == "drain":
                ingest.drain()
            elif kind == "active":
                flags["active"] = op["v"]
            elif kind == "asr":
                flags["asr"] = op["v"]
            elif kind == "fail":
                fails.extend([op["kind"]] * op["count"])
            elif kind == "candidate":
                events.append(["candidate", task_json(ingest.get_refine_candidate(op["pid"]))])
            elif kind == "refine_cb":
                ingest.set_refine_submit_callback(submit if op["v"] else None)
            elif kind == "stats":
                events.append(["stats", stats_json(ingest.get_stats())])
            else:
                raise ValueError(kind)
            events.append(
                [
                    "flags",
                    ingest.degraded,
                    ingest.pcm_ever_seen,
                    ingest.last_drain_generation,
                ]
            )
    per_speaker.log_rate_limited = original
    return {
        "events": events,
        "internals": internals(ingest),
        "log": stderr.getvalue().splitlines(),
    }


request = json.load(sys.stdin)
print(json.dumps([run(s) for s in request["scenarios"]], ensure_ascii=False))
