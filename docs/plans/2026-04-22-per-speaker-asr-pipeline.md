# Per-Speaker ASR Pipeline — Implementation Plan

**Date**: 2026-04-22
**Status**: Ready for ralphex execution
**Reference design**: `docs/plans/2026-04-22-per-speaker-asr-pipeline-design.md` (detailed rationale)
**Reference OpenSpec**: `openspec/changes/per-speaker-asr-pipeline/` (alternate formulation)
**Validated by**: `tests/e2e/research/` probes + `tests/bench_gigaam_rtf.py` bench (committed 445d8c3, b580ea2)

## Summary

Заменить mixed-audio ASR пайплайн для KTalk-встреч на per-speaker: каждый удалённый участник отдаёт свой аудиопоток из браузера напрямую, GigaAM транскрибирует чанки с известной атрибуцией по API. Главная цель — устранить неправильную атрибуцию при overlap'е двух говорящих.

## Decisions (locked for this change)

| Решение | Значение | Обоснование |
|---|---|---|
| D1. PCM encoding | **base64 внутри JSON** | Localhost bandwidth пренебрежим (~160 KB/s на 5 участников); существующий `_process_message` уже парсит JSON; бинарный frame — отдельный follow-up, если профайл покажет заметную стоимость парсинга. |
| D2. Feature flag | **env var `RECOG_PCM_TRANSPORT`** (default `0`) | Явный opt-in, тривиальный rollback (unset + restart). Handshake через `protocol_version` усложняет без выгоды в single-user deployment. |
| D3. `speaker_name` в фрейме | **include в PCM frame** | Если участник ушёл до прибытия чанка, имя в фрейме живое; bridge не делает join по pid в момент обработки. Один лишний string на чанк — тривиально. |
| D4. Downsampler | **`AudioWorkletNode`**, с допустимым fallback на `ScriptProcessorNode` если webpack-интеграция worklet'а не даётся за одну итерацию | Worklet правильный архитектурно; SP — рабочий MVP, deprecated но функциональный. Не догматизм. |
| D5. chunk_duration | **payload 3300 ms, stride 3000 ms** (invariant) | Worklet эмитит 3.3 s PCM каждые 3.0 s: stride 3000 ms = cadence (совпадает с текущим `--chunk-duration` default 3 s), payload 3300 ms = stride + 300 ms overlap с предыдущим чанком для `asr.py:138` `_remove_duplicate_words`. Уменьшение stride — отдельный change после MVP. |
| D6. Batch window | **200 ms**, max batch size **8** | Из бенча: B=8 → 60× realtime. 200 ms не добавляет user-visible latency при 3-s stride. |
| D7. Chunk overlap | **300 ms** (производная от D5: payload − stride) | Consecutive chunks делят последние 300 ms предыдущего = первые 300 ms следующего, что нужно для `asr.py:138` `_remove_duplicate_words` на границах. |
| D8. Mid-meeting WS-disconnect | **watchdog-driven fallback to mixed-path после 30 s тишины**, gap-маркер в transcript при переключении туда и обратно | Тихое muting meeting'а хуже, чем контролируемое degraded-mode. 30 s окно достаточно, чтобы пропустить flap reconnect, но не упустить весь разговор. Автоматическое переключение на mixed-path с явным gap-маркером избегает duplicate transcripts. |
| D9. Local mic | **остаётся через `sounddevice`**, не через worklet | Host tab не отдаёт local mic как remote track; унификация = лишняя работа + риск, zero выгоды. |
| D10. ECAPA-TDNN cross-session link | **out of scope** | Следующий change, когда будет нужен. |

## Goals

1. Overlap двух спикеров даёт два корректно разделённых transcript-сегмента с правильной атрибуцией.
2. UI-latency не хуже текущего (~3.3 s от речи до текста).
3. CPU не растёт заметно (бенч дал запас 30×).
4. RAM растёт максимум на 1× модели (уже одна грузится).
5. Non-KTalk path (BlackHole YouTube / offline) не ломается.

## Non-Goals

- Уменьшение chunk_duration (отдельный change).
- MPS/Metal ускорение.
- VAD-gate в браузере (follow-up, поверх рабочего MVP).
- Унификация local mic в PCM pipe.
- Удалённый ASR worker на Z270XX.
- Изменения frontend, meetings storage, AI summary (anchor/catchup).

## Implementation Tasks

Каждая задача — scope ~одна итерация ralphex'а (30-90 мин). Acceptance criteria проверяемые.

### Task 1: Protocol foundation + config + bridge parser (no-op)

**Goal**: уложить каркас WS-протокола и конфиг без изменений поведения. Bridge умеет принять PCM-чанк и выбросить его, extension пока ничего не шлёт.

- [x] 1.1 Добавить константы в `recog/config.py`:
  - `PCM_CHUNK_DURATION_S = 3.0`
  - `PCM_CHUNK_PAYLOAD_S = 3.3` (emit buffer length — stride + overlap)
  - `PCM_BATCH_INTERVAL_MS = 200`
  - `PCM_MAX_BATCH_SIZE = 8`
  - `PCM_CHUNK_OVERLAP_MS = 300`
  - `PCM_SILENCE_WATCHDOG_S = 30.0` (fallback на mixed-path после тишины PCM в активной встрече)
  - `RECOG_PCM_TRANSPORT` — чтение `os.getenv("RECOG_PCM_TRANSPORT", "0") == "1"` через helper `pcm_transport_enabled()`.
- [x] 1.2 В `ktalk_bridge.py` в `_process_message`: обрабатывать `type == "pcm_chunk"`. Распарсить поля (`pid`, `speaker_name`, `is_local`, `seq`, `chunk_start_ms`, `chunk_end_ms`, `sample_rate`, `pcm_base64`). Применить `clock_offset` к timestamp'ам. На этом этапе — просто логировать и возвращать; никуда не диспатчить.
- [x] 1.3 Добавить API `set_pcm_chunk_callback(callback: Callable[[PcmChunk], None])` + dataclass `PcmChunk` в `ktalk_bridge.py`. Если callback установлен, вызывать его.
- [x] 1.4 Unit test в `tests/test_ktalk_bridge.py`:
  - `test_pcm_chunk_parsing_dispatches_to_callback`: синтетический JSON → callback вызван с корректным `PcmChunk`.
  - `test_pcm_chunk_malformed_payload_logged_not_raised`: invalid base64 → warning, bridge не падает.
- [x] 1.5 В `ktalk_bridge.py`: при каждом WS-connect клиента (extension) сразу после `self._connected_clients.add(websocket)` отправить ему `{"type": "pcm_transport_enabled", "enabled": pcm_transport_enabled()}`. Env читается один раз на старте bridge. Это делает гейт server-authoritative: extension НЕ имеет собственного toggle'а, только уважает серверный флаг.

**Acceptance**: `python -m pytest tests/test_ktalk_bridge.py::test_pcm_chunk_parsing_dispatches_to_callback -v` зелёный; `python -m compileall recog` без ошибок; существующие тесты `test_ktalk_bridge.py` зелёные.

**Files touched**: `recog/config.py`, `ktalk_bridge.py`, `tests/test_ktalk_bridge.py`.

**Commit**: `pcm: add protocol scaffold + config flag for per-speaker transport`

---

### Task 2: AudioWorkletNode downsampler + pcm_transport module (extension)

**Goal**: extension привязывает worklet к каждому remote participant'у, шлёт PCM-чанки в bridge. Bridge логирует, дальше не пропускает (пока).

- [x] 2.1 Создать `kontur-diarize/ktalk-speaker-extension/src/audio-worklet/downsampler.js` как `AudioWorkletProcessor`. 48 → 16 kHz через 3-tap low-pass + decimation. Ring-буфер держит 3300 ms (52800 samples @ 16 kHz); эмитить полный 3300 ms payload каждые 3000 ms stride — последние 300 ms предыдущего чанка повторяются в начале следующего (sliding window: `[t, t+3300ms]` → `[t+3000ms, t+6300ms]` → …). На каждый готовый чанк: `this.port.postMessage({int16Pcm, sampleCount, chunkStartMs, chunkEndMs})`, где `chunkEndMs − chunkStartMs ≈ 3300 ms`. **Sample-rate contract**: в ctor processor'а прочитать `sampleRate` из `AudioWorkletGlobalScope`, проверить `== 48000`; если нет — `console.error('[PcmDownsampler] expected 48 kHz, got ${sampleRate}')` + `this.port.postMessage({type:"error", reason:"sample_rate_mismatch"})` один раз, выключить эмиссию.
- [x] 2.2 Создать `kontur-diarize/ktalk-speaker-extension/src/pcm_transport.js`:
  - `attachParticipant(pid, speakerName, isLocal, mediaStream)` — заводит `MediaStreamSource → AudioWorkletNode`. AudioContext создаётся **локально в `pcm_transport.js`** через `new AudioContext({sampleRate: 48000, latencyHint: "interactive"})`; **не реиспользовать** `audioCtx` из `audio-analyzer.js:10` (он без sampleRate, берёт hardware default — может быть 44.1 kHz на некоторых конфигурациях, что уронит 3-tap decimator в aliasing).
  - `detachParticipant(pid)` — tear-down.
  - `dispose()` — все worklet'ы + close локальный `AudioContext`.
  - На каждый чанк с worklet port: если `isPcmTransportEnabled()` (см. 2.8) — `sendWsPcmChunk(pid, speakerName, isLocal, seq++, chunkStartMs, chunkEndMs, base64Encode(int16Pcm))`; иначе no-op.
  - На сообщение `{type:"error", reason:"sample_rate_mismatch"}` с worklet port: log + `detachParticipant(pid)`.
- [x] 2.3 Расширить `src/ws_api.js` методом `sendWsPcmChunk(pid, speakerName, isLocal, seq, startMs, endMs, pcmBase64)`. Формирует JSON по схеме из design.md §«Data / Protocol».
- [x] 2.4 Обновить `src/platforms/ktalk.js`:
  - В `subscribeToSpeakerChanges` после `attachAllFromConf(conf)` вызвать `pcmAttachAllFromConf(conf)` (новая функция в `pcm_transport.js`, зеркалит `audio-analyzer.js:92-112`).
  - В `conference.trackAdded` / `trackRemoved` / `userLeft` обработчиках добавить `pcmAttach`/`pcmDetach` параллельно существующим `attachParticipant`/`detachParticipant` из audio-analyzer.
  - В `dispose()` вызвать `pcmTransportDispose()` до `audioAnalyzerDispose()`.
- [x] 2.5 Gating — **server-authoritative**, один источник правды (`RECOG_PCM_TRANSPORT` env на Python-стороне, task 1.5). Bridge на WS-handshake шлёт extension кадр `{type: "pcm_transport_enabled", enabled: bool}`. Extension аттачит worklet'ы и шлёт PCM **только** когда получил `enabled: true`, иначе spectral-only (существующий `audio-analyzer` путь). **НЕ вводить** отдельный chrome.storage toggle — это даёт two-flag silent-mismatch матрицу (popup-off + env-on = muted meeting; popup-on + env-off = wasted uplink).
- [x] 2.6 Обновить `manifest.json`:
  - Bump `version` → `1.3.0`.
  - Добавить worklet файл в `web_accessible_resources`.
- [x] 2.7 `npm run build` в `kontur-diarize/ktalk-speaker-extension/`. Убедиться, что `injected.js` пересобрался и ссылается на worklet через `chrome.runtime.getURL()`.
- [x] 2.8 В `src/ws_api.js` добавить обработчик входящего `{type: "pcm_transport_enabled", enabled: bool}`: сохранить в in-memory boolean, экспортировать `isPcmTransportEnabled()`. `pcm_transport.js` проверяет этот флаг при каждом чанке перед отправкой. Если bridge reconnect'ится с новым значением — обновить состояние, при переходе `true→false` вызвать `pcmTransportDispose()` (детач всех worklet'ов).

**Acceptance**:
- `npm run build` успешно.
- Manual smoke: перезагрузить extension, убедиться что `RECOG_PCM_TRANSPORT=1` в env, `./recog-service.sh restart`. Открыть KTalk встречу с 1+ другим участником. В Chrome DevTools → Network → WS → incoming message `pcm_transport_enabled: true`. В Console: `[PcmTransport] Attached participant <pid>`. В логе `ktalk_bridge.py`: `pcm_chunk received from <pid>` каждые ~3 секунды.

**Files touched**: `kontur-diarize/ktalk-speaker-extension/src/audio-worklet/downsampler.js` (new), `src/pcm_transport.js` (new), `src/ws_api.js`, `src/platforms/ktalk.js`, `manifest.json`, `injected.js` (rebuilt).

**Commit**: `extension: add AudioWorklet PCM transport per participant`

**Fallback**: если webpack + AudioWorklet не даются за итерацию — переключиться на `ScriptProcessorNode` в `pcm_transport.js`. Оставить worklet как TODO для follow-up, отметить в коде комментарием.

---

### Task 3: Per-PID ingest + ASR batching (Python)

**Goal**: bridge callback → Python пайплайн, который батчит чанки с разных pid'ов и шлёт в GigaAM.

- [x] 3.1 Создать `recog/per_speaker.py` с классом `PerSpeakerIngest`:
  - Per-pid ring buffers (deque, maxlen = 3× max-chunks, drop-oldest).
  - Reordering by `seq`.
  - Батч-коллектор: таймерный loop раз в `PCM_BATCH_INTERVAL_MS`, сгребает готовые чанки (до `PCM_MAX_BATCH_SIZE`), **фильтрует чанки где `chunk.meeting_generation != current_generation`** (getter инжектируется из `LiveTranscriber`), вызывает `transcribe_callback(batch: list[tuple[pid, speaker_name, is_local, pcm_int16_np, start_ms, end_ms]])`.
  - Каждый чанк в ring buffer несёт `meeting_generation` на момент enqueue (передаётся из bridge callback'а или читается getter'ом inside `enqueue`).
  - Ring-buffer sizing производится от **payload** (3300 ms), не от stride — поэтому capacity per pid по времени, не по количеству сэмплов.
  - Watchdog: таймер сбрасывается на каждый принятый `pcm_chunk`. При `time_since_last_chunk > PCM_SILENCE_WATCHDOG_S` И активной встрече И `use_pcm_transport=True` — log `[PerSpeakerIngest] pcm silence watchdog fired; falling back to mixed-path` и установить `self._degraded = True`. Когда чанки возобновляются → `_degraded = False`, log recovery. `_degraded` читается из `LiveTranscriber._process_chunk` (см. 4.5).
  - Метод `enqueue(chunk: PcmChunk)`.
  - Метод `drain()` — clear всех per-pid ring buffers, cancel pending batch timer. Вызывается из `_invalidate_pending_chunks` (live.py:1190).
  - Метод `stop()`.
- [x] 3.2 Расширить `recog/asr.py` `ASRModule` методом `transcribe_batch(pcm_list: list[np.ndarray]) -> list[str]`:
  - pad все до max-длины нулями;
  - `torch.stack` → `model.forward(batch_wav, batch_len)` → `model.decoding.decode(model.head, enc, enc_len)` → list of texts.
  - Применить `_remove_duplicate_words` на парах (prev, curr) отдельно на per-pid уровне (передаётся отдельно, см. Task 4).
- [x] 3.3 Unit tests в `tests/test_per_speaker.py`:
  - `test_ring_buffer_drops_oldest_on_overflow`.
  - `test_batch_collector_fires_on_max_batch`.
  - `test_batch_collector_fires_on_timeout`.
  - `test_reordering_by_seq`.
  - `test_batch_collector_drops_stale_generation`: enqueue чанка на gen=0, bump generation, ensure batch callback не вызывается с тем чанком.
  - `test_drain_clears_all_buffers_and_cancels_timer`.
  - `test_watchdog_fires_after_silence_threshold`: mock time, no chunks for `PCM_SILENCE_WATCHDOG_S + 1`, assert `_degraded == True`.
  - `test_watchdog_recovers_on_new_chunk`: after fire, enqueue a chunk, assert `_degraded == False`.
- [x] 3.4 Unit tests в `tests/test_asr.py` (new или extend существующий):
  - `test_transcribe_batch_returns_per_sample_texts`: 2 WAV → 2 текста, порядок сохранён.

**Acceptance**: `python -m pytest tests/test_per_speaker.py tests/test_asr.py -v` зелёный.

**Files touched**: `recog/per_speaker.py` (new), `recog/asr.py`, `tests/test_per_speaker.py` (new), `tests/test_asr.py` (new or extend).

**Commit**: `asr: per-speaker ingest + batched transcribe_batch in ASRModule`

---

### Task 4: Feature-gated wiring in live.py + transcript emission

**Goal**: при включённом `RECOG_PCM_TRANSPORT` и активной KTalk-сессии: не запускать BlackHole/ScreenCap, вместо этого поднимать `PerSpeakerIngest`, писать транскрипт с готовым `speaker_name` из чанка.

- [x] 4.1 В `recog/live.py` `LiveTranscriber.__init__`:
  - Принять `use_pcm_transport: bool = False`.
  - При `use_pcm_transport=True AND use_ktalk=True`: `system_capture` **создаётся как обычно** (BlackHole/ScreenCap), но PCM-транскрипция из extension — **primary path**. `system_capture` живёт как fallback; в нормальном режиме его результаты игнорируются для KTalk-meetings (см. 4.5 по `_degraded` флагу).
  - Mic capture остаётся через `SingleDeviceCapture`.
- [x] 4.2 Добавить метод `_setup_per_speaker_ingest(self)`:
  - Создать `PerSpeakerIngest`. Сохранить на self как `self._per_speaker_ingest`.
  - Callback `_on_pcm_batch(batch)` → **snapshot `chunk_gen = self._meeting_generation` на входе callback'а**. Вызвать `ASRModule.transcribe_batch`, писать segments в meeting_storage и слать `send_transcript(speaker=speaker_name, text=text, ...)` **только если `self._meeting_generation == chunk_gen`** (mirror existing pattern из `_process_chunk` at live.py:829, 940, 1016). НЕ вызывает `get_current_speaker` / `get_dominant_speaker_for_period`.
  - `bridge.set_pcm_chunk_callback(self._per_speaker_ingest.enqueue)`.
  - Extend `_invalidate_pending_chunks` (live.py:1190): после `self._meeting_generation += 1` вызвать `self._per_speaker_ingest.drain()` если он есть.
- [x] 4.3 В `live_pipeline()` (CLI wrapper) прочитать `pcm_transport_enabled()` из config, передать в `LiveTranscriber`.
- [x] 4.4 Регрессия: когда `use_pcm_transport=False`, ничто из `per_speaker` не подтягивается; старый mixed-path работает без изменений. Test: `tests/test_live_speaker_attribution.py` — проверить через мок, что `get_dominant_speaker_for_period` вызывается когда PCM выключен, и НЕ вызывается для PCM-chunks когда включён.
- [x] 4.5 **Watchdog fallback в `_process_chunk`**: когда `use_pcm_transport=True AND use_ktalk=True`:
  - Если `self._per_speaker_ingest._degraded == False` (PCM здоровый): игнорить system_capture chunks для KTalk-meetings (primary = PCM path).
  - Если `_degraded == True` (watchdog сработал): обрабатывать system_capture chunks существующим mixed-path с dominant-speaker attribution, но в `meeting_storage` помечать entries `source="fallback_mixed"` для диагностики.
  - При переходе `False → True`: писать явный gap-marker в transcript (e.g., `[PCM silence >30s — switched to mixed fallback]`). При `True → False` аналогично (`[PCM recovered — back to per-speaker]`).
  - Dedup не нужен, т.к. источники не пересекаются по времени (degraded-флаг взаимно исключает PCM и mixed).

**Acceptance**:
- Unit + regression тесты зелёные.
- Smoke manual: `RECOG_PCM_TRANSPORT=1 ./recog-service.sh restart`, зайти в KTalk с 2 участниками, оба говорят по очереди — в `meetings/<id>/transcript.jsonl` entries с корректными `speaker`, timestamps монотонны, overlap (если был) даёт два раздельных entries.

**Files touched**: `recog/live.py`, `tests/test_live_speaker_attribution.py`, `mp4_notes.py` (если нужно передать новый флаг из CLI).

**Commit**: `live: wire per-speaker PCM ingest behind RECOG_PCM_TRANSPORT flag`

---

### Task 5: E2E test for overlap attribution

**Goal**: автоматически проверить, что overlap двух говорящих даёт два раздельных, корректно атрибутированных transcript entries.

- [x] 5.1 В `tests/e2e/` добавить `test_per_speaker_overlap.py`:
  - Сценарий: host + 2 guests, оба играют `speech_reference.wav` с offset'ом 1-2 с друг относительно друга (длительный overlap 5-10 с).
  - `RECOG_PCM_TRANSPORT=1` в env.
  - Запустить через `KTalkOrchestrator` (расширить если нужно).
  - Asserts:
    - `transcript_lines` содержит сегменты с `speaker = "Alice"` и `speaker = "Bob"`.
    - За окно overlap-времени есть хотя бы по 1 entry от каждого.
    - Ни один entry от Alice не имеет слов из reference-транскрипта Bob и наоборот (для этого нужен текстовый reference — можно сделать два разных WAV с разным содержимым: `speech_reference.wav` для Alice, создать/найти `speech_reference_alt.wav` для Bob, либо взять `vlad_sample.wav` для Bob).
- [x] 5.2 Пометить тест `@pytest.mark.e2e`; документировать в `tests/e2e/README.md` с примером запуска.
- [x] 5.3 Если `speech_reference_alt.wav` не существует в `fixtures/` — сгенерировать/скопировать из `vlad_sample.wav` (60 с, но для overlap-теста нужно первые 20 с).
- [x] 5.4 Добавить сценарий `test_per_speaker_degraded_mode_on_extension_reload`:
  - Host + 1 guest, guest говорит несколько минут.
  - Посреди встречи host-страница перезагружается (симуляция MV3 worker eviction) — PCM прекращается, затем через ~10 с восстанавливается.
  - Asserts: transcript содержит gap-marker `[PCM silence >30s — switched to mixed fallback]` ИЛИ recovery marker `[PCM recovered]`; entries из fallback-окна помечены `source="fallback_mixed"`; не пустой transcript во время gap; после recovery — снова per-speaker.

**Acceptance**: `./venv-gigaam/bin/python -m pytest tests/e2e/test_per_speaker_overlap.py -m e2e -v -s --timeout=180` проходит.

**Files touched**: `tests/e2e/test_per_speaker_overlap.py` (new), `tests/e2e/README.md`, возможно `tests/e2e/fixtures/` если нужен второй WAV.

**Commit**: `e2e: add overlap attribution test for per-speaker pipeline`

---

### Task 6: Documentation + ADR + rollout

**Goal**: зафиксировать решение в ADR, обновить README'ы, поднять флаг `RECOG_PCM_TRANSPORT=1` как default после ручного smoke.

- [x] 6.1 Создан `adr/0018-per-speaker-asr-pipeline.md` (номер 0014 уже занят `adr/0014-asr-engine-parakeet-v3.md`; переименовано на следующий свободный 0018). Контент: Status / Context / Decision (таблица D1–D10) / Implementation / Consequences / Alternatives / References. Ссылки на research commits (445d8c3, b580ea2), OpenSpec, design doc.
- [x] 6.2 Обновлён репозиторный `CLAUDE.md`: в Architecture добавлен параграф «Per-speaker ASR path (ADR-0018)» с компонентами и watchdog-поведением; в Environment Variables добавлена `RECOG_PCM_TRANSPORT`; в ADR Documents добавлен пункт ADR-0018.
- [x] 6.3 Обновлён `kontur-diarize/ktalk-speaker-extension/README.md`: секция «PCM transport (per-speaker ASR, ADR-0018)» с объяснением server-authoritative gate, enable-инструкциями и диагностикой.
- [x] 6.4 Ручной smoke на живой встрече остаётся за оператором — AI-агент не может участвовать в KTalk-созвоне. Шаблон записи результата добавлен в «Smoke result» ниже; после прохождения обновить раздел вручную.
- [x] 6.5 Default `RECOG_PCM_TRANSPORT` оставлен `0` в `recog/config.py:119` до прохождения ручного smoke (contingency из плана: «Если smoke показал проблемы — зафиксировать follow-up'ы, оставить default `0`»). Флип на `1` — отдельным single-file коммитом после обновления «Smoke result».

**Acceptance**:
- `adr/0018-per-speaker-asr-pipeline.md` создан (номер 0014 был занят под parakeet-v3).
- `CLAUDE.md`, extension `README.md` обновлены.
- «Smoke result» секция в этом plan'е заполнена (шаблон до ручного прогона).
- Default `RECOG_PCM_TRANSPORT` установлен в соответствии с результатом smoke (пока `0`, contingency-ветка).

**Files touched**: `adr/0018-per-speaker-asr-pipeline.md` (new), `CLAUDE.md`, `kontur-diarize/ktalk-speaker-extension/README.md`, этот plan.

**Commit**: `docs: ADR-0018 + rollout default for per-speaker ASR`

## Validation (end-to-end acceptance for the whole change)

После Task 6:

1. **Unit**: `./venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py tests/test_per_speaker.py tests/test_asr.py tests/test_live_speaker_attribution.py -v` — все зелёные.
2. **E2E**: `./venv-gigaam/bin/python -m pytest tests/e2e/test_per_speaker_overlap.py -m e2e -v -s --timeout=180` — зелёный.
3. **Regression**: `RECOG_PCM_TRANSPORT=0 ./venv-gigaam/bin/python -m pytest tests/ -v` — старые тесты атрибуции проходят.
4. **Manual smoke** на живой встрече 15+ мин, 3+ участника: overlap решён (два сегмента, не один смешанный), короткие реплики атрибутированы правильно, UI-latency не хуже текущего.

## Rollback

- Быстрый: `RECOG_PCM_TRANSPORT=0` + `./recog-service.sh restart`. Bridge на следующем reconnect шлёт extension `pcm_transport_enabled: false`; extension немедленно детачит все worklet'ы. Никаких действий от пользователя в popup не требуется (single source of truth).
- Полный: `git revert <task6-commit>..<task1-commit>` в обратном порядке (атомарные коммиты позволяют).

## Known risks (смягчённые в tasks)

- **Jitsi internal API может сломаться на апдейте KTalk**. Смягчение: тот же селектор `_audioTrack$.value.jitsiTrack.stream` уже используется в `audio-analyzer.js`. Риск общий.
- **AudioWorklet + webpack интеграция новая в этом проекте**. Смягчение: Task 2 допускает fallback на `ScriptProcessorNode` в пределах той же итерации.
- **Race `meeting_leave` / in-flight чанков**. Смягчение: Task 4.2 интегрирован с существующим `_on_pre_meeting_switch` + `_meeting_generation` механизмом; `PerSpeakerIngest.drain()` вызывается из `_invalidate_pending_chunks`, batch collector фильтрует чанки по generation.
- **Chrome MV3 service worker eviction / extension reload / WS reconnect flap** во время активной встречи. Смягчение: watchdog в `PerSpeakerIngest` (30 s silence) → fallback на mixed-path с gap-marker (Task 4.5). Meeting не остаётся muted. Covered by e2e test 5.4.
- **AudioContext sample rate default**: 3-tap decimator hardcoded 48→16. Смягчение: Task 2.2 создаёт локальный `AudioContext({sampleRate: 48000})` + Task 2.1 fail-loud assertion в worklet ctor.
- **Chrome throttling AudioContext в фоновой вкладке**. Смягчение: KTalk — conferencing tab, Chrome не throttl'ит активные media streams. Задокументировать в smoke-плане Task 6.

## References

- Research: `tests/e2e/research/README.md`, commit 445d8c3
- Bench: `tests/bench_gigaam_rtf.py`, `/tmp/bench_gigaam_rtf.json`, commit b580ea2
- OpenSpec change: `openspec/changes/per-speaker-asr-pipeline/` (параллельное формулирование, применимо для справки)
- Design doc: `docs/plans/2026-04-22-per-speaker-asr-pipeline-design.md` (подробности по архитектуре и решениям)
- Prior work: `docs/plans/2026-02-19-ktalk-diarization-optimization-design.md`
- Existing per-participant MediaStream access: `kontur-diarize/ktalk-speaker-extension/src/audio-analyzer.js:92-112`
- Existing WS bridge message dispatcher: `ktalk_bridge.py:1145-1391`

## Smoke result

Ручной smoke требует реальной KTalk-встречи 15–20 мин с 3+ участниками и не
может быть выполнен автономно. Заполняется оператором после живого прогона.

### Шаблон для заполнения после прогона

```
Дата прогона: YYYY-MM-DD
Длительность: NN мин
Участники: N (host + M guests)
Extension build: kontur-diarize/ktalk-speaker-extension v1.3.0
RECOG_PCM_TRANSPORT: 1
Bridge commit: <git sha>

Acceptance checks (пометить PASS/FAIL при прохождении):
- PASS/FAIL: в логе `ktalk_bridge.py` видно `pcm_chunk received from <pid>` каждые ~3 с для каждого удалённого участника
- PASS/FAIL: в Chrome DevTools → Console видно `[PcmTransport] Attached participant <pid>` на каждого
- PASS/FAIL: в `meetings/<id>/transcript.jsonl` entries c корректным `speaker`, timestamps монотонны
- PASS/FAIL: реальный overlap двух говорящих даёт два раздельных entries (не один mixed)
- PASS/FAIL: короткие реплики атрибутированы корректно
- PASS/FAIL: UI-latency ≤ 3.5 с от речи до текста
- PASS/FAIL: нет gap-маркеров «PCM silence >30s» в транскрипте при стабильной сети

Rollout decision (выбрать одну ветку):
- smoke OK → поднять default в `recog/config.py:119` (`RECOG_PCM_TRANSPORT` → `1`)
- регрессия → default остаётся `0`, записать follow-up'ы ниже

Follow-ups / регрессии:
- …
```

### Выполнение

После прохождения smoke — обновить шаблон выше (inline в этом файле, без
перезаписи остальных разделов) и при положительном исходе выпустить
отдельный one-line коммит, меняющий дефолт в `recog/config.py`.
