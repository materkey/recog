# ADR-0005: Пост-кластеризация спикеров после параллельной диаризации

## Status
Accepted

## Context
При параллельной диаризации (ADR-0003) аудио разбивается на независимые сегменты. Каждый сегмент обрабатывается отдельным воркером, который присваивает спикерам локальные ID:

```
Сегмент 1: SPEAKER_00, SPEAKER_01
Сегмент 2: SPEAKER_00, SPEAKER_01  ← Разные люди с одинаковыми ID!
Сегмент 3: SPEAKER_00
```

После нормализации получаем:
```
speaker_0, speaker_1, speaker_2, speaker_3, speaker_4, speaker_5...
```

Проблема: один реальный человек может получить 3-4 разных ID, если его речь попала в разные сегменты.

## Decision
Добавить этап **пост-кластеризации** после объединения результатов диаризации.

### Алгоритм:

```
┌─────────────────────────────────────────────────────────────┐
│  1. СБОР УНИКАЛЬНЫХ SPEAKER_ID                              │
│     [speaker_0, speaker_1, speaker_2, ..., speaker_N]       │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  2. ИЗВЛЕЧЕНИЕ EMBEDDINGS ДЛЯ КАЖДОГО SPEAKER_ID            │
│     speaker_0 → [0.12, -0.34, 0.56, ...]                    │
│     speaker_1 → [0.45, -0.12, 0.78, ...]                    │
│     speaker_2 → [0.11, -0.35, 0.55, ...]  ≈ speaker_0!      │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  3. КЛАСТЕРИЗАЦИЯ ПО СХОДСТВУ                               │
│     Cosine similarity > 0.80 → один человек                 │
│                                                             │
│     Cluster A: [speaker_0, speaker_2, speaker_5]            │
│     Cluster B: [speaker_1, speaker_3]                       │
│     Cluster C: [speaker_4]                                  │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  4. ПЕРЕИМЕНОВАНИЕ                                          │
│     speaker_0, speaker_2, speaker_5 → speaker_0             │
│     speaker_1, speaker_3            → speaker_1             │
│     speaker_4                       → speaker_2             │
└─────────────────────────────────────────────────────────────┘
```

### Метод кластеризации:
**Agglomerative Clustering** с порогом расстояния (не фиксированное число кластеров):
```python
from sklearn.cluster import AgglomerativeClustering

clustering = AgglomerativeClustering(
    n_clusters=None,
    distance_threshold=0.20,  # 1 - cosine_similarity(0.80)
    metric='cosine',
    linkage='average'
)
labels = clustering.fit_predict(embeddings)
```

## Implementation

```python
def cluster_speakers(
    audio_path: str,
    diarization_segments: List[DiarizationSegment],
    similarity_threshold: float = 0.80
) -> Dict[str, str]:
    """
    Кластеризует спикеров по голосовым embeddings.

    Returns:
        Маппинг old_speaker_id → new_speaker_id
    """
    # 1. Собираем уникальные speaker_id
    unique_speakers = list(set(seg.speaker_id for seg in diarization_segments))

    if len(unique_speakers) <= 1:
        return {s: s for s in unique_speakers}

    # 2. Извлекаем embeddings
    embeddings = {}
    for speaker_id in unique_speakers:
        # Находим сегменты этого спикера
        speaker_segments = [s for s in diarization_segments if s.speaker_id == speaker_id]
        # Берём самые длинные для надёжности
        best_segments = sorted(speaker_segments, key=lambda s: s.end - s.start, reverse=True)[:3]

        emb = compute_average_embedding(audio_path, best_segments)
        if emb is not None:
            embeddings[speaker_id] = emb

    # 3. Кластеризация
    speaker_ids = list(embeddings.keys())
    emb_matrix = np.array([embeddings[s] for s in speaker_ids])

    clustering = AgglomerativeClustering(
        n_clusters=None,
        distance_threshold=1 - similarity_threshold,
        metric='cosine',
        linkage='average'
    )
    labels = clustering.fit_predict(emb_matrix)

    # 4. Создаём маппинг
    cluster_to_canonical = {}
    for speaker_id, label in zip(speaker_ids, labels):
        if label not in cluster_to_canonical:
            cluster_to_canonical[label] = f"speaker_{len(cluster_to_canonical)}"

    return {speaker_id: cluster_to_canonical[label]
            for speaker_id, label in zip(speaker_ids, labels)}
```

## Performance Impact

| Этап | Время (5 мин аудио) |
|------|---------------------|
| Текущий пайплайн | ~106 сек |
| + Пост-кластеризация | +15-30 сек |
| **Итого** | ~120-135 сек |

Overhead: ~15-25% от общего времени.

## Consequences

### Positive:
- Консистентные speaker ID между сегментами
- Корректное количество уникальных спикеров
- Улучшенное качество при параллельной обработке

### Negative:
- Дополнительное время на извлечение embeddings
- Зависимость от sklearn
- Порог 0.80 может требовать тюнинга для разных аудио

### Риски:
- Голоса могут быть ошибочно объединены (порог слишком низкий)
- Голоса могут остаться разделёнными (порог слишком высокий)
- Короткие сегменты дают менее надёжные embeddings

## Alternatives Considered

1. **Не делать параллельную диаризацию**
   - Отклонено: слишком медленно для длинных записей

2. **Передавать контекст между сегментами**
   - Сложно: pyannote не поддерживает incremental diarization

3. **Использовать overlapping segments и голосование**
   - Отклонено: увеличивает общее время обработки

## Dependencies
- `scikit-learn` для AgglomerativeClustering
- `pyannote/wespeaker-voxceleb-resnet34-LM` для embeddings (уже используется)
