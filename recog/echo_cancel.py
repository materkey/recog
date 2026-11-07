"""Echo cancellation on the microphone: what the speakers play leaves the transcript.

The chain, its numbers and why each part is there:
docs/plans/2026-10-30-mic-echo-cancel-design.md. In short: a high-pass and the
inverse of Speex's low cut, then Speex, then a suppressor of what Speex leaves
behind, and raw audio whenever there is no echo to remove. Passing an echo through
is today's behaviour and the lesser failure; losing the user's words is not.

Both streams come with time stamps on the host uptime clock (PortAudio's ADC time,
ScreenCaptureKit's presentation time) and are placed by them, not by counting
samples: unnoticed drift hurts Speex more than a sudden shift does.
"""

import math
import threading
from collections import deque
from typing import Optional, Tuple

import numpy as np
import speexdsp_aec

SR = 16000
FRAME = 160  # Speex
HOP = 256  # suppressor
BLOCK = 1280  # a whole number of both: decisions are made per block

# The reference leads the microphone by about 50 ms by their stamps. Speex's usable
# range is about 5-250 ms, so the microphone is paired 75 ms later to sit in the
# middle of it, which also covers the batches the system audio arrives in.
REFERENCE_LEAD = 1200
# The suppressor's reference offset follows the lead.
SUPPRESSOR_REFERENCE_DELAY = 1950
TAIL = 4096
HIGH_PASS_HZ = 60.0
# How much microphone to hold for a late reference before going on without it.
REFERENCE_WAIT = int(0.3 * SR)

# Raw audio while the reference has been this quiet for this long: Speex thins a
# voice even when it has nothing to cancel.
QUIET_RMS = 0.002
QUIET_HOLD_S = 0.3

# Echo path: peak correlation of the microphone and the reference.
PATH_WINDOW_S = 3.0
PATH_HOP_S = 0.5
PATH_MAX_LEAD_S = 0.3
PATH_BAND_HZ = (200.0, 3500.0)
PATH_ENTER = 0.2
PATH_LEAVE = 0.13
PATH_LEAVE_WINDOWS = 10


def _to16(x: np.ndarray) -> np.ndarray:
    return np.clip(x * 32767, -32768, 32767).astype(np.int16)


class _Speex:
    """Speex's canceller: the Rust port in rust/, bit-exact with pyaec 1.0.1, which
    ran it with 8 kHz settings whatever rate it was given; so does this. A block of
    whole frames goes through in one call, with the interpreter lock released."""

    def __init__(self):
        self._aec = speexdsp_aec.EchoCanceller(FRAME, TAIL)

    def block(self, mic: np.ndarray, reference: np.ndarray) -> np.ndarray:
        m, r = _to16(mic).astype("<i2"), _to16(reference).astype("<i2")
        out = np.frombuffer(self._aec.cancel(m.tobytes(), r.tobytes()), dtype="<i2")
        return out.astype(np.float64) / 32768.0


def inverse_low_cut_fir(taps: int = 1024, regularization: float = 1e-3) -> np.ndarray:
    """A causal inverse of what Speex does to a microphone with a silent reference.

    With a silent reference Speex is not transparent: white noise loses 14 dB at
    100-200 Hz, and a quiet voice fell entirely under the VAD threshold. Measured
    from the canceller with noise, so it follows the library it is used with.
    """
    noise = np.random.default_rng(0).normal(0, 0.02, 30 * SR)
    thinned = _Speex().block(noise, np.zeros(len(noise)))
    x, y = thinned[SR:], noise[SR:]
    size = 2 ** math.ceil(math.log2(len(x) + taps))
    X, Y = np.fft.rfft(x, size), np.fft.rfft(y, size)
    autocorrelation = np.fft.irfft(abs(X) ** 2)[:taps]
    cross = np.fft.irfft(Y * np.conj(X))[:taps]
    autocorrelation[0] *= 1 + regularization
    index = np.abs(np.arange(taps)[:, None] - np.arange(taps)[None, :])
    return np.linalg.solve(autocorrelation[index], cross)


class _HighPass:
    """Second-order Butterworth."""

    def __init__(self, cutoff: float):
        k = math.tan(math.pi * cutoff / SR)
        norm = 1 / (1 + math.sqrt(2) * k + k * k)
        self.b0 = norm
        self.a1 = 2 * (k * k - 1) * norm
        self.a2 = (1 - math.sqrt(2) * k + k * k) * norm
        self.z1 = self.z2 = 0.0

    def block(self, x: np.ndarray) -> np.ndarray:
        b0, a1, a2, z1, z2 = self.b0, self.a1, self.a2, self.z1, self.z2
        out = []
        for sample in x.tolist():
            y = b0 * sample + z1
            z1 = -2 * b0 * sample - a1 * y + z2
            z2 = b0 * sample - a2 * y
            out.append(y)
        self.z1, self.z2 = z1, z2
        return np.array(out)


class _Fir:
    def __init__(self, taps: np.ndarray):
        self.taps = taps
        self.tail = np.zeros(len(taps) - 1)

    def block(self, x: np.ndarray) -> np.ndarray:
        full = np.convolve(x, self.taps)
        full[: len(self.tail)] += self.tail
        self.tail = full[len(x) :]
        return full[: len(x)]


class _Suppressor:
    """Takes down what Speex leaves, in proportion to what the reference plays."""

    N = 512
    H = HOP

    def __init__(self, beta=2.0, floor=0.2, delay=SUPPRESSOR_REFERENCE_DELAY):
        self.beta, self.floor = beta, floor
        self.decay, self.smooth = 0.6, 0.6
        self.win = np.sqrt(np.hanning(self.N + 1)[:-1])
        bins = self.N // 2 + 1
        self.memory = np.zeros(bins)
        self.output_power = np.zeros(bins)
        self.coupling = np.zeros(bins)
        self.kernel = np.ones(9) / 9
        # The coupling tracks causally: 30th percentile, over 30 s, of the median
        # output-to-reference ratio per 2 s.
        self.block_hops = int(2.0 * SR / self.H)
        self.history = int(30.0 / 2.0)
        self.output_buffer = np.zeros(self.N)
        self.reference_buffer = np.zeros(self.N + delay)
        self.overlap = np.zeros(self.N)
        self.past = []
        self.ratios = []
        self.hops = 0

    def hop(self, output: np.ndarray, reference: np.ndarray) -> np.ndarray:
        H, N = self.H, self.N
        self.output_buffer = np.r_[self.output_buffer[H:], output]
        self.reference_buffer = np.r_[self.reference_buffer[H:], reference]
        E = np.fft.rfft(self.output_buffer * self.win)
        X = np.fft.rfft(self.reference_buffer[:N] * self.win)
        self.memory = np.maximum(abs(X) ** 2, self.decay * self.memory)
        spread = np.convolve(self.memory, self.kernel, mode="same")
        power = abs(E) ** 2
        self.output_power = self.smooth * self.output_power + (1 - self.smooth) * power
        gain = np.clip(
            1 - self.beta * self.coupling * spread / (self.output_power + 1e-14), self.floor, 1.0
        )
        if spread.mean() > 1e-2:  # unscaled FFT power: the reference above silence
            self.ratios.append(power / (spread + 1e-14))
        self.hops += 1
        if self.hops % self.block_hops == 0:
            if len(self.ratios) > self.block_hops // 3:
                self.past.append(np.median(np.array(self.ratios), axis=0))
                self.past = self.past[-self.history :]
                self.coupling = np.percentile(np.array(self.past), 30, axis=0)
            self.ratios = []
        y = np.fft.irfft(gain * E, N) * self.win
        self.overlap += y
        out = self.overlap[:H].copy()
        self.overlap = np.r_[self.overlap[H:], np.zeros(H)]
        return out


class TimelinePlacer:
    """Turns stamped blocks into one stream that holds to the stamps.

    A hole becomes zeros: audio closing up shifts the echo, while a hole barely
    troubles Speex (0.64% lost in blocks: -2.9 dB closed, -16.2 dB zero-filled). The
    device clock's slow drift against its stamps is taken up one sample at a time.
    """

    # Below this a difference is not a hole. Stamps jitter by under 0.1 sample and
    # drift keeps the difference under 2, while the microphone child loses 7-15
    # samples now and then: taken up as drift, such a hole left the echo misplaced
    # for seconds and Speex let it through.
    HOLE_SAMPLES = 4
    # Longer than this the capture was off: start over rather than fill it.
    GAP_S = 1.0
    DRIFT_SMOOTHING_S = 5.0

    def __init__(self, sample_rate: int = SR):
        self.sample_rate = sample_rate
        self.next_position: Optional[int] = None
        self.drift = 0.0

    def place(self, samples: np.ndarray, time_s: float) -> Tuple[np.ndarray, bool]:
        """The block as it belongs in the stream, and whether the stream started over."""
        position = time_s * self.sample_rate
        if (
            self.next_position is None
            or abs(position - self.next_position) > self.GAP_S * self.sample_rate
        ):
            jumped = self.next_position is not None
            self.next_position = round(position) + len(samples)
            self.drift = 0.0
            return samples, jumped
        difference = position - self.next_position
        if abs(difference) > self.HOLE_SAMPLES:
            skip = round(difference)
            self.drift = 0.0
        else:
            weight = min(1.0, len(samples) / (self.DRIFT_SMOOTHING_S * self.sample_rate))
            self.drift += weight * (difference - self.drift)
            skip = 0
            if abs(self.drift) >= 1.0:
                skip = 1 if self.drift > 0 else -1
                self.drift -= skip
        if skip > 0:
            samples = np.r_[np.zeros(skip, dtype=samples.dtype), samples]
        elif skip < 0:
            samples = samples[-skip:]
        self.next_position += len(samples)
        return samples, False


def peak_correlation(mic: np.ndarray, reference: np.ndarray) -> float:
    """Peak normalised correlation in the speech band, with the reference leading.

    `reference` is longer than `mic` by the largest lead and ends at the same moment.
    """
    max_lead = len(reference) - len(mic)
    size = 2 ** math.ceil(math.log2(len(reference) + len(mic)))
    freqs = np.fft.rfftfreq(size, 1 / SR)
    band = (freqs >= PATH_BAND_HZ[0]) & (freqs <= PATH_BAND_HZ[1])
    M = np.fft.rfft(mic, size) * band
    R = np.fft.rfft(reference, size) * band
    r = np.fft.irfft(R, size)[: len(reference)]
    m = np.fft.irfft(M, size)[: len(mic)]
    # correlation[k]: the microphone against the reference starting at k, so k == 0
    # is the largest lead and k == max_lead no lead at all.
    correlation = np.fft.irfft(R * np.conj(M), size)[: max_lead + 1]
    energy = np.r_[0.0, np.cumsum(r * r)]
    reference_norm = np.sqrt(energy[len(mic) : len(mic) + max_lead + 1] - energy[: max_lead + 1])
    return float(np.max(np.abs(correlation) / (np.linalg.norm(m) * reference_norm + 1e-12)))


class EchoPathDetector:
    """Does the microphone hear the reference at all?

    The default output does not tell where a call plays: an application can route to
    headphones while the default stays on the speakers, and running the chain with no
    echo costs a quiet voice 10% of its words. Starts absent. A wrong "absent" under a
    loud voice leaves an echo for a moment, the accepted lesser failure. A silent
    reference says nothing about the path, so such windows do not count.
    """

    def __init__(self, sample_rate: int = SR):
        self.window = int(PATH_WINDOW_S * sample_rate)
        self.lead = int(PATH_MAX_LEAD_S * sample_rate)
        self.hop = int(PATH_HOP_S * sample_rate)
        self.mic = np.zeros(self.window)
        self.reference = np.zeros(self.window + self.lead)
        self.since_window = 0
        self.recent: deque = deque(maxlen=PATH_LEAVE_WINDOWS)
        self.present = False

    def feed(self, mic: np.ndarray, reference: np.ndarray) -> None:
        n = len(mic)
        self.mic = np.r_[self.mic[n:], mic]
        self.reference = np.r_[self.reference[n:], reference]
        self.since_window += n
        if self.since_window < self.hop:
            return
        self.since_window = 0
        if self.reference[-self.window :].std() < QUIET_RMS:
            return
        peak = peak_correlation(self.mic, self.reference)
        if self.present:
            self.recent.append(peak)
            if (
                len(self.recent) == PATH_LEAVE_WINDOWS
                and float(np.median(self.recent)) < PATH_LEAVE
            ):
                self.present = False
                self.recent.clear()
        elif peak > PATH_ENTER:
            self.present = True
            self.recent.clear()


class _ReferenceRing:
    """The reference by its position on the timeline; one writer, one reader."""

    CAPACITY = 4 * SR

    def __init__(self):
        self.data = np.zeros(self.CAPACITY)
        self.end: Optional[int] = None

    def write(self, position: int, samples: np.ndarray) -> None:
        n = min(len(samples), self.CAPACITY)
        samples = samples[-n:]
        position += len(samples) - n
        if self.end is not None and position > self.end:
            # Whatever was never delivered is silence, not an older round of the ring.
            for start in range(self.end, position, self.CAPACITY):
                count = min(position - start, self.CAPACITY)
                self._put(start, np.zeros(count))
        self._put(position, samples)
        self.end = position + n if self.end is None else max(self.end, position + n)

    def _put(self, position: int, samples: np.ndarray) -> None:
        at = position % self.CAPACITY
        first = min(len(samples), self.CAPACITY - at)
        self.data[at : at + first] = samples[:first]
        self.data[: len(samples) - first] = samples[first:]

    def read(self, position: int, n: int) -> Tuple[np.ndarray, bool]:
        """`n` samples from `position`, and whether all of them are real."""
        if self.end is None or position + n > self.end or position + n <= self.end - self.CAPACITY:
            return np.zeros(n), False
        indices = np.arange(position, position + n) % self.CAPACITY
        return self.data[indices], position >= self.end - self.CAPACITY


# The inverse filter takes about a second to measure and never changes in a process.
_inverse_fir_lock = threading.Lock()
_inverse_fir: Optional[np.ndarray] = None


def _shared_inverse_fir() -> np.ndarray:
    global _inverse_fir
    with _inverse_fir_lock:
        if _inverse_fir is None:
            _inverse_fir = inverse_low_cut_fir()
        return _inverse_fir


class EchoStage:
    """The microphone goes in and comes out the same length, later, without the echo.

    `add_reference` comes from the system capture's reader thread, `add_mic` from the
    microphone's. The microphone is raw whenever any of these fails: the output can
    make an echo (built-in speakers, not muted), the echo path is present, the
    reference has not been quiet, the reference exists.
    """

    def __init__(self):
        self._lock = threading.Lock()
        self._reference = _ReferenceRing()
        self._reference_placer = TimelinePlacer()
        self._echo_possible = False
        self._mic_placer = TimelinePlacer()
        self._pending = np.zeros(0)
        self._position: Optional[int] = None
        self._highpass = _HighPass(HIGH_PASS_HZ)
        self._inverse = _Fir(_shared_inverse_fir())
        self._speex = _Speex()
        self._suppressor = _Suppressor()
        self._detector = EchoPathDetector()
        self._raw_delay = np.zeros(HOP)
        self._running = False
        self._cold = True
        self._quiet_frames = 0
        self._raw_share = 1.0
        self.active = False

    def add_reference(self, samples: np.ndarray, time_s: float, echo_possible: bool) -> None:
        with self._lock:
            placed, jumped = self._reference_placer.place(samples.astype(np.float64), time_s)
            end = self._reference_placer.next_position
            if jumped:
                self._reference = _ReferenceRing()
            self._reference.write(end - len(placed), placed)
            self._echo_possible = echo_possible

    def add_mic(self, samples: np.ndarray, time_s: float) -> np.ndarray:
        """Whatever is ready of the microphone, in the order it came, as float32."""
        placed, jumped = self._mic_placer.place(samples.astype(np.float64), time_s)
        out = []
        if jumped or self._position is None:
            out.append(self._raw_from_delay_line(self._pending))
            self._pending = np.zeros(0)
            self._position = self._mic_placer.next_position - len(placed)
        self._pending = np.r_[self._pending, placed]
        while len(self._pending) >= BLOCK:
            with self._lock:
                reference_end = self._reference.end
            ready = (
                reference_end is not None
                and reference_end >= self._position + REFERENCE_LEAD + BLOCK
            )
            if not ready and len(self._pending) < BLOCK + REFERENCE_WAIT:
                break
            block, self._pending = self._pending[:BLOCK], self._pending[BLOCK:]
            out.append(self._block(block))
            self._position += BLOCK
        return np.concatenate(out).astype(np.float32) if out else np.zeros(0, dtype=np.float32)

    def flush_raw(self) -> np.ndarray:
        """What is still held, raw: the stage is being left."""
        out = self._raw_from_delay_line(self._pending)
        self._pending = np.zeros(0)
        return np.r_[out, self._raw_delay].astype(np.float32)

    def _raw_from_delay_line(self, block: np.ndarray) -> np.ndarray:
        """`block` through the delay that keeps raw audio level with the suppressor's."""
        joined = np.r_[self._raw_delay, block]
        self._raw_delay = joined[len(block) :]
        return joined[: len(block)]

    def _block(self, mic: np.ndarray) -> np.ndarray:
        with self._lock:
            lead, has_reference = self._reference.read(self._position + REFERENCE_LEAD, BLOCK)
            present, _ = self._reference.read(self._position, BLOCK)
            echo_possible = self._echo_possible
        raw = self._raw_from_delay_line(mic)
        self._detector.feed(mic, present)
        wanted = has_reference and echo_possible and self._detector.present
        self.active = wanted

        if not wanted and not self._running:
            self._raw_share = 1.0
            return raw

        cancelled = self._cancel(mic, lead)
        cold = self._cold
        self._cold = False
        self._running = wanted
        if not wanted:
            # One more block, to fade to raw from audio that exists; then Speex pauses
            # keeping its filter, which beats a running or a reset one on the way back.
            self._cold = True
        return self._mix(raw, cancelled, lead, wanted, cold)

    def _cancel(self, mic: np.ndarray, reference: np.ndarray) -> np.ndarray:
        filtered = self._inverse.block(self._highpass.block(mic))
        output = self._speex.block(filtered, reference)
        return np.concatenate(
            [
                self._suppressor.hop(output[i : i + HOP], reference[i : i + HOP])
                for i in range(0, BLOCK, HOP)
            ]
        )

    def _mix(
        self,
        raw: np.ndarray,
        cancelled: np.ndarray,
        reference: np.ndarray,
        wanted: bool,
        cold: bool,
    ) -> np.ndarray:
        """A crossfade of one frame to raw or back, as each frame asks."""
        share = np.zeros(BLOCK)
        current = self._raw_share
        step = 1.0 / FRAME
        for i in range(0, BLOCK, FRAME):
            quiet = reference[i : i + FRAME].std() < QUIET_RMS
            self._quiet_frames = self._quiet_frames + 1 if quiet else 0
            target = not wanted or cold or self._quiet_frames * FRAME / SR >= QUIET_HOLD_S
            ramp = current + np.arange(1, FRAME + 1) * (step if target else -step)
            share[i : i + FRAME] = np.clip(ramp, 0.0, 1.0)
            current = float(share[i + FRAME - 1])
        self._raw_share = current
        return share * raw + (1.0 - share) * cancelled
