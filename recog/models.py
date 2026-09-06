"""
Модели данных для Recog.
"""

from dataclasses import dataclass


@dataclass
class Segment:
    """Сегмент транскрипции с информацией о спикере."""
    text: str
    start: float
    end: float
    speaker_id: str = "speaker_0"
    speaker_label: str = "speaker_0"
