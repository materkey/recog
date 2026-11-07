"""The reference: recog's own ASRModule (GigaAM v3_e2e_rnnt on PyTorch) on the corpus.

Run with the recog checkout on PYTHONPATH so `recog` imports:
    cd ~/projects/recog && PYTHONPATH=. venv-gigaam/bin/python <bench>/ref.py <bench>/corpus <out.json> [device]

live:   transcribe_chunk per window, then transcribe_batch over groups of 8 (PCM_MAX_BATCH_SIZE)
refine: transcribe_with_timestamps per window (word start/end in ms)
"""

import json
import platform
import resource
import sys
import time
from pathlib import Path

import numpy as np

from recog.asr import ASRModule


def peak_rss_mb() -> float:
    r = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return r / 2**20 if platform.system() == "Darwin" else r / 1024


def main(corpus: str, out: str, device: str = "auto") -> None:
    corpus_dir = Path(corpus)
    manifest = json.loads((corpus_dir / "manifest.json").read_text())
    t0 = time.perf_counter()
    asr = ASRModule(device=device)
    asr.load_model(quiet=True)
    result = {
        "runtime": "pytorch",
        "device": asr.device,
        "load_s": time.perf_counter() - t0,
        "host": platform.node(),
        "live": {},
        "batch": {},
        "refine": {},
        "times": {},
    }
    pcm = {s: np.fromfile(corpus_dir / f"{s}.f32", dtype="<f4") for s in manifest["shapes"]}

    def win(shape, it):
        return pcm[shape][it["offset"] : it["offset"] + it["len"]]

    live = manifest["shapes"]["live"]
    asr.transcribe_chunk(win("live", live[0]))  # warm-up, not timed
    times = []
    for it in live:
        t = time.perf_counter()
        result["live"][it["id"]] = asr.transcribe_chunk(win("live", it)) or ""
        times.append(time.perf_counter() - t)
    result["times"]["live"] = times

    times = []
    for g in range(0, len(live), 8):
        group = live[g : g + 8]
        t = time.perf_counter()
        texts = asr.transcribe_batch([win("live", it) for it in group])
        times.append(time.perf_counter() - t)
        for it, text in zip(group, texts):
            result["batch"][it["id"]] = text.strip()
    result["times"]["batch"] = times

    times = []
    for it in manifest["shapes"]["refine"]:
        t = time.perf_counter()
        r = asr.transcribe_with_timestamps(win("refine", it), quiet=True)
        times.append(time.perf_counter() - t)
        result["refine"][it["id"]] = {
            "text": r.text,
            "words": [[w.text, w.start, w.end] for w in (r.words or [])],
        }
    result["times"]["refine"] = times
    result["peak_rss_mb"] = peak_rss_mb()
    Path(out).write_text(json.dumps(result, ensure_ascii=False))
    print(
        f"{asr.device}: load {result['load_s']:.1f}s, rss {result['peak_rss_mb']:.0f} MB, "
        f"live {sum(result['times']['live']):.1f}s, batch {sum(result['times']['batch']):.1f}s, "
        f"refine {sum(result['times']['refine']):.1f}s",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main(*sys.argv[1:])
