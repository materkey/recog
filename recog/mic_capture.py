"""The microphone in a child process (`recog/mic_worker.py`), see why there."""

import sys
from pathlib import Path
from typing import List, Optional, Tuple

import numpy as np

from recog.log_utils import log_rate_limited
from recog.mic_worker import BYTES_PER_SAMPLE, FRAME_HEADER
from recog.system_audio import SubprocessPCMCapture

# Below this a gap in ADC time is the clock's own jitter (measured under 1 ms).
ADC_HOLE_REPORT_S = 0.005
# macOS answers a refused microphone with exact zeros, which a real one never
# gives, so the transcript would just stay empty.
ONLY_ZEROS_REPORT_S = 30.0


def mic_worker_argv(device_name: Optional[str], sample_rate: int) -> List[str]:
    worker = Path(__file__).resolve().with_name("mic_worker.py")
    argv = [sys.executable, "-I", str(worker), "--sample-rate", str(sample_rate)]
    if device_name is not None:
        argv += ["--device", device_name]
    return argv


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
                f"{ONLY_ZEROS_REPORT_S:.0f}s: is its permission granted to Recog?",
                file=sys.stderr,
            )

    def _decode_read(self, data: bytes) -> List[Tuple[np.ndarray, float]]:
        data = self._frame_tail + data
        parts = []
        captured_at = None
        offset = 0
        while len(data) - offset >= FRAME_HEADER.size:
            adc_time, wall_time, frames = FRAME_HEADER.unpack_from(data, offset)
            end = offset + FRAME_HEADER.size + frames * BYTES_PER_SAMPLE
            if end > len(data):
                break
            parts.append(
                np.frombuffer(
                    data, dtype=np.float32, count=frames, offset=offset + FRAME_HEADER.size
                )
            )
            self._note_adc_time(adc_time, frames)
            if captured_at is None:
                captured_at = wall_time
            offset = end
        self._frame_tail = data[offset:]
        self._note_zeros()
        if not parts:
            return []
        return [(np.concatenate(parts), captured_at)]
