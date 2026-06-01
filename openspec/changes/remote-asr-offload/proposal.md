## Why

GigaAM (`v3_e2e_rnnt`) runs locally on the MacBook for every inference surface: the per-speaker live batch (ADR-0018), the offline CLI, the refine contour (ADR-0019), and the direct `model.forward()` for mic / mixed-fallback in `live.py`. All four share a single model instance under `ASRModule._model_lock` (~1.6 GB RSS on M1 Pro 16 GB, ~60× realtime batched). Refine and live contend for that one mutex; long meetings accrue an MPS NSObject leak (ADR-0019 "Operational concerns") that is the symptom of one machine carrying ASR + UI + browser + extension at once.

On the same LAN sits **Z270XX** — an RTX PRO 6000 Blackwell, 96 GiB GDDR7 (skill `local-linux-models`) — fronting GigaAM v3 + Qwen3-ASR behind an OpenAI-Whisper-compatible gateway at `https://materkey-z270x-1.tail4799d.ts.net/asr`. GigaAM there runs at **~99× RTF**, an order of magnitude over the local batch, and LAN RTT is **3–5 ms** (`ping` min 3.858 ms — real LAN, not a DERP relay). Offloading inference there frees the MacBook's GPU/CPU for the UI and removes the live↔refine mutex contention.

This change adds an **opt-in** remote ASR offload behind a single `build_asr()` constructor with a **lazy** local fallback and a circuit breaker. The committed default is `RECOG_ASR_BACKEND=local`, so repository behaviour is unchanged until the user flips the env var. All four GigaAM paths are offloaded — including refine, which requires word-level timestamps the current gateway does not emit; the gateway is extended in-repo (`deploy/z270xx/`) to supply them.

## What Changes

- **Single constructor `build_asr()`** (`recog/remote_asr.py`) returns `ASRModule` (local) or `FallbackASR` (remote) based on `recog.config`. Becomes the only ASR constructor callers use (`LiveTranscriber.__init__`, offline CLI in `mp4_notes.py`).
- **`RemoteASRClient`** over the OpenAI-Whisper gateway (sync `httpx.Client`; WAV built in memory via `soundfile.write(BytesIO, float32, sr, format="WAV", subtype="FLOAT")`). Implements `healthz`, `transcribe_chunk`, `transcribe_batch`, `transcribe_simple`, `transcribe_with_timestamps` — returning structures byte-compatible with the local `ASRModule` (same `Segment`, same `TranscriptionResult` with `Word.start/end` in **int milliseconds**).
- **`FallbackASR`** wraps `RemoteASRClient` with a thread-safe circuit breaker. The local `ASRModule` is **constructed** eagerly (cheap — no model load) but **loaded** lazily, only on the first remote failure. Delegates `model`, `device`, `_model_lock` (the same object — `RefineWorker` acquires it externally), and `_maybe_empty_mps_cache` to the eager local instance.
- **All four GigaAM paths offloaded:** `transcribe_batch` (live per-speaker), `transcribe_simple` (offline CLI), `transcribe_with_timestamps` (refine), and the direct `model.forward()` mic/mixed-fallback in `live.py`. The fourth gains a public method `ASRModule.transcribe_chunk` extracted from `LiveTranscriber._transcribe_direct`.
- **Gateway extended in-repo** (`deploy/z270xx/gigaam_backend.py`): a new optional form field `timestamp_granularities="word"` switches the backend from `transcribe_longform` to `transcribe(word_timestamps=True)` and adds top-level `words:[{word,start,end}]` to the JSON response. All existing behaviour (json/verbose_json/diarized/srt/vtt, ffmpeg transcode) is untouched when word ts is not requested.
- **Env config** in `recog/config.py`: `RECOG_ASR_BACKEND` (default `local`), `RECOG_ASR_REMOTE_URL`, `RECOG_ASR_REMOTE_MODEL`, `RECOG_ASR_REMOTE_TIMEOUT_S`, `RECOG_ASR_CIRCUIT_FAILS`, `RECOG_ASR_CIRCUIT_OPEN_S`.
- **`.env.example`** documents the new `RECOG_ASR_*` vars.

## Capabilities

### Modified Capabilities

- `per-speaker-transcription`: gains an optional remote inference backend for all four GigaAM paths, a lazy-loaded local fallback, and a circuit breaker. When `RECOG_ASR_BACKEND=local` (default) behaviour is identical to ADR-0018/0019; when `remote`, inference is offloaded to Z270XX with transparent fallback.

### New Capabilities

None.

## Impact

**Code:**
- `recog/remote_asr.py` (new) — `RemoteASRError`, `RemoteASRClient`, `FallbackASR`, `build_asr()`.
- `recog/asr.py` — new `ASRModule.transcribe_chunk(audio, sample_rate=16000) -> Optional[str]` (extracted from `_transcribe_direct`). `_model_lock` stays an `RLock`.
- `recog/config.py` — `RECOG_ASR_*` constants (module-top `os.environ.get`).
- `recog/live.py` — `self.asr = build_asr()`; `_transcribe_direct` calls `self.asr.transcribe_chunk`; `_refine_transcribe_fn` gates the decoder monkey-patch on `getattr(self.asr, "using_remote", False)`.
- `mp4_notes.py` — offline transcription routes through `build_asr()`.
- `recog/__init__.py` — export `build_asr`, `RemoteASRClient`, `FallbackASR`.
- `deploy/z270xx/gigaam_backend.py` (new) — patched gateway backend with word-timestamp support.
- `deploy/z270xx/README.md` (new) — scp target + `systemctl --user restart asr-gigaam` + curl smoke test.

**APIs:**
- Gateway gains an optional `timestamp_granularities` form field (additive; absent = current behaviour).
- No change to `transcript.jsonl`, WS message types, or `MeetingStorage`.

**Dependencies:**
- `httpx`, `soundfile`, `numpy` — already in `venv-gigaam`. No new PyPI packages on the recog side.

**Operational:**
- Network: every live batch may traverse the LAN (3–5 ms). `RECOG_ASR_REMOTE_TIMEOUT_S=15` bounds per-request stalls; circuit breaker bounds repeated failures.
- Memory: in remote mode the local model is never loaded (lazy) — zero local VRAM/RAM until a fallback. On fallback, ~1.6 GB local model loads once.
- First-paint: warm remote round-trip ≈ network + (clip_seconds / 99); cold fallback pays one `load_model()` (~seconds), once per incident.

**Non-goals:**
- Changing live `chunk_duration` (stays 3.0 s) or the refine window (stays 20 s).
- Replacing GigaAM with Whisper/Qwen3-ASR (the gateway exposes them, but recog stays on `gigaam`).
- A separate `asr-api` repository (the gateway patch lives in `deploy/z270xx/`).
- Async ASR clients (sync `httpx.Client` + ThreadPoolExecutor for the batch).
- Default-on offload (committed default is `local`; opt-in only).
- Diarization via the gateway's `diarized` formats (recog owns attribution).
