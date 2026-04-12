"""
База данных известных спикеров.
"""

import json
import sys
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from .config import SPEAKER_SIMILARITY_THRESHOLD
from .models import EmbeddingSample, SpeakerInfo


class SpeakerDatabase:
    """База данных известных спикеров."""

    def __init__(self, db_path: Optional[str] = None):
        self.db_path = db_path
        self.speakers: Dict[str, SpeakerInfo] = {}

        if db_path and Path(db_path).exists():
            self.load()

    def load(self):
        """Загружает базу из файла. Поддерживает старый и новый формат."""
        if self.db_path is None:
            return

        try:
            with open(self.db_path, 'r', encoding='utf-8') as f:
                data = json.load(f)

            self.speakers = {}
            total_embeddings = 0

            for key, value in data.items():
                embeddings = []

                # Новый формат: embeddings с метаданными
                if "embeddings" in value:
                    for emb_data in value["embeddings"]:
                        embeddings.append(EmbeddingSample(
                            embedding=emb_data.get("embedding", []),
                            source_file=emb_data.get("source_file", ""),
                            start_time=emb_data.get("start_time", 0.0),
                            end_time=emb_data.get("end_time", 0.0),
                            transcript=emb_data.get("transcript", "")
                        ))
                # Старый формат: один embedding
                elif "embedding" in value:
                    embeddings.append(EmbeddingSample(
                        embedding=value["embedding"],
                        source_file="legacy",
                        transcript="migrated from old format"
                    ))

                self.speakers[key] = SpeakerInfo(
                    display_name=value.get("display_name", key),
                    embeddings=embeddings
                )
                total_embeddings += len(embeddings)

            print(f"Загружено {len(self.speakers)} спикеров ({total_embeddings} embeddings) из базы.",
                  file=sys.stderr)

        except Exception as e:
            print(f"Ошибка загрузки speaker-db: {e}", file=sys.stderr)

    def save(self):
        """Сохраняет базу в файл в новом формате с метаданными."""
        if self.db_path is None:
            return

        data = {}
        for key, info in self.speakers.items():
            embeddings_data = []
            for sample in info.embeddings:
                embeddings_data.append({
                    "embedding": sample.embedding,
                    "source_file": sample.source_file,
                    "start_time": sample.start_time,
                    "end_time": sample.end_time,
                    "transcript": sample.transcript
                })

            data[key] = {
                "display_name": info.display_name,
                "embeddings": embeddings_data
            }

        with open(self.db_path, 'w', encoding='utf-8') as f:
            json.dump(data, f, ensure_ascii=False, indent=2)

        print(f"База спикеров сохранена в {self.db_path}", file=sys.stderr)

    def add_speaker(
        self,
        speaker_id: str,
        display_name: str,
        embedding: np.ndarray,
        source_file: str = "",
        start_time: float = 0.0,
        end_time: float = 0.0,
        transcript: str = ""
    ):
        """Добавляет новый embedding к спикеру (или создаёт нового спикера)."""
        emb_list = embedding.tolist() if isinstance(embedding, np.ndarray) else embedding

        sample = EmbeddingSample(
            embedding=emb_list,
            source_file=source_file,
            start_time=start_time,
            end_time=end_time,
            transcript=transcript
        )

        if speaker_id in self.speakers:
            # Добавляем к существующему спикеру
            self.speakers[speaker_id].embeddings.append(sample)
        else:
            # Создаём нового спикера
            self.speakers[speaker_id] = SpeakerInfo(
                display_name=display_name,
                embeddings=[sample]
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
        Сравнивает со всеми embeddings каждого спикера и берёт максимальный similarity.

        Returns:
            (speaker_id, display_name, max_similarity) или None
        """
        if not self.speakers or embedding is None:
            return None

        best_match = None
        best_similarity = -1.0

        for speaker_id, info in self.speakers.items():
            if not info.embeddings:
                continue

            # Находим максимальный similarity среди всех образцов спикера
            max_sim_for_speaker = -1.0
            for sample in info.embeddings:
                if not sample.embedding:
                    continue
                db_embedding = np.array(sample.embedding)
                similarity = self.cosine_similarity(embedding, db_embedding)
                if similarity > max_sim_for_speaker:
                    max_sim_for_speaker = similarity

            if max_sim_for_speaker > best_similarity:
                best_similarity = max_sim_for_speaker
                best_match = (speaker_id, info.display_name, max_sim_for_speaker)

        if best_match and best_match[2] >= threshold:
            return best_match

        return None

    def delete_speaker(self, speaker_id: str) -> bool:
        """Удаляет спикера полностью."""
        if speaker_id not in self.speakers:
            return False
        del self.speakers[speaker_id]
        return True

    def rename_speaker(self, speaker_id: str, new_display_name: str) -> bool:
        """Переименовывает спикера."""
        if speaker_id not in self.speakers:
            return False
        self.speakers[speaker_id].display_name = new_display_name
        return True

    def delete_embedding(self, speaker_id: str, index: int) -> bool:
        """Удаляет отдельный embedding у спикера."""
        if speaker_id not in self.speakers:
            return False
        speaker = self.speakers[speaker_id]
        if index < 0 or index >= len(speaker.embeddings):
            return False
        speaker.embeddings.pop(index)
        return True

    def get_all_speakers(self) -> List[dict]:
        """Возвращает список всех спикеров с краткой информацией."""
        result = []
        for speaker_id, info in self.speakers.items():
            embeddings_info = []
            for i, emb in enumerate(info.embeddings):
                embeddings_info.append({
                    "index": i,
                    "source_file": emb.source_file,
                    "start_time": emb.start_time,
                    "end_time": emb.end_time,
                    "transcript": emb.transcript[:100] + "..." if len(emb.transcript) > 100 else emb.transcript
                })
            result.append({
                "id": speaker_id,
                "display_name": info.display_name,
                "embedding_count": len(info.embeddings),
                "embeddings": embeddings_info
            })
        return result

    @staticmethod
    def cosine_similarity(a: np.ndarray, b: np.ndarray) -> float:
        """Вычисляет косинусное сходство между векторами."""
        a = a.flatten()
        b = b.flatten()

        norm_a = np.linalg.norm(a)
        norm_b = np.linalg.norm(b)

        if norm_a == 0 or norm_b == 0:
            return 0.0

        return float(np.dot(a, b) / (norm_a * norm_b))
