"""
Удалённый ASR-клиент с ленивым локальным fallback'ом.

Этот модуль реализует все четыре GigaAM-пути (live per-speaker
``transcribe_batch``, offline CLI ``transcribe_simple``, refine
``transcribe_with_timestamps`` и прямой mic/mixed-fallback
``transcribe_chunk``) поверх сетевого шлюза (OpenAI-Whisper-совместимый),
сохраняя локальный GigaAM как резерв. Через ``build_asr()`` сегодня вызываются
три live-пути; offline ``transcribe_simple`` реализован, но ещё не подключён к
CLI (``mp4_notes.py`` — только ``live``/``meetings``).

Две идеи определяют дизайн:

1. **Ленивый локальный fallback.** ``FallbackASR`` СОЗДАЁТ локальный
   ``ASRModule`` сразу (это дёшево — ``ASRModule.__init__`` модель не
   грузит), но ВЕСА грузит в ФОНОВОМ потоке — триггер первый сбой удалённого
   шлюза. Пока шлюз жив, тяжёлая модель на Mac не поднимается вовсе. Пока
   веса грузятся (а remote лежит), методы поднимают
   ``ASRTemporarilyUnavailable`` и caller буферизует данные у себя —
   ничего не блокируется и не теряется.

2. **Circuit breaker.** Live (chunk/batch) и фоновые (refine/offline) запросы
   имеют независимые состояния: ошибка refine не отключает live. Каждый сбой
   удалёнки накапливается в своём состоянии; после
   ``fail_threshold`` за последние 60 секунд circuit «открывается» на ``open_seconds`` —
   запросы идут сразу в локаль, минуя таймауты сети. По истечении окна
   разрешается один probe (half-open): успех закрывает circuit, ещё один
   сбой открывает заново. Состояние под ``threading.Lock`` + монотонными
   часами, потому что live-loop, PerSpeakerIngest-worker и RefineWorker
   ходят сюда конкурентно.

Поведение репозитория не меняется, пока ``RECOG_ASR_BACKEND`` остаётся
``"local"`` (default): ``build_asr()`` тогда отдаёт чистый ``ASRModule``.
"""

import io
import sys
import threading
import time
import traceback
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from typing import TYPE_CHECKING, Any, Callable, List, Optional, Union

import httpx
import numpy as np
import soundfile as sf

from .log_utils import log_rate_limited
from .models import Segment

if TYPE_CHECKING:
    # Imported where used: gigaam and recog.asr import torch, which the wavo
    # backend never needs.
    from gigaam.types import TranscriptionResult

    from .asr import ASRModule
    from .wavo_backend import WavoASR


def _local_asr_module() -> "ASRModule":
    """The PyTorch model's wrapper, without its weights (they load lazily)."""
    from .asr import ASRModule

    return ASRModule()


class RemoteASRError(Exception):
    """Поднимается при любом сбое удалённого шлюза (HTTP/parse/empty)."""
    pass


class ASRTemporarilyUnavailable(Exception):
    """ASR прямо сейчас не может обслужить запрос: remote упал/недоступен, а
    локальная модель ещё грузится (или её загрузка недавно упала).

    Контракт для caller'а: данные НЕ потеряны — сохрани их (requeue в свой
    буфер) и повтори позже. Поднимается только из FallbackASR; чистый
    ASRModule его никогда не кидает.
    """
    pass


def _to_float32_mono(audio: Union[np.ndarray, Any]) -> np.ndarray:
    """Нормализует вход к 1-D float32 в [-1, 1] (как в ASRModule).

    Принимает np.ndarray / torch.Tensor / что приводимо к массиву. Int16
    PCM делится на 32768; многомерный вход сжимается до моно.
    """
    try:
        import torch
        if isinstance(audio, torch.Tensor):
            audio = audio.detach().cpu().numpy()
    except ImportError:
        pass

    arr = np.asarray(audio)
    if arr.ndim > 1:
        arr = np.squeeze(arr)
    if arr.ndim != 1:
        raise ValueError(f"audio must be 1-D mono; got shape {arr.shape}")

    if arr.dtype == np.int16:
        arr = arr.astype(np.float32) / 32768.0
    elif arr.dtype != np.float32:
        arr = arr.astype(np.float32)
    return arr


def _wav_bytes(audio: np.ndarray, sample_rate: int) -> bytes:
    """Сериализует float32-моно в in-memory WAV (FLOAT subtype)."""
    buf = io.BytesIO()
    sf.write(buf, audio, sample_rate, format="WAV", subtype="FLOAT")
    return buf.getvalue()


class RemoteASRClient:
    """Синхронный клиент OpenAI-Whisper-совместимого ASR-шлюза.

    Один ``httpx.Client`` переиспользуется для keep-alive; ``transcribe_batch``
    раскидывает клипы по ``ThreadPoolExecutor`` с сохранением порядка. Любой
    HTTP/parse-сбой превращается в ``RemoteASRError``, чтобы ``FallbackASR``
    мог поймать его и уйти в локаль.
    """

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
        # Один долгоживущий пул на клиент: transcribe_batch зовётся ~5 раз/с
        # (батч-окно 200 ms), создавать/сносить по 8 потоков на каждый вызов —
        # лишний churn в горячем live-пути.
        self._pool = ThreadPoolExecutor(
            max_workers=max_workers, thread_name_prefix="remote-asr"
        )

    def healthz(self) -> bool:
        """GET /healthz — True только если шлюз достижим И вернул ok.

        Шлюз бывает двух форм: плоский ``{"ok": true}`` (бэкенд напрямую) и
        агрегатор per-engine ``{"gigaam": {"ok": true}, "qwen": {...}}``. Во
        втором случае смотрим на движок нашей модели, иначе — на любой ok.
        """
        try:
            resp = self._client.get(f"{self.base_url}/healthz")
            resp.raise_for_status()
            data = resp.json()
        except Exception:
            return False
        if not isinstance(data, dict):
            return False
        if "ok" in data:
            return bool(data["ok"])
        engine = data.get(self.model)
        if isinstance(engine, dict) and "ok" in engine:
            return bool(engine["ok"])
        return any(isinstance(v, dict) and v.get("ok") for v in data.values())

    def transcribe_chunk(self, audio: np.ndarray, sample_rate: int = 16000) -> str:
        """Транскрибирует один клип; POST response_format=json -> {text}.

        Поднимает ``RemoteASRError`` при любом HTTP/parse-сбое (caller
        упадёт в локаль на весь чанк/батч).
        """
        arr = _to_float32_mono(audio)
        files = {"file": ("chunk.wav", _wav_bytes(arr, sample_rate), "audio/wav")}
        data = {"model": self.model, "response_format": "json"}
        try:
            resp = self._client.post(
                f"{self.base_url}/v1/audio/transcriptions",
                files=files,
                data=data,
                timeout=self.live_timeout_s,
            )
            resp.raise_for_status()
            return resp.json()["text"]
        except Exception as e:
            raise RemoteASRError(f"transcribe_chunk failed: {e}") from e

    def transcribe_batch(self, pcm_list: List[np.ndarray]) -> List[str]:
        """Параллельный transcribe_chunk по списку, ПОРЯДОК сохраняется.

        Если ХОТЬ один клип упал — поднимает ``RemoteASRError`` (caller
        пере-транскрибирует весь батч локально, чтобы не было дыр в
        per-speaker атрибуции).
        """
        if not pcm_list:
            return []
        try:
            # `map` сохраняет входной порядок и пробрасывает первое
            # исключение при материализации списка.
            return list(self._pool.map(self.transcribe_chunk, pcm_list))
        except RemoteASRError:
            raise
        except Exception as e:
            raise RemoteASRError(f"transcribe_batch failed: {e}") from e

    def transcribe_simple(self, audio_path: str, **kwargs: Any) -> List[Segment]:
        """Offline-файл; POST response_format=verbose_json -> segments[].

        Маппит каждый сегмент шлюза (start/end в СЕКУНДАХ) в
        ``recog.models.Segment(text, start, end)``.
        """
        data = {"model": self.model, "response_format": "verbose_json"}
        try:
            with open(audio_path, "rb") as fh:
                files = {"file": (audio_path, fh.read(), "audio/wav")}
            resp = self._client.post(
                f"{self.base_url}/v1/audio/transcriptions", files=files, data=data
            )
            resp.raise_for_status()
            payload = resp.json()
        except Exception as e:
            raise RemoteASRError(f"transcribe_simple failed: {e}") from e

        segments: List[Segment] = []
        for seg in payload.get("segments", []):
            segments.append(
                Segment(
                    text=seg.get("text", ""),
                    start=float(seg.get("start", 0.0)),
                    end=float(seg.get("end", 0.0)),
                )
            )
        return segments

    def transcribe_with_timestamps(
        self, audio: Union[np.ndarray, Any, str], sample_rate: int = 16000
    ) -> "TranscriptionResult":
        """Refine-путь; POST WAV с timestamp_granularities="word".

        Парсит top-level ``words[]={word,start(sec),end(sec)}`` в
        ``Word(text, start_ms, end_ms)`` (INT миллисекунды — та же форма,
        что у ``ASRModule.transcribe_with_timestamps``), оборачивает в
        ``TranscriptionResult(text, words)``. Принимает path/ndarray/Tensor;
        Int16 нормализуется в float32 как в локальном пути.
        """
        if isinstance(audio, str):
            with open(audio, "rb") as fh:
                wav_payload = fh.read()
        else:
            arr = _to_float32_mono(audio)
            wav_payload = _wav_bytes(arr, sample_rate)

        files = {"file": ("refine.wav", wav_payload, "audio/wav")}
        # verbose_json: шлюз отдаёт top-level words[] только под verbose_json/
        # diarized_json (плоский json возвращает лишь {text}).
        data = {
            "model": self.model,
            "response_format": "verbose_json",
            "timestamp_granularities": "word",
        }
        try:
            resp = self._client.post(
                f"{self.base_url}/v1/audio/transcriptions", files=files, data=data
            )
            resp.raise_for_status()
            payload = resp.json()
        except Exception as e:
            raise RemoteASRError(f"transcribe_with_timestamps failed: {e}") from e

        from gigaam.types import TranscriptionResult, Word

        words: List[Word] = []
        for w in payload.get("words") or []:
            words.append(
                Word(
                    text=w["word"],
                    start=int(round(float(w["start"]) * 1000)),
                    end=int(round(float(w["end"]) * 1000)),
                )
            )
        return TranscriptionResult(text=payload.get("text", ""), words=words)


class _CircuitBreaker:
    """Independent failure window and recovery probe for one request class."""

    FAILURE_WINDOW_S = 60.0

    def __init__(self, fail_threshold: int, open_seconds: float, scope: str):
        self.fail_threshold = fail_threshold
        self.open_seconds = open_seconds
        self.scope = scope
        self._cb_lock = threading.Lock()
        self._failures = 0
        self._failure_times = deque()
        self._opened_until = 0.0
        self._epoch = 0

    @property
    def using_remote(self) -> bool:
        with self._cb_lock:
            return self._opened_until == 0.0 or time.monotonic() >= self._opened_until

    def _decide(self) -> tuple[bool, int]:
        """Решает remote-vs-local И резервирует единственный half-open probe.

        Возвращает ``(try_remote, epoch)``. Когда окно истекло, первый
        поток под lock'ом продлевает окно (резервируя единственный probe) и
        получает ``try_remote=True``; конкурентные потоки видят продлённое
        окно и идут в локаль (без thundering herd по только что
        восстановившемуся шлюзу). ``epoch`` фиксирует поколение open, чтобы
        запоздавший success не сбросил circuit, который позже переоткрыл сбой.
        """
        with self._cb_lock:
            now = time.monotonic()
            if self._opened_until == 0.0:
                return True, self._epoch
            if now >= self._opened_until:
                self._opened_until = now + self.open_seconds  # reserve the lone probe
                return True, self._epoch
            return False, self._epoch

    def _record_success(self, epoch: int) -> None:
        was_open = False
        with self._cb_lock:
            if epoch != self._epoch:
                return  # stale: circuit re-opened after we started — не отменяем
            was_open = self._opened_until != 0.0
            # Isolated successes must not hide a flapping connection. Only a
            # successful recovery probe starts a fresh observation window.
            if was_open:
                self._failure_times.clear()
                self._failures = 0
            self._opened_until = 0.0
        if was_open:
            print(f"[FallbackASR] {self.scope} remote recovered — circuit CLOSED", file=sys.stderr)

    def _record_failure(self, epoch: int) -> None:
        with self._cb_lock:
            if epoch != self._epoch:
                return  # A late failure must not poison a recovered circuit.
            now = time.monotonic()
            while self._failure_times and self._failure_times[0] <= now - self.FAILURE_WINDOW_S:
                self._failure_times.popleft()
            self._failure_times.append(now)
            self._failures = len(self._failure_times)
            # Переход closed->open. Если окно уже выставлено (open или
            # зарезервированный probe) — сбой просто копится, окно держится.
            if self._failures >= self.fail_threshold and self._opened_until == 0.0:
                self._opened_until = time.monotonic() + self.open_seconds
                self._epoch += 1
                print(
                    f"[FallbackASR] {self.scope} remote circuit OPEN for {self.open_seconds}s "
                    f"after {self._failures} fail(s) in {self.FAILURE_WINDOW_S}s",
                    file=sys.stderr,
                )


class FallbackASR:
    """Маршрутизатор remote→local с thread-safe circuit breaker'ом.

    Локальный ``ASRModule`` СОЗДАЁТСЯ сразу (дёшево), но его ВЕСА грузятся
    лениво — в ФОНОВОМ потоке, триггер — первый сбой remote. Пока ни один
    бэкенд не может обслужить запрос (circuit открыт + локаль ещё грузится),
    методы поднимают ``ASRTemporarilyUnavailable``, а ``available`` отдаёт
    False — потребители обязаны буферизовать данные, а не выбрасывать их
    (см. docs/plans/2026-06-02-asr-fallback-buffering-design.md). Делегирует
    ``model`` / ``device`` / ``_model_lock`` / ``_maybe_empty_mps_cache``
    в локальный модуль, чтобы существующие caller'ы и ``RefineWorker``
    (который берёт тот же ``_model_lock`` извне) продолжали работать.
    """

    # Повторная попытка фоновой загрузки локали после провала — не раньше чем
    # через это число секунд (защита от спама загрузками при битой модели).
    LOCAL_LOAD_RETRY_S = 300.0

    def __init__(
        self,
        remote: RemoteASRClient,
        local_factory: Callable[[], "ASRModule"] = _local_asr_module,
        fail_threshold: int = 3,
        open_seconds: float = 60.0,
    ):
        self.remote = remote
        # Дёшево: ASRModule.__init__ модель не грузит. load_model() НЕ зовём.
        self._local = local_factory()
        self.fail_threshold = fail_threshold
        self.open_seconds = open_seconds
        self._live_circuit = _CircuitBreaker(fail_threshold, open_seconds, "live")
        self._background_circuit = _CircuitBreaker(fail_threshold, open_seconds, "background")
        self._announced_fallback = False
        # --- Фоновая загрузка локальной модели (non-blocking fallback) ------
        # Дизайн: docs/plans/2026-06-02-asr-fallback-buffering-design.md §1.
        self._load_lock = threading.Lock()
        self._local_load_thread: Optional[threading.Thread] = None
        self._local_ready = False
        self._local_load_failed_at = 0.0  # monotonic; 0 => не падала

    # ------------------------------------------------------------------ #
    # Делегирование в локальный модуль (для caller'ов и RefineWorker).
    # ------------------------------------------------------------------ #
    @property
    def model(self):
        return self._local.model

    @property
    def device(self):
        return self._local.device

    @property
    def _model_lock(self):
        # Тот же объект lock'а — RefineWorker берёт его извне.
        return self._local._model_lock

    def _maybe_empty_mps_cache(self) -> None:
        return self._local._maybe_empty_mps_cache()

    # ------------------------------------------------------------------ #
    # Circuit breaker.
    # ------------------------------------------------------------------ #
    @property
    def using_remote(self) -> bool:
        """Advisory state of live chunk/batch requests; does not reserve a probe."""
        return self._live_circuit.using_remote

    @property
    def available(self) -> bool:
        """True, когда запрос можно обслужить прямо сейчас.

        Либо remote presumed alive (circuit закрыт или его окно истекло —
        можно пробовать probe), либо локальная модель уже загружена.
        Потребители (live loop, PerSpeakerIngest) гейтят на этом флаге
        вычитку своих буферов — False означает «не трогай буфер, копи».
        """
        return self._local_ready or self.using_remote

    def wait_local_ready(self, timeout: Optional[float] = None) -> bool:
        """Блокирующее ожидание завершения фоновой загрузки (тесты/диагностика)."""
        t = self._local_load_thread
        if t is not None:
            t.join(timeout)
        return self._local_ready

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

    # ------------------------------------------------------------------ #
    # Загрузка модели — для обоих режимов.
    # ------------------------------------------------------------------ #
    def load_model(self, quiet: bool = False):
        """В remote-режиме — health-probe (НЕ грузит локаль), иначе грузит локаль."""
        if self.using_remote:
            ok = self.remote.healthz()
            if not quiet:
                state = "ready" if ok else "UNREACHABLE"
                print(
                    f"[FallbackASR] remote ASR {state} at {self.remote.base_url}",
                    file=sys.stderr,
                )
            return None
        return self._local.load_model(quiet)

    # ------------------------------------------------------------------ #
    # Dispatch: remote с circuit breaker'ом, локаль как резерв.
    # ------------------------------------------------------------------ #
    def _run(self, remote_call: Callable[[], Any], local_call: Callable[[], Any],
             *, background: bool = False) -> Any:
        """Общий dispatch: remote под breaker'ом; локаль — только если уже загружена.

        Решение remote-vs-local и запись success/failure epoch-защищены (см.
        ``_decide``), так что запоздавший success не отменяет свежий open и в
        half-open окне probe'ит ровно один поток.

        Если обслужить некому (remote упал/circuit открыт И локаль не готова) —
        поднимает ``ASRTemporarilyUnavailable``. Caller обязан сохранить данные
        и повторить позже; ни один путь больше не блокируется на load_model.
        """
        circuit = self._background_circuit if background else self._live_circuit
        try_remote, epoch = circuit._decide()
        if try_remote:
            try:
                out = remote_call()
                circuit._record_success(epoch)
                return out
            except (RemoteASRError, httpx.HTTPError) as exc:
                circuit._record_failure(epoch)
                log_rate_limited(
                    f"fallback-remote-fail-{circuit.scope}",
                    f"[FallbackASR] {circuit.scope} remote call failed: {type(exc).__name__}: {exc}",
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

    def transcribe_chunk(self, audio: np.ndarray, sample_rate: int = 16000) -> Optional[str]:
        """Один клип. На полном двустороннем сбое возвращает None (локаль best-effort)."""
        return self._run(
            lambda: self.remote.transcribe_chunk(audio, sample_rate),
            lambda: self._local.transcribe_chunk(audio, sample_rate),
        )

    def transcribe_batch(self, pcm_list: List[np.ndarray]) -> List[str]:
        """Per-speaker батч. Если локаль тоже падает — пере-поднимает её исключение."""
        return self._run(
            lambda: self.remote.transcribe_batch(pcm_list),
            lambda: self._local.transcribe_batch(pcm_list),
        )

    def transcribe_simple(self, audio_path: str, **kwargs: Any) -> List[Segment]:
        """Offline CLI. Если локаль тоже падает — пере-поднимает её исключение."""
        return self._run(
            lambda: self.remote.transcribe_simple(audio_path, **kwargs),
            lambda: self._local.transcribe_simple(audio_path, **kwargs),
            background=True,
        )

    def transcribe_with_timestamps(
        self,
        audio: Union[np.ndarray, Any, str],
        sample_rate: int = 16000,
        *,
        local_transcribe: Optional[Callable[["ASRModule"], Any]] = None,
    ) -> Any:
        """Refine. Если локаль тоже падает — пере-поднимает её исключение."""
        return self._run(
            lambda: self.remote.transcribe_with_timestamps(audio, sample_rate),
            lambda: (local_transcribe(self._local) if local_transcribe is not None
                     else self._local.transcribe_with_timestamps(audio, sample_rate)),
            background=True,
        )


def build_asr() -> "Union[ASRModule, FallbackASR, WavoASR]":
    """Единственный конструктор ASR для caller'ов.

    ``RECOG_ASR_BACKEND == "remote"`` -> ``FallbackASR`` поверх шлюза;
    ``"wavo"`` -> ``WavoASR`` (та же модель на Rust-рантайме wavo, GPU);
    иначе чистый локальный ``ASRModule`` (committed default — поведение
    репозитория не меняется, пока пользователь не выставит env).

    Raises:
        RuntimeError: backend ``remote``, но ``RECOG_ASR_REMOTE_URL`` пуст —
            адреса шлюза по умолчанию нет, и молча уходить на локальную
            модель нельзя: это разное потребление RAM и разная латентность.
    """
    from .install_config import INSTALL_CONFIG

    settings = INSTALL_CONFIG.asr
    if settings.backend == "remote":
        if not settings.remote_url.strip():
            raise RuntimeError(
                "RECOG_ASR_BACKEND=remote requires RECOG_ASR_REMOTE_URL"
            )
        remote = RemoteASRClient(
            settings.remote_url,
            settings.remote_model,
            settings.remote_timeout_s,
            live_timeout_s=settings.live_timeout_s,
        )
        return FallbackASR(
            remote,
            fail_threshold=settings.circuit_fails,
            open_seconds=settings.circuit_open_s,
        )
    if settings.backend == "wavo":
        from .wavo_backend import WavoASR

        return WavoASR()
    return _local_asr_module()
