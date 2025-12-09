"""
Основные пайплайны транскрибирования.
"""

import os
import sys
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np

from .config import DEFAULT_MODEL, DEFAULT_CHUNK_DURATION, DEFAULT_OVERLAP
from .models import DiarizationSegment
from .audio import get_audio_path
from .asr import ASRModule
from .diarization import DiarizationModule, run_diarization_pyannote4
from .speaker_db import SpeakerDatabase
from .alignment import align_segments_with_speakers, resolve_speaker_labels
from .output import generate_markdown


def transcribe_pipeline(
    input_path: str,
    output_path: str,
    model_name: str = DEFAULT_MODEL,
    mode: str = "simple",
    device: Optional[str] = None,
    speaker_db_path: Optional[str] = None,
    include_timestamps: bool = True,
    no_diarization: bool = False,
    chunk_duration: float = DEFAULT_CHUNK_DURATION,
    overlap: float = DEFAULT_OVERLAP
):
    """
    Основной пайплайн транскрибирования.
    """
    input_file = Path(input_path)
    if not input_file.exists():
        print(f"Ошибка: файл '{input_path}' не найден", file=sys.stderr)
        sys.exit(1)

    # Инициализация модулей
    asr = ASRModule(model_name=model_name, device=device)
    diarization = DiarizationModule()
    speaker_db = SpeakerDatabase(speaker_db_path) if speaker_db_path else SpeakerDatabase()

    # Подготовка аудио
    print(f"Обработка файла: {input_path}", file=sys.stderr)
    wav_path, need_cleanup = get_audio_path(input_path)

    try:
        # 1. ASR транскрибирование
        print(f"Транскрибирование (режим: {mode})...", file=sys.stderr)
        if mode == "longform":
            asr_segments = asr.transcribe_longform(wav_path)
        else:
            asr_segments = asr.transcribe_simple(
                wav_path,
                chunk_duration=chunk_duration,
                overlap=overlap
            )

        print(f"Получено {len(asr_segments)} сегментов транскрипции.", file=sys.stderr)

        # 2. Диаризация (ADR-0007: унифицированный pyannote 4.x)
        if no_diarization:
            print("Диаризация отключена, используется один спикер.", file=sys.stderr)
            import soundfile as sf
            info = sf.info(wav_path)
            diar_segments = [DiarizationSegment(
                start=0.0,
                end=info.duration,
                speaker_id="speaker_0"
            )]
            speaker_embeddings = {}
        else:
            # ADR-0007: только pyannote 4.x через subprocess
            # ADR-0009: без двойной кластеризации
            diar_segments, speaker_embeddings = run_diarization_pyannote4(wav_path)
            print(f"Получено {len(diar_segments)} сегментов диаризации.", file=sys.stderr)

        # 3. Сопоставление ASR и спикеров
        aligned_segments = align_segments_with_speakers(asr_segments, diar_segments)

        # 4. Идентификация известных спикеров
        if speaker_db_path and speaker_embeddings:
            aligned_segments = resolve_speaker_labels(
                aligned_segments,
                speaker_embeddings,
                speaker_db
            )

        # 5. Генерация Markdown
        title = input_file.stem
        markdown = generate_markdown(
            aligned_segments,
            title=title,
            include_timestamps=include_timestamps
        )

        # 6. Сохранение
        output_file = Path(output_path)
        output_file.write_text(markdown, encoding='utf-8')

        print(f"\nРезультат сохранён в {output_path}", file=sys.stderr)
        print(markdown)

    finally:
        # Очистка временного файла
        if need_cleanup and os.path.exists(wav_path):
            os.remove(wav_path)


def enroll_speaker_pipeline(
    speaker_id: str,
    audio_files: List[str],
    display_name: str,
    speaker_db_path: str
):
    """
    Пайплайн добавления спикера в базу.
    """
    diarization = DiarizationModule()
    speaker_db = SpeakerDatabase(speaker_db_path)

    all_embeddings = []

    for audio_file in audio_files:
        print(f"Обработка файла: {audio_file}", file=sys.stderr)

        wav_path, need_cleanup = get_audio_path(audio_file)

        try:
            embedding = diarization.get_embedding(wav_path)
            if embedding is not None:
                all_embeddings.append(embedding)
                print(f"  Embedding извлечён.", file=sys.stderr)
            else:
                print(f"  Не удалось извлечь embedding.", file=sys.stderr)
        finally:
            if need_cleanup and os.path.exists(wav_path):
                os.remove(wav_path)

    if not all_embeddings:
        print("Ошибка: не удалось извлечь ни одного embedding.", file=sys.stderr)
        sys.exit(1)

    # Усредняем embeddings
    avg_embedding = np.mean(all_embeddings, axis=0)

    # Добавляем в базу
    speaker_db.add_speaker(speaker_id, display_name, avg_embedding)
    speaker_db.save()

    print(f"\nСпикер '{display_name}' (id: {speaker_id}) добавлен в базу.", file=sys.stderr)


def build_speaker_db_pipeline(
    input_path: str,
    speaker_db_path: str,
    speaker_mapping: Optional[Dict[str, str]] = None
):
    """
    Пайплайн для создания базы спикеров из файла.

    Выполняет диаризацию, извлекает эмбеддинги и сохраняет в базу.
    Можно указать маппинг speaker_id -> display_name.
    """
    input_file = Path(input_path)
    if not input_file.exists():
        print(f"Ошибка: файл '{input_path}' не найден", file=sys.stderr)
        sys.exit(1)

    diarization = DiarizationModule()
    speaker_db = SpeakerDatabase(speaker_db_path)

    # Подготовка аудио
    print(f"Обработка файла: {input_path}", file=sys.stderr)
    wav_path, need_cleanup = get_audio_path(input_path)

    try:
        # Диаризация
        diar_segments = diarization.diarize(wav_path)
        print(f"Найдено спикеров: {len(set(s.speaker_id for s in diar_segments))}", file=sys.stderr)

        # Получаем эмбеддинги
        speaker_embeddings = diarization.get_speaker_embeddings(wav_path, diar_segments)

        # Сохраняем в базу
        for speaker_id, embedding in speaker_embeddings.items():
            display_name = speaker_mapping.get(speaker_id, speaker_id) if speaker_mapping else speaker_id
            speaker_db.add_speaker(speaker_id, display_name, embedding)
            print(f"  Добавлен: {speaker_id} -> {display_name}", file=sys.stderr)

        speaker_db.save()
        print(f"\nБаза спикеров сохранена в {speaker_db_path}", file=sys.stderr)

    finally:
        if need_cleanup and os.path.exists(wav_path):
            os.remove(wav_path)
