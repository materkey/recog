## Context

GigaAM runs locally across four inference surfaces in recog, all sharing one model instance under `ASRModule._model_lock` (an `RLock` so `RefineWorker` can acquire it externally while `transcribe_with_timestamps` re-acquires it internally, ADR-0019):

1. `ASRModule.transcribe_batch(pcm_list)` — live per-speaker batched forward (ADR-0018). Pads to `T_max`, one `model.forward` + `decode`, returns `List[str]` in input order.
2. `ASRModule.transcribe_simple(audio_path, ...)` — offline CLI chunked transcription, returns `List[Segment]`.
3. `ASRModule.transcribe_with_timestamps(audio, sample_rate=16000)` — refine contour (ADR-0019). Wraps `model.transcribe(path, word_timestamps=True)` and normalizes each `Word.start/end` to **int milliseconds in place**, returning the `TranscriptionResult`.
4. `LiveTranscriber._transcribe_direct(audio_chunk)` — direct `model.forward()` + `decode[0][0]` for mic / mixed-fallback (`recog/live.py:441-479`). No public `ASRModule` method today; the body is inlined in `LiveTranscriber`.

Z270XX (RTX PRO 6000 Blackwell, 96 GiB) fronts GigaAM v3 behind an OpenAI-Whisper-compatible gateway. Verified live:

- `GET /healthz` → `{"gigaam":{"ok":true,"variant":"v3_e2e_rnnt","diarization":true}, ...}`.
- `GET /v1/models` → aliases including `gigaam`, `gigaam-v3-e2e-rnnt`.
- `POST /v1/audio/transcriptions` multipart: `file`, `model` (default `gigaam`), `language`, `response_format`, `prompt`, `temperature`. `response_format=json → {text}`; `verbose_json → {task, language, duration, text, segments:[{id, start(sec), end(sec), text, speaker, ...}]}`.
- RTF ~99×; LAN RTT min 3.858 ms.

**Gap:** the gateway has no word-level timestamps. Its backend uses `model.transcribe_longform(wav)` (segment-level only). The refine path (#3) needs word timestamps to align refined text onto live segments; the local contract returns `Word.start/end` in **int ms**. The gateway must be extended to match, or refine cannot be offloaded.

**Constraints:**
- Committed default must leave repository behaviour unchanged (`RECOG_ASR_BACKEND=local`).
- Remote structures must be byte-compatible with the local `ASRModule` so downstream (refine alignment, `segment_id` contract, AI summary) is untouched.
- Network and remote GPU are external; remote failure must not crash a live meeting.
- One repository: the gateway patch and deploy steps live in `deploy/z270xx/`, not a separate `asr-api` repo.

## Goals / Non-Goals

**Goals:**
- Offload all four GigaAM paths to Z270XX behind one constructor `build_asr()`.
- Lazy local fallback: do not load the local model until the first remote failure.
- Circuit breaker so a dead remote fails fast and re-probes on a schedule.
- Extend the gateway in-repo to emit word-level timestamps for refine.
- Opt-in via `RECOG_ASR_BACKEND`; default `local` preserves current behaviour exactly.

**Non-Goals:**
- Changing live `chunk_duration` (3.0 s) or refine window (20 s).
- Swapping GigaAM for Whisper / Qwen3-ASR.
- A separate `asr-api` repository.
- Async ASR clients.
- Default-on offload.
- Using the gateway's diarization (recog owns speaker attribution).

## Decisions

### D1. `build_asr()` is the only ASR constructor

**Choice:** `recog/remote_asr.py::build_asr() -> ASRModule | FallbackASR` reads `recog.config`. When `RECOG_ASR_BACKEND == "remote"` it returns `FallbackASR(RemoteASRClient(RECOG_ASR_REMOTE_URL, RECOG_ASR_REMOTE_MODEL, RECOG_ASR_REMOTE_TIMEOUT_S), fail_threshold=RECOG_ASR_CIRCUIT_FAILS, open_seconds=RECOG_ASR_CIRCUIT_OPEN_S)`. Otherwise it returns a plain `ASRModule()`. `LiveTranscriber.__init__` and the offline CLI in `mp4_notes.py` both construct ASR exclusively through `build_asr()`.

**Why:** a single branch point means every caller sees one interface. No call site needs to know whether inference is local or remote. `FallbackASR` is structurally interchangeable with `ASRModule` (same method names + the delegated attributes in D4), so the swap is invisible to `LiveTranscriber`, `RefineWorker`, and the CLI.

**Alternative — per-call-site env checks:** rejected. Scatters the local/remote decision across `live.py`, `refine.py`, and `mp4_notes.py`; easy to miss a path and silently leave it local.

### D2. Offload all four paths; extract `transcribe_chunk` for the fourth

**Choice:** offload `transcribe_batch`, `transcribe_simple`, `transcribe_with_timestamps`, and the direct `model.forward()` mic/mixed-fallback. The fourth has no public method, so extract the forward+decode body from `LiveTranscriber._transcribe_direct` (`recog/live.py:466-475`) into:

```
ASRModule.transcribe_chunk(self, audio: np.ndarray, sample_rate: int = 16000) -> Optional[str]
```

It calls `load_model(quiet=True)`, moves the array to device/dtype, then under `self._model_lock`, `torch.inference_mode()`, and `_mps_autoreleasepool(self.device)` runs `model.forward` + `decode[0][0]`, calls `_maybe_empty_mps_cache()`, and returns `text.strip()` or `None`. It returns `None` on any exception (best-effort, matching the current direct path). `_transcribe_direct` keeps its `refine_worker.set_live_pending()/clear_live_pending()` try/finally wrapper and its outer try/except, and inside simply `return self.asr.transcribe_chunk(audio_chunk)`.

**Why offload refine too (user decision):** refine is a major share of inference load and the main competitor for the live mutex (ADR-0019 D6). Leaving it local would forfeit half the benefit. The price — extending the gateway for word timestamps — is paid once in `deploy/z270xx/` (D7).

**Why extract a method rather than offload inline:** `RemoteASRClient` and `FallbackASR` must expose the same `transcribe_chunk` name so `_transcribe_direct` is backend-agnostic. Extracting also de-duplicates the forward/decode pattern that already appears in `transcribe_batch` and `transcribe_simple`.

### D3. `RemoteASRClient` over the OpenAI-Whisper gateway

**Choice:** sync `httpx.Client` (standardize on httpx even though requests/aiohttp are present). Audio is serialized to an in-memory WAV via `soundfile.write(BytesIO, float32_array, sr, format="WAV", subtype="FLOAT")`.

```
class RemoteASRError(Exception): pass

class RemoteASRClient:
    __init__(self, base_url, model="gigaam", timeout_s=15.0, max_workers=8)
    healthz() -> bool
    transcribe_chunk(audio, sample_rate=16000) -> str
    transcribe_batch(pcm_list) -> list[str]
    transcribe_simple(audio_path, **kwargs) -> list[Segment]
    transcribe_with_timestamps(audio, sample_rate=16000) -> TranscriptionResult
```

| method | gateway call | response | mapping |
|---|---|---|---|
| `healthz` | `GET /healthz` | `{ok, variant, diarization}` | `True` only if reachable AND `ok` |
| `transcribe_chunk` | `POST` multipart, `response_format=json` | `{text}` | return `text`; raise `RemoteASRError` on any HTTP/parse failure |
| `transcribe_batch` | `ThreadPoolExecutor(max_workers)` over `transcribe_chunk` | — | **order-preserving**; if ANY item raises → raise `RemoteASRError` (caller falls back to local for the whole batch) |
| `transcribe_simple` | `POST file`, `response_format=verbose_json` | `{segments:[{start,end,text}]}` | `segments[]` → `recog.models.Segment(text, start, end)` |
| `transcribe_with_timestamps` | `POST WAV` + form `timestamp_granularities="word"` | top-level `words:[{word,start(sec),end(sec)}]` | `Word(text=word, start=int(round(start*1000)), end=int(round(end*1000)))` → `TranscriptionResult(text=..., words=[...])` |

`transcribe_with_timestamps` accepts `np.ndarray` / `torch.Tensor` / str path and normalizes int16→float32 exactly like `ASRModule.transcribe_with_timestamps`. It returns `Word.start/end` in **int ms**, so the refine alignment in `LiveTranscriber._apply_refine` consumes remote and local results identically.

**Why no batched-forward on remote:** the gateway accepts one `file` per request. `transcribe_batch` parallelizes independent `transcribe_chunk` calls over a `ThreadPoolExecutor(max_workers)`, preserving input order via `executor.map` / index-keyed futures. At 99× RTF + LAN this is acceptable; on any failure the whole batch falls back to the local padded forward (D4 contract).

**Why order-preserving + all-or-nothing:** `transcribe_batch` callers (`PerSpeakerIngest`) zip results back to pids positionally. A partial result would misattribute. Raising `RemoteASRError` on any item lets `FallbackASR` re-run the entire batch locally where the contract is guaranteed.

### D4. `FallbackASR` — lazy local load + circuit breaker

**Choice:**

```
class FallbackASR:
    __init__(self, remote, local_factory=ASRModule, fail_threshold=3, open_seconds=60.0)
```

- **Eager construct, lazy load.** `self._local = local_factory()` is built immediately in `__init__`. `ASRModule.__init__` is cheap — it does NOT load the model. `self._local.load_model()` is NOT called until the first fallback.
- **Circuit state** guarded by `threading.Lock`, timed with `time.monotonic()`.
- **`using_remote -> bool`** property: `True` when the circuit is closed or half-open (the next call will try remote).
- **Delegated attributes** (so existing callers + `RefineWorker` keep working): `model -> self._local.model`, `device -> self._local.device`, `_model_lock = self._local._model_lock` (the **same** lock object — `RefineWorker` acquires it externally), `_maybe_empty_mps_cache() -> self._local._maybe_empty_mps_cache()`.
- **`load_model(quiet=False)`:** if `using_remote` → call `remote.healthz()` (log readiness), do NOT load local, return `None`. Else (fallback / local active) → `self._local.load_model(quiet)` and return the local model.
- **`_dispatch` pattern** for `transcribe_chunk` / `transcribe_batch` / `transcribe_simple` / `transcribe_with_timestamps`:
  - Circuit **closed/half-open** → try `remote.<m>(...)`. On `RemoteASRError` / httpx error → `_record_failure()`, log a one-line mode-switch on first fallback, ensure local loaded (`self._local.load_model(quiet=True)`), call `local.<m>(...)`. On remote success → `_record_success()` (reset).
  - Circuit **OPEN** → straight to local.
  - **Half-open:** when `open_seconds` elapsed, allow ONE remote probe; success closes the circuit, failure re-opens it.
- **`_record_failure`:** `failures += 1`; if `failures >= fail_threshold` → `opened_until = monotonic() + open_seconds`; log `[FallbackASR] remote circuit OPEN for {open_seconds}s after {failures} fail(s)`.
- **Error contract:** `transcribe_chunk` returns `None` on total failure (both sides) — matches the existing `_transcribe_direct` contract. `transcribe_batch` / `transcribe_simple` / `transcribe_with_timestamps` re-raise the LOCAL exception if local also fails. A fallback where both sides failed is NEVER swallowed silently.

**Why eager-construct / lazy-load:** `_model_lock`, `model`, `device` must exist as real attributes the moment `FallbackASR` is built, because `RefineWorker` and `_transcribe_direct` read them at wiring time. Constructing `ASRModule` is free (no model). But loading 1.6 GB while remote is healthy would defeat the offload — so `load_model()` is deferred to the first actual fallback.

**Why the same `_model_lock` object:** `RefineWorker` acquires `asr._model_lock` externally to serialize with the live path (ADR-0019). If `FallbackASR` had its own lock, the local fallback's `forward` and the worker would race. Exposing `self._local._model_lock` keeps the single-lock invariant.

**Alternative — separate breaker per method:** rejected. A dead remote is dead for all methods; one breaker is simpler and fails over all paths together.

### D5. `_refine_transcribe_fn` must skip the decoder monkey-patch in remote mode

**Choice:** in `LiveTranscriber._refine_transcribe_fn` (`recog/live.py:807-829`), guard the cooperative-abort decoder monkey-patch:

```
if getattr(self.asr, "using_remote", False):
    return self.asr.transcribe_with_timestamps(pcm_samples)   # no load_model, no monkey-patch
else:
    # existing local path: load_model + make_abortable_decode + try/finally restore
```

**Why:** `make_abortable_decode` mutates `model.decoding.decode` on the local model. In remote mode there is no loaded local model (lazy), and `self.asr.model` is `None` — calling `load_model()` would force a pointless local load, and patching `None.decoding` would crash. Cooperative abort is a local-mutex-contention optimization (ADR-0019 D6); the remote backend has no shared local mutex, so the patch is meaningless there. `using_remote` is `True` while the circuit will try remote; if a fallback happens mid-call, `RemoteASRClient.transcribe_with_timestamps` raises and `FallbackASR` re-runs locally — but that local run is a plain decode without the abort hook (acceptable: refine is not preempting anything when remote is the primary path).

### D6. `_model_lock` stays an `RLock`; remote calls do not hold it

**Choice:** keep `ASRModule._model_lock` as a `threading.RLock` (ADR-0019). `FallbackASR` exposes the same object. Remote dispatch does NOT acquire `_model_lock` — there is no shared local tensor state for a network call. Only the local fallback path (inside `local.<m>(...)`, which re-acquires the lock internally) touches it.

**Why:** holding the lock across a 15 s network timeout would serialize live and refine on the network round-trip — the opposite of the offload's intent. The lock guards local `model.forward`; remote calls have no such resource. The lock is still needed (and still an `RLock`) for the fallback path where `RefineWorker` acquires it externally and `transcribe_with_timestamps` re-acquires it internally.

### D7. Gateway extended in-repo for word timestamps

**Choice:** `deploy/z270xx/gigaam_backend.py` is a patched copy of the current Z270XX backend. It adds an optional form field `timestamp_granularities` (string; `"word"` enables word timestamps). When word timestamps are requested, the backend calls `state["model"].transcribe(wav_path, word_timestamps=True)` (returns `TranscriptionResult` with `.text` and `.words[Word(text, start_sec, end_sec)]`) instead of `transcribe_longform`, and includes a top-level `"words": [{"word": w.text, "start": float(w.start), "end": float(w.end)}]` in the JSON response alongside `text`. All existing behaviour (json / verbose_json / diarized / srt / vtt, ffmpeg transcode) is intact when word ts is not requested.

`deploy/z270xx/README.md` documents: scp target `~/projects/asr-api/gigaam_backend.py` on Z270XX, restart `systemctl --user restart asr-gigaam`, curl smoke test that the response carries `words` when `timestamp_granularities=word` is sent.

**Why in-repo (user decision):** the client (`recog/remote_asr.py`) and the gateway contract it depends on must version together. A separate `asr-api` repo would let the two drift; a deploy artifact in `deploy/z270xx/` keeps them in one place.

**Why additive form field:** absent `timestamp_granularities`, the gateway is byte-identical to today — other consumers (MacWhisper custom provider, `local-linux-models`) are unaffected. Word timestamps are opt-in per request, mirroring the recog opt-in.

### D8. Sample-rate and dtype handling

**Choice:** `transcribe_batch` and `transcribe_chunk` receive 16 kHz PCM (Int16 or float32) from the per-speaker pipeline. `RemoteASRClient` normalizes int16→float32 and writes a 16 kHz FLOAT WAV. `transcribe_with_timestamps` accepts the same array/tensor/path forms as the local method. The gateway resamples via ffmpeg on load, so a mismatched declared rate still works, but recog always sends 16 kHz to avoid a needless transcode.

**Why:** mirrors the existing local `ASRModule` input contract exactly — no caller needs to learn a remote-specific input shape.

## Risks / Trade-offs

- **[Network on the live path]** → every live batch may traverse the LAN. LAN 3–5 ms makes it negligible, but a Tailscale DERP relay (WAN, another country) would wreck latency. Mitigation: `RECOG_ASR_REMOTE_TIMEOUT_S=15` + circuit breaker; user enables only on confirmed LAN (per global CLAUDE.md `ping` rule).
- **[Cold-fallback first-paint]** → the first real fallback loads the 1.6 GB local model (~seconds), once per incident. Circuit breaker prevents repeated remote hammering; subsequent calls hit the warm local path.
- **[Gateway contract drift]** → word timestamps are added by patching our own backend in `deploy/z270xx/`. An incompatible gateway redeploy makes `transcribe_with_timestamps` fail → fallback to local. The README smoke test is mandatory after deploy.
- **[Lost batched-forward win]** → remote `transcribe_batch` is N independent requests, not one padded forward. At 99× RTF + LAN this is tolerable; any item-raise falls the whole batch back to the local padded forward.
- **[In-memory WAV encode cost]** → `soundfile.write` per chunk adds CPU on the MacBook. For 3 s 16 kHz clips this is negligible.
- **[`using_remote` flips mid-refine]** → if the circuit opens between `_refine_transcribe_fn`'s `using_remote` check and the call, the remote call raises and `FallbackASR` re-runs locally without the abort hook. Acceptable: that local run is a one-off, not a steady-state preemption target.

## Migration Plan

1. **Phase 0 — gateway patch:** deploy `deploy/z270xx/gigaam_backend.py` to Z270XX; `systemctl --user restart asr-gigaam`; curl smoke test confirms `words` in the response for `timestamp_granularities=word`.
2. **Phase 1 — client + fallback:** land `recog/remote_asr.py`, `transcribe_chunk` extraction, config consts, `build_asr()` wiring, exports. Default `RECOG_ASR_BACKEND=local` — repository behaviour unchanged; full unit suite green.
3. **Phase 2 — opt-in validation:** set `RECOG_ASR_BACKEND=remote` in a personal `.env`; run a real KTalk meeting on confirmed LAN; verify live + refine offload, force a remote outage and confirm transparent fallback + circuit-breaker logs.
4. **Rollback:** `RECOG_ASR_BACKEND=local` (or unset) + restart → `build_asr()` returns plain `ASRModule`; zero data migration.

## Open Questions

1. **Default flip:** should `RECOG_ASR_BACKEND` ever default to `remote`? Proposed: no — network/GPU dependency must stay an explicit opt-in. Revisit only if Z270XX becomes a guaranteed-present LAN peer.
2. **Per-method timeouts:** `transcribe_simple` on a long offline file legitimately exceeds 15 s. Proposed: a longer timeout (or no timeout) for `transcribe_simple` only; live/refine keep the tight bound. Decide during Phase 2 if offline-CLI offload trips the breaker spuriously.
3. **Batch parallelism cap:** `max_workers=8` matches `PCM_MAX_BATCH_SIZE`. If the gateway serializes internally, parallel requests gain nothing; revisit if remote batch latency is dominated by queueing.
4. **Healthz cadence:** `load_model()` probes `healthz` once at meeting start in remote mode. A mid-meeting remote death surfaces only on the next dispatch failure. Proposed: rely on the circuit breaker rather than a background poller (no extra thread).
