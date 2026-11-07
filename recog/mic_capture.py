"""Captures that read framed audio from a child process.

The microphone in a child process (`recog/mic_worker.py`, see why there) and, with
echo cancellation on, the system audio with its time stamps as the reference.
"""

import os
import struct
import sys
from pathlib import Path
from typing import List, Optional, Tuple

import numpy as np

from recog.install_config import INSTALL_CONFIG, REPO_ROOT
from recog.log_utils import log_rate_limited
from recog.mic_worker import BYTES_PER_SAMPLE, FRAME_HEADER
from recog.system_audio import SubprocessPCMCapture

# The Rust worker (`[live] mic_worker = "rust"`): the same arguments, frames and exit
# codes, but a callback that takes no lock, so CoreAudio never waits for it.
RUST_MIC_WORKER = REPO_ROOT / "rust" / "target" / "release" / "recog-mic"

# Below this a gap in ADC time is the clock's own jitter (measured under 1 ms).
ADC_HOLE_REPORT_S = 0.005
# macOS answers a refused microphone and a closed lid with exact zeros, which an
# open one never gives, so the transcript would just stay empty.
ONLY_ZEROS_REPORT_S = 30.0

# sck_audio_capture --frames: presentation time, wall clock, frame count, whether
# the default output can make an echo; float32 mono samples follow.
REFERENCE_HEADER = struct.Struct("<ddIB")


def mic_worker_argv(device_name: Optional[str], sample_rate: int) -> List[str]:
    choice = INSTALL_CONFIG.live.mic_worker
    if choice not in ("python", "rust"):
        raise ValueError(f'[live] mic_worker must be "python" or "rust", not {choice!r}')
    if choice == "rust" and os.access(RUST_MIC_WORKER, os.X_OK):
        argv = [str(RUST_MIC_WORKER), "--sample-rate", str(sample_rate)]
    else:
        if choice == "rust":
            print(
                f"[mic] {RUST_MIC_WORKER} is not built (./setup.sh builds it when cargo is "
                "installed): the microphone runs in the Python worker",
                file=sys.stderr,
            )
        worker = Path(__file__).resolve().with_name("mic_worker.py")
        argv = [sys.executable, "-I", str(worker), "--sample-rate", str(sample_rate)]
    if device_name is not None:
        argv += ["--device", device_name]
    return argv


def split_frames(data: bytes, header: struct.Struct) -> Tuple[List[tuple], bytes]:
    """Whole frames of `data` as (header fields, samples), and what is left over.

    The frame count is the third header field on every pipe.
    """
    frames = []
    offset = 0
    while len(data) - offset >= header.size:
        fields = header.unpack_from(data, offset)
        end = offset + header.size + fields[2] * BYTES_PER_SAMPLE
        if end > len(data):
            break
        samples = np.frombuffer(
            data, dtype=np.float32, count=fields[2], offset=offset + header.size
        )
        frames.append((fields, samples))
        offset = end
    return frames, data[offset:]


class ChildMicCapture(SubprocessPCMCapture):
    """Reads framed audio from the worker into the shared buffer.

    Restart, loss and stderr handling are the system capture's. A respawn keeps
    the buffer: the overlap the WAV writer relies on is still in it.
    """

    def __init__(
        self,
        device_name: Optional[str],
        sample_rate: int,
        argv: Optional[List[str]] = None,
        max_buffer_s: Optional[float] = None,
        echo_stage=None,
    ):
        super().__init__(
            argv or mic_worker_argv(device_name, sample_rate),
            name="mic",
            sample_rate=sample_rate,
            max_buffer_s=max_buffer_s,
        )
        self.device_name = device_name
        self._frame_tail = b""
        # ADC time expected of the next frame, None until a child's first one.
        self._next_adc_time: Optional[float] = None
        self._zeros_reported = False
        self._echo_stage = echo_stage
        # What the stage has been given and has given back, to stamp its output.
        self._stage_held = 0
        self._stage_active = False

    def _on_new_child(self) -> None:
        self._frame_tail = b""
        self._next_adc_time = None

    def _note_adc_time(self, adc_time: float, frames: int) -> None:
        """Logs lost audio: PortAudio reports no status for it."""
        if self._next_adc_time is not None and adc_time - self._next_adc_time >= ADC_HOLE_REPORT_S:
            log_rate_limited(
                "mic-adc-hole",
                f"[{self.name}] {(adc_time - self._next_adc_time) * 1000:.0f} ms hole in the captured audio",
            )
        self._next_adc_time = adc_time + frames / self.sample_rate

    def _note_zeros(self) -> None:
        if self.digital_silence_s() < ONLY_ZEROS_REPORT_S:
            self._zeros_reported = False
        elif not self._zeros_reported:
            self._zeros_reported = True
            print(
                f"[{self.name}] only exact zeros from the microphone for "
                f"{ONLY_ZEROS_REPORT_S:.0f}s: is the lid closed, or its permission not granted to Recog?",
                file=sys.stderr,
            )

    def _through_stage(
        self, samples: np.ndarray, adc_time: float, wall_time: float
    ) -> Tuple[np.ndarray, float]:
        """What leaves the echo stage for this frame, and the wall clock of its first sample.

        Whatever goes wrong there, the microphone goes on raw.
        """
        stage = self._echo_stage
        if stage is None:
            return samples, wall_time
        try:
            output = stage.add_mic(samples, adc_time)
        except Exception as e:
            print(
                f"[{self.name}] Echo cancellation failed, the microphone stays raw: {e}",
                file=sys.stderr,
            )
            self._echo_stage = None
            held = self._stage_held
            self._stage_held = 0
            return np.r_[stage.flush_raw(), samples], wall_time - held / self.sample_rate
        self._stage_held += len(samples) - len(output)
        active = getattr(stage, "active", False)
        if active != self._stage_active:
            self._stage_active = active
            print(
                f"[{self.name}] " + ("Echo cancelling" if active else "Back to the raw microphone"),
                file=sys.stderr,
            )
        return output, wall_time + (
            len(samples) - len(output) - self._stage_held
        ) / self.sample_rate

    def _decode_read(self, data: bytes) -> List[Tuple[np.ndarray, float]]:
        frames, self._frame_tail = split_frames(self._frame_tail + data, FRAME_HEADER)
        parts = []
        captured_at = None
        for (adc_time, wall_time, count), samples in frames:
            self._note_adc_time(adc_time, count)
            output, output_at = self._through_stage(samples, adc_time, wall_time)
            if len(output):
                parts.append(output)
                if captured_at is None:
                    captured_at = output_at
        self._note_zeros()
        if not parts:
            return []
        return [(np.concatenate(parts), captured_at)]


class FramedSystemCapture(SubprocessPCMCapture):
    """System audio with its time stamps, handed to the echo stage on the way."""

    def __init__(
        self, argv: List[str], echo_stage, sample_rate: int, max_buffer_s: Optional[float] = None
    ):
        super().__init__(
            argv, name="screencapture", sample_rate=sample_rate, max_buffer_s=max_buffer_s
        )
        self._echo_stage = echo_stage
        self._frame_tail = b""

    def _on_new_child(self) -> None:
        self._frame_tail = b""

    def _decode_read(self, data: bytes) -> List[Tuple[np.ndarray, float]]:
        frames, self._frame_tail = split_frames(self._frame_tail + data, REFERENCE_HEADER)
        parts = []
        captured_at = None
        for (presentation_time, wall_time, _count, echo_possible), samples in frames:
            parts.append(samples)
            if captured_at is None:
                captured_at = wall_time
            if self._echo_stage is not None:
                try:
                    self._echo_stage.add_reference(samples, presentation_time, bool(echo_possible))
                except Exception as e:
                    # Without new reference the stage leaves the microphone raw by itself.
                    print(
                        f"[{self.name}] Echo cancellation lost its reference: {e}", file=sys.stderr
                    )
                    self._echo_stage = None
        if not parts:
            return []
        return [(np.concatenate(parts), captured_at)]
