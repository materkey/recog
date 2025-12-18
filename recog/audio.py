"""
Утилиты для работы с аудио.
"""

import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Optional, Tuple


def extract_audio_to_wav(input_path: str, output_path: str, sample_rate: int = 16000) -> bool:
    """
    Извлекает аудио из видео/аудио файла в WAV (mono, 16kHz).
    Использует ffmpeg.
    """
    try:
        cmd = [
            "ffmpeg", "-y", "-i", input_path,
            "-vn",  # без видео
            "-acodec", "pcm_s16le",
            "-ar", str(sample_rate),
            "-ac", "1",  # mono
            output_path
        ]
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=True
        )
        return True
    except subprocess.CalledProcessError as e:
        print(f"Ошибка ffmpeg: {e.stderr}", file=sys.stderr)
        return False
    except FileNotFoundError:
        print("Ошибка: ffmpeg не найден. Установите ffmpeg.", file=sys.stderr)
        return False


def get_audio_path(input_path: str, temp_dir: Optional[str] = None) -> Tuple[str, bool]:
    """
    Возвращает путь к WAV-файлу.
    Если вход уже WAV - возвращает его.
    Иначе конвертирует во временный WAV.

    Returns:
        (путь к wav, нужно ли удалять файл после использования)
    """
    path = Path(input_path)
    if path.suffix.lower() == ".wav":
        return str(path), False

    if temp_dir is None:
        temp_dir = tempfile.gettempdir()

    temp_dir_path = Path(temp_dir)
    temp_dir_path.mkdir(parents=True, exist_ok=True)

    with tempfile.NamedTemporaryFile(
        prefix=f"{path.stem}_",
        suffix=".wav",
        dir=temp_dir_path,
        delete=False,
    ) as tmp:
        wav_path = tmp.name

    if extract_audio_to_wav(input_path, wav_path):
        return wav_path, True

    try:
        os.remove(wav_path)
    except OSError:
        pass
    raise RuntimeError(f"Не удалось извлечь аудио из {input_path}")
