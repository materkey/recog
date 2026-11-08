"""The Python side of tests/echo_parity.rs: recog/echo_cancel.py on Rust's input.

Run as `python echo_parity.py <repo root>` with a JSON request on stdin; prints JSON.
Sample arrays travel as base64 of their little-endian bytes ("<f4" or "<f8").
"""

import base64
import json
import sys

sys.path.insert(0, sys.argv[1])

import numpy as np  # noqa: E402

from recog import echo_cancel  # noqa: E402
from recog.echo_cancel import (  # noqa: E402
    EchoPathDetector,
    EchoStage,
    TimelinePlacer,
    _HighPass,
    _ReferenceRing,
    _Suppressor,
    peak_correlation,
)


def arr(text, dtype):
    return np.frombuffer(base64.b64decode(text), dtype=dtype)


def b64(a, dtype):
    return base64.b64encode(np.ascontiguousarray(a, dtype=dtype).tobytes()).decode()


def numpy_ops(request):
    out = []
    for text in request["arrays"]:
        x = arr(text, "<f8")
        out.append(
            {
                # sum, mean, std, median, 30th percentile: as bytes, so no float is
                # reparsed on the way
                "scalars": b64(
                    [np.sum(x), np.mean(x), np.std(x), np.median(x), np.percentile(x, 30)],
                    "<f8",
                ),
                "cumsum": b64(np.cumsum(x), "<f8"),
            }
        )
    window = _Suppressor().win
    return {"arrays": out, "window": b64(window, "<f8")}


def normal(request):
    return {"noise": b64(np.random.default_rng(0).normal(0, 0.02, request["n"]), "<f8")}


def inverse(_request):
    return {"taps": b64(echo_cancel.inverse_low_cut_fir(), "<f8")}


def placer(request):
    p = TimelinePlacer(echo_cancel.SR)
    out = []
    for block in request["blocks"]:
        placed, jumped = p.place(arr(block["samples"], "<f8"), block["t"])
        out.append({"placed": b64(placed, "<f8"), "jumped": jumped, "next": p.next_position})
    return {"blocks": out}


def ring(request):
    r = _ReferenceRing()
    out = []
    for op in request["ops"]:
        if op["op"] == "write":
            r.write(op["position"], arr(op["samples"], "<f8"))
            out.append({"end": r.end})
        else:
            samples, real = r.read(op["position"], op["n"])
            out.append({"samples": b64(samples, "<f8"), "real": real})
    return {"ops": out}


def highpass(request):
    hp = _HighPass(echo_cancel.HIGH_PASS_HZ)
    return {"blocks": [b64(hp.block(arr(b, "<f8")), "<f8") for b in request["blocks"]]}


def correlation(request):
    return {
        "peaks": b64(
            [
                peak_correlation(arr(pair["mic"], "<f8"), arr(pair["reference"], "<f8"))
                for pair in request["pairs"]
            ],
            "<f8",
        )
    }


def detector(request):
    d = EchoPathDetector(echo_cancel.SR)
    present = []
    for pair in request["blocks"]:
        d.feed(arr(pair["mic"], "<f8"), arr(pair["reference"], "<f8"))
        present.append(d.present)
    return {"present": present}


def stage(request):
    taps = arr(request["taps"], "<f8") if request.get("taps") else None
    if taps is not None:
        echo_cancel._inverse_fir = taps.copy()
    results = []
    for scenario in request["scenarios"]:
        s = EchoStage()
        calls = []
        for event in scenario:
            if event["kind"] == "ref":
                s.add_reference(arr(event["samples"], "<f4"), event["t"], event["echo_possible"])
            elif event["kind"] == "mic":
                out = s.add_mic(arr(event["samples"], "<f4"), event["t"])
                calls.append({"out": b64(out, "<f4"), "active": s.active})
            else:
                calls.append({"out": b64(s.flush_raw(), "<f4"), "active": s.active})
        results.append(calls)
    return {"scenarios": results}


OPS = {
    "numpy": numpy_ops,
    "normal": normal,
    "inverse": inverse,
    "placer": placer,
    "ring": ring,
    "highpass": highpass,
    "correlation": correlation,
    "detector": detector,
    "stage": stage,
}

request = json.load(sys.stdin)
print(json.dumps(OPS[request["op"]](request)))
