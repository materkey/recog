# ADR-0008: Модель speaker embeddings

## Status
Accepted

## Context
Для идентификации спикеров используются voice embeddings — векторные представления голоса.

### Рассмотренные варианты:

| Модель | Архитектура | EER (VoxCeleb) | Размерность | Доступ |
|--------|-------------|----------------|-------------|--------|
| **wespeaker-resnet34-LM** | ResNet34 | ~2.5% | 256 | Открытый |
| pyannote/embedding | ECAPA-TDNN | ~1.2% | 192 | Gated (требует approval) |
| speechbrain/ecapa-voxceleb | ECAPA-TDNN | ~0.8% | 192 | Открытый |

## Decision
**Использовать `pyannote/wespeaker-voxceleb-resnet34-LM`**.

### Причины:
1. **Открытый доступ**: не требует дополнительного approval на HuggingFace
2. **Достаточное качество**: 0.97 similarity для Руслана, 1.00 для Вячеслава
3. **Совместимость**: работает с существующей speaker database
4. **Та же экосистема**: интегрирован в pyannote.audio

### Почему не pyannote/embedding:
- Требует отдельного approval на HuggingFace (gated repo)
- Усложняет onboarding новых пользователей

### Почему не SpeechBrain:
- Дополнительная зависимость
- Сложнее интеграция

## Implementation

```python
# diarize_worker.py и mp4_notes.py
model = Model.from_pretrained(
    "pyannote/wespeaker-voxceleb-resnet34-LM",
    token=hf_token
)
```

### Threshold:
```python
SPEAKER_SIMILARITY_THRESHOLD = 0.30  # Низкий для коротких реплик
```

## Consequences

### Positive:
- Простой onboarding (только один HF approval для diarization model)
- Совместимость с существующей базой спикеров
- Достаточная точность для 2-5 спикеров

### Negative:
- Менее точен на очень коротких репликах (<2 сек)
- 256-dim embeddings (больше чем ECAPA 192-dim)

### Future:
При необходимости можно мигрировать на `pyannote/embedding` после получения доступа.
