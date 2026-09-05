# Per-Speaker ASR Pipeline — Design

**Date**: 2026-04-22
**Status**: Proposal — awaiting review

## Summary

Заменить текущий mixed-audio ASR пайплайн для KTalk-встреч на per-speaker: каждый удалённый участник отдаёт свой аудиопоток из браузера напрямую, GigaAM транскрибирует чанки с известной атрибуцией по API, диаризация по dominant-speaker убирается из hot-path. Главная цель — устранить неправильную атрибуцию спикера при одновременной речи двух участников.

## Problem

В текущей архитектуре (mixed audio + post-hoc attribution) три устойчивые боли:

1. **Overlap → chaos.** Когда два участника говорят одновременно, GigaAM получает их смешанный голос в одном PCM-потоке и выдаёт кашу из слов. Dominant-speaker атрибуция приписывает всю кашу одному, что усиливает ошибку.

2. **Границы коротких фраз.** При 3-секундных чанках короткая реплика (1-2 с), пришедшаяся на границу между двумя dominant-speaker'ами, систематически приписывается не тому.

3. **Dominant-speaker latency.** Событие `conference.dominantSpeaker` приходит с задержкой 200-500 мс от начала речи. Первые слова нового говорящего атрибутируются предыдущему.

Все три корневые причины — одна: **аудио и идентичность разорваны во времени**. Аудио приходит смикшированное, идентичность приходит как отдельный поток событий, и мы сопоставляем их по таймштампам.

## Validated Assumptions

Перед написанием этого документа прогнаны пробы и бенчмарки на `app.ktalk.ru`; детали в `tests/e2e/research/README.md` и `scripts/benchmarks/bench_gigaam_rtf.py`. Резюме:

| Вопрос | Ответ | Источник |
|---|---|---|
| Jitsi SFU отдаёт per-participant MediaStreamTrack'и? | Да, детерминированно. RMS Bob=0 когда Alice=0.17 на том же приёмнике. | `tests/e2e/research/` probe 1 |
| KTalk уходит в P2P на 1-на-1? | Нет. `isP2PActive()` → false даже на 2 участниках. | probe 2 |
| Смешение потоков при parallel playback? | Нет. Flatness Alice=0.013 (речь), Bob=0.375 (шум). 29× разница. | probe 3 |
| Формат треков | 48 kHz mono 16-bit, `latency:0.01`, `readyState:live` | probe 4 |
| GigaAM RTF одного потока на M-series CPU | 0.032 на 11 с, 0.105 longform на 60 с | bench |
| GigaAM chunk latency (1 forward) | 184 мс для 1-с чанка, 205 мс для 2-с, 233 мс для 4-с | bench |
| Threaded concurrency (shared model) | 4 потока = 51× realtime throughput | bench |
| Batched forward | B=8 → 60× realtime throughput, per-sample 188 мс | bench |
| RSS одной модели | 1634 MB (CPU, fp32) | bench |

**Главный вывод**: архитектурных блокеров нет. Удалённый ASR worker не нужен (запас ~30× на один CPU-инстанс). Микс отдельных `MediaStreamTrack` в per-speaker — правильное направление, а не гипотеза.

## Goals

1. Атрибуция удалённых спикеров становится **ошибоёмкой только когда в сам KTalk приходит неверное имя** (социальная ошибка), не техническая.
2. Overlap двух спикеров даёт два корректно разделённых transcript-сегмента, а не склеенный.
3. UI-latency не ухудшается относительно сейчас (сейчас ~3.3 с от речи до текста).
4. CPU-нагрузка не растёт; RAM растёт максимум на 1× модели (мы уже грузим 1× сейчас).
5. Не ломаем non-KTalk сценарии (BlackHole-capture YouTube/offline video, встречи без extension).

## Non-Goals

- Уменьшение UI-latency (через меньший chunk_duration). Возможно потом отдельным change.
- MPS-ускорение GigaAM. Возможно потом.
- VAD-gate для экономии батареи. Возможно потом.
- Замена локального mic-capture. Microphone input остаётся через `sounddevice` как сейчас — там нет проблемы атрибуции (там один спикер по определению).
- Frontend UI-изменения. Протокол `transcript` WS-frame остаётся тот же.
- Удалённые ASR workers (Z270XX). Бенч показал что не нужны.
- Изменение diarization БД (ECAPA-TDNN) — останется для не-KTalk путей.

## Architecture

### Текущая (оставляем для не-KTalk)

```
BlackHole / ScreenCap ──▶ PCM mix 16 kHz ──▶ GigaAM ──▶ text
                                                  │
            KTalk extension ──▶ ktalk_bridge ─────┴──▶ attribute by time
                   (speaker_update по WS)            (post-hoc)
```

### Новая (для KTalk с extension)

```
Browser (extension)                      Python (ktalk_bridge + live)
──────────────────                       ────────────────────────────

conf._participantsMap
  ├─ pid_A (Alice, remote)
  │    └─ _audioTrack$.value.jitsiTrack.stream ──┐
  ├─ pid_B (Bob, remote)                         │
  │    └─ _audioTrack$.value.jitsiTrack.stream ──┤
  └─ pid_LOCAL (me)                              │
       └─ [existing mic path, unchanged]         │
                                                 ▼
                                      AudioContext (48 kHz)
                                      ┌──────────────────────────┐
                                      │ per-pid MediaStreamSource│
                                      │           ↓              │
                                      │ AudioWorkletNode:        │
                                      │  - 48 → 16 kHz downsample│
                                      │  - accumulate 3 s        │
                                      │  - emit Int16 chunk      │
                                      │           ↓              │
                                      │ WS binary / base64 JSON  │
                                      │   {type: "pcm_chunk",    │
                                      │    pid, speaker_name,    │
                                      │    seq, start_ms,        │
                                      │    end_ms, pcm}          │
                                      └───────────┬──────────────┘
                                                  │  ws://localhost:8765
                                                  ▼
                                      ┌──────────────────────────┐
                                      │ ktalk_bridge.py          │
                                      │  per-pid ring buffer     │
                                      │  batch collector (200 ms)│
                                      └───────────┬──────────────┘
                                                  │
                                                  ▼
                                      ┌──────────────────────────┐
                                      │ recog/live.py            │
                                      │ shared ASRModule         │
                                      │ forward(B × T) → decode  │
                                      │ attribute by speaker_name│
                                      └───────────┬──────────────┘
                                                  │
                                                  ▼
                                      transcript WS frame
                                      → extension UI
                                      → frontend
                                      → meetings/ storage
```

### Что не меняется

- `recog/ktalk_bridge.py` контракты `meeting_join` / `meeting_leave` / `transcript` — как есть.
- `meetings/<id>/transcript.jsonl` формат — как есть.
- Frontend WS-консьюмер — как есть.
- Microphone capture через `sounddevice` — как есть.
- `increment.py`, `speaker_db.py`, `prompt_registry.py` — не трогаются.
- ECAPA-TDNN используется только для non-KTalk (на границу проверить что ветка не отвалилась).

## Data / Protocol

### Новое WS-сообщение `pcm_chunk` (extension → bridge)

```json
{
  "type": "pcm_chunk",
  "pid": "067a4c51",
  "speaker_name": "Alice",
  "is_local": false,
  "seq": 42,
  "chunk_start_ms": 1745312345678,
  "chunk_end_ms": 1745312348678,
  "sample_rate": 16000,
  "pcm_base64": "<Int16 LE samples, base64-encoded>"
}
```

Design choices:
- **JSON + base64**, не binary frame. 3-с чанк = 96 КБ Int16 = 128 КБ base64, на localhost это несущественно, зато совместимо с существующим текстовым WS-протоколом. Можно мигрировать на binary позже без ломания контракта.
- **seq per-pid**, монотонный с момента подключения клиента. OOO detection — точно как для `speaker_update`.
- **chunk_start_ms / chunk_end_ms** — вalue `Date.now()` в браузере, но bridge применяет `clock_offset` (уже существующий в `_clock_offset`). Одна и та же синхронизация, что для `speaker_update`.
- **speaker_name** внутри фрейма: чтобы не делать join по pid в bridge. Имя может устареть (участник ушёл до приезда чанка) — это окей, атрибуция остаётся корректной.

### Новое WS-сообщение `pcm_transport_status` (extension → bridge)

Служебное: extension сообщает, сколько участников в аплинк-режиме, когда аттачится или отваливается worklet. Bridge использует для диагностики.

```json
{ "type": "pcm_transport_status", "active_pids": ["067a4c51", "8d568760"], "note": "attached|reattached|dispose" }
```

### Существующий `speaker_update` — остаётся

Используется для:
- UI-индикации "кто сейчас говорит" (независимо от ASR).
- Metadata в `transcript.jsonl` (for audit).
- Non-KTalk fallback (когда PCM pipe отключён).

## Implementation Scope by Module

### 1. Browser extension (`browser-extension/`)

- `src/audio-worklet/downsampler.js` (new) — `AudioWorkletProcessor`, 48 → 16 kHz через простой FIR (кратность 3), аккумулирует 3 с → emits Float32Array в main thread.
- `src/pcm_transport.js` (new) — на каждого удалённого participant аттачит `MediaStreamSource` → `AudioWorkletNode`. Получает чанки, конвертит в Int16, шлёт на WS. Управляет aттач/детач по событиям `trackAdded` / `trackRemoved` / `trackMuteChanged`.
- `src/init.js` — регистрирует `pcm_transport` аналогично существующему `audio-analyzer`. Подключение за feature-флагом `RECOG_PCM_TRANSPORT` (см. раздел Migration).
- `src/ws_api.js` — добавляется `sendWsPcmChunk(pid, speaker, seq, start, end, int16_array)`.
- Не ломаем существующий `audio-analyzer.js` — он остаётся для spectral-flatness VAD.

### 2. `recog/ktalk_bridge.py`

- Новый тип сообщения `pcm_chunk` в `_process_message`: читает, clock-корректит, кладёт в per-pid `asyncio.Queue`.
- Новый вспомогательный `PcmChannelBuffer` (один на pid) — хранит чанки с timestamps, умеет предоставить последние N чанков по запросу.
- `BatchCollector` (новый) — таймерный loop раз в `batch_interval_ms` (init 200 мс), сгребает готовые чанки со всех pid'ов, отдаёт в ASR через callback.
- `set_pcm_batch_callback(cb)` — контракт с `live.py`.
- Логика `get_dominant_speaker_for_period` не меняется — остаётся для non-PCM pути.

### 3. `recog/live.py`

- Новый `AsrBatchWorker` — один поток, слушает колбэк BatchCollector'а, батчит (до B=8) чанки, вызывает `model.forward(batch, lengths)` + `decoding.decode()`, диспатчит результаты по pid.
- Результат: `pid → text`, сразу формирует сегмент с `speaker_name`, `start_ms`, `end_ms`, пишет в `meeting_storage`, отправляет через существующий `send_transcript`.
- LiveTranscriber при `use_ktalk=True` и фиче-флаге PCM-transport: НЕ запускает `system_capture` (BlackHole/ScreenCap), поскольку его заменяет per-speaker PCM. Mic capture остаётся.
- При фиче-флаге off — всё по-старому, затрагиваемые пути не выполняются.

### 4. `recog/asr.py`

- Новый метод `transcribe_batch(pcm_batch: List[np.ndarray]) -> List[str]` — поддержка батчинга, внутри:
  - pad до одинаковой длины
  - `torch.stack` → `forward`
  - `decoding.decode` возвращает list, соответствующий индексам
- Используется только `AsrBatchWorker`'ом.

### 5. `recog/config.py`

- `PCM_CHUNK_DURATION_S = 3.0` — константа, пока не конфигурируема.
- `PCM_BATCH_INTERVAL_MS = 200` — идем.
- `PCM_MAX_BATCH_SIZE = 8` — идем.
- `RECOG_PCM_TRANSPORT` env var — фича-флаг (default: `0` = выключен на MVP до ручной валидации, потом `1`).

## Migration & Backwards Compatibility

**Feature flag**: `RECOG_PCM_TRANSPORT` (env). Default — off. Когда off, ничто не меняется на обеих сторонах.

Порядок включения после имплементации:
1. Флаг `RECOG_PCM_TRANSPORT=1` в `~/.config/recog/.env` на machine dev.
2. Build extension, перезалить в Chrome.
3. Перезапуск `recog-service.sh restart`.
4. Тест на живой KTalk встрече 15-20 мин.
5. Если норм — включить по умолчанию в коде (`default: 1`), оставить возможность force-off через env для отладки.

**Rollback**: `RECOG_PCM_TRANSPORT=0` → старый путь. Extension при off не аттачит worklet, не шлёт pcm_chunk. Bridge игнорит pcm_chunk если прилетают, no-op.

**Non-KTalk case** (YouTube, offline file, meetings без extension): extension не активен → PCM-transport не поднимается → ktalk_bridge не получает pcm_chunk → LiveTranscriber идёт по старому `system_device` пути. Не ломаем.

## Validation

### Unit

- `test_pcm_channel_buffer.py` — накапливает, отдаёт по окну, правильно обрезает старое.
- `test_asr_batch_worker.py` — батчит чанки из разных pid, результаты атрибутированы корректно.
- `test_asr_transcribe_batch.py` — `transcribe_batch([pcm1, pcm2])` возвращает 2 текста в том же порядке.

### Integration

- `tests/e2e/test_per_speaker_pipeline.py` (new) — использует существующую e2e-инфру (`ktalk_orchestrator.py`), но с `RECOG_PCM_TRANSPORT=1`. Сценарий: 2 гостя, оба говорят в overlap интервал 5-10 с, проверяем что transcript.jsonl содержит оба сегмента с правильными `speaker_name`.

### Manual smoke

- Живая встреча 3+ участника, 15 мин.
- Overlap: 2 говорят одновременно → в UI видно два блока.
- Granularity: короткая реплика в 1-2 с → атрибутирована правильно.
- Reconnect guest mid-call → PCM-transport'у событие `trackAdded` → не теряем его из виду.
- Local mic (я) — продолжает работать через sounddevice, попадает в тот же transcript.

### Benchmarks (post-implementation)

- WER на реальной встрече (recog'ом же) — сравнить с baseline на той же WAV через старый path. Ожидание: ΔWER < 1 pp.
- CPU usage live — `top` / `Activity Monitor`. Ожидание: +0-5% по сравнению с текущим.
- UI-latency (через debug-метрики в frontend) — ожидание: не хуже текущего ~3.3 с.

## Open Questions

1. **AudioWorklet для незнакомой codebase дорог.** Возможно проще обойтись `ScriptProcessorNode` (deprecated, но рабочий)? — Предпочту worklet, но if the dev experience будет мучительной, допускаю fallback на ScriptProcessor как степ-1, worklet как optimization.

2. **Что делать когда browser tab в фоне?** Chrome throttl'ит `AudioContext` в фоновых вкладках. Для KTalk-вкладки с активной видеовстречей это обычно не проблема (Chrome оставляет conferencing tabs активными), но надо проверить эмпирически.

3. **Guest без имени / псевдоним дубликатом.** Сейчас extension берёт `displayName || id`. Дубликаты имён ("Guest", "Guest") → два одинаковых `speaker_name` в transcript.jsonl. Не проблема для атрибуции (pid уникален), но UI может путать. Можно дополнять `speaker_name` суффиксом из последних 4 символов pid'а. Опционально, не MVP.

4. **Mic локального спикера через PCM-transport тоже?** Сейчас план — оставить mic через `sounddevice`. Но `_audioTrack$.value` у local participant'а тоже есть (я видел `hasJitsiStream: true` в пробах). Можно было бы унифицировать. **Не в MVP.** Мотивация оставить mic отдельно: если extension крашнется или tab закроют, mic-капча через `sounddevice` выживает и продолжает писать.

5. **Нужен ли VAD-gate на стороне браузера?** Не для CPU (есть 30× запас), а для экономии трафика: пересылать PCM тишины — расход base64 впустую. Но bandwidth внутри localhost пренебрежим. Отложено на «optimize потом».

6. **chunk_duration = 3 s зафиксирован.** Окей для MVP, но будущий change может сделать его адаптивным (короче когда UI активен, длиннее в фоне). Не в scope.

## Dependencies / Risks

- **Зависимость от extension buildchain.** Нужно webpack-рабочий — уже есть. AudioWorklet в webpack бандлится через `new URL(..., import.meta.url)` паттерн или копированием файла в `chrome.runtime.getURL()`.
- **Chrome-only.** Extension только для Chromium. Если пользователь мигрирует на Safari/Firefox — всё равно никогда не работало, не новый риск.
- **Jitsi API изменения.** `_audioTrack$.value.jitsiTrack.stream` — internal API. Может сломаться при апдейтах KTalk/Jitsi. Текущий `audio-analyzer.js` использует тот же селектор — риск распределён.
- **Race на отключении участника.** Если участник вышел, а чанки ещё в pipe — bridge может получить pcm_chunk после meeting_leave. Решение: при `meeting_leave` дренируем per-pid queues + инвалидируем текущую generation (существующий механизм `_on_pre_meeting_switch`).

## Follow-ups (не в этот change)

- Chunk-duration адаптивный.
- MPS для GigaAM (device override в `ASRModule.__init__`).
- VAD-gate для экономии трафика на idle-участниках.
- Binary WS-frame вместо base64 JSON.
- Унификация local mic в PCM-transport pipeline.
- Удаление legacy ECAPA-TDNN hot-path (оставить только для энроллмента).

## References

- Пробы: `tests/e2e/research/README.md`, probes 1-3
- Бенчмарк: `scripts/benchmarks/bench_gigaam_rtf.py`, results `/tmp/bench_gigaam_rtf.json`
- Существующий per-participant audio handling: `browser-extension/src/audio-analyzer.js:32-47`, `:92-112`
- Existing WS bridge: `ktalk_bridge.py:1145-1391`
- Speaker attribution: `2026-02-19-ktalk-diarization-optimization-design.md` (related prior work)
