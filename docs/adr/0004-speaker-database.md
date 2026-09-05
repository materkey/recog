# ADR-0004: База данных спикеров для идентификации

## Status
Accepted

## Context
После диаризации спикеры имеют анонимные ID (speaker_0, speaker_1). Для удобства чтения транскрипта нужно заменять их на реальные имена известных участников.

## Decision
Использовать **JSON-файл с голосовыми embeddings** для идентификации известных спикеров.

### Формат базы:
```json
{
  "speaker_id": {
    "display_name": "Руслан",
    "embedding": [0.12, -0.34, 0.56, ...]  // 256 float
  }
}
```

### Алгоритм идентификации:
1. Извлечь embedding каждого speaker_id из аудио
2. Сравнить с embeddings в базе (косинусное сходство)
3. Если similarity > 0.75 — заменить на display_name

### Модель embeddings:
`pyannote/wespeaker-voxceleb-resnet34-LM` — 256-мерные векторы

## Implementation

### Добавление спикера в базу:
```bash
python -m recog enroll-speaker ruslan sample.wav --display-name "Руслан"
```

### Использование при транскрипции:
```bash
python -m recog transcribe meeting.mp4 --speaker-db speakers.json
```

### Код идентификации:
```python
def match_speaker(embedding, threshold=0.75):
    best_match = None
    best_similarity = -1

    for speaker_id, info in database.items():
        similarity = cosine_similarity(embedding, info.embedding)
        if similarity > best_similarity:
            best_similarity = similarity
            best_match = (speaker_id, info.display_name)

    if best_match and best_similarity >= threshold:
        return best_match
    return None
```

## Consequences

### Positive:
- Автоматическая идентификация известных спикеров
- Простой формат (JSON) — легко редактировать вручную
- Не требует переобучения модели

### Negative:
- Требует предварительное добавление спикеров в базу
- Качество зависит от качества образцов голоса
- Порог 0.75 может давать false positives/negatives

### Рекомендации:
- Использовать 2-3 образца голоса для каждого спикера
- Образцы должны быть чистыми (без фона, других голосов)
- Длительность образца: 10-30 секунд
