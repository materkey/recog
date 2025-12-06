# План: Live-режим транскрипции с диаризацией

## Архитектура

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
         │  - или mono mix       │
         └───────────┬───────────┘
                     ▼
         ┌───────────────────────┐
         │  RingBuffer           │
         │  - накопление чанков  │
         │  - overlap для ASR    │
         └───────────┬───────────┘
                     ▼
    ┌────────────────┴────────────────┐
    ▼                                 ▼
┌─────────────┐              ┌─────────────────┐
│  ASR        │              │  Диаризация     │
│  (GigaAM)   │              │  (pyannote emb) │
└──────┬──────┘              └────────┬────────┘
       │                              │
       └──────────────┬───────────────┘
                      ▼
         ┌───────────────────────┐
         │  SpeakerTracker       │
         │  + display            │
         └───────────────────────┘
```

## Настройка BlackHole

1. Установка: `brew install blackhole-2ch`
2. В Audio MIDI Setup создать Multi-Output Device:
   - BlackHole 2ch + Built-in Output (наушники/колонки)
3. Выбрать Multi-Output как системный output
4. В скрипте захватывать BlackHole 2ch как input

## CLI интерфейс

```bash
python mp4_notes.py live [OPTIONS]

Options:
  --mic-device ID        # Микрофон (default: системный default input)
  --system-device ID     # BlackHole device ID
  --chunk-duration SEC   # Размер чанка для ASR (default: 5.0)
  --speaker-db FILE      # База спикеров для идентификации
  --output FILE          # Сохранять транскрипт в файл
  --no-diarization       # Только ASR без диаризации
  --list-devices         # Показать доступные устройства
  --mix-mode MODE        # stereo (mic L, sys R) | mono (mix)
```

## Компоненты

### 1. LiveAudioCapture
```python
class LiveAudioCapture:
    """Захват аудио с двух устройств одновременно."""

    def __init__(self, mic_device, system_device, sample_rate=16000):
        self.mic_stream = sd.InputStream(device=mic_device, ...)
        self.sys_stream = sd.InputStream(device=system_device, ...)

    def get_mixed_chunk(self, duration) -> np.ndarray:
        """Возвращает смикшированный чанк."""
```

### 2. RingBuffer
```python
class RingBuffer:
    """Кольцевой буфер с поддержкой overlap."""

    def __init__(self, max_duration, overlap_ratio=0.5):
        ...

    def add(self, audio_chunk):
        ...

    def get_chunk_for_processing(self) -> np.ndarray:
        """Возвращает чанк с overlap от предыдущего."""
```

### 3. LiveDiarizer
```python
class LiveDiarizer:
    """Диаризация в реальном времени."""

    def __init__(self, speaker_db, embedding_model):
        self.speaker_db = speaker_db
        self.active_speakers = {}  # Трекинг спикеров в сессии

    def process_chunk(self, audio_chunk) -> List[SpeakerSegment]:
        """Диаризирует чанк и возвращает сегменты."""
        # 1. Получаем embeddings через pyannote
        # 2. Сравниваем с базой известных спикеров
        # 3. Сравниваем с уже встреченными в сессии
        # 4. Возвращаем сегменты с speaker_id
```

### 4. LiveTranscriber (главный класс)
```python
class LiveTranscriber:
    """Координатор live транскрипции."""

    def __init__(self, config):
        self.capture = LiveAudioCapture(...)
        self.buffer = RingBuffer(...)
        self.asr = ASRModule(...)
        self.diarizer = LiveDiarizer(...)
        self.display = LiveDisplay(...)

    def run(self):
        """Основной цикл."""
        while self.running:
            chunk = self.buffer.get_chunk_for_processing()

            # Параллельно запускаем ASR и диаризацию
            with ThreadPoolExecutor() as executor:
                asr_future = executor.submit(self.asr.transcribe, chunk)
                diar_future = executor.submit(self.diarizer.process, chunk)

            text = asr_future.result()
            speakers = diar_future.result()

            # Объединяем и выводим
            self.display.show(text, speakers)
```

### 5. LiveDisplay
```python
class LiveDisplay:
    """Отображение результатов в терминале."""

    def show(self, text, speakers):
        # Формат: [HH:MM:SS] Speaker: text
        # Цвета для разных спикеров (rich library)
```

## Этапы реализации

### Этап 1: Базовый захват и ASR
- [ ] Класс LiveAudioCapture с двумя потоками
- [ ] RingBuffer с overlap
- [ ] Интеграция с существующим ASRModule
- [ ] Базовый вывод в консоль

### Этап 2: Диаризация
- [ ] LiveDiarizer с использованием pyannote embeddings
- [ ] SpeakerTracker для отслеживания между чанками
- [ ] Интеграция с SpeakerDatabase

### Этап 3: UI и polish
- [ ] Цветной вывод с rich
- [ ] Сохранение в файл
- [ ] Graceful shutdown (Ctrl+C)
- [ ] Индикатор уровня звука

## Зависимости

```
sounddevice          # уже есть
numpy                # уже есть
rich                 # для цветного вывода (опционально)
```

## Особенности

### Диаризация коротких чанков
- pyannote работает хуже на коротких сегментах
- Решение: использовать только embeddings, без полной диаризации
- Делить чанк на "активные" участки по VAD

### Трекинг спикеров между чанками
```
Чанк 1: speaker_A говорит
Чанк 2: speaker_A продолжает → тот же человек (по embedding similarity)
Чанк 3: новый голос → speaker_B или известный из базы
```

### Задержка
- ASR: ~0.5 сек на чанк
- Диаризация: ~1 сек на чанк
- Итого: 2-3 сек от речи до отображения

## Пример использования

```bash
# Показать устройства
python mp4_notes.py live --list-devices

# Запуск с BlackHole
python mp4_notes.py live --system-device "BlackHole 2ch" --speaker-db speakers_multi.json

# Только микрофон, без системного звука
python mp4_notes.py live --speaker-db speakers_multi.json

# Сохранение в файл
python mp4_notes.py live --output meeting_2024-01-15.md
```

## Формат вывода

```
════════════════════════════════════════════════════════════
 LIVE TRANSCRIPTION
 Mic: MacBook Pro Microphone | System: BlackHole 2ch
════════════════════════════════════════════════════════════

[14:32:15] Руслан: Давайте начнём митинг.
[14:32:18] Вячеслав: Да, я готов. Что у нас сегодня?
[14:32:22] speaker_2: У меня есть вопрос по релизу.
[14:32:25] Руслан: Слушаю.

────────────────────────────────────────────────────────────
 Press Ctrl+C to stop
```
