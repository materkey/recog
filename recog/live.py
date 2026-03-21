"""
Модуль live транскрипции (микрофон, системный звук, KTalk).
"""

import struct
import sys
import time
import threading
from datetime import datetime
from pathlib import Path
from typing import Optional

import numpy as np
import torch

from collections import deque
from dataclasses import dataclass

from .config import (SPEAKER_SIMILARITY_THRESHOLD, VAD_RMS_THRESHOLD, SILENCE_SPLIT_TIMEOUT,
                     LAST_SPEAKER_TTL, MIC_SPEAKER_NAME,
                     RETROACTIVE_MAX_SEGMENTS, RETROACTIVE_MAX_SECONDS)
from .asr import ASRModule
from .speaker_db import SpeakerDatabase


class IncrementalWavWriter:
    """
    Writes WAV file incrementally during live capture.
    Supports streaming write of audio chunks without loading entire file in memory.
    Handles overlap by skipping duplicate samples at chunk boundaries.
    """

    def __init__(self, path: Path, sample_rate: int = 16000, channels: int = 1, overlap_seconds: float = 0.5):
        self.path = Path(path)
        self.sample_rate = sample_rate
        self.channels = channels
        self._file = None
        self._samples_written = 0
        # Overlap handling: chunks from get_chunk() include overlap for ASR quality,
        # but we should only write non-overlapping samples to avoid duplication
        self._overlap_samples = int(overlap_seconds * sample_rate)
        self._first_chunk = True

    def open(self, continuation=False):
        """Opens file and writes WAV header placeholder. Appends if file exists.

        Args:
            continuation: If True, audio capture is still running (overlap in buffer).
                         Skips overlap on first chunk to avoid duplication at boundary.
        """
        self.path.parent.mkdir(parents=True, exist_ok=True)
        if self.path.exists() and self.path.stat().st_size >= 44:
            # Reopen existing WAV: restore sample count from file size and append
            existing_data_size = self.path.stat().st_size - 44  # 44-byte WAV header
            self._samples_written = existing_data_size // 2  # 2 bytes per int16 sample
            self._file = open(self.path, 'r+b')
            self._file.seek(0, 2)  # Seek to end for appending
            # If capture is still running (meeting change), the buffer has overlap
            # from the previous chunk — skip it. On program restart, buffer is fresh.
            self._first_chunk = not continuation
        else:
            self._file = open(self.path, 'wb')
            self._write_header(0)  # Placeholder with 0 data size
            # Respect continuation for new files too: if capture is still running
            # (meeting change), skip overlap to avoid leaking old-meeting audio.
            self._first_chunk = not continuation

    def write_chunk(self, chunk: np.ndarray):
        """
        Appends audio chunk to file, skipping overlap samples to avoid duplication.

        Args:
            chunk: Audio samples as float32 array (values in [-1, 1])
        """
        if self._file is None:
            return

        # Skip overlap samples (except for first chunk) to avoid audio duplication
        # Chunks from get_chunk() include 0.5s overlap for ASR quality,
        # but we only write non-overlapping portion to WAV
        if self._first_chunk:
            self._first_chunk = False
            samples_to_write = chunk
        else:
            # Skip first overlap_samples - they were already written in previous chunk
            samples_to_write = chunk[self._overlap_samples:]

        if len(samples_to_write) == 0:
            return

        # Convert float32 [-1, 1] to int16
        chunk_int16 = (samples_to_write * 32767).astype(np.int16)
        self._file.write(chunk_int16.tobytes())
        self._samples_written += len(samples_to_write)

    def get_offset_ms(self) -> int:
        """Returns current offset in milliseconds based on actual samples written."""
        # Use samples_written instead of wall-clock time to account for
        # skipped overlap samples - wall-clock would drift by 0.5s per chunk
        return int(self._samples_written / self.sample_rate * 1000)

    def finalize(self):
        """Updates header with final size and closes file."""
        if self._file is None:
            return

        # Update header with correct data size
        data_size = self._samples_written * 2  # 2 bytes per int16 sample
        self._file.seek(0)
        self._write_header(data_size)
        self._file.close()
        self._file = None

    def _write_header(self, data_size: int):
        """Writes WAV header with specified data size."""
        # WAV header structure (44 bytes)
        # RIFF chunk
        self._file.write(b'RIFF')
        self._file.write(struct.pack('<I', 36 + data_size))  # File size - 8
        self._file.write(b'WAVE')

        # fmt chunk
        self._file.write(b'fmt ')
        self._file.write(struct.pack('<I', 16))  # Subchunk1 size (PCM)
        self._file.write(struct.pack('<H', 1))   # Audio format (1 = PCM)
        self._file.write(struct.pack('<H', self.channels))
        self._file.write(struct.pack('<I', self.sample_rate))
        byte_rate = self.sample_rate * self.channels * 2  # 2 bytes per sample
        self._file.write(struct.pack('<I', byte_rate))
        block_align = self.channels * 2
        self._file.write(struct.pack('<H', block_align))
        self._file.write(struct.pack('<H', 16))  # Bits per sample

        # data chunk
        self._file.write(b'data')
        self._file.write(struct.pack('<I', data_size))


class SingleDeviceCapture:
    """Захват аудио с одного устройства."""

    def __init__(self, device, sample_rate=16000, name="device"):
        import sounddevice as sd
        self.sd = sd
        self.sample_rate = sample_rate
        self.device = device
        self.name = name
        self.buffer = []
        self.running = False
        self.stream = None
        self._lock = None

    def _callback(self, indata, frames, time, status):
        if status:
            print(f"Audio status ({self.name}): {status}", file=sys.stderr)
        if self._lock:
            with self._lock:
                self.buffer.append(indata.copy())
        else:
            self.buffer.append(indata.copy())

    def start(self):
        import threading
        self._lock = threading.Lock()

        # Определяем количество каналов устройства
        device_info = self.sd.query_devices(self.device)
        max_channels = device_info.get('max_input_channels', 1)
        channels = min(max_channels, 2)  # Используем 1 или 2 канала

        self.stream = self.sd.InputStream(
            device=self.device,
            channels=channels,
            samplerate=self.sample_rate,
            callback=self._callback
        )
        self.stream.start()
        self.running = True
        self._channels = channels

    def stop(self):
        if self.running:
            self.stream.stop()
            self.stream.close()
            self.running = False

    def clear_buffer(self):
        """Discard all buffered audio (used on meeting boundary)."""
        if self._lock:
            with self._lock:
                self.buffer = []
        else:
            self.buffer = []

    def get_chunk(self, duration: float) -> Optional[np.ndarray]:
        """Возвращает накопленный аудио чанк (моно)."""
        samples_needed = int(duration * self.sample_rate)
        if not self.buffer:
            return None

        if self._lock:
            self._lock.acquire()
        try:
            if not self.buffer:
                return None
            audio = np.concatenate(self.buffer, axis=0)
            if len(audio) < samples_needed:
                return None

            chunk = audio[:samples_needed]
            overlap_samples = int(0.5 * self.sample_rate)  # 0.5 сек overlap для лучшего распознавания на границах
            overlap_start = max(0, samples_needed - overlap_samples)
            self.buffer = [audio[overlap_start:]]
        finally:
            if self._lock:
                self._lock.release()

        # Конвертируем стерео в моно если нужно
        if chunk.ndim > 1 and chunk.shape[1] > 1:
            chunk = np.mean(chunk, axis=1)

        return chunk.flatten()

    @staticmethod
    def list_devices():
        import sounddevice as sd
        print("Доступные аудио устройства:")
        print(sd.query_devices())

    @staticmethod
    def find_builtin_mic():
        """Находит встроенный микрофон MacBook."""
        import sounddevice as sd
        devices = sd.query_devices()
        for i, dev in enumerate(devices):
            name = dev['name'].lower()
            # Ищем встроенный микрофон MacBook
            if 'macbook' in name and 'microphone' in name and dev['max_input_channels'] > 0:
                return i
        # Fallback: любое устройство с "built-in" или "internal"
        for i, dev in enumerate(devices):
            name = dev['name'].lower()
            if ('built-in' in name or 'internal' in name) and dev['max_input_channels'] > 0:
                return i
        return None

    @staticmethod
    def find_blackhole():
        """Находит BlackHole устройство для захвата системного звука."""
        import sounddevice as sd
        devices = sd.query_devices()
        for i, dev in enumerate(devices):
            if 'blackhole' in dev['name'].lower() and dev['max_input_channels'] > 0:
                return i
        return None


class LiveAudioCapture:
    """Захват аудио с микрофона и/или системного звука (BlackHole)."""

    def __init__(self, mic_device=None, system_device=None, sample_rate=16000):
        self.sample_rate = sample_rate
        self.mic_device = mic_device
        self.system_device = system_device

    @staticmethod
    def list_devices():
        SingleDeviceCapture.list_devices()


@dataclass
class RecentSegment:
    """Tracks a recent system-audio segment for retroactive correction."""
    speaker: str
    chunk_time: float
    line_idx: int                # Index in transcript_lines
    meeting_segment_id: int      # Segment ID in MeetingStorage JSONL
    is_local: bool
    timestamp_display: str       # HH:MM:SS for extension correction
    text: str                    # Original text for extension correction


class LiveTranscriber:
    """Координатор live транскрипции."""

    def __init__(
        self,
        mic_device=None,
        system_device=None,
        speaker_db_path=None,
        chunk_duration=3.0,  # Баланс между задержкой и качеством
        output_file=None,
        no_diarization=False,
        use_ktalk=False,  # Использовать KTalk bridge вместо embedding-based diarization
        use_screencapture=False,  # Использовать ScreenCaptureKit вместо BlackHole
        save_audio=False,  # Сохранять аудио для последующей диаризации
        mic_speaker_name=MIC_SPEAKER_NAME
    ):
        self.mic_device = mic_device
        self.system_device = system_device
        self.sample_rate = 16000
        self.use_screencapture = use_screencapture
        self.save_audio = save_audio

        # Определяем режим: dual (оба), mic-only, system-only, screencapture
        self.dual_mode = (mic_device is not None and (system_device is not None or use_screencapture))

        # Создаём capture устройства
        self.mic_capture = None
        self.system_capture = None

        if use_screencapture:
            # Используем ScreenCaptureKit для системного звука
            from screencapture_audio import ScreenCaptureAudio
            self.system_capture = ScreenCaptureAudio(self.sample_rate)
            if mic_device is not None:
                self.mic_capture = SingleDeviceCapture(mic_device, self.sample_rate, "mic")
        elif self.dual_mode:
            self.mic_capture = SingleDeviceCapture(mic_device, self.sample_rate, "mic")
            self.system_capture = SingleDeviceCapture(system_device, self.sample_rate, "system")
        elif system_device is not None:
            self.system_capture = SingleDeviceCapture(system_device, self.sample_rate, "system")
        else:
            self.mic_capture = SingleDeviceCapture(mic_device, self.sample_rate, "mic")

        self.asr = ASRModule()
        self.speaker_db = SpeakerDatabase(speaker_db_path) if speaker_db_path else None
        self.chunk_duration = chunk_duration
        self.output_file = output_file
        self.no_diarization = no_diarization
        self.use_ktalk = use_ktalk
        self.mic_speaker_name = mic_speaker_name
        self.running = False
        self.session_speakers = {}  # embedding -> speaker_label
        self._session_lock = threading.Lock()
        self.speaker_counter = 0
        self.transcript_lines = []
        self._model_device = None
        self._model_dtype = None

        # KTalk bridge для получения данных о спикерах
        self._ktalk_bridge = None
        self._ktalk_current_speaker = None

        # Meeting storage для сохранения транскриптов
        self._meeting_storage = None

        # Буфер аудио для двухпроходной транскрипции (KTalk mode)
        self._audio_buffer = []  # [(timestamp, audio_chunk), ...]
        self._audio_buffer_max_seconds = 60  # Максимум 60 сек в буфере
        self._draft_segments = {}  # segment_id -> {'lines': [...], 'speaker': str, 'start_time': float, 'chunks': [...]}
        self._current_segment_id = None
        self._current_segment_start_time = None  # Python timestamp начала текущего сегмента

        # Recent system-audio segments for retroactive correction of Unknown runs
        self._recent_system_segments: deque[RecentSegment] = deque(maxlen=RETROACTIVE_MAX_SEGMENTS)

        # Audio recording for on-demand diarization
        self._wav_writer_mic: Optional[IncrementalWavWriter] = None
        self._wav_writer_system: Optional[IncrementalWavWriter] = None
        self._wav_lock = threading.Lock()  # Guards _wav_writer_mic/_wav_writer_system
        self._state_lock = threading.Lock()  # Guards per-source state during meeting transitions
        self._meeting_generation = 0  # Incremented on meeting change; used to discard stale chunks

    def _transcribe_direct(self, audio_chunk: np.ndarray) -> Optional[str]:
        """Транскрибирует напрямую через tensor без temp файлов."""
        try:
            model = self.asr.model
            if model is None:
                return None

            # Кешируем device/dtype при первом вызове
            if self._model_device is None:
                self._model_device = model._device
                self._model_dtype = model._dtype

            wav = torch.from_numpy(audio_chunk).float()
            wav = wav.to(self._model_device).to(self._model_dtype).unsqueeze(0)
            length = torch.tensor([wav.shape[-1]], device=self._model_device, dtype=torch.long)

            encoded, encoded_len = model.forward(wav, length)
            text = model.decoding.decode(model.head, encoded, encoded_len)[0]
            return text.strip() if text else None

        except Exception as e:
            # Fallback на temp file если direct не работает
            return None

    # Кэш для модели embedding (загружается один раз)
    _ecapa_classifier = None
    _ecapa_lock = threading.Lock()

    def _get_embedding(self, audio_chunk: np.ndarray) -> Optional[np.ndarray]:
        """Извлекает embedding из аудио чанка используя SpeechBrain ECAPA-TDNN."""
        if self.no_diarization:
            return None

        try:
            import warnings

            # Ленивая загрузка модели (один раз) с double-checked locking
            if LiveTranscriber._ecapa_classifier is None:
                with LiveTranscriber._ecapa_lock:
                    if LiveTranscriber._ecapa_classifier is None:
                        with warnings.catch_warnings():
                            warnings.filterwarnings("ignore", category=UserWarning)
                            warnings.filterwarnings("ignore", category=FutureWarning)
                            from speechbrain.inference.speaker import EncoderClassifier
                            LiveTranscriber._ecapa_classifier = EncoderClassifier.from_hparams(
                                source="speechbrain/spkrec-ecapa-voxceleb",
                                savedir="/tmp/speechbrain_ecapa"
                            )

            # Прямое извлечение embedding без temp файлов
            with warnings.catch_warnings():
                warnings.filterwarnings("ignore")
                audio_tensor = torch.from_numpy(audio_chunk).float().unsqueeze(0)
                emb = LiveTranscriber._ecapa_classifier.encode_batch(audio_tensor)
            return emb.squeeze().cpu().numpy()

        except Exception as e:
            print(f"Embedding error: {e}", file=sys.stderr)

        return None

    def _identify_speaker(self, embedding: Optional[np.ndarray]) -> str:
        """Идентифицирует спикера по embedding."""
        if embedding is None:
            return "speaker"

        # Приводим к float32 для консистентности
        embedding = embedding.astype(np.float32)

        # Сначала проверяем speaker-DB
        if self.speaker_db:
            match = self.speaker_db.match_speaker(embedding, SPEAKER_SIMILARITY_THRESHOLD)
            if match:
                return match[1]  # display_name

        # Проверяем среди спикеров сессии (best match, not first match)
        with self._session_lock:
            best_label = None
            best_similarity = -1.0
            for emb_key, label in self.session_speakers.items():
                db_emb = np.frombuffer(emb_key, dtype=np.float32)
                similarity = SpeakerDatabase._cosine_similarity(embedding, db_emb)
                if similarity >= SPEAKER_SIMILARITY_THRESHOLD and similarity > best_similarity:
                    best_similarity = similarity
                    best_label = label

            if best_label is not None:
                return best_label

            # Новый спикер
            self.speaker_counter += 1
            label = f"speaker_{self.speaker_counter}"
            self.session_speakers[embedding.astype(np.float32).tobytes()] = label
            return label

    def _ktalk_speaker_callback(self, speaker_name: Optional[str], is_speaking: bool):
        """Callback для обновления текущего спикера из KTalk."""
        self._ktalk_current_speaker = speaker_name if is_speaking else None

    def _get_ktalk_speaker(self) -> Optional[str]:
        """Возвращает текущего спикера из KTalk bridge (только реальные данные от extension)."""
        if self._ktalk_bridge:
            # get_current_speaker уже включает "память" о последнем спикере (3 сек)
            return self._ktalk_bridge.get_current_speaker()
        return self._ktalk_current_speaker

    def _retroactive_correct_unknown_run(
        self,
        identified_speaker: str,
        current_time: float,
        *,
        correction_snapshot=None,
    ):
        """
        Scan backwards through recent system-audio segments and re-attribute
        consecutive Unknown segments to the newly identified speaker.

        Stops at: named speaker, is_local=True, max segments, or max time window.

        If correction_snapshot is provided (segments, generation, meeting_path),
        uses that instead of re-acquiring the lock — eliminates a race where
        _invalidate_pending_chunks clears the deque between the caller's lock
        release and this method's lock acquisition.
        """
        if correction_snapshot is not None:
            segments, snapshot_gen, snapshot_meeting_path = correction_snapshot
        else:
            with self._state_lock:
                segments = list(self._recent_system_segments)
                snapshot_gen = self._meeting_generation
                snapshot_meeting_path = (
                    self._meeting_storage.current_meeting_path
                    if self._meeting_storage else None
                )
        # Last element is the current (already identified) segment — skip it
        to_correct = []
        for seg in reversed(segments[:-1]):
            if seg.speaker != "Unknown":
                break
            if seg.is_local:
                break
            if (current_time - seg.chunk_time) > RETROACTIVE_MAX_SECONDS:
                break
            to_correct.append(seg)

        if not to_correct:
            return

        # Update in-memory transcript_lines (session-wide, safe regardless of
        # meeting switch since line_idx is absolute within the session).
        for seg in to_correct:
            if seg.line_idx < len(self.transcript_lines):
                old_line = self.transcript_lines[seg.line_idx]
                new_line = old_line.replace("**Unknown**", f"**{identified_speaker}**", 1)
                self.transcript_lines[seg.line_idx] = new_line

            # Update the segment record itself
            seg.speaker = identified_speaker

        # Check if a meeting switch occurred.  Even if it did, we still
        # update the JSONL using snapshot_meeting_path (captured above)
        # so corrections land in the old meeting where the segments live.
        # Only skip the Chrome extension notification on switch, since
        # the extension has already moved to the new meeting context.
        meeting_switched = False
        with self._state_lock:
            if self._meeting_generation != snapshot_gen:
                meeting_switched = True

        # Update MeetingStorage JSONL
        if self._meeting_storage:
            updates = [
                {
                    "segment_id": seg.meeting_segment_id,
                    "speaker": identified_speaker,
                    "uncertain": False,
                    "uncertain_reason": "",
                }
                for seg in to_correct
                if seg.meeting_segment_id > 0
            ]
            if updates:
                self._meeting_storage.update_segments(
                    updates, meeting_path=snapshot_meeting_path
                )

        # Send corrections to Chrome extension (skip if meeting switched —
        # the extension has already moved to the new meeting context).
        if self._ktalk_bridge and not meeting_switched:
            for seg in to_correct:
                self._ktalk_bridge.send_transcript(
                    identified_speaker,
                    seg.text,
                    seg.timestamp_display,
                    is_continuation=False,
                    is_local=False,
                    uncertain=False,
                    uncertain_reason="",
                    correction=True,
                )

    def _add_to_audio_buffer(self, timestamp: float, audio_chunk: np.ndarray):
        """Добавляет аудио в буфер для последующей перетранскрипции."""
        self._audio_buffer.append((timestamp, audio_chunk.copy()))

        # Удаляем старые данные
        cutoff_time = timestamp - self._audio_buffer_max_seconds
        while self._audio_buffer and self._audio_buffer[0][0] < cutoff_time:
            self._audio_buffer.pop(0)

    def _get_audio_for_segment(self, start_time: float, end_time: float) -> Optional[np.ndarray]:
        """Извлекает аудио для заданного временного диапазона."""
        chunks = []
        for ts, chunk in self._audio_buffer:
            chunk_end = ts + len(chunk) / self.sample_rate
            # Проверяем пересечение с запрошенным диапазоном
            if chunk_end >= start_time and ts <= end_time:
                chunks.append(chunk)

        if not chunks:
            return None

        return np.concatenate(chunks)

    def _handle_segment_complete(self, segment, speaker_width: int):
        """Обрабатывает завершённый сегмент - перетранскрибирует и заменяет черновики."""
        # Ищем сегмент по speaker_id с суффиксом _active
        matching_segment_id = f"{segment.speaker_id}_active"

        if matching_segment_id not in self._draft_segments:
            # Это нормально если сегмент уже был обработан (например, по MAX_SEGMENT_DURATION)
            # Не выводим ошибку для коротких сегментов
            if segment.duration_ms > 500:
                pass  # Тихо пропускаем - возможно дубликат
            return

        draft_info = self._draft_segments[matching_segment_id]
        audio_chunks = draft_info.get('chunks', [])

        if not audio_chunks:
            print(f"\n[SEGMENT] No audio chunks for {segment.speaker_name}", file=sys.stderr)
            del self._draft_segments[matching_segment_id]
            return

        # Склеиваем все чанки аудио
        audio = np.concatenate(audio_chunks)
        if len(audio) < 1000:
            del self._draft_segments[matching_segment_id]
            return

        # Перетранскрибируем весь сегмент
        final_text = self._transcribe_direct(audio)
        if not final_text:
            del self._draft_segments[matching_segment_id]
            return

        # Форматируем время (используем Python timestamp из draft_info)
        import time as time_module
        start_time = draft_info.get('start_time', time_module.time())
        start_dt = datetime.fromtimestamp(start_time)
        timestamp = start_dt.strftime("%H:%M:%S")

        # Формируем финальную строку
        speaker = segment.speaker_name
        duration_sec = len(audio) / self.sample_rate

        # Находим черновые строки
        draft_lines = []
        line_indices = draft_info.get('lines', [])
        for idx in line_indices:
            if idx < len(self.transcript_lines) and self.transcript_lines[idx]:
                draft_lines.append(self.transcript_lines[idx])

        del self._draft_segments[matching_segment_id]

        # Выводим финальную транскрипцию
        sys.stdout.write(f"\n")
        sys.stdout.write(f"{'─' * 60}\n")
        sys.stdout.write(f"[FINAL {timestamp}] {speaker:{speaker_width}s} ({duration_sec:.1f}s):\n")
        sys.stdout.write(f"  {final_text}\n")
        if draft_lines:
            sys.stdout.write(f"  (черновик: {len(draft_lines)} строк)\n")
        sys.stdout.write(f"{'─' * 60}\n")
        sys.stdout.flush()

        # Обновляем transcript_lines (markdown формат)
        final_line = f"\n**{speaker}** *[{timestamp}]*: {final_text}\n"
        self.transcript_lines.append(final_line)

    def run(self, force_exit: bool = False):
        """Основной цикл live транскрипции."""
        import concurrent.futures

        print("=" * 60)
        print(" LIVE TRANSCRIPTION")
        if self.use_screencapture:
            if self.mic_capture:
                print(f" Mode: DUAL (Mic: {self.mic_device} + ScreenCaptureKit)")
            else:
                print(f" System audio: ScreenCaptureKit (no BlackHole needed)")
        elif self.dual_mode:
            print(f" Mode: DUAL (Mic: {self.mic_device} + System: {self.system_device})")
        elif self.system_device is not None:
            print(f" Mic: none | System: {self.system_device}")
        else:
            mic_display = self.mic_device if self.mic_device is not None else 'default'
            print(f" Mic: {mic_display} | System: none")

        if self.use_ktalk:
            print(" Speaker detection: KTalk (via Chrome extension)")
        elif self.no_diarization:
            print(" Speaker detection: disabled")
        else:
            print(" Speaker detection: embedding-based")

        print("=" * 60)
        print()
        print("Press Ctrl+C to stop")
        print("-" * 60)
        print()

        # Initialize Meeting Storage
        try:
            from meeting_storage import MeetingStorage
            self._meeting_storage = MeetingStorage()
            meeting_id = self._meeting_storage.start_session(
                mic=self.mic_capture is not None,
                system_audio=self.system_capture is not None,
                ktalk=self.use_ktalk,
                screencapture=self.use_screencapture,
                chunk_duration_ms=int(self.chunk_duration * 1000)
            )
            print(f"Meeting started: {meeting_id}")

            # Initialize audio recording if enabled
            if self.save_audio:
                meeting_path = self._meeting_storage.base_path / meeting_id
                if self.mic_capture:
                    mic_audio_path = meeting_path / "audio_mic.wav"
                    self._wav_writer_mic = IncrementalWavWriter(mic_audio_path, self.sample_rate)
                    self._wav_writer_mic.open()
                    print(f"Recording mic audio: {mic_audio_path}")
                if self.system_capture:
                    system_audio_path = meeting_path / "audio_system.wav"
                    self._wav_writer_system = IncrementalWavWriter(system_audio_path, self.sample_rate)
                    self._wav_writer_system.open()
                    print(f"Recording system audio: {system_audio_path}")
        except ImportError:
            print("Warning: meeting_storage module not found, transcripts won't be saved", file=sys.stderr)
            self._meeting_storage = None
        except Exception as e:
            print(f"Warning: MeetingStorage error: {e}, transcripts won't be saved", file=sys.stderr)
            self._meeting_storage = None

        # Initialize KTalk bridge if needed
        if self.use_ktalk:
            try:
                from ktalk_bridge import KTalkBridge
                self._ktalk_bridge = KTalkBridge()
                self._ktalk_bridge.set_speaker_callback(self._ktalk_speaker_callback)
                # Connect MeetingStorage to KTalk bridge
                if self._meeting_storage:
                    self._ktalk_bridge.set_meeting_storage(self._meeting_storage)
                    # Wire IncrementGenerator for auto-summaries on ktalk_leave
                    from recog.increment import IncrementGenerator
                    self._increment_generator = IncrementGenerator(self._meeting_storage.base_path)
                    self._increment_generator.set_callback(
                        lambda mid, ver: print(
                            f"[increment] ready: {mid} v{ver}", file=sys.stderr
                        )
                    )
                    self._ktalk_bridge.set_increment_generator(self._increment_generator)
                # Register callbacks for meeting changes:
                # pre-switch invalidates in-flight chunks after path changes
                self._ktalk_bridge.set_pre_meeting_switch_callback(self._invalidate_pending_chunks)
                # post-switch reinitializes audio recording at new path
                self._ktalk_bridge.set_meeting_change_callback(self._on_meeting_change)
                self._ktalk_bridge.start()
                print("KTalk bridge started, waiting for Chrome extension...")
                time.sleep(1)
            except ImportError:
                print("Warning: ktalk_bridge module not found, falling back to embedding-based", file=sys.stderr)
                self.use_ktalk = False
            except Exception as e:
                print(f"Warning: KTalk bridge error: {e}, falling back to embedding-based", file=sys.stderr)
                self.use_ktalk = False

        self.asr.load_model(quiet=True)

        # Запускаем capture устройства
        if self.mic_capture:
            self.mic_capture.start()
        if self.system_capture:
            self.system_capture.start()

        self.running = True
        self._is_tty = sys.stdout.isatty()

        # Executor для фоновой диаризации
        executor = concurrent.futures.ThreadPoolExecutor(max_workers=2)

        speaker_width = 24  # Для полных имён (имя + фамилия)
        mic_speaker_name = self.mic_speaker_name

        # Отдельные состояния для каждого источника
        class SourceState:
            def __init__(self):
                self.pending_updates = []
                self.last_words = []
                self.last_speaker = None
                self.last_speech_time = 0
                self.segment_start_time = 0  # Время начала текущего сегмента
                self.last_text_time = time.time()
                self.audio_offset = 0  # Offset in WAV file where current chunk starts

        self._mic_state = SourceState()
        self._sys_state = SourceState()
        mic_state = self._mic_state
        sys_state = self._sys_state

        speaker_turn_gap = 3.0
        max_segment_duration = 20.0  # Принудительный разрыв сегмента после N секунд

        def normalize_word(w: str) -> str:
            return ''.join(c.lower() for c in w if c.isalnum())

        def deduplicate(text: str, prev_words: list) -> str:
            if not prev_words or not text:
                return text
            words = text.split()
            if not words:
                return text
            norm_words = [normalize_word(w) for w in words]
            norm_prev = [normalize_word(w) for w in prev_words]
            max_overlap = min(len(words), len(prev_words), 6)
            for overlap_len in range(max_overlap, 0, -1):
                if norm_words[:overlap_len] == norm_prev[-overlap_len:]:
                    result = ' '.join(words[overlap_len:])
                    return result if result.strip() else None
            return text

        def process_pending(state, is_mic=False):
            """Обрабатывает все pending updates для источника."""
            # Lock prevents _on_meeting_change from clearing state mid-iteration,
            # which would cause segments committed to the wrong meeting or
            # the pending_updates clear being overwritten by still_pending.
            with self._state_lock:
                if not state.pending_updates:
                    return

                still_pending = []
                for i, pending in enumerate(state.pending_updates):
                    line_idx, ts, txt, future, chunk_time, pending_audio_offset, pending_gen = pending
                    if not future.done():
                        # Stop at first incomplete future to preserve commit order
                        still_pending = state.pending_updates[i:]
                        break

                    try:
                        speaker = future.result(timeout=0)
                    except Exception:
                        speaker = "???"

                    # Skip if meeting changed since this chunk was captured
                    if self._meeting_generation != pending_gen:
                        continue

                    time_gap = chunk_time - state.last_speech_time
                    segment_duration = chunk_time - state.segment_start_time
                    is_continuation = (speaker == state.last_speaker and time_gap < speaker_turn_gap and segment_duration < max_segment_duration)

                    if speaker not in ("???", "speaker"):
                        state.last_speaker = speaker
                    state.last_speech_time = chunk_time
                    if not is_continuation:
                        state.segment_start_time = chunk_time

                    # Markdown формат: **Speaker** *[HH:MM:SS]*: text
                    final_line = f"\n**{speaker}** *[{ts}]*: {txt}\n"

                    if line_idx < len(self.transcript_lines):
                        self.transcript_lines[line_idx] = final_line
                        # Rewrite line on terminal if it's the last one
                        if line_idx == len(self.transcript_lines) - 1:
                            sys.stdout.write(f"\r\033[K{final_line}")
                            sys.stdout.flush()

                    if self._meeting_storage:
                        uncertain = speaker == "???"
                        self._meeting_storage.append_segment(
                            speaker=speaker,
                            speaker_id=speaker,
                            text=txt,
                            is_local=is_mic,
                            is_continuation=is_continuation,
                            uncertain=uncertain,
                            uncertain_reason="embedding_error" if uncertain else "",
                            audio_offset_ms=pending_audio_offset
                        )

                state.pending_updates = still_pending

        def process_chunk(capture, state, is_mic=False):
            """Обрабатывает чанк из источника."""
            # Snapshot meeting generation BEFORE reading buffer so that audio
            # captured before a meeting switch is tagged with the old generation
            # and discarded by the generation check downstream.
            chunk_gen = self._meeting_generation
            chunk = capture.get_chunk(self.chunk_duration)
            if chunk is None or len(chunk) < 1000:
                return
            # Audio was captured during the PREVIOUS chunk_duration seconds
            chunk_end_time = time.time()
            chunk_start_time = chunk_end_time - self.chunk_duration

            # Capture audio offset BEFORE writing chunk (for segment playback)
            # This offset points to where this chunk's audio STARTS in the WAV file
            # Lock protects against _on_meeting_change swapping writers from KTalk thread.
            # Generation check inside the lock prevents writing old-meeting audio to the
            # new meeting's WAV: _invalidate_pending_chunks bumps generation BEFORE
            # _on_meeting_change swaps writers under this same lock, so by the time we
            # acquire _wav_lock after a swap, the generation will already differ.
            with self._wav_lock:
                if self._meeting_generation != chunk_gen:
                    return
                if is_mic and self._wav_writer_mic:
                    state.audio_offset = self._wav_writer_mic.get_offset_ms()
                    self._wav_writer_mic.write_chunk(chunk)
                elif not is_mic and self._wav_writer_system:
                    state.audio_offset = self._wav_writer_system.get_offset_ms()
                    self._wav_writer_system.write_chunk(chunk)

            # Пропускаем запись микрофона если на KTalk встрече и mic замьючен
            # Проверяем ДО ASR inference чтобы не тратить CPU на muted chunks
            if is_mic and self._ktalk_bridge and self._ktalk_bridge.is_local_mic_muted():
                return

            # VAD: пропускаем тишину (не тратим CPU на inference)
            rms = np.sqrt(np.mean(chunk ** 2))
            if rms < VAD_RMS_THRESHOLD:
                return

            text = self._transcribe_direct(chunk)
            if not text:
                return

            text = deduplicate(text, state.last_words)
            if not text:
                return

            # Bail out if meeting changed during ASR — prevents stale
            # state updates (last_words/last_speaker) and transcript sends
            # from leaking across meeting boundaries
            if self._meeting_generation != chunk_gen:
                return

            state.last_words = text.split()[-5:]

            current_time = time.time()
            if self._meeting_storage:
                silence_duration = current_time - state.last_text_time
                if silence_duration >= SILENCE_SPLIT_TIMEOUT:
                    self._meeting_storage.split_on_silence_timeout()
                    # Reset speaker state after silence split to prevent
                    # stale speaker from being reused in new meeting segment
                    state.last_speaker = None
                    state.last_words = []
                state.last_text_time = current_time

            timestamp = datetime.now().strftime("%H:%M:%S")
            chunk_time = current_time

            if self.transcript_lines:
                sys.stdout.write("\n")

            # Микрофон: без диаризации, фиксированное имя
            if is_mic:
                time_gap = chunk_time - state.last_speech_time
                segment_duration = chunk_time - state.segment_start_time
                is_continuation = (state.last_speaker == mic_speaker_name and time_gap < speaker_turn_gap and segment_duration < max_segment_duration)

                state.last_speaker = mic_speaker_name
                state.last_speech_time = chunk_time
                if not is_continuation:
                    state.segment_start_time = chunk_time

                # Markdown формат: **Speaker** *[HH:MM:SS]*: text
                line = f"\n**{mic_speaker_name}** *[{timestamp}]*: {text}\n"
                sys.stdout.write(line)
                sys.stdout.flush()
                self.transcript_lines.append(line)

                # Отправляем транскрипт микрофона в Chrome extension (is_local=True)
                if self._ktalk_bridge:
                    self._ktalk_bridge.send_transcript(mic_speaker_name, text, timestamp, is_continuation, is_local=True)

                # Сохраняем в MeetingStorage (skip if meeting changed mid-chunk)
                if self._meeting_storage and self._meeting_generation == chunk_gen:
                    self._meeting_storage.append_segment(
                        speaker=mic_speaker_name,
                        speaker_id="local",
                        text=text,
                        is_local=True,
                        is_continuation=is_continuation,
                        uncertain=False,
                        uncertain_reason="",
                        audio_offset_ms=state.audio_offset
                    )

            # System audio: speaker attribution via KTalk extension
            elif self.use_ktalk:
                # Speaker assignment fallback chain (see ADR / plan for rationale):
                #
                #   1. dominant_speaker_for_period — time-window voting from Jitsi events
                #   2. get_current_speaker — point-in-time with 3 s memory
                #   3. last_speaker TTL — reuse recent speaker if <10 s and not muted
                #   4. sole_unmuted_remote — exactly 1 remote mic on → likely speaker
                #      (marked uncertain; avoids false attribution when 2+ are unmuted)
                #   5. "Unknown" — no attribution possible
                #
                # Why sole_unmuted fallback exists: Jitsi's _activeSpeaker$ event lags
                # 1-5 s behind actual speech. When Speaker A mutes and Speaker B starts,
                # the extension filters out stale A (muted) but Jitsi hasn't switched to
                # B yet, leaving a gap with no positive signal. If only B is unmuted, we
                # can safely attribute speech to B. Remaining edge cases are caught by
                # retroactive correction (see _retroactive_correct_unknown_run).
                ktalk_speaker = None
                if self._ktalk_bridge:
                    ktalk_speaker = self._ktalk_bridge.get_dominant_speaker_for_period(
                        chunk_start_time, chunk_end_time
                    )
                    if not ktalk_speaker:
                        ktalk_speaker = self._ktalk_bridge.get_current_speaker()
                else:
                    ktalk_speaker = self._ktalk_current_speaker
                sole_unmuted_fallback = False
                if ktalk_speaker:
                    speaker = ktalk_speaker
                    no_speaker_yet = False
                elif (state.last_speaker
                      and (chunk_time - state.last_speech_time) < LAST_SPEAKER_TTL
                      and not (self._ktalk_bridge and self._ktalk_bridge.is_speaker_muted(state.last_speaker))):
                    speaker = state.last_speaker
                    no_speaker_yet = False
                elif self._ktalk_bridge and (sole_unmuted := self._ktalk_bridge.get_sole_unmuted_remote_participant()):
                    speaker = sole_unmuted
                    no_speaker_yet = False
                    sole_unmuted_fallback = True
                else:
                    speaker = "Unknown"
                    no_speaker_yet = True

                time_gap = chunk_time - state.last_speech_time
                segment_duration = chunk_time - state.segment_start_time
                is_continuation = (speaker == state.last_speaker and time_gap < speaker_turn_gap and segment_duration < max_segment_duration)

                # Only update last_speaker with confidently identified speakers;
                # "Unknown" would poison the TTL fallback window, and
                # sole_unmuted is uncertain — propagating it via TTL would
                # lose the uncertainty flag and cascade into retroactive rewrites.
                if speaker != "Unknown" and not sole_unmuted_fallback:
                    state.last_speaker = speaker
                state.last_speech_time = chunk_time
                if not is_continuation:
                    state.segment_start_time = chunk_time

                # Markdown формат: **Speaker** *[HH:MM:SS]*: text
                line = f"\n**{speaker}** *[{timestamp}]*: {text}\n"
                sys.stdout.write(line)
                sys.stdout.flush()
                self.transcript_lines.append(line)
                transcript_line_idx = len(self.transcript_lines) - 1

                # Отправляем транскрипт в Chrome extension
                # System audio = всегда remote speaker (is_local=False)
                if self._ktalk_bridge:
                    is_local = False  # System audio always from remote speaker
                    all_mics_muted = self._ktalk_bridge.are_all_mics_muted()

                    # Определяем неопределённость и её причину
                    # Приоритет: no_speaker > sole_unmuted_fallback > all_mics_muted
                    uncertain = False
                    uncertain_reason = ""
                    if no_speaker_yet:
                        # Ещё не было информации о спикере от extension
                        uncertain = True
                        uncertain_reason = "no_speaker"
                    elif sole_unmuted_fallback:
                        # Speaker inferred from sole unmuted participant, not confirmed by Jitsi
                        uncertain = True
                        uncertain_reason = "sole_unmuted_fallback"
                    elif all_mics_muted:
                        # Все микрофоны выключены - непонятно кто говорит
                        uncertain = True
                        uncertain_reason = "mics_off"

                    self._ktalk_bridge.send_transcript(speaker, text, timestamp, is_continuation, is_local=is_local, uncertain=uncertain, uncertain_reason=uncertain_reason)

                    # Сохраняем в MeetingStorage (skip if meeting changed mid-chunk)
                    meeting_seg_id = 0
                    if self._meeting_storage and self._meeting_generation == chunk_gen:
                        meeting_seg_id = self._meeting_storage.append_segment(
                            speaker=speaker,
                            speaker_id=speaker,  # Используем имя как ID для KTalk
                            text=text,
                            is_local=is_local,
                            is_continuation=is_continuation,
                            uncertain=uncertain,
                            uncertain_reason=uncertain_reason,
                            audio_offset_ms=state.audio_offset
                        )

                    # Track segment for retroactive correction.
                    # Lock guards against _invalidate_pending_chunks racing
                    # between the generation check and the deque append;
                    # without the lock a meeting switch can clear the deque
                    # after we read the generation but before we append.
                    needs_correction = False
                    correction_snapshot = None
                    with self._state_lock:
                        if self._meeting_generation == chunk_gen:
                            self._recent_system_segments.append(RecentSegment(
                                speaker=speaker,
                                chunk_time=chunk_time,
                                line_idx=transcript_line_idx,
                                meeting_segment_id=meeting_seg_id,
                                is_local=False,
                                timestamp_display=timestamp,
                                text=text,
                            ))

                            # Retroactive correction: if we just identified a speaker
                            # after a run of Unknown segments, correct them backwards
                            needs_correction = (
                                speaker != "Unknown"
                                and len(self._recent_system_segments) >= 2
                                and self._recent_system_segments[-2].speaker == "Unknown"
                            )
                            # Snapshot segments and generation inside the lock so
                            # a concurrent _invalidate_pending_chunks cannot clear
                            # the deque between here and the correction method.
                            if needs_correction:
                                correction_snapshot = (
                                    list(self._recent_system_segments),
                                    self._meeting_generation,
                                    self._meeting_storage.current_meeting_path
                                    if self._meeting_storage else None,
                                )

                    if needs_correction:
                        self._retroactive_correct_unknown_run(
                            speaker, chunk_time,
                            correction_snapshot=correction_snapshot,
                        )

            elif self.no_diarization:
                # Markdown формат: **Speaker** *[HH:MM:SS]*: text
                line = f"\n**speaker** *[{timestamp}]*: {text}\n"
                sys.stdout.write(line)
                sys.stdout.flush()
                self.transcript_lines.append(line)

                if self._meeting_storage and self._meeting_generation == chunk_gen:
                    self._meeting_storage.append_segment(
                        speaker="speaker",
                        speaker_id="speaker",
                        text=text,
                        is_local=is_mic,
                        uncertain=True,
                        uncertain_reason="no_diarization",
                        audio_offset_ms=state.audio_offset
                    )
            else:
                # Placeholder для async diarization
                draft = f"\n**...** *[{timestamp}]*: {text}\n"
                sys.stdout.write(draft)
                sys.stdout.flush()
                self.transcript_lines.append(draft)

                chunk_copy = chunk.copy()

                def get_speaker(c):
                    emb = self._get_embedding(c)
                    return self._identify_speaker(emb)

                future = executor.submit(get_speaker, chunk_copy)
                with self._state_lock:
                    state.pending_updates.append((len(self.transcript_lines) - 1, timestamp, text, future, chunk_time, state.audio_offset, chunk_gen))

        try:
            while self.running:
                time.sleep(0.1)

                # Обрабатываем pending updates
                if self.mic_capture:
                    process_pending(mic_state, is_mic=True)
                if self.system_capture:
                    process_pending(sys_state, is_mic=False)

                # Обрабатываем новые чанки
                if self.mic_capture:
                    process_chunk(self.mic_capture, mic_state, is_mic=True)
                if self.system_capture:
                    process_chunk(self.system_capture, sys_state, is_mic=False)

        except KeyboardInterrupt:
            pass
        except Exception as e:
            print(f"\n[FATAL ERROR] {e}", file=sys.stderr)
            import traceback
            traceback.print_exc()

        finally:
            print("\n" + "-" * 60)
            print("Остановлено.")

            self.running = False
            executor.shutdown(wait=False, cancel_futures=True)

            if self.mic_capture:
                self.mic_capture.stop()
            if self.system_capture:
                self.system_capture.stop()

            # Finalize audio recordings
            mic_audio_path = None
            system_audio_path = None
            with self._wav_lock:
                if self._wav_writer_mic:
                    self._wav_writer_mic.finalize()
                    mic_audio_path = str(self._wav_writer_mic.path)
                    print(f"Mic audio saved: {mic_audio_path}")
                if self._wav_writer_system:
                    self._wav_writer_system.finalize()
                    system_audio_path = str(self._wav_writer_system.path)
                    print(f"System audio saved: {system_audio_path}")

            # Store audio paths in meeting meta
            if self._meeting_storage and (mic_audio_path or system_audio_path):
                self._meeting_storage.set_audio_paths(mic_audio_path, system_audio_path)

            # Stop KTalk bridge
            if self._ktalk_bridge:
                self._ktalk_bridge.stop()

            # End meeting session
            if self._meeting_storage:
                meeting_id = self._meeting_storage.end_session()
                if meeting_id:
                    print(f"Meeting saved: {meeting_id}")

            if self.output_file and self.transcript_lines:
                with open(self.output_file, 'w', encoding='utf-8') as f:
                    f.write("\n".join(self.transcript_lines))
                print(f"Транскрипт сохранён в {self.output_file}")

            if force_exit:
                import os
                os._exit(0)

    def _invalidate_pending_chunks(self):
        """Bump generation and clear pending state on meeting boundary.

        Called from KTalk bridge thread via pre_meeting_switch callback,
        BEFORE MeetingStorage changes current_meeting_path.
        This ensures process_pending and direct paths discard in-flight chunks
        that were captured for the old meeting.

        Also clears capture buffers so that stale audio accumulated since the
        last get_chunk() call does not leak into the new meeting.
        """
        with self._state_lock:
            self._meeting_generation += 1
            self._recent_system_segments.clear()
            if hasattr(self, '_mic_state'):
                for state in (self._mic_state, self._sys_state):
                    for pending in state.pending_updates:
                        pending[3].cancel()  # pending[3] is the Future
                    state.last_speaker = None
                    state.last_words = []
                    state.segment_start_time = 0
                    state.pending_updates = []
        # Clear capture buffers so the next get_chunk() returns only
        # audio recorded AFTER the meeting switch, not stale old-meeting
        # samples that accumulated since the last drain.
        if self.mic_capture:
            self.mic_capture.clear_buffer()
        if self.system_capture:
            self.system_capture.clear_buffer()

    def _on_meeting_change(self, meeting_id: str, meeting_path: Path):
        """Called after meeting switch to reinitialize audio recording."""
        if not self.save_audio:
            return

        with self._wav_lock:
            # Finalize current wav writers and persist audio paths for outgoing meeting
            mic_audio_path_old = None
            system_audio_path_old = None
            if self._wav_writer_mic:
                self._wav_writer_mic.finalize()
                mic_audio_path_old = str(self._wav_writer_mic.path)
                print(f"Mic audio saved: {mic_audio_path_old}")
                self._wav_writer_mic = None
            if self._wav_writer_system:
                self._wav_writer_system.finalize()
                system_audio_path_old = str(self._wav_writer_system.path)
                print(f"System audio saved: {system_audio_path_old}")
                self._wav_writer_system = None

            # Persist audio paths for the outgoing meeting.
            # Note: MeetingStorage.current_meeting_path already points to the NEW meeting
            # (switched by on_meeting_join before this callback), so we write meta directly
            # using the WAV file's parent directory which IS the old meeting's directory.
            old_meeting_path = None
            if mic_audio_path_old:
                old_meeting_path = Path(mic_audio_path_old).parent
            elif system_audio_path_old:
                old_meeting_path = Path(system_audio_path_old).parent
            if old_meeting_path:
                import json
                meta_path = old_meeting_path / "meta.json"
                if meta_path.exists():
                    meta = json.loads(meta_path.read_text(encoding="utf-8"))
                    if mic_audio_path_old:
                        meta["audio_mic_path"] = mic_audio_path_old
                    if system_audio_path_old:
                        meta["audio_system_path"] = system_audio_path_old
                    meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")

            # Open new wav writers for new meeting.
            # continuation=False: _invalidate_pending_chunks() already cleared the
            # capture buffers, so there is no overlap from the previous chunk.
            # Using continuation=True here would incorrectly skip the first 0.5s
            # of fresh audio (the overlap that no longer exists after clear_buffer).
            if self.mic_capture:
                mic_audio_path = meeting_path / "audio_mic.wav"
                self._wav_writer_mic = IncrementalWavWriter(mic_audio_path, self.sample_rate)
                self._wav_writer_mic.open(continuation=False)
                print(f"Recording mic audio: {mic_audio_path}")
            if self.system_capture:
                system_audio_path = meeting_path / "audio_system.wav"
                self._wav_writer_system = IncrementalWavWriter(system_audio_path, self.sample_rate)
                self._wav_writer_system.open(continuation=False)
                print(f"Recording system audio: {system_audio_path}")


def live_pipeline(
    mic_device=None,
    system_device=None,
    speaker_db_path=None,
    chunk_duration=5.0,
    output_file=None,
    no_diarization=False,
    list_devices=False,
    use_ktalk=False,
    use_screencapture=False,
    mic_speaker_name=MIC_SPEAKER_NAME
):
    """Пайплайн live транскрипции."""
    if list_devices:
        LiveAudioCapture.list_devices()
        return

    # Автоопределение устройств
    if mic_device == "auto":
        mic_device = SingleDeviceCapture.find_builtin_mic()
        if mic_device is not None:
            import sounddevice as sd
            name = sd.query_devices(mic_device)['name']
            print(f"Auto-detected mic: {name} (device {mic_device})", file=sys.stderr)
        else:
            print("Warning: Could not find built-in microphone", file=sys.stderr)

    if not use_screencapture and system_device == "auto":
        system_device = SingleDeviceCapture.find_blackhole()
        if system_device is not None:
            import sounddevice as sd
            name = sd.query_devices(system_device)['name']
            print(f"Auto-detected system audio: {name} (device {system_device})", file=sys.stderr)
        else:
            print("Warning: Could not find BlackHole device", file=sys.stderr)

    transcriber = LiveTranscriber(
        mic_device=mic_device,
        system_device=system_device if not use_screencapture else None,
        speaker_db_path=speaker_db_path,
        chunk_duration=chunk_duration,
        output_file=output_file,
        no_diarization=no_diarization,
        use_ktalk=use_ktalk,
        use_screencapture=use_screencapture,
        mic_speaker_name=mic_speaker_name
    )
    transcriber.run(force_exit=True)
