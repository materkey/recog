"""
Модуль live транскрипции (микрофон, системный звук, KTalk).
"""

import struct
import subprocess
import sys
import time
import threading
from datetime import datetime
from pathlib import Path
from typing import Optional, Tuple

import numpy as np
import torch

from collections import deque
from dataclasses import dataclass

from .config import (SPEAKER_SIMILARITY_THRESHOLD, VAD_RMS_THRESHOLD, SILENCE_SPLIT_TIMEOUT,
                     LAST_SPEAKER_TTL, MIC_SPEAKER_NAME,
                     RETROACTIVE_MAX_SEGMENTS, RETROACTIVE_MAX_SECONDS,
                     SPEAKER_TURN_GAP, MAX_SEGMENT_DURATION,
                     PCM_SILENCE_WATCHDOG_S, PCM_REFINE_WINDOW_S,
                     PCM_WAV_ANCHOR_MAX_AGE_MS,
                     DEDUP_MAX_GAP_S, ASR_PENDING_BUFFER_CAP_S)
from .asr import ASRModule, _mps_autoreleasepool
from .audio_capture import BufferedAudioCapture
from .log_utils import log_rate_limited
from .remote_asr import ASRTemporarilyUnavailable, build_asr
from .refine import RefineResult, RefineWorker, make_abortable_decode
from .speaker_db import SpeakerDatabase
from .config import get_data_dir
from .user_profile import UserProfileStore, resolve_identity

# WAV stores RIFF/data sizes as 32-bit values — a data chunk beyond ~4 GiB
# corrupts the header. Cap at 2 GiB (safe for signed-int32 parsers too);
# at 16 kHz mono int16 that is ~18.6 h of continuous audio per meeting.
MAX_WAV_DATA_BYTES = 2**31

# How often the live loop rotates old meeting audio (mtime-based 24h
# retention in MeetingStorage.cleanup_old_audio — a stat-only scan).
AUDIO_CLEANUP_INTERVAL_S = 3600

# `--mic-device none`: явный отказ от микрофона. Отличается от None, который
# означает «устройство ввода по умолчанию» для sounddevice.
NO_MIC = "none"

# Спека системного звука без источника (та же строка, что понимает
# recog.system_audio.resolve_system_audio_backend).
NO_SYSTEM_AUDIO = "none"

PACTL_TIMEOUT_S = 2.0


def normalize_system_audio(system_audio=None, system_device=None,
                           use_screencapture: bool = False) -> str:
    """Одна спека системного звука из нового флага и старых аргументов.

    Приоритет `--system-audio` > `--screencapture` > `--system-device`
    держится здесь, чтобы CLI и прямые вызовы `LiveTranscriber` не разошлись.
    """
    if system_audio:
        return system_audio
    if use_screencapture:
        return "screencapture"
    if system_device is not None:
        return f"device:{system_device}"
    return NO_SYSTEM_AUDIO


def parse_system_audio_device(spec: str):
    """Устройство из спеки `device:<id>`; None для остальных бэкендов."""
    if not spec.startswith("device:"):
        return None
    device = spec[len("device:"):]
    try:
        return int(device)
    except ValueError:
        return device


class IncrementalWavWriter:
    """
    Writes WAV file incrementally during live capture.
    Supports streaming write of audio chunks without loading entire file in memory.
    Handles overlap by skipping duplicate samples at chunk boundaries.
    """

    def __init__(self, path: Path, sample_rate: int = 16000, channels: int = 1, overlap_seconds: float = 0.5,
                 max_data_bytes: int = MAX_WAV_DATA_BYTES):
        self.path = Path(path)
        self.sample_rate = sample_rate
        self.channels = channels
        self._file = None
        self._samples_written = 0
        # Overlap handling: chunks from get_chunk() include overlap for ASR quality,
        # but we should only write non-overlapping samples to avoid duplication
        self._overlap_samples = int(overlap_seconds * sample_rate)
        self._first_chunk = True
        self._max_data_bytes = max_data_bytes
        self._cap_warned = False

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

    def write_chunk(self, chunk: np.ndarray) -> Optional[int]:
        """
        Appends audio chunk to file, skipping overlap samples to avoid duplication.

        Args:
            chunk: Audio samples as float32 array (values in [-1, 1])

        Returns:
            Number of head samples skipped (0 for the first chunk,
            _overlap_samples otherwise), or None if nothing was written
            (writer not open, empty remainder after the overlap cut, or
            size cap reached). Callers use this to anchor the written
            audio's wall-clock time to its WAV position.
        """
        if self._file is None:
            return None

        # Skip overlap samples (except for first chunk) to avoid audio duplication
        # Chunks from get_chunk() include 0.5s overlap for ASR quality,
        # but we only write non-overlapping portion to WAV
        if self._first_chunk:
            self._first_chunk = False
            skipped_samples = 0
            samples_to_write = chunk
        else:
            # Skip first overlap_samples - they were already written in previous chunk
            skipped_samples = self._overlap_samples
            samples_to_write = chunk[self._overlap_samples:]

        if len(samples_to_write) == 0:
            return None

        # WAV format limit: sizes are 32-bit, so stop before the data chunk
        # would exceed the cap instead of silently corrupting the header.
        if (self._samples_written + len(samples_to_write)) * 2 > self._max_data_bytes:
            if not self._cap_warned:
                self._cap_warned = True
                print(
                    f"Warning: {self.path.name} reached max WAV size "
                    f"({self._max_data_bytes} bytes), stopping audio recording",
                    file=sys.stderr,
                )
            return None

        # Convert float32 [-1, 1] to int16
        chunk_int16 = (samples_to_write * 32767).astype(np.int16)
        self._file.write(chunk_int16.tobytes())
        self._samples_written += len(samples_to_write)
        # Keep the header valid after every write: a SIGKILL must not leave a
        # WAV whose header claims 0 data bytes (strict parsers reject it, and
        # finalize() may never run). Two in-place 4-byte patches per chunk.
        self._patch_header_sizes()
        return skipped_samples

    def mark_discontinuity(self):
        """Следующий чанк писать целиком: поток захвата прервался.

        Срез overlap исходит из того, что первые 0.5 с чанка уже записаны
        предыдущим. После разрыва (сменился источник системного звука) буфер
        захвата сброшен, и эти 0.5 с — новое аудио: срезать их значит
        выкинуть из WAV полсекунды речи.
        """
        self._first_chunk = True

    def _patch_header_sizes(self):
        """Update RIFF/data size fields in place, preserving file position."""
        data_size = self._samples_written * 2
        pos = self._file.tell()
        self._file.seek(4)
        self._file.write(struct.pack('<I', 36 + data_size))
        self._file.seek(40)
        self._file.write(struct.pack('<I', data_size))
        self._file.seek(pos)

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


class SingleDeviceCapture(BufferedAudioCapture):
    """Захват аудио с одного устройства."""

    def __init__(self, device, sample_rate=16000, name="device",
                 max_buffer_s: float = ASR_PENDING_BUFFER_CAP_S):
        import sounddevice as sd
        super().__init__(sample_rate=sample_rate, name=name, max_buffer_s=max_buffer_s)
        self.sd = sd
        self.device = device
        self.stream = None

    def _callback(self, indata, frames, time_info, status):
        if status:
            print(f"Audio status ({self.name}): {status}", file=sys.stderr)
        captured_at = time.time() - frames / self.sample_rate
        with self._lock:
            dropped = self._append_locked(indata.copy(), captured_at)
        if dropped:
            log_rate_limited(
                f"capture-overflow-{self.name}",
                f"[SingleDeviceCapture:{self.name}] ASR unavailable too long — "
                f"dropped {dropped / self.sample_rate:.1f}s of oldest audio",
            )

    def start(self):
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

    @staticmethod
    def list_devices(platform: str = "", run=subprocess.run):
        import sounddevice as sd
        print("Доступные аудио устройства:")
        print(sd.query_devices())
        platform = platform or sys.platform
        if platform.startswith("linux"):
            SingleDeviceCapture.print_pulse_sources(run=run)

    @staticmethod
    def print_pulse_sources(run=subprocess.run):
        """Источники PulseAudio/PipeWire — их имена не видны в PortAudio.

        `--system-audio pulse` берёт монитор sink'а, а не PortAudio-устройство,
        поэтому список из `sd.query_devices()` для него бесполезен.
        """
        try:
            result = run(["pactl", "list", "short", "sources"],
                         capture_output=True, text=True, timeout=PACTL_TIMEOUT_S)
        except Exception as e:
            print(f"\n(pactl недоступен: {e})", file=sys.stderr)
            return
        if getattr(result, "returncode", 1) != 0:
            print(f"\n(pactl list short sources завершился с кодом {result.returncode})",
                  file=sys.stderr)
            return
        print()
        print("Источники PulseAudio/PipeWire (pactl list short sources):")
        print((getattr(result, "stdout", "") or "").rstrip())

    @staticmethod
    def find_default_mic(platform: str = ""):
        """Устройство ввода по умолчанию для текущей ОС.

        macOS: встроенный микрофон MacBook (как раньше). Linux: устройство
        PortAudio, проксирующее звуковой сервер (`pulse`/`pipewire`/`default`)
        — оно само следует за дефолтным источником сервера. Дальше —
        дефолтный вход PortAudio, и только потом None.
        """
        import sounddevice as sd
        devices = sd.query_devices()
        platform = platform or sys.platform

        if platform == "darwin":
            for i, dev in enumerate(devices):
                name = dev['name'].lower()
                # Ищем встроенный микрофон MacBook
                if 'macbook' in name and ('microphone' in name or 'микрофон' in name) and dev['max_input_channels'] > 0:
                    return i
            # Fallback: любое устройство с "built-in" или "internal"
            for i, dev in enumerate(devices):
                name = dev['name'].lower()
                if ('built-in' in name or 'internal' in name) and dev['max_input_channels'] > 0:
                    return i

        for preferred in ("pulse", "pipewire", "default"):
            for i, dev in enumerate(devices):
                if dev['name'].strip().lower() == preferred and dev['max_input_channels'] > 0:
                    return i

        try:
            return sd.query_devices(kind="input")["index"]
        except Exception:
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


@dataclass
class SourceState:
    """Tracks per-source (mic / system) state during live transcription."""
    pending_updates: list = None
    last_words: list = None
    last_speaker: str = None
    last_speech_time: float = 0
    segment_start_time: float = 0
    last_text_time: float = 0
    audio_offset: int = 0
    # Capture-время (epoch) конца чанка, давшего последний записанный сегмент.
    # Дедуп применяется только если новый чанк начинается в пределах
    # DEDUP_MAX_GAP_S от этого времени (дизайн §4).
    last_words_capture_end: float = 0.0
    # Однослотовый retry: чанк, на котором ASR кинул ASRTemporarilyUnavailable
    # (гонка первого сбоя). Хранится как (chunk, capture_start_ts, generation)
    # и диспатчится первым при следующем _process_chunk с доступным ASR —
    # тот же паттерн, что _pending_retry_batch в PerSpeakerIngest. Чанк уже
    # записан в WAV первой попыткой, поэтому ретрай запись пропускает.
    pending_chunk: Optional[tuple] = None

    def __post_init__(self):
        if self.pending_updates is None:
            self.pending_updates = []
        if self.last_words is None:
            self.last_words = []
        if self.last_text_time == 0:
            self.last_text_time = time.time()


def normalize_word(w: str) -> str:
    """Strip punctuation and lowercase a word for deduplication comparison."""
    return ''.join(c.lower() for c in w if c.isalnum())


def deduplicate(text: str, prev_words: list) -> str | None:
    """Remove leading overlap between *text* and *prev_words*.

    Returns ``None`` when the entire text is a duplicate.
    """
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


class LiveTranscriber:
    """Координатор live транскрипции."""

    def __init__(
        self,
        mic_device=None,
        system_audio=None,  # auto|screencapture|pulse|device:<id>|none
        speaker_db_path=None,
        chunk_duration=3.0,  # Баланс между задержкой и качеством
        output_file=None,
        no_diarization=False,
        use_ktalk=False,  # Использовать KTalk bridge вместо embedding-based diarization
        save_audio=False,  # Сохранять аудио для последующей диаризации
        mic_speaker_name=None,
        system_device=None,  # legacy-алиас: device:<id>
        use_screencapture=False,  # legacy-алиас: screencapture
    ):
        from recog.system_audio import (create_system_capture,
                                        resolve_system_audio_backend)

        # `auto` разрешается в конкретный бэкенд здесь, чтобы прямые вызовы
        # (тесты, e2e) вели себя так же, как CLI.
        self.system_audio = resolve_system_audio_backend(
            normalize_system_audio(system_audio, system_device, use_screencapture)
        )
        self.system_device = parse_system_audio_device(self.system_audio)

        # `none` — явный отказ от микрофона; None — устройство по умолчанию.
        self.mic_enabled = mic_device != NO_MIC
        self.mic_device = mic_device if self.mic_enabled else None
        self.sample_rate = 16000
        self.save_audio = save_audio
        # Per-speaker PCM transport is the only ingest path for KTalk
        # meetings. Without KTalk there is no extension to source per-pid
        # PCM from, so the path is implicitly off.
        self.use_pcm_transport = bool(use_ktalk)

        # Создаём capture устройства
        self.mic_capture = None
        self.system_capture = None

        if self.system_audio in ("screencapture", "pulse"):
            self.system_capture = create_system_capture(self.system_audio, self.sample_rate)
            if self.mic_enabled and self.mic_device is not None:
                self.mic_capture = SingleDeviceCapture(self.mic_device, self.sample_rate, "mic")
        elif self.system_device is not None:
            self.system_capture = SingleDeviceCapture(self.system_device, self.sample_rate, "system")
            if self.mic_enabled and self.mic_device is not None:
                self.mic_capture = SingleDeviceCapture(self.mic_device, self.sample_rate, "mic")
        elif self.mic_enabled:
            self.mic_capture = SingleDeviceCapture(self.mic_device, self.sample_rate, "mic")

        if self.mic_capture is None and self.system_capture is None:
            raise ValueError(
                "нечего захватывать: микрофон отключён (--mic-device none) "
                "и системный звук выключен (--system-audio none)"
            )

        # Определяем режим: dual (оба), mic-only, system-only
        self.dual_mode = (self.mic_capture is not None and self.system_capture is not None)

        self.asr = build_asr()
        self.speaker_db = SpeakerDatabase(speaker_db_path) if speaker_db_path else None
        self.chunk_duration = chunk_duration
        self.output_file = output_file
        self.no_diarization = no_diarization
        self.use_ktalk = use_ktalk
        self._mic_speaker_override = mic_speaker_name
        self._user_profile_store = UserProfileStore(get_data_dir())
        self._user_identity = self._read_user_identity()
        self.mic_speaker_name = self._user_identity["local_user_name"]
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

        # Recent system-audio segments for retroactive correction of Unknown runs
        self._recent_system_segments: deque[RecentSegment] = deque(maxlen=RETROACTIVE_MAX_SEGMENTS)

        # Audio recording for on-demand diarization
        self._wav_writer_mic: Optional[IncrementalWavWriter] = None
        self._wav_writer_system: Optional[IncrementalWavWriter] = None
        self._wav_lock = threading.Lock()  # Guards _wav_writer_mic/_wav_writer_system
        # Wall-clock↔WAV anchor for PCM-primary offsets: (wall_ms of the first
        # sample actually written by the last system-audio write, WAV offset_ms
        # before that write). PCM segments extrapolate their audio_offset_ms
        # from this anchor instead of `wav_offset_now - chunk_age`, which drifts
        # because the system WAV advances in ~chunk_duration steps (sawtooth lag)
        # while chunk age runs on a continuous clock. Guarded by _wav_lock.
        self._system_wav_anchor: Optional[Tuple[float, int]] = None
        # Снимок source_epoch системного захвата: расхождение = разрыв потока
        # (смена default sink), после которого чанк пишется в WAV целиком.
        self._system_source_epoch = 0
        self._state_lock = threading.Lock()  # Guards per-source state during meeting transitions
        self._meeting_generation = 0  # Incremented on meeting change; used to discard stale chunks
        self._mic_state: Optional[SourceState] = None
        self._sys_state: Optional[SourceState] = None

        # Per-speaker PCM path state (see plan §4.2).  Populated only when
        # `use_pcm_transport=True` and KTalk bridge is set up.
        self._per_speaker_ingest = None  # type: ignore[assignment]
        self._pcm_prev_words: dict[str, list] = {}   # pid -> last N words for dedup
        self._pcm_pid_state: dict[str, dict] = {}    # pid -> {last_speech_time, segment_start_time}
        # Per-pid live-segment registry for refine alignment (Task 8 of
        # docs/plans/2026-04-24-per-speaker-refine-backend.md). Each entry:
        # (segment_id, pid_audio_ms_start, pid_audio_ms_end). Populated by
        # `_register_pcm_live_segment` after append_segment; consumed by
        # `_apply_refine` (Task 9) to map refined word timestamps back to the
        # live segment IDs in transcript.jsonl. All reads/writes under
        # `self._state_lock`.
        self._pcm_live_segments: dict[str, list[tuple[int, float, float]]] = {}
        # Latest text per live segment_id — cache used by `_apply_refine`
        # (Task 9) to skip no-op writes when refined text is byte-identical
        # to what's already in the JSONL. Avoids JSONL re-reads on every
        # refine. Populated in `_on_pcm_batch` alongside
        # `_register_pcm_live_segment`; pruned together with the segment
        # registry. All reads/writes under `self._state_lock`.
        self._pcm_segment_texts: dict[int, str] = {}
        # Refine worker placeholder — `_setup_refine_worker` builds it and
        # wires `apply_fn=self._apply_refine`. Kept as None here so
        # `_teardown_session` / `_on_pcm_batch` priority signals can
        # no-op cleanly until the worker is constructed.
        self._refine_worker = None
        # Tracks the last-observed watchdog state so we emit a transition
        # marker only on actual flips (see plan §4.5).
        self._last_pcm_degraded: bool = False
        # Захват системного звука сдался (исчерпал рестарты) — до конца
        # процесса это необратимо, но каждая встреча, оставшаяся без
        # системного звука, должна получить свой маркер. Ключ — встреча, в
        # которую маркер уже записан: reconnect в ту же встречу второго не
        # добавит, а любая новая (в том числе orphan-сплит по тишине, идущий
        # мимо `_invalidate_pending_chunks`) получит собственный.
        self._system_capture_lost: bool = False
        self._system_capture_lost_meeting = None

    def _read_user_identity(self) -> dict:
        return resolve_identity(
            self._user_profile_store.read(),
            legacy_name=MIC_SPEAKER_NAME,
            override=self._mic_speaker_override,
        )

    def _apply_user_identity(self, identity: dict) -> None:
        # Called under the bridge lock, before publishing the same snapshot.
        self.mic_speaker_name = identity["local_user_name"]
        self._user_identity = identity

    def system_audio_label(self) -> str:
        """Человекочитаемое имя источника системного звука для баннера."""
        if self.system_audio == "screencapture":
            return "ScreenCaptureKit"
        if self.system_audio == "pulse":
            return "PulseAudio/PipeWire (parec)"
        if self.system_device is not None:
            return str(self.system_device)
        return "none"

    def _asr_available(self) -> bool:
        """True, когда ASR может обслужить запрос прямо сейчас.

        FallbackASR выставляет .available (False = remote упал И локаль ещё
        грузится — буферы трогать нельзя); чистый ASRModule свойства не имеет
        и всегда доступен.
        """
        return bool(getattr(self.asr, "available", True))

    def _transcribe_direct(self, audio_chunk: np.ndarray) -> Optional[str]:
        """Транскрибирует напрямую через tensor без temp файлов.

        ASRTemporarilyUnavailable пробрасывается наверх (caller обязан сохранить
        чанк в state.pending_chunk для ретрая). Любое другое исключение — лог +
        None (best-effort, контракт прежний).
        """
        try:
            # Serialize with PerSpeakerIngest worker's transcribe_batch: both
            # paths share self.asr.model, and PyTorch modules are not safe for
            # concurrent forward on a shared instance. Signal live priority
            # before contending so cooperative refine aborts promptly instead
            # of making mic/fallback live ASR wait behind a full refine pass.
            refine_worker = getattr(self, "_refine_worker", None)
            if refine_worker is not None:
                refine_worker.set_live_pending()
            try:
                # The forward+decode body lives in ASRModule.transcribe_chunk so
                # the remote-offload FallbackASR can route this path too; the
                # _model_lock / inference_mode / autoreleasepool live inside.
                return self.asr.transcribe_chunk(audio_chunk)
            finally:
                if refine_worker is not None:
                    refine_worker.clear_live_pending()

        except ASRTemporarilyUnavailable:
            raise
        except Exception as e:
            log_rate_limited(
                "live-transcribe-direct",
                f"[LiveTranscriber] _transcribe_direct failed: {type(e).__name__}: {e}",
            )
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
                similarity = SpeakerDatabase.cosine_similarity(embedding, db_emb)
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

    def _resolve_speaker_embedding(self, chunk: np.ndarray) -> str:
        """Get embedding for *chunk* and identify speaker (runs in executor thread)."""
        emb = self._get_embedding(chunk)
        return self._identify_speaker(emb)

    def _ktalk_speaker_callback(self, speaker_name: Optional[str], is_speaking: bool):
        """Callback для обновления текущего спикера из KTalk."""
        self._ktalk_current_speaker = speaker_name if is_speaking else None

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
                    segment_id=seg.meeting_segment_id,
                )

    def _setup_meeting_storage(self):
        """Initialize MeetingStorage and audio recording writers."""
        try:
            from recog.meeting_storage import MeetingStorage
            self._meeting_storage = MeetingStorage()
            meeting_id = self._meeting_storage.start_session(
                mic=self.mic_capture is not None,
                system_audio=self.system_capture is not None,
                ktalk=self.use_ktalk,
                screencapture=self.system_audio == "screencapture",
                system_audio_backend=self.system_audio,
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
            # Fail fast instead of running without persistence: launchd
            # (KeepAlive) restarts the service until storage is usable again.
            # Swallowing the error here (e.g. ENOSPC at startup) used to leave
            # the service transcribing into stdout only, losing every meeting
            # until someone noticed the empty webapp.
            print(f"[FATAL ERROR] MeetingStorage init failed: {e}", file=sys.stderr)
            raise

    def _setup_ktalk_bridge(self):
        """Wire up KTalk bridge, IncrementGenerator, and meeting-change callbacks."""
        if not self.use_ktalk:
            return
        try:
            from recog.ktalk_bridge import KTalkBridge
            self._ktalk_bridge = KTalkBridge()
            self._ktalk_bridge.configure_user_profile(
                self._user_identity, self._read_user_identity, self._apply_user_identity,
            )
            self._ktalk_bridge.set_speaker_callback(self._ktalk_speaker_callback)
            # Connect MeetingStorage to KTalk bridge
            if self._meeting_storage:
                self._ktalk_bridge.set_meeting_storage(self._meeting_storage)
                # Wire IncrementGenerator for auto-summaries on ktalk_leave
                from recog.increment import IncrementGenerator
                self._increment_generator = IncrementGenerator(
                    self._meeting_storage.base_path,
                    prompts_dir=Path(__file__).parent / "prompts",
                )
                self._increment_generator.set_callback(
                    lambda mid, ver, pid: print(
                        f"[increment] ready: {mid} v{ver} ({pid})", file=sys.stderr
                    )
                )
                self._ktalk_bridge.set_increment_generator(self._increment_generator)
            # Register callbacks for meeting changes:
            # pre-switch invalidates in-flight chunks after path changes
            self._ktalk_bridge.set_pre_meeting_switch_callback(self._invalidate_pending_chunks)
            # post-switch reinitializes audio recording at new path
            self._ktalk_bridge.set_meeting_change_callback(self._on_meeting_change)
            # NOTE: bridge.start() is deferred to _start_ktalk_bridge() so that
            # _setup_per_speaker_ingest can wire set_pcm_chunk_callback before
            # the WS server begins accepting connections.  Otherwise a fast
            # extension reconnect could deliver pcm_chunk frames to a None
            # callback (dropped silently at ktalk_bridge.py:1521).
        except ImportError:
            print("Warning: ktalk_bridge module not found, falling back to embedding-based", file=sys.stderr)
            self.use_ktalk = False
        except Exception as e:
            print(f"Warning: KTalk bridge error: {e}, falling back to embedding-based", file=sys.stderr)
            self.use_ktalk = False

    def _setup_per_speaker_ingest(self):
        """Wire per-speaker PCM ingest when KTalk is on.

        Creates `PerSpeakerIngest`, registers its `enqueue` as the bridge's
        PCM callback, and spawns the internal worker thread.  Also gates on
        having a working bridge — `_setup_ktalk_bridge` may flip
        `self.use_ktalk` to False on failure, in which case PCM is a no-op.
        """
        if not (self.use_pcm_transport and self.use_ktalk and self._ktalk_bridge):
            return
        try:
            from .per_speaker import PerSpeakerIngest
        except ImportError as e:
            print(
                f"Warning: per_speaker ingest unavailable ({e}); "
                f"PCM transport disabled for this session",
                file=sys.stderr,
            )
            self.use_pcm_transport = False
            return

        self._per_speaker_ingest = PerSpeakerIngest(
            transcribe_callback=self._on_pcm_batch,
            generation_getter=lambda: self._meeting_generation,
            meeting_active_getter=lambda: bool(
                self._meeting_storage
                and self._meeting_storage.get_current_meeting_id()
            ),
            asr_available_getter=self._asr_available,
        )
        self._ktalk_bridge.set_pcm_chunk_callback(self._per_speaker_ingest.enqueue)
        self._ktalk_bridge.set_pcm_stats_getter(self._per_speaker_ingest.get_stats)
        self._per_speaker_ingest.start()
        print("Per-speaker PCM ingest started")

        # Refine bolts the second-tier contour on top of the ingest.
        # Build + start the worker, then register its submit method so the
        # ingest's tick loop feeds it per-pid refine candidates.
        self._setup_refine_worker()

    def _setup_refine_worker(self) -> None:
        """Build and start `RefineWorker`.

        Notes:
            * `self._per_speaker_ingest` is not None — the ingest owns the
              per-pid ring + trigger state the worker's tasks come from.

        Wiring:
            * `model_lock = self.asr._model_lock` (RLock) — shared with the
              live batched-forward path so cooperative abort in the worker
              yields the model promptly when `_on_pcm_batch` signals
              `set_live_pending()`.
            * `transcribe_fn = self._refine_transcribe_fn` — installs the
              abortable decoder monkey-patch in cooperative mode, then runs
              `asr.transcribe_with_timestamps`. Restores the decoder on
              the way out.
            * `apply_fn = self._apply_refine` — executed OUTSIDE the model
              lock; takes its own locks for JSONL rewrite (`MeetingStorage._
              segment_lock`) and live state (`self._state_lock`).
            * `abort_mode = "cooperative"` — the default from the spike
              (see `docs/experiments/2026-04-24-refine-abort-spike.md`); saves
              ~77 % of inference cost per aborted refine on MPS.
        """
        if self._per_speaker_ingest is None:
            return
        self._refine_worker = RefineWorker(
            model_lock=self.asr._model_lock,
            transcribe_fn=self._refine_transcribe_fn,
            apply_fn=self._apply_refine,
            abort_mode="cooperative",
        )
        self._refine_worker.start()
        self._per_speaker_ingest.set_refine_submit_callback(
            self._refine_worker.submit
        )
        print("RefineWorker started")

    def _refine_transcribe_fn(self, pcm_samples, *, should_abort=None):
        """Bridge between `RefineWorker` and `ASRModule.transcribe_with_
        timestamps`.

        In cooperative mode (`should_abort` is a callable) install the
        abortable decoder monkey-patch for the duration of one transcribe,
        then restore. Coarse mode (`should_abort is None`) is a direct
        pass-through — no monkey-patch, no restoration.

        The monkey-patch mutates `model.decoding.decode`. It is safe because
        `RefineWorker._process_task` holds `asr._model_lock` across this
        call, so no other caller can observe the replaced decoder.
        """
        # Coarse mode: no abort hook by contract.
        if should_abort is None:
            return self.asr.transcribe_with_timestamps(pcm_samples)

        # Install the abort hook whenever a LOCAL model is (or will be) the one
        # decoding — independent of the advisory `using_remote` read. In remote
        # mode the gateway decode is a plain HTTP call with nothing to patch,
        # but `FallbackASR` can flip to a local decode between this guard and the
        # call (`using_remote` and `_decide()` are intentionally separate), so a
        # full local refine could otherwise run uninterruptible while
        # RefineWorker holds `_model_lock`. Patching the local decoder up front
        # keeps that fallback decode cooperatively abortable.
        #
        # `self.asr.model` is the local GigaAM model (None until loaded) for both
        # ASRModule and FallbackASR (the latter delegates `.model` to its local
        # module). Only load eagerly in pure-local mode; in remote mode a
        # not-yet-loaded model means the gateway will serve this call, so leave
        # weights unloaded (preserves remote-mode's zero local cost).
        model = getattr(self.asr, "model", None)
        if model is None and not getattr(self.asr, "using_remote", False):
            model = self.asr.load_model(quiet=True)
        if model is None:
            # Remote path, no local weights resident: nothing to patch.
            return self.asr.transcribe_with_timestamps(pcm_samples)

        original_decode = model.decoding.decode
        abortable = make_abortable_decode(model.decoding, should_abort)
        try:
            model.decoding.decode = abortable
            return self.asr.transcribe_with_timestamps(pcm_samples)
        finally:
            model.decoding.decode = original_decode

    def _start_ktalk_bridge(self):
        """Start the KTalk bridge WS server after all callbacks are wired.

        Kept separate from `_setup_ktalk_bridge` so PCM chunk callback (set by
        `_setup_per_speaker_ingest`) is guaranteed to be registered before the
        server accepts its first connection.
        """
        if not self.use_ktalk or self._ktalk_bridge is None:
            return
        self._ktalk_bridge.start()
        print("KTalk bridge started, waiting for Chrome extension...")
        time.sleep(1)

    def _on_pcm_batch(self, batch):
        """Callback from `PerSpeakerIngest` worker thread.

        Runs a single batched ASR forward on the raw PCM and emits transcript
        entries with the speaker_name already attached to each chunk — we do
        NOT query the KTalk bridge's `get_current_speaker` /
        `get_dominant_speaker_for_period` here (see plan §4.2 "НЕ вызывает ...").

        Meeting-generation safety: reading `self._meeting_generation` at
        callback entry races with `_invalidate_pending_chunks`, which can
        bump the counter between `_drain_batch_locked` and this callback.
        We use `PerSpeakerIngest.last_drain_generation` instead — it reflects
        the generation snapshot taken inside the drain under the ingest lock,
        so the per-chunk check below correctly detects meeting switches that
        happened after drain.
        """
        if not batch:
            return
        ingest = self._per_speaker_ingest
        drain_gen = ingest.last_drain_generation if ingest is not None else None
        chunk_gen = drain_gen if drain_gen is not None else self._meeting_generation

        pcm_list = [item[3] for item in batch]
        # Priority-signal to the refine worker: live is about to wait on
        # `asr._model_lock`; cooperative mode aborts in-flight refine at
        # the next decoder step-boundary so live does not block on a full
        # refine window. Clear after transcribe_batch returns (both on
        # success and failure) so a subsequent refine can run.
        refine_worker = self._refine_worker
        if refine_worker is not None:
            refine_worker.set_live_pending()
        try:
            texts = self.asr.transcribe_batch(pcm_list)
        except ASRTemporarilyUnavailable:
            # Пробрасываем в PerSpeakerIngest.tick — он сохранит батч для
            # повторного диспатча (дизайн §3, гонка первого сбоя).
            raise
        except Exception as e:
            print(f"[PerSpeaker] transcribe_batch failed: {e}", file=sys.stderr)
            return
        finally:
            if refine_worker is not None:
                refine_worker.clear_live_pending()

        current_time = time.time()
        batch_now_ms = current_time * 1000.0
        # Snapshot the wall-clock↔WAV anchor so each PCM segment can point at
        # the correct byte range in audio_system.wav for playback ("extract
        # clip") and "enroll from segments". The anchor ties a real captured
        # sample's wall-clock to its WAV offset; extrapolating from it keeps
        # consecutive PCM segments' offsets tracking their chunk_start times.
        # wav_offset_now_ms is kept only as a fallback when the anchor is
        # missing (before the first system write) or stale (degraded-mode
        # silence-skip broke the WAV timeline ahead of it).
        with self._wav_lock:
            anchor = self._system_wav_anchor
            wav_offset_now_ms = (
                self._wav_writer_system.get_offset_ms()
                if self._wav_writer_system
                else 0
            )
        # transcribe_batch must return one text per input (both local and
        # remote backends guarantee this). zip() would silently truncate to
        # the shorter sequence on a mismatch, dropping the tail speakers'
        # utterances with no signal — drop the whole batch loudly instead of
        # risking misattribution.
        if len(texts) != len(batch):
            print(
                f"[PerSpeaker] transcribe_batch returned {len(texts)} text(s) for "
                f"{len(batch)} input(s) — dropping batch to avoid misattribution",
                file=sys.stderr,
            )
            return
        for (pid, speaker_name, is_local, _pcm, chunk_start_ms, _end_ms, pid_audio_ms_start, pid_audio_ms_end), raw_text in zip(batch, texts):
            text = (raw_text or "").strip()
            if not text:
                continue

            # _pcm_prev_words / _pcm_pid_state are also cleared by
            # _invalidate_pending_chunks under _state_lock.  Holding the lock
            # across dedup, per-pid state update, send_transcript, and
            # append_segment keeps meeting-switch atomic: a concurrent
            # _invalidate_pending_chunks cannot bump the generation (and
            # swing current_meeting_path) between our gen-check and the I/O,
            # so PCM from the old meeting never lands in the new meeting's
            # transcript.jsonl or UI.
            with self._state_lock:
                if self._meeting_generation != chunk_gen:
                    # Drop emissions whose meeting has already switched.
                    continue
                prev_words = self._pcm_prev_words.get(pid, [])
                deduped = deduplicate(text, prev_words)
                if not deduped:
                    continue

                # speaker_name is untrusted (extension-provided); strip newlines so
                # it cannot break the markdown transcript line or confuse downstream
                # regex parsers (`_parse_anchor_lines`, AI summary prompts).
                safe_name = (speaker_name or "").replace("\n", " ").replace("\r", " ").strip()
                speaker_label = safe_name or f"Участник {pid[:6]}"

                self._pcm_prev_words[pid] = deduped.split()[-5:]

                pid_state = self._pcm_pid_state.setdefault(
                    pid,
                    {"last_speech_time": 0.0, "segment_start_time": current_time},
                )
                time_gap = current_time - pid_state["last_speech_time"]
                segment_duration = current_time - pid_state["segment_start_time"]
                is_continuation = (
                    time_gap < SPEAKER_TURN_GAP
                    and segment_duration < MAX_SEGMENT_DURATION
                )
                pid_state["last_speech_time"] = current_time
                if not is_continuation:
                    pid_state["segment_start_time"] = current_time

                timestamp = datetime.now().strftime("%H:%M:%S")

                # Convert the chunk's wall-clock start (already carries the
                # bridge's clock_offset) to an offset inside audio_system.wav.
                # Preferred: extrapolate from the wall↔WAV anchor in one clock
                # domain — audio_offset = anchor_wav + (chunk_start - anchor_wall).
                # This tracks each chunk's real capture time, so consecutive
                # segments get distinct offsets even when the WAV writer's
                # sample count is mid-step (it advances in ~chunk_duration
                # jumps). Fallback to `wav_offset_now - chunk_age` when there is
                # no anchor yet or it is stale (degraded-mode silence-skip broke
                # the timeline ahead of it). A negative delta (backlog replay
                # after an ASR failure) is legal: the pcm-primary WAV is
                # continuous, so extrapolating backwards still lands in-file.
                if (
                    anchor is not None
                    and batch_now_ms - anchor[0] <= PCM_WAV_ANCHOR_MAX_AGE_MS
                ):
                    audio_offset_ms = max(0, int(anchor[1] + (chunk_start_ms - anchor[0])))
                else:
                    chunk_age_ms = int(batch_now_ms - chunk_start_ms)
                    audio_offset_ms = max(0, wav_offset_now_ms - chunk_age_ms)

                line = f"\n**{speaker_label}** *[{timestamp}]*: {deduped}\n"
                sys.stdout.write(line)
                sys.stdout.flush()
                self.transcript_lines.append(line)

                # Write to MeetingStorage first so the freshly-minted
                # `segment_id` can be threaded into `send_transcript` for the
                # extension's `data-segment-id` attribute (tier-2 refine binds
                # to that id on the DOM). Without the reorder the bridge would
                # emit a bubble the refine pass cannot later rewrite.
                segment_id = 0
                if self._meeting_storage:
                    segment_id = self._meeting_storage.append_segment(
                        speaker=speaker_label,
                        speaker_id=pid,
                        text=deduped,
                        is_local=is_local,
                        is_continuation=is_continuation,
                        uncertain=False,
                        uncertain_reason="",
                        audio_offset_ms=audio_offset_ms,
                    )
                    # Stamp the live segment with its pid-frame bounds so the
                    # refine contour (Task 9) can align refined word timestamps
                    # back to this segment_id for the JSONL rewrite. Use the
                    # per-chunk `pid_audio_ms_start`/`_end` captured at enqueue
                    # time directly — deriving start from `len(_pcm) / rate`
                    # would re-introduce the 600 ms payload-overlap that the
                    # ingest already stripped from the pid-frame counter.
                    self._register_pcm_live_segment(
                        pid, segment_id, pid_audio_ms_start, pid_audio_ms_end
                    )
                    # Cache the freshly-written text so `_apply_refine` can
                    # short-circuit identical refinements without an extra
                    # JSONL read.
                    self._pcm_segment_texts[segment_id] = deduped

                if self._ktalk_bridge:
                    self._ktalk_bridge.send_transcript(
                        speaker_label,
                        deduped,
                        timestamp,
                        is_continuation=is_continuation,
                        is_local=is_local,
                        uncertain=False,
                        uncertain_reason="",
                        segment_id=segment_id,
                        source_id=pid,
                    )

    def _register_pcm_live_segment(
        self,
        pid: str,
        segment_id: int,
        pid_audio_ms_start: float,
        pid_audio_ms_end: float,
    ) -> None:
        """Append (segment_id, pid_audio_ms_start, pid_audio_ms_end) to the
        per-pid live-segment registry and prune entries outside the refine
        window + head room.

        Caller must hold `self._state_lock`. Both bounds are per-chunk
        snapshots taken at enqueue time and threaded through `BatchedChunk`
        so multiple chunks of the same pid in one batch get their own
        non-overlapping bounds. Computing `start` locally from
        `len(chunk_samples) / sample_rate` would re-introduce the 600 ms
        payload-overlap that the ingest strips from `_pid_audio_ms` — the
        previous-chunk tail is physically present in the live-path PCM but
        its pid-frame range was already covered by the previous segment's
        bounds, so refine alignment must not claim it again here. Prune
        floor = `pid_audio_ms_end - (PCM_REFINE_WINDOW_S + 5) * 1000` ms,
        matching the capacity of the PCM ring so stale entries do not grow
        unboundedly when a pid talks for a long time.
        """
        if self._per_speaker_ingest is None:
            return
        entries = self._pcm_live_segments.setdefault(pid, [])
        entries.append((
            segment_id,
            pid_audio_ms_start,
            pid_audio_ms_end,
        ))
        prune_floor = pid_audio_ms_end - (PCM_REFINE_WINDOW_S + 5.0) * 1000.0
        while entries and entries[0][2] < prune_floor:
            pruned = entries.pop(0)
            # Keep the text cache in lockstep with the live registry so it
            # does not grow unboundedly on long-running meetings.
            self._pcm_segment_texts.pop(pruned[0], None)

    def _apply_refine(self, result: RefineResult) -> None:
        """Align refined words back to live segments and rewrite JSONL.

        Invoked by `RefineWorker` on its daemon thread OUTSIDE `model_lock`
        (see `recog/refine.py`). Upstream `Word` timestamps are relative to
        the refine window; rebase into the per-pid frame via
        `task.offset_base_ms`, intersect with `_pcm_live_segments[pid]` to
        build per-segment update dicts, then hand off to
        `MeetingStorage.update_segments` (Task 10 extends it to accept
        `text` + `refined=True`).

        Meeting-generation safety: `_invalidate_pending_chunks` bumps
        `_meeting_generation` and clears `_pcm_live_segments` under
        `_state_lock`. We snapshot `target_meeting_path` inside the lock and
        compare `task.meeting_generation` to drop refines whose meeting
        switched between dispatch and apply (design.md D11).

        The actual `update_segments` call happens OUTSIDE `_state_lock` —
        that method takes its own `_segment_lock` for the JSONL rewrite, and
        holding two locks across file I/O would invite deadlocks.
        """
        task = result.task
        pid = task.pid
        words = result.words or []

        # Rebase upstream word ms timestamps into the per-pid frame once —
        # `task.offset_base_ms` is the pid_audio_ms of `pcm_samples[0]`.
        rebased: list[tuple[float, float, str]] = []
        for w in words:
            w_text = (getattr(w, "text", "") or "").strip()
            if not w_text:
                continue
            try:
                w_start = float(w.start)
                w_end = float(w.end)
            except (TypeError, ValueError):
                continue
            rebased.append((
                task.offset_base_ms + w_start,
                task.offset_base_ms + w_end,
                w_text,
            ))

        updates: list[dict] = []
        target_meeting_path = None

        with self._state_lock:
            if task.meeting_generation != self._meeting_generation:
                return
            # Closes the "pause lands between post-transcribe check and
            # apply" race (refine.py:465-473 only covers the window up to
            # `_apply_fn` entry). `_emit_pcm_transition_marker` holds
            # `_state_lock` across `pause()`, so checking here serialises
            # the spurious-promotion gate with the watchdog transition.
            # Lock order `_state_lock` → `_paused_lock` matches both paths
            # — no deadlock risk.
            if self._refine_worker is not None and self._refine_worker.is_paused():
                return
            if self._meeting_storage is None:
                return
            target_meeting_path = self._meeting_storage.current_meeting_path
            if target_meeting_path is None:
                return

            window_ms = PCM_REFINE_WINDOW_S * 1000.0
            task_start_ms = task.offset_base_ms
            task_end_ms = task.offset_base_ms + window_ms

            for segment_id, seg_start, seg_end in self._pcm_live_segments.get(pid, []):
                # Skip segments outside the refine window — saves work on
                # long-running pids where the registry holds many entries.
                if seg_end < task_start_ms or seg_start > task_end_ms:
                    continue
                collected: list[str] = []
                for w_start, _w_end, w_text in rebased:
                    # Single-point alignment by word start (design.md D1):
                    # "segment.pid_audio_ms_start <= word_pid_ms <=
                    # segment.pid_audio_ms_end". Using both bounds would
                    # drop boundary-straddling words (e.g. a word at
                    # [2900, 3100] when segments meet at 3000) — the exact
                    # case that motivated 20-s refine context (proposal.md).
                    if seg_start <= w_start < seg_end:
                        collected.append(w_text)
                if not collected:
                    continue
                new_text = " ".join(collected).strip()
                if not new_text:
                    continue
                cached_text = self._pcm_segment_texts.get(segment_id)
                if cached_text is not None and cached_text == new_text:
                    continue
                updates.append({
                    "segment_id": segment_id,
                    "text": new_text,
                    "refined": True,
                })

        if not updates:
            return
        self._meeting_storage.update_segments(
            updates, meeting_path=target_meeting_path
        )
        # Publish the refined text to extension + webapp clients via the
        # bridge so UI surfaces can promote tier-1 rows to tier-2 without
        # tailing the JSONL (the file rewrite is invisible to the forward
        # seek tail reader — see plan §3 / design D10).
        if self._ktalk_bridge is not None:
            try:
                self._ktalk_bridge.send_refine(
                    [{"segment_id": u["segment_id"], "text": u["text"]} for u in updates],
                    meeting_id=target_meeting_path.name,
                )
            except Exception as exc:
                print(f"[Live] send_refine failed: {exc}", file=sys.stderr)
        # Cache AFTER successful update_segments — if the JSONL rewrite
        # fails (I/O error, permissions), the cache stays in sync with
        # disk so a subsequent refine with the same text retries instead
        # of silently skipping.
        with self._state_lock:
            # Re-check the generation: _invalidate_pending_chunks may have
            # cleared _pcm_segment_texts between the update_segments call
            # (outside the lock) and this reacquire. Writing stale segment_ids
            # back would pollute the new meeting's cache (segment_ids restart
            # at 1 per meeting → collisions possible).
            if task.meeting_generation != self._meeting_generation:
                return
            # Snapshot currently-present segment_ids for `pid` so we do NOT
            # re-populate cache entries that `_register_pcm_live_segment`
            # has pruned while the lock was released for the JSONL rewrite.
            # Otherwise the cache grows unbounded over long meetings, since
            # pruned entries are only cleared when `_register_pcm_live_segment`
            # evicts them from the registry (and it cannot evict what it
            # doesn't know about).
            live_ids = {
                seg_id for seg_id, _s, _e in self._pcm_live_segments.get(pid, [])
            }
            for upd in updates:
                if upd["segment_id"] in live_ids:
                    self._pcm_segment_texts[upd["segment_id"]] = upd["text"]

    def _emit_pcm_transition_marker(self, degraded: bool, marker_gen: int):
        """Write a gap marker to the transcript on PCM ↔ mixed fallback flip.

        Called from the system-audio branch of `_process_chunk` when
        `PerSpeakerIngest.degraded` changes state.  Appears both in stdout
        and in MeetingStorage so downstream consumers (frontend, AI summary)
        can surface the degraded window.

        Also pauses the refine worker while we're running on the mixed
        fallback path: tier-2 rewrites cannot be trusted without per-pid
        PCM (the rolling ring has gone stale and the live segments now come
        from the mixed-audio branch, which has no pid-frame mapping).
        Resume on recovery.

        `marker_gen` is captured together with the `_last_pcm_degraded`
        compare-and-set at the call site, so that a concurrent
        `_invalidate_pending_chunks` (meeting switch) does not cause us to
        write the marker to the wrong meeting's transcript.jsonl OR pause
        the refine worker without a matching resume in the new meeting.
        `_invalidate_pending_chunks` resets `_last_pcm_degraded=False` AND
        drains the ingest (`ingest._degraded=False`), so a stale pause
        applied here would never observe a healthy→degraded→healthy cycle
        in the new meeting and refine would stay disabled indefinitely.
        """
        if degraded:
            msg = f"[PCM silence >{int(PCM_SILENCE_WATCHDOG_S)}s — switched to mixed fallback]"
        else:
            msg = "[PCM recovered — back to per-speaker]"
        timestamp = datetime.now().strftime("%H:%M:%S")
        line = f"\n*{msg}* *[{timestamp}]*\n"
        sys.stdout.write(line)
        sys.stdout.flush()
        self.transcript_lines.append(line)
        # Hold _state_lock across the generation check + pause/resume +
        # append_segment so a concurrent _invalidate_pending_chunks on the
        # bridge thread cannot bump the generation between our check and
        # the mutations. Pause/resume must sit behind the same guard as the
        # JSONL write — otherwise a stale transition could leave the worker
        # paused for the next meeting (see method docstring).
        with self._state_lock:
            if self._meeting_generation != marker_gen:
                return
            if self._refine_worker is not None:
                if degraded:
                    self._refine_worker.pause()
                else:
                    self._refine_worker.resume()
            if self._meeting_storage:
                self._meeting_storage.append_segment(
                    speaker="Система",
                    speaker_id="__system__",
                    text=msg,
                    is_local=False,
                    is_continuation=False,
                    uncertain=False,
                    uncertain_reason="pcm_watchdog_transition",
                    audio_offset_ms=0,
                )

    def _check_system_capture_alive(self):
        """Один маркер, когда системный захват исчерпал рестарты и сдался.

        Отдельно от `_emit_pcm_transition_marker`: PCM-путь при этом может
        быть совершенно здоров (звук участников идёт из расширения), поэтому
        watchdog-переход PCM↔mixed не срабатывает и сообщать о нём было бы
        враньём. Refine здесь не трогаем по той же причине — per-pid PCM на
        месте, тир-2 остаётся рабочим.
        """
        capture = self.system_capture
        if capture is None:
            return
        if not getattr(capture, "dead", False):
            return
        # Compare-and-set вместе с чтением встречи и поколения под одним
        # локом: взвод вне лока успевал попасть в окно «взвели → встреча
        # сменилась → читаем уже новое поколение», и новая встреча получала
        # два одинаковых маркера подряд.
        with self._state_lock:
            meeting_key = self._current_meeting_key()
            if self._system_capture_lost and self._system_capture_lost_meeting == meeting_key:
                return
            self._system_capture_lost = True
            self._system_capture_lost_meeting = meeting_key
            marker_gen = self._meeting_generation
        self._emit_capture_loss_marker(marker_gen)

    def _current_meeting_key(self):
        """Текущая встреча как ключ per-meeting состояний watchdog'а.

        `current_meeting_path` меняется ровно на смене встречи — и на
        bridge-пути, и на orphan-сплите по тишине — и остаётся прежним при
        reconnect расширения в ту же встречу. Поэтому идентичность встречи
        берётся отсюда, а не из событий, которые её только сопровождают.
        """
        storage = self._meeting_storage
        if storage is None:
            return None
        return getattr(storage, "current_meeting_path", None)

    def _emit_capture_loss_marker(self, marker_gen: int):
        """Пишет маркер потери системного захвата в stdout и в транскрипт."""
        msg = "[Системный звук потерян — захват не восстановился]"
        timestamp = datetime.now().strftime("%H:%M:%S")
        line = f"\n*{msg}* *[{timestamp}]*\n"
        sys.stdout.write(line)
        sys.stdout.flush()
        self.transcript_lines.append(line)
        # Та же защита от смены встречи между наблюдением и записью, что и у
        # watchdog-маркера: `_invalidate_pending_chunks` бампает поколение.
        with self._state_lock:
            if self._meeting_generation != marker_gen:
                return
            if self._meeting_storage:
                self._meeting_storage.append_segment(
                    speaker="Система",
                    speaker_id="__system__",
                    text=msg,
                    is_local=False,
                    is_continuation=False,
                    uncertain=False,
                    uncertain_reason="system_audio_capture_lost",
                    audio_offset_ms=0,
                )

    def _start_audio_captures(self):
        """Start mic and system audio capture devices."""
        if self.mic_capture:
            self.mic_capture.start()
        if self.system_capture:
            self.system_capture.start()

    def _teardown_session(self, executor, force_exit: bool):
        """Stop captures, finalize audio, close meeting, write transcript."""
        print("\n" + "-" * 60)
        print("Остановлено.")

        self.running = False
        executor.shutdown(wait=False, cancel_futures=True)

        if self.mic_capture:
            self.mic_capture.stop()
        if self.system_capture:
            self.system_capture.stop()

        # Stop the refine worker before the ingest so that a late refine
        # submission from the ingest's last tick does not land in a queue
        # about to be drained by `RefineWorker.stop()`. The worker joins
        # its thread with a short timeout; no-op if refine was never enabled.
        if self._refine_worker is not None:
            self._refine_worker.stop()
            self._refine_worker = None

        # Stop per-speaker ingest worker before final ASR draw-down so that
        # queued batches are not left dangling.
        if self._per_speaker_ingest is not None:
            self._per_speaker_ingest.stop()

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
            self._system_wav_anchor = None

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

    def _process_pending(self, state, is_mic=False):
        """Process all pending embedding updates for a source."""
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
                is_continuation = (speaker == state.last_speaker
                                   and time_gap < SPEAKER_TURN_GAP
                                   and segment_duration < MAX_SEGMENT_DURATION)

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

    def _check_silence_split(self, state, current_time, chunk=None, is_mic=False):
        """Check silence duration and split meeting session if threshold exceeded."""
        if self._meeting_storage:
            silence_duration = current_time - state.last_text_time
            if silence_duration >= SILENCE_SPLIT_TIMEOUT:
                new_id = self._meeting_storage.split_on_silence_timeout()
                if new_id is not None:
                    # Сплит меняет current_meeting_path мимо KTalk-bridge
                    # callback — ротируем WAV-writer'ы здесь, иначе сегменты
                    # нового митинга ссылаются на файл старого.
                    # continuation=True: capture-буферы не чистились, у
                    # следующих чанков есть overlap от предыдущих.
                    self._on_meeting_change(
                        new_id,
                        self._meeting_storage.base_path / new_id,
                        continuation=True,
                    )
                    # Триггерный чанк уже записан в WAV старого митинга —
                    # перезаписываем его в новый файл, чтобы первый сегмент
                    # нового митинга указывал в собственное аудио.
                    if chunk is not None:
                        with self._wav_lock:
                            writer = (
                                self._wav_writer_mic if is_mic
                                else self._wav_writer_system
                            )
                            if writer:
                                state.audio_offset = writer.get_offset_ms()
                                writer.write_chunk(chunk)
                state.last_speaker = None
                state.last_words = []
            state.last_text_time = current_time

    def _handle_mic_chunk(self, text, timestamp, chunk_time, chunk_gen, state):
        """Handle a transcribed mic chunk: fixed speaker name, is_local=True."""
        mic_speaker_name = self.mic_speaker_name
        time_gap = chunk_time - state.last_speech_time
        segment_duration = chunk_time - state.segment_start_time
        is_continuation = (state.last_speaker == mic_speaker_name
                           and time_gap < SPEAKER_TURN_GAP
                           and segment_duration < MAX_SEGMENT_DURATION)

        state.last_speaker = mic_speaker_name
        state.last_speech_time = chunk_time
        if not is_continuation:
            state.segment_start_time = chunk_time

        # Микрофон: без диаризации, фиксированное имя
        line = f"\n**{mic_speaker_name}** *[{timestamp}]*: {text}\n"
        sys.stdout.write(line)
        sys.stdout.flush()
        self.transcript_lines.append(line)

        # Сохраняем в MeetingStorage сначала, чтобы `segment_id` попал в
        # `send_transcript` и extension мог пометить bubble атрибутом
        # `data-segment-id` для будущих refine-апдейтов.
        segment_id = 0
        if self._meeting_storage and self._meeting_generation == chunk_gen:
            segment_id = self._meeting_storage.append_segment(
                speaker=mic_speaker_name,
                speaker_id="local",
                text=text,
                is_local=True,
                is_continuation=is_continuation,
                uncertain=False,
                uncertain_reason="",
                audio_offset_ms=state.audio_offset
            )

        # Отправляем транскрипт микрофона в Chrome extension (is_local=True)
        if self._ktalk_bridge:
            self._ktalk_bridge.send_transcript(
                mic_speaker_name, text, timestamp, is_continuation,
                is_local=True, segment_id=segment_id)

    def _handle_ktalk_chunk(self, text, timestamp, chunk_start_time, chunk_end_time,
                            chunk_time, chunk_gen, state, *, fallback_mixed: bool = False):
        """Handle a transcribed system-audio chunk with KTalk speaker attribution.

        Speaker assignment fallback chain (see ADR / plan for rationale):

          1. dominant_speaker_for_period -- time-window voting from Jitsi events
          2. get_current_speaker -- point-in-time with 3 s memory
          3. last_speaker TTL -- reuse recent speaker if <10 s and not muted
          4. sole_unmuted_remote -- exactly 1 remote mic on -> likely speaker
             (marked uncertain; avoids false attribution when 2+ are unmuted)
          5. "Unknown" -- no attribution possible

        Why sole_unmuted fallback exists: Jitsi's _activeSpeaker$ event lags
        1-5 s behind actual speech. When Speaker A mutes and Speaker B starts,
        the extension filters out stale A (muted) but Jitsi hasn't switched to
        B yet, leaving a gap with no positive signal. If only B is unmuted, we
        can safely attribute speech to B. Remaining edge cases are caught by
        retroactive correction (see _retroactive_correct_unknown_run).
        """
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
        is_continuation = (speaker == state.last_speaker
                           and time_gap < SPEAKER_TURN_GAP
                           and segment_duration < MAX_SEGMENT_DURATION)

        # Only update last_speaker with confidently identified speakers;
        # "Unknown" would poison the TTL fallback window, and
        # sole_unmuted is uncertain -- propagating it via TTL would
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

            # Persisted (not sent to extension) diagnostic marker when the
            # mixed-audio path ran only because the PCM watchdog tripped.
            # Frontend compares uncertain_reason by exact key, so compound
            # "fallback_mixed/<reason>" falls through to the default
            # uncertain suffix without breaking known-reason rendering.
            persisted_reason = uncertain_reason
            if fallback_mixed:
                persisted_reason = (
                    f"fallback_mixed/{uncertain_reason}" if uncertain_reason
                    else "fallback_mixed"
                )

            # Сохраняем в MeetingStorage первым, чтобы `segment_id`
            # сразу был доступен для payload'а send_transcript (tier-2
            # refine ищет bubble по `data-segment-id`).
            meeting_seg_id = 0
            if self._meeting_storage and self._meeting_generation == chunk_gen:
                meeting_seg_id = self._meeting_storage.append_segment(
                    speaker=speaker,
                    speaker_id=speaker,  # Используем имя как ID для KTalk
                    text=text,
                    is_local=is_local,
                    is_continuation=is_continuation,
                    uncertain=uncertain,
                    uncertain_reason=persisted_reason,
                    audio_offset_ms=state.audio_offset
                )

            self._ktalk_bridge.send_transcript(
                speaker, text, timestamp, is_continuation,
                is_local=is_local, uncertain=uncertain,
                uncertain_reason=uncertain_reason,
                segment_id=meeting_seg_id)

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

    def _handle_embedding_chunk(self, text, timestamp, chunk_time, chunk_gen, state,
                                chunk, executor, is_mic=False):
        """Handle transcribed chunk via no-diarization or async embedding."""
        if self.no_diarization:
            time_gap = chunk_time - state.last_speech_time
            segment_duration = chunk_time - state.segment_start_time
            is_continuation = (state.last_speaker == "speaker"
                               and time_gap < SPEAKER_TURN_GAP
                               and segment_duration < MAX_SEGMENT_DURATION)

            state.last_speaker = "speaker"
            state.last_speech_time = chunk_time
            if not is_continuation:
                state.segment_start_time = chunk_time

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
                    is_continuation=is_continuation,
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

            future = executor.submit(self._resolve_speaker_embedding, chunk_copy)
            with self._state_lock:
                state.pending_updates.append(
                    (len(self.transcript_lines) - 1, timestamp, text, future,
                     chunk_time, state.audio_offset, chunk_gen))

    def _process_chunk(self, capture, state, executor, is_mic=False):
        """Read audio chunk, run ASR, and dispatch to appropriate handler."""
        # ASR availability gate (дизайн §2): пока FallbackASR не может обслужить
        # запрос (remote упал, локаль грузится в фоне) — НЕ вычитываем буфер.
        # Capture-буфер и есть спул: аудио копится там до восстановления.
        if not self._asr_available():
            return

        # Snapshot meeting generation BEFORE reading buffer so that audio
        # captured before a meeting switch is tagged with the old generation
        # and discarded by the generation check downstream.
        chunk_gen = self._meeting_generation

        # Сначала — retry-слот: чанк, не обслуженный прошлой попыткой
        # (ASRTemporarilyUnavailable). Он старше всего в capture-буфере и уже
        # записан в WAV, поэтому запись ниже пропускается. Чанк из другого
        # поколения (встреча сменилась) молча выбрасывается.
        retry_pending = state.pending_chunk
        state.pending_chunk = None
        if retry_pending is not None and retry_pending[2] != chunk_gen:
            retry_pending = None
        if retry_pending is not None:
            chunk, capture_start_ts, _ = retry_pending
            # Retry-чанк в WAV уже записан первой попыткой, до сравнения
            # эпох ниже дело не дойдёт.
            capture_epoch = self._system_source_epoch
        else:
            result = capture.get_chunk(self.chunk_duration)
            if result is None:
                return
            chunk, capture_start_ts = result
            if len(chunk) < 1000:
                return
            # Номер отрезка спрашиваем у самого чанка, а не снимком до/после
            # чтения буфера: capture фиксирует его под тем же локом, что и
            # нарезку (см. BufferedAudioCapture.chunk_epoch), поэтому граница
            # источников, прошедшая рядом с этим чтением, не отнесётся к
            # чужому чанку.
            capture_epoch = capture.chunk_epoch
        # Времена чанка — от capture-времени (НЕ time.time()): при обработке
        # бэклога после сбоя ASR сегменты должны лечь в то время, когда речь
        # была произнесена.
        chunk_start_time = capture_start_ts
        chunk_end_time = capture_start_ts + self.chunk_duration

        # RMS считается один раз — и для решения о записи WAV здесь, и для
        # VAD-гейта перед ASR ниже. Тишину на диск не пишем: 24/7-сервис между
        # встречами иначе бесконечно раздувает WAV текущего orphan-митинга.
        # Исключение — pcm-primary: оффсеты PCM-сегментов экстраполируются от
        # wall-clock↔WAV якоря _system_wav_anchor (см. _on_pcm_batch), а он
        # корректен только при непрерывной real-time timeline в
        # audio_system.wav — поэтому тишину здесь пишем. Mixed-path сегменты
        # берут оффсет в момент записи своего чанка, им пропуски тишины не
        # вредят.
        rms = np.sqrt(np.mean(chunk ** 2))
        pcm_primary = (
            not is_mic
            and self._per_speaker_ingest is not None
            and self._per_speaker_ingest.pcm_ever_seen
            and not self._per_speaker_ingest.degraded
        )
        skip_wav_write = rms < VAD_RMS_THRESHOLD and not pcm_primary

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
            if retry_pending is not None or skip_wav_write:
                # retry: чанк уже записан первой попыткой; state.audio_offset
                # тоже выставлен ей и с тех пор не перезаписывался (этот код —
                # его единственный писатель для данного state). Якорь тоже
                # выставлен первой попыткой — повторно не двигаем, иначе
                # wall-часть уехала бы вперёд без записи новых сэмплов.
                # skip: тихий чанк не породит сегмент (VAD-гейт ниже), значит
                # его оффсет никто не прочитает — не трогаем state.audio_offset;
                # запись не идёт, поэтому и якорь не обновляем.
                pass
            elif is_mic and self._wav_writer_mic:
                state.audio_offset = self._wav_writer_mic.get_offset_ms()
                self._wav_writer_mic.write_chunk(chunk)
            elif not is_mic and self._wav_writer_system:
                if capture_epoch != self._system_source_epoch:
                    # Захват сменил источник и сбросил буфер: overlap-головы,
                    # которую писатель срезает по умолчанию, в WAV нет.
                    self._system_source_epoch = capture_epoch
                    self._wav_writer_system.mark_discontinuity()
                offset_before = self._wav_writer_system.get_offset_ms()
                state.audio_offset = offset_before
                skipped = self._wav_writer_system.write_chunk(chunk)
                if skipped is not None:
                    # Anchor the first actually-written sample's wall-clock to
                    # its WAV offset. capture_start_ts is the epoch time of the
                    # chunk's first sample; the overlap head was already written
                    # by the previous chunk, so skip past it.
                    self._system_wav_anchor = (
                        (capture_start_ts + skipped / self.sample_rate) * 1000.0,
                        offset_before,
                    )

        # Per-speaker PCM primary-path gating for system-audio chunks
        # (see plan §4.5).  WAV writing above still runs so that the
        # system capture stays available as an offline fallback, but
        # we skip ASR while the PCM path is healthy and emit a gap
        # marker on degraded↔healthy transitions.
        #
        # `pcm_ever_seen` keeps the mixed-path running until the extension
        # has actually produced a chunk for this meeting — otherwise the
        # pre-join / broken-extension window silently swallows up to
        # PCM_SILENCE_WATCHDOG_S of system audio before the watchdog fires.
        fallback_mixed = False
        if (
            not is_mic
            and self._per_speaker_ingest is not None
            and self._per_speaker_ingest.pcm_ever_seen
        ):
            degraded_now = self._per_speaker_ingest.degraded
            # _last_pcm_degraded is also reset by _invalidate_pending_chunks
            # under _state_lock; guard the compare-and-swap so we don't emit
            # a spurious transition marker on a meeting boundary. Capture
            # chunk_gen atomically with the flag update so the transition
            # marker below lands on the meeting that observed the flip and
            # not a new one entered while we were releasing the lock.
            with self._state_lock:
                should_emit = degraded_now != self._last_pcm_degraded
                if should_emit:
                    self._last_pcm_degraded = degraded_now
                marker_gen = self._meeting_generation
            if should_emit:
                self._emit_pcm_transition_marker(degraded_now, marker_gen)
            if not degraded_now:
                # PCM is primary; drop system-audio ASR work entirely.
                return
            fallback_mixed = True

        # Пропускаем запись микрофона если на KTalk встрече и mic замьючен
        # Проверяем ДО ASR inference чтобы не тратить CPU на muted chunks
        if is_mic and self._ktalk_bridge and self._ktalk_bridge.is_local_mic_muted():
            return

        # VAD: пропускаем тишину (не тратим CPU на inference); rms посчитан
        # выше, перед решением о записи WAV
        if rms < VAD_RMS_THRESHOLD:
            return

        try:
            text = self._transcribe_direct(chunk)
        except ASRTemporarilyUnavailable:
            # ASR отвалился между гейтом и вызовом (первый сбой remote).
            # Чанк не теряем — кладём в retry-слот, он будет обработан после
            # восстановления (фоновая загрузка локали / probe remote).
            state.pending_chunk = (chunk, capture_start_ts, chunk_gen)
            return
        if not text:
            return

        # Дедуп-окно (дизайн §4): last_words применяются только к временно́
        # смежным чанкам (audio overlap 0.5 s + джиттер). За пределами окна
        # повторённая фраза — реальная речь, её нельзя выбрасывать.
        if chunk_start_time - state.last_words_capture_end > DEDUP_MAX_GAP_S:
            state.last_words = []

        text = deduplicate(text, state.last_words)
        if not text:
            return

        # Bail out if meeting changed during ASR -- prevents stale
        # state updates (last_words/last_speaker) and transcript sends
        # from leaking across meeting boundaries
        if self._meeting_generation != chunk_gen:
            return

        state.last_words = text.split()[-5:]
        state.last_words_capture_end = chunk_end_time

        current_time = chunk_end_time
        self._check_silence_split(state, current_time, chunk, is_mic=is_mic)

        timestamp = datetime.fromtimestamp(chunk_end_time).strftime("%H:%M:%S")
        chunk_time = current_time

        if self.transcript_lines:
            sys.stdout.write("\n")

        if is_mic:
            self._handle_mic_chunk(text, timestamp, chunk_time, chunk_gen, state)
        elif self.use_ktalk:
            self._handle_ktalk_chunk(
                text, timestamp, chunk_start_time, chunk_end_time,
                chunk_time, chunk_gen, state,
                fallback_mixed=fallback_mixed)
        else:
            self._handle_embedding_chunk(
                text, timestamp, chunk_time, chunk_gen, state,
                chunk, executor, is_mic=is_mic)


    def run(self, force_exit: bool = False):
        """Основной цикл live транскрипции."""
        import concurrent.futures

        print("=" * 60)
        print(" LIVE TRANSCRIPTION")
        system_display = self.system_audio_label()
        mic_display = self.mic_device if self.mic_device is not None else 'default'
        if self.dual_mode:
            print(f" Mode: DUAL (Mic: {mic_display} + System: {system_display})")
        elif self.system_capture is not None:
            print(f" Mic: none | System: {system_display}")
        else:
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

        self._setup_meeting_storage()
        self._setup_ktalk_bridge()
        self.asr.load_model(quiet=True)

        self._setup_per_speaker_ingest()
        self._start_ktalk_bridge()

        self.running = True

        executor = concurrent.futures.ThreadPoolExecutor(max_workers=2)

        self._mic_state = SourceState()
        self._sys_state = SourceState()

        last_audio_cleanup = time.monotonic()

        try:
            # Внутри try: отказ старта захвата (микрофон занят, устройство
            # исчезло) до этого уходил наружу мимо `_teardown_session` —
            # встреча оставалась открытой навсегда (`stale_session`), а WAV'ы
            # недописанными. Под сервисом с Restart=always каждый рестарт
            # добавлял ещё одну такую встречу.
            self._start_audio_captures()

            while self.running:
                time.sleep(0.1)

                # Ротация аудио старше 24 ч: сервис живёт неделями, а cleanup
                # в start_session срабатывает только при старте. Скан stat-only
                # (см. cleanup_old_audio), раз в час — нагрузки не даёт.
                if time.monotonic() - last_audio_cleanup >= AUDIO_CLEANUP_INTERVAL_S:
                    last_audio_cleanup = time.monotonic()
                    if self._meeting_storage:
                        try:
                            self._meeting_storage.cleanup_old_audio()
                        except Exception as e:
                            print(f"Warning: audio cleanup failed: {e}", file=sys.stderr)

                if self.mic_capture:
                    self._process_pending(self._mic_state, is_mic=True)
                if self.system_capture:
                    self._process_pending(self._sys_state, is_mic=False)

                if self.mic_capture:
                    self._process_chunk(self.mic_capture, self._mic_state, executor, is_mic=True)
                if self.system_capture:
                    self._check_system_capture_alive()
                    self._process_chunk(self.system_capture, self._sys_state, executor, is_mic=False)

        except KeyboardInterrupt:
            pass
        except Exception as e:
            print(f"\n[FATAL ERROR] {e}", file=sys.stderr)
            import traceback
            traceback.print_exc()

        finally:
            self._teardown_session(executor, force_exit)


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
            if self._mic_state is not None:
                for state in (self._mic_state, self._sys_state):
                    for pending in state.pending_updates:
                        pending[3].cancel()  # pending[3] is the Future
                    state.last_speaker = None
                    state.last_words = []
                    state.segment_start_time = 0
                    state.pending_updates = []
            # Reset PCM per-pid state so continuation/dedup don't carry
            # across meetings.  Watchdog transition flag also resets because
            # a fresh meeting should start assuming PCM healthy.
            self._pcm_prev_words.clear()
            self._pcm_pid_state.clear()
            # Live-segment registry is per-meeting: segment_ids restart at 0
            # on the new meeting and pid_audio_ms bounds are only meaningful
            # within one meeting's pid frame.
            self._pcm_live_segments.clear()
            # Segment-text cache is keyed by segment_id; those IDs restart on
            # meeting switch, so any cached values would collide with fresh
            # segment_ids in the new meeting's pid frame.
            self._pcm_segment_texts.clear()
            self._last_pcm_degraded = False
            # If a degraded-trip in the old meeting paused the refine
            # worker, the pause state outlives the meeting switch. The
            # watchdog compare-and-set above is now reset
            # (_last_pcm_degraded=False, ingest._degraded=False after
            # drain()), so the healthy→degraded→healthy edge that calls
            # resume() via _emit_pcm_transition_marker will never fire in
            # the new meeting. Resume eagerly so refine is active from the
            # first PCM frame of meeting B; the watchdog can re-pause if
            # PCM goes silent there.
            if self._refine_worker is not None:
                self._refine_worker.resume()
        # Clear per-speaker ingest buffers (bumped generation would drop
        # them at drain time anyway, but clearing eagerly avoids a 200 ms
        # window where stale-gen chunks occupy capacity).
        if self._per_speaker_ingest is not None:
            self._per_speaker_ingest.drain()
        # Clear capture buffers so the next get_chunk() returns only
        # audio recorded AFTER the meeting switch, not stale old-meeting
        # samples that accumulated since the last drain.
        if self.mic_capture:
            self.mic_capture.clear_buffer()
        if self.system_capture:
            self.system_capture.clear_buffer()

    def _on_meeting_change(self, meeting_id: str, meeting_path: Path, continuation: bool = False):
        """Called after meeting switch to reinitialize audio recording.

        Args:
            continuation: True when audio capture keeps running across the
                switch WITHOUT a buffer clear (silence split) — the next
                chunk still carries the 0.5s overlap already written to the
                previous meeting's WAV, so the writer must skip it.
        """
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
            # Anchor belongs to the outgoing meeting's WAV — the new writer
            # starts a fresh timeline, so drop it until the first system write
            # of the new meeting re-seeds it. Covers both the bridge switch and
            # the silence-split path (via _check_silence_split -> here).
            self._system_wav_anchor = None

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
            # Bridge path (continuation=False): _invalidate_pending_chunks()
            # already cleared the capture buffers, so there is no overlap from
            # the previous chunk and skipping 0.5s would drop fresh audio.
            # Silence-split path (continuation=True): buffers keep flowing, the
            # overlap is already in the previous meeting's WAV — skip it.
            if self.mic_capture:
                mic_audio_path = meeting_path / "audio_mic.wav"
                self._wav_writer_mic = IncrementalWavWriter(mic_audio_path, self.sample_rate)
                self._wav_writer_mic.open(continuation=continuation)
                print(f"Recording mic audio: {mic_audio_path}")
            if self.system_capture:
                system_audio_path = meeting_path / "audio_system.wav"
                self._wav_writer_system = IncrementalWavWriter(system_audio_path, self.sample_rate)
                self._wav_writer_system.open(continuation=continuation)
                print(f"Recording system audio: {system_audio_path}")


def live_pipeline(
    mic_device=None,
    system_audio=None,
    speaker_db_path=None,
    chunk_duration=5.0,
    output_file=None,
    no_diarization=False,
    list_devices=False,
    use_ktalk=False,
    mic_speaker_name=None,
    save_audio=True,
    system_device=None,  # legacy-алиас: device:<id>
    use_screencapture=False,  # legacy-алиас: screencapture
):
    """Пайплайн live транскрипции."""
    if list_devices:
        SingleDeviceCapture.list_devices()
        return

    system_audio = normalize_system_audio(system_audio, system_device, use_screencapture)

    # Автоопределение устройств
    if mic_device == "auto":
        mic_device = SingleDeviceCapture.find_default_mic()
        if mic_device is not None:
            import sounddevice as sd
            name = sd.query_devices(mic_device)['name']
            print(f"Auto-detected mic: {name} (device {mic_device})", file=sys.stderr)
        else:
            print("Warning: Could not find an input device", file=sys.stderr)

    # BlackHole ищем только для явного `device:auto` — у screencapture/pulse
    # свой источник, а не PortAudio-устройство.
    if system_audio == "device:auto":
        blackhole = SingleDeviceCapture.find_blackhole()
        if blackhole is not None:
            import sounddevice as sd
            name = sd.query_devices(blackhole)['name']
            print(f"Auto-detected system audio: {name} (device {blackhole})", file=sys.stderr)
            system_audio = f"device:{blackhole}"
        else:
            print("Warning: Could not find BlackHole device", file=sys.stderr)
            system_audio = NO_SYSTEM_AUDIO

    transcriber = LiveTranscriber(
        mic_device=mic_device,
        system_audio=system_audio,
        speaker_db_path=speaker_db_path,
        chunk_duration=chunk_duration,
        output_file=output_file,
        no_diarization=no_diarization,
        use_ktalk=use_ktalk,
        mic_speaker_name=mic_speaker_name,
        save_audio=save_audio,
    )
    transcriber.run(force_exit=True)
