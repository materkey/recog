# Z270XX ASR gateway — word-timestamp deploy

Patched copies of the Z270XX ASR gateway that backs
`https://materkey-z270x-1.tail4799d.ts.net/asr`. They add optional word-level
timestamps for the per-speaker refine path (ADR-0019/0020) while preserving
every existing behavior.

The artifacts live inside this `recog` repo on purpose — there is **no separate
`asr-api` repo**. These files are the source of truth; Z270XX runs copies at
`~/projects/asr-api/`.

Two files must be deployed together — the word-granularity field has to survive
both the proxy and the backend:

- `gateway.py` — the OpenAI-compatible proxy (port 8770). Forwards the optional
  `timestamp_granularities` form field to the backend (the stock proxy dropped
  unknown fields, so without this the backend never sees the request).
- `gigaam_backend.py` — the GigaAM backend (port 9002). Runs the word pass and
  attaches a top-level `words` array.

## What changed vs the stock gateway

`gateway.py`:
- Accepts `timestamp_granularities: str | None = Form(None)` and forwards it
  verbatim to the backend `data` payload. Logs it (`ts_gran=`).

`gigaam_backend.py`:
- New optional form field `timestamp_granularities`. When its value contains
  `word`, words are produced from the **same** `transcribe_longform(...,
  word_timestamps=True)` pass (no separate short-form call → no length limit).
- `verbose_json` / `diarized_json` gain a top-level `words` array **only** when
  `word` was requested: `{word, start, end}` with `start`/`end` in **float
  seconds** (OpenAI verbose_json shape). The recog client
  (`recog/remote_asr.py`) normalizes seconds → integer ms on its side.
- Adapted to the GigaAM **object** API: `transcribe_longform` now returns a
  `LongformTranscriptionResult` of `Segment` objects (not dicts), converted once
  to the internal `{transcription, boundaries}` dict shape so speaker
  assignment + all formatters stay unchanged.

Default behavior is untouched: without `timestamp_granularities`, responses are
exactly as before.

## Prerequisite — GigaAM >= 6e4b027 (object API + word_timestamps)

The patched backend requires the newer GigaAM that exposes
`transcribe_longform(word_timestamps=True)` and the
`LongformTranscriptionResult`/`Segment`/`Word` object API (github
`salute-developers/GigaAM` @ `6e4b027`, the same version `recog` uses locally).
The gitverse mirror checked out at `~/projects/GigaAM` on Z270XX was older
(`6fc2fe7`, dict API, no word_timestamps). The install is editable (`pip -e`),
so syncing the package source from the Mac checkout is enough — no reinstall:

```bash
# From the Mac, sync the working gigaam/ package onto Z270XX (LAN). Preserves
# Z270XX's local setup.py patch + untracked files (only touches gigaam/).
ssh Z270XX 'cd ~/projects/GigaAM && tar czf /tmp/gigaam_pkg_backup.tgz gigaam/'   # backup
rsync -a --delete --exclude='__pycache__' --exclude='*.pyc' \
    /Users/vkkovalev/projects/GigaAM/gigaam/ Z270XX:projects/GigaAM/gigaam/
# verify the new signature:
ssh Z270XX '~/venvs/gigaam/bin/python -c "import inspect; from gigaam.model import GigaAMASR; print(inspect.signature(GigaAMASR.transcribe_longform))"'
# rollback if needed: cd ~/projects/GigaAM && git checkout -- gigaam   (restores tracked files)
```

## Deploy

```bash
# 1. Copy both patched files onto Z270XX (back up the live ones first).
ssh Z270XX 'cd ~/projects/asr-api && cp gateway.py gateway.py.bak && cp gigaam_backend.py gigaam_backend.py.bak'
scp /Users/vkkovalev/projects/recog/deploy/z270xx/gateway.py        Z270XX:~/projects/asr-api/gateway.py
scp /Users/vkkovalev/projects/recog/deploy/z270xx/gigaam_backend.py Z270XX:~/projects/asr-api/gigaam_backend.py

# 2. Restart the user services (gateway depends on the backend).
ssh Z270XX systemctl --user restart asr-gigaam
ssh Z270XX systemctl --user restart asr-gateway

# 3. Confirm health (gigaam.ok true; model load ~10s).
ssh Z270XX 'curl -s http://127.0.0.1:8770/healthz'
# expected: {"qwen":{...},"gigaam":{"ok":true,"variant":"v3_e2e_rnnt","diarization":true}}
```

## Smoke test (word timestamps)

```bash
curl -s \
  -F file=@/path/to/sample.wav \
  -F model=gigaam \
  -F response_format=verbose_json \
  -F timestamp_granularities=word \
  https://materkey-z270x-1.tail4799d.ts.net/asr/v1/audio/transcriptions \
  | python3 -m json.tool | grep -A4 '"words"'
```

Expect a non-empty `words` array of `{word, start, end}` objects with float
second offsets:

```json
    "words": [
        {
            "word": "привет",
            "start": 0.12,
            "end": 0.54
        },
```

If `grep` finds nothing: check that **both** files were deployed (the proxy
drops `timestamp_granularities` if `gateway.py` is stock) and that GigaAM is
>= 6e4b027 (older builds raise `TypeError: ... unexpected keyword argument
'word_timestamps'`).

## Note — diarization GPU memory

The `gigaam-diarized` path loads a pyannote embedding model on CUDA. When other
tenants (e.g. the vLLM Qwen3.6 server) occupy the GPU, it can fail with
`torch.OutOfMemoryError`. This is unrelated to the word-timestamp / refine
offload — `recog` does its own speaker attribution (KTalk extension + ECAPA) and
does not use the gateway's diarization.
