# ADR-0020: Remote GigaAM offload to a GPU server with lazy local fallback

## Status
Accepted (2026-05-31)

## Context

Live KTalk-транскрипция и offline-CLI крутят GigaAM (`v3_e2e_rnnt`) локально на
MacBook. Все hot inference paths делят один экземпляр модели под
`ASRModule._model_lock` (ADR-0018 per-speaker batched forward, ADR-0019 refine).
На M1 Pro 16 GB модель занимает ~1.6 GB RSS, throughput на батче ~60× realtime
(`scripts/benchmarks/bench_gigaam_rtf.py`, ADR-0018). Этого хватает на live-путь, но:

- Refine-контур (ADR-0019) конкурирует с live за единственный mutex; cooperative
  abort смягчает, но не убирает контеншн.
- При длинных встречах копится MPS NSObject-утечка (ADR-0019 «Operational
  concerns») — митигирована autoreleasepool'ом, но это симптом перегруза одной
  машины ASR + UI + браузер + расширение.
- Offline-CLI на длинных файлах надолго занимает локальный GPU/CPU.

В той же LAN живёт **GPU-сервер** — RTX PRO 6000 Blackwell, 96 GiB GDDR7
(см. skill `local-linux-models`). На нём поднят OpenAI-Whisper-совместимый
ASR-gateway (`https://<gpu-host>/asr`), фронтящий GigaAM v3
+ Qwen3-ASR. Замеры:

- **RTF ~99×** на GigaAM на Blackwell — на порядок выше локального батча.
- **LAN RTT 3–5 ms** (`ping <gpu-host>` → min 3.858 ms). По правилу из
  global CLAUDE.md это настоящая LAN, не DERP-relay; сетевой round-trip
  пренебрежимо мал относительно inference.
- Gateway уже отдаёт `GET /healthz → {ok, variant: v3_e2e_rnnt, diarization}` и
  `GET /v1/models`, `POST /v1/audio/transcriptions` (multipart: `file`, `model`,
  `language`, `response_format`, `prompt`, `temperature`).

### Текущий контракт gateway и разрыв

`response_format=json → {text}`; `verbose_json → {task, language, duration, text,
segments:[{id, start(sec), end(sec), text, speaker, ...}]}`. Сегменты — только на
уровне фраз: backend зовёт `model.transcribe_longform(wav)`.

**Разрыв:** нет word-level timestamps. Refine-контур (ADR-0019) выравнивает
refined-текст на live-сегменты по словам через
`ASRModule.transcribe_with_timestamps`, который нормализует `Word.start/end` в
**int миллисекунды**. Remote-клиент обязан вернуть ту же форму, иначе refine
offload'ить нельзя — а это один из четырёх путей, которые мы хотим вынести.

### Constraints

- Поведение репозитория не должно меняться, пока пользователь сам не включит
  offload. Дефолт — локальный путь.
- Remote-клиент обязан отдавать **байт-совместимые** структуры с локальным
  `ASRModule` (тот же `Segment`, тот же `TranscriptionResult` с `Word` в int ms),
  чтобы downstream (refine alignment, segment_id-контракт, AI-саммари) не трогать.
- Сеть и удалённый GPU — внешние зависимости: они могут отвалиться. Падение
  remote'а не должно ронять live-встречу.
- Один репозиторий: патч gateway + deploy-шаги живут внутри recog (никакого
  отдельного asr-api репо).

## Decision

Ввести опциональный **remote ASR offload** на GPU-сервер за единым конструктором
`build_asr()` с **ленивым** локальным fallback'ом и circuit breaker'ом. Включается
строго через env; committed default оставляет всё локально.

### D1. `build_asr()` — единственный конструктор для всех вызывающих

`recog/remote_asr.py::build_asr() -> ASRModule | FallbackASR`. Читает
`recog.config`: при `RECOG_ASR_BACKEND == "remote"` возвращает `FallbackASR`
поверх `RemoteASRClient`, иначе — обычный `ASRModule()`. `LiveTranscriber.__init__`
и offline-CLI в `recog/cli.py` конструируют ASR только через `build_asr()`. Это
единственная точка ветвления local/remote — остальной код видит один интерфейс.

### D2. Offload ВСЕХ четырёх GigaAM-путей

Выносятся все четыре inference-поверхности, а не только «дорогой» offline:

1. `transcribe_batch` — live per-speaker batched forward (ADR-0018).
2. `transcribe_simple` — offline CLI (`recog/cli.py`).
3. `transcribe_with_timestamps` — refine-контур (ADR-0019), требует
   word-timestamps от gateway.
4. Прямой `model.forward()` mic / mixed-fallback в `live.py::_transcribe_direct`.

Путь №4 не имел публичного метода — тело forward+decode выделяется в новый
`ASRModule.transcribe_chunk(audio, sample_rate=16000) -> Optional[str]` (под
`_model_lock`, `torch.inference_mode()`, `_mps_autoreleasepool`,
`_maybe_empty_mps_cache`; `None` на исключении — сохраняет best-effort контракт
старого `_transcribe_direct`). `RemoteASRClient` и `FallbackASR` реализуют тот же
метод, так что `_transcribe_direct` просто вызывает `self.asr.transcribe_chunk(...)`.

### D3. `RemoteASRClient` поверх OpenAI-Whisper gateway

Sync `httpx.Client` (стандартизируемся на httpx; в venv есть и requests/aiohttp).
WAV в памяти через `soundfile.write(BytesIO, float32, sr, format="WAV",
subtype="FLOAT")`. Соответствие путей:

| метод | gateway | response | маппинг |
|---|---|---|---|
| `healthz()` | `GET /healthz` | `{ok, variant, diarization}` | `True` только если reachable И `ok` |
| `transcribe_chunk(audio)` | `POST` multipart, `response_format=json` | `{text}` | `text`; `RemoteASRError` на любом HTTP/parse fail |
| `transcribe_batch(pcm_list)` | `ThreadPoolExecutor(max_workers)` над `transcribe_chunk` | — | order-preserving; ЛЮБОЙ raise → `RemoteASRError` (caller fallback'ит весь батч локально) |
| `transcribe_simple(path)` | `POST file`, `verbose_json` | `{segments:[{start,end,text}]}` | `segments[] → recog.models.Segment(text, start, end)` |
| `transcribe_with_timestamps(audio)` | `POST WAV` + form `timestamp_granularities="word"` | top-level `words:[{word,start(sec),end(sec)}]` | `Word(text=word, start=int(round(s*1000)), end=int(round(e*1000)))` → `TranscriptionResult(text, words)` |

`transcribe_with_timestamps` принимает `np.ndarray` / `torch.Tensor` / path,
нормализует int16→float32 ровно как локальный `ASRModule.transcribe_with_timestamps`,
и возвращает `Word.start/end` в **int ms** — байт-совместимо с локальным контрактом.

### D4. `FallbackASR` — ленивый local fallback + circuit breaker

Обёртка вокруг `RemoteASRClient`, делегирует методы remote'у и держит
thread-safe circuit breaker (`threading.Lock`, `time.monotonic()`).

- **Lazy LOAD, не lazy construct.** В `__init__` сразу строится
  `self._local = local_factory()` (по умолчанию `ASRModule`). `ASRModule.__init__`
  дёшев — модель НЕ грузится. `self._local.load_model()` НЕ вызывается до первого
  фактического fallback'а. Это даёт remote-режиму нулевую цену по VRAM/RAM на
  локали, пока remote жив.
- **Делегирование к eager-local'у.** Свойства `model`, `device`, объект
  `_model_lock` (тот же самый — `RefineWorker` берёт его снаружи),
  `_maybe_empty_mps_cache()` проксируются на `self._local`, чтобы существующие
  callers и RefineWorker работали без изменений.
- **`using_remote`** — `True`, когда circuit closed/half-open (следующий вызов
  попробует remote).
- **`load_model(quiet)`**: если `using_remote` — зовёт `remote.healthz()` (лог
  готовности), локаль НЕ грузит, возвращает `None`. Иначе (fallback/local active)
  — `self._local.load_model(quiet)` и возвращает локальную модель.
- **`_dispatch`**: closed/half-open → пробуем `remote.<m>(...)`; на
  `RemoteASRError`/httpx-ошибке → `_record_failure()`, лог one-line mode-switch на
  первом fallback'е, `self._local.load_model(quiet=True)`, `local.<m>(...)`.
  Успех remote'а → `_record_success()` (reset). OPEN → сразу локаль. Half-open:
  по истечении `open_seconds` пускаем ОДИН remote-probe; успех закрывает, фейл
  переоткрывает.
- **`_record_failure`**: `failures += 1`; при `failures >= fail_threshold` →
  `opened_until = monotonic() + open_seconds`, лог `[FallbackASR] remote circuit
  OPEN for {open_seconds}s after {failures} fail(s)`.
- **Контракт ошибок:** `transcribe_chunk` возвращает `None` при полном фейле (оба
  пути упали) — как существующий `_transcribe_direct`. `transcribe_batch` /
  `transcribe_simple` / `transcribe_with_timestamps` ре-raise'ят ЛОКАЛЬНОЕ
  исключение, если упала и локаль. Fallback-with-both-sides-failed никогда не
  глотается молча.

### D5. Gateway расширяется в этом репозитории под word-timestamps

Патч текущего backend'а GPU-сервера лежит в `recog/asr_server/gigaam_backend.py` (один
репозиторий — отдельного asr-api нет). Добавляется опциональное form-поле
`timestamp_granularities` (string; `"word"` включает word-timestamps). Когда
запрошены word ts — backend зовёт `state["model"].transcribe(wav_path,
word_timestamps=True)` (вернёт `TranscriptionResult` с `.text` и
`.words[Word(text, start_sec, end_sec)]`) вместо `transcribe_longform` и кладёт в
JSON top-level `"words": [{"word", "start", "end"}]` рядом с `text`. Без флага —
всё текущее поведение (json/verbose_json/diarized/srt/vtt, ffmpeg transcode)
нетронуто. Deploy: `docs/asr-server.md` — scp в `~/projects/asr-api/
gigaam_backend.py` на GPU-сервере, `systemctl --user restart asr-gigaam`, curl
smoke-test на word ts.

### D6. Safety default — opt-in

Committed `RECOG_ASR_BACKEND="local"`: репозиторий ведёт себя ровно как до
изменения, пока пользователь не выставит `RECOG_ASR_BACKEND="remote"` в `.env`.
Env-консты в `recog/config.py` (паттерн `os.environ.get(...)` на верхнем уровне,
как `MIC_SPEAKER_NAME` / `PCM_*`):

| env | default | смысл |
|---|---|---|
| `RECOG_ASR_BACKEND` | `local` | `local` \| `remote` |
| `RECOG_ASR_REMOTE_URL` | `""` | база gateway; при `remote` пустое значение — `RuntimeError` на старте (дефолт убран в ADR-0021, адрес задаётся в `.env`) |
| `RECOG_ASR_REMOTE_MODEL` | `gigaam` | алиас модели |
| `RECOG_ASR_REMOTE_TIMEOUT_S` | `15` | per-request timeout |
| `RECOG_ASR_CIRCUIT_FAILS` | `3` | порог открытия circuit |
| `RECOG_ASR_CIRCUIT_OPEN_S` | `60` | окно half-open пробы |

## Implementation

### Ключевые файлы

- `recog/remote_asr.py` (новый) — `RemoteASRError`, `RemoteASRClient`,
  `FallbackASR`, `build_asr()`.
- `recog/asr.py` — новый `ASRModule.transcribe_chunk` (извлечён из
  `_transcribe_direct`); `_model_lock` остаётся RLock'ом (refine берёт снаружи).
- `recog/config.py` — `RECOG_ASR_*` консты.
- `recog/live.py` — `self.asr = build_asr()`; `_transcribe_direct` зовёт
  `self.asr.transcribe_chunk`; `_refine_transcribe_fn` гейтит decoder
  monkey-patch на `getattr(self.asr, "using_remote", False)` (в remote-режиме
  ни load_model, ни patch — сразу `transcribe_with_timestamps`).
- `recog/cli.py` — offline-транскрипция через `build_asr()`.
- `recog/__init__.py` — экспорт `build_asr`, `RemoteASRClient`, `FallbackASR`.
- `recog/asr_server/gigaam_backend.py` + `docs/asr-server.md` — патч gateway.

## Consequences

### Positive

- ~99× RTF на Blackwell vs ~60× локально: live-batch, refine и offline-CLI
  снимаются с MacBook'а, освобождая локальный GPU/CPU под UI/браузер/расширение.
- Refine offload'ится наравне с остальными — gateway расширен под word-timestamps
  в том же репозитории, контракт `Word` в int ms сохранён.
- Нулевая локальная цена в remote-режиме: модель не грузится, пока remote жив
  (lazy load).
- Атомарный rollback и safe-by-default: committed `RECOG_ASR_BACKEND=local`,
  включение — одна env-строка; circuit breaker автоматически возвращает на локаль
  при сбое remote'а без падения встречи.
- Downstream не трогается: один `build_asr()`, байт-совместимые структуры,
  `_model_lock` тот же объект — RefineWorker работает как раньше.

### Negative / Risks

- **Сетевая зависимость live-пути.** Каждый live-batch теперь может уйти по сети.
  LAN 3–5 ms делает это пренебрежимым, но WAN/DERP-фоллбэк Tailscale (другая
  страна) убил бы latency. Митигация: per-request timeout (`RECOG_ASR_REMOTE_
  TIMEOUT_S=15`) + circuit breaker; пользователь сам решает включать только в LAN.
- **First-paint при холодном fallback'е.** Первый фактический fallback тянет
  `load_model()` на локали (~секунды). Это происходит один раз на инцидент;
  circuit breaker не молотит remote повторно. Тёплый локальный путь после этого.
- **Контрактный дрейф gateway.** Word-timestamps добавлены патчем нашего же
  backend'а; если на GPU-сервере выкатят несовместимую версию, `transcribe_with_
  timestamps` упадёт → fallback на локаль. Smoke-test в `docs/asr-server.md`
  обязателен после деплоя.
- **Батч теряет batched-forward выигрыш.** `transcribe_batch` на remote'е —
  ThreadPoolExecutor из независимых `transcribe_chunk` (gateway не принимает
  батч), а не один padded forward. На 99× RTF + LAN это терпимо; любой item-raise
  фоллбэкает весь батч локально (батч-форвард сохраняется на локали).
- **WAV-сериализация в памяти** добавляет CPU на сторону MacBook'а per chunk
  (soundfile encode). Для 3-s 16 kHz клипов пренебрежимо.

### Mitigation

- Circuit breaker (`fail_threshold=3`, `open_seconds=60`) + half-open probe:
  быстрый отказ от мёртвого remote'а, периодический re-probe на восстановление.
- `healthz()` на `load_model()` в remote-режиме логирует готовность gateway до
  встречи.
- `transcribe_chunk` → `None`, остальные методы ре-raise'ят локальное исключение:
  никаких тихих потерь.
- Default `local` + opt-in: до флипа поведение репозитория не меняется.

## Alternatives Considered

1. **Не выносить refine (только offline + batch).** Отклонено: refine — заметная
   доля inference-load'а и главный конкурент live за mutex; оставить его на
   локали = половина выигрыша. Цена — расширить gateway под word ts — оплачена
   один раз патчем в `recog/asr_server/`.
2. **Eager local load в remote-режиме (страховка).** Отклонено: держать 1.6 GB
   модель загруженной «на всякий» убивает смысл offload'а. Lazy load + circuit
   breaker дают ту же надёжность без постоянной цены.
3. **Отдельный репозиторий asr-api для gateway-патча.** Отклонено (user decision):
   один репозиторий, deploy-артефакт в `recog/asr_server/`, чтобы версия gateway и
   клиент жили вместе и не расходились.
4. **gRPC / бинарный протокол вместо OpenAI-Whisper multipart.** Отклонено:
   gateway уже OpenAI-Whisper-совместим и используется другими инструментами
   (MacWhisper custom provider, см. `local-linux-models`). Переиспользуем готовый
   контракт; WAV-multipart на LAN дёшев.
5. **Async-клиент (aiohttp/httpx.AsyncClient).** Отклонено: live/refine пути
   синхронны под `_model_lock`; sync `httpx.Client` + ThreadPoolExecutor для
   батча проще и не вводит event-loop в worker-потоки.
6. **Default `remote`.** Отклонено (safety): сетевой/GPU-сбой не должен менять
   поведение из коробки. Default `local`, offload — осознанный opt-in.

## References

- **OpenSpec change** (детальный дизайн): `openspec/changes/remote-asr-offload/`
  — `proposal.md`, `design.md`, `tasks.md`.
- Gateway: `https://<gpu-host>/asr`
  (`GET /healthz`, `GET /v1/models`, `POST /v1/audio/transcriptions`).
- Deploy-артефакт: `recog/asr_server/gigaam_backend.py`, `docs/asr-server.md`.
- Skill `local-linux-models` — GPU-сервер (RTX PRO 6000 Blackwell), ASR gateway,
  модельные алиасы.
- Prior ADRs: ADR-0018 (per-speaker ASR pipeline — `transcribe_batch`,
  `_transcribe_direct`), ADR-0019 (stream-refine — `transcribe_with_timestamps`,
  shared `_model_lock`, MPS autoreleasepool).
