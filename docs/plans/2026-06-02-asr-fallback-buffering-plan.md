# ASR Fallback Buffering — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Сбой remote ASR (Z270XX) не теряет речь — аудио буферизуется, локальная GigaAM грузится в фоне, бэклог обрабатывается с capture-временем, дедуп ограничен окном, ошибки логируются.

**Architecture:** `FallbackASR` перестаёт блокировать вызывающий поток (фоновая загрузка локали + `ASRTemporarilyUnavailable`); потребители (`_process_chunk`, `PerSpeakerIngest.tick`, `RefineWorker`) гейтятся на `asr.available` и не вычитывают свои буферы, пока ASR недоступен.

**Tech Stack:** Python 3.12, pytest, threading, httpx, numpy. Дизайн: `docs/plans/2026-06-02-asr-fallback-buffering-design.md`.

**Запуск тестов:** `./venv-gigaam/bin/python -m pytest tests/<file> -v` (venv обязателен — torch/httpx/numpy оттуда).

---

## File Structure

| Файл | Роль |
|------|------|
| `recog/log_utils.py` (create) | Rate-limited stderr-логгер для горячих путей |
| `recog/config.py` (modify) | +`RECOG_ASR_LIVE_TIMEOUT_S`, `DEDUP_MAX_GAP_S`, `ASR_PENDING_BUFFER_CAP_S` |
| `recog/remote_asr.py` (modify) | +`ASRTemporarilyUnavailable`, фоновая загрузка, `available`, live-timeout, лог circuit close |
| `recog/asr.py` (modify) | Лог проглатываемых исключений в `transcribe_chunk` |
| `recog/live.py` (modify) | `AudioCapture` capture-время/requeue/кап; `_process_chunk` гейт + capture-метки + дедуп-окно; `_transcribe_direct` re-raise; `_on_pcm_batch` re-raise; wiring `PerSpeakerIngest` |
| `recog/per_speaker.py` (modify) | Drain gate + `_pending_retry_batch` |
| `recog/refine.py` (modify) | `ASRTemporarilyUnavailable` → re-enqueue с задержкой |
| `tests/test_log_utils.py` (create) | Тесты логгера |
| `tests/test_remote_asr.py` (modify) | Новые тесты + обновление 5 существующих под async-fallback |
| `tests/test_live_buffering.py` (create) | AudioCapture + гейт `_process_chunk` + дедуп-окно |
| `tests/test_per_speaker.py` (modify) | Drain gate + retry batch |
| `tests/test_refine_fallback.py` (modify) | Unavailable → re-enqueue |
| `tests/test_live_speaker_attribution.py` (modify) | Обновить моки `get_chunk` (кортеж) |
| `tests/test_config.py` (modify) | Новые константы |

---

### Task 1: Rate-limited логгер

**Files:**
- Create: `recog/log_utils.py`
- Test: `tests/test_log_utils.py`

- [x] **Step 1.1: Написать падающий тест**

```python
# tests/test_log_utils.py
"""Тесты rate-limited stderr-логгера (recog/log_utils.py)."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.log_utils import log_rate_limited, _reset_for_tests


def test_first_message_emitted_immediately(capsys):
    """given: чистое состояние логгера
    when: log_rate_limited вызывается один раз
    then: сообщение сразу попадает в stderr"""
    _reset_for_tests()
    log_rate_limited("key-a", "boom", interval_s=30.0)
    assert "boom" in capsys.readouterr().err


def test_repeat_within_interval_suppressed_and_counted(capsys):
    """given: сообщение с key-b уже было залогировано
    when: то же key логируется повторно внутри interval, потом по истечении
    then: повторы подавлены; следующая эмиссия содержит счётчик подавленных"""
    _reset_for_tests()
    fake_now = [1000.0]
    log_rate_limited("key-b", "первое", interval_s=30.0, _now_fn=lambda: fake_now[0])
    fake_now[0] += 1.0
    log_rate_limited("key-b", "второе", interval_s=30.0, _now_fn=lambda: fake_now[0])
    fake_now[0] += 1.0
    log_rate_limited("key-b", "третье", interval_s=30.0, _now_fn=lambda: fake_now[0])
    err = capsys.readouterr().err
    assert "первое" in err
    assert "второе" not in err and "третье" not in err

    fake_now[0] += 30.0
    log_rate_limited("key-b", "четвёртое", interval_s=30.0, _now_fn=lambda: fake_now[0])
    err = capsys.readouterr().err
    assert "четвёртое" in err
    assert "+2 suppressed" in err


def test_different_keys_do_not_interfere(capsys):
    """given: key-c уже залогирован
    when: логируется key-d
    then: key-d эмитится сразу (отдельный счётчик)"""
    _reset_for_tests()
    log_rate_limited("key-c", "c-msg", interval_s=30.0)
    log_rate_limited("key-d", "d-msg", interval_s=30.0)
    err = capsys.readouterr().err
    assert "c-msg" in err and "d-msg" in err
```

- [x] **Step 1.2: Убедиться, что тест падает**

Run: `./venv-gigaam/bin/python -m pytest tests/test_log_utils.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'recog.log_utils'`

- [x] **Step 1.3: Реализация**

```python
# recog/log_utils.py
"""Rate-limited stderr-логирование для горячих путей (live ASR loop).

Контракт: первое сообщение по ключу эмитится сразу; повторы внутри interval_s
подавляются и считаются; следующая эмиссия добавляет "(+N suppressed)".
"""

import sys
import threading
import time
from typing import Callable, Dict, Tuple

_lock = threading.Lock()
# key -> (monotonic время последней эмиссии, счётчик подавленных с тех пор)
_state: Dict[str, Tuple[float, int]] = {}


def log_rate_limited(
    key: str,
    msg: str,
    interval_s: float = 30.0,
    _now_fn: Callable[[], float] = time.monotonic,
) -> None:
    """Печатает msg в stderr не чаще раза в interval_s на каждый key."""
    now = _now_fn()
    with _lock:
        last, suppressed = _state.get(key, (None, 0))
        if last is not None and now - last < interval_s:
            _state[key] = (last, suppressed + 1)
            return
        _state[key] = (now, 0)
    suffix = f" (+{suppressed} suppressed)" if suppressed else ""
    print(f"{msg}{suffix}", file=sys.stderr)


def _reset_for_tests() -> None:
    """Сбрасывает состояние (только для unit-тестов)."""
    with _lock:
        _state.clear()
```

- [x] **Step 1.4: Тесты зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_log_utils.py -v`
Expected: 3 passed

- [x] **Step 1.5: Commit**

```bash
git add recog/log_utils.py tests/test_log_utils.py
git commit -m "feat(asr): add rate-limited stderr logger for hot paths" -- recog/log_utils.py tests/test_log_utils.py
```

---

### Task 2: Конфиг-константы

**Files:**
- Modify: `recog/config.py` (после строки 154, конец секции REMOTE ASR OFFLOAD)
- Test: `tests/test_config.py`

- [x] **Step 2.1: Падающий тест**

Добавить в конец `tests/test_config.py`:

```python
class TestAsrFallbackBufferingConfig:
    """Константы из docs/plans/2026-06-02-asr-fallback-buffering-design.md."""

    def test_live_timeout_default_is_5s(self):
        from recog import config
        assert config.RECOG_ASR_LIVE_TIMEOUT_S == 5.0

    def test_dedup_max_gap_default_is_2s(self):
        from recog import config
        assert config.DEDUP_MAX_GAP_S == 2.0

    def test_pending_buffer_cap_default_is_600s(self):
        from recog import config
        assert config.ASR_PENDING_BUFFER_CAP_S == 600.0
```

- [x] **Step 2.2: Тест падает**

Run: `./venv-gigaam/bin/python -m pytest tests/test_config.py -v -k Buffering`
Expected: FAIL — `AttributeError: module 'recog.config' has no attribute 'RECOG_ASR_LIVE_TIMEOUT_S'`

- [x] **Step 2.3: Добавить константы в `recog/config.py`** (после строки 154 `RECOG_ASR_CIRCUIT_OPEN_S = ...`):

```python

# ============================================================================
# ASR FALLBACK BUFFERING (docs/plans/2026-06-02-asr-fallback-buffering-design.md)
# ============================================================================

# Таймаут remote-запроса для live-путей (transcribe_chunk / transcribe_batch).
# Короче RECOG_ASR_REMOTE_TIMEOUT_S: 3-секундный live-чанк не должен блокировать
# пайплайн на 15 s, когда шлюз лежит. Refine/offline остаются на длинном таймауте.
RECOG_ASR_LIVE_TIMEOUT_S = float(os.environ.get("RECOG_ASR_LIVE_TIMEOUT_S", "5"))

# Окно дедупликации live-сегментов: last_words применяются только если разрыв
# (по capture-времени) между концом предыдущего сегмента и началом нового чанка
# меньше этого значения. Дедуп существует для 0.5 s audio-overlap соседних
# чанков; за пределами окна повторённая фраза — это реальная речь.
DEDUP_MAX_GAP_S = float(os.environ.get("DEDUP_MAX_GAP_S", "2.0"))

# Кап аудио-буфера AudioCapture, пока ASR недоступен (секунды). При превышении
# старейшее аудио отбрасывается с rate-limited логом.
ASR_PENDING_BUFFER_CAP_S = float(os.environ.get("ASR_PENDING_BUFFER_CAP_S", "600"))
```

- [x] **Step 2.4: Тесты зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_config.py -v`
Expected: все passed (старые + 3 новых)

- [x] **Step 2.5: Commit**

```bash
git add recog/config.py tests/test_config.py
git commit -m "feat(asr): add live-timeout, dedup-window and buffer-cap config" -- recog/config.py tests/test_config.py
```

---

### Task 3: FallbackASR — неблокирующий fallback

**Files:**
- Modify: `recog/remote_asr.py`
- Test: `tests/test_remote_asr.py`

- [x] **Step 3.1: Новые падающие тесты**

Добавить в конец `tests/test_remote_asr.py`:

```python
# -----------------------------------------------------------------------------
# Non-blocking fallback (docs/plans/2026-06-02-asr-fallback-buffering-design.md)
# -----------------------------------------------------------------------------

from recog.remote_asr import ASRTemporarilyUnavailable


def test_first_remote_failure_raises_unavailable_and_starts_background_load():
    """given: remote падает, локальная модель не загружена
    when: transcribe_chunk
    then: ASRTemporarilyUnavailable + фоновый поток загрузки запущен
          (вызывающий поток НЕ блокируется на load_model)"""
    fb, remote, local = _make_fallback()
    remote.fail_next = 1
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)

    assert fb.wait_local_ready(timeout=2.0) is True
    assert local.calls["load_model"] == 1


def test_after_background_load_local_serves_calls():
    """given: circuit открыт, фоновая загрузка завершена
    when: transcribe_chunk
    then: LOCAL_CHUNK без обращения к remote"""
    fb, remote, local = _make_fallback(fail_threshold=1)
    remote.fail_next = 1
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)  # сбой + открытие circuit + старт загрузки
    assert fb.wait_local_ready(timeout=2.0)

    remote_calls_before = remote.calls["transcribe_chunk"]
    assert fb.transcribe_chunk(audio) == "LOCAL_CHUNK"
    assert remote.calls["transcribe_chunk"] == remote_calls_before  # remote не трогали


def test_available_false_while_circuit_open_and_local_cold():
    """given: circuit открыт, локаль не готова
    then: available == False (потребители не вычитывают буферы)"""
    fb, remote, local = _make_fallback(fail_threshold=1, open_seconds=60.0)
    # Блокируем фоновую загрузку, чтобы детерминированно поймать "не готова"
    load_started = threading.Event()
    load_release = threading.Event()

    real_load = local.load_model
    def slow_load(quiet=False):
        load_started.set()
        load_release.wait(timeout=5.0)
        return real_load(quiet)
    local.load_model = slow_load

    remote.fail_next = 1
    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(np.zeros(SAMPLE_RATE, dtype=np.float32))

    assert load_started.wait(timeout=2.0)
    assert fb.available is False  # circuit открыт И локаль ещё грузится
    load_release.set()
    assert fb.wait_local_ready(timeout=2.0)
    assert fb.available is True  # локаль готова


def test_available_true_when_circuit_closed():
    """given: свежий FallbackASR (circuit закрыт, локаль холодная)
    then: available == True (remote presumed alive)"""
    fb, _, _ = _make_fallback()
    assert fb.available is True


def test_failed_local_load_logged_and_not_retried_within_backoff(capsys):
    """given: load_model локали кидает исключение
    when: повторный сбой remote внутри окна LOCAL_LOAD_RETRY_S
    then: загрузка НЕ перезапускается (backoff), ошибка залогирована"""
    fb, remote, local = _make_fallback(fail_threshold=1)
    def broken_load(quiet=False):
        raise RuntimeError("MPS boom")
    local.load_model = broken_load

    remote.fail_next = 10
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)
    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    # Дождаться завершения (упавшего) потока загрузки
    fb._local_load_thread.join(timeout=2.0)
    assert "load FAILED" in capsys.readouterr().err

    threads_before = fb._local_load_thread
    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    assert fb._local_load_thread is threads_before  # новый поток не создан


def test_circuit_close_logged_on_remote_recovery(capsys, monkeypatch):
    """given: circuit открыт после сбоев
    when: окно истекло и probe удался
    then: '[FallbackASR] remote recovered' в stderr"""
    fb, remote, local = _make_fallback(fail_threshold=1, open_seconds=60.0)
    fake_now = [1000.0]
    monkeypatch.setattr("recog.remote_asr.time.monotonic", lambda: fake_now[0])

    remote.fail_next = 1
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)
    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    fb.wait_local_ready(timeout=2.0)
    capsys.readouterr()

    fake_now[0] += 61.0  # окно истекло -> probe
    assert fb.transcribe_chunk(audio) == "REMOTE_CHUNK"
    assert "remote recovered" in capsys.readouterr().err


def test_live_timeout_passed_to_chunk_requests(monkeypatch):
    """given: RemoteASRClient с live_timeout_s=5
    when: transcribe_chunk
    then: POST вызывается с timeout=5 (а не дефолтным таймаутом клиента)"""
    captured = {}

    class _SpyClient:
        def post(self, url, files=None, data=None, timeout=None):
            captured["timeout"] = timeout
            return _FakeResponse(json_data={"text": "ok"})
        def get(self, url):
            raise AssertionError("not used")

    client = RemoteASRClient(
        base_url="https://example.test/asr", model="gigaam",
        timeout_s=15.0, live_timeout_s=5.0,
    )
    client._client = _SpyClient()
    client.transcribe_chunk(np.zeros(SAMPLE_RATE, dtype=np.float32))
    assert captured["timeout"] == 5.0
```

Также добавить `import threading` в начало файла, если его там нет (проверить — `grep -n "^import threading" tests/test_remote_asr.py`; он уже есть, т.к. FakeLocal использует `threading.RLock`).

Примечание: `_FakeResponse` уже определён в файле (строка ~195) — проверить его конструктор (`grep -n "class _FakeResponse" -A 15 tests/test_remote_asr.py`) и подстроить вызов `_FakeResponse(json_data={"text": "ok"})` под его реальную сигнатуру.

- [x] **Step 3.2: Тесты падают**

Run: `./venv-gigaam/bin/python -m pytest tests/test_remote_asr.py -v -k "unavailable or available or background or recovery or live_timeout or backoff"`
Expected: FAIL — `ImportError: cannot import name 'ASRTemporarilyUnavailable'`

- [x] **Step 3.3: Реализация в `recog/remote_asr.py`**

3.3.1. Добавить импорты в шапку (после `import time`):

```python
import traceback
```

и после существующих импортов из `.asr`:

```python
from .log_utils import log_rate_limited
```

3.3.2. После класса `RemoteASRError` добавить:

```python
class ASRTemporarilyUnavailable(Exception):
    """ASR прямо сейчас не может обслужить запрос: remote упал/недоступен, а
    локальная модель ещё грузится (или её загрузка недавно упала).

    Контракт для caller'а: данные НЕ потеряны — сохрани их (requeue в свой
    буфер) и повтори позже. Поднимается только из FallbackASR; чистый
    ASRModule его никогда не кидает.
    """
    pass
```

3.3.3. `RemoteASRClient.__init__` — добавить `live_timeout_s`:

```python
    def __init__(
        self,
        base_url: str,
        model: str = "gigaam",
        timeout_s: float = 15.0,
        max_workers: int = 8,
        live_timeout_s: Optional[float] = None,
    ):
        self.base_url = base_url.rstrip("/")
        self.model = model
        self.timeout_s = timeout_s
        # Live-пути (chunk/batch) ждут меньше: 3 s чанк не должен висеть 15 s.
        self.live_timeout_s = live_timeout_s if live_timeout_s is not None else timeout_s
        self.max_workers = max_workers
        self._client = httpx.Client(timeout=timeout_s)
```

3.3.4. `RemoteASRClient.transcribe_chunk` — пробросить live-таймаут в POST (заменить вызов `self._client.post(...)`):

```python
            resp = self._client.post(
                f"{self.base_url}/v1/audio/transcriptions",
                files=files,
                data=data,
                timeout=self.live_timeout_s,
            )
```

(`transcribe_batch` наследует через `pool.map(self.transcribe_chunk, ...)`; `transcribe_simple` / `transcribe_with_timestamps` НЕ трогаем — остаются на клиентском таймауте 15 s.)

3.3.5. `FallbackASR.__init__` — добавить состояние фоновой загрузки (после `self._announced_fallback = False`):

```python
        # --- Фоновая загрузка локальной модели (non-blocking fallback) ------
        # Дизайн: docs/plans/2026-06-02-asr-fallback-buffering-design.md §1.
        self._load_lock = threading.Lock()
        self._local_load_thread: Optional[threading.Thread] = None
        self._local_ready = False
        self._local_load_failed_at = 0.0  # monotonic; 0 => не падала
```

И класс-константу (рядом с объявлением класса, после docstring):

```python
    # Повторная попытка фоновой загрузки локали после провала — не раньше чем
    # через это число секунд (защита от спама загрузками при битой модели).
    LOCAL_LOAD_RETRY_S = 300.0
```

3.3.6. Добавить свойство `available` и метод `wait_local_ready` (после свойства `using_remote`):

```python
    @property
    def available(self) -> bool:
        """True, когда запрос можно обслужить прямо сейчас.

        Либо remote presumed alive (circuit закрыт или его окно истекло —
        можно пробовать probe), либо локальная модель уже загружена.
        Потребители (live loop, PerSpeakerIngest) гейтят на этом флаге
        вычитку своих буферов — False означает «не трогай буфер, копи».
        """
        if self._local_ready:
            return True
        with self._cb_lock:
            return self._opened_until == 0.0 or time.monotonic() >= self._opened_until

    def wait_local_ready(self, timeout: Optional[float] = None) -> bool:
        """Блокирующее ожидание завершения фоновой загрузки (тесты/диагностика)."""
        t = self._local_load_thread
        if t is not None:
            t.join(timeout)
        return self._local_ready
```

3.3.7. Заменить `_ensure_local_loaded` на `_start_local_load` + `_load_local_weights`:

```python
    def _start_local_load(self) -> None:
        """Идемпотентно запускает фоновую загрузку локальных весов.

        Никогда не блокирует вызывающий поток (загрузка GigaAM на MPS — это
        30–60 s; до фикса 2026-06-02 она выполнялась синхронно в live loop и
        замораживала пайплайн). Повторная попытка после провала — не раньше
        чем через LOCAL_LOAD_RETRY_S.
        """
        with self._load_lock:
            if self._local_ready:
                return
            if self._local_load_thread is not None and self._local_load_thread.is_alive():
                return
            if (
                self._local_load_failed_at > 0.0
                and time.monotonic() - self._local_load_failed_at < self.LOCAL_LOAD_RETRY_S
            ):
                return
            if not self._announced_fallback:
                print(
                    "[FallbackASR] remote failed — loading local GigaAM in background",
                    file=sys.stderr,
                )
                self._announced_fallback = True
            self._local_load_thread = threading.Thread(
                target=self._load_local_weights,
                daemon=True,
                name="fallback-asr-local-load",
            )
            self._local_load_thread.start()

    def _load_local_weights(self) -> None:
        """Тело фонового потока: грузит веса, выставляет _local_ready."""
        try:
            self._local.load_model(quiet=True)
        except Exception as exc:
            with self._load_lock:
                self._local_load_failed_at = time.monotonic()
            print(
                f"[FallbackASR] local GigaAM load FAILED: {type(exc).__name__}: {exc}",
                file=sys.stderr,
            )
            traceback.print_exc()
            return
        self._local_ready = True
        print("[FallbackASR] local GigaAM ready — fallback active", file=sys.stderr)
```

3.3.8. `_record_success` — лог восстановления:

```python
    def _record_success(self, epoch: int) -> None:
        was_open = False
        with self._cb_lock:
            if epoch != self._epoch:
                return  # stale: circuit re-opened after we started — не отменяем
            was_open = self._opened_until != 0.0
            self._failures = 0
            self._opened_until = 0.0
            self._announced_fallback = False
        if was_open:
            print("[FallbackASR] remote recovered — circuit CLOSED", file=sys.stderr)
```

3.3.9. `_run` — новая логика диспатча (полная замена тела):

```python
    def _run(self, remote_call: Callable[[], Any], local_call: Callable[[], Any]) -> Any:
        """Общий dispatch: remote под breaker'ом; локаль — только если уже загружена.

        Если обслужить некому (remote упал/circuit открыт И локаль не готова) —
        поднимает ASRTemporarilyUnavailable. Caller обязан сохранить данные и
        повторить позже; ни один путь больше не блокируется на load_model.
        """
        try_remote, epoch = self._decide()
        if try_remote:
            try:
                out = remote_call()
                self._record_success(epoch)
                return out
            except (RemoteASRError, httpx.HTTPError) as exc:
                self._record_failure(epoch)
                log_rate_limited(
                    "fallback-remote-fail",
                    f"[FallbackASR] remote call failed: {type(exc).__name__}: {exc}",
                )
                self._start_local_load()
                if self._local_ready:
                    return local_call()
                raise ASRTemporarilyUnavailable(
                    f"remote failed and local model is not loaded yet: {exc}"
                ) from exc
        if self._local_ready:
            return local_call()
        self._start_local_load()
        raise ASRTemporarilyUnavailable("circuit open and local model is not loaded yet")
```

3.3.10. `build_asr()` — пробросить live-timeout:

```python
    if config.RECOG_ASR_BACKEND == "remote":
        remote = RemoteASRClient(
            config.RECOG_ASR_REMOTE_URL,
            config.RECOG_ASR_REMOTE_MODEL,
            config.RECOG_ASR_REMOTE_TIMEOUT_S,
            live_timeout_s=config.RECOG_ASR_LIVE_TIMEOUT_S,
        )
```

3.3.11. Обновить module docstring (строки 12–25): пункт «1. Ленивый локальный fallback» дополнить — «ВЕСА грузятся в ФОНОВОМ потоке при первом сбое; пока они грузятся, FallbackASR поднимает ASRTemporarilyUnavailable и caller буферизует данные».

- [x] **Step 3.4: Обновить 5 существующих тестов под async-fallback**

В `tests/test_remote_asr.py` найти и переписать (имена сохранить — меняется только given/when/then и тело):

```python
def test_remote_failure_falls_back_to_local_and_logs_mode_switch(capsys):
    """given: remote падает дважды, локаль холодная
    when: первый вызов — unavailable (старт фоновой загрузки), второй — после загрузки
    then: второй вызов отдаёт LOCAL_CHUNK; mode-switch залогирован один раз"""
    fb, remote, local = _make_fallback()
    remote.fail_next = 2
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    assert fb.wait_local_ready(timeout=2.0)

    assert fb.transcribe_chunk(audio) == "LOCAL_CHUNK"
    err = capsys.readouterr().err
    assert err.count("loading local GigaAM in background") == 1
    assert local.calls["load_model"] == 1
```

```python
def test_remote_failure_then_recovery_resets_circuit_and_returns_to_remote():
    """given: один сбой remote (unavailable), затем remote ожил
    when: следующий вызов
    then: REMOTE_CHUNK, счётчик сбоев сброшен"""
    fb, remote, local = _make_fallback(fail_threshold=3)
    remote.fail_next = 1
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    fb.wait_local_ready(timeout=2.0)

    assert fb.transcribe_chunk(audio) == "REMOTE_CHUNK"
    assert fb._failures == 0
```

```python
def test_circuit_opens_after_fail_threshold_and_routes_straight_to_local(capsys):
    """given: remote падает >= fail_threshold раз
    when: следующий вызов после фоновой загрузки локали
    then: circuit OPEN залогирован; вызов идёт в локаль без обращения к remote"""
    fb, remote, local = _make_fallback(fail_threshold=3, open_seconds=60.0)
    remote.fail_next = 10
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    for _ in range(3):
        with pytest.raises(ASRTemporarilyUnavailable):
            fb.transcribe_chunk(audio)
    assert "circuit OPEN" in capsys.readouterr().err
    assert fb.wait_local_ready(timeout=2.0)

    remote_calls_before = remote.calls["transcribe_chunk"]
    assert fb.transcribe_chunk(audio) == "LOCAL_CHUNK"
    assert remote.calls["transcribe_chunk"] == remote_calls_before
```

```python
def test_transcribe_batch_any_item_failure_falls_back_whole_batch_to_local():
    """given: remote batch падает, локаль холодная
    when: первый батч — unavailable; после загрузки — повтор
    then: повторный батч отдаёт LOCAL_* для каждого элемента"""
    fb, remote, local = _make_fallback()
    remote.fail_next = 2
    pcm_list = [np.zeros(SAMPLE_RATE, dtype=np.int16) for _ in range(3)]

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_batch(pcm_list)
    assert fb.wait_local_ready(timeout=2.0)

    out = fb.transcribe_batch(pcm_list)
    assert out == ["LOCAL_0", "LOCAL_1", "LOCAL_2"]
    assert local.calls["transcribe_batch"] == 1
```

Тесты `test_half_open_probe_closes_circuit_on_success` / `test_half_open_probe_failure_reopens_circuit` (строки 385, 423): прочитать текущие тела. Probe-успех не зависит от локали — тест должен пройти без изменений. Probe-провал: заменить ожидание `LOCAL_CHUNK` на `pytest.raises(ASRTemporarilyUnavailable)` + `fb.wait_local_ready(timeout=2.0)` + повторный вызов → `LOCAL_CHUNK` (по образцу выше). Если тест уже падал в Step 3.2 — обновить; если проходил — не трогать.

- [x] **Step 3.5: Все тесты remote_asr зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_remote_asr.py -v`
Expected: все passed (существующие + 7 новых)

- [x] **Step 3.6: Commit**

```bash
git add recog/remote_asr.py tests/test_remote_asr.py
git commit -m "feat(asr): non-blocking FallbackASR — background local load + ASRTemporarilyUnavailable

Remote failure no longer blocks the live loop on a 30-60s local model
load: the load runs in a daemon thread, and until it completes
FallbackASR raises ASRTemporarilyUnavailable so callers keep their
audio buffered instead of dropping it. Live chunk/batch requests use a
shorter timeout (RECOG_ASR_LIVE_TIMEOUT_S=5) than refine/offline (15s).
Circuit close and probe failures are now logged." -- recog/remote_asr.py tests/test_remote_asr.py
```

---

### Task 4: ASRModule — логирование проглатываемых исключений

**Files:**
- Modify: `recog/asr.py:302-303` (except в `transcribe_chunk`)
- Test: `tests/test_asr.py`

- [x] **Step 4.1: Падающий тест** (добавить в конец `tests/test_asr.py`)

```python
def test_transcribe_chunk_logs_swallowed_exception(capsys, monkeypatch):
    """given: model.forward кидает исключение внутри transcribe_chunk
    when: transcribe_chunk вызывается
    then: возвращается None (контракт прежний), но ошибка попадает в stderr"""
    from recog.asr import ASRModule
    from recog.log_utils import _reset_for_tests
    _reset_for_tests()

    asr = ASRModule.__new__(ASRModule)  # без __init__ (не грузим gigaam)
    asr.device = "cpu"
    import threading as _threading
    asr._model_lock = _threading.RLock()
    asr._mps_inference_count = 0

    class _BrokenModel:
        _device = "cpu"
        _dtype = None
        def forward(self, wav, length):
            raise RuntimeError("MPS command buffer error")

    monkeypatch.setattr(asr, "load_model", lambda quiet=False: _BrokenModel())

    import numpy as np
    result = asr.transcribe_chunk(np.zeros(16000, dtype=np.float32))

    assert result is None
    err = capsys.readouterr().err
    assert "transcribe_chunk failed" in err
    assert "MPS command buffer error" in err
```

- [x] **Step 4.2: Тест падает**

Run: `./venv-gigaam/bin/python -m pytest tests/test_asr.py -v -k swallowed`
Expected: FAIL — stderr пустой (`assert "transcribe_chunk failed" in err`)

- [x] **Step 4.3: Реализация** — в `recog/asr.py`:

Добавить импорт после `from .models import Segment`:

```python
from .log_utils import log_rate_limited
```

Заменить строки 302–303 (`except Exception:` / `return None`):

```python
        except Exception as e:
            # Контракт прежний (best-effort None), но след в логах обязателен —
            # молчаливое проглатывание скрыло инцидент 2026-06-02 (см.
            # docs/plans/2026-06-02-asr-fallback-buffering-design.md).
            log_rate_limited(
                "asr-transcribe-chunk",
                f"[ASRModule] transcribe_chunk failed: {type(e).__name__}: {e}",
            )
            return None
```

- [x] **Step 4.4: Тесты зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_asr.py -v`
Expected: все passed

- [x] **Step 4.5: Commit**

```bash
git add recog/asr.py tests/test_asr.py
git commit -m "feat(asr): log swallowed exceptions in ASRModule.transcribe_chunk" -- recog/asr.py tests/test_asr.py
```

---

### Task 5: AudioCapture — capture-время, requeue, кап буфера

**Files:**
- Modify: `recog/live.py:146-230` (класс AudioCapture; точные строки сместятся)
- Test: `tests/test_live_buffering.py` (create)

- [x] **Step 5.1: Падающие тесты**

```python
# tests/test_live_buffering.py
"""Тесты буферизации аудио при недоступном ASR.

Дизайн: docs/plans/2026-06-02-asr-fallback-buffering-design.md §2.
Класс AudioCapture конструируется без sounddevice-стрима: __init__ импортирует
sounddevice, поэтому тесты строят объект через __new__ и заполняют поля руками
(паттерн из tests/test_asr.py::_make_mocked_asr).
"""
import sys
import threading
import time
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.live import AudioCapture


SR = 16000


def _make_capture() -> AudioCapture:
    """AudioCapture без sounddevice: __new__ + ручная инициализация полей."""
    cap = AudioCapture.__new__(AudioCapture)
    cap.sd = None
    cap.sample_rate = SR
    cap.device = None
    cap.name = "test"
    cap.buffer = []
    cap.running = False
    cap.stream = None
    cap._lock = threading.Lock()
    cap._buffer_start_ts = None
    cap._buffered_samples = 0
    cap._requeued = []
    cap._overlap_samples = int(0.5 * SR)
    cap._max_buffered_samples = int(10.0 * SR)  # маленький кап для теста (10 s)
    return cap


def _feed(cap: AudioCapture, seconds: float, captured_at: float):
    """Эмулирует callback: добавляет seconds аудио, захваченного начиная с captured_at."""
    data = np.ones((int(seconds * SR), 1), dtype=np.float32) * 0.1
    cap._append_locked_for_tests(data, captured_at)


class TestCaptureTimestamps:
    def test_get_chunk_returns_capture_start_time(self):
        """given: 3 s аудио, захваченного в t=1000.0
        when: get_chunk(3.0)
        then: возвращается (chunk, 1000.0)"""
        cap = _make_capture()
        _feed(cap, 3.0, captured_at=1000.0)
        result = cap.get_chunk(3.0)
        assert result is not None
        chunk, ts = result
        assert len(chunk) == 3 * SR
        assert ts == pytest.approx(1000.0)

    def test_consecutive_chunks_advance_capture_time_minus_overlap(self):
        """given: 6 s аудио с t=1000.0
        when: два get_chunk(3.0) подряд
        then: второй чанк начинается на 2.5 s позже первого (3 s - 0.5 s overlap)"""
        cap = _make_capture()
        _feed(cap, 6.0, captured_at=1000.0)
        _, ts1 = cap.get_chunk(3.0)
        _, ts2 = cap.get_chunk(3.0)
        assert ts1 == pytest.approx(1000.0)
        assert ts2 == pytest.approx(1002.5)

    def test_not_enough_audio_returns_none(self):
        cap = _make_capture()
        _feed(cap, 1.0, captured_at=1000.0)
        assert cap.get_chunk(3.0) is None


class TestRequeue:
    def test_requeued_chunk_returned_first_with_original_timestamp(self):
        """given: чанк вычитан (ASR оказался недоступен) и возвращён requeue
        when: следующий get_chunk
        then: возвращается тот же чанк с исходным capture-временем"""
        cap = _make_capture()
        _feed(cap, 6.0, captured_at=1000.0)
        chunk1, ts1 = cap.get_chunk(3.0)

        cap.requeue(chunk1, ts1)
        chunk_again, ts_again = cap.get_chunk(3.0)

        assert ts_again == pytest.approx(ts1)
        np.testing.assert_array_equal(chunk_again, chunk1)

    def test_after_requeued_chunk_consumed_buffer_continues_in_order(self):
        """given: requeue + остаток в буфере
        when: get_chunk дважды
        then: первый — requeued (t=1000.0), второй — продолжение буфера (t=1002.5)"""
        cap = _make_capture()
        _feed(cap, 6.0, captured_at=1000.0)
        chunk1, ts1 = cap.get_chunk(3.0)
        cap.requeue(chunk1, ts1)

        _, ts_first = cap.get_chunk(3.0)
        _, ts_second = cap.get_chunk(3.0)
        assert ts_first == pytest.approx(1000.0)
        assert ts_second == pytest.approx(1002.5)

    def test_clear_buffer_drops_requeued_too(self):
        cap = _make_capture()
        _feed(cap, 3.0, captured_at=1000.0)
        chunk, ts = cap.get_chunk(3.0)
        cap.requeue(chunk, ts)
        cap.clear_buffer()
        assert cap.get_chunk(3.0) is None


class TestBufferCap:
    def test_oldest_audio_dropped_beyond_cap(self):
        """given: кап 10 s
        when: накормили 14 s аудио
        then: буфер держит <= 10 s; capture-время первого сэмпла сдвинуто вперёд"""
        cap = _make_capture()
        for i in range(14):
            _feed(cap, 1.0, captured_at=1000.0 + i)
        assert cap._buffered_samples <= 10 * SR
        # Старейшие ~4 s отброшены -> первый сэмпл захвачен не раньше t=1004.0
        result = cap.get_chunk(3.0)
        assert result is not None
        _, ts = result
        assert ts >= pytest.approx(1004.0, abs=0.1)
```

- [x] **Step 5.2: Тесты падают**

Run: `./venv-gigaam/bin/python -m pytest tests/test_live_buffering.py -v`
Expected: FAIL — `AttributeError: ... no attribute '_append_locked_for_tests'` / `'_requeued'` / tuple unpack

- [x] **Step 5.3: Реализация AudioCapture в `recog/live.py`**

5.3.1. Импорты: в строку 19–23 (`from .config import (...)`) добавить `DEDUP_MAX_GAP_S, ASR_PENDING_BUFFER_CAP_S` (DEDUP понадобится в Task 6, добавить сразу). После строки 26 (`from .speaker_db import SpeakerDatabase`) добавить:

```python
from .log_utils import log_rate_limited
from .remote_asr import ASRTemporarilyUnavailable
```

Также добавить `Tuple` в импорт typing (строка 11): `from typing import Optional, Tuple`.

5.3.2. `AudioCapture.__init__` (строка 146) — дописать поля:

```python
    def __init__(self, device, sample_rate=16000, name="device"):
        import sounddevice as sd
        self.sd = sd
        self.sample_rate = sample_rate
        self.device = device
        self.name = name
        self.buffer = []
        self.running = False
        self.stream = None
        self._lock = None
        # --- capture-время + буферизация при недоступном ASR -----------------
        # Дизайн: docs/plans/2026-06-02-asr-fallback-buffering-design.md §2.
        # _buffer_start_ts — epoch-время захвата первого невычитанного сэмпла.
        self._buffer_start_ts: Optional[float] = None
        self._buffered_samples = 0
        # Чанки, возвращённые requeue() (ASR был недоступен) — отдаются первыми.
        self._requeued: list = []
        self._overlap_samples = int(0.5 * sample_rate)
        self._max_buffered_samples = int(ASR_PENDING_BUFFER_CAP_S * sample_rate)
```

5.3.3. `_callback` (строка 157) — учёт времени и капа (параметр `time` переименовать в `time_info`, иначе он затеняет модуль `time`):

```python
    def _callback(self, indata, frames, time_info, status):
        if status:
            print(f"Audio status ({self.name}): {status}", file=sys.stderr)
        captured_at = time.time() - frames / self.sample_rate
        if self._lock:
            with self._lock:
                dropped = self._append_locked(indata.copy(), captured_at)
        else:
            dropped = self._append_locked(indata.copy(), captured_at)
        if dropped:
            log_rate_limited(
                f"capture-overflow-{self.name}",
                f"[AudioCapture:{self.name}] ASR unavailable too long — "
                f"dropped {dropped / self.sample_rate:.1f}s of oldest audio",
            )

    def _append_locked(self, data: np.ndarray, captured_at: float) -> int:
        """Добавляет data в буфер; возвращает число сэмплов, отброшенных по капу.

        Вызывается с уже взятым _lock (или без лока до start())."""
        if not self.buffer:
            self._buffer_start_ts = captured_at
        self.buffer.append(data)
        self._buffered_samples += len(data)
        dropped = 0
        while self._buffered_samples > self._max_buffered_samples and self.buffer:
            oldest = self.buffer.pop(0)
            self._buffered_samples -= len(oldest)
            if self._buffer_start_ts is not None:
                self._buffer_start_ts += len(oldest) / self.sample_rate
            dropped += len(oldest)
        return dropped

    def _append_locked_for_tests(self, data: np.ndarray, captured_at: float) -> int:
        """Тестовый шов: _append_locked под собственным локом."""
        if self._lock:
            with self._lock:
                return self._append_locked(data, captured_at)
        return self._append_locked(data, captured_at)
```

5.3.4. `clear_buffer` (строка 191):

```python
    def clear_buffer(self):
        """Discard all buffered audio (used on meeting boundary)."""
        if self._lock:
            with self._lock:
                self.buffer = []
                self._requeued = []
                self._buffer_start_ts = None
                self._buffered_samples = 0
        else:
            self.buffer = []
            self._requeued = []
            self._buffer_start_ts = None
            self._buffered_samples = 0
```

5.3.5. `get_chunk` (строка 199) — кортеж + requeued-приоритет:

```python
    def get_chunk(self, duration: float) -> Optional[Tuple[np.ndarray, float]]:
        """Возвращает (аудио чанк (моно), epoch-время захвата первого сэмпла).

        Сначала отдаёт чанки из _requeued (возвращённые после
        ASRTemporarilyUnavailable), затем — накопленный буфер.
        """
        samples_needed = int(duration * self.sample_rate)

        if self._lock:
            self._lock.acquire()
        try:
            if self._requeued:
                return self._requeued.pop(0)
            if not self.buffer:
                return None
            audio = np.concatenate(self.buffer, axis=0)
            if len(audio) < samples_needed:
                return None

            chunk = audio[:samples_needed]
            chunk_start_ts = (
                self._buffer_start_ts
                if self._buffer_start_ts is not None
                else time.time() - duration
            )
            overlap_start = max(0, samples_needed - self._overlap_samples)
            self.buffer = [audio[overlap_start:]]
            self._buffered_samples = len(audio) - overlap_start
            self._buffer_start_ts = chunk_start_ts + overlap_start / self.sample_rate
        finally:
            if self._lock:
                self._lock.release()

        # Конвертируем стерео в моно если нужно
        if chunk.ndim > 1 and chunk.shape[1] > 1:
            chunk = np.mean(chunk, axis=1)

        return chunk.flatten(), chunk_start_ts

    def requeue(self, chunk: np.ndarray, capture_start_ts: float):
        """Возвращает уже вычитанный чанк обратно (ASR оказался недоступен).

        Чанк хранится в отдельном списке _requeued (он уже моно и фиксированной
        длины — обратно в покадровый буфер его не вставить из-за несовпадения
        размерностей со стерео-кадрами callback'а) и будет отдан первым же
        get_chunk. Порядок и capture-время сохраняются.
        """
        if self._lock:
            with self._lock:
                self._requeued.insert(0, (chunk, capture_start_ts))
        else:
            self._requeued.insert(0, (chunk, capture_start_ts))
```

- [x] **Step 5.4: Тесты зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_live_buffering.py -v`
Expected: 8 passed

- [x] **Step 5.5: Найти и обновить существующие вызовы `get_chunk`**

Run: `grep -rn "get_chunk" recog/ tests/ recog/web/ *.py`

Ожидаемые места:
- `recog/live.py:1662` (`_process_chunk`) — обновляется в Task 6
- `tests/test_live_speaker_attribution.py:420,458,494,706` — моки `capture.get_chunk.return_value = np.ones(48000, ...)` заменить на кортеж:

```python
    capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)
```

(во всех четырёх местах; если grep найдёт другие места — обновить по аналогии: возвращаемое значение становится кортежем `(chunk, capture_start_ts)`).

ВАЖНО: `python -m compileall recog` должен проходить, но `pytest tests/test_live_speaker_attribution.py` сейчас упадёт — это нормально, Task 6 чинит `_process_chunk` под кортеж. Закоммитить Task 5 и Task 6 вместе НЕЛЬЗЯ разрывать: поэтому коммит этого таска делаем только после Step 6.5 (см. Task 6). Если нужен промежуточный коммит — допустимо закоммитить только `tests/test_live_buffering.py` + `recog/live.py` с пометкой, что `_process_chunk` обновляется следующим коммитом, но прогон полного сьюта между ними будет красным. РЕКОМЕНДАЦИЯ: один коммит на Task 5+6.

---

### Task 6: `_process_chunk` — гейт доступности, capture-метки, дедуп-окно

**Files:**
- Modify: `recog/live.py` — `SourceState` (~290), `_transcribe_direct` (~442), `_process_chunk` (~1656)
- Test: `tests/test_live_buffering.py` (дополнить), `tests/test_live_speaker_attribution.py` (моки)

- [x] **Step 6.1: Падающие тесты** (добавить в `tests/test_live_buffering.py`)

```python
# --- гейт _process_chunk + дедуп-окно ----------------------------------------
from unittest.mock import Mock

from recog.live import LiveTranscriber, SourceState, deduplicate
from recog.remote_asr import ASRTemporarilyUnavailable


def _make_transcriber_stub(asr_available: bool = True) -> Mock:
    """Минимальный LiveTranscriber-образный объект для _process_chunk.

    Паттерн из tests/test_live_speaker_attribution.py::_fresh_transcriber_mock,
    локальная копия — чтобы не зависеть от чужого test-модуля."""
    t = Mock(spec=LiveTranscriber)
    t._meeting_generation = 0
    t._wav_lock = threading.Lock()
    t._state_lock = threading.Lock()
    t._wav_writer_mic = None
    t._wav_writer_system = None
    t._per_speaker_ingest = None
    t._ktalk_bridge = None
    t._meeting_storage = None
    t._refine_worker = None
    t.use_ktalk = False
    t.use_pcm_transport = False
    t.chunk_duration = 3.0
    t.transcript_lines = []
    t.mic_speaker_name = "Me"
    t._asr_available = lambda: asr_available
    return t


class TestAsrAvailabilityGate:
    def test_unavailable_asr_does_not_consume_buffer(self):
        """given: ASR недоступен (FallbackASR.available == False)
        when: _process_chunk
        then: get_chunk НЕ вызывается — аудио остаётся в буфере"""
        t = _make_transcriber_stub(asr_available=False)
        capture = Mock()
        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=True)
        capture.get_chunk.assert_not_called()

    def test_unavailable_exception_requeues_chunk(self):
        """given: ASR стал недоступен между get_chunk и transcribe
        when: _transcribe_direct кидает ASRTemporarilyUnavailable
        then: чанк возвращается в capture.requeue с исходным временем"""
        t = _make_transcriber_stub(asr_available=True)
        chunk = np.ones(48000, dtype=np.float32) * 0.1
        capture = Mock()
        capture.get_chunk.return_value = (chunk, 1000.0)
        t._transcribe_direct = Mock(side_effect=ASRTemporarilyUnavailable("loading"))
        t._ktalk_bridge = None

        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=True)

        capture.requeue.assert_called_once()
        requeued_chunk, requeued_ts = capture.requeue.call_args[0]
        np.testing.assert_array_equal(requeued_chunk, chunk)
        assert requeued_ts == pytest.approx(1000.0)

    def test_segment_timestamp_uses_capture_time_not_processing_time(self):
        """given: чанк, захваченный час назад (бэклог после сбоя)
        when: _process_chunk обрабатывает его сейчас
        then: _handle_mic_chunk получает chunk_time из capture-времени"""
        t = _make_transcriber_stub(asr_available=True)
        capture_ts = time.time() - 3600.0
        capture = Mock()
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, capture_ts)
        t._transcribe_direct = Mock(return_value="привет мир")
        t._check_silence_split = Mock()
        t._handle_mic_chunk = Mock()
        t._ktalk_bridge = None

        LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=True)

        t._handle_mic_chunk.assert_called_once()
        # сигнатура: (text, timestamp, chunk_time, chunk_gen, state)
        chunk_time_arg = t._handle_mic_chunk.call_args[0][2]
        expected_end = capture_ts + 3.0
        assert chunk_time_arg == pytest.approx(expected_end, abs=0.1)


class TestDedupWindow:
    def test_adjacent_chunks_still_deduplicated(self):
        """given: сегмент записан, следующий чанк начинается сразу (overlap)
        when: тот же текст
        then: dedup срабатывает — второй сегмент не пишется"""
        t = _make_transcriber_stub(asr_available=True)
        state = SourceState()
        capture = Mock()
        t._transcribe_direct = Mock(return_value="раз два три")
        t._check_silence_split = Mock()
        t._handle_mic_chunk = Mock()
        t._ktalk_bridge = None

        # Чанк 1: captured at 1000.0, конец 1003.0
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 1

        # Чанк 2: captured at 1002.5 (через overlap) — тот же текст -> dedup
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1002.5)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 1  # не вырос

    def test_repeated_phrase_after_gap_not_deduplicated(self):
        """given: сегмент записан; пользователь молчал > DEDUP_MAX_GAP_S
        when: та же фраза снова (capture-время через 30 s)
        then: dedup НЕ срабатывает — сегмент пишется"""
        t = _make_transcriber_stub(asr_available=True)
        state = SourceState()
        capture = Mock()
        t._transcribe_direct = Mock(return_value="раз два три")
        t._check_silence_split = Mock()
        t._handle_mic_chunk = Mock()
        t._ktalk_bridge = None

        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 1

        # Та же фраза через 30 секунд тишины
        capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1033.0)
        LiveTranscriber._process_chunk(t, capture, state, Mock(), is_mic=True)
        assert t._handle_mic_chunk.call_count == 2  # записан!
```

Примечание: `Mock(spec=LiveTranscriber)` не позволяет задавать произвольные атрибуты, которых нет в классе — если упадёт `AttributeError` на `t._asr_available = ...`, заменить `Mock(spec=LiveTranscriber)` на обычный `Mock()` и присвоить атрибуты так же.

- [x] **Step 6.2: Тесты падают**

Run: `./venv-gigaam/bin/python -m pytest tests/test_live_buffering.py -v -k "Gate or DedupWindow"`
Expected: FAIL (нет гейта; tuple unpack ломается; нет дедуп-окна)

- [x] **Step 6.3: Реализация в `recog/live.py`**

6.3.1. `SourceState` — добавить поле после `audio_offset: int = 0` (строка 296):

```python
    audio_offset: int = 0
    # Capture-время (epoch) конца чанка, давшего последний записанный сегмент.
    # Дедуп применяется только если новый чанк начинается в пределах
    # DEDUP_MAX_GAP_S от этого времени (дизайн §4).
    last_words_capture_end: float = 0.0
```

6.3.2. `LiveTranscriber._asr_available` — новый метод (рядом с `_transcribe_direct`, перед ним):

```python
    def _asr_available(self) -> bool:
        """True, когда ASR может обслужить запрос прямо сейчас.

        FallbackASR выставляет .available (False = remote упал И локаль ещё
        грузится — буферы трогать нельзя); чистый ASRModule свойства не имеет
        и всегда доступен.
        """
        return bool(getattr(self.asr, "available", True))
```

6.3.3. `_transcribe_direct` (строка 442) — re-raise unavailable + лог остального:

```python
    def _transcribe_direct(self, audio_chunk: np.ndarray) -> Optional[str]:
        """Транскрибирует напрямую через tensor без temp файлов.

        ASRTemporarilyUnavailable пробрасывается наверх (caller обязан вернуть
        чанк в буфер через capture.requeue). Любое другое исключение — лог +
        None (best-effort, контракт прежний).
        """
        try:
            # Serialize with PerSpeakerIngest worker's transcribe_batch: both
            # paths share self.asr.model, and PyTorch modules are not safe for
            # concurrent forward on a shared instance. Signal live priority
            # before contending so cooperative refine aborts promptly instead
            # of making mic/fallback live ASR wait behind a full refine pass.
            refine_worker = getattr(self, "_refine_worker", None)
            if refine_worker is not None:
                refine_worker.set_live_pending()
            try:
                # The forward+decode body lives in ASRModule.transcribe_chunk so
                # the remote-offload FallbackASR can route this path too; the
                # _model_lock / inference_mode / autoreleasepool live inside.
                return self.asr.transcribe_chunk(audio_chunk)
            finally:
                if refine_worker is not None:
                    refine_worker.clear_live_pending()

        except ASRTemporarilyUnavailable:
            raise
        except Exception as e:
            log_rate_limited(
                "live-transcribe-direct",
                f"[LiveTranscriber] _transcribe_direct failed: {type(e).__name__}: {e}",
            )
            return None
```

6.3.4. `_process_chunk` (строка 1656) — полная замена до строки 1734 (`text = self._transcribe_direct(chunk)` + 2 строки после):

```python
    def _process_chunk(self, capture, state, executor, is_mic=False):
        """Read audio chunk, run ASR, and dispatch to appropriate handler."""
        # ASR availability gate (дизайн §2): пока FallbackASR не может обслужить
        # запрос (remote упал, локаль грузится в фоне) — НЕ вычитываем буфер.
        # Capture-буфер и есть спул: аудио копится там до восстановления.
        if not self._asr_available():
            return

        # Snapshot meeting generation BEFORE reading buffer so that audio
        # captured before a meeting switch is tagged with the old generation
        # and discarded by the generation check downstream.
        chunk_gen = self._meeting_generation
        result = capture.get_chunk(self.chunk_duration)
        if result is None:
            return
        chunk, capture_start_ts = result
        if len(chunk) < 1000:
            return
        # Времена чанка — от capture-времени (НЕ time.time()): при обработке
        # бэклога после сбоя ASR сегменты должны лечь в то время, когда речь
        # была произнесена.
        chunk_start_time = capture_start_ts
        chunk_end_time = capture_start_ts + self.chunk_duration

        # Capture audio offset BEFORE writing chunk (for segment playback)
        # This offset points to where this chunk's audio STARTS in the WAV file
        # Lock protects against _on_meeting_change swapping writers from KTalk thread.
        # Generation check inside the lock prevents writing old-meeting audio to the
        # new meeting's WAV: _invalidate_pending_chunks bumps generation BEFORE
        # _on_meeting_change swaps writers under this same lock, so by the time we
        # acquire _wav_lock after a swap, the generation will already differ.
        with self._wav_lock:
            if self._meeting_generation != chunk_gen:
                return
            if is_mic and self._wav_writer_mic:
                state.audio_offset = self._wav_writer_mic.get_offset_ms()
                self._wav_writer_mic.write_chunk(chunk)
            elif not is_mic and self._wav_writer_system:
                state.audio_offset = self._wav_writer_system.get_offset_ms()
                self._wav_writer_system.write_chunk(chunk)

        # Per-speaker PCM primary-path gating for system-audio chunks
        # (see plan §4.5).  WAV writing above still runs so that the
        # system capture stays available as an offline fallback, but
        # we skip ASR while the PCM path is healthy and emit a gap
        # marker on degraded↔healthy transitions.
        #
        # `pcm_ever_seen` keeps the mixed-path running until the extension
        # has actually produced a chunk for this meeting — otherwise the
        # pre-join / broken-extension window silently swallows up to
        # PCM_SILENCE_WATCHDOG_S of system audio before the watchdog fires.
        fallback_mixed = False
        if (
            not is_mic
            and self._per_speaker_ingest is not None
            and self._per_speaker_ingest.pcm_ever_seen
        ):
            degraded_now = self._per_speaker_ingest.degraded
            # _last_pcm_degraded is also reset by _invalidate_pending_chunks
            # under _state_lock; guard the compare-and-swap so we don't emit
            # a spurious transition marker on a meeting boundary. Capture
            # chunk_gen atomically with the flag update so the transition
            # marker below lands on the meeting that observed the flip and
            # not a new one entered while we were releasing the lock.
            with self._state_lock:
                should_emit = degraded_now != self._last_pcm_degraded
                if should_emit:
                    self._last_pcm_degraded = degraded_now
                marker_gen = self._meeting_generation
            if should_emit:
                self._emit_pcm_transition_marker(degraded_now, marker_gen)
            if not degraded_now:
                # PCM is primary; drop system-audio ASR work entirely.
                return
            fallback_mixed = True

        # Пропускаем запись микрофона если на KTalk встрече и mic замьючен
        # Проверяем ДО ASR inference чтобы не тратить CPU на muted chunks
        if is_mic and self._ktalk_bridge and self._ktalk_bridge.is_local_mic_muted():
            return

        # VAD: пропускаем тишину (не тратим CPU на inference)
        rms = np.sqrt(np.mean(chunk ** 2))
        if rms < VAD_RMS_THRESHOLD:
            return

        try:
            text = self._transcribe_direct(chunk)
        except ASRTemporarilyUnavailable:
            # ASR отвалился между гейтом и вызовом (первый сбой remote).
            # Чанк не теряем — возвращаем в буфер, он будет обработан после
            # восстановления (фоновая загрузка локали / probe remote).
            capture.requeue(chunk, capture_start_ts)
            return
        if not text:
            return

        # Дедуп-окно (дизайн §4): last_words применяются только к временно́
        # смежным чанкам (audio overlap 0.5 s + джиттер). За пределами окна
        # повторённая фраза — реальная речь, её нельзя выбрасывать.
        if chunk_start_time - state.last_words_capture_end > DEDUP_MAX_GAP_S:
            state.last_words = []

        text = deduplicate(text, state.last_words)
        if not text:
            return

        # Bail out if meeting changed during ASR -- prevents stale
        # state updates (last_words/last_speaker) and transcript sends
        # from leaking across meeting boundaries
        if self._meeting_generation != chunk_gen:
            return

        state.last_words = text.split()[-5:]
        state.last_words_capture_end = chunk_end_time

        current_time = chunk_end_time
        self._check_silence_split(state, current_time)

        timestamp = datetime.fromtimestamp(chunk_end_time).strftime("%H:%M:%S")
        chunk_time = current_time

        if self.transcript_lines:
            sys.stdout.write("\n")

        if is_mic:
            self._handle_mic_chunk(text, timestamp, chunk_time, chunk_gen, state)
        elif self.use_ktalk:
            self._handle_ktalk_chunk(
                text, timestamp, chunk_start_time, chunk_end_time,
                chunk_time, chunk_gen, state,
                fallback_mixed=fallback_mixed)
        else:
            self._handle_embedding_chunk(
                text, timestamp, chunk_time, chunk_gen, state,
                chunk, executor, is_mic=is_mic)
```

(Изменения относительно текущего кода: гейт `_asr_available` в начале; `get_chunk` → кортеж; `chunk_start_time/chunk_end_time` из capture-времени вместо `time.time()`; try/except вокруг `_transcribe_direct` с requeue; дедуп-окно перед `deduplicate`; `state.last_words_capture_end` после записи; `current_time`/`timestamp` из capture-времени. Всё остальное — без изменений.)

- [x] **Step 6.4: Обновить моки в `tests/test_live_speaker_attribution.py`**

В строках 420, 458, 494, 706 заменить:

```python
    capture.get_chunk.return_value = np.ones(48000, dtype=np.float32) * 0.1
```

на:

```python
    capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)
```

Затем прогнать `./venv-gigaam/bin/python -m pytest tests/test_live_speaker_attribution.py -v` — если упадут тесты из-за `t._asr_available` (Mock-объекты без атрибута), добавить в `_fresh_transcriber_mock()` (найти: `grep -n "_fresh_transcriber_mock" tests/test_live_speaker_attribution.py`) строку:

```python
    t._asr_available = lambda: True
```

- [x] **Step 6.5: Все тесты зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_live_buffering.py tests/test_live_speaker_attribution.py tests/test_asr.py tests/test_live_decomposed.py -v`
Expected: все passed

- [x] **Step 6.6: Commit (Task 5 + Task 6 вместе)**

```bash
git add recog/live.py tests/test_live_buffering.py tests/test_live_speaker_attribution.py
git commit -m "feat(live): buffer audio while ASR unavailable, capture-time stamps, dedup window

_process_chunk no longer consumes the capture buffer when FallbackASR
cannot serve (remote down + local model still loading in background) —
the buffer becomes the spool, capped at ASR_PENDING_BUFFER_CAP_S.
Backlog segments get capture-time timestamps (when speech was spoken),
not processing-time. Deduplication now applies only to temporally
adjacent chunks (DEDUP_MAX_GAP_S): a phrase repeated after a pause is
real speech, not a chunk-overlap artifact — time-unbounded dedup is what
silently ate the user's speech in the 2026-06-02 incident." -- recog/live.py tests/test_live_buffering.py tests/test_live_speaker_attribution.py
```

---

### Task 7: PerSpeakerIngest — drain gate + retry batch

**Files:**
- Modify: `recog/per_speaker.py` — `__init__` (~93), `tick` (~421)
- Modify: `recog/live.py` — `_setup_per_speaker_ingest` (~737), `_on_pcm_batch` (~882)
- Test: `tests/test_per_speaker.py`

- [x] **Step 7.1: Падающие тесты** (добавить в конец `tests/test_per_speaker.py`)

Перед написанием посмотреть существующие тесты (например `test_batch_collector_fires_on_timeout`, строка 147) — как конструируется `PerSpeakerIngest` и как ему скармливаются чанки (`PcmChunk`), и переиспользовать тот же паттерн/хелперы:

```python
# --- ASR availability drain gate ---------------------------------------------
from recog.remote_asr import ASRTemporarilyUnavailable


def test_tick_does_not_drain_when_asr_unavailable():
    """given: ASR недоступен (asr_available_getter -> False)
    when: tick()
    then: transcribe_callback НЕ вызывается, чанки остаются в буферах"""
    received = []
    asr_available = [False]
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda batch: received.append(batch),
        generation_getter=lambda: 0,
        meeting_active_getter=lambda: True,
        asr_available_getter=lambda: asr_available[0],
    )
    # скормить один чанк (взять хелпер создания PcmChunk из существующих тестов,
    # например из test_batch_collector_fires_on_timeout)
    ingest.enqueue(_make_chunk(pid="p1", seq=0))

    ingest.tick()
    assert received == []

    # ASR вернулся -> следующий tick выгребает накопленное
    asr_available[0] = True
    ingest.tick()
    assert len(received) == 1


def test_unavailable_exception_holds_batch_for_retry():
    """given: callback кидает ASRTemporarilyUnavailable (гонка первого сбоя)
    when: tick(), затем ASR восстановился и tick() снова
    then: тот же батч диспатчится повторно — ничего не потеряно"""
    calls = []
    fail_first = [True]

    def callback(batch):
        calls.append(batch)
        if fail_first[0]:
            fail_first[0] = False
            raise ASRTemporarilyUnavailable("remote just died")

    ingest = PerSpeakerIngest(
        transcribe_callback=callback,
        generation_getter=lambda: 0,
        meeting_active_getter=lambda: True,
        asr_available_getter=lambda: True,
    )
    ingest.enqueue(_make_chunk(pid="p1", seq=0))

    ingest.tick()   # callback падает с unavailable -> батч сохранён
    ingest.tick()   # повторный диспатч того же батча

    assert len(calls) == 2
    assert calls[0] == calls[1]  # тот же батч (те же BatchedChunk кортежи)
```

`_make_chunk` — это НЕ существующий хелпер: создать его в этом же блоке по образцу того, как существующие тесты конструируют `PcmChunk` (посмотреть `grep -n "PcmChunk(" tests/test_per_speaker.py | head -3` и скопировать поля с минимальными значениями).

- [x] **Step 7.2: Тесты падают**

Run: `./venv-gigaam/bin/python -m pytest tests/test_per_speaker.py -v -k "drain_when_asr or holds_batch"`
Expected: FAIL — `TypeError: __init__() got an unexpected keyword argument 'asr_available_getter'`

- [x] **Step 7.3: Реализация `recog/per_speaker.py`**

7.3.1. Импорт (в шапке файла, после существующих импортов из `.`):

```python
from .remote_asr import ASRTemporarilyUnavailable
```

7.3.2. `__init__` — новый параметр (после `refine_submit_callback`):

```python
        refine_submit_callback: Optional[Callable[[RefineTask], None]] = None,
        asr_available_getter: Optional[Callable[[], bool]] = None,
    ):
```

и в теле `__init__` (после `self._refine_submit_callback = ...`):

```python
        # ASR availability gate (дизайн 2026-06-02 §3): пока ASR не может
        # обслужить batch (remote упал, локаль грузится) — drain не выгребает
        # буферы, PCM копится в per-pid ring'ах (с их существующими капами).
        self._asr_available_getter = asr_available_getter or (lambda: True)
        # Однослотовый retry: батч, на котором callback кинул
        # ASRTemporarilyUnavailable (гонка первого сбоя). Диспатчится первым
        # при следующем tick с доступным ASR.
        self._pending_retry_batch: Optional[List[BatchedChunk]] = None
```

7.3.3. `tick()` — гейт + retry (заменить блок `with self._lock:` и блок `if batch:`):

```python
        with self._lock:
            self._check_watchdog_locked(now)
            if self._asr_available_getter():
                if self._pending_retry_batch is not None:
                    batch = self._pending_retry_batch
                    self._pending_retry_batch = None
                else:
                    batch = self._drain_batch_locked()
            # else: ASR недоступен — гейт закрыт, PCM остаётся в буферах.
            # Advance per-pid silence counter for pids that did not see a new
            # chunk this interval. Runs unconditionally so Task 7's trigger
            # logic has fresh `_silence_ms` / `_continuous_speech_ms` numbers
            # even when the drain itself was a no-op.
            self._update_per_pid_silence_locked()
            if self._refine_submit_callback is not None:
                # Snapshot pid list under the lock; get_refine_candidate
                # re-acquires _lock (it's threading.Lock, not reentrant), so
                # the per-pid check must run without _lock held.
                active_pids = list(self._pid_audio_ms.keys())
        if batch:
            try:
                self._transcribe_callback(batch)
            except ASRTemporarilyUnavailable:
                # Гонка первого сбоя: available был True на входе в tick, но
                # remote упал во время transcribe. Батч уже выгребен из буферов
                # (drain деструктивен) — держим его для повторного диспатча.
                with self._lock:
                    self._pending_retry_batch = batch
                print(
                    "[PerSpeakerIngest] ASR unavailable — holding batch for retry",
                    file=sys.stderr,
                )
            except Exception as e:
                print(
                    f"[PerSpeakerIngest] transcribe callback raised: {e}",
                    file=sys.stderr,
                )
```

- [x] **Step 7.4: Реализация в `recog/live.py`**

7.4.1. `_setup_per_speaker_ingest` (~строка 737) — добавить getter:

```python
        self._per_speaker_ingest = PerSpeakerIngest(
            transcribe_callback=self._on_pcm_batch,
            generation_getter=lambda: self._meeting_generation,
            meeting_active_getter=lambda: bool(
                self._meeting_storage
                and self._meeting_storage.get_current_meeting_id()
            ),
            asr_available_getter=self._asr_available,
        )
```

7.4.2. `_on_pcm_batch` (~строка 882) — пробросить unavailable (заменить try/except вокруг `transcribe_batch`):

```python
        try:
            texts = self.asr.transcribe_batch(pcm_list)
        except ASRTemporarilyUnavailable:
            # Пробрасываем в PerSpeakerIngest.tick — он сохранит батч для
            # повторного диспатча (дизайн §3, гонка первого сбоя).
            raise
        except Exception as e:
            print(f"[PerSpeaker] transcribe_batch failed: {e}", file=sys.stderr)
            return
        finally:
            if refine_worker is not None:
                refine_worker.clear_live_pending()
```

- [x] **Step 7.5: Тесты зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_per_speaker.py tests/test_live_speaker_attribution.py -v`
Expected: все passed (существующие тесты per_speaker не задеты — новый параметр опционален)

- [x] **Step 7.6: Commit**

```bash
git add recog/per_speaker.py recog/live.py tests/test_per_speaker.py
git commit -m "feat(per-speaker): gate PCM drain on ASR availability, hold batch on failure

PerSpeakerIngest.tick() no longer drains per-pid buffers while
FallbackASR cannot serve — PCM keeps accumulating in the existing rings.
A batch caught mid-failure (drained before remote died) is held in a
single retry slot and re-dispatched first after recovery, instead of
being dropped with only a log line." -- recog/per_speaker.py recog/live.py tests/test_per_speaker.py
```

---

### Task 8: RefineWorker — unavailable → re-enqueue с задержкой

**Files:**
- Modify: `recog/refine.py` — `_process_task` (~412-480)
- Test: `tests/test_refine_fallback.py`

- [x] **Step 8.1: Падающий тест**

Сначала посмотреть существующие тесты: `grep -n "def test_\|RefineWorker(" tests/test_refine_fallback.py` — переиспользовать их паттерн конструирования `RefineWorker` и `RefineTask`. Добавить:

```python
def test_unavailable_asr_requeues_task_with_delay(monkeypatch):
    """given: _transcribe_fn кидает ASRTemporarilyUnavailable (remote упал,
           локаль грузится)
    when: _process_task
    then: задача возвращается в очередь (не потеряна), перед этим выдержана
          пауза (no busy-spin)"""
    from recog.remote_asr import ASRTemporarilyUnavailable
    import recog.refine as refine_mod

    sleeps = []
    monkeypatch.setattr(refine_mod.time, "sleep", lambda s: sleeps.append(s))

    # Конструирование worker/task — по образцу существующих тестов этого файла
    # (скопировать их setup; _transcribe_fn заменить на кидающий unavailable).
    worker, task = _make_worker_and_task(
        transcribe_fn=Mock(side_effect=ASRTemporarilyUnavailable("loading")),
    )

    worker._process_task(task)

    assert worker._queue.qsize() == 1          # задача вернулась в очередь
    assert worker._queue.get_nowait() is task  # та же задача
    assert sleeps and sleeps[0] >= 2.0          # пауза перед re-enqueue
```

`_make_worker_and_task` — хелпер, который нужно написать в этом тест-файле по образцу setup-кода существующих тестов (worker строится с `_transcribe_fn`, `_apply_fn`, `abort_mode`; task — `RefineTask` с минимальными полями). Если в файле уже есть подходящий хелпер/fixture — использовать его и не создавать новый.

- [x] **Step 8.2: Тест падает**

Run: `./venv-gigaam/bin/python -m pytest tests/test_refine_fallback.py -v -k unavailable`
Expected: FAIL — исключение ASRTemporarilyUnavailable не поймано (вылетает из `_process_task`) или задача не в очереди

- [x] **Step 8.3: Реализация `recog/refine.py`**

8.3.1. Импорты (в шапке):

```python
from .remote_asr import ASRTemporarilyUnavailable
```

(проверить циклические импорты: `remote_asr` не импортирует `refine` — цикла нет; если `refine.py` импортируется из `remote_asr.py` транзитивно через `.asr`, то вынести импорт внутрь `_process_task`.)

8.3.2. Класс-константа `RefineWorker` (рядом с `_POLL_S`):

```python
    # Пауза перед возвратом задачи в очередь, когда ASR временно недоступен
    # (remote упал + локаль грузится). Без неё worker крутится в busy-loop:
    # get -> unavailable -> put -> get -> ...
    _UNAVAILABLE_RETRY_DELAY_S = 2.0
```

8.3.3. `_process_task` — добавить ветку (в try-блоке после `except RefineAborted:` и обработку после выхода из `with self._model_lock:`):

```python
        unavailable = False
        ...
                except RefineAborted:
                    aborted = True
                except ASRTemporarilyUnavailable:
                    unavailable = True

        if unavailable:
            # ASR (remote + локаль) сейчас не может обслужить refine. Это
            # tier-2 контур — просто вернуть задачу в очередь после паузы;
            # live-путь тем временем буферизует своё аудио независимо.
            time.sleep(self._UNAVAILABLE_RETRY_DELAY_S)
            self._queue.put(task)
            return

        if aborted:
            # Re-enqueue; the pre-lock gate on the next iteration will wait
            # for live to release `_model_lock` before contending again.
            self._queue.put(task)
            return
```

(Конкретно: объявить `unavailable = False` рядом с `aborted = False` в начале `_process_task`; добавить `except ASRTemporarilyUnavailable: unavailable = True` после `except RefineAborted: aborted = True` (строка ~473-474); добавить блок `if unavailable:` ПЕРЕД существующим `if aborted:` (строка ~476).)

- [x] **Step 8.4: Тесты зелёные**

Run: `./venv-gigaam/bin/python -m pytest tests/test_refine_fallback.py tests/test_refine.py tests/test_refine_integration.py -v`
Expected: все passed

- [x] **Step 8.5: Commit**

```bash
git add recog/refine.py tests/test_refine_fallback.py
git commit -m "feat(refine): re-enqueue refine task with delay when ASR temporarily unavailable" -- recog/refine.py tests/test_refine_fallback.py
```

---

### Task 9: Документация, полный прогон, перезапуск сервиса

**Files:**
- Modify: `CLAUDE.md` (секции "Remote ASR offload" и "Environment Variables")
- Modify: `.env.example`
- Modify: `docs/plans/2026-06-02-asr-fallback-buffering-plan.md` (отметить чекбоксы)

- [x] **Step 9.1: Полный прогон тестов и компиляция**

Run: `./venv-gigaam/bin/python -m pytest tests/ -v -x --ignore=tests/e2e && ./venv-gigaam/bin/python -m compileall recog frontend -q`
Expected: все passed, компиляция без ошибок. Если что-то падает — чинить ДО доков.

- [x] **Step 9.2: Обновить CLAUDE.md**

В секции "### Remote ASR offload (opt-in)" заменить предложение про lazy load:

Было: «`FallbackASR` constructs the local `ASRModule` eagerly but loads its weights **lazily** — only on the first remote failure — and trips a thread-safe circuit breaker...»

Стало: «`FallbackASR` constructs the local `ASRModule` eagerly but loads its weights **lazily in a background thread** — triggered by the first remote failure. While neither backend can serve (circuit open + local still loading) it raises `ASRTemporarilyUnavailable` and exposes `available=False`; consumers (`_process_chunk`, `PerSpeakerIngest.tick`, `RefineWorker`) gate on it and keep their audio buffered instead of dropping chunks (see `docs/plans/2026-06-02-asr-fallback-buffering-design.md`). It also trips a thread-safe circuit breaker...»

В секцию "## Environment Variables" после `RECOG_ASR_CIRCUIT_OPEN_S` добавить:

```markdown
- `RECOG_ASR_LIVE_TIMEOUT_S` - Remote timeout for live chunk/batch requests (default `5`). Refine/offline keep `RECOG_ASR_REMOTE_TIMEOUT_S`.
- `DEDUP_MAX_GAP_S` - Live dedup window (default `2.0`): `last_words` dedup applies only to chunks captured within this many seconds of the previous segment.
- `ASR_PENDING_BUFFER_CAP_S` - Max audio buffered in memory while ASR is unavailable (default `600`).
```

- [x] **Step 9.3: Обновить `.env.example`**

Посмотреть текущее содержимое (`cat .env.example`), добавить в секцию ASR (или создать её по образцу существующих комментариев):

```bash
# ASR fallback buffering (docs/plans/2026-06-02-asr-fallback-buffering-design.md)
# RECOG_ASR_LIVE_TIMEOUT_S=5
# DEDUP_MAX_GAP_S=2.0
# ASR_PENDING_BUFFER_CAP_S=600
```

- [x] **Step 9.4: Commit доков**

```bash
git add CLAUDE.md .env.example docs/plans/2026-06-02-asr-fallback-buffering-plan.md
git commit -m "docs(asr): document fallback buffering env vars and behavior" -- CLAUDE.md .env.example docs/plans/2026-06-02-asr-fallback-buffering-plan.md
```

- [x] **Step 9.5: Перезапуск сервиса и smoke-проверка**

```bash
./recog-service.sh restart
sleep 5
./recog-service.sh status
```

Expected: оба сервиса Running. Затем попросить пользователя сказать фразу и проверить:

```bash
tail -3 logs/live.stdout.log
```

Expected: реплика появилась. Также проверить отсутствие новых ошибок: `tail -20 logs/live.stderr.log | grep -v "Clock sync"`.

- [x] **Step 9.6: Финальная проверка инцидент-сценария (опционально, при доступе к Z270XX)**

Эмулировать сбой remote: на Z270XX выполнить `ssh Z270XX 'systemctl --user stop asr-gateway'`, сказать 2–3 фразы в течение минуты, затем `ssh Z270XX 'systemctl --user start asr-gateway'`. Проверить:
1. В stderr: `loading local GigaAM in background` (один раз), затем `local GigaAM ready`
2. Все сказанные фразы появились в transcript.jsonl с временем произнесения
3. После восстановления gateway: `remote recovered — circuit CLOSED` в stderr при следующем probe

---

## Self-Review (выполнено при написании)

1. **Spec coverage:** §1 FallbackASR → Task 3; §2 mic/system буфер → Tasks 5–6; §3 per-speaker → Task 7; §4 дедуп → Task 6; §5 логирование → Tasks 1, 3, 4, 6; D5 live-timeout → Task 3; D7 капы → Tasks 5, 7; тесты → во всех тасках; доки → Task 9. Требование 7 (local backend не меняется) — гейт `_asr_available()` возвращает True для ASRModule (нет атрибута `available`), дедуп/метки применяются к обоим. ✓
2. **Placeholder scan:** хелперы `_make_chunk` (Task 7) и `_make_worker_and_task` (Task 8) требуют чтения существующих тестов на месте — это намеренная отсылка к реальному коду, а не заглушка; шаги содержат точные grep-команды для их написания. ✓
3. **Type consistency:** `ASRTemporarilyUnavailable` (remote_asr) ← импортируется в live/per_speaker/refine/тесты; `available` (property) + `_asr_available()` (метод LiveTranscriber); `get_chunk -> Optional[Tuple[np.ndarray, float]]`; `requeue(chunk, capture_start_ts)`; `asr_available_getter` (параметр PerSpeakerIngest). Имена сверены между тасками. ✓
