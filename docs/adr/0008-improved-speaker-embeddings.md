# ADR-0008: SpeechBrain ECAPA-TDNN для speaker embeddings

## Status
Accepted

## Context
Для идентификации спикеров используются voice embeddings — векторные представления голоса.

### Рассмотренные варианты:

| Модель | Архитектура | EER (VoxCeleb) | Размерность | Доступ |
|--------|-------------|----------------|-------------|--------|
| wespeaker-resnet34-LM | ResNet34 | ~2.5% | 256 | Открытый |
| pyannote/embedding | ECAPA-TDNN | ~1.2% | 192 | Gated |
| **speechbrain/spkrec-ecapa-voxceleb** | ECAPA-TDNN | ~0.8% | 192 | Открытый |

## Decision
**Использовать `speechbrain/spkrec-ecapa-voxceleb`** (ECAPA-TDNN).

### Причины:
1. **Лучшее качество**: 0.8% EER — state-of-the-art для speaker verification
2. **Открытый доступ**: не требует approval на HuggingFace
3. **Меньше размерность**: 192 vs 256 (компактнее база, быстрее сравнение)
4. **Лучше на коротких сегментах**: TDNN архитектура эффективнее на <3 сек

### Почему не pyannote/embedding:
- Требует отдельного approval на HuggingFace (gated repo)

### Почему не wespeaker-resnet34-LM:
- Хуже качество (2.5% vs 0.8% EER)
- Больше размерность embeddings

## Implementation

```python
# diarize_worker.py
from speechbrain.inference.speaker import EncoderClassifier

classifier = EncoderClassifier.from_hparams(
    source="speechbrain/spkrec-ecapa-voxceleb",
    savedir="/tmp/speechbrain_ecapa"
)
embedding = classifier.encode_batch(audio_tensor)
```

### Threshold:
```python
SPEAKER_SIMILARITY_THRESHOLD = 0.30  # Подбирается эмпирически
```

## Consequences

### Positive:
- Лучшая точность идентификации спикеров
- Стабильнее на коротких репликах
- Открытый доступ без approval
- Меньше размер embeddings (192-dim)

### Negative:
- **Breaking change**: embeddings несовместимы с предыдущей базой (256→192 dim)
- Дополнительная зависимость: speechbrain

### Migration:
1. Установить speechbrain в venv-pyannote
2. Перегенерировать speaker database
