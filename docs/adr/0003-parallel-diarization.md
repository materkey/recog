# ADR-0003: Параллельная диаризация для ускорения обработки

## Status
Accepted

## Context
Диаризация pyannote.audio — самый медленный этап пайплайна:
- 5 мин аудио → ~5 мин обработки (sequential)
- Соотношение 1:1 неприемлемо для длинных записей

Необходимо ускорить диаризацию без потери качества.

## Decision
Разбить аудио на сегменты и обрабатывать их параллельно через **ProcessPoolExecutor**.

### Параметры:
- **Segment duration**: 60 сек (1 минута)
- **Overlap**: 5 сек (для корректной склейки)
- **Workers**: `min(num_segments, cpu_count - 1)`

### Архитектура:
```
Audio (5 min)
    │
    ├─► Segment 0 [0:00 - 1:00]  ──► Worker 1 ──┐
    ├─► Segment 1 [0:55 - 1:55]  ──► Worker 2 ──┤
    ├─► Segment 2 [1:50 - 2:50]  ──► Worker 3 ──┼──► Merge Results
    ├─► Segment 3 [2:45 - 3:45]  ──► Worker 4 ──┤
    ├─► Segment 4 [3:40 - 4:40]  ──► Worker 5 ──┤
    └─► Segment 5 [4:35 - 5:00]  ──► Worker 6 ──┘
```

### Почему ProcessPoolExecutor, а не ThreadPoolExecutor:
- PyTorch/pyannote используют GIL-блокирующие операции
- Процессы дают истинную параллельность
- Каждый воркер загружает свою копию pipeline

## Implementation
```python
def _diarize_segment_worker(args):
    """Модульная функция для pickle в ProcessPoolExecutor."""
    seg_idx, audio_segment, sample_rate, start_time, hf_token = args

    # Каждый воркер загружает свой pipeline
    pipeline = Pipeline.from_pretrained(...)

    # Обработка сегмента
    results = pipeline(segment_audio)

    # Сдвиг временных меток
    return [(r.start + start_time, r.end + start_time, speaker) for r in results]

# Параллельный запуск
with ProcessPoolExecutor(max_workers=6) as executor:
    futures = {executor.submit(_diarize_segment_worker, args): i for i, args in enumerate(segments)}
    for future in as_completed(futures):
        results.extend(future.result())
```

## Benchmarks (5 min audio, 6 workers)

| Метрика | Sequential | Parallel | Улучшение |
|---------|------------|----------|-----------|
| Диаризация | 298 сек | 99 сек | **3x** |
| Общее время | 5:08 | 1:46 | **2.9x** |
| CPU usage | ~100% | ~600% | Полная утилизация |

## Consequences

### Positive:
- 3x ускорение диаризации
- Линейное масштабирование с количеством ядер
- Прогресс-бар показывает реальный прогресс

### Negative:
- **Высокое потребление RAM**: ~2 GB × N воркеров
- **Фрагментация спикеров**: спикер в разных сегментах получает разные ID
- **Overhead**: загрузка pipeline в каждом воркере (~10-15 сек)

### Trade-offs:
| Сегмент | Воркеров | RAM | Скорость | Качество ID |
|---------|----------|-----|----------|-------------|
| 120 сек | 3 | ~8 GB | Средняя | Лучше |
| 60 сек | 6 | ~15 GB | Высокая | Хуже |
| 30 сек | 12 | ~25 GB | Максимум | Плохое |

## Related
- ADR-0004: Post-clustering спикеров (решает проблему фрагментации)
