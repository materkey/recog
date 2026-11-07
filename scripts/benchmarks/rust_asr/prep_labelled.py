"""A labelled corpus: utterances from public Russian test sets with checked references.

ToneWebinars (Apache-2.0, webinar lectures, punctuated references) and FLEURS ru_ru
(CC-BY-4.0, read sentences, normalized references). Utterances up to 25 s, GigaAM's window.
Same layout as prep.py: corpus/refine.f32 holds the utterances, corpus/live.f32 the first 8
(the harness needs a live shape), manifest items carry "ref".

    python prep_labelled.py <out dir> [per-set count]
"""

import json
import subprocess
import sys
from pathlib import Path

import numpy as np
from datasets import Audio, load_dataset

SR = 16000
SETS = [
    ("tone_webinars", dict(path="Vikhrmodels/ToneWebinars", split="validation"), "text"),
    (
        "fleurs_ru",
        dict(
            path="parquet",
            split="train",
            data_files="hf://datasets/google/fleurs@refs%2Fconvert%2Fparquet/ru_ru/test/*.parquet",
        ),
        "transcription",
    ),
]


def decode(blob: bytes) -> np.ndarray:
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", "pipe:0", "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"],
        input=blob,
        check=True,
        capture_output=True,
    ).stdout
    return np.frombuffer(raw, dtype="<f4")


def main(out_dir: str, count: str = "300") -> None:
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    items, chunks, offset = [], [], 0
    for name, spec, field in SETS:
        ds = load_dataset(**spec, streaming=True).cast_column("audio", Audio(decode=False))
        taken = 0
        for i, row in enumerate(ds):
            ref = (row.get(field) or "").strip()
            pcm = decode(row["audio"]["bytes"])
            if not ref or not 1 * SR <= len(pcm) <= 25 * SR:
                continue
            items.append(
                {
                    "id": f"{name}/{i}",
                    "source": name,
                    "kind": "labelled",
                    "ref": ref,
                    "start": 0,
                    "offset": offset,
                    "len": len(pcm),
                }
            )
            chunks.append(pcm)
            offset += len(pcm)
            taken += 1
            if taken >= int(count):
                break
        print(name, taken, file=sys.stderr)
    audio = np.concatenate(chunks).astype("<f4")
    audio.tofile(out / "refine.f32")
    first = items[:8]
    audio[: first[-1]["offset"] + first[-1]["len"]].tofile(out / "live.f32")
    manifest = {"sample_rate": SR, "shapes": {"refine": items, "live": first}}
    (out / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=1))
    print(f"{offset / SR / 60:.1f} min", file=sys.stderr)


if __name__ == "__main__":
    main(*sys.argv[1:])
