"""pyaec's output on the cases of tests/adapted_golden.rs, as that test's fixture.

usage: python adapted_golden.py CASES_DIR > adapted_golden.txt

CASES_DIR is what the test's ignored writer made: a directory per case with
meta.json, mic.i16 and ref.i16 (little-endian int16). pyaec 1.0.1 runs each one the
way recog called it, one cancel_echo per frame. It never sets the canceller's
sampling rate, so the rate passed here changes nothing.
"""

import hashlib
import importlib.metadata
import json
import platform
import sys
from array import array
from pathlib import Path

import pyaec

BLOCK_FRAMES = 50
LAST_SAMPLES = 8


def samples(path: Path) -> array:
    values = array("h")
    values.frombytes(path.read_bytes())
    return values


def digest(values: array) -> str:
    return hashlib.sha256(values.tobytes()).hexdigest()


def cancel(mic: array, far: array, frame: int, filter_length: int) -> array:
    aec = pyaec.Aec(frame, filter_length, 16000, False)
    out = array("h")
    for i in range(0, len(mic), frame):
        out.extend(aec.cancel_echo(mic[i : i + frame].tolist(), far[i : i + frame].tolist()))
    return out


def line(case: Path) -> str:
    meta = json.loads((case / "meta.json").read_text())
    frame = meta["frame_size"]
    mic, far = samples(case / "mic.i16"), samples(case / "ref.i16")
    out = cancel(mic, far, frame, meta["filter_length"])
    block = BLOCK_FRAMES * frame
    blocks = ",".join(digest(out[i : i + block])[:16] for i in range(0, len(out), block))
    last = ",".join(map(str, out[-LAST_SAMPLES:]))
    return f"{case.name} mic={digest(mic)} ref={digest(far)} out={digest(out)} blocks={blocks} last={last}"


def main() -> int:
    assert sys.byteorder == "little", "the cases are little-endian int16"
    library = Path(pyaec.lib._name)
    print(
        f"# pyaec {importlib.metadata.version('pyaec')} on {platform.system()} {platform.machine()},"
        f" {library.name} sha256 {hashlib.sha256(library.read_bytes()).hexdigest()}"
    )
    print("# written by tests/fixtures/adapted_golden.py; see tests/adapted_golden.rs")
    for case in sorted(path for path in Path(sys.argv[1]).iterdir() if path.is_dir()):
        print(line(case))
    return 0


if __name__ == "__main__":
    sys.exit(main())
