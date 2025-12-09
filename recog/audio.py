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

    wav_path = os.path.join(temp_dir, f"{path.stem}_temp.wav")

    if extract_audio_to_wav(input_path, wav_path):
        return wav_path, True
    else:
        raise RuntimeError(f"Не удалось извлечь аудио из {input_path}")
