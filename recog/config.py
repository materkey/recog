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

# Имя спикера для микрофонного входа (live transcription)
MIC_SPEAKER_NAME = os.environ.get("MIC_SPEAKER_NAME", "").strip() or "Me"

# Путь к venv для pyannote 4.x
PYANNOTE4_VENV = Path(__file__).parent.parent / "venv-pyannote"
DIARIZE_WORKER = Path(__file__).parent.parent / "diarize_worker.py"
