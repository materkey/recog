# ADR-0014: Поддержка ASR NVIDIA Parakeet-TDT-0.6B-v3 (NeMo)

## Status
Proposed

## Context

Сейчас в проекте основной движок ASR — **GigaAM** (см. ADR-0001). Он хорошо подходит для русского, но есть кейсы, где полезно иметь альтернативный движок:
- **Мультиязычность** (ru/uk/en + другие европейские языки) с автоопределением языка
- **Пунктуация и капитализация** “из коробки”
- **Word/segment timestamps** для более точного матчингa с диаризацией и для UI
- **Высокая пропускная способность на GPU** (batch inference)

Кандидат: **NVIDIA Parakeet-TDT-0.6B-v3** (`nvidia/parakeet-tdt-0.6b-v3`), распространяется под **CC BY 4.0**, заявляет поддержку 25 европейских языков (включая `ru` и `uk`) и предоставляет таймкоды.

Ограничения и риски интеграции:
- Рекомендованный стек выполнения — **NVIDIA NeMo**, модель оптимизирована под **NVIDIA GPU**, предпочтительная ОС — **Linux** (по model card).
- Вероятны конфликты по версиям `torch`/CUDA и тяжёлые зависимости → важно не ломать текущий workflow (см. ADR-0006).
- В текущем коде `recog/asr.py` и `recog/live.py` завязаны на прямой доступ к объекту GigaAM-модели (для live “direct tensor” path).

## Decision

1) Добавить **опциональный ASR backend** `parakeet-v3` на базе NeMo + `nvidia/parakeet-tdt-0.6b-v3`.

2) **Оставить GigaAM дефолтом** для macOS/local CPU/MPS сценариев.

3) Интегрировать Parakeet через **отдельное окружение + subprocess worker** по паттерну ADR-0006 (изоляция зависимостей и упрощение установки).

4) Начальный скоуп: **команда `mp4_notes.py transcribe`** (batch/offline). Live/streaming — отдельная фаза (см. “TODO”).

## Notes: как использовать Parakeet v3 (из model card)

### Установка
```bash
pip install -U nemo_toolkit['asr']
```

### Инференс (Python)
```python
import nemo.collections.asr as nemo_asr

asr_model = nemo_asr.models.ASRModel.from_pretrained(
    model_name="nvidia/parakeet-tdt-0.6b-v3"
)

output = asr_model.transcribe(["audio.wav"], timestamps=True)
text = output[0].text
segment_timestamps = output[0].timestamp["segment"]
```

### Long-form
Model card рекомендует переключать self-attention на local attention:
```python
asr_model.change_attention_model(
    self_attention_model="rel_pos_local_attn",
    att_context_size=[256, 256],
)
```

## Proposed Architecture

### 1) ASR backend абстракция

Цель: выбрать движок на уровне пайплайна без переписывания остального кода.

```
mp4_notes.py
   │
   ▼
recog/pipeline.py ── chooses ASR engine ──► recog/asr_backends/*
   │                                         │
   │                                         ├─ gigaam (in-process)
   │                                         └─ parakeet-v3 (subprocess)
   ▼
diarization (subprocess, ADR-0006)
```

### 2) Subprocess протокол (Parakeet worker)

Ввод (stdin → `parakeet_worker.py`):
```json
{
  "audio_path": "/path/to/audio.wav",
  "model_name": "nvidia/parakeet-tdt-0.6b-v3",
  "timestamps": "segment",
  "longform": true
}
```

Вывод (stdout ← `parakeet_worker.py`):
```json
{
  "segments": [
    {"start": 0.12, "end": 2.34, "text": "Привет."}
  ],
  "meta": {
    "engine": "parakeet-v3",
    "model": "nvidia/parakeet-tdt-0.6b-v3"
  }
}
```

Маппинг в `recog.models.Segment`:
- `text` → `Segment.text`
- `start`/`end` → `Segment.start`/`Segment.end`

## План реализации

| Шаг | Файл | Описание |
|-----|------|----------|
| 1 | `recog/cli.py` | Добавить `--asr-engine` (`gigaam`/`parakeet-v3`) и описать ограничения (parakeet: Linux+NVIDIA) |
| 2 | `recog/asr.py` | Вынести текущую реализацию в backend `gigaam` (без изменения поведения), подготовить общий интерфейс вызова |
| 3 | `recog/pipeline.py` | Пробросить выбор движка из CLI и переключать backend при транскрипции |
| 4 | `parakeet_worker.py` | Добавить воркер NeMo: загрузка модели, `transcribe(..., timestamps=True)`, выдача JSON сегментов |
| 5 | `recog/config.py` | Добавить пути/настройки для Parakeet worker/venv (по аналогии с `PYANNOTE4_VENV`/`DIARIZE_WORKER`) |
| 6 | `setup_venvs.sh` или отдельный скрипт | Документировать отдельную установку Parakeet v3 (рекомендуемо Linux+CUDA; не ломать macOS bootstrap) |
| 7 | `README.md` | Добавить краткую секцию “ASR engines”: как включить `parakeet-v3`, какие требования и как проверить |
| 8 | Smoke check | `python -m compileall recog` + короткий `transcribe` на wav, сравнить таймкоды и разметку по спикерам |

## TODO / Open Questions

- Live/streaming: либо интегрировать NeMo streaming (RNN-T) в `recog/live.py`, либо оставить live только на GigaAM.
- Выбор timestamp granularity: `segment` vs `word` (word-level может дать более точный speaker attribution, но увеличит число сегментов).
- Совместимость `nemo_toolkit['asr']` с Python 3.12 и стратегия установки (pin Python/torch, отдельный контейнер, отдельный хост).
- Нужно ли требовать `HF_TOKEN` для скачивания модели в окружении Parakeet (зависит от условий доступа на Hugging Face).

## Consequences

### Positive
- Дополнительный качественный ASR движок с мультиязычностью и таймкодами
- Возможность ускорения на GPU и пакетной обработки
- Изоляция зависимостей через subprocess (минимум риска поломать текущие venv)

### Negative
- Сложнее установка (NeMo, CUDA, предпочтительно Linux)
- Дополнительный overhead на запуск отдельного процесса
- Live-режим потребует отдельной работы (streaming API и интеграция)
- Лицензия CC BY 4.0 → может требоваться атрибуция при распространении/публикации результатов/артефактов

## Alternatives Considered

1) **faster-whisper** / Whisper
   - Проще кроссплатформенно, но качество на русском может быть ниже, и нужна отдельная работа с punctuation/timestamps.

2) **NVIDIA Riva / NIM**
   - Готовый серверный API, но добавляет сервис/деплой и может быть “не локально” для текущего standalone режима.

3) **Сохранить только GigaAM**
   - Самый простой вариант, но ограничивает мультиязычность и возможности GPU inference.

## References
- Hugging Face model card: https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3
- NeMo ASR docs: https://docs.nvidia.com/nemo-framework/user-guide/latest/nemotoolkit/asr/models.html
- Streaming inference example (NeMo): https://github.com/NVIDIA/NeMo/blob/main/examples/asr/asr_chunked_inference/rnnt/speech_to_text_streaming_infer_rnnt.py
- Technical report: https://arxiv.org/abs/2509.14128

