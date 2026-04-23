"""
Конфигурация и константы для Recog.
"""

import os
from pathlib import Path


def load_dotenv(env_path: Path = None):
    """Загружает переменные окружения из .env файла."""
    if env_path is None:
        env_path = Path(__file__).parent.parent / ".env"

    if not env_path.exists():
        return

    with open(env_path, 'r') as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if '=' in line:
                key, value = line.split('=', 1)
                key = key.strip()
                value = value.strip().strip('"').strip("'")
                if key and key not in os.environ:
                    os.environ[key] = value


# Загружаем .env при импорте модуля
load_dotenv()

# ============================================================================
# КОНФИГУРАЦИЯ
# ============================================================================

DEFAULT_MODEL = "v3_e2e_rnnt"
DEFAULT_CHUNK_DURATION = 20.0
DEFAULT_OVERLAP = 1.0
# Порог косинусного сходства для идентификации спикеров
SPEAKER_SIMILARITY_THRESHOLD = 0.50

# VAD (Voice Activity Detection) - порог RMS для определения тишины
# Аудио с RMS ниже этого порога считается тишиной и не отправляется на inference
VAD_RMS_THRESHOLD = 0.005

# TTL для fallback на последнего известного спикера (в секундах)
# Если KTalk не определил спикера, используем last_speaker только если он говорил недавно.
# После истечения TTL — спикер сбрасывается на "Unknown", чтобы избежать каскадной ошибочной атрибуции.
LAST_SPEAKER_TTL = 10.0

# Таймаут тишины для автоматического split orphan-митингов (в секундах)
# Если 10 минут без распознанного текста — при следующей реплике создаётся новый митинг
SILENCE_SPLIT_TIMEOUT = 10 * 60  # 10 minutes

# Retroactive correction: when a speaker is identified after an Unknown run,
# scan backwards and re-attribute Unknown segments to the identified speaker.
RETROACTIVE_MAX_SEGMENTS = 20   # Max segments to scan backwards
RETROACTIVE_MAX_SECONDS = 60.0  # Max time window for retroactive correction

# Порог смены спикера: если пауза между репликами больше этого значения (секунды),
# следующая реплика считается новым сегментом (новый speaker turn)
SPEAKER_TURN_GAP = 3.0

# Принудительный разрыв сегмента после N секунд непрерывной речи одного спикера
MAX_SEGMENT_DURATION = 20.0

# Имя спикера для микрофонного входа (live transcription)
MIC_SPEAKER_NAME = os.environ.get("MIC_SPEAKER_NAME", "").strip() or "Me"

# ============================================================================
# PER-SPEAKER PCM TRANSPORT (RECOG_PCM_TRANSPORT)
# ============================================================================
#
# Константы протокола per-speaker ASR пайплайна: extension аттачит
# AudioWorkletNode к каждому удалённому участнику KTalk-конференции и шлёт
# 16 kHz Int16 PCM напрямую в ktalk_bridge. Python-сторона батчит чанки
# с разных pid'ов и гоняет через общий GigaAM в batched forward.
#
# См. docs/plans/2026-04-22-per-speaker-asr-pipeline.md «Decisions D1–D10».

# Cadence эмиссии чанков worklet'ом (stride). Совпадает с текущим
# `--chunk-duration` default 3 s, чтобы UI-latency не деградировал.
PCM_CHUNK_DURATION_S = 3.0

# Длина PCM payload в одном чанке = stride + overlap (300 ms).
# Worklet держит ring-буфер на 3300 ms; каждые 3000 ms эмитит последние
# 3300 ms — последние 300 ms предыдущего чанка повторяются в начале
# следующего для _remove_duplicate_words на границах.
PCM_CHUNK_PAYLOAD_S = 3.3

# Окно batch-коллектора в Python-side ingest. До 200 ms собираем чанки
# с разных pid'ов, после чего вызываем GigaAM.forward() один раз на весь
# батч. 200 ms не добавляет user-visible latency при 3-секундных чанках.
PCM_BATCH_INTERVAL_MS = 200

# Максимальный размер batch'а для GigaAM.forward(). Бенч дал 60× realtime
# на B=8 (vs 30× на B=1); выше падает отдача при линейно растущем padding.
PCM_MAX_BATCH_SIZE = 8

# Overlap между соседними чанками (производная от D5: payload − stride).
# Явная константа для _remove_duplicate_words и тестов на границах.
PCM_CHUNK_OVERLAP_MS = 300

# Silence watchdog: если в активной KTalk-встрече PCM-чанков нет дольше
# этого окна, PerSpeakerIngest переходит в degraded-mode и отдаёт
# управление mixed-path fallback'у. 30 s достаточно, чтобы пропустить
# flap reconnect, но не упустить весь разговор.
PCM_SILENCE_WATCHDOG_S = 30.0


def pcm_transport_enabled() -> bool:
    """Читает RECOG_PCM_TRANSPORT env var и возвращает True если включён.

    Server-authoritative gate: extension получает этот флаг через handshake
    от bridge на WS-connect и шлёт PCM только если флаг True. Нет отдельного
    toggle в popup/chrome.storage — один источник правды.
    """
    return os.environ.get("RECOG_PCM_TRANSPORT", "0") == "1"
