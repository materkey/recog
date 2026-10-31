"""Microphone capture process: frames with their capture time on stdout.

`sounddevice` calls back about a thousand times a second, and each call needs
the interpreter lock. In the live process any busy Python thread makes those
calls late, and PortAudio then loses samples without reporting a status (a
callback-time recording in the service was 0.41% short of this process's take
of the same sound). Here nothing else competes for the lock.

Every way a stream stops capturing ends in an exit, and the parent's respawn
(`SubprocessPCMCapture`) opens a new stream on a device list read afresh.

Run by file path with `-I`: importing `recog` would bring its heavy imports here.
"""

import argparse
import os
import struct
import sys
import threading
import time
from collections import deque
from typing import BinaryIO, Optional

# ADC time of the first sample (PortAudio's, on the host uptime clock), wall
# clock of the same sample, frame count; float32 mono samples follow.
FRAME_HEADER = struct.Struct("<ddI")
BYTES_PER_SAMPLE = 4

# How often buffered frames go to the pipe. The audio callback never writes
# there itself: a parent that stalls until the pipe is full must not block it.
WRITE_INTERVAL_S = 0.02

# CoreAudio calls back every ~1 ms; a stop that never reached the finished
# callback leaves the stream silent like this.
CALLBACK_STALL_S = 10.0

EXIT_STREAM_LOST = 3
EXIT_DEVICE_NOT_FOUND = 4


def encode_frame(adc_time: float, wall_time: float, samples) -> bytes:
    """One frame of the pipe; `samples` is a 1-D float32 array."""
    return FRAME_HEADER.pack(adc_time, wall_time, len(samples)) + samples.tobytes()


def find_input_device(devices, name: str) -> Optional[int]:
    """Index of the input device called `name`: indices shift between starts."""
    for index, device in enumerate(devices):
        if device["name"] == name and device["max_input_channels"] > 0:
            return index
    return None


def capture_loop(
    sd,
    device_name: Optional[str],
    sample_rate: int,
    out: BinaryIO,
    stall_s: float = CALLBACK_STALL_S,
) -> int:
    """Captures until the stream stops or the parent goes; returns the exit code."""
    if device_name is None:
        device = sd.query_devices(kind="input")["index"]
    else:
        device = find_input_device(sd.query_devices(), device_name)
        if device is None:
            print(f"Input device {device_name!r} not found", file=sys.stderr, flush=True)
            return EXIT_DEVICE_NOT_FOUND
    channels = min(sd.query_devices(device)["max_input_channels"], 2)

    frames_out: deque = deque()
    finished = threading.Event()
    last_callback = [time.monotonic()]

    def on_audio(indata, frames, time_info, status):
        last_callback[0] = time.monotonic()
        if status:
            print(f"Audio status: {status}", file=sys.stderr, flush=True)
        mono = indata[:, 0] if channels == 1 else indata.mean(axis=1)
        frames_out.append(
            encode_frame(time_info.inputBufferAdcTime, time.time() - frames / sample_rate, mono)
        )

    with sd.InputStream(
        device=device,
        channels=channels,
        samplerate=sample_rate,
        dtype="float32",
        callback=on_audio,
        finished_callback=finished.set,
    ) as stream:
        while True:
            time.sleep(WRITE_INTERVAL_S)
            try:
                if frames_out:
                    out.write(b"".join(frames_out.popleft() for _ in range(len(frames_out))))
                    out.flush()
            except BrokenPipeError:
                return 0
            if finished.is_set() or not stream.active:
                print(
                    "The input stream was stopped by the audio system", file=sys.stderr, flush=True
                )
                return EXIT_STREAM_LOST
            gap = time.monotonic() - last_callback[0]
            if gap >= stall_s:
                print(f"No audio callbacks for {gap:.0f}s", file=sys.stderr, flush=True)
                return EXIT_STREAM_LOST


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--device", default=None, help="input device name; default input without it"
    )
    parser.add_argument("--sample-rate", type=int, required=True)
    args = parser.parse_args()

    # stdout carries binary frames: anything a library prints goes to stderr.
    out = os.fdopen(os.dup(sys.stdout.fileno()), "wb")
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())

    import sounddevice as sd

    return capture_loop(sd, args.device, args.sample_rate, out)


if __name__ == "__main__":
    sys.exit(main())
