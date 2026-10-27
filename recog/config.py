"""
Конфигурация и константы для Recog.
"""

import os
import sys
from pathlib import Path

# Чтение .env и config.toml живёт в install_config; load_dotenv остаётся
# доступным отсюда для прежних импортов.
from recog.install_config import load_dotenv as load_dotenv


def data_entries(root: Path) -> list[Path]:
    """Переносимые данные, включая старые базы спикеров, без .env."""
    candidates = [root / "meetings", root / "logs", root / "user-profile.json", root / "ai-settings.json", root / "privacy.json",
                  *sorted(root.glob("speakers*.json"))]
    return [path for path in candidates if path.exists() or path.is_symlink()]


def data_dir_target() -> Path:
    """Новый каталог: одинаковый для CLI, setup и сервисов, независимо от cwd."""
    value = os.environ.get("RECOG_DATA_DIR", "").strip()
    path = Path(value).expanduser() if value else Path.home() / ".recog"
    if not path.is_absolute():
        raise ValueError("RECOG_DATA_DIR must be an absolute path (or start with ~/)")
    return path


def get_data_dir(repo_dir: Path = None) -> Path:
    """Старую установку переключает только явная миграция или env override."""
    target = data_dir_target()
    if os.environ.get("RECOG_DATA_DIR", "").strip():
        return target
    repo_dir = repo_dir or Path(__file__).resolve().parent.parent
    legacy = data_entries(repo_dir)
    if legacy and repo_dir.resolve() != target.resolve():
        if data_entries(target):
            raise RuntimeError(
                f"Recog data exists in both {repo_dir} and {target}; "
                "resolve the conflict before starting services (scripts/migrate-data.py)."
            )
        print(
            f"Recog: using legacy data in {repo_dir}; "
            "run scripts/migrate-data.py to move it outside the checkout.",
            file=sys.stderr,
        )
        return repo_dir
    return target

# ============================================================================
# КОНФИГУРАЦИЯ
# ============================================================================

DEFAULT_MODEL = "v3_e2e_rnnt"
DEFAULT_CHUNK_DURATION = 20.0
DEFAULT_OVERLAP = 1.0

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
# PER-SPEAKER PCM TRANSPORT
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

# Длина PCM payload в одном чанке = stride + overlap (600 ms).
# Worklet держит ring-буфер на 3600 ms; каждые 3000 ms эмитит последние
# 3600 ms — последние 600 ms предыдущего чанка повторяются в начале
# следующего для _remove_duplicate_words на границах.
# Bumped from 300 → 600 ms to give dedup more sub-second context — at
# 300 ms overlap the word right on the chunk boundary could be
# transcribed slightly differently on each side, and dedup would drop
# the wrong copy. +10 % ASR work, safe at 60× realtime batched forward.
PCM_CHUNK_PAYLOAD_S = 3.6

# Окно batch-коллектора в Python-side ingest. До 200 ms собираем чанки
# с разных pid'ов, после чего вызываем GigaAM.forward() один раз на весь
# батч. 200 ms не добавляет user-visible latency при 3-секундных чанках.
PCM_BATCH_INTERVAL_MS = 200

# Максимальный размер batch'а для GigaAM.forward(). Бенч дал 60× realtime
# на B=8 (vs 30× на B=1); выше падает отдача при линейно растущем padding.
PCM_MAX_BATCH_SIZE = 8

# Overlap между соседними чанками (производная от D5: payload − stride).
# Явная константа для _remove_duplicate_words и тестов на границах.
PCM_CHUNK_OVERLAP_MS = 600

# Silence watchdog: если в активной KTalk-встрече PCM-чанков нет дольше
# этого окна, PerSpeakerIngest переходит в degraded-mode и отдаёт
# управление mixed-path fallback'у. 30 s достаточно, чтобы пропустить
# flap reconnect, но не упустить весь разговор.
PCM_SILENCE_WATCHDOG_S = 30.0

# Staleness-guard для wall-clock↔WAV якоря PCM-оффсетов (_system_wav_anchor).
# Якорь корректен только пока system-WAV пишется непрерывно (pcm-primary не
# пропускает тишину). В degraded-окне включается silence-skip и таймлайн
# перед устаревшим якорем рвётся — тогда _on_pcm_batch откатывается на старую
# формулу wav_offset_now - chunk_age вместо экстраполяции. Порог в тон
# PCM_SILENCE_WATCHDOG_S: после recovery первый же pcm-primary write освежает
# якорь в пределах chunk_duration.
PCM_WAV_ANCHOR_MAX_AGE_MS = 30_000


# ============================================================================
# PER-SPEAKER REFINE
# ============================================================================
#
# Рефайн-контур второго эшелона: поверх live-транскрипции, по мере накопления
# PCM в пер-pid ring'е, запускается `transcribe_with_timestamps` на окне и
# результат заменяет live-текст соответствующих сегментов. Включён всегда
# (work с per-speaker PCM transport).

# Длина скользящего окна refine (секунды). При срабатывании триггера
# (gap/monolog) собирается trailing window не длиннее этого значения.
PCM_REFINE_WINDOW_S: float = 20.0

# Cap на непрерывную речь одного спикера без gap'а — после этого refine
# всё равно запускается (monolog fallback), чтобы длинные монологи не
# висели без коррекции.
PCM_REFINE_MONOLOG_CAP_S: float = 15.0


# Настройки установки (ASR-бэкенд и устройство, таймауты, дедуп live, буфер
# при недоступном ASR, WS origin'ы, Zoom-приложение и др.) — типизированные
# секции INSTALL_CONFIG в recog/install_config.py (config.toml + окружение).

# ============================================================================
# ASR FALLBACK BUFFERING (docs/plans/2026-06-02-asr-fallback-buffering-design.md)
# ============================================================================

# Пока в capture-буфере не больше этого (consumer успевает: overlap + чанк),
# метка времени буфера ресинхронизируется с wall-clock на каждом блоке. Больше —
# бэклог при недоступном ASR, метки считаются по числу сэмплов (время произнесения).
CAPTURE_RESYNC_MAX_S = 6.0
