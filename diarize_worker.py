#!/usr/bin/env python3
"""
Воркер диаризации для pyannote.audio 4.x.
Запускается как subprocess из mp4_notes.py.

Протокол:
- stdin: JSON с параметрами
- stdout: JSON с результатами
- stderr: логи и прогресс
"""

import json
import os
import sys
from typing import Dict, List, Optional

import numpy as np
import torch

# PyTorch 2.8+ requires safe globals for loading pyannote models
# Patch both torch.load and lightning's pl_load
import torch.serialization
_original_torch_load = torch.load
def _patched_torch_load(*args, **kwargs):
    kwargs['weights_only'] = False
    return _original_torch_load(*args, **kwargs)
torch.load = _patched_torch_load

# Also patch lightning's loader
try:
    from lightning.fabric.utilities import cloud_io
    _original_pl_load = cloud_io._load
    def _patched_pl_load(path_or_url, map_location=None):
        return torch.load(path_or_url, map_location=map_location, weights_only=False)
    cloud_io._load = _patched_pl_load
except:
    pass


def run_diarization(
    audio_path: str,
    hf_token: str,
    similarity_threshold: float = 0.60,
    use_parallel: bool = True,
    segment_duration: float = 60.0
) -> dict:
    """
    Выполняет диаризацию с pyannote.audio 4.x.

    Returns:
        {
            "segments": [{"start": float, "end": float, "speaker_id": str}, ...],
            "embeddings": {"speaker_id": [float, ...], ...}
        }
    """
    import soundfile as sf
    import torch
    from tqdm import tqdm

    # Загрузка pipeline
    print("Загрузка pyannote 4.0 (community-1)...", file=sys.stderr)

    from pyannote.audio import Pipeline

    pipeline = Pipeline.from_pretrained(
        "pyannote/speaker-diarization-community-1",
        token=hf_token
    )

    if torch.cuda.is_available():
        pipeline.to(torch.device("cuda"))
    elif torch.backends.mps.is_available():
        pipeline.to(torch.device("mps"))

    # Загружаем аудио через soundfile (обход проблемы с torchcodec)
    waveform, sample_rate = sf.read(audio_path)
    if len(waveform.shape) == 1:
        waveform = waveform.reshape(1, -1)  # mono -> (1, samples)
    else:
        waveform = waveform.T  # (samples, channels) -> (channels, samples)

    waveform_tensor = torch.from_numpy(waveform).float()
    duration = waveform_tensor.shape[-1] / sample_rate

    print(f"Диаризация ({duration:.1f} сек)...", file=sys.stderr)

    # Передаём как dict с waveform (обход torchcodec)
    audio_input = {"waveform": waveform_tensor, "sample_rate": sample_rate}
    diarization = pipeline(audio_input)

    # Конвертируем в список сегментов (pyannote 4.x API)
    segments = []
    for turn, speaker in diarization.speaker_diarization:
        segments.append({
            "start": turn.start,
            "end": turn.end,
            "speaker_id": str(speaker)
        })

    # Нормализуем speaker_id
    speaker_map = {}
    speaker_counter = 0
    normalized_segments = []

    for seg in segments:
        if seg["speaker_id"] not in speaker_map:
            speaker_map[seg["speaker_id"]] = f"speaker_{speaker_counter}"
            speaker_counter += 1

        normalized_segments.append({
            "start": seg["start"],
            "end": seg["end"],
            "speaker_id": speaker_map[seg["speaker_id"]]
        })

    print(f"Найдено {len(speaker_map)} спикеров", file=sys.stderr)

    # Извлекаем embeddings
    embeddings = extract_embeddings(audio_path, normalized_segments, hf_token)

    # Кластеризация
    if len(embeddings) > 2:
        normalized_segments, embeddings = cluster_speakers(
            normalized_segments,
            embeddings,
            similarity_threshold
        )

    return {
        "segments": normalized_segments,
        "embeddings": {k: v.tolist() for k, v in embeddings.items()}
    }


def extract_embeddings(
    audio_path: str,
    segments: List[dict],
    hf_token: str
) -> Dict[str, np.ndarray]:
    """Извлекает embeddings для каждого спикера."""
    import soundfile as sf
    from tqdm import tqdm
    from pyannote.audio import Model, Inference

    print("Извлечение embeddings...", file=sys.stderr)

    # Загружаем аудио через soundfile (обход torchcodec)
    waveform, sample_rate = sf.read(audio_path)
    if len(waveform.shape) == 1:
        waveform = waveform.reshape(1, -1)
    else:
        waveform = waveform.T

    model = Model.from_pretrained(
        "pyannote/wespeaker-voxceleb-resnet34-LM",
        token=hf_token
    )
    inference = Inference(model, window="whole")

    # Группируем сегменты по спикерам
    speaker_segments: Dict[str, List[dict]] = {}
    for seg in segments:
        sid = seg["speaker_id"]
        if sid not in speaker_segments:
            speaker_segments[sid] = []
        speaker_segments[sid].append(seg)

    embeddings = {}

    for speaker_id in tqdm(speaker_segments.keys(), desc="Embeddings", file=sys.stderr):
        segs = speaker_segments[speaker_id]
        # Берём самые длинные сегменты
        sorted_segs = sorted(segs, key=lambda s: s["end"] - s["start"], reverse=True)[:3]

        seg_embeddings = []
        for seg in sorted_segs:
            if seg["end"] - seg["start"] >= 1.0:
                try:
                    # Извлекаем сегмент вручную
                    start_sample = int(seg["start"] * sample_rate)
                    end_sample = int(seg["end"] * sample_rate)
                    segment_waveform = waveform[:, start_sample:end_sample]

                    # Передаём как dict с waveform
                    audio_input = {
                        "waveform": torch.from_numpy(segment_waveform).float(),
                        "sample_rate": sample_rate
                    }
                    emb = inference(audio_input)
                    if hasattr(emb, 'numpy'):
                        emb = emb.numpy()
                    seg_embeddings.append(np.array(emb))
                except Exception as e:
                    print(f"Ошибка embedding для {speaker_id}: {e}", file=sys.stderr)

        if seg_embeddings:
            embeddings[speaker_id] = np.mean(seg_embeddings, axis=0)

    return embeddings


def cluster_speakers(
    segments: List[dict],
    embeddings: Dict[str, np.ndarray],
    similarity_threshold: float
) -> tuple:
    """Кластеризует спикеров по сходству embeddings."""
    from sklearn.cluster import AgglomerativeClustering

    if len(embeddings) <= 1:
        return segments, embeddings

    print(f"Кластеризация {len(embeddings)} спикеров (threshold={similarity_threshold})...", file=sys.stderr)

    speaker_ids = list(embeddings.keys())
    emb_matrix = np.array([embeddings[s] for s in speaker_ids])

    clustering = AgglomerativeClustering(
        n_clusters=None,
        distance_threshold=1 - similarity_threshold,
        metric='cosine',
        linkage='average'
    )
    labels = clustering.fit_predict(emb_matrix)

    # Создаём маппинг
    cluster_to_canonical = {}
    speaker_mapping = {}

    for speaker_id, label in zip(speaker_ids, labels):
        if label not in cluster_to_canonical:
            cluster_to_canonical[label] = f"speaker_{len(cluster_to_canonical)}"
        speaker_mapping[speaker_id] = cluster_to_canonical[label]

    print(f"Объединено в {len(cluster_to_canonical)} уникальных спикеров", file=sys.stderr)

    # Применяем маппинг к сегментам
    new_segments = []
    for seg in segments:
        new_segments.append({
            "start": seg["start"],
            "end": seg["end"],
            "speaker_id": speaker_mapping.get(seg["speaker_id"], seg["speaker_id"])
        })

    # Пересчитываем embeddings для кластеров
    new_embeddings = {}
    for old_id, new_id in speaker_mapping.items():
        if old_id in embeddings:
            if new_id not in new_embeddings:
                new_embeddings[new_id] = []
            new_embeddings[new_id].append(embeddings[old_id])

    # Усредняем embeddings в кластерах
    final_embeddings = {}
    for speaker_id, emb_list in new_embeddings.items():
        final_embeddings[speaker_id] = np.mean(emb_list, axis=0)

    return new_segments, final_embeddings


def main():
    # Читаем входные данные из stdin
    input_data = json.load(sys.stdin)

    audio_path = input_data["audio_path"]
    hf_token = input_data.get("hf_token") or os.environ.get("HF_TOKEN")
    similarity_threshold = input_data.get("similarity_threshold", 0.60)

    if not hf_token:
        print("Ошибка: HF_TOKEN не указан", file=sys.stderr)
        sys.exit(1)

    try:
        result = run_diarization(
            audio_path=audio_path,
            hf_token=hf_token,
            similarity_threshold=similarity_threshold
        )

        # Выводим результат в stdout
        print(json.dumps(result))

    except Exception as e:
        print(f"Ошибка диаризации: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
