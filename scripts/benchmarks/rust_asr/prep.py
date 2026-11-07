"""Cut the source recordings into the shapes recog feeds the model.

live:   3 s windows every 2.5 s (the mixed microphone/system path, `live.py`)
refine: 20 s windows every 19 s (refine and the offline path, `config.py`)
Silent windows (RMS below recog's VAD threshold 0.005) are dropped, as recog does.

Writes corpus/<shape>.f32 (concatenated little-endian float32, 16 kHz mono) and
corpus/manifest.json with each window's source, start, offset and length in samples.
"""

import json
import subprocess
import sys
from pathlib import Path

import numpy as np

SR = 16000
VAD_RMS = 0.005
SHAPES = {"live": (3.0, 2.5), "refine": (20.0, 19.0)}


def load(path: Path, start_s: float, dur_s: float) -> np.ndarray:
    raw = subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-ss",
            str(start_s),
            "-t",
            str(dur_s),
            "-i",
            str(path),
            "-ac",
            "1",
            "-ar",
            str(SR),
            "-f",
            "f32le",
            "-",
        ],
        check=True,
        capture_output=True,
    ).stdout
    return np.frombuffer(raw, dtype="<f4")


def main(sources_file: str, out_dir: str) -> None:
    sources = json.loads(Path(sources_file).read_text())
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    manifest = {"sample_rate": SR, "shapes": {}}
    for shape, (win, step) in SHAPES.items():
        items, chunks, offset = [], [], 0
        for src in sources:
            audio = load(Path(src["path"]).expanduser(), src.get("start", 0), src["duration"])
            n, s = int(win * SR), int(step * SR)
            for i, at in enumerate(range(0, max(1, len(audio) - SR // 2), s)):
                piece = audio[at : at + n]
                if len(piece) < SR // 2 or float(np.sqrt(np.mean(piece**2))) < VAD_RMS:
                    continue
                items.append(
                    {
                        "id": f"{src['name']}/{shape}/{i}",
                        "source": src["name"],
                        "kind": src["kind"],
                        "start": at,
                        "offset": offset,
                        "len": len(piece),
                    }
                )
                chunks.append(piece)
                offset += len(piece)
        np.concatenate(chunks).astype("<f4").tofile(out / f"{shape}.f32")
        manifest["shapes"][shape] = items
        print(shape, len(items), "windows", f"{offset / SR / 60:.1f} min", file=sys.stderr)
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1))


if __name__ == "__main__":
    main(*sys.argv[1:])
