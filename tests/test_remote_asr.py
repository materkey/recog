"""Unit tests for recog/remote_asr.py.

These tests MUST NOT hit the real network and MUST NOT load the real GigaAM
model. Two seams make that possible:

1. The HTTP layer is faked by monkeypatching ``httpx.Client`` inside the
   ``recog.remote_asr`` module namespace with ``_FakeHttpxClient`` — a
   stand-in that returns canned ``_FakeResponse`` objects and never opens a
   socket. The remote-word-parsing test drives the *real*
   ``RemoteASRClient.transcribe_with_timestamps`` through this fake so the
   seconds→int-ms conversion is exercised end to end.

2. The local backend is injected via
   ``FallbackASR(local_factory=lambda: FakeLocal())``. ``FakeLocal`` stubs
   every method the circuit breaker may delegate to and counts calls, so the
   circuit-routing tests assert *which* backend served each call without ever
   constructing the real ``ASRModule`` (which would pull in torch/gigaam).

Style mirrors tests/test_asr.py and tests/test_refine_fallback.py:
``__new__``-built stubs, given/when/then docstrings, MagicMock spies.
"""
from __future__ import annotations

import sys
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from gigaam.types import TranscriptionResult, Word

import recog.remote_asr as remote_asr_mod
from recog.remote_asr import (
    ASRTemporarilyUnavailable,
    FallbackASR,
    RemoteASRClient,
    RemoteASRError,
)


SAMPLE_RATE = 16000


# -----------------------------------------------------------------------------
# Fake local backend injected via FallbackASR(local_factory=...)
# -----------------------------------------------------------------------------


class FakeLocal:
    """Stand-in for ASRModule wired into FallbackASR via ``local_factory``.

    Constructing this is cheap (no model). It exposes every attribute and
    method the FallbackASR contract delegates to, returns canned values, and
    counts calls so tests can assert when the local path is actually hit.

    ``load_model`` must NOT be invoked until the first fallback (lazy load
    contract). ``model`` / ``device`` / ``_model_lock`` /
    ``_maybe_empty_mps_cache`` are passthrough surfaces required by existing
    callers + RefineWorker.
    """

    def __init__(self) -> None:
        # The eagerly-constructed local model object (RefineWorker reuses it).
        self.model = object()
        self.device = "cpu"
        self._model_lock = threading.RLock()
        self._loaded = False
        self.calls: dict[str, int] = {
            "load_model": 0,
            "transcribe_chunk": 0,
            "transcribe_batch": 0,
            "transcribe_simple": 0,
            "transcribe_with_timestamps": 0,
            "_maybe_empty_mps_cache": 0,
        }
        # Allow individual tests to make the local path raise.
        self.raise_on: set[str] = set()

    def load_model(self, quiet: bool = False) -> Any:
        self.calls["load_model"] += 1
        self._loaded = True
        return self.model

    def _maybe_empty_mps_cache(self) -> None:
        self.calls["_maybe_empty_mps_cache"] += 1

    def _maybe_raise(self, name: str) -> None:
        if name in self.raise_on:
            raise RuntimeError(f"local {name} boom")

    def transcribe_chunk(self, audio: np.ndarray, sample_rate: int = SAMPLE_RATE) -> Optional[str]:
        self.calls["transcribe_chunk"] += 1
        self._maybe_raise("transcribe_chunk")
        return "LOCAL_CHUNK"

    def transcribe_batch(self, pcm_list: list[np.ndarray]) -> list[str]:
        self.calls["transcribe_batch"] += 1
        self._maybe_raise("transcribe_batch")
        return [f"LOCAL_{i}" for i in range(len(pcm_list))]

    def transcribe_simple(self, audio_path: str, **kwargs: Any) -> list:
        self.calls["transcribe_simple"] += 1
        self._maybe_raise("transcribe_simple")
        from recog.models import Segment
        return [Segment(text="LOCAL_SIMPLE", start=0.0, end=1.0)]

    def transcribe_with_timestamps(self, audio: Any, sample_rate: int = SAMPLE_RATE) -> TranscriptionResult:
        self.calls["transcribe_with_timestamps"] += 1
        self._maybe_raise("transcribe_with_timestamps")
        return TranscriptionResult(text="LOCAL_TS", words=[Word(text="LOCAL_TS", start=0, end=1000)])


# -----------------------------------------------------------------------------
# Fake remote client injected directly as FallbackASR(remote=...)
# -----------------------------------------------------------------------------


class FakeRemote:
    """Stand-in for RemoteASRClient: each method returns a canned value or
    raises a queued exception. Counts calls so circuit-routing tests can
    assert remote was (or wasn't) consulted.

    ``fail_next`` is a count of upcoming calls that should raise
    RemoteASRError before the method starts succeeding again. This models a
    backend that is down for N calls and then recovers.
    """

    def __init__(self) -> None:
        self.calls: dict[str, int] = {
            "healthz": 0,
            "transcribe_chunk": 0,
            "transcribe_batch": 0,
            "transcribe_simple": 0,
            "transcribe_with_timestamps": 0,
        }
        self.fail_next = 0
        self.healthz_return = True

    def _maybe_fail(self) -> None:
        if self.fail_next > 0:
            self.fail_next -= 1
            raise RemoteASRError("remote down")

    def healthz(self) -> bool:
        self.calls["healthz"] += 1
        return self.healthz_return

    def transcribe_chunk(self, audio: np.ndarray, sample_rate: int = SAMPLE_RATE) -> str:
        self.calls["transcribe_chunk"] += 1
        self._maybe_fail()
        return "REMOTE_CHUNK"

    def transcribe_batch(self, pcm_list: list[np.ndarray]) -> list[str]:
        self.calls["transcribe_batch"] += 1
        self._maybe_fail()
        return [f"REMOTE_{i}" for i in range(len(pcm_list))]

    def transcribe_simple(self, audio_path: str, **kwargs: Any) -> list:
        self.calls["transcribe_simple"] += 1
        self._maybe_fail()
        from recog.models import Segment
        return [Segment(text="REMOTE_SIMPLE", start=0.0, end=1.0)]

    def transcribe_with_timestamps(self, audio: Any, sample_rate: int = SAMPLE_RATE) -> TranscriptionResult:
        self.calls["transcribe_with_timestamps"] += 1
        self._maybe_fail()
        return TranscriptionResult(text="REMOTE_TS", words=[Word(text="REMOTE_TS", start=0, end=1000)])


def _make_fallback(
    *,
    fail_threshold: int = 3,
    open_seconds: float = 60.0,
    remote: Optional[FakeRemote] = None,
    local: Optional[FakeLocal] = None,
) -> tuple[FallbackASR, FakeRemote, FakeLocal]:
    """Build a FallbackASR with fake remote + fake (lazily-loaded) local."""
    remote = remote or FakeRemote()
    local = local or FakeLocal()
    fb = FallbackASR(
        remote=remote,
        local_factory=lambda: local,
        fail_threshold=fail_threshold,
        open_seconds=open_seconds,
    )
    return fb, remote, local


def _gate_local_load(local: FakeLocal) -> tuple[threading.Event, threading.Event]:
    """Делает local.load_model блокирующимся до release.set().

    FakeLocal грузится мгновенно, поэтому фоновый поток _load_local_weights
    успевает выставить _local_ready=True до проверки в _run() — и вместо
    ASRTemporarilyUnavailable вызов отдаёт LOCAL_CHUNK. Для продакшена это
    правильное поведение (загрузилась — обслуживай), но тестам нужен
    детерминизм. Возвращает (started, release)."""
    started = threading.Event()
    release = threading.Event()
    real_load = local.load_model

    def gated_load(quiet=False):
        started.set()
        release.wait(timeout=5.0)
        return real_load(quiet)

    local.load_model = gated_load
    return started, release


# -----------------------------------------------------------------------------
# Fake httpx layer for exercising the REAL RemoteASRClient (no network)
# -----------------------------------------------------------------------------


@dataclass
class _FakeResponse:
    """Minimal httpx.Response stand-in: ``status_code``, ``json()``,
    ``text``, and a ``raise_for_status`` that mimics httpx semantics."""

    status_code: int = 200
    _json: Any = field(default_factory=dict)
    text: str = ""

    def json(self) -> Any:
        return self._json

    def raise_for_status(self) -> "_FakeResponse":
        if self.status_code >= 400:
            raise remote_asr_mod.httpx.HTTPStatusError(
                f"status {self.status_code}", request=None, response=None
            )
        return self


class _FakeHttpxClient:
    """Drop-in for ``httpx.Client`` used as a context manager.

    Captures the most recent POST/GET kwargs so the parsing test can assert
    the multipart form fields (model, response_format, timestamp_granularities)
    without parsing real multipart bodies. ``post_response`` / ``get_response``
    are the canned replies; if set to an Exception instance they are raised
    to simulate transport errors.
    """

    last_instance: Optional["_FakeHttpxClient"] = None

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        self.init_kwargs = kwargs
        self.posts: list[dict[str, Any]] = []
        self.gets: list[dict[str, Any]] = []
        self.post_response: Any = _FakeResponse(status_code=200, _json={"text": ""})
        self.get_response: Any = _FakeResponse(status_code=200, _json={"ok": True})
        type(self).last_instance = self

    def __enter__(self) -> "_FakeHttpxClient":
        return self

    def __exit__(self, *exc: Any) -> bool:
        return False

    def post(self, url: str, **kwargs: Any) -> _FakeResponse:
        self.posts.append({"url": url, **kwargs})
        if isinstance(self.post_response, Exception):
            raise self.post_response
        return self.post_response

    def get(self, url: str, **kwargs: Any) -> _FakeResponse:
        self.gets.append({"url": url, **kwargs})
        if isinstance(self.get_response, Exception):
            raise self.get_response
        return self.get_response

    def close(self) -> None:
        pass


# =============================================================================
# (a) happy remote path returns remote result, local NOT loaded
# =============================================================================


def test_happy_remote_path_returns_remote_result_and_does_not_load_local():
    """
    given: a FallbackASR with a healthy remote and a lazily-loaded local
    when: transcribe_chunk is called and remote succeeds
    then: the remote result is returned and local.load_model is never called
    """
    fb, remote, local = _make_fallback()

    result = fb.transcribe_chunk(np.zeros(SAMPLE_RATE, dtype=np.int16))

    assert result == "REMOTE_CHUNK"
    assert remote.calls["transcribe_chunk"] == 1
    assert local.calls["transcribe_chunk"] == 0
    assert local.calls["load_model"] == 0, "local model must stay unloaded on remote success"
    assert fb.using_remote is True


def test_repeated_remote_success_keeps_circuit_closed_and_local_cold():
    """
    given: a FallbackASR whose remote never fails
    when: many chunks are transcribed
    then: every call is served by remote and the local model is never loaded
    """
    fb, remote, local = _make_fallback()

    for _ in range(10):
        assert fb.transcribe_chunk(np.zeros(800, dtype=np.int16)) == "REMOTE_CHUNK"

    assert remote.calls["transcribe_chunk"] == 10
    assert local.calls["load_model"] == 0
    assert fb.using_remote is True


# =============================================================================
# (b) remote raises -> falls back to local, local result returned, mode-switch logged
# =============================================================================


def test_remote_failure_falls_back_to_local_and_logs_mode_switch(capsys):
    """
    given: a remote that raises RemoteASRError on the next two calls
    when: the first transcribe_chunk fails (unavailable + background load
          starts) and a second call is made after the load completes
    then: the second call is served locally, and a one-line mode-switch is
          logged to stderr exactly once
    """
    remote = FakeRemote()
    remote.fail_next = 2
    fb, remote, local = _make_fallback(remote=remote)
    started, release = _gate_local_load(local)
    audio = np.zeros(SAMPLE_RATE, dtype=np.int16)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    assert started.wait(timeout=2.0), "background load must start"
    release.set()
    assert fb.wait_local_ready(timeout=2.0), "background load must complete"

    result = fb.transcribe_chunk(audio)

    assert result == "LOCAL_CHUNK"
    assert remote.calls["transcribe_chunk"] == 2
    assert local.calls["transcribe_chunk"] == 1
    assert local.calls["load_model"] == 1, "local model loaded once, in background"

    err = capsys.readouterr().err
    assert err.count("loading local GigaAM in background") == 1, (
        f"expected exactly one mode-switch log line on stderr; got: {err!r}"
    )


def test_remote_failure_then_recovery_resets_circuit_and_returns_to_remote():
    """
    given: remote fails once (below fail_threshold) then recovers
    when: a failing transcribe_chunk (unavailable) is followed by another call
    then: the failing call raises ASRTemporarilyUnavailable (nothing served),
          the next call is served by remote again, failure counter reset
    """
    remote = FakeRemote()
    remote.fail_next = 1
    fb, remote, local = _make_fallback(remote=remote, fail_threshold=3)
    started, release = _gate_local_load(local)
    audio = np.zeros(800, dtype=np.int16)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    release.set()
    fb.wait_local_ready(timeout=2.0)

    second = fb.transcribe_chunk(audio)

    assert second == "REMOTE_CHUNK"
    assert remote.calls["transcribe_chunk"] == 2
    assert fb.using_remote is True
    assert fb._failures == 0


# =============================================================================
# (c) circuit opens after fail_threshold consecutive failures and routes
#     straight to local while open
# =============================================================================


def test_circuit_opens_after_fail_threshold_and_routes_straight_to_local(capsys):
    """
    given: a remote that fails on every call and fail_threshold == 3
    when: enough chunks are transcribed to cross the threshold, then more
    when: the circuit is open
    then: post-threshold calls do NOT touch remote at all (straight to local)
          and an OPEN-circuit log line is emitted
    """
    remote = FakeRemote()
    remote.fail_next = 100  # never recovers
    fb, remote, local = _make_fallback(remote=remote, fail_threshold=3, open_seconds=60.0)
    started, release = _gate_local_load(local)
    audio = np.zeros(800, dtype=np.int16)

    # 3 failing calls cross the threshold (each still consults remote once).
    # Локаль грузится в фоне (заблокирована гейтом) — каждый сбой даёт unavailable.
    for _ in range(3):
        with pytest.raises(ASRTemporarilyUnavailable):
            fb.transcribe_chunk(audio)

    assert remote.calls["transcribe_chunk"] == 3
    assert fb.using_remote is False, "circuit must be OPEN after fail_threshold failures"
    release.set()
    assert fb.wait_local_ready(timeout=2.0), "background load must complete"

    # Further calls while OPEN must bypass remote entirely (served locally).
    remote_calls_before = remote.calls["transcribe_chunk"]
    for _ in range(5):
        assert fb.transcribe_chunk(audio) == "LOCAL_CHUNK"
    assert remote.calls["transcribe_chunk"] == remote_calls_before, (
        "open circuit must route straight to local without consulting remote"
    )
    assert local.calls["transcribe_chunk"] == 5

    err = capsys.readouterr().err
    assert "OPEN" in err, f"expected an OPEN-circuit log line; got: {err!r}"


# =============================================================================
# (d) half-open probe closes the circuit on success
# =============================================================================


def test_half_open_probe_closes_circuit_on_success(monkeypatch):
    """
    given: an OPEN circuit whose open_seconds window has elapsed and a remote
           that has since recovered
    when: the next transcribe_chunk is called
    then: exactly one remote probe is allowed, it succeeds, the circuit closes,
          and subsequent calls go to remote again
    """
    # Drive time.monotonic so the open window can be advanced deterministically.
    clock = {"t": 1000.0}
    monkeypatch.setattr(remote_asr_mod.time, "monotonic", lambda: clock["t"])

    remote = FakeRemote()
    remote.fail_next = 3  # exactly enough to open with fail_threshold=3
    fb, remote, local = _make_fallback(remote=remote, fail_threshold=3, open_seconds=60.0)
    audio = np.zeros(800, dtype=np.int16)

    for _ in range(3):
        try:
            fb.transcribe_chunk(audio)
        except ASRTemporarilyUnavailable:
            pass
    assert fb.using_remote is False  # circuit OPEN
    assert fb.wait_local_ready(timeout=2.0)
    remote_calls_open = remote.calls["transcribe_chunk"]

    # Still inside the open window: no probe (served locally).
    clock["t"] += 30.0
    assert fb.transcribe_chunk(audio) == "LOCAL_CHUNK"
    assert remote.calls["transcribe_chunk"] == remote_calls_open, "no probe before open_seconds elapse"

    # Window elapsed -> half-open: one probe allowed. Remote has recovered.
    clock["t"] += 31.0  # total elapsed 61s > 60s
    result = fb.transcribe_chunk(audio)

    assert result == "REMOTE_CHUNK", "half-open probe served by recovered remote"
    assert remote.calls["transcribe_chunk"] == remote_calls_open + 1
    assert fb.using_remote is True, "successful probe must CLOSE the circuit"

    # Circuit closed: next call goes straight to remote.
    assert fb.transcribe_chunk(audio) == "REMOTE_CHUNK"


def test_half_open_probe_failure_reopens_circuit(monkeypatch):
    """
    given: an OPEN circuit whose window has elapsed but remote is still down
    when: the half-open probe is attempted and fails
    then: the call falls back to local and the circuit re-opens (using_remote
          stays False)
    """
    clock = {"t": 5000.0}
    monkeypatch.setattr(remote_asr_mod.time, "monotonic", lambda: clock["t"])

    remote = FakeRemote()
    remote.fail_next = 100  # never recovers
    fb, remote, local = _make_fallback(remote=remote, fail_threshold=3, open_seconds=60.0)
    audio = np.zeros(800, dtype=np.int16)

    for _ in range(3):
        try:
            fb.transcribe_chunk(audio)
        except ASRTemporarilyUnavailable:
            pass
    assert fb.using_remote is False
    assert fb.wait_local_ready(timeout=2.0)

    clock["t"] += 61.0  # window elapsed -> one probe allowed
    result = fb.transcribe_chunk(audio)

    assert result == "LOCAL_CHUNK", "failed probe falls back to local (already loaded)"
    assert fb.using_remote is False, "failed half-open probe must RE-OPEN the circuit"


# =============================================================================
# (e) transcribe_batch preserves order and any-item-failure triggers
#     whole-batch local fallback
# =============================================================================


def test_transcribe_batch_remote_success_preserves_order():
    """
    given: a healthy remote and a 3-item PCM batch
    when: transcribe_batch is called
    then: remote serves it, the result order matches the input order, and the
          local backend is never loaded
    """
    fb, remote, local = _make_fallback()

    batch = [
        np.zeros(8000, dtype=np.int16),
        np.zeros(16000, dtype=np.int16),
        np.zeros(24000, dtype=np.int16),
    ]
    texts = fb.transcribe_batch(batch)

    assert texts == ["REMOTE_0", "REMOTE_1", "REMOTE_2"]
    assert remote.calls["transcribe_batch"] == 1
    assert local.calls["transcribe_batch"] == 0
    assert local.calls["load_model"] == 0


def test_transcribe_batch_any_item_failure_falls_back_whole_batch_to_local():
    """
    given: a remote whose transcribe_batch raises RemoteASRError (the contract
           requires it to raise if ANY item fails)
    when: transcribe_batch is called on a 3-item batch
    then: the WHOLE batch is re-transcribed by the local backend and the
          local result (order-preserving) is returned
    """
    remote = FakeRemote()
    remote.fail_next = 2  # обе попытки remote batch падают
    fb, remote, local = _make_fallback(remote=remote)
    started, release = _gate_local_load(local)

    batch = [np.zeros(800, dtype=np.int16) for _ in range(3)]

    # Первый сбой: unavailable (локаль грузится в фоне), батч сохраняет caller.
    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_batch(batch)
    release.set()
    assert fb.wait_local_ready(timeout=2.0)

    # Повторный диспатч того же батча: remote снова падает -> локаль.
    texts = fb.transcribe_batch(batch)

    assert texts == ["LOCAL_0", "LOCAL_1", "LOCAL_2"], "whole batch must be served locally, in order"
    assert remote.calls["transcribe_batch"] == 2
    assert local.calls["transcribe_batch"] == 1
    assert local.calls["load_model"] == 1


# =============================================================================
# (f) transcribe_with_timestamps remote-word parsing: seconds -> int ms
# =============================================================================


def test_transcribe_with_timestamps_remote_parses_seconds_to_int_ms(monkeypatch):
    """
    given: the gateway returns top-level words[] with float-second start/end
    when: the REAL RemoteASRClient.transcribe_with_timestamps is driven through
          a faked httpx.Client (no network)
    then: a TranscriptionResult is returned whose Words carry text=word and
          start/end converted to INT milliseconds (round(sec * 1000)), and the
          POST carries the word-granularity form field
    """
    monkeypatch.setattr(remote_asr_mod.httpx, "Client", _FakeHttpxClient)

    payload = {
        "text": "раз два",
        "words": [
            {"word": "раз", "start": 0.8, "end": 1.12},
            {"word": "два", "start": 1.164, "end": 1.36},
        ],
    }
    # RemoteASRClient.__init__ builds its httpx.Client eagerly; the fake records
    # itself as last_instance, so arm the canned POST on that instance.
    client = RemoteASRClient(base_url="https://example.test/asr", model="gigaam")
    _FakeHttpxClient.last_instance.post_response = _FakeResponse(status_code=200, _json=payload)

    result = client.transcribe_with_timestamps(np.zeros(SAMPLE_RATE, dtype=np.int16))

    assert isinstance(result, TranscriptionResult)
    assert result.text == "раз два"
    assert [w.text for w in result.words] == ["раз", "два"]

    assert result.words[0].start == 800
    assert result.words[0].end == 1120
    assert result.words[1].start == 1164  # round(1.164 * 1000)
    assert result.words[1].end == 1360
    for w in result.words:
        assert isinstance(w.start, int)
        assert isinstance(w.end, int)

    # The POST must request word-level timestamps via the extra form field.
    inst = _FakeHttpxClient.last_instance
    assert inst is not None and inst.posts, "expected at least one POST through the fake client"
    last_post = inst.posts[-1]
    form_blob = repr(last_post.get("data", {})) + repr(last_post.get("files", {}))
    assert "word" in form_blob, (
        "POST must carry timestamp_granularities=word so the gateway returns word ts"
    )


def test_transcribe_with_timestamps_empty_words_returns_empty_list(monkeypatch):
    """
    given: the gateway returns text but no words[] (silent / segment-only)
    when: RemoteASRClient.transcribe_with_timestamps is driven through fake httpx
    then: a TranscriptionResult with words == [] (not None) is returned and the
          text passes through
    """
    monkeypatch.setattr(remote_asr_mod.httpx, "Client", _FakeHttpxClient)

    payload = {"text": "тишина", "words": []}
    client = RemoteASRClient(base_url="https://example.test/asr")
    _FakeHttpxClient.last_instance.post_response = _FakeResponse(status_code=200, _json=payload)

    result = client.transcribe_with_timestamps(np.zeros(SAMPLE_RATE, dtype=np.int16))

    assert result.text == "тишина"
    assert result.words == []


def test_word_ts_wire_contract_backend_emit_to_client_parse(monkeypatch):
    """
    given: the gateway/backend word-timestamp wire contract — top-level words[]
           of {word, start(FLOAT sec), end(FLOAT sec)} under
           response_format=verbose_json + timestamp_granularities=word
    when: a backend-shaped payload (the exact dict deploy/z270xx/gigaam_backend.py
          builds from lf.words: {"word": w.text, "start": float(w.start),
          "end": float(w.end)}) is driven through the REAL
          RemoteASRClient.transcribe_with_timestamps via faked httpx
    then: each Word arrives as text + start/end in INT ms (round(sec*1000)), and
          the client POST declares response_format=verbose_json with a
          timestamp_granularities form field containing "word"

    Pins the gigaam_backend.py -> gateway.py -> remote_asr.py word contract: if
    the backend renames a key, switches seconds->ms, or the client stops sending
    the word-granularity form field / verbose_json, this fails loudly.
    """
    monkeypatch.setattr(remote_asr_mod.httpx, "Client", _FakeHttpxClient)

    # Backend emit shape verbatim from deploy/z270xx/gigaam_backend.py.
    backend_words = [
        {"word": "контракт", "start": 0.5, "end": 1.0},
        {"word": "слов", "start": 1.0, "end": 1.5},
    ]
    payload = {"text": "контракт слов", "words": backend_words}

    client = RemoteASRClient(base_url="https://example.test/asr", model="gigaam")
    _FakeHttpxClient.last_instance.post_response = _FakeResponse(
        status_code=200, _json=payload
    )

    result = client.transcribe_with_timestamps(np.zeros(SAMPLE_RATE, dtype=np.int16))

    # Word keys + seconds->int-ms conversion (the load-bearing contract).
    assert [w.text for w in result.words] == ["контракт", "слов"]
    assert result.words[0].start == 500 and result.words[0].end == 1000
    assert result.words[1].start == 1000 and result.words[1].end == 1500
    assert all(isinstance(w.start, int) and isinstance(w.end, int) for w in result.words)

    # Request side: response_format=verbose_json AND a word-granularity field —
    # drop either and the gateway returns only {text}, silently killing refine.
    inst = _FakeHttpxClient.last_instance
    sent = inst.posts[-1].get("data", {})
    assert sent.get("response_format") == "verbose_json"
    assert "word" in str(sent.get("timestamp_granularities", "")).lower()


# =============================================================================
# RemoteASRClient HTTP-error mapping (no network) + healthz
# =============================================================================


def test_transcribe_chunk_http_error_raises_remote_asr_error(monkeypatch):
    """
    given: the gateway returns HTTP 500 for response_format=json
    when: RemoteASRClient.transcribe_chunk is called through fake httpx
    then: RemoteASRError is raised (the FallbackASR contract relies on this to
          trigger local fallback)
    """
    monkeypatch.setattr(remote_asr_mod.httpx, "Client", _FakeHttpxClient)

    client = RemoteASRClient(base_url="https://example.test/asr")
    _FakeHttpxClient.last_instance.post_response = _FakeResponse(
        status_code=500, _json={}, text="boom"
    )

    with pytest.raises(RemoteASRError):
        client.transcribe_chunk(np.zeros(SAMPLE_RATE, dtype=np.int16))


def test_transcribe_chunk_transport_error_raises_remote_asr_error(monkeypatch):
    """
    given: httpx raises a ConnectError (gateway unreachable)
    when: RemoteASRClient.transcribe_chunk is called
    then: it is wrapped/surfaced as RemoteASRError, never an httpx error
    """
    monkeypatch.setattr(remote_asr_mod.httpx, "Client", _FakeHttpxClient)

    client = RemoteASRClient(base_url="https://example.test/asr")
    _FakeHttpxClient.last_instance.post_response = remote_asr_mod.httpx.ConnectError("no route")

    with pytest.raises(RemoteASRError):
        client.transcribe_chunk(np.zeros(SAMPLE_RATE, dtype=np.int16))


def test_healthz_true_only_when_reachable_and_ok(monkeypatch):
    """
    given: GET /healthz returns {ok: True}
    when: RemoteASRClient.healthz is called through fake httpx
    then: it returns True; on a transport error it returns False (never raises)
    """
    monkeypatch.setattr(remote_asr_mod.httpx, "Client", _FakeHttpxClient)

    client = RemoteASRClient(base_url="https://example.test/asr")
    _FakeHttpxClient.last_instance.get_response = _FakeResponse(status_code=200, _json={"ok": True})
    assert client.healthz() is True

    client2 = RemoteASRClient(base_url="https://example.test/asr")
    _FakeHttpxClient.last_instance.get_response = remote_asr_mod.httpx.ConnectError("down")
    assert client2.healthz() is False


def test_healthz_parses_nested_per_engine_aggregator_shape(monkeypatch):
    """
    given: the gateway aggregator nests health under per-engine keys
           {"gigaam": {"ok": true}, "qwen": {"error": ...}} (no top-level "ok")
    when: RemoteASRClient(model="gigaam").healthz is called
    then: it reports True off the gigaam engine even though qwen is down
          (the deployed tailscale-serve gateway uses this nested shape)
    """
    monkeypatch.setattr(remote_asr_mod.httpx, "Client", _FakeHttpxClient)

    client = RemoteASRClient(base_url="https://example.test/asr", model="gigaam")
    _FakeHttpxClient.last_instance.get_response = _FakeResponse(
        status_code=200,
        _json={"gigaam": {"ok": True, "variant": "v3_e2e_rnnt"}, "qwen": {"error": "down"}},
    )
    assert client.healthz() is True

    # gigaam engine itself down -> False
    client2 = RemoteASRClient(base_url="https://example.test/asr", model="gigaam")
    _FakeHttpxClient.last_instance.get_response = _FakeResponse(
        status_code=200, _json={"gigaam": {"ok": False}, "qwen": {"ok": False}}
    )
    assert client2.healthz() is False


# =============================================================================
# FallbackASR passthrough surfaces (RefineWorker + existing callers)
# =============================================================================


def test_fallback_exposes_local_lock_model_and_device_passthrough():
    """
    given: a FallbackASR over an eagerly-constructed FakeLocal
    when: model / device / _model_lock / _maybe_empty_mps_cache are accessed
    then: they are the SAME objects the local backend owns (RefineWorker
          acquires _model_lock externally, so identity must hold)
    """
    fb, _remote, local = _make_fallback()

    assert fb.model is local.model
    assert fb.device == local.device
    assert fb._model_lock is local._model_lock

    fb._maybe_empty_mps_cache()
    assert local.calls["_maybe_empty_mps_cache"] == 1


def test_load_model_in_remote_mode_healthchecks_without_loading_local():
    """
    given: a FallbackASR whose circuit is closed (remote active)
    when: load_model(quiet=True) is called (the live-setup path)
    then: remote.healthz is consulted, the local model is NOT loaded, and None
          is returned (the contract for remote readiness probing)
    """
    fb, remote, local = _make_fallback()

    returned = fb.load_model(quiet=True)

    assert returned is None
    assert remote.calls["healthz"] >= 1
    assert local.calls["load_model"] == 0, "remote-mode load_model must not load local"


# =============================================================================
# Non-blocking fallback (docs/plans/2026-06-02-asr-fallback-buffering-design.md)
# =============================================================================

from recog.remote_asr import ASRTemporarilyUnavailable


def test_first_remote_failure_raises_unavailable_and_starts_background_load():
    """
    given: remote падает, локальная модель не загружена
    when: transcribe_chunk
    then: ASRTemporarilyUnavailable + фоновый поток загрузки запущен
          (вызывающий поток НЕ блокируется на load_model)
    """
    fb, remote, local = _make_fallback()
    started, release = _gate_local_load(local)
    remote.fail_next = 1
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)

    assert started.wait(timeout=2.0), "background load thread must start"
    release.set()
    assert fb.wait_local_ready(timeout=2.0) is True
    assert local.calls["load_model"] == 1


def test_after_background_load_local_serves_calls():
    """
    given: circuit открыт, фоновая загрузка завершена
    when: transcribe_chunk
    then: LOCAL_CHUNK без обращения к remote
    """
    fb, remote, local = _make_fallback(fail_threshold=1)
    started, release = _gate_local_load(local)
    remote.fail_next = 1
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)  # сбой + открытие circuit + старт загрузки
    release.set()
    assert fb.wait_local_ready(timeout=2.0)

    remote_calls_before = remote.calls["transcribe_chunk"]
    assert fb.transcribe_chunk(audio) == "LOCAL_CHUNK"
    assert remote.calls["transcribe_chunk"] == remote_calls_before  # remote не трогали


def test_available_false_while_circuit_open_and_local_cold():
    """
    given: circuit открыт, локаль не готова (загрузка заблокирована)
    then: available == False (потребители не вычитывают буферы)
    """
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
    """
    given: свежий FallbackASR (circuit закрыт, локаль холодная)
    then: available == True (remote presumed alive)
    """
    fb, _, _ = _make_fallback()
    assert fb.available is True


def test_failed_local_load_logged_and_not_retried_within_backoff(capsys):
    """
    given: load_model локали кидает исключение
    when: повторный сбой remote внутри окна LOCAL_LOAD_RETRY_S
    then: загрузка НЕ перезапускается (backoff), ошибка залогирована
    """
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

    thread_before = fb._local_load_thread
    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    assert fb._local_load_thread is thread_before  # новый поток не создан


def test_circuit_close_logged_on_remote_recovery(capsys, monkeypatch):
    """
    given: circuit открыт после сбоев
    when: окно истекло и probe удался
    then: '[FallbackASR] remote recovered' в stderr
    """
    clock = {"t": 1000.0}
    monkeypatch.setattr(remote_asr_mod.time, "monotonic", lambda: clock["t"])

    fb, remote, local = _make_fallback(fail_threshold=1, open_seconds=60.0)
    started, release = _gate_local_load(local)
    remote.fail_next = 1
    audio = np.zeros(SAMPLE_RATE, dtype=np.float32)

    with pytest.raises(ASRTemporarilyUnavailable):
        fb.transcribe_chunk(audio)
    release.set()
    fb.wait_local_ready(timeout=2.0)
    capsys.readouterr()

    clock["t"] += 61.0  # окно истекло -> probe
    assert fb.transcribe_chunk(audio) == "REMOTE_CHUNK"
    assert "remote recovered" in capsys.readouterr().err


def test_live_timeout_passed_to_chunk_requests():
    """
    given: RemoteASRClient с live_timeout_s=5
    when: transcribe_chunk
    then: POST вызывается с timeout=5 (а не дефолтным таймаутом клиента)
    """
    client = RemoteASRClient(
        base_url="https://example.test/asr", model="gigaam",
        timeout_s=15.0, live_timeout_s=5.0,
    )
    fake = _FakeHttpxClient()
    fake.post_response = _FakeResponse(status_code=200, _json={"text": "ok"})
    client._client = fake

    client.transcribe_chunk(np.zeros(SAMPLE_RATE, dtype=np.float32))

    assert fake.posts[0]["timeout"] == 5.0
