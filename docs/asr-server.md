# Optional ASR server

Recog includes two independently runnable HTTP services, both built from the
crate `rust/recog-asr-server`:

- `recog-asr-gigaam`: GigaAM transcription, default `127.0.0.1:9002`.
- `recog-asr-gateway`: an OpenAI-compatible proxy, default `0.0.0.0:8770`. It
  routes GigaAM and Qwen aliases to configured backend URLs.

These services are optional. `setup.sh` and `recog-service.sh` do not build,
install, start, or manage them. Installing the normal Recog application does
not start an ASR server or reserve GPU memory. They need neither Python,
PyTorch nor pyannote, and have no speaker diarization (see below).

## Build on a server

Install Rust (<https://rustup.rs>) and put `ffmpeg` on `PATH`; the backend
decodes every upload with it. Build both programs from the repository:

```bash
cd rust && cargo build --release -p recog-asr-server
```

The programs are `rust/target/release/recog-asr-gigaam` and
`rust/target/release/recog-asr-gateway`; copy them anywhere, they read nothing
from the checkout. Qwen is an external backend: this repository neither
installs nor starts it.

The backend runs GigaAM v3 on the `wavo` crate, as the live process does: the
Q8_0 GGUF of `GIGAAM_VARIANT` (`v3_e2e_rnnt` is `gigaam-v3`; `v3_e2e_ctc`,
`v3_rnnt` and `v3_ctc` also exist, older variants stop the start with an
error) comes from the Hugging Face cache (`HF_HOME`) and is downloaded there on
the first start; no token is needed. wavo picks the GPU itself through wgpu
(Vulkan on Linux, `WGPU_BACKEND` overrides the API), so a second GPU cannot be
chosen. A failed model load exits with code 3.

## Start and configure

Run the backend and gateway in separate terminals or configure your process
manager to run these same commands:

```bash
# Backend first: it loads the model before it listens.
GIGAAM_DIARIZATION=0 rust/target/release/recog-asr-gigaam

# Gateway: use a loopback bind for a local-only installation.
ASR_GATEWAY_HOST=127.0.0.1 rust/target/release/recog-asr-gateway
```

Settings are read from the process environment when each program starts; they
are independent of Recog's client settings. A value the server cannot use (a
port that is not a number, for example) stops the start with a message.

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
| `GIGAAM_DIARIZATION` | `1` | Whether diarization is asked for at startup (`1` or `0`); with `1` the backend logs that it is unavailable |
| `GIGAAM_DEVICE`, `GIGAAM_FP16_ENCODER` | `cuda`, `1` for CUDA | Only logged: wavo chooses the device and precision |
| `DIAR_MODEL`, `HF_TOKEN_PATH` | | No effect |

Changing `GIGAAM_PORT` also requires updating the gateway's `GIGAAM_URL`.
Use one backend process per intended model instance; each backend process loads
its own model. Requests run concurrently, health checks included.

The gateway has no authentication. Its historical default exposes all
interfaces; restrict access to a trusted network, bind to loopback, or place an
authenticated reverse proxy in front. Configure the host firewall explicitly
if clients connect remotely. A listening socket and local health check do not
establish that remote access is available.

## No speaker diarization

pyannote has no Rust counterpart. The backend answers as the former Python
backend did when its diarization pipeline failed to load: with
`GIGAAM_DIARIZATION=1` it logs
`pyannote pipeline failed to load — diarization will be disabled`, `/healthz`
reports `"diarization": false`, and a request with a `diarized` model or
`response_format=diarized_json` gets HTTP 503
`diarization pipeline unavailable` after the transcription.

Without pyannote's VAD the audio is cut at its quietest moments into pieces of
at most 25 s; each piece with text is one segment from its first to its last
sample, so a segment can begin or end in silence. Words keep GigaAM's rule
(from a word's first token to the end of its last 40 ms frame), in float
seconds.

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
Only an actual transcription verifies audio decoding, model access, and
inference.

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

The routes, bodies and status codes are those of the former Python server,
FastAPI's own answers included (422 validation details, 400 for an unreadable
form, 404, 405 with `Allow`, the 307 of a trailing slash). The frozen tests
(`gateway_frozen`, `backend_frozen`) hold both programs to the answers the
Python server gave to the same requests, recorded in
`rust/recog-asr-server/tests/fixtures/`, against one fake backend and a fake
model, without loading a model:

```bash
cd rust && cargo test -p recog-asr-server
```

These tests do not validate GPU inference or model access on the target host;
run the transcription smoke there before relying on the service.
