"""Тесты контракта BufferedAudioCapture.get_chunk (recog/audio_capture.py).

Общий буфер вырос из двух копий (`SingleDeviceCapture` / `ScreenCaptureAudio`)
с расходящимися `get_chunk`. Здесь фиксируется сведённый контракт: 1-D моно
float32 и ранний выход при остановленном захвате. Кейсы capture-времени и капа
покрыты параметризованно по обоим подклассам в tests/test_live_buffering.py.
"""
import sys
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.audio_capture import BufferedAudioCapture


SR = 16000


def _make_capture(max_buffer_s: float = 10.0) -> BufferedAudioCapture:
    cap = BufferedAudioCapture(SR, "test", max_buffer_s=max_buffer_s)
    cap.running = True
    return cap


class TestGetChunkContract:
    def test_stereo_frames_are_downmixed_to_flat_mono_float32(self):
        """given: продюсер отдаёт кадры (n, 2) — 2-канальное устройство
        when: get_chunk(3.0)
        then: 1-D float32 длины samples_needed со средним по каналам

        Без свёртки и .flatten() чанк уходит в ASR как (n, 2)/(n, 1): forward
        падает, исключение глотается — источник молча замолкает."""
        n = 3 * SR
        frames = np.stack(
            [np.full(n, 0.2, dtype=np.float32), np.full(n, 0.4, dtype=np.float32)],
            axis=1,
        )
        cap = _make_capture()
        cap._append_locked_for_tests(frames, 1000.0)

        chunk, ts = cap.get_chunk(3.0)

        assert chunk.ndim == 1
        assert chunk.dtype == np.float32
        assert len(chunk) == 3 * SR
        assert chunk == pytest.approx(np.full(3 * SR, 0.3, dtype=np.float32), abs=1e-6)
        assert ts == pytest.approx(1000.0)

    def test_single_channel_frames_are_flattened(self):
        """given: кадры (n, 1) — sounddevice всегда 2-D, даже на моно-устройстве
        when: get_chunk(3.0)
        then: результат 1-D, а не (n, 1)"""
        cap = _make_capture()
        cap._append_locked_for_tests(np.full((3 * SR, 1), 0.1, dtype=np.float32), 1000.0)

        chunk, _ = cap.get_chunk(3.0)

        assert chunk.shape == (3 * SR,)
        assert chunk.dtype == np.float32

    def test_flat_frames_pass_through_as_mono_float32(self):
        """given: 1-D кадры (np.frombuffer из stdout сабпроцесса)
        when: get_chunk(3.0)
        then: контракт тот же — 1-D float32"""
        cap = _make_capture()
        cap._append_locked_for_tests(np.full(3 * SR, 0.1, dtype=np.float32), 1000.0)

        chunk, _ = cap.get_chunk(3.0)

        assert chunk.shape == (3 * SR,)
        assert chunk.dtype == np.float32

    def test_stopped_capture_returns_none_without_consuming_buffer(self):
        """given: захват остановлен, но в буфере ещё есть аудио
        when: get_chunk
        then: None, буфер не тронут"""
        cap = _make_capture()
        cap._append_locked_for_tests(np.full(3 * SR, 0.1, dtype=np.float32), 1000.0)
        cap.running = False

        assert cap.get_chunk(3.0) is None
        assert cap._buffered_samples == 3 * SR

    def test_clear_buffer_resets_capture_time(self):
        cap = _make_capture()
        cap._append_locked_for_tests(np.full(3 * SR, 0.1, dtype=np.float32), 1000.0)

        cap.clear_buffer()

        assert cap.buffer == []
        assert cap._buffered_samples == 0
        assert cap._buffer_start_ts is None
        assert cap.get_chunk(3.0) is None
