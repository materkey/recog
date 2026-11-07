"""Scores the runs: agreement with recog's PyTorch output, WER against checked references,
speed, word timings, memory, abort latency. Prints Markdown.

    python score.py <corpus dir> <run dir> [reference run name, default pytorch-*]
"""

import difflib
import json
import re
import statistics
import sys
from pathlib import Path

SR = 16000


def norm(text: str) -> list[str]:
    text = text.lower().replace("ё", "е")
    text = re.sub(r"[^\w\s]|_", " ", text)
    return text.split()


def edits(ref: list[str], hyp: list[str]) -> int:
    prev = list(range(len(hyp) + 1))
    for i, r in enumerate(ref, 1):
        cur = [i] + [0] * len(hyp)
        for j, h in enumerate(hyp, 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (r != h))
        prev = cur
    return prev[-1]


def wer(pairs) -> float:
    e = n = 0
    for ref, hyp in pairs:
        r = norm(ref)
        e += edits(r, norm(hyp))
        n += len(r)
    return 100.0 * e / max(n, 1)


def peak_mb(log: Path) -> float | None:
    for line in log.read_text(errors="replace").splitlines():
        if "maximum resident set size" in line:
            return int(line.split()[0]) / 2**20
        if "Maximum resident set size" in line:  # GNU time -v, KiB
            return int(line.split()[-1]) / 1024
    return None


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))] if xs else float("nan")


def timing_diffs(ref_words, hyp_words) -> list[int]:
    a = [norm(w[0])[0] if norm(w[0]) else "" for w in ref_words]
    b = [norm(w[0])[0] if norm(w[0]) else "" for w in hyp_words]
    out = []
    for blk in difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks():
        for k in range(blk.size):
            out.append(abs(ref_words[blk.a + k][1] - hyp_words[blk.b + k][1]))
    return out


def main(corpus: str, runs: str, ref_name: str | None = None) -> None:
    manifest = json.loads((Path(corpus) / "manifest.json").read_text())
    items = {s: v for s, v in manifest["shapes"].items()}
    runs_dir = Path(runs)
    results = {p.stem: json.loads(p.read_text()) for p in sorted(runs_dir.glob("*.json"))}
    ref_name = ref_name or next((n for n in results if n.startswith("pytorch")), None)
    labelled = any("ref" in it for it in items["refine"])
    audio_s = {s: sum(it["len"] for it in v) / SR for s, v in items.items()}

    print(
        f"Corpus: {len(items['live'])} live windows ({audio_s['live'] / 60:.1f} min), "
        f"{len(items['refine'])} refine windows ({audio_s['refine'] / 60:.1f} min)\n"
    )
    head = "| run | device | load s | peak RSS MB | live RTF | live p50 ms | live p95 ms | 8 clips ms (p50) | refine RTF | abort ms (p50) |"
    print(head)
    print("|---|---|" + "---:|" * 8)
    for name, r in results.items():
        t = r["times"]
        log = runs_dir / f"{name}.log"
        rss = peak_mb(log) if log.exists() else None
        aborts = [a for a in r.get("abort_ms") or [] if a is not None]
        ab = (
            f"{statistics.median(aborts):.0f}"
            if aborts
            else ("n/a" if r.get("abort_ms") is not None else "–")
        )
        print(
            f"| {name} | {str(r['device'])[:40]} | {r['load_s']:.1f} | {rss or 0:.0f} | "
            f"{sum(t['live']) / audio_s['live']:.4f} | {pct(t['live'], 0.5) * 1000:.0f} | {pct(t['live'], 0.95) * 1000:.0f} | "
            f"{pct(t['batch'], 0.5) * 1000:.0f} | {sum(t['refine']) / audio_s['refine']:.4f} | {ab} |"
        )

    if labelled:
        print(
            "\nWER against the checked references (normalized: lowercase, ё→е, no punctuation), %\n"
        )
        sources = sorted({it["source"] for it in items["refine"]})
        print("| run | " + " | ".join(sources) + " |")
        print("|---|" + "---:|" * len(sources))
        for name, r in results.items():
            cells = [
                f"{wer((it['ref'], r['refine'][it['id']]['text']) for it in items['refine'] if it['source'] == s):.2f}"
                for s in sources
            ]
            print(f"| {name} | " + " | ".join(cells) + " |")

    if ref_name:
        ref = results[ref_name]
        print(f"\nAgreement with {ref_name}: word disagreement %, normalized (same windows)\n")
        kinds = sorted({it["kind"] for it in items["live"]})
        cols = (
            [f"live {k}" for k in kinds]
            + [f"refine {k}" for k in kinds]
            + ["refine text identical %", "word start |Δ| p50/p95 ms"]
        )
        print("| run | " + " | ".join(cols) + " |")
        print("|---|" + "---:|" * len(cols))
        for name, r in results.items():
            if name == ref_name:
                continue
            cells = []
            for shape in ("live", "refine"):
                for k in kinds:
                    get = (lambda d, i: d[i]) if shape == "live" else (lambda d, i: d[i]["text"])
                    cells.append(
                        f"{wer((get(ref[shape], it['id']), get(r[shape], it['id'])) for it in items[shape] if it['kind'] == k):.2f}"
                    )
            same = [
                ref["refine"][it["id"]]["text"].strip() == r["refine"][it["id"]]["text"].strip()
                for it in items["refine"]
            ]
            cells.append(f"{100 * sum(same) / len(same):.0f}")
            d = [
                x
                for it in items["refine"]
                for x in timing_diffs(
                    ref["refine"][it["id"]]["words"], r["refine"][it["id"]]["words"]
                )
            ]
            cells.append(f"{pct(d, 0.5):.0f} / {pct(d, 0.95):.0f}")
            print(f"| {name} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main(*sys.argv[1:])
