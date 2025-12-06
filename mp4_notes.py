#!/usr/bin/env python3
"""
MP4 Notes - CLI-утилита для транскрибирования аудио/видео с диаризацией спикеров.

Использует GigaAM для ASR и pyannote.audio для диаризации.
Выход: Markdown-файл с разметкой по спикерам.

Основные команды:
- transcribe: транскрибирование файла с диаризацией
- enroll-speaker: добавление известного спикера в базу

Пример:
    python mp4_notes.py transcribe example.mp4 --out example_result.md
    python mp4_notes.py enroll-speaker slava sample1.wav --display-name "Слава"
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Tuple, Union

import numpy as np
import torch

# ============================================================================
# КОНФИГУРАЦИЯ
# ============================================================================

DEFAULT_MODEL = "v3_e2e_rnnt"
DEFAULT_CHUNK_DURATION = 20.0
DEFAULT_OVERLAP = 1.0
SPEAKER_SIMILARITY_THRESHOLD = 0.75  # Порог косинусного сходства для идентификации


# ============================================================================
# DATACLASSES
# ============================================================================

@dataclass
class Segment:
    """Сегмент транскрипции с информацией о спикере."""
    text: str
    start: float
    end: float
    speaker_id: str = "speaker_0"
    speaker_label: str = "speaker_0"


@dataclass
class DiarizationSegment:
    """Сегмент диаризации (интервал спикера)."""
    start: float
    end: float
    speaker_id: str


@dataclass
class SpeakerInfo:
    """Информация о спикере в базе."""
    display_name: str
    embedding: List[float]


# ============================================================================
# AUDIO UTILS
# ============================================================================

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


# ============================================================================
# ASR MODULE (GigaAM)
# ============================================================================

class ASRModule:
    """Модуль для транскрибирования с GigaAM."""

    def __init__(self, model_name: str = DEFAULT_MODEL, device: Optional[str] = None):
        import gigaam
        from gigaam.preprocess import SAMPLE_RATE

        self.gigaam = gigaam
        self.SAMPLE_RATE = SAMPLE_RATE
        self.model_name = model_name
        self.device = device
        self.model = None

    def load_model(self):
        """Загружает модель GigaAM."""
        if self.model is None:
            print(f"Загрузка модели {self.model_name}...", file=sys.stderr)
            self.model = self.gigaam.load_model(self.model_name, device=self.device)
        return self.model

    def transcribe_longform(self, audio_path: str) -> List[Segment]:
        """Транскрибирует с использованием longform режима (pyannote VAD)."""
        model = self.load_model()
        results = model.transcribe_longform(audio_path)
        results = self._remove_duplicate_words(results)

        segments = []
        for r in results:
            start, end = r["boundaries"]
            segments.append(Segment(
                text=r["transcription"],
                start=start,
                end=end
            ))
        return segments

    def transcribe_simple(
        self,
        audio_path: str,
        chunk_duration: float = DEFAULT_CHUNK_DURATION,
        overlap: float = DEFAULT_OVERLAP
    ) -> List[Segment]:
        """
        Транскрибирует, разбивая на фиксированные чанки.
        Не требует pyannote VAD.
        """
        from gigaam.preprocess import load_audio
        from tqdm import tqdm

        model = self.load_model()
        audio = load_audio(audio_path)
        total_samples = audio.shape[0]
        total_duration = total_samples / self.SAMPLE_RATE

        chunk_samples = int(chunk_duration * self.SAMPLE_RATE)
        overlap_samples = int(overlap * self.SAMPLE_RATE)
        step_samples = chunk_samples - overlap_samples

        device = model._device
        dtype = model._dtype

        results = []
        start_sample = 0

        # Подсчёт количества чанков для прогресс-бара
        num_chunks = max(1, (total_samples - overlap_samples) // step_samples + 1)
        print(f"Длительность аудио: {total_duration:.1f} сек ({total_duration/60:.1f} мин)", file=sys.stderr)

        with tqdm(total=num_chunks, desc="ASR", unit="chunk", file=sys.stderr) as pbar:
            while start_sample < total_samples:
                end_sample = min(start_sample + chunk_samples, total_samples)
                chunk = audio[start_sample:end_sample]

                if chunk.shape[0] < self.SAMPLE_RATE * 0.5:
                    break

                wav = chunk.to(device).to(dtype).unsqueeze(0)
                length = torch.full([1], wav.shape[-1], device=device)

                with torch.inference_mode():
                    encoded, encoded_len = model.forward(wav, length)
                    text = model.decoding.decode(model.head, encoded, encoded_len)[0]

                if text.strip():
                    start_time = start_sample / self.SAMPLE_RATE
                    end_time = end_sample / self.SAMPLE_RATE
                    results.append({
                        "transcription": text,
                        "boundaries": (start_time, end_time)
                    })

                start_sample += step_samples
                pbar.update(1)

        results = self._remove_duplicate_words(results)

        segments = []
        for r in results:
            start, end = r["boundaries"]
            segments.append(Segment(
                text=r["transcription"],
                start=start,
                end=end
            ))
        return segments

    def _remove_duplicate_words(
        self,
        results: List[Dict],
        max_overlap_words: int = 10
    ) -> List[Dict]:
        """Удаляет дублирующиеся слова на границах сегментов."""
        if len(results) < 2:
            return results

        cleaned = [results[0].copy()]

        for i in range(1, len(results)):
            prev_text = cleaned[-1]["transcription"]
            curr_text = results[i]["transcription"]

            prev_words = prev_text.split()
            curr_words = curr_text.split()

            if not prev_words or not curr_words:
                cleaned.append(results[i].copy())
                continue

            best_overlap = 0
            check_words = min(max_overlap_words, len(prev_words), len(curr_words))

            for overlap_len in range(check_words, 0, -1):
                prev_suffix = prev_words[-overlap_len:]
                curr_prefix = curr_words[:overlap_len]

                prev_normalized = [re.sub(r'[^\w]', '', w.lower()) for w in prev_suffix]
                curr_normalized = [re.sub(r'[^\w]', '', w.lower()) for w in curr_prefix]

                if prev_normalized == curr_normalized:
                    best_overlap = overlap_len
                    break

            if best_overlap > 0:
                new_text = " ".join(curr_words[best_overlap:])
            else:
                new_text = curr_text

            if new_text.strip():
                cleaned.append({
                    "transcription": new_text,
                    "boundaries": results[i]["boundaries"]
                })

        return cleaned


# ============================================================================
# DIARIZATION WORKER (для параллельной обработки)
# ============================================================================

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


# ============================================================================
# DIARIZATION MODULE
# ============================================================================

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
        """Загружает модель для speaker embeddings."""
        if self.embedding_model is None:
            try:
                from pyannote.audio import Model
                from pyannote.audio import Inference

                print("Загрузка модели speaker embeddings...", file=sys.stderr)
                model = Model.from_pretrained(
                    "pyannote/wespeaker-voxceleb-resnet34-LM",
                    use_auth_token=self.hf_token
                )
                self.embedding_model = Inference(model, window="whole")

                if torch.cuda.is_available():
                    self.embedding_model.to(torch.device("cuda"))

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
        import tempfile
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
        Вычисляет speaker embedding для аудио или его части.

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
            from pyannote.core import Segment as PyannoteSegment

            if start is not None and end is not None:
                excerpt = PyannoteSegment(start, end)
                embedding = embedding_model.crop(audio_path, excerpt)
            else:
                embedding = embedding_model(audio_path)

            return embedding.numpy() if hasattr(embedding, 'numpy') else np.array(embedding)

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
        similarity_threshold: float = 0.80
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


# ============================================================================
# SPEAKER DATABASE
# ============================================================================

class SpeakerDatabase:
    """База данных известных спикеров."""

    def __init__(self, db_path: Optional[str] = None):
        self.db_path = db_path
        self.speakers: Dict[str, SpeakerInfo] = {}

        if db_path and Path(db_path).exists():
            self.load()

    def load(self):
        """Загружает базу из файла."""
        if self.db_path is None:
            return

        try:
            with open(self.db_path, 'r', encoding='utf-8') as f:
                data = json.load(f)

            self.speakers = {}
            for key, value in data.items():
                self.speakers[key] = SpeakerInfo(
                    display_name=value.get("display_name", key),
                    embedding=value.get("embedding", [])
                )

            print(f"Загружено {len(self.speakers)} спикеров из базы.", file=sys.stderr)

        except Exception as e:
            print(f"Ошибка загрузки speaker-db: {e}", file=sys.stderr)

    def save(self):
        """Сохраняет базу в файл."""
        if self.db_path is None:
            return

        data = {}
        for key, info in self.speakers.items():
            data[key] = {
                "display_name": info.display_name,
                "embedding": info.embedding
            }

        with open(self.db_path, 'w', encoding='utf-8') as f:
            json.dump(data, f, ensure_ascii=False, indent=2)

        print(f"База спикеров сохранена в {self.db_path}", file=sys.stderr)

    def add_speaker(self, speaker_id: str, display_name: str, embedding: np.ndarray):
        """Добавляет или обновляет спикера в базе."""
        self.speakers[speaker_id] = SpeakerInfo(
            display_name=display_name,
            embedding=embedding.tolist() if isinstance(embedding, np.ndarray) else embedding
        )

    def get_speaker(self, speaker_id: str) -> Optional[SpeakerInfo]:
        """Получает информацию о спикере."""
        return self.speakers.get(speaker_id)

    def match_speaker(
        self,
        embedding: np.ndarray,
        threshold: float = SPEAKER_SIMILARITY_THRESHOLD
    ) -> Optional[Tuple[str, str, float]]:
        """
        Находит наиболее похожего спикера в базе.

        Returns:
            (speaker_id, display_name, similarity) или None
        """
        if not self.speakers or embedding is None:
            return None

        best_match = None
        best_similarity = -1.0

        for speaker_id, info in self.speakers.items():
            if not info.embedding:
                continue

            db_embedding = np.array(info.embedding)
            similarity = self._cosine_similarity(embedding, db_embedding)

            if similarity > best_similarity:
                best_similarity = similarity
                best_match = (speaker_id, info.display_name, similarity)

        if best_match and best_match[2] >= threshold:
            return best_match

        return None

    @staticmethod
    def _cosine_similarity(a: np.ndarray, b: np.ndarray) -> float:
        """Вычисляет косинусное сходство между векторами."""
        a = a.flatten()
        b = b.flatten()

        norm_a = np.linalg.norm(a)
        norm_b = np.linalg.norm(b)

        if norm_a == 0 or norm_b == 0:
            return 0.0

        return float(np.dot(a, b) / (norm_a * norm_b))


# ============================================================================
# SEGMENT ALIGNMENT
# ============================================================================

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


# ============================================================================
# MARKDOWN OUTPUT
# ============================================================================

def format_time(seconds: float, short: bool = True) -> str:
    """
    Форматирует время.

    Args:
        seconds: время в секундах
        short: True для MM:SS, False для HH:MM:SS.mmm
    """
    hours = int(seconds // 3600)
    minutes = int((seconds % 3600) // 60)
    secs = int(seconds % 60)

    if short:
        if hours > 0:
            return f"{hours}:{minutes:02d}:{secs:02d}"
        else:
            return f"{minutes:02d}:{secs:02d}"
    else:
        return f"{hours:02d}:{minutes:02d}:{secs:06.3f}"


def generate_markdown(
    segments: List[Segment],
    title: str,
    include_timestamps: bool = True,
    merge_same_speaker: bool = True,
    merge_gap_threshold: float = 2.0,
    meeting_date: Optional[str] = None
) -> str:
    """
    Генерирует Markdown-файл из сегментов.

    Формат вывода (стиль example_result.md):
    # title

    **Meeting Date:** ...

    ---

    **Speaker** *[MM:SS]*: text

    Args:
        segments: список сегментов с текстом и спикерами
        title: заголовок документа
        include_timestamps: включать ли таймкоды
        merge_same_speaker: объединять ли соседние реплики одного спикера
        merge_gap_threshold: максимальный промежуток для объединения (секунды)
        meeting_date: дата встречи (опционально)
    """
    lines = [f"# {title}", ""]

    # Добавляем дату встречи, если указана
    if meeting_date:
        lines.append(f"**Meeting Date:** {meeting_date}")
    else:
        # Генерируем текущую дату
        from datetime import datetime
        now = datetime.now()
        date_str = now.strftime("%d %b, %Y - %I:%M %p")
        lines.append(f"**Meeting Date:** {date_str}")

    lines.append("")
    lines.append("---")
    lines.append("")

    if not segments:
        lines.append("*Транскрипт пуст*")
        return "\n".join(lines)

    # Опционально объединяем соседние сегменты одного спикера
    if merge_same_speaker:
        merged = []
        current = None

        for seg in segments:
            if current is None:
                current = Segment(
                    text=seg.text,
                    start=seg.start,
                    end=seg.end,
                    speaker_id=seg.speaker_id,
                    speaker_label=seg.speaker_label
                )
            elif (current.speaker_id == seg.speaker_id and
                  seg.start - current.end <= merge_gap_threshold):
                # Объединяем
                current.text += " " + seg.text
                current.end = seg.end
            else:
                merged.append(current)
                current = Segment(
                    text=seg.text,
                    start=seg.start,
                    end=seg.end,
                    speaker_id=seg.speaker_id,
                    speaker_label=seg.speaker_label
                )

        if current:
            merged.append(current)

        segments = merged

    # Генерируем строки в формате: **Speaker** *[MM:SS]*: text
    for seg in segments:
        if include_timestamps:
            time_str = format_time(seg.start, short=True)
            line = f"**{seg.speaker_label}** *[{time_str}]*: {seg.text}"
        else:
            line = f"**{seg.speaker_label}**: {seg.text}"

        lines.append(line)

    return "\n".join(lines)


# ============================================================================
# MAIN PIPELINE
# ============================================================================

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

        # 2. Диаризация
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
            diar_segments = diarization.diarize(wav_path)
            print(f"Получено {len(diar_segments)} сегментов диаризации.", file=sys.stderr)

            # 2.5. Пост-кластеризация спикеров (объединение фрагментированных ID)
            unique_before = len(set(s.speaker_id for s in diar_segments))
            if unique_before > 2:  # Кластеризация имеет смысл при > 2 спикеров
                diar_segments = diarization.cluster_speakers(wav_path, diar_segments)

            # Получаем embeddings спикеров
            speaker_embeddings = diarization.get_speaker_embeddings(wav_path, diar_segments)

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


# ============================================================================
# CLI
# ============================================================================

def main():
    parser = argparse.ArgumentParser(
        prog="mp4-notes",
        description="CLI-утилита для транскрибирования аудио/видео с диаризацией спикеров"
    )

    subparsers = parser.add_subparsers(dest="command", help="Доступные команды")

    # Команда: transcribe
    transcribe_parser = subparsers.add_parser(
        "transcribe",
        help="Транскрибирование файла с диаризацией"
    )
    transcribe_parser.add_argument(
        "input",
        help="Путь к аудио/видео файлу (mp4, mp3, wav и др.)"
    )
    transcribe_parser.add_argument(
        "-o", "--out",
        help="Путь к выходному .md файлу (по умолчанию: input.md)"
    )
    transcribe_parser.add_argument(
        "-m", "--model",
        default=DEFAULT_MODEL,
        help=f"Модель GigaAM (по умолчанию: {DEFAULT_MODEL})"
    )
    transcribe_parser.add_argument(
        "--mode",
        choices=["longform", "simple"],
        default="simple",
        help="Режим транскрибирования (по умолчанию: simple)"
    )
    transcribe_parser.add_argument(
        "--device",
        help="Устройство (cuda/cpu/auto)"
    )
    transcribe_parser.add_argument(
        "--speaker-db",
        help="Путь к файлу базы спикеров (JSON)"
    )
    transcribe_parser.add_argument(
        "--no-timestamps",
        action="store_true",
        help="Не включать таймкоды в вывод"
    )
    transcribe_parser.add_argument(
        "--no-diarization",
        action="store_true",
        help="Отключить диаризацию (один спикер)"
    )
    transcribe_parser.add_argument(
        "--chunk-duration",
        type=float,
        default=DEFAULT_CHUNK_DURATION,
        help=f"Длительность чанка (сек) для режима simple (по умолчанию: {DEFAULT_CHUNK_DURATION})"
    )
    transcribe_parser.add_argument(
        "--overlap",
        type=float,
        default=DEFAULT_OVERLAP,
        help=f"Перекрытие чанков (сек) для режима simple (по умолчанию: {DEFAULT_OVERLAP})"
    )

    # Команда: enroll-speaker
    enroll_parser = subparsers.add_parser(
        "enroll-speaker",
        help="Добавление известного спикера в базу"
    )
    enroll_parser.add_argument(
        "speaker_id",
        help="Идентификатор спикера (например: slava)"
    )
    enroll_parser.add_argument(
        "audio_files",
        nargs="+",
        help="Аудио/видео файлы с речью спикера"
    )
    enroll_parser.add_argument(
        "--display-name",
        required=True,
        help="Отображаемое имя спикера (например: 'Слава')"
    )
    enroll_parser.add_argument(
        "--speaker-db",
        default="speakers.json",
        help="Путь к файлу базы спикеров (по умолчанию: speakers.json)"
    )

    # Команда: build-speaker-db
    build_db_parser = subparsers.add_parser(
        "build-speaker-db",
        help="Создание базы спикеров из файла (диаризация + эмбеддинги)"
    )
    build_db_parser.add_argument(
        "input",
        help="Путь к аудио/видео файлу"
    )
    build_db_parser.add_argument(
        "--speaker-db",
        default="speakers.json",
        help="Путь к файлу базы спикеров (по умолчанию: speakers.json)"
    )
    build_db_parser.add_argument(
        "--mapping",
        nargs="*",
        help="Маппинг speaker_id:display_name (например: speaker_0:Руслан speaker_1:Вячеслав)"
    )

    args = parser.parse_args()

    if args.command == "transcribe":
        input_path = args.input
        output_path = args.out or str(Path(input_path).with_suffix(".md"))

        transcribe_pipeline(
            input_path=input_path,
            output_path=output_path,
            model_name=args.model,
            mode=args.mode,
            device=args.device,
            speaker_db_path=args.speaker_db,
            include_timestamps=not args.no_timestamps,
            no_diarization=args.no_diarization,
            chunk_duration=args.chunk_duration,
            overlap=args.overlap
        )

    elif args.command == "enroll-speaker":
        enroll_speaker_pipeline(
            speaker_id=args.speaker_id,
            audio_files=args.audio_files,
            display_name=args.display_name,
            speaker_db_path=args.speaker_db
        )

    elif args.command == "build-speaker-db":
        # Парсим маппинг
        speaker_mapping = None
        if args.mapping:
            speaker_mapping = {}
            for m in args.mapping:
                if ":" in m:
                    sid, name = m.split(":", 1)
                    speaker_mapping[sid] = name

        build_speaker_db_pipeline(
            input_path=args.input,
            speaker_db_path=args.speaker_db,
            speaker_mapping=speaker_mapping
        )

    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
