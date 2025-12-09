"""
Функции для сопоставления ASR-сегментов с диаризацией.
"""

import sys
from typing import Dict, List

import numpy as np

from .models import Segment, DiarizationSegment
from .speaker_db import SpeakerDatabase


def align_segments_with_speakers(
    asr_segments: List[Segment],
    diarization_segments: List[DiarizationSegment]
) -> List[Segment]:
    """
    Сопоставляет ASR-сегменты с диаризационными сегментами.
    Каждому ASR-сегменту присваивается speaker_id с максимальным пересечением.
    """
    aligned = []

    for asr_seg in asr_segments:
        best_speaker = "speaker_0"
        best_overlap = 0.0

        for diar_seg in diarization_segments:
            # Вычисляем пересечение интервалов
            overlap_start = max(asr_seg.start, diar_seg.start)
            overlap_end = min(asr_seg.end, diar_seg.end)
            overlap = max(0.0, overlap_end - overlap_start)

            if overlap > best_overlap:
                best_overlap = overlap
                best_speaker = diar_seg.speaker_id

        aligned.append(Segment(
            text=asr_seg.text,
            start=asr_seg.start,
            end=asr_seg.end,
            speaker_id=best_speaker,
            speaker_label=best_speaker
        ))

    return aligned


def resolve_speaker_labels(
    segments: List[Segment],
    speaker_embeddings: Dict[str, np.ndarray],
    speaker_db: SpeakerDatabase
) -> List[Segment]:
    """
    Заменяет speaker_id на известные имена из базы, если возможно.
    """
    # Создаём маппинг speaker_id -> display_name
    speaker_labels = {}

    for speaker_id, embedding in speaker_embeddings.items():
        match = speaker_db.match_speaker(embedding)
        if match:
            db_id, display_name, similarity = match
            speaker_labels[speaker_id] = display_name
            print(f"Спикер {speaker_id} идентифицирован как '{display_name}' (сходство: {similarity:.2f})",
                  file=sys.stderr)
        else:
            # Логируем лучший матч даже если не прошёл порог
            best_match = speaker_db.match_speaker(embedding, threshold=0.0)
            if best_match:
                _, best_name, best_sim = best_match
                print(f"Спикер {speaker_id} не идентифицирован (лучший: '{best_name}' = {best_sim:.2f})",
                      file=sys.stderr)
            speaker_labels[speaker_id] = speaker_id

    # Применяем маппинг к сегментам
    resolved = []
    for seg in segments:
        label = speaker_labels.get(seg.speaker_id, seg.speaker_id)
        resolved.append(Segment(
            text=seg.text,
            start=seg.start,
            end=seg.end,
            speaker_id=seg.speaker_id,
            speaker_label=label
        ))

    return resolved
