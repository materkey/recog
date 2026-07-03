"""Тесты 24-часового retention аудио и записи WAV в live-пайплайне.

Фикс: "Audio error: No audio file available for this meeting" — запись аудио
никогда не включалась (save_audio не пробрасывался через live_pipeline), а
cleanup_old_audio работал по meta.created_at с 7-дневным retention и
вызывался только на старте сессии.

Новая семантика:
- cleanup_old_audio: retention 24 ч по mtime файла (активно пишущийся файл
  всегда свежий — текущая встреча защищена), дешёвый stat-скан без парсинга
  meta.json для директорий без аудио.
- IncrementalWavWriter: жёсткий cap размера data-чанка (WAV хранит размеры
  как 32-bit — файл больше 4 ГиБ ломает формат).
- _process_chunk: тишина (rms < VAD_RMS_THRESHOLD) не пишется в WAV, кроме
  pcm-primary режима, где оффсеты PCM-сегментов требуют непрерывной timeline.
"""
import json
import os
import sys
import threading
import time
from pathlib import Path
from unittest.mock import Mock

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "frontend"))

from meeting_storage import MeetingStorage
from recog.config import SILENCE_SPLIT_TIMEOUT
from recog.live import IncrementalWavWriter, LiveTranscriber, SourceState

SR = 16000


# =============================================================================
# cleanup_old_audio: 24h retention по mtime
# =============================================================================


def _make_meeting_dir(base: Path, name: str, meta: dict | None = None) -> Path:
    d = base / name
    d.mkdir()
    if meta is not None:
        (d / "meta.json").write_text(json.dumps(meta), encoding="utf-8")
    return d


def _touch_wav(meeting_dir: Path, track: str, age_hours: float) -> Path:
    wav = meeting_dir / f"audio_{track}.wav"
    wav.write_bytes(b"RIFF" + b"\x00" * 40)
    ts = time.time() - age_hours * 3600
    os.utime(wav, (ts, ts))
    return wav


class TestCleanupOldAudio:
    def test_audio_older_than_24h_deleted_and_meta_nulled(self, tmp_path):
        """given: встреча с mic+system аудио, mtime 25 часов назад
        when: cleanup_old_audio()
        then: оба файла удалены, meta-пути обнулены, audio_deleted_at выставлен"""
        storage = MeetingStorage(base_path=tmp_path)
        d = _make_meeting_dir(
            tmp_path,
            "2026-07-01_120000_aaaa1111",
            meta={
                "audio_mic_path": str(tmp_path / "2026-07-01_120000_aaaa1111" / "audio_mic.wav"),
                "audio_system_path": str(tmp_path / "2026-07-01_120000_aaaa1111" / "audio_system.wav"),
            },
        )
        mic = _touch_wav(d, "mic", age_hours=25)
        system = _touch_wav(d, "system", age_hours=25)

        deleted = storage.cleanup_old_audio()

        assert deleted == 2
        assert not mic.exists()
        assert not system.exists()
        meta = json.loads((d / "meta.json").read_text(encoding="utf-8"))
        assert meta["audio_mic_path"] is None
        assert meta["audio_system_path"] is None
        assert meta.get("audio_deleted_at")

    def test_fresh_audio_kept(self, tmp_path):
        """given: аудио с mtime 1 час назад
        when: cleanup_old_audio()
        then: файл не тронут"""
        storage = MeetingStorage(base_path=tmp_path)
        d = _make_meeting_dir(tmp_path, "2026-07-02_120000_bbbb2222", meta={})
        wav = _touch_wav(d, "system", age_hours=1)

        deleted = storage.cleanup_old_audio()

        assert deleted == 0
        assert wav.exists()

    def test_mixed_ages_only_stale_track_deleted(self, tmp_path):
        """given: mic старый (25h), system свежий (1h)
        when: cleanup_old_audio()
        then: удалён только mic; только его meta-ключ обнулён"""
        storage = MeetingStorage(base_path=tmp_path)
        d = _make_meeting_dir(
            tmp_path,
            "2026-07-01_130000_cccc3333",
            meta={"audio_mic_path": "x", "audio_system_path": "y"},
        )
        mic = _touch_wav(d, "mic", age_hours=25)
        system = _touch_wav(d, "system", age_hours=1)

        deleted = storage.cleanup_old_audio()

        assert deleted == 1
        assert not mic.exists()
        assert system.exists()
        meta = json.loads((d / "meta.json").read_text(encoding="utf-8"))
        assert meta["audio_mic_path"] is None
        assert meta["audio_system_path"] == "y"

    def test_dirs_without_audio_do_not_parse_meta(self, tmp_path):
        """given: 3 директории без аудиофайлов, meta.json с невалидным JSON
        when: cleanup_old_audio()
        then: 0 удалений и ни одной ошибки — meta не парсится (stat-only скан)"""
        storage = MeetingStorage(base_path=tmp_path)
        for i in range(3):
            d = tmp_path / f"2026-07-02_1200{i:02d}_dddd444{i}"
            d.mkdir()
            (d / "meta.json").write_text("{not-json", encoding="utf-8")

        deleted = storage.cleanup_old_audio()

        assert deleted == 0

    def test_old_audio_without_meta_still_deleted(self, tmp_path):
        """given: старый WAV в директории без meta.json
        when: cleanup_old_audio()
        then: файл удалён (meta — только бухгалтерия, не условие)"""
        storage = MeetingStorage(base_path=tmp_path)
        d = _make_meeting_dir(tmp_path, "2026-07-01_140000_eeee5555", meta=None)
        wav = _touch_wav(d, "system", age_hours=30)

        deleted = storage.cleanup_old_audio()

        assert deleted == 1
        assert not wav.exists()

    def test_retention_override(self, tmp_path):
        """given: аудио возрастом 2 часа
        when: cleanup_old_audio(retention_hours=1)
        then: удалено"""
        storage = MeetingStorage(base_path=tmp_path)
        d = _make_meeting_dir(tmp_path, "2026-07-02_100000_ffff6666", meta={})
        wav = _touch_wav(d, "mic", age_hours=2)

        deleted = storage.cleanup_old_audio(retention_hours=1)

        assert deleted == 1
        assert not wav.exists()


# =============================================================================
# IncrementalWavWriter: cap размера данных (WAV = 32-bit размеры)
# =============================================================================


class TestWavWriterCap:
    def test_write_stops_at_max_data_bytes(self, tmp_path):
        """given: writer с cap на ~1 секунду данных
        when: пишем 3 чанка по 1 секунде
        then: samples_written не превышает cap, файл остаётся валидным"""
        writer = IncrementalWavWriter(
            tmp_path / "audio.wav", SR, max_data_bytes=SR * 2  # 1s int16
        )
        writer.open()
        chunk = np.ones(SR, dtype=np.float32) * 0.1
        for _ in range(3):
            writer.write_chunk(chunk)
        writer.finalize()

        data_size = (tmp_path / "audio.wav").stat().st_size - 44
        assert data_size <= SR * 2

    def test_no_cap_by_default_for_normal_sizes(self, tmp_path):
        """given: writer с дефолтным cap
        when: пишем 2 секунды
        then: всё записано"""
        writer = IncrementalWavWriter(tmp_path / "audio.wav", SR)
        writer.open()
        chunk = np.ones(SR, dtype=np.float32) * 0.1
        writer.write_chunk(chunk)  # первый чанк — без среза overlap
        writer.finalize()

        data_size = (tmp_path / "audio.wav").stat().st_size - 44
        assert data_size == SR * 2


class TestWavWriterSkippedReturn:
    """write_chunk возвращает число пропущенных head-сэмплов (или None),
    чтобы _process_chunk мог привязать wall-clock захвата к позиции в WAV
    (якорь _system_wav_anchor)."""

    def test_first_chunk_returns_zero_skipped(self, tmp_path):
        """given: свежий writer
        when: первый write_chunk
        then: overlap не срезается — возвращает 0"""
        writer = IncrementalWavWriter(tmp_path / "audio.wav", SR)
        writer.open()
        assert writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1) == 0

    def test_subsequent_chunk_returns_overlap_samples(self, tmp_path):
        """given: writer после первого чанка
        when: второй write_chunk
        then: срезается overlap — возвращает _overlap_samples"""
        writer = IncrementalWavWriter(tmp_path / "audio.wav", SR)
        writer.open()
        writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1)
        skipped = writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1)
        assert skipped == writer._overlap_samples

    def test_cap_hit_returns_none(self, tmp_path):
        """given: writer с cap на ~1 секунду
        when: чанк, переполняющий cap
        then: ничего не записано — None"""
        writer = IncrementalWavWriter(
            tmp_path / "audio.wav", SR, max_data_bytes=SR * 2
        )
        writer.open()
        writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1)  # заполнил cap
        assert writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1) is None

    def test_chunk_shorter_than_overlap_returns_none(self, tmp_path):
        """given: writer после первого чанка
        when: второй чанк короче overlap (remainder пустой)
        then: None"""
        writer = IncrementalWavWriter(tmp_path / "audio.wav", SR)
        writer.open()
        writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1)
        short = np.ones(writer._overlap_samples - 1, dtype=np.float32) * 0.1
        assert writer.write_chunk(short) is None

    def test_unopened_writer_returns_none(self, tmp_path):
        """given: writer без open()
        when: write_chunk
        then: None"""
        writer = IncrementalWavWriter(tmp_path / "audio.wav", SR)
        assert writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1) is None


# =============================================================================
# _process_chunk: тишина не пишется в WAV (кроме pcm-primary)
# =============================================================================


def _make_transcriber_stub() -> Mock:
    """Минимальный LiveTranscriber-образный объект для _process_chunk
    (паттерн из tests/test_live_buffering.py)."""
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
    t.sample_rate = SR
    t._system_wav_anchor = None
    t.transcript_lines = []
    t.mic_speaker_name = "Me"
    t._asr_available = lambda: True
    return t


def _capture_returning(chunk: np.ndarray) -> Mock:
    capture = Mock()
    capture.get_chunk.return_value = (chunk, 1000.0)
    return capture


class TestWavSilenceSkip:
    def test_silent_chunk_not_written_to_wav(self):
        """given: тихий чанк (rms < VAD_RMS_THRESHOLD), mic writer активен
        when: _process_chunk
        then: write_chunk НЕ вызывается (тишина не занимает диск)"""
        t = _make_transcriber_stub()
        t._wav_writer_mic = Mock()
        t._wav_writer_mic.get_offset_ms.return_value = 0
        silent = np.zeros(3 * SR, dtype=np.float32)

        LiveTranscriber._process_chunk(
            t, _capture_returning(silent), SourceState(), Mock(), is_mic=True
        )

        t._wav_writer_mic.write_chunk.assert_not_called()

    def test_speech_chunk_written_to_wav(self):
        """given: громкий чанк, mic writer активен
        when: _process_chunk
        then: write_chunk вызывается"""
        t = _make_transcriber_stub()
        t._wav_writer_mic = Mock()
        t._wav_writer_mic.get_offset_ms.return_value = 0
        t._transcribe_direct = Mock(return_value="")
        speech = np.ones(3 * SR, dtype=np.float32) * 0.1

        LiveTranscriber._process_chunk(
            t, _capture_returning(speech), SourceState(), Mock(), is_mic=True
        )

        t._wav_writer_mic.write_chunk.assert_called_once()

    def test_silent_chunk_written_when_pcm_primary(self):
        """given: тихий system-чанк, PCM primary (pcm_ever_seen, не degraded)
        when: _process_chunk
        then: write_chunk вызывается — оффсеты PCM-сегментов требуют
        непрерывной timeline в audio_system.wav"""
        t = _make_transcriber_stub()
        t._wav_writer_system = Mock()
        t._wav_writer_system.get_offset_ms.return_value = 0
        t._wav_writer_system.write_chunk.return_value = 0  # skipped samples
        ingest = Mock()
        ingest.pcm_ever_seen = True
        ingest.degraded = False
        t._per_speaker_ingest = ingest
        t._last_pcm_degraded = False
        silent = np.zeros(3 * SR, dtype=np.float32)

        LiveTranscriber._process_chunk(
            t, _capture_returning(silent), SourceState(), Mock(), is_mic=False
        )

        t._wav_writer_system.write_chunk.assert_called_once()


# =============================================================================
# _system_wav_anchor: привязка wall-clock захвата к позиции в WAV
# =============================================================================


def _pcm_primary_stub(tmp_path) -> Mock:
    """Stub с pcm-primary ingest и реальным system-writer в tmp_path."""
    t = _make_transcriber_stub()
    writer = IncrementalWavWriter(tmp_path / "audio_system.wav", SR)
    writer.open()
    t._wav_writer_system = writer
    ingest = Mock()
    ingest.pcm_ever_seen = True
    ingest.degraded = False
    t._per_speaker_ingest = ingest
    t._last_pcm_degraded = False
    return t


class TestSystemWavAnchor:
    def test_process_chunk_sets_anchor_on_first_system_write(self, tmp_path):
        """given: pcm-primary, первый system-чанк с capture_start_ts=1000.0
        when: _process_chunk
        then: якорь = (wall первого сэмпла в ms, WAV offset 0)"""
        t = _pcm_primary_stub(tmp_path)
        capture = Mock()
        capture.get_chunk.return_value = (
            np.ones(3 * SR, dtype=np.float32) * 0.1, 1000.0
        )

        LiveTranscriber._process_chunk(
            t, capture, SourceState(), Mock(), is_mic=False
        )

        assert t._system_wav_anchor == (1000.0 * 1000.0, 0)

    def test_anchor_advances_by_overlap_on_second_chunk(self, tmp_path):
        """given: pcm-primary, два последовательных чанка (ts1, ts2)
        when: два _process_chunk
        then: второй якорь = ((ts2 + overlap/rate)*1000, samples после 1-го)
        — overlap-голова уже записана первым чанком"""
        t = _pcm_primary_stub(tmp_path)
        overlap = t._wav_writer_system._overlap_samples
        capture = Mock()
        chunk = np.ones(3 * SR, dtype=np.float32) * 0.1
        capture.get_chunk.side_effect = [(chunk, 1000.0), (chunk, 1003.0)]

        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=False)
        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=False)

        expected_wall = (1003.0 + overlap / SR) * 1000.0
        expected_offset = int((3 * SR) / SR * 1000)  # первый чанк целиком
        assert t._system_wav_anchor == (expected_wall, expected_offset)

    def test_retry_pending_does_not_move_anchor(self, tmp_path):
        """given: якорь-sentinel, retry-слот с чанком (уже записан ранее)
        when: _process_chunk
        then: якорь не тронут — повторная запись не идёт"""
        t = _pcm_primary_stub(tmp_path)
        sentinel = (12345.0, 999)
        t._system_wav_anchor = sentinel
        state = SourceState()
        state.pending_chunk = (
            np.ones(3 * SR, dtype=np.float32) * 0.1, 5000.0, 0
        )

        LiveTranscriber._process_chunk(t, Mock(), state, Mock(), is_mic=False)

        assert t._system_wav_anchor == sentinel

    def test_silent_skip_does_not_move_anchor(self, tmp_path):
        """given: якорь-sentinel, НЕ pcm-primary, тихий чанк
        when: _process_chunk
        then: тишина не пишется → якорь не тронут"""
        t = _make_transcriber_stub()
        writer = IncrementalWavWriter(tmp_path / "audio_system.wav", SR)
        writer.open()
        t._wav_writer_system = writer
        sentinel = (12345.0, 999)
        t._system_wav_anchor = sentinel
        silent = np.zeros(3 * SR, dtype=np.float32)

        LiveTranscriber._process_chunk(
            t, _capture_returning(silent), SourceState(), Mock(), is_mic=False
        )

        assert t._system_wav_anchor == sentinel

    def test_mic_write_does_not_set_anchor(self, tmp_path):
        """given: якорь-sentinel, mic-запись речевого чанка
        when: _process_chunk is_mic=True
        then: system-якорь не тронут (mic — отдельный трек)"""
        t = _make_transcriber_stub()
        writer = IncrementalWavWriter(tmp_path / "audio_mic.wav", SR)
        writer.open()
        t._wav_writer_mic = writer
        t._transcribe_direct = Mock(return_value="")
        sentinel = (12345.0, 999)
        t._system_wav_anchor = sentinel
        speech = np.ones(3 * SR, dtype=np.float32) * 0.1

        LiveTranscriber._process_chunk(
            t, _capture_returning(speech), SourceState(), Mock(), is_mic=True
        )

        assert t._system_wav_anchor == sentinel


# =============================================================================
# Silence-split: ротация WAV-writer'ов на новый митинг
# =============================================================================


class TestSilenceSplitWavRotation:
    """split_on_silence_timeout() меняет current_meeting_path мимо
    KTalk-bridge callback — writer'ы должны ротироваться там же, иначе
    сегменты нового orphan-митинга ссылаются на WAV старого
    ("No audio file available for this meeting")."""

    def _make_split_stub(self, tmp_path):
        t = _make_transcriber_stub()
        storage = MeetingStorage(base_path=tmp_path)
        # ktalk=True — начальный source="orphan"; split_on_silence_timeout
        # работает только для orphan-митингов
        storage.start_session(system_audio=True, ktalk=True)
        old_id = storage.current_meeting_id
        storage.append_segment(speaker="A", speaker_id="A", text="старая реплика")
        t._meeting_storage = storage

        t.save_audio = True
        t.sample_rate = SR
        t.mic_capture = None
        t.system_capture = object()  # writer-гейт в _on_meeting_change

        writer = IncrementalWavWriter(
            storage.base_path / old_id / "audio_system.wav", SR
        )
        writer.open()
        writer.write_chunk(np.ones(3 * SR, dtype=np.float32) * 0.1)
        t._wav_writer_system = writer
        # Якорь старого митинга — после ротации должен обнулиться
        t._system_wav_anchor = (111.0, 222)

        # Реальная ротация вместо Mock-атрибута
        t._on_meeting_change = (
            lambda *a, **k: LiveTranscriber._on_meeting_change(t, *a, **k)
        )
        return t, storage, old_id

    def test_split_rotates_writer_and_rewrites_chunk(self, tmp_path):
        """given: orphan-митинг с сегментом и пишущимся system WAV,
               тишина дольше SILENCE_SPLIT_TIMEOUT
        when: _check_silence_split с триггерным чанком
        then: storage перешёл на новый митинг, в его директории появился
              audio_system.wav с перезаписанным чанком, state.audio_offset
              указывает в НОВЫЙ файл, а meta старого митинга получил
              audio_system_path"""
        t, storage, old_id = self._make_split_stub(tmp_path)

        state = SourceState()
        now = time.time()
        state.last_text_time = now - SILENCE_SPLIT_TIMEOUT - 1
        chunk = np.ones(3 * SR, dtype=np.float32) * 0.1

        LiveTranscriber._check_silence_split(t, state, now, chunk, is_mic=False)

        new_id = storage.current_meeting_id
        assert new_id != old_id
        new_wav = storage.base_path / new_id / "audio_system.wav"
        assert new_wav.exists()
        assert t._wav_writer_system.path == new_wav
        # Триггерный чанк перезаписан в новый файл — его сегмент будет
        # указывать на валидный диапазон
        assert t._wav_writer_system._samples_written > 0
        assert state.audio_offset == 0
        # Якорь старого митинга сброшен — новый WAV начинает свою timeline
        assert t._system_wav_anchor is None
        # Старый митинг получил путь к финализированному аудио
        old_meta = json.loads(
            (storage.base_path / old_id / "meta.json").read_text(encoding="utf-8")
        )
        assert old_meta.get("audio_system_path", "").endswith(
            f"{old_id}/audio_system.wav"
        )

    def test_no_split_below_timeout_keeps_writer(self, tmp_path):
        """given: тишина меньше SILENCE_SPLIT_TIMEOUT
        when: _check_silence_split
        then: митинг и writer не меняются"""
        t, storage, old_id = self._make_split_stub(tmp_path)

        state = SourceState()
        now = time.time()
        state.last_text_time = now - 1.0
        chunk = np.ones(3 * SR, dtype=np.float32) * 0.1

        LiveTranscriber._check_silence_split(t, state, now, chunk, is_mic=False)

        assert storage.current_meeting_id == old_id
        assert t._wav_writer_system.path.parent.name == old_id


# =============================================================================
# Длительность слайса сегмента: только offset'ы своего трека
# =============================================================================


class TestSegmentSliceDuration:
    """Mic и system WAV пишутся независимо (silence-skip) — их таймлайны
    разные, и длительность сегмента можно ограничивать только offset'ом
    следующего сегмента ТОГО ЖЕ трека (is_local)."""

    def _rows(self):
        # Реальный паттерн: mic/system двойники одной фразы интерливятся
        return [
            {"speaker_id": "local", "is_local": True, "audio_offset_ms": 0},
            {"speaker_id": "Unknown", "is_local": False, "audio_offset_ms": 0},
            {"speaker_id": "local", "is_local": True, "audio_offset_ms": 3000},
            {"speaker_id": "Unknown", "is_local": False, "audio_offset_ms": 3000},
            {"speaker_id": "local", "is_local": True, "audio_offset_ms": 5500},
            {"speaker_id": "Unknown", "is_local": False, "audio_offset_ms": 8000},
        ]

    def test_mic_segment_bounded_by_next_mic_segment(self):
        """given: mic-сегмент (3000), следом system-двойник (3000) и mic (5500)
        when: compute_segment_duration_ms
        then: длительность 2500 (до следующего mic), а не кламп 1000 от
        system-двойника с тем же offset"""
        from server import compute_segment_duration_ms
        assert compute_segment_duration_ms(self._rows(), 2) == 2500

    def test_system_segment_bounded_by_next_system_segment(self):
        from server import compute_segment_duration_ms
        assert compute_segment_duration_ms(self._rows(), 3) == 5000

    def test_last_segment_on_track_uses_default_chunk(self):
        from server import compute_segment_duration_ms
        # последний mic-сегмент (5500): дефолтные 3000
        assert compute_segment_duration_ms(self._rows(), 4) == 3000

    def test_continuations_extend_across_other_track_rows(self):
        """given: продолжения спикера, между ними строки другого трека
        when: compute_segment_duration_ms для начала группы
        then: слайс тянется до конца группы + дефолтный хвост"""
        from server import compute_segment_duration_ms
        rows = [
            {"speaker_id": "A", "is_local": False, "audio_offset_ms": 1000},
            {"speaker_id": "local", "is_local": True, "audio_offset_ms": 500},
            {"speaker_id": "A", "is_local": False, "audio_offset_ms": 4000, "cont": True},
            {"speaker_id": "A", "is_local": False, "audio_offset_ms": 7000, "cont": True},
        ]
        assert compute_segment_duration_ms(rows, 0) == (7000 - 1000) + 3000

    def test_event_rows_do_not_bound_duration(self):
        """given: маркер-событие (offset 0) между речевыми сегментами
        when: compute_segment_duration_ms
        then: событие пропускается — иначе отрицательная разница даёт кламп 1s"""
        from server import compute_segment_duration_ms
        rows = [
            {"speaker_id": "A", "is_local": False, "audio_offset_ms": 10000},
            {"speaker_id": "system", "is_local": False, "audio_offset_ms": 0,
             "event_type": "pcm_degraded"},
            {"speaker_id": "A", "is_local": False, "audio_offset_ms": 12000},
        ]
        assert compute_segment_duration_ms(rows, 0) == 2000

    def test_clamped_to_bounds(self):
        from server import compute_segment_duration_ms
        rows = [
            {"speaker_id": "A", "is_local": False, "audio_offset_ms": 0},
            {"speaker_id": "A", "is_local": False, "audio_offset_ms": 100},
            {"speaker_id": "B", "is_local": False, "audio_offset_ms": 999000},
        ]
        assert compute_segment_duration_ms(rows, 0) == 1000   # min clamp
        assert compute_segment_duration_ms(rows, 1) == 30000  # max clamp


# =============================================================================
# IncrementalWavWriter: заголовок валиден в любой момент
# =============================================================================


class TestWavHeaderAlwaysValid:
    def test_wav_readable_without_finalize(self, tmp_path):
        """given: writer с записанным чанком, finalize НЕ вызван (SIGKILL)
        when: файл читается строгим парсером (stdlib wave)
        then: заголовок содержит реальный размер данных"""
        import wave
        writer = IncrementalWavWriter(tmp_path / "audio.wav", SR)
        writer.open()
        writer.write_chunk(np.ones(SR, dtype=np.float32) * 0.1)

        w = wave.open(str(tmp_path / "audio.wav"))
        assert w.getnframes() == SR
