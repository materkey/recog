"""
Модели данных для Recog.
"""

from dataclasses import dataclass
from typing import List


@dataclass
class Segment:
    """Сегмент транскрипции с информацией о спикере."""
    text: str
    start: float
    end: float
    speaker_id: str = "speaker_0"
    speaker_label: str = "speaker_0"


@dataclass
class EmbeddingSample:
    """Один образец embedding с метаданными."""
    embedding: List[float]
    source_file: str = ""
    start_time: float = 0.0
    end_time: float = 0.0
    transcript: str = ""


@dataclass
class SpeakerInfo:
    """Информация о спикере в базе."""
    display_name: str
    embeddings: List[EmbeddingSample] = None  # Несколько образцов с метаданными
    # Для обратной совместимости со старым форматом
    embedding: List[float] = None  # deprecated, use embeddings

    def __post_init__(self):
        if self.embeddings is None:
            self.embeddings = []
