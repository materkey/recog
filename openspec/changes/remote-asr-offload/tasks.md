## 0. Gateway patch (deploy/z270xx)

- [ ] 0.1 Copy the current Z270XX backend into `deploy/z270xx/gigaam_backend.py`; add an optional form field `timestamp_granularities` (string) to the `/v1/audio/transcriptions` handler
- [ ] 0.2 When `timestamp_granularities == "word"`, call `state["model"].transcribe(wav_path, word_timestamps=True)` instead of `transcribe_longform`; include top-level `"words": [{"word": w.text, "start": float(w.start), "end": float(w.end)}]` in the JSON response alongside `text`
- [ ] 0.3 Confirm all existing behaviour (json / verbose_json / diarized / srt / vtt, ffmpeg transcode) is byte-identical when `timestamp_granularities` is absent
- [ ] 0.4 Write `deploy/z270xx/README.md`: scp target `~/projects/asr-api/gigaam_backend.py`, `systemctl --user restart asr-gigaam`, curl smoke test asserting `words` is present for `timestamp_granularities=word` and absent otherwise
- [ ] 0.5 Deploy to Z270XX and run the smoke test from the README

## 1. Config

- [ ] 1.1 Add to `recog/config.py` (module-top `os.environ.get`, matching `MIC_SPEAKER_NAME` / `PCM_*`): `RECOG_ASR_BACKEND` (default `"local"`), `RECOG_ASR_REMOTE_URL` (default `https://materkey-z270x-1.tail4799d.ts.net/asr`), `RECOG_ASR_REMOTE_MODEL` (default `"gigaam"`), `RECOG_ASR_REMOTE_TIMEOUT_S` (`float`, default `15`), `RECOG_ASR_CIRCUIT_FAILS` (`int`, default `3`), `RECOG_ASR_CIRCUIT_OPEN_S` (`float`, default `60`)
- [ ] 1.2 Document the new vars in `.env.example` with one-line comments; committed default keeps offload OFF

## 2. ASRModule.transcribe_chunk extraction

- [ ] 2.1 Add `ASRModule.transcribe_chunk(self, audio: np.ndarray, sample_rate: int = 16000) -> Optional[str]`: `load_model(quiet=True)`; move to device/dtype; under `self._model_lock`, `torch.inference_mode()`, `_mps_autoreleasepool(self.device)` run `model.forward` + `decode[0][0]`; `self._maybe_empty_mps_cache()`; return `text.strip()` or `None`; return `None` on exception (best-effort, like the current direct path)
- [ ] 2.2 Verify `_model_lock` remains a `threading.RLock` (refine acquires externally, `transcribe_with_timestamps` re-acquires internally)
- [ ] 2.3 Unit test in `tests/test_asr.py`: `transcribe_chunk` returns stripped text on a known clip and `None` on a forced forward error

## 3. RemoteASRClient

- [ ] 3.1 New file `recog/remote_asr.py` with `class RemoteASRError(Exception)` and `class RemoteASRClient(base_url, model="gigaam", timeout_s=15.0, max_workers=8)` on a sync `httpx.Client`
- [ ] 3.2 `healthz() -> bool`: `GET /healthz`; `True` only if reachable AND `ok`
- [ ] 3.3 `transcribe_chunk(audio, sample_rate=16000) -> str`: build in-memory WAV via `soundfile.write(BytesIO, float32, sr, format="WAV", subtype="FLOAT")`; `POST` multipart `response_format=json`; return `text`; raise `RemoteASRError` on any HTTP/parse failure
- [ ] 3.4 `transcribe_batch(pcm_list) -> list[str]`: `ThreadPoolExecutor(max_workers)` over `transcribe_chunk`, ORDER-PRESERVING; if ANY item raises → raise `RemoteASRError`
- [ ] 3.5 `transcribe_simple(audio_path, **kwargs) -> list[Segment]`: `POST file`, `response_format=verbose_json`; map `segments[]` → `recog.models.Segment(text, start, end)`
- [ ] 3.6 `transcribe_with_timestamps(audio, sample_rate=16000) -> TranscriptionResult`: accept `np.ndarray` / `torch.Tensor` / str path, int16→float32 like `ASRModule.transcribe_with_timestamps`; `POST WAV` with extra form field `timestamp_granularities="word"`; parse top-level `words[]={word,start(sec),end(sec)}` → `Word(text=word, start=int(round(start*1000)), end=int(round(end*1000)))`; return `TranscriptionResult(text=..., words=[...])`
- [ ] 3.7 Unit tests in `tests/test_remote_asr.py` with a mocked `httpx` transport: healthz true/false, chunk text, batch order + all-or-nothing raise, simple→Segment mapping, timestamps→int-ms Word mapping

## 4. FallbackASR

- [ ] 4.1 `class FallbackASR(remote, local_factory=ASRModule, fail_threshold=3, open_seconds=60.0)`: construct `self._local = local_factory()` IMMEDIATELY (no `load_model()`); circuit state under `threading.Lock`, timed with `time.monotonic()`
- [ ] 4.2 `using_remote -> bool`: `True` when circuit closed/half-open
- [ ] 4.3 Delegate `model -> self._local.model`, `device -> self._local.device`, `_model_lock = self._local._model_lock` (SAME object), `_maybe_empty_mps_cache() -> self._local._maybe_empty_mps_cache()`
- [ ] 4.4 `load_model(quiet=False)`: if `using_remote` → `remote.healthz()` (log readiness), do NOT load local, return `None`; else → `self._local.load_model(quiet)` and return the local model
- [ ] 4.5 `_dispatch` for all four methods: closed/half-open → try remote; on `RemoteASRError`/httpx error → `_record_failure()`, one-line mode-switch log on first fallback, `self._local.load_model(quiet=True)`, `local.<m>(...)`; on remote success → `_record_success()`; OPEN → straight to local; half-open → one remote probe after `open_seconds`
- [ ] 4.6 `_record_failure`: `failures += 1`; at `failures >= fail_threshold` set `opened_until = monotonic() + open_seconds`, log `[FallbackASR] remote circuit OPEN for {open_seconds}s after {failures} fail(s)`
- [ ] 4.7 Error contract: `transcribe_chunk` returns `None` on total failure; `transcribe_batch` / `transcribe_simple` / `transcribe_with_timestamps` re-raise the LOCAL exception if local also fails; never swallow both-sides-failed
- [ ] 4.8 Unit tests in `tests/test_remote_asr.py`: lazy load (local model not loaded while remote healthy), circuit open after `fail_threshold`, half-open probe closes on success / re-opens on failure, `transcribe_chunk` → `None` on total failure, batch/simple/timestamps re-raise local exception

## 5. build_asr() and wiring

- [ ] 5.1 `build_asr() -> ASRModule | FallbackASR` in `recog/remote_asr.py`: read `recog.config`; `remote` → `FallbackASR(RemoteASRClient(...), fail_threshold=RECOG_ASR_CIRCUIT_FAILS, open_seconds=RECOG_ASR_CIRCUIT_OPEN_S)`; else → `ASRModule()`
- [ ] 5.2 `recog/live.py` `LiveTranscriber.__init__`: replace `self.asr = ASRModule()` with `from .remote_asr import build_asr; self.asr = build_asr()`
- [ ] 5.3 `recog/live.py` `_transcribe_direct`: keep the `set_live_pending()/clear_live_pending()` try/finally + outer try/except; inside `return self.asr.transcribe_chunk(audio_chunk)`
- [ ] 5.4 `recog/live.py` `_refine_transcribe_fn`: guard the decoder monkey-patch — `if getattr(self.asr, "using_remote", False): return self.asr.transcribe_with_timestamps(pcm_samples)` (no load_model, no patch); else existing local path
- [ ] 5.5 Confirm `_setup` `self.asr.load_model(quiet=True)` works for both (remote-mode healthcheck, local-mode load)
- [ ] 5.6 `mp4_notes.py`: route the offline `transcribe_simple` / `transcribe_longform` call sites through `build_asr()` instead of constructing `ASRModule()` directly
- [ ] 5.7 `recog/__init__.py`: export `build_asr`, `RemoteASRClient`, `FallbackASR` via the existing lazy-export map

## 6. Tests + validation

- [ ] 6.1 `python -m compileall recog frontend` clean; `python mp4_notes.py --help` smoke
- [ ] 6.2 Full suite `python -m pytest tests/ -v` green with default `RECOG_ASR_BACKEND=local` (repository behaviour unchanged)
- [ ] 6.3 With `RECOG_ASR_BACKEND=remote` against the live gateway: a real KTalk meeting on confirmed LAN (`ping` < 5 ms) offloads live + refine; verify transcript + tier-2 refine arrive
- [ ] 6.4 Force a remote outage mid-meeting (stop the gateway / block the URL); confirm transparent fallback, the `[FallbackASR] remote circuit OPEN ...` log, and that the live meeting does not crash
- [ ] 6.5 Confirm lazy load: with a healthy remote, the local GigaAM is never loaded (no "Загрузка модели" log, no ~1.6 GB RSS bump)

## 7. Docs + rollout

- [ ] 7.1 `docs/adr/0020-remote-asr-offload.md` (this change's ADR)
- [ ] 7.2 Update `CLAUDE.md` Architecture section: `build_asr()`, remote backend, lazy fallback, circuit breaker, `RECOG_ASR_*` env vars
- [ ] 7.3 `openspec archive remote-asr-offload` once tasks 0–6 complete and the gateway smoke test + outage drill pass
