"""Echo cancellation on the microphone (`recog/echo_cancel.py`).

Signals here are synthetic: the takes behind the numbers in
docs/plans/2026-10-30-mic-echo-cancel-design.md hold a voice and stay out of git.
What the tests hold is what the design promises a user:
- speech from the speakers leaves the microphone;
- without an echo the microphone stays exactly what it was: no reference, another
  output, a reference nobody hears, or a quiet reference;
- both streams are placed by their time stamps, holes filled with zeros.
"""

import sys
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

pytest.importorskip("pyaec")

from recog.echo_cancel import (
    BLOCK,
    SR,
    EchoPathDetector,
    EchoStage,
    TimelinePlacer,
    inverse_low_cut_fir,
    peak_correlation,
)

ECHO_DELAY_S = 0.05
START_S = 1000.0


def _speech_like(seconds, seed, level=0.05):
    """Noise shaped to the speech band, in bursts with short pauses."""
    rng = np.random.default_rng(seed)
    n = int(seconds * SR)
    spectrum = np.fft.rfft(rng.normal(0, 1, n))
    freqs = np.fft.rfftfreq(n, 1 / SR)
    spectrum[(freqs < 200) | (freqs > 3500)] = 0
    x = np.fft.irfft(spectrum, n)
    x = x / x.std()
    gate = (np.sin(2 * np.pi * 0.7 * np.arange(n) / SR) > -0.7).astype(float)
    return (x * gate * level).astype(np.float32)


def _echo_of(reference):
    """What the microphone hears: delayed, filtered, with room noise."""
    room = np.zeros(int(0.06 * SR))
    for lag_ms, gain in ((0, 0.4), (3, -0.25), (11, 0.12), (37, 0.05)):
        room[int(lag_ms * SR / 1000)] = gain
    delay = int(ECHO_DELAY_S * SR)
    echo = np.convolve(np.r_[np.zeros(delay), reference], room)[: len(reference)]
    return (echo + np.random.default_rng(99).normal(0, 0.0007, len(reference))).astype(np.float32)


def _run(mic, reference, echo_possible=True, block=160, ref_block=341):
    """Feeds both streams in real-time order; returns what the stage gave back."""
    stage = EchoStage()
    out = []
    ref_at = 0
    for i in range(0, len(mic) - block + 1, block):
        t = START_S + i / SR
        while (
            reference is not None and ref_at + ref_block <= len(reference) and ref_at <= i + block
        ):
            stage.add_reference(
                reference[ref_at : ref_at + ref_block], START_S + ref_at / SR, echo_possible
            )
            ref_at += ref_block
        out.append(stage.add_mic(mic[i : i + block], t))
    return np.concatenate(out), stage


def _rms(x):
    return float(np.sqrt(np.mean(np.square(x))))


def test_speech_from_the_speakers_leaves_the_microphone():
    """given: a microphone hearing only the speakers
    when: the stage has had time to settle
    then: the level is far below the raw one, under the VAD threshold"""
    reference = _speech_like(30, seed=1)
    mic = _echo_of(reference)

    out, stage = _run(mic, reference)

    assert stage.active
    tail = int(10 * SR)
    assert _rms(out[-tail:]) < _rms(mic[-tail:]) * 0.2
    assert _rms(out[-tail:]) < 0.005


def test_a_voice_over_the_echo_stays():
    """given: the user speaks over the echo, at the level of a quiet voice
    when: the stage runs
    then: the voice keeps most of its level while the echo goes"""
    reference = _speech_like(40, seed=1)
    voice = _speech_like(40, seed=2, level=0.012)
    voice[: 25 * SR] = 0
    mic = _echo_of(reference) + voice

    out, _ = _run(mic, reference)

    tail = int(10 * SR)
    expected = _rms(voice[-tail:])
    assert _rms(out[-tail:]) > expected * 0.6
    assert _rms(out[-tail:]) < expected * 1.6


def _delay_of(out, mic):
    """How many samples `out` lags behind `mic`."""
    n = 2 * SR
    return max(range(4000), key=lambda lag: float(np.dot(out[lag : lag + n], mic[:n])))


def test_no_reference_passes_the_microphone_unchanged():
    """given: the system capture is absent, so no reference ever comes
    when: the microphone runs
    then: nothing is held back for long and every sample is the raw one"""
    mic = _speech_like(5, seed=3)

    out, stage = _run(mic, None)

    assert not stage.active
    assert len(out) > len(mic) - int(0.5 * SR)
    delay = _delay_of(out, mic)
    assert np.array_equal(out[delay:], mic[: len(out) - delay])


def test_output_elsewhere_passes_the_microphone_unchanged():
    """given: the output is headphones, so the reference plays and nobody hears it
    when: an echo-looking signal is at the microphone
    then: the stage does not touch it"""
    reference = _speech_like(10, seed=1)
    mic = _echo_of(reference)

    out, stage = _run(mic, reference, echo_possible=False)

    assert not stage.active
    delay = _delay_of(out, mic)
    assert np.array_equal(out[delay:], mic[: len(out) - delay])


def test_reference_nobody_hears_passes_the_microphone_unchanged():
    """given: built-in speakers are the default, but the call plays elsewhere: the
    reference plays and the microphone hears only a voice
    when: the stage runs
    then: no echo path is found and the voice is untouched"""
    reference = _speech_like(15, seed=1)
    mic = _speech_like(15, seed=2, level=0.02)

    out, stage = _run(mic, reference)

    assert not stage.active
    delay = _delay_of(out, mic)
    assert np.array_equal(out[delay:], mic[: len(out) - delay])


def test_quiet_reference_gives_the_raw_voice_back():
    """given: the stage is settled on an echo, then the speakers go silent and the user speaks
    when: the reference has been quiet long enough
    then: the output is the raw microphone again, not the thinned Speex output"""
    n = 40 * SR
    reference = _speech_like(40, seed=1)
    reference[25 * SR :] = 0
    voice = _speech_like(40, seed=2, level=0.012)
    voice[: 28 * SR] = 0
    mic = _echo_of(reference) + voice

    out, stage = _run(mic, reference)

    assert stage.active
    delay = _delay_of(out[29 * SR :], mic[29 * SR :])
    # out[i] is mic[i - delay]: the stage holds audio back
    assert np.allclose(
        out[35 * SR + delay : n - 2 * SR], mic[35 * SR : n - 2 * SR - delay], atol=1e-6
    )


def test_output_switch_pauses_the_filter_instead_of_losing_it():
    """given: settled on the speakers, the output leaves for 10 s and comes back
    when: the echo returns with the output
    then: the first second back is already suppressed well, far better than from nothing"""
    reference = _speech_like(50, seed=1)
    mic = _echo_of(reference)
    stage = EchoStage()
    out = []
    block = 160
    ref_at = 0
    for i in range(0, len(mic) - block + 1, block):
        away = 25 * SR <= i < 35 * SR
        while ref_at + 341 <= len(reference) and ref_at <= i + block:
            stage.add_reference(reference[ref_at : ref_at + 341], START_S + ref_at / SR, not away)
            ref_at += 341
        out.append(
            stage.add_mic(
                np.zeros(block, np.float32) if away else mic[i : i + block], START_S + i / SR
            )
        )
    out = np.concatenate(out)

    back = out[36 * SR : 38 * SR]
    fresh = _run(mic[: 2 * SR], reference[: 2 * SR])[0]
    assert _rms(back) < _rms(fresh[-SR:])


def test_hole_in_the_microphone_is_filled_and_keeps_the_filter():
    """given: the microphone lost 40 ms, which audio closing up would turn into a shift
    when: the next block arrives with its real time
    then: the output stays as long as the time that passed, and the echo stays down"""
    reference = _speech_like(30, seed=1)
    mic = _echo_of(reference)
    stage = EchoStage()
    block = 160
    out = []
    ref_at = 0
    hole = range(20 * SR, 20 * SR + 640)
    for i in range(0, len(mic) - block + 1, block):
        while ref_at + 341 <= len(reference) and ref_at <= i + block:
            stage.add_reference(reference[ref_at : ref_at + 341], START_S + ref_at / SR, True)
            ref_at += 341
        if i in hole:
            continue
        out.append(stage.add_mic(mic[i : i + block], START_S + i / SR))
    out = np.concatenate(out)

    assert abs(len(out) - len(mic)) < int(0.5 * SR)
    assert _rms(out[-5 * SR :]) < 0.005


# --- the timeline -------------------------------------------------------------


def test_consecutive_blocks_stay_untouched():
    placer = TimelinePlacer(SR)
    a, _ = placer.place(np.ones(160, np.float32), 10.0)
    b, _ = placer.place(np.ones(160, np.float32) * 2, 10.01)
    assert len(a) == 160 and b.tolist() == [2.0] * 160


def test_hole_is_zero_filled_to_the_block_time():
    placer = TimelinePlacer(SR)
    placer.place(np.ones(160, np.float32), 10.0)
    placed, jumped = placer.place(np.ones(160, np.float32), 10.05)
    assert not jumped
    assert len(placed) == 800
    assert not placed[:640].any() and placed[640:].all()


def test_overlap_is_dropped_to_the_block_time():
    placer = TimelinePlacer(SR)
    placer.place(np.ones(1600, np.float32), 10.0)
    placed, _ = placer.place(np.arange(320, dtype=np.float32), 10.09)
    assert placed.tolist() == list(range(160, 320))


def test_slow_clock_drift_moves_one_sample_at_a_time():
    """given: a device whose samples run 0.1 per second behind their stamps, as measured
    when: minutes pass
    then: single zero samples hold the stream to the stamps, no larger steps"""
    placer = TimelinePlacer(SR)
    rate = 15999.9
    t = 100.0
    extra = 0
    for _ in range(int(120 * 100)):
        placed, _ = placer.place(np.ones(160, np.float32), t)
        extra_now = len(placed) - 160
        assert abs(extra_now) <= 1
        extra += extra_now
        t += 160 / rate
    assert 8 <= extra <= 16


def test_long_gap_starts_over_instead_of_filling_it():
    """given: the capture was off between meetings
    when: audio comes back an hour later
    then: nobody gets an hour of zeros"""
    placer = TimelinePlacer(SR)
    placer.place(np.ones(160, np.float32), 10.0)
    placed, jumped = placer.place(np.ones(160, np.float32), 3610.0)
    assert jumped and len(placed) == 160


# --- the echo path ---------------------------------------------------------------


def test_correlation_sees_a_delayed_copy_and_not_another_signal():
    reference = _speech_like(3.3, seed=1).astype(float)
    window = int(3 * SR)
    lead = int(0.3 * SR)
    echo = np.r_[np.zeros(800), reference][: len(reference)][lead : lead + window]
    other = _speech_like(3, seed=2).astype(float)
    assert (
        peak_correlation(echo + np.random.default_rng(1).normal(0, 1e-3, window), reference) > 0.5
    )
    assert peak_correlation(other, reference) < 0.1


def test_detector_enters_on_echo_leaves_after_it_and_ignores_a_silent_reference():
    detector = EchoPathDetector(SR)
    reference = _speech_like(60, seed=1).astype(float)
    echo = _echo_of(reference.astype(np.float32)).astype(float)
    voice = _speech_like(60, seed=2, level=0.02).astype(float)
    assert not detector.present

    entered_at = None
    for i in range(0, 15 * SR, BLOCK):
        detector.feed(echo[i : i + BLOCK], reference[i : i + BLOCK])
        if detector.present and entered_at is None:
            entered_at = i / SR
    assert entered_at is not None and entered_at < 5

    silence = np.zeros(BLOCK)
    for _ in range(30 * SR // BLOCK):
        detector.feed(np.random.default_rng(5).normal(0, 7e-4, BLOCK), silence)
    assert detector.present, "a silent reference says nothing about the path"

    left_at = None
    for i in range(15 * SR, 40 * SR, BLOCK):
        detector.feed(voice[i : i + BLOCK], reference[i : i + BLOCK])
        if not detector.present and left_at is None:
            left_at = (i - 15 * SR) / SR
    assert left_at is not None and left_at < 12


# --- the chain ------------------------------------------------------------------


def test_inverse_filter_lifts_what_speex_cuts_and_leaves_the_rest():
    """given: the filter measured from pyaec itself
    then: it lifts the low band Speex cuts by about 14 dB, and is near 0 dB above 1 kHz"""
    h = inverse_low_cut_fir()
    assert len(h) == 1024
    response = np.fft.rfft(h, 16384)
    freqs = np.fft.rfftfreq(16384, 1 / SR)

    def db(frequency):
        return 20 * np.log10(abs(response[np.argmin(abs(freqs - frequency))]))

    assert db(150) > 8
    assert abs(db(2000)) < 1.5
