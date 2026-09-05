"""Общий буфер захвата аудио для микрофона и системного звука.

Оба продюсера (sounddevice-callback и stdout сабпроцесса) кормят один и тот
же кольцевой буфер и отдают чанки через единый `get_chunk`.

Контракт `get_chunk` — 1-D моно `float32`: его ждут и ASR
(`ASRModule.transcribe_chunk`), и `IncrementalWavWriter`.

Дизайн буферизации: docs/plans/2026-06-02-asr-fallback-buffering-design.md §2.
"""

import threading
import time
from typing import Optional, Tuple

import numpy as np

from .config import ASR_PENDING_BUFFER_CAP_S, CAPTURE_RESYNC_MAX_S


class BufferedAudioCapture:
    """Буфер захвата с capture-временем, капом и overlap-нарезкой чанков."""

    def __init__(self, sample_rate: int, name: str,
                 max_buffer_s: float = ASR_PENDING_BUFFER_CAP_S):
        self.sample_rate = sample_rate
        self.name = name
        self.buffer = []
        self.running = False
        self._lock = threading.Lock()
        # _buffer_start_ts — epoch-время захвата первого невычитанного сэмпла.
        self._buffer_start_ts: Optional[float] = None
        self._buffered_samples = 0
        self._overlap_samples = int(0.5 * sample_rate)
        self._max_buffered_samples = int(max_buffer_s * sample_rate)
        self._resync_max_samples = int(CAPTURE_RESYNC_MAX_S * sample_rate)
        self._source_epoch = 0
        self._chunk_epoch = 0

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
        # Realtime (consumer keeps up): re-anchor the buffer start to wall-clock.
        # Sample counting alone never re-syncs — the 0.5 s overlap keeps the
        # buffer non-empty, so any device under-delivery accumulates forever
        # (mic stamps trailed the PCM speakers by ~5 min by 2026-09-03). A
        # backlog (ASR outage) keeps sample-derived spoken-time stamps.
        if self._buffered_samples <= self._resync_max_samples:
            self._buffer_start_ts = captured_at + (len(data) - self._buffered_samples) / self.sample_rate
        return dropped

    def _append_locked_for_tests(self, data: np.ndarray, captured_at: float) -> int:
        """Тестовый шов: _append_locked под собственным локом."""
        with self._lock:
            return self._append_locked(data, captured_at)

    def _clear_locked(self):
        self.buffer = []
        self._buffer_start_ts = None
        self._buffered_samples = 0

    def clear_buffer(self):
        """Discard all buffered audio (used on meeting boundary)."""
        with self._lock:
            self._clear_locked()

    @property
    def source_epoch(self) -> int:
        """Номер отрезка непрерывного потока; растёт на каждой границе источников."""
        return self._source_epoch

    @property
    def chunk_epoch(self) -> int:
        """`source_epoch` отрезка, из которого нарезан последний `get_chunk`.

        Снимок `source_epoch` рядом с вызовом `get_chunk` этого не даёт ни в
        каком порядке: до чтения буфера — граница, прошедшая между снимком и
        чтением, отдаёт чанк уже нового источника под старым номером (писатель
        срежет с него 0.5 с как overlap, которого в WAV нет); после — наоборот,
        чанк со старого источника получает новый номер и пишется целиком,
        дублируя overlap. Значение фиксируется под тем же локом, что и нарезка,
        и переписывается только следующим `get_chunk` — читатель у каждого
        захвата один (главный цикл `LiveTranscriber.run`).
        """
        return self._chunk_epoch

    def mark_source_boundary(self):
        """Граница источников: буфер сбрасывается, и разрыв виден потребителю.

        Одного сброса буфера мало. `IncrementalWavWriter` срезает с каждого
        чанка 0.5 с головы как overlap, уже записанный предыдущим чанком, — а
        после сброса эти 0.5 с и есть новое аудио с нового источника, и
        срезать их значит потерять их из WAV. Потребитель сравнивает
        `source_epoch` со своим снимком и на расхождении говорит писателю
        писать следующий чанк целиком.
        """
        with self._lock:
            self._clear_locked()
            self._source_epoch += 1

    def get_chunk(self, duration: float) -> Optional[Tuple[np.ndarray, float]]:
        """Возвращает (аудио чанк (1-D моно float32), epoch-время его первого сэмпла)."""
        if not self.running:
            return None

        samples_needed = int(duration * self.sample_rate)

        with self._lock:
            if not self.buffer:
                return None
            audio = np.concatenate(self.buffer, axis=0)
            if len(audio) < samples_needed:
                return None

            chunk = audio[:samples_needed]
            self._chunk_epoch = self._source_epoch
            chunk_start_ts = (
                self._buffer_start_ts
                if self._buffer_start_ts is not None
                else time.time() - duration
            )
            # 0.5 сек overlap для лучшего распознавания на границах
            overlap_start = max(0, samples_needed - self._overlap_samples)
            self.buffer = [audio[overlap_start:]]
            self._buffered_samples = len(audio) - overlap_start
            self._buffer_start_ts = chunk_start_ts + overlap_start / self.sample_rate

        # sounddevice всегда отдаёт (frames, channels) — даже на одном канале,
        # поэтому моно-свёртка и .flatten() обязательны: без них чанк уезжает в
        # ASR как (n,1) и forward падает молча.
        if chunk.ndim > 1 and chunk.shape[1] > 1:
            chunk = np.mean(chunk, axis=1)

        return chunk.astype(np.float32, copy=False).flatten(), chunk_start_ts
