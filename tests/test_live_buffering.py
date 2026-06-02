"""Тесты буферизации аудио при недоступном ASR.

Дизайн: docs/plans/2026-06-02-asr-fallback-buffering-design.md §2, §4.

Покрывает оба capture-класса с одинаковым контрактом get_chunk/requeue:
- SingleDeviceCapture (recog/live.py) — микрофон/sounddevice
- ScreenCaptureAudio (screencapture_audio.py) — системный звук/ScreenCaptureKit

Оба конструируются через __new__ без внешних зависимостей (sounddevice /
swift-бинарь) — паттерн из tests/test_asr.py::_make_mocked_asr.
"""
import sys
import threading
import time
from pathlib import Path
from unittest.mock import Mock

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.live import LiveTranscriber, SingleDeviceCapture, SourceState
from recog.remote_asr import ASRTemporarilyUnavailable
from screencapture_audio import ScreenCaptureAudio


SR = 16000


def _init_buffer_fields(cap, max_buffer_s: float = 10.0):
    """Общие поля буферизации (capture-время, requeue, кап)."""
    cap.buffer = []
    cap._lock = threading.Lock()
    cap._buffer_start_ts = None
    cap._buffered_samples = 0
    cap._requeued = []
    cap._overlap_samples = int(0.5 * SR)
    cap._max_buffered_samples = int(max_buffer_s * SR)


def _make_single_device_capture() -> SingleDeviceCapture:
    """SingleDeviceCapture без sounddevice."""
    cap = SingleDeviceCapture.__new__(SingleDeviceCapture)
    cap.sd = None
    cap.sample_rate = SR
    cap.device = None
    cap.name = "test-mic"
    cap.running = True
    cap.stream = None
    _init_buffer_fields(cap)
    return cap


def _make_screencapture() -> ScreenCaptureAudio:
    """ScreenCaptureAudio без swift-сабпроцесса."""
    cap = ScreenCaptureAudio.__new__(ScreenCaptureAudio)
    cap.sample_rate = SR
    cap.process = None
    cap.running = True
    cap.name = "test-screencapture"
    _init_buffer_fields(cap)
    return cap


@pytest.fixture(params=["single_device", "screencapture"])
def capture(request):
    if request.param == "single_device":
        return _make_single_device_capture()
    return _make_screencapture()


def _feed(cap, seconds: float, captured_at: float):
    """Эмулирует продюсера: добавляет seconds аудио, захваченного с captured_at.

    SingleDeviceCapture получает 2-D кадры (sounddevice), ScreenCaptureAudio —
    1-D (np.frombuffer), как в реальных продюсерах."""
    n = int(seconds * SR)
    if isinstance(cap, SingleDeviceCapture):
        data = np.ones((n, 1), dtype=np.float32) * 0.1
    else:
        data = np.ones(n, dtype=np.float32) * 0.1
    cap._append_locked_for_tests(data, captured_at)


# =============================================================================
# Capture-классы: capture-время, requeue, кап (оба класса)
# =============================================================================


class TestCaptureTimestamps:
    def test_get_chunk_returns_capture_start_time(self, capture):
        """given: 3 s аудио, захваченного в t=1000.0
        when: get_chunk(3.0)
        then: возвращается (chunk, 1000.0)"""
        _feed(capture, 3.0, captured_at=1000.0)
        result = capture.get_chunk(3.0)
        assert result is not None
        chunk, ts = result
        assert len(chunk) == 3 * SR
        assert ts == pytest.approx(1000.0)

    def test_consecutive_chunks_advance_capture_time_minus_overlap(self, capture):
        """given: 6 s аудио с t=1000.0
        when: два get_chunk(3.0) подряд
        then: второй чанк начинается на 2.5 s позже первого (3 s - 0.5 s overlap)"""
        _feed(capture, 6.0, captured_at=1000.0)
        _, ts1 = capture.get_chunk(3.0)
        _, ts2 = capture.get_chunk(3.0)
        assert ts1 == pytest.approx(1000.0)
        assert ts2 == pytest.approx(1002.5)

    def test_not_enough_audio_returns_none(self, capture):
        _feed(capture, 1.0, captured_at=1000.0)
        assert capture.get_chunk(3.0) is None


class TestRequeue:
    def test_requeued_chunk_returned_first_with_original_timestamp(self, capture):
        """given: чанк вычитан (ASR оказался недоступен) и возвращён requeue
        when: следующий get_chunk
        then: возвращается тот же чанк с исходным capture-временем"""
        _feed(capture, 6.0, captured_at=1000.0)
        chunk1, ts1 = capture.get_chunk(3.0)

        capture.requeue(chunk1, ts1)
        chunk_again, ts_again = capture.get_chunk(3.0)

        assert ts_again == pytest.approx(ts1)
        np.testing.assert_array_equal(chunk_again, chunk1)

    def test_after_requeued_chunk_consumed_buffer_continues_in_order(self, capture):
        """given: requeue + остаток в буфере
        when: get_chunk дважды
        then: первый — requeued (t=1000.0), второй — продолжение буфера (t=1002.5)"""
        _feed(capture, 6.0, captured_at=1000.0)
        chunk1, ts1 = capture.get_chunk(3.0)
        capture.requeue(chunk1, ts1)

        _, ts_first = capture.get_chunk(3.0)
        _, ts_second = capture.get_chunk(3.0)
        assert ts_first == pytest.approx(1000.0)
        assert ts_second == pytest.approx(1002.5)

    def test_clear_buffer_drops_requeued_too(self, capture):
        _feed(capture, 3.0, captured_at=1000.0)
        chunk, ts = capture.get_chunk(3.0)
        capture.requeue(chunk, ts)
        capture.clear_buffer()
        assert capture.get_chunk(3.0) is None


class TestBufferCap:
    def test_oldest_audio_dropped_beyond_cap(self, capture):
        """given: кап 10 s
        when: накормили 14 s аудио
        then: буфер держит <= 10 s; capture-время первого сэмпла сдвинуто вперёд"""
        for i in range(14):
            _feed(capture, 1.0, captured_at=1000.0 + i)
        assert capture._buffered_samples <= 10 * SR
        # Старейшие ~4 s отброшены -> первый сэмпл захвачен не раньше t=1004.0
        result = capture.get_chunk(3.0)
        assert result is not None
        _, ts = result
        assert ts >= 1004.0 - 0.1


# =============================================================================
# _process_chunk: гейт доступности ASR + capture-метки + дедуп-окно
# =============================================================================


def _make_transcriber_stub(asr_available: bool = True) -> Mock:
    """Минимальный LiveTranscriber-образный объект для _process_chunk."""
    t = Mock()
    t._meeting_generation = 0
    t._wav_lock = threading.Lock()
    t._state_lock = threading.Lock()
    t._wav_writer_mic = None
    t._wav_writer_system = None
    t._per_speaker_ingest = None
    t._ktalk_bridge = None
    t._meeting_storage = None
    t._refine_worker = None
    t.use_ktalk = False
    t.use_pcm_transport = False
    t.chunk_duration = 3.0
    t.transcript_lines = []
    t.mic_speaker_name = "Me"
    t._asr_available = lambda: asr_available
    return t


class TestAsrAvailabilityGate:
    def test_unavailable_asr_does_not_consume_buffer(self):
        """given: ASR недоступен (FallbackASR.available == False)
        when: _process_chunk
        then: get_chunk НЕ вызывается — аудио остаётся в буфере"""
        t = _make_transcriber_stub(asr_available=False)
        capture = Mock()
        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=True)
        capture.get_chunk.assert_not_called()

    def test_unavailable_exception_requeues_chunk(self):
        """given: ASR стал недоступен между get_chunk и transcribe
        when: _transcribe_direct кидает ASRTemporarilyUnavailable
        then: чанк возвращается в capture.requeue с исходным временем"""
        t = _make_transcriber_stub(asr_available=True)
        chunk = np.ones(48000, dtype=np.float32) * 0.1
        capture = Mock()
        capture.get_chunk.return_value = (chunk, 1000.0)
        t._transcribe_direct = Mock(side_effect=ASRTemporarilyUnavailable("loading"))

        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=True)

        capture.requeue.assert_called_once()
        requeued_chunk, requeued_ts = capture.requeue.call_args[0]
        np.testing.assert_array_equal(requeued_chunk, chunk)
        assert requeued_ts == pytest.approx(1000.0)

    def test_segment_timestamp_uses_capture_time_not_processing_time(self):
        """given: чанк, захваченный час назад (бэклог после сбоя)
        when: _process_chunk обрабатывает его сейчас
        then: _handle_mic_chunk получает chunk_time из capture-времени"""
        t = _make_transcriber_stub(asr_available=True)
        capture_ts = time.time() - 3600.0
        capture = Mock()
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, capture_ts)
        t._transcribe_direct = Mock(return_value="привет мир")
        t._check_silence_split = Mock()
        t._handle_mic_chunk = Mock()

        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=True)

        t._handle_mic_chunk.assert_called_once()
        # сигнатура: (text, timestamp, chunk_time, chunk_gen, state)
        chunk_time_arg = t._handle_mic_chunk.call_args[0][2]
        expected_end = capture_ts + 3.0
        assert chunk_time_arg == pytest.approx(expected_end, abs=0.1)


class TestDedupWindow:
    def test_adjacent_chunks_still_deduplicated(self):
        """given: сегмент записан, следующий чанк начинается сразу (overlap)
        when: тот же текст
        then: dedup срабатывает — второй сегмент не пишется"""
        t = _make_transcriber_stub(asr_available=True)
        state = SourceState()
        capture = Mock()
        t._transcribe_direct = Mock(return_value="раз два три")
        t._check_silence_split = Mock()
        t._handle_mic_chunk = Mock()

        # Чанк 1: captured at 1000.0, конец 1003.0
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 1

        # Чанк 2: captured at 1002.5 (через overlap) — тот же текст -> dedup
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1002.5)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 1  # не вырос

    def test_repeated_phrase_after_gap_not_deduplicated(self):
        """given: сегмент записан; пользователь молчал > DEDUP_MAX_GAP_S
        when: та же фраза снова (capture-время через 30 s)
        then: dedup НЕ срабатывает — сегмент пишется

        Это именно тот баг, что съел речь в инциденте 2026-06-02: повторённая
        через минуты тестовая фраза полностью совпала с last_words."""
        t = _make_transcriber_stub(asr_available=True)
        state = SourceState()
        capture = Mock()
        t._transcribe_direct = Mock(return_value="раз два три")
        t._check_silence_split = Mock()
        t._handle_mic_chunk = Mock()

        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 1

        # Та же фраза через 30 секунд тишины
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1033.0)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 2  # записан!
