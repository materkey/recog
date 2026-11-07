"""Writes tests/fixtures/speexdsp_aec_pyaec.npz: pyaec's output on two synthetic echoes.

The fixture lets tests/test_speexdsp_aec.py check the module recog loads against
pyaec 1.0.1 without pyaec installed. Run once, with an interpreter that has pyaec:

    python rust/speexdsp-aec-py/make_fixture.py tests/fixtures/speexdsp_aec_pyaec.npz

Settings are recog's (echo_cancel.py): 160-sample frames, a 4096-sample tail; pyaec
ignores the rate it is given and runs with 8 kHz settings.
"""

import sys

import numpy as np
import pyaec

FRAME, TAIL, SR = 160, 4096, 16000


def case(seed: int, seconds: float = 3.0) -> tuple[np.ndarray, np.ndarray]:
    """A far end, its echo through a decaying room, noise, and from 1.5 s a near talker."""
    rng = np.random.default_rng(seed)
    n = int(seconds * SR) // FRAME * FRAME
    far = rng.normal(0, 4000, n).clip(-32768, 32767)
    room = rng.normal(0, 0.3, 300) * np.exp(-np.arange(300) / 60)
    echo = np.convolve(far, room)[:n]
    near = np.where(np.arange(n) >= 1.5 * SR, 3000 * np.sin(np.arange(n) / (20 + 10 * seed)), 0)
    mic = echo + rng.normal(0, 300, n) + near
    return mic.clip(-32768, 32767).astype(np.int16), far.astype(np.int16)


def main(out: str) -> None:
    arrays = {}
    for seed in (0, 1):
        mic, far = case(seed)
        aec = pyaec.Aec(FRAME, TAIL, SR, False)
        frames = [
            aec.cancel_echo(mic[i : i + FRAME].tolist(), far[i : i + FRAME].tolist())
            for i in range(0, len(mic), FRAME)
        ]
        arrays[f"mic{seed}"], arrays[f"far{seed}"] = mic, far
        arrays[f"out{seed}"] = np.concatenate(frames).astype(np.int16)
    np.savez_compressed(out, **arrays)


if __name__ == "__main__":
    main(*sys.argv[1:])
