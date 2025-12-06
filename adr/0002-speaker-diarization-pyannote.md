# ADR-0002: Использование pyannote.audio для диаризации спикеров

## Status
Accepted

## Context
После распознавания речи (ASR) необходимо определить, кто из участников говорит в каждый момент времени (speaker diarization). Требования:
- Автоматическое определение количества спикеров
- Временные метки для каждого спикера
- Возможность идентификации известных спикеров

## Decision
Использовать **pyannote.audio** с моделью `speaker-diarization-3.1`.

### Причины выбора:
1. **Качество**: State-of-the-art для speaker diarization
2. **Гибкость**: Автоматическое определение числа спикеров
3. **Embeddings**: Встроенная модель `wespeaker-voxceleb-resnet34-LM` для голосовых отпечатков
4. **Интеграция**: Совместим с PyTorch экосистемой

### Альтернативы:
- **resemblyzer** — проще, но хуже качество
- **SpeechBrain** — мощнее, но сложнее интеграция
- **NVIDIA NeMo** — требует GPU

## Implementation
```python
from pyannote.audio import Pipeline

pipeline = Pipeline.from_pretrained(
    "pyannote/speaker-diarization-3.1",
    use_auth_token=HF_TOKEN  # Требует токен HuggingFace
)

diarization = pipeline(audio_path)
for turn, _, speaker in diarization.itertracks(yield_label=True):
    print(f"{turn.start:.1f} - {turn.end:.1f}: {speaker}")
```

## Consequences

### Positive:
- Высокая точность определения спикеров
- Не требует указывать количество спикеров заранее
- Поддержка извлечения embeddings для идентификации

### Negative:
- Требует HuggingFace токен для загрузки моделей
- Медленная обработка: ~60 сек на 1 мин аудио (CPU)
- Высокое потребление RAM: ~2 GB на pipeline
