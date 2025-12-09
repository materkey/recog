"""
Модуль диаризации спикеров.
"""

import json
import os
import subprocess as sp
import sys
from typing import Dict, List, Optional, Tuple

import numpy as np
import torch

from .config import PYANNOTE4_VENV, DIARIZE_WORKER
from .models import DiarizationSegment


def run_diarization_pyannote4(
    audio_path: str,
    hf_token: Optional[str] = None
) -> Tuple[List[DiarizationSegment], Dict[str, np.ndarray]]:
    """
    Запускает диаризацию через subprocess с pyannote 4.x.
    ADR-0007: унифицированный пайплайн speaker-diarization-3.1

    Returns:
        (segments, embeddings)
    """
    python_path = PYANNOTE4_VENV / "bin" / "python"

    if not python_path.exists():
        raise RuntimeError(f"venv-pyannote не найден: {python_path}")

    if not DIARIZE_WORKER.exists():
        raise RuntimeError(f"diarize_worker.py не найден: {DIARIZE_WORKER}")

    hf_token = hf_token or os.environ.get("HF_TOKEN")

    # ADR-0009: убран similarity_threshold (нет двойной кластеризации)
    input_data = json.dumps({
        "audio_path": audio_path,
        "hf_token": hf_token
    })

    result = sp.run(
        [str(python_path), str(DIARIZE_WORKER)],
        input=input_data,
        capture_output=True,
        text=True
    )

    if result.returncode != 0:
        print(f"diarize_worker stderr: {result.stderr}", file=sys.stderr)
        raise RuntimeError(f"diarize_worker.py failed: {result.returncode}")

    output = json.loads(result.stdout)

    segments = [
        DiarizationSegment(
            start=s["start"],
            end=s["end"],
            speaker_id=s["speaker_id"]
        )
        for s in output["segments"]
    ]

    embeddings = {
        k: np.array(v)
        for k, v in output.get("embeddings", {}).items()
    }

    return segments, embeddings


def _diarize_segment_worker(args):
    """
    Воркер для параллельной диаризации сегмента.
    Должен быть на уровне модуля для pickle в ProcessPoolExecutor.
    """
    seg_idx, audio_segment, sample_rate, start_time, hf_token = args

    import tempfile
    import os
    import torch

    # Каждый процесс загружает свой pipeline
    try:
        from pyannote.audio import Pipeline

        pipeline = Pipeline.from_pretrained(
            "pyannote/speaker-diarization-3.1",
            use_auth_token=hf_token
        )

        if torch.cuda.is_available():
            pipeline.to(torch.device("cuda"))

    except Exception as e:
        return []

    # Сохраняем сегмент во временный файл
    import soundfile as sf

    with tempfile.NamedTemporaryFile(suffix='.wav', delete=False) as f:
        tmp_path = f.name
        sf.write(tmp_path, audio_segment, sample_rate)

    try:
        seg_diarization = pipeline(tmp_path, num_speakers=None)
        results = []
        for turn, _, speaker in seg_diarization.itertracks(yield_label=True):
            results.append({
                'start': turn.start + start_time,
                'end': turn.end + start_time,
                'speaker': f"seg{seg_idx}_{speaker}"
            })
        return results
    finally:
        os.remove(tmp_path)


class DiarizationModule:
    """
    Модуль диаризации спикеров.
    Использует pyannote.audio для определения, кто и когда говорит.
    """

    def __init__(self, hf_token: Optional[str] = None):
        self.hf_token = hf_token or os.environ.get("HF_TOKEN")
        self.pipeline = None
        self.embedding_model = None

    def _load_pipeline(self):
        """Загружает pipeline для диаризации."""
        if self.pipeline is None:
            if not self.hf_token:
                print("Предупреждение: HF_TOKEN не установлен.", file=sys.stderr)
                print("Диаризация требует токен HuggingFace для загрузки моделей pyannote.", file=sys.stderr)
                print("Установите: export HF_TOKEN='your_token'", file=sys.stderr)
                print("Будет использован fallback (один спикер).", file=sys.stderr)
                return None

            try:
                from pyannote.audio import Pipeline

                print("Загрузка модели диаризации...", file=sys.stderr)
                self.pipeline = Pipeline.from_pretrained(
                    "pyannote/speaker-diarization-3.1",
                    use_auth_token=self.hf_token
                )

                if torch.cuda.is_available():
                    self.pipeline.to(torch.device("cuda"))

            except Exception as e:
                print(f"Предупреждение: не удалось загрузить диаризацию: {e}", file=sys.stderr)
                print("Будет использован fallback (один спикер).", file=sys.stderr)
                self.pipeline = None

        return self.pipeline

    def _load_embedding_model(self):
        """Загружает модель для speaker embeddings (SpeechBrain ECAPA-TDNN)."""
        if self.embedding_model is None:
            try:
                from speechbrain.inference.speaker import EncoderClassifier

                print("Загрузка модели speaker embeddings (ECAPA-TDNN)...", file=sys.stderr)
                self.embedding_model = EncoderClassifier.from_hparams(
                    source="speechbrain/spkrec-ecapa-voxceleb",
                    savedir="/tmp/speechbrain_ecapa"
                )

            except Exception as e:
                print(f"Предупреждение: не удалось загрузить embedding модель: {e}", file=sys.stderr)
                self.embedding_model = None

        return self.embedding_model

    def diarize(self, audio_path: str) -> List[DiarizationSegment]:
        """
        Выполняет диаризацию аудио.

        Returns:
            Список сегментов с speaker_id
        """
        pipeline = self._load_pipeline()

        if pipeline is None:
            # Fallback: весь файл - один спикер
            import soundfile as sf
            info = sf.info(audio_path)
            return [DiarizationSegment(
                start=0.0,
                end=info.duration,
                speaker_id="speaker_0"
            )]

        import soundfile as sf
        from concurrent.futures import ProcessPoolExecutor, as_completed
        import multiprocessing

        info = sf.info(audio_path)
        duration = info.duration

        # Для коротких файлов - без параллелизации
        SEGMENT_DURATION = 60  # 1 минута на сегмент (больше параллелизма)
        OVERLAP = 5  # 5 сек перекрытие

        if duration <= SEGMENT_DURATION * 2:
            print(f"Выполняется диаризация ({duration:.1f} сек)...", file=sys.stderr)
            diarization = pipeline(audio_path, num_speakers=None)
        else:
            # Параллельная диаризация по сегментам
            num_segments = int(np.ceil(duration / (SEGMENT_DURATION - OVERLAP)))

            # Определяем количество воркеров
            num_workers = min(num_segments, max(1, multiprocessing.cpu_count() - 1))
            print(f"Параллельная диаризация ({duration:.1f} сек, {num_segments} сегментов, {num_workers} воркеров)...", file=sys.stderr)

            # Загружаем аудио
            audio_data, sr = sf.read(audio_path)
            if len(audio_data.shape) > 1:
                audio_data = audio_data.mean(axis=1)  # stereo -> mono

            # Подготавливаем аргументы для воркеров
            worker_args = []
            for seg_idx in range(num_segments):
                start_time = seg_idx * (SEGMENT_DURATION - OVERLAP)
                end_time = min(start_time + SEGMENT_DURATION, duration)

                start_sample = int(start_time * sr)
                end_sample = int(end_time * sr)
                segment_audio = audio_data[start_sample:end_sample]

                worker_args.append((
                    seg_idx,
                    segment_audio,
                    sr,
                    start_time,
                    self.hf_token
                ))

            # Запускаем параллельную обработку
            all_results = []
            from tqdm import tqdm

            with ProcessPoolExecutor(max_workers=num_workers) as executor:
                futures = {executor.submit(_diarize_segment_worker, args): args[0] for args in worker_args}

                with tqdm(total=num_segments, desc="Диаризация", file=sys.stderr) as pbar:
                    for future in as_completed(futures):
                        seg_idx = futures[future]
                        try:
                            results = future.result()
                            all_results.extend(results)
                        except Exception as e:
                            print(f"Ошибка в сегменте {seg_idx}: {e}", file=sys.stderr)
                        pbar.update(1)

            # Сортируем по времени
            all_results.sort(key=lambda x: x['start'])

            # Создаём Annotation для объединения
            from pyannote.core import Annotation, Segment as PyannoteSegment
            diarization = Annotation()
            for r in all_results:
                diarization[PyannoteSegment(r['start'], r['end'])] = r['speaker']

        segments = []
        for turn, _, speaker in diarization.itertracks(yield_label=True):
            segments.append(DiarizationSegment(
                start=turn.start,
                end=turn.end,
                speaker_id=speaker
            ))

        # Нормализуем speaker_id в speaker_0, speaker_1, ...
        speaker_map = {}
        speaker_counter = 0

        normalized_segments = []
        for seg in segments:
            if seg.speaker_id not in speaker_map:
                speaker_map[seg.speaker_id] = f"speaker_{speaker_counter}"
                speaker_counter += 1

            normalized_segments.append(DiarizationSegment(
                start=seg.start,
                end=seg.end,
                speaker_id=speaker_map[seg.speaker_id]
            ))

        return normalized_segments

    def get_embedding(self, audio_path: str, start: float = None, end: float = None) -> Optional[np.ndarray]:
        """
        Вычисляет speaker embedding для аудио или его части (SpeechBrain ECAPA-TDNN).

        Args:
            audio_path: путь к аудиофайлу
            start: начало сегмента (опционально)
            end: конец сегмента (опционально)

        Returns:
            numpy array с embedding или None при ошибке
        """
        embedding_model = self._load_embedding_model()

        if embedding_model is None:
            return None

        try:
            import soundfile as sf

            waveform, sample_rate = sf.read(audio_path)
            if len(waveform.shape) > 1:
                waveform = waveform.mean(axis=1)  # stereo -> mono

            if start is not None and end is not None:
                start_sample = int(start * sample_rate)
                end_sample = int(end * sample_rate)
                waveform = waveform[start_sample:end_sample]

            audio_tensor = torch.from_numpy(waveform).float().unsqueeze(0)
            embedding = embedding_model.encode_batch(audio_tensor)

            return embedding.squeeze().numpy()

        except Exception as e:
            print(f"Ошибка при вычислении embedding: {e}", file=sys.stderr)
            return None

    def get_speaker_embeddings(
        self,
        audio_path: str,
        diarization_segments: List[DiarizationSegment],
        max_segments_per_speaker: int = 5
    ) -> Dict[str, np.ndarray]:
        """
        Вычисляет embedding для каждого спикера на основе диаризации.

        Returns:
            Dict[speaker_id -> embedding]
        """
        # Группируем сегменты по спикерам
        speaker_segments: Dict[str, List[DiarizationSegment]] = {}
        for seg in diarization_segments:
            if seg.speaker_id not in speaker_segments:
                speaker_segments[seg.speaker_id] = []
            speaker_segments[seg.speaker_id].append(seg)

        embeddings = {}

        for speaker_id, segments in speaker_segments.items():
            # Берём самые длинные сегменты спикера
            sorted_segments = sorted(segments, key=lambda s: s.end - s.start, reverse=True)
            selected = sorted_segments[:max_segments_per_speaker]

            # Вычисляем embedding для каждого сегмента и усредняем
            segment_embeddings = []
            for seg in selected:
                # Минимальная длина сегмента для embedding - 1 секунда
                if seg.end - seg.start < 1.0:
                    continue

                emb = self.get_embedding(audio_path, seg.start, seg.end)
                if emb is not None:
                    segment_embeddings.append(emb)

            if segment_embeddings:
                embeddings[speaker_id] = np.mean(segment_embeddings, axis=0)

        return embeddings

    def cluster_speakers(
        self,
        audio_path: str,
        diarization_segments: List[DiarizationSegment],
        similarity_threshold: float = 0.50
    ) -> List[DiarizationSegment]:
        """
        Кластеризует спикеров по голосовым embeddings.
        Объединяет спикеров с похожими голосами в один ID.

        Args:
            audio_path: путь к аудиофайлу
            diarization_segments: сегменты после диаризации
            similarity_threshold: порог сходства для объединения (0.0-1.0)

        Returns:
            Сегменты с объединёнными speaker_id
        """
        from tqdm import tqdm

        unique_speakers = list(set(seg.speaker_id for seg in diarization_segments))

        if len(unique_speakers) <= 1:
            return diarization_segments

        print(f"Кластеризация {len(unique_speakers)} спикеров...", file=sys.stderr)

        # Извлекаем embeddings для каждого спикера
        embeddings = {}
        for speaker_id in tqdm(unique_speakers, desc="Embeddings", file=sys.stderr):
            speaker_segs = [s for s in diarization_segments if s.speaker_id == speaker_id]
            # Берём самые длинные сегменты
            sorted_segs = sorted(speaker_segs, key=lambda s: s.end - s.start, reverse=True)[:3]

            seg_embeddings = []
            for seg in sorted_segs:
                if seg.end - seg.start >= 1.0:
                    emb = self.get_embedding(audio_path, seg.start, seg.end)
                    if emb is not None:
                        seg_embeddings.append(emb)

            if seg_embeddings:
                embeddings[speaker_id] = np.mean(seg_embeddings, axis=0)

        # Если мало embeddings, возвращаем как есть
        if len(embeddings) <= 1:
            return diarization_segments

        # Кластеризация с AgglomerativeClustering
        try:
            from sklearn.cluster import AgglomerativeClustering

            speaker_ids = list(embeddings.keys())
            emb_matrix = np.array([embeddings[s] for s in speaker_ids])

            # distance_threshold = 1 - similarity_threshold для косинусной метрики
            clustering = AgglomerativeClustering(
                n_clusters=None,
                distance_threshold=1 - similarity_threshold,
                metric='cosine',
                linkage='average'
            )
            labels = clustering.fit_predict(emb_matrix)

            # Создаём маппинг старых ID в новые
            cluster_to_canonical = {}
            speaker_mapping = {}

            for speaker_id, label in zip(speaker_ids, labels):
                if label not in cluster_to_canonical:
                    cluster_to_canonical[label] = f"speaker_{len(cluster_to_canonical)}"
                speaker_mapping[speaker_id] = cluster_to_canonical[label]

            num_clusters = len(cluster_to_canonical)
            print(f"Объединено в {num_clusters} уникальных спикеров", file=sys.stderr)

            # Применяем маппинг
            clustered_segments = []
            for seg in diarization_segments:
                new_id = speaker_mapping.get(seg.speaker_id, seg.speaker_id)
                clustered_segments.append(DiarizationSegment(
                    start=seg.start,
                    end=seg.end,
                    speaker_id=new_id
                ))

            return clustered_segments

        except ImportError:
            print("sklearn не установлен, кластеризация пропущена", file=sys.stderr)
            return diarization_segments
        except Exception as e:
            print(f"Ошибка кластеризации: {e}", file=sys.stderr)
            return diarization_segments
