# ADR-0006: Двойной venv с subprocess для совместимости версий

## Status
Accepted

## Context
Существует конфликт версий PyTorch между компонентами:
- **GigaAM** (ASR): требует `torch<=2.5.1`
- **pyannote.audio 4.0** (диаризация): требует `torch>=2.8.0`

Невозможно установить обе библиотеки в один Python-окружение.

При этом pyannote.audio 4.0 имеет значительные улучшения:
- Новая модель `community-1` с лучшей точностью
- На 40% быстрее обучение и инференс
- Лучшая работа с шумными записями
- Улучшенный подсчёт спикеров

## Decision
Использовать **два отдельных virtual environment** и **subprocess** для коммуникации.

### Архитектура:
```
┌─────────────────────────────────────────────────────────────┐
│  venv-gigaam (torch 2.5)                                    │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  mp4_notes.py                                       │    │
│  │    1. Конвертация аудио (ffmpeg)                    │    │
│  │    2. ASR транскрипция (GigaAM)                     │    │
│  │    3. Вызов subprocess для диаризации ──────────────┼────┼──┐
│  │    4. Объединение результатов                       │    │  │
│  │    5. Генерация Markdown                            │    │  │
│  └─────────────────────────────────────────────────────┘    │  │
└─────────────────────────────────────────────────────────────┘  │
                                                                 │
┌─────────────────────────────────────────────────────────────┐  │
│  venv-pyannote (torch 2.8+)                                 │  │
│  ┌─────────────────────────────────────────────────────┐    │  │
│  │  diarize_worker.py  ◄───────────────────────────────┼────┼──┘
│  │    1. Загрузка pyannote 4.0 pipeline                │    │
│  │    2. Диаризация (community-1 модель)               │    │
│  │    3. Извлечение speaker embeddings                 │    │
│  │    4. Кластеризация спикеров                        │    │
│  │    5. Вывод JSON результата                         │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

### Протокол обмена данными:
```json
// stdin → diarize_worker.py
{
  "audio_path": "/path/to/audio.wav",
  "hf_token": "hf_xxx",
  "similarity_threshold": 0.60
}

// stdout ← diarize_worker.py
{
  "segments": [
    {"start": 0.0, "end": 5.2, "speaker_id": "speaker_0"},
    {"start": 5.2, "end": 12.1, "speaker_id": "speaker_1"}
  ],
  "embeddings": {
    "speaker_0": [0.12, -0.34, ...],
    "speaker_1": [0.45, -0.12, ...]
  }
}
```

## Implementation

### 1. Структура файлов:
```
recog/
├── venv-gigaam/          # ASR окружение
├── venv-pyannote/        # Диаризация окружение
├── mp4_notes.py          # Основной скрипт
├── diarize_worker.py     # Воркер диаризации
└── requirements/
    ├── gigaam.txt        # torch<=2.5, gigaam, etc.
    └── pyannote.txt      # torch>=2.8, pyannote.audio>=4.0
```

### 2. Вызов subprocess:
```python
def run_diarization_subprocess(audio_path: str, hf_token: str) -> dict:
    import subprocess
    import json

    worker_path = Path(__file__).parent / "diarize_worker.py"
    python_path = Path(__file__).parent / "venv-pyannote" / "bin" / "python"

    input_data = json.dumps({
        "audio_path": audio_path,
        "hf_token": hf_token,
        "similarity_threshold": 0.60
    })

    result = subprocess.run(
        [str(python_path), str(worker_path)],
        input=input_data,
        capture_output=True,
        text=True
    )

    return json.loads(result.stdout)
```

## Performance Impact

| Метрика | Единый процесс | Subprocess |
|---------|----------------|------------|
| Время старта | Быстрее (~2 сек) | +3-5 сек (загрузка 2-го venv) |
| Качество диаризации | pyannote 3.1 | pyannote 4.0 (лучше) |
| Использование RAM | ~15 GB | ~8 GB (не одновременно) |
| Параллелизация | Ограничена | Полная изоляция |

## Consequences

### Positive:
- Доступ к pyannote.audio 4.0 и модели community-1
- Улучшенное качество диаризации
- Независимое обновление компонентов
- Изоляция зависимостей
- Меньше RAM (процессы не пересекаются)

### Negative:
- Два venv требуют больше места на диске (~5 GB)
- Дополнительный overhead на запуск subprocess
- Сложнее отладка (два процесса)
- Необходимость синхронизации версий протокола

### Миграция:
- Существующий код продолжает работать
- Новая архитектура включается флагом `--pyannote4`
- Постепенный переход без breaking changes

## Alternatives Considered

1. **Ждать обновления GigaAM**
   - Неизвестно когда будет совместимость с torch 2.8
   - Отклонено: блокирует улучшения

2. **Заменить GigaAM на Whisper**
   - Whisper совместим с torch 2.8
   - Отклонено: GigaAM лучше для русского языка

3. **Патчить зависимости GigaAM**
   - Рискованно, может сломать модель
   - Отклонено: непредсказуемые последствия

4. **Docker контейнеры**
   - Избыточно для данной задачи
   - Отклонено: усложняет развёртывание
