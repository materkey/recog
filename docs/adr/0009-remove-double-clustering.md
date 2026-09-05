# ADR-0009: Удаление двойной кластеризации

## Status
Accepted

## Context
Текущая архитектура выполняет **двойную кластеризацию**:

### Первая кластеризация (внутри pyannote):
```
pyannote pipeline → VAD → Segmentation → Clustering → speaker_0, speaker_1, ...
```

### Вторая кластеризация (наш код):
```python
# diarize_worker.py
def cluster_speakers(segments, embeddings, similarity_threshold):
    clustering = AgglomerativeClustering(
        n_clusters=None,
        distance_threshold=1 - similarity_threshold,
        metric='cosine',
        linkage='average'
    )
    labels = clustering.fit_predict(emb_matrix)
```

### Проблемы:
1. **Избыточность**: pyannote уже кластеризует спикеров
2. **Риск ошибок**: можем склеить разных людей
3. **Нестабильность**: threshold 0.50 слишком агрессивный
4. **Путаница**: speaker_0 от pyannote → speaker_0 от нас (разные люди!)

### Когда вторая кластеризация была нужна:
- ADR-0005: при параллельной диаризации сегменты обрабатывались независимо
- Каждый сегмент получал свои speaker_0, speaker_1
- Нужно было объединить их в глобальные ID

### Почему больше не нужна:
- ADR-0007: унификация на pyannote 4.x
- Диаризация всего файла за один проход
- pyannote сам выдаёт консистентные ID

## Decision
**Удалить вторую кластеризацию**.

Embeddings использовать **только для speaker-DB matching**:
```
pyannote → speaker_0, speaker_1, speaker_2
                ↓
extract embeddings для каждого
                ↓
match с speaker-DB → Руслан, Вячеслав, speaker_2
```

## Implementation

### 1. Удалить cluster_speakers() из diarize_worker.py:
```python
# было
if len(embeddings) > 2:
    normalized_segments, embeddings = cluster_speakers(
        normalized_segments,
        embeddings,
        similarity_threshold
    )

# стало
# (удалить вызов cluster_speakers)
```

### 2. Удалить cluster_speakers() из DiarizationModule:
```python
# было
if unique_before > 2:
    diar_segments = diarization.cluster_speakers(wav_path, diar_segments)

# стало
# (удалить вызов cluster_speakers)
```

### 3. Удалить similarity_threshold из протокола subprocess:
```python
# было
input_data = json.dumps({
    "audio_path": audio_path,
    "hf_token": hf_token,
    "similarity_threshold": similarity_threshold  # удалить
})
```

### 4. Оставить только один threshold:
```python
# Порог для match_speaker() в speaker-DB
SPEAKER_SIMILARITY_THRESHOLD = 0.50  # ADR-0008: новый порог для ECAPA
```

## Consequences

### Positive:
- Стабильные результаты: только pyannote решает кто есть кто
- Меньше ошибок: не склеиваем разных людей
- Проще код: удаляем 100+ строк
- Быстрее: не нужна дополнительная кластеризация

### Negative:
- Если pyannote over-split (один человек = несколько ID):
  - Решение: speaker-DB сматчит все ID на одно имя
- Если pyannote under-merge (два человека = один ID):
  - Проблема остаётся (но это редкость)

### Риски:
- Качество полностью зависит от pyannote
- Но: pyannote 3.1 достаточно хорош для 2-5 спикеров
- При >5 спикерах могут быть проблемы (rare case)

## Migration:
1. Удалить cluster_speakers() из обоих файлов
2. Удалить similarity_threshold из subprocess протокола
3. Тесты на example_5min.mp4
