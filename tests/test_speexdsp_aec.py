"""The Rust echo canceller recog loads (rust/speexdsp-aec-py) against pyaec's own output.

The fixture is pyaec 1.0.1's output on two synthetic echoes, written by
rust/speexdsp-aec-py/make_fixture.py; the crate's own parity tests cover 80 cases.
"""

from pathlib import Path

import numpy as np
import pytest

speexdsp_aec = pytest.importorskip("speexdsp_aec")

FIXTURE = Path(__file__).parent / "fixtures" / "speexdsp_aec_pyaec.npz"
FRAME, TAIL = 160, 4096  # recog/echo_cancel.py
BLOCK = 1280  # what EchoStage hands it at a time


def _cancel(aec, mic, far, step):
    out = [
        aec.cancel(mic[i : i + step].tobytes(), far[i : i + step].tobytes())
        for i in range(0, len(mic), step)
    ]
    return np.frombuffer(b"".join(out), dtype="<i2")


@pytest.fixture(scope="module")
def recorded():
    with np.load(FIXTURE) as data:
        return [(data[f"mic{s}"], data[f"far{s}"], data[f"out{s}"]) for s in (0, 1)]


@pytest.mark.parametrize("step", [FRAME, BLOCK])
def test_pyaec_settings_any_block_size_output_is_pyaecs_bit_for_bit(recorded, step):
    """given: recog's settings and pyaec's output on two echoes with a near talker
    when: the microphone goes through in frames or in EchoStage's blocks
    then: every sample is pyaec's"""
    for mic, far, expected in recorded:
        aec = speexdsp_aec.EchoCanceller(FRAME, TAIL)
        np.testing.assert_array_equal(_cancel(aec, mic, far, step), expected)


def test_reset_cancelling_again_repeats_the_first_pass(recorded):
    """given: a canceller that has adapted to one echo
    when: it is reset and given the same audio again
    then: it answers as a new one did"""
    mic, far, expected = recorded[0]
    aec = speexdsp_aec.EchoCanceller(FRAME, TAIL)
    _cancel(aec, mic, far, BLOCK)
    aec.reset()
    np.testing.assert_array_equal(_cancel(aec, mic, far, BLOCK), expected)


@pytest.mark.parametrize(
    "mic,far",
    [
        (b"\0" * 321, b"\0" * 321),  # an odd byte
        (b"\0" * 320, b"\0" * 640),  # lengths differ
        (b"\0" * 200, b"\0" * 200),  # not whole frames
    ],
)
def test_malformed_audio_cancel_raises_value_error(mic, far):
    """given: audio that is not the same whole number of 16-bit frames on both sides
    when: it is cancelled
    then: ValueError, not a short or garbled output"""
    with pytest.raises(ValueError):
        speexdsp_aec.EchoCanceller(FRAME, TAIL).cancel(mic, far)


def test_frame_size_speex_cannot_do_construction_raises_value_error():
    """given: a frame size with a prime factor above 17, which KISS FFT cannot do
    when: a canceller is made
    then: ValueError instead of speexdsp's abort"""
    with pytest.raises(ValueError, match="frame size"):
        speexdsp_aec.EchoCanceller(19 * 2, TAIL)


def test_defaults_settings_are_pyaecs():
    """given: no sampling rate
    when: a canceller is made
    then: it runs with the 8 kHz settings pyaec always uses"""
    aec = speexdsp_aec.EchoCanceller(FRAME, TAIL)
    assert (aec.frame_size, aec.sampling_rate) == (FRAME, 8000)
