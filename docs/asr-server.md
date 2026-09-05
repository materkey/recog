# Optional ASR server

Recog includes two independently runnable HTTP services:

- `python -m recog.asr_server.gigaam_backend`: GigaAM transcription and optional
  pyannote speaker diarization, default `127.0.0.1:9002`.
- `python -m recog.asr_server.gateway`: an OpenAI-compatible proxy, default
  `0.0.0.0:8770`. It routes GigaAM and Qwen aliases to configured backend URLs.

These services are optional. `setup.sh` and `recog-service.sh` do not install,
start, or manage them. Installing the normal Recog application does not start
an ASR server or reserve GPU memory. Run the commands below from the repository
root in a separate server virtualenv; keep the checkout available for module
imports.

## Install on a Linux server

Use Python 3.10 or newer, Git, and `ffmpeg` on `PATH`. The default device is
`cuda`: this requires an NVIDIA GPU, a compatible driver, and a PyTorch build
with CUDA support. Linux alone does not imply CUDA availability. For CPU use,
set `GIGAAM_DEVICE=cpu`; the default encoder precision then becomes float32.
CPU configuration is supported by the entry point but its inference performance
has not been validated by the repository's configuration tests.

```bash
python3 -m venv .venv-asr-server
source .venv-asr-server/bin/activate
# Install the PyTorch/torchaudio build matching the server's CPU or CUDA runtime.
python -m pip install -r deploy/python/asr-server.txt
python -m pip check
```

The dependency file installs GigaAM at the `6e4b027` pin used by Recog, its
`longform` extras, and pyannote.audio 4.x. The backend uses the
`LongformTranscriptionResult`/`Segment`/`Word` object API and
`transcribe_longform(word_timestamps=True)`. The HTTP multipart dependency is
`python-multipart`. Qwen is an external backend: this repository neither
installs nor starts it.

GigaAM longform uses the `pyannote/segmentation-3.0` VAD model even when speaker
diarization is disabled. Before first use, arrange model access and accept any
required model terms for your Hugging Face account. Speaker diarization also
requires access to `pyannote/speaker-diarization-community-1` by default.
Provide `HF_TOKEN` in the process environment or use a token file at
`~/.cache/huggingface/token` (override with `HF_TOKEN_PATH`). An existing
`HF_TOKEN` takes precedence. Model downloads and inference happen in the server
process, not while importing its Python modules.

## Start and configure

Run the backend and gateway in separate terminals or configure your process
manager to run these same commands with the repository as its working directory:

```bash
# Backend: CUDA and speaker diarization are enabled by default.
python -m recog.asr_server.gigaam_backend

# Gateway: use a loopback bind for a local-only installation.
ASR_GATEWAY_HOST=127.0.0.1 python -m recog.asr_server.gateway
```

For a CPU backend without speaker diarization:

```bash
GIGAAM_DEVICE=cpu GIGAAM_DIARIZATION=0 python -m recog.asr_server.gigaam_backend
```

Settings are read from the process environment when each module starts; they
are independent of Recog's client settings.

| Variable | Default | Purpose |
| --- | --- | --- |
| `ASR_GATEWAY_HOST` | `0.0.0.0` | Gateway listening address |
| `ASR_GATEWAY_PORT` | `8770` | Gateway listening port |
| `GIGAAM_URL` | `http://127.0.0.1:9002` | Gateway's GigaAM backend URL |
| `QWEN_ASR_URL` | `http://127.0.0.1:9001` | Gateway's external Qwen backend URL |
| `ASR_TIMEOUT_S` | `1800` | Gateway HTTP request timeout in seconds |
| `GIGAAM_HOST` | `127.0.0.1` | GigaAM listening address |
| `GIGAAM_PORT` | `9002` | GigaAM listening port |
| `GIGAAM_VARIANT` | `v3_e2e_rnnt` | GigaAM model variant |
| `GIGAAM_DEVICE` | `cuda` | PyTorch device, for example `cuda:1` or `cpu` |
| `GIGAAM_FP16_ENCODER` | `1` for CUDA, otherwise `0` | Encoder half precision (`1` or `0`) |
| `GIGAAM_DIARIZATION` | `1` | Load speaker diarization at startup (`1` or `0`) |
| `DIAR_MODEL` | `pyannote/speaker-diarization-community-1` | Speaker diarization model |
| `HF_TOKEN_PATH` | `~/.cache/huggingface/token` | Fallback token file |

Changing `GIGAAM_PORT` also requires updating the gateway's `GIGAAM_URL`.
Use one backend process per intended model instance; each backend process loads
its own models. Disabling speaker diarization reduces startup model loading,
but does not remove longform VAD's pyannote dependency. A diarization request
returns HTTP 503 when its pipeline is disabled or unavailable.

The gateway has no authentication. Its historical default exposes all
interfaces; restrict access to a trusted network, bind to loopback, or place an
authenticated reverse proxy in front. Configure the host firewall explicitly
if clients connect remotely. A listening socket and local health check do not
establish that remote access is available.

## API and verification

```bash
curl -s http://127.0.0.1:9002/healthz
curl -s http://127.0.0.1:8770/healthz
curl -s http://127.0.0.1:8770/v1/models
curl -s \
  -F file=@/path/to/sample.wav \
  -F model=gigaam \
  -F response_format=verbose_json \
  -F timestamp_granularities=word \
  http://127.0.0.1:8770/v1/audio/transcriptions
```

Backend health reports `ok`, `variant`, and `diarization`. Gateway health
aggregates backend responses; it can return HTTP 200 while a backend has an
`error`. An absent Qwen backend produces a Qwen error without preventing GigaAM
requests. `/v1/models` lists configured aliases, not a live readiness inventory.
Only an actual transcription verifies audio decoding, VAD/model access, and
inference; health alone does not exercise longform VAD.

The gateway retains GigaAM aliases (`gigaam`, `gigaam-v3`,
`gigaam-v3-e2e-rnnt`), diarized aliases (`gigaam-diarized`,
`gigaam-diarized-v3`), Qwen aliases (`qwen`, `qwen3-asr`, `qwen3-asr-1.7b`),
and `whisper-1` (routed to GigaAM). It forwards the optional
`timestamp_granularities` field unchanged.

The backend accepts `json`, `text`, `srt`, `vtt`, `verbose_json`, and
`diarized_json`. A model name containing `diarized` or the `diarized_json`
format requests speaker diarization. Requesting `word` granularity adds a
`words` array to verbose/diarized JSON, with `{word, start, end}` and float
second offsets. Recog converts these offsets to milliseconds on receipt.

For a Recog client, set `RECOG_ASR_BACKEND=remote` and
`RECOG_ASR_REMOTE_URL=http://<server-address>:8770` in its launch environment.
Use the gateway's externally reachable base URL, including a reverse-proxy path
prefix if applicable. Server installation does not change any client's backend
selection.

Configuration and gateway forwarding can be checked without loading models:

```bash
python -m pytest tests/test_asr_server.py -q
```

Install pytest separately if using only the server requirements. These tests do
not validate a fresh dependency installation, CUDA/CPU inference, model access,
or speaker diarization on the target host; run the transcription smoke there
before relying on the service.
