# ADR-0010: Live-режим транскрипции с BlackHole

## Status
Accepted

## Context
Текущий инструмент работает только с файлами. Пользователи хотят:
- Транскрибировать онлайн-митинги в реальном времени
- Захватывать одновременно микрофон и системный звук
- Видеть результат сразу, не дожидаясь окончания записи

### Техническая проблема на macOS:
- Системный звук нельзя захватить напрямую
- Нужен виртуальный audio device

### Решение: BlackHole
- Open-source виртуальный audio driver
- Создаёт loopback: системный звук → виртуальный input
- Установка: `brew install blackhole-2ch`

## Decision
**Добавить команду `live`** для real-time транскрипции.

### Архитектура:
```
┌─────────────────┐     ┌─────────────────────────┐
│  Микрофон       │     │  BlackHole 2ch          │
│  (default input)│     │  (системный звук)       │
└────────┬────────┘     └────────┬────────────────┘
         │                       │
         └───────────┬───────────┘
                     ▼
         ┌───────────────────────┐
         │  AudioMixer           │
         │  - 2 канала: mic + sys│
         └───────────┬───────────┘
                     ▼
         ┌───────────────────────┐
         │  RingBuffer           │
         │  - chunk_duration сек │
         │  - overlap            │
         └───────────┬───────────┘
                     ▼
    ┌────────────────┴────────────────┐
    ▼                                 ▼
┌─────────────┐              ┌─────────────────┐
│  ASR        │              │  Embeddings     │
│  (GigaAM)   │              │  (pyannote)     │
└──────┬──────┘              └────────┬────────┘
       │                              │
       └──────────────┬───────────────┘
                      ▼
         ┌───────────────────────┐
         │  SpeakerTracker       │
         │  + Display            │
         └───────────────────────┘
```

## Implementation

### CLI:
```bash
python mp4_notes.py live [OPTIONS]

Options:
  --mic-device ID        # Микрофон (default: системный)
  --system-device ID     # BlackHole device ID
  --chunk-duration SEC   # Размер чанка для ASR (default: 5.0)
  --speaker-db FILE      # База спикеров
  --output FILE          # Сохранять транскрипт в файл
  --no-diarization       # Только ASR
  --list-devices         # Показать устройства
```

### Компоненты:

#### 1. LiveAudioCapture
```python
class LiveAudioCapture:
    """Захват аудио с двух устройств."""

    def __init__(self, mic_device, system_device, sample_rate=16000):
        self.mic_stream = sd.InputStream(device=mic_device, ...)
        self.sys_stream = sd.InputStream(device=system_device, ...)

    def get_mixed_chunk(self, duration) -> np.ndarray:
        """Возвращает смикшированный чанк."""
```

#### 2. RingBuffer
```python
class RingBuffer:
    """Кольцевой буфер с overlap."""

    def add(self, chunk):
        ...

    def get_chunk(self) -> np.ndarray:
        """Chunk + overlap от предыдущего."""
```

#### 3. LiveTranscriber
```python
class LiveTranscriber:
    """Координатор live транскрипции."""

    def run(self):
        while self.running:
            chunk = self.buffer.get_chunk()

            # Параллельно
            text = self.asr.transcribe(chunk)
            embedding = self.get_embedding(chunk)

            # Идентификация спикера
            speaker = self.tracker.identify(embedding)

            self.display.show(text, speaker)
```

### Особенности:

#### VAD для определения спикера:
- pyannote VAD для нахождения речевых участков
- Embedding только с речевых участков
- Сравнение с speaker-DB

#### Трекинг спикеров:
```python
class SpeakerTracker:
    """Трекинг спикеров между чанками."""

    def __init__(self, speaker_db):
        self.session_speakers = {}  # Спикеры текущей сессии

    def identify(self, embedding) -> str:
        # 1. Match с speaker-DB
        # 2. Или match с session_speakers
        # 3. Или новый speaker
```

#### Формат вывода:
```
════════════════════════════════════════════════════════════
 LIVE TRANSCRIPTION
 Mic: MacBook Pro Microphone | System: BlackHole 2ch
════════════════════════════════════════════════════════════

[14:32:15] Руслан: Давайте начнём митинг.
[14:32:18] Вячеслав: Да, я готов.
[14:32:22] speaker_2: У меня есть вопрос.

────────────────────────────────────────────────────────────
 Press Ctrl+C to stop
```

## Performance

### Задержка:
| Компонент | Время |
|-----------|-------|
| Audio capture | ~50ms |
| VAD | ~100ms |
| ASR (5 сек chunk) | ~500ms |
| Embedding | ~200ms |
| **Total** | **~2-3 сек** |

### Ресурсы:
- CPU: ~30% (M1/M2)
- RAM: ~2 GB (GigaAM + pyannote embeddings)

## Consequences

### Positive:
- Real-time транскрипция митингов
- Захват микрофона + системного звука
- Идентификация известных спикеров
- Сохранение в файл

### Negative:
- Требуется BlackHole для системного звука
- Задержка 2-3 сек от речи до текста
- Диаризация коротких чанков менее точна

### Dependencies:
- sounddevice (уже есть)
- BlackHole (пользователь устанавливает отдельно)
- Опционально: rich для цветного вывода
