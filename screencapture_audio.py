#!/usr/bin/env python3
"""
ScreenCaptureKit audio capture for macOS 13+

Captures system audio without BlackHole or Multi-Output Device.
Volume control works normally.

Usage:
    from screencapture_audio import ScreenCaptureAudio

    capture = ScreenCaptureAudio()
    capture.start()

    # Get audio chunks
    chunk = capture.get_chunk(duration=3.0)

    capture.stop()
"""

import subprocess
import threading
import os
import sys
import time
import numpy as np
from collections import deque
from typing import Optional, Tuple
from pathlib import Path

from recog.config import ASR_PENDING_BUFFER_CAP_S
from recog.log_utils import log_rate_limited


class ScreenCaptureAudio:
    """
    Captures system audio using macOS ScreenCaptureKit via Swift utility.

    Requires macOS 13+ and Screen Recording permission.
    """

    SAMPLE_RATE = 16000
    BYTES_PER_SAMPLE = 4  # float32

    def __init__(self, sample_rate: int = 16000):
        self.sample_rate = sample_rate
        self.process = None
        self.buffer = []
        self._lock = threading.Lock()
        self._reader_thread = None
        self._stderr_thread = None
        self._stderr_lock = threading.Lock()
        self._stderr_tail = deque(maxlen=200)
        self.running = False
        self.name = "screencapture"
        # --- capture-время + буферизация при недоступном ASR -----------------
        # Дизайн: docs/plans/2026-06-02-asr-fallback-buffering-design.md §2.
        # Тот же контракт, что у SingleDeviceCapture (recog/live.py).
        self._buffer_start_ts: Optional[float] = None
        self._buffered_samples = 0
        self._overlap_samples = int(0.5 * sample_rate)
        self._max_buffered_samples = int(ASR_PENDING_BUFFER_CAP_S * sample_rate)

        # Find the Swift utility
        self._utility_path = self._find_utility()

    def _find_utility(self) -> str:
        """Find the sck_audio_capture utility."""
        # Check in same directory as this script
        script_dir = Path(__file__).parent
        utility = script_dir / "sck_audio_capture"
        source = script_dir / "sck_audio_capture.swift"

        def compile_utility():
            print(f"[ScreenCaptureAudio] Compiling {source}...", file=sys.stderr)
            result = subprocess.run([
                "swiftc", "-o", str(utility), str(source),
                "-framework", "ScreenCaptureKit",
                "-framework", "CoreMedia",
                "-framework", "AVFoundation"
            ], capture_output=True, text=True)

            if result.returncode == 0 and utility.exists():
                return str(utility)

            raise RuntimeError(f"Failed to compile utility: {result.stderr}")

        if utility.exists():
            # Rebuild if source is newer (repo may ship an old binary)
            if source.exists():
                try:
                    if source.stat().st_mtime > utility.stat().st_mtime:
                        return compile_utility()
                except OSError:
                    # If stat fails for any reason, try compiling
                    return compile_utility()
            return str(utility)

        # Compile from source if binary is missing
        if source.exists():
            return compile_utility()

        raise FileNotFoundError(
            f"sck_audio_capture utility not found. "
            f"Expected at: {utility}"
        )

    def _reader_loop(self):
        """Read audio data from subprocess stdout with auto-restart."""
        bytes_per_chunk = self.sample_rate * self.BYTES_PER_SAMPLE // 10  # 100ms chunks
        restart_count = 0
        max_restarts = 5

        while self.running:
            # Check if process is alive
            if not self.process or self.process.poll() is not None:
                if restart_count >= max_restarts:
                    print(f"[ScreenCaptureAudio] Max restarts ({max_restarts}) reached, giving up", file=sys.stderr)
                    break

                # Process died, try to restart
                print(f"[ScreenCaptureAudio] Process died, restarting... (attempt {restart_count + 1})", file=sys.stderr)
                try:
                    self._restart_process()
                    restart_count += 1
                    # NB: модульный `import time` (строка 24); локальный импорт
                    # здесь делал `time` локальной переменной всей функции и
                    # ломал capture-timestamp код выше по телу цикла.
                    time.sleep(1)  # Wait for process to stabilize
                    continue
                except Exception as e:
                    print(f"[ScreenCaptureAudio] Restart failed: {e}", file=sys.stderr)
                    break

            try:
                data = self.process.stdout.read(bytes_per_chunk)
                if data:
                    # Convert bytes to float32 numpy array
                    samples = np.frombuffer(data, dtype=np.float32)
                    captured_at = time.time() - len(samples) / self.sample_rate
                    with self._lock:
                        dropped = self._append_locked(samples, captured_at)
                    if dropped:
                        log_rate_limited(
                            f"capture-overflow-{self.name}",
                            f"[ScreenCaptureAudio:{self.name}] ASR unavailable too long — "
                            f"dropped {dropped / self.sample_rate:.1f}s of oldest audio",
                        )
                    # Reset restart count on successful read
                    restart_count = 0
            except Exception as e:
                if self.running:
                    print(f"[ScreenCaptureAudio] Reader error: {e}", file=sys.stderr)
                # Don't break, let the loop try to restart

    def _start_stderr_drain_thread(self):
        """Drain stderr to prevent the subprocess from blocking on a full stderr pipe."""
        if not self.process or self.process.stderr is None:
            return

        with self._stderr_lock:
            self._stderr_tail.clear()

        def _drain(pipe):
            try:
                for line in iter(pipe.readline, b""):
                    with self._stderr_lock:
                        self._stderr_tail.append(line.decode(errors="ignore"))
            except Exception:
                pass

        self._stderr_thread = threading.Thread(target=_drain, args=(self.process.stderr,), daemon=True)
        self._stderr_thread.start()

    def _restart_process(self):
        """Restart the audio capture subprocess."""
        # Clean up old process
        if self.process:
            try:
                self.process.terminate()
                self.process.wait(timeout=1.0)
            except Exception:
                try:
                    self.process.kill()
                except Exception:
                    pass

        # Start new process
        self.process = subprocess.Popen(
            [self._utility_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0
        )

        # Wait for startup
        import time
        time.sleep(0.5)

        if self.process.poll() is not None:
            stderr = self.process.stderr.read().decode()
            raise RuntimeError(f"Audio capture failed to restart: {stderr}")

        self._start_stderr_drain_thread()
        print(f"[ScreenCaptureAudio] Restarted (PID: {self.process.pid})", file=sys.stderr)

    def start(self):
        """Start capturing system audio."""
        if self.running:
            return

        # Start the Swift utility
        self.process = subprocess.Popen(
            [self._utility_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0
        )

        self.running = True
        self.buffer = []

        # Start reader thread
        self._reader_thread = threading.Thread(target=self._reader_loop, daemon=True)
        self._reader_thread.start()

        # Wait a bit for startup
        import time
        time.sleep(0.5)

        # Check if process is still running
        if self.process.poll() is not None:
            stderr = self.process.stderr.read().decode()
            raise RuntimeError(f"Audio capture failed to start: {stderr}")

        self._start_stderr_drain_thread()
        print(f"[ScreenCaptureAudio] Started (PID: {self.process.pid})", file=sys.stderr)

    def stop(self):
        """Stop capturing."""
        if not self.running:
            return

        self.running = False

        if self.process:
            self.process.terminate()
            try:
                self.process.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                self.process.kill()
            self.process = None

        if self._reader_thread:
            self._reader_thread.join(timeout=1.0)
            self._reader_thread = None

        print("[ScreenCaptureAudio] Stopped", file=sys.stderr)

    def clear_buffer(self):
        """Discard all buffered audio (used on meeting boundary)."""
        with self._lock:
            self.buffer = []
            self._buffer_start_ts = None
            self._buffered_samples = 0

    def _append_locked(self, data: np.ndarray, captured_at: float) -> int:
        """Добавляет data в буфер; возвращает число сэмплов, отброшенных по капу.

        Вызывается с уже взятым _lock."""
        if not self.buffer:
            self._buffer_start_ts = captured_at
        self.buffer.append(data)
        self._buffered_samples += len(data)
        dropped = 0
        while self._buffered_samples > self._max_buffered_samples and self.buffer:
            oldest = self.buffer.pop(0)
            self._buffered_samples -= len(oldest)
            if self._buffer_start_ts is not None:
                self._buffer_start_ts += len(oldest) / self.sample_rate
            dropped += len(oldest)
        return dropped

    def _append_locked_for_tests(self, data: np.ndarray, captured_at: float) -> int:
        """Тестовый шов: _append_locked под собственным локом."""
        with self._lock:
            return self._append_locked(data, captured_at)

    def get_chunk(self, duration: float) -> Optional[Tuple[np.ndarray, float]]:
        """Get (audio chunk, epoch-время захвата первого сэмпла)."""
        if not self.running:
            return None

        samples_needed = int(duration * self.sample_rate)

        with self._lock:
            if not self.buffer:
                return None

            # Concatenate all buffered audio
            audio = np.concatenate(self.buffer)

            if len(audio) < samples_needed:
                return None

            # Extract chunk
            chunk = audio[:samples_needed]
            chunk_start_ts = (
                self._buffer_start_ts
                if self._buffer_start_ts is not None
                else time.time() - duration
            )

            # Keep overlap (0.5 seconds)
            overlap_start = max(0, samples_needed - self._overlap_samples)
            self.buffer = [audio[overlap_start:]]
            self._buffered_samples = len(audio) - overlap_start
            self._buffer_start_ts = chunk_start_ts + overlap_start / self.sample_rate

            return chunk.astype(np.float32), chunk_start_ts

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *args):
        self.stop()


def test_capture():
    """Test audio capture."""
    import time

    print("Testing ScreenCaptureKit audio capture...")
    print("Make sure Screen Recording is allowed in System Settings > Privacy & Security")
    print()

    try:
        capture = ScreenCaptureAudio()
        capture.start()

        print("Capturing... play some audio!")
        time.sleep(4)

        chunk = capture.get_chunk(3.0)
        if chunk is not None:
            print(f"Got chunk: {len(chunk)} samples, {len(chunk)/16000:.2f}s")
            print(f"Audio level (RMS): {np.sqrt(np.mean(chunk**2)):.6f}")
            print(f"Max amplitude: {np.abs(chunk).max():.6f}")
        else:
            print("No audio captured (buffer too small)")

        capture.stop()
        print("Done!")

    except Exception as e:
        print(f"Error: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    test_capture()
