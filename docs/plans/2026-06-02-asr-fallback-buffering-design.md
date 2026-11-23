# ASR Fallback Buffering — Design

## Summary

Сбой remote ASR (GPU-сервер) не должен терять речь. Аудио буферизуется в памяти, пока ASR
недоступен (remote упал, локальная GigaAM ещё грузится), локальная модель грузится в
фоновом потоке, бэклог обрабатывается после восстановления с корректными временными
метками (время произнесения, не обработки). Дедупликация ограничивается временным
окном. Все проглатываемые исключения логируются.

## Контекст — инцидент 2026-06-02 10:10

Хронология (восстановлена при отладке):

1. 10:10:54–55 — две реплики транскрибированы через remote (GPU-сервер) успешно
2. ~10:10:56 — Tailscale-путь Mac↔GPU-сервер стал нестабильным (conn-track timeouts в
   логах tailscaled на GPU-сервере; машина была перезагружена в 08:42) → remote-запросы
   начали падать
3. Каскад блокировок main thread: 3 × 15s timeout + ~30–60s загрузка локальной GigaAM
   на MPS = **~1.5–2.5 минуты полного простоя пайплайна**
4. Речь пользователя в этом окне копилась в capture-буфере, но после восстановления
   повторённая тестовая фраза полностью совпала с `state.last_words` последнего
   сегмента → `deduplicate()` вернула `None` → **молча выброшена**
5. Ошибки проглочены на трёх уровнях (`except Exception: return None`) → ноль следов
   в логах

Корневые причины потери реплик:

| # | Причина | Фикс (секция) |
|---|---------|---------------|
| 1 | Загрузка локальной модели блокирует main loop на 30–60s | §1 фоновая загрузка |
| 2 | Remote timeout 15s × 3 блокирует main loop | §1 отдельный live-timeout 5s |
| 3 | Чанк, на котором remote упал, теряется | §2 requeue |
| 4 | Per-speaker батч теряется при исключении ASR (drain деструктивен) | §3 drain gate |
| 5 | Дедуп ест реально повторённые фразы (без ограничения по времени) | §4 дедуп-окно |
| 6 | Сегменты бэклога получают время обработки, а не произнесения | §2 capture timestamps |
| 7 | Исключения ASR молча проглатываются | §5 логирование |

## Requirements

1. Сбой GPU-сервера (любой длительности) не теряет ни одного фрагмента речи
2. Аудио-буфер в памяти (RAM, ~8 MB/мин); переживает сбой remote и загрузку локальной
   модели; кап 10 минут
3. Локальная модель грузится лениво (как сейчас) — но в фоне, не блокируя пайплайн
4. После загрузки локальной модели бэклог обрабатывается с временными метками
   произнесения
5. Повторённые через паузу фразы не дедуплицируются
6. Любой сбой ASR оставляет след в stderr (rate-limited)
7. Для `RECOG_ASR_BACKEND=local` ASR-диспатч не меняется (нет circuit breaker /
   фоновой загрузки); дедуп-окно, capture-метки и логирование применяются к обоим
   бэкендам

## Design

### D-таблица решений

| # | Решение | Альтернативы | Почему |
|---|---------|--------------|--------|
| D1 | Буфер в памяти (существующий capture-буфер) | disk spool | Рестарт процесса теряет live-сессию в любом случае; диск — лишняя сложность |
| D2 | «Не вычитывать, пока ASR не готов» | отдельная pending-очередь | Минимум нового состояния; capture-буфер уже копит аудио |
| D3 | Фоновая загрузка локальной модели (lazy сохраняется) | eager preload при старте | Сохраняет смысл remote offload — память Mac свободна, пока шлюз жив |
| D4 | `ASRTemporarilyUnavailable` — новое исключение | sentinel-значение / None | None уже означает «пустой текст»; нужен различимый сигнал «не теряй данные» |
| D5 | Live-timeout 5s, refine/offline остаются 15s | один таймаут на всё | 15s ожидания для 3s-чанка — главный источник блокировок |
| D6 | Дедуп-окно 2s (`DEDUP_MAX_GAP_S`) | убрать дедуп совсем | Дедуп нужен для 0.5s overlap соседних чанков; окно сохраняет это и убирает ложные срабатывания |
| D7 | Капы: capture-буфер 600s, retry-batch один слот | unbounded | Защита от вечной недоступности (remote мёртв И локальная загрузка упала) |

### §1. `FallbackASR` (recog/remote_asr.py) — неблокирующий fallback

Новое:

```python
class ASRTemporarilyUnavailable(Exception):
    """ASR сейчас недоступен (remote упал, локаль грузится). Caller обязан
    сохранить данные и повторить позже."""
```

Состояние фоновой загрузки:
- `_local_ready: bool` — модель загружена
- `_local_load_thread: Optional[threading.Thread]`
- `_local_load_failed_at: float` — monotonic время последнего провала загрузки
  (повторная попытка не раньше чем через `LOCAL_LOAD_RETRY_S = 300`)

Свойство `available` (для гейтов в live.py / per_speaker.py):
- `True`, когда circuit закрыт (remote presumed alive) **или** `_local_ready`

`_start_local_load()` (заменяет блокирующий `_ensure_local_loaded`):
- Идемпотентно запускает daemon-поток с `self._local.load_model(quiet=True)`
- Успех → `_local_ready = True` + лог `[FallbackASR] local GigaAM ready (fallback active)`
- Ошибка → лог traceback + `_local_load_failed_at = monotonic()`

`_run()` (новая логика диспатча):

```
try_remote, epoch = _decide()
if try_remote:
    try:
        out = remote_call(); _record_success(epoch); return out
    except (RemoteASRError, httpx.HTTPError):
        _record_failure(epoch)
        _start_local_load()
        if _local_ready: return local_call()
        raise ASRTemporarilyUnavailable
else:  # circuit открыт
    if _local_ready: return local_call()
    _start_local_load()
    raise ASRTemporarilyUnavailable
```

Таймауты: `RemoteASRClient` получает `live_timeout_s` (новый конфиг
`RECOG_ASR_LIVE_TIMEOUT_S`, default 5) — используется в `transcribe_chunk` и
`transcribe_batch`. `transcribe_simple` / `transcribe_with_timestamps` остаются на
`RECOG_ASR_REMOTE_TIMEOUT_S` (15).

Логирование: переход circuit closed→open (есть), open→closed (новое), сбои probe
(rate-limited), успех/провал фоновой загрузки.

### §2. Mic/system путь (recog/live.py) — буфер не вычитывается

`AudioCapture`:
- `_buffer_start_ts: float` — wall-clock время первого невычитанного сэмпла
  (выставляется в `_callback` при добавлении в пустой буфер, корректируется в
  `get_chunk` при вычитке)
- `get_chunk(duration)` → `Optional[Tuple[np.ndarray, float]]` — чанк + время захвата
  его первого сэмпла
- `requeue(chunk, capture_start_ts)` — вернуть чанк в начало буфера (вызывается, когда
  ASR кинул `ASRTemporarilyUnavailable` на уже вычитанном чанке)
- Кап буфера: `ASR_PENDING_BUFFER_CAP_S = 600` — при превышении старейшее аудио
  отбрасывается + громкий лог (раз в 30s)

`LiveTranscriber._process_chunk`:
- В начале: `if not self._asr_available(): return` — аудио остаётся в буфере.
  `_asr_available()` → `getattr(self.asr, "available", True)` (plain `ASRModule` всегда
  доступен)
- `_transcribe_direct` пробрасывает `ASRTemporarilyUnavailable` (не глотает) →
  `_process_chunk` ловит → `capture.requeue(chunk, ts)` → return
- Временные метки: `chunk_start_time = capture_start_ts`,
  `chunk_end_time = capture_start_ts + chunk_duration`, отображаемый `timestamp` —
  из capture-времени. `_check_silence_split` и `_handle_*_chunk` получают эти же
  значения (сегменты бэклога ложатся в прошлое, где была речь)

### §3. Per-speaker путь (recog/per_speaker.py) — drain gate

`PerSpeakerIngest.__init__` получает `asr_available_getter: Callable[[], bool]`
(по аналогии с `_generation_getter`; для plain ASRModule — `lambda: True`).

`tick()`:
- Если `not asr_available_getter()` → пропустить `_drain_batch_locked()` → PCM копится
  в существующих per-pid буферах (их капы уже есть — `_enforce_ring_capacity_locked`)
- Гонка первого сбоя (available стал False между drain и transcribe): callback кидает
  `ASRTemporarilyUnavailable` → батч сохраняется в однослотовый
  `_pending_retry_batch` → диспатчится первым при следующем tick с доступным ASR.
  Один слот достаточен: после первого unavailable гейт закрывается и новые батчи не
  выгребаются

`RefineWorker` (recog/refine.py): `ASRTemporarilyUnavailable` обрабатывается как abort
→ задача возвращается в очередь.

### §4. Дедуп-фикс (recog/live.py)

- `SourceState.last_words_capture_end: float = 0.0` — capture-время конца чанка
  последнего записанного сегмента
- Перед `deduplicate()`: если `chunk_capture_start - state.last_words_capture_end >
  DEDUP_MAX_GAP_S` (конфиг, default 2.0) → `state.last_words = []` (дедуп не
  применяется)
- Дедуп существует для 0.5s audio overlap соседних чанков — окно 2s покрывает overlap
  + джиттер обработки и исключает подавление реально повторённых фраз

### §5. Логирование (recog/asr.py, recog/live.py)

- Хелпер `log_rate_limited(key: str, msg: str, interval_s: float = 30.0)` — первое
  сообщение сразу, дальше раз в interval с счётчиком подавленных
- `ASRModule.transcribe_chunk`: `except Exception as e` → лог + `return None`
  (контракт сохраняется)
- `LiveTranscriber._transcribe_direct`: `except ASRTemporarilyUnavailable: raise`;
  `except Exception` → лог с traceback + `return None`

## Тесты

| Область | Сценарии |
|---------|----------|
| FallbackASR | фоновая загрузка стартует на первом сбое; `available` для всех комбинаций circuit/local; `ASRTemporarilyUnavailable` когда локаль не готова; retry загрузки после провала не раньше 300s; live-timeout vs refine-timeout |
| AudioCapture | capture-время чанков; requeue сохраняет порядок; кап буфера отбрасывает старейшее |
| _process_chunk | ASR недоступен → буфер не вычитывается; восстановление → бэклог с capture-метками |
| Дедуп | смежные чанки дедуплицируются; разрыв > 2s — нет |
| PerSpeakerIngest | drain gate; retry batch после восстановления |
| Существующие | `python -m pytest tests/ -v` зелёный |

## Вне скоупа

- Полностью асинхронный ASR-пайплайн (вариант C из обсуждения)
- Disk spool буфера
- Изменение ASR-диспатча для `RECOG_ASR_BACKEND=local`
- Доработки на стороне gateway GPU-сервера
