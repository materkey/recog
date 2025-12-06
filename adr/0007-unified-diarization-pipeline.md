# ADR-0007: Унификация пайплайна диаризации

## Status
Accepted

## Context
Текущая архитектура использует два разных пайплайна диаризации:

1. **diarize_worker.py** (pyannote 4.x venv):
   ```python
   Pipeline.from_pretrained("pyannote/speaker-diarization-community-1", token=hf_token)
   ```

2. **mp4_notes.py / DiarizationModule** (pyannote 3.x):
   ```python
   Pipeline.from_pretrained("pyannote/speaker-diarization-3.1", use_auth_token=hf_token)
   ```

### Проблемы:
- Путаница: разные модели дают разные результаты
- Дублирование кода: параллельная диаризация реализована в обоих местах
- Сложность поддержки: два API (pyannote 3.x и 4.x)

## Decision
**Унифицировать на pyannote 4.x** (`speaker-diarization-community-1` через subprocess).

### Причины выбора модели community-1:
1. Оптимизирована для pyannote 4.x
2. Быстрее инференс
3. Лучше работает с шумными записями

### Архитектура после унификации:
```
┌─────────────────────────────────────────────────────────────┐
│  mp4_notes.py (venv-gigaam)                                 │
│  ┌────────────────────────────────────────────────────┐     │
│  │  transcribe_pipeline()                              │     │
│  │    1. ASR (GigaAM)                                  │     │
│  │    2. run_diarization_pyannote4() ──────────────────┼─────┼──┐
│  │    3. Align + Speaker ID                            │     │  │
│  └────────────────────────────────────────────────────┘     │  │
└─────────────────────────────────────────────────────────────┘  │
                                                                  │
┌─────────────────────────────────────────────────────────────┐  │
│  diarize_worker.py (venv-pyannote)                          │  │
│  ┌────────────────────────────────────────────────────┐     │  │
│  │  speaker-diarization-community-1 (pyannote 4.x) ◄──┼─────┼──┘
│  │  + Extract embeddings                               │     │
│  │  (без кластеризации!)                               │     │
│  └────────────────────────────────────────────────────┘     │
└─────────────────────────────────────────────────────────────┘
```

## Implementation

### 1. diarize_worker.py:
```python
# Используем community-1 (pyannote 4.x)
Pipeline.from_pretrained("pyannote/speaker-diarization-community-1", token=hf_token)
```

### 2. Удалить DiarizationModule.diarize():
- Метод `diarize()` больше не нужен
- Вся диаризация через subprocess

### 3. Удалить _diarize_segment_worker():
- Параллельная обработка больше не нужна
- pyannote 4.x достаточно быстр

### 4. Удалить флаг --pyannote3:
- Только один пайплайн
- Меньше путаницы

## Consequences

### Positive:
- Единый пайплайн = предсказуемые результаты
- Меньше кода = проще поддержка
- pyannote 4.x быстрее и точнее

### Negative:
- Зависимость от subprocess (overhead ~2-3 сек на запуск)
- Требуется venv-pyannote

### Migration:
- `--pyannote3` → удалить (breaking change)
- `DiarizationModule.diarize()` → удалить
- `_diarize_segment_worker()` → удалить
