"""Тесты буферизации аудио при недоступном ASR.

Дизайн: docs/plans/2026-06-02-asr-fallback-buffering-design.md §2, §4.
Отличие от дизайна: retry-слот живёт в SourceState.pending_chunk (consumer),
а не в capture.requeue() — см. коммит-историю.

Покрывает оба capture-класса с одинаковым контрактом get_chunk:
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
    """Общие поля буферизации (capture-время, кап)."""
    cap.buffer = []
    cap._lock = threading.Lock()
    cap._buffer_start_ts = None
    cap._buffered_samples = 0
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
# Capture-классы: capture-время, кап (оба класса)
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

    def test_unavailable_exception_holds_chunk_in_pending_slot(self):
        """given: ASR стал недоступен между get_chunk и transcribe
        when: _transcribe_direct кидает ASRTemporarilyUnavailable
        then: чанк сохраняется в state.pending_chunk с исходным временем"""
        t = _make_transcriber_stub(asr_available=True)
        chunk = np.ones(48000, dtype=np.float32) * 0.1
        capture = Mock()
        capture.get_chunk.return_value = (chunk, 1000.0)
        t._transcribe_direct = Mock(side_effect=ASRTemporarilyUnavailable("loading"))
        state = SourceState()

        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)

        assert state.pending_chunk is not None
        pend_chunk, pend_ts, pend_gen = state.pending_chunk
        np.testing.assert_array_equal(pend_chunk, chunk)
        assert pend_ts == pytest.approx(1000.0)
        assert pend_gen == t._meeting_generation

    def test_pending_chunk_retried_without_rereading_or_rewriting_wav(self):
        """given: первая попытка записала чанк в WAV и упала (pending_chunk)
        when: следующий _process_chunk с доступным ASR
        then: capture.get_chunk не зовётся, WAV не пишется второй раз,
              сегмент получает исходное capture-время"""
        t = _make_transcriber_stub(asr_available=True)
        chunk = np.ones(48000, dtype=np.float32) * 0.1
        capture = Mock()
        capture.get_chunk.return_value = (chunk, 1000.0)
        wav_writer = Mock()
        wav_writer.get_offset_ms.return_value = 0
        t._wav_writer_mic = wav_writer
        t._check_silence_split = Mock()
        t._handle_mic_chunk = Mock()
        state = SourceState()

        # Попытка 1: WAV записан, ASR упал -> pending_chunk
        t._transcribe_direct = Mock(side_effect=ASRTemporarilyUnavailable("loading"))
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert wav_writer.write_chunk.call_count == 1
        assert state.pending_chunk is not None

        # Попытка 2: ASR ожил — ретрай из слота, без повторного чтения/записи
        t._transcribe_direct = Mock(return_value="привет мир")
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)

        capture.get_chunk.assert_called_once()  # только попытка 1
        assert wav_writer.write_chunk.call_count == 1  # не вырос
        assert state.pending_chunk is None
        t._handle_mic_chunk.assert_called_once()
        chunk_time_arg = t._handle_mic_chunk.call_args[0][2]
        assert chunk_time_arg == pytest.approx(1000.0 + 3.0, abs=0.1)

    def test_pending_chunk_from_old_meeting_generation_dropped(self):
        """given: pending_chunk из прошлой встречи (generation сменился)
        when: _process_chunk
        then: чанк выброшен, обработка продолжается с capture-буфера"""
        t = _make_transcriber_stub(asr_available=True)
        capture = Mock()
        capture.get_chunk.return_value = None
        t._transcribe_direct = Mock()
        state = SourceState()
        state.pending_chunk = (np.ones(48000, dtype=np.float32), 1000.0, 0)
        t._meeting_generation = 1  # встреча сменилась

        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)

        assert state.pending_chunk is None
        capture.get_chunk.assert_called_once()
        t._transcribe_direct.assert_not_called()

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
