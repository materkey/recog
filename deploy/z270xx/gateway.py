"""OpenAI-compatible ASR gateway. Listens on 0.0.0.0:8770.

Routes /v1/audio/transcriptions by `model` form field to local backends.
Lists available models via /v1/models. No auth (Tailscale-only network).

Word-level timestamps (refine path, ADR-0019/0020): the optional
`timestamp_granularities` form field is forwarded verbatim to the backend so
GigaAM can emit a top-level "words" array under verbose_json/diarized_json.
Without this passthrough the field is dropped at the proxy and the backend
never runs the word pass.
"""
import logging
import os

import httpx
import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import Response

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("gateway")

QWEN_URL = os.environ.get("QWEN_ASR_URL", "http://127.0.0.1:9001")
GIGAAM_URL = os.environ.get("GIGAAM_URL", "http://127.0.0.1:9002")
TIMEOUT = float(os.environ.get("ASR_TIMEOUT_S", "1800"))

# Model name → backend URL.
MODEL_MAP = {
    # GigaAM aliases — best Russian quality.
    "gigaam": GIGAAM_URL,
    "gigaam-v3": GIGAAM_URL,
    "gigaam-v3-e2e-rnnt": GIGAAM_URL,
    # GigaAM + pyannote diarization.
    "gigaam-diarized": GIGAAM_URL,
    "gigaam-diarized-v3": GIGAAM_URL,
    # Qwen3-ASR aliases — multilingual fast.
    "qwen3-asr": QWEN_URL,
    "qwen3-asr-1.7b": QWEN_URL,
    "qwen": QWEN_URL,
    # Default OpenAI client model — route to GigaAM (best for ru).
    "whisper-1": GIGAAM_URL,
}

app = FastAPI(title="asr-gateway")
client = httpx.AsyncClient(timeout=TIMEOUT)


@app.on_event("shutdown")
async def _shutdown():
    await client.aclose()


@app.get("/healthz")
async def healthz():
    out = {}
    for name, url in {"qwen": QWEN_URL, "gigaam": GIGAAM_URL}.items():
        try:
            r = await client.get(f"{url}/healthz", timeout=5)
            out[name] = r.json()
        except Exception as e:
            out[name] = {"error": str(e)}
    return out


@app.get("/v1/models")
def models():
    return {
        "object": "list",
        "data": [
            {"id": m, "object": "model", "owned_by": "local"}
            for m in sorted(set(MODEL_MAP.keys()))
        ],
    }


@app.post("/v1/audio/transcriptions")
async def transcribe(
    file: UploadFile = File(...),
    model: str = Form("whisper-1"),
    language: str | None = Form(None),
    response_format: str = Form("json"),
    prompt: str | None = Form(None),
    temperature: float | None = Form(None),
    timestamp_granularities: str | None = Form(None),
):
    backend = MODEL_MAP.get(model.lower())
    if not backend:
        raise HTTPException(
            400,
            f"unknown model '{model}'. Available: {sorted(set(MODEL_MAP.keys()))}",
        )

    payload_data = await file.read()
    files = {"file": (file.filename or "audio.wav", payload_data, file.content_type or "application/octet-stream")}
    data: dict = {"model": model, "response_format": response_format}
    if language:
        data["language"] = language
    if prompt:
        data["prompt"] = prompt
    if temperature is not None:
        data["temperature"] = str(temperature)
    # Forward word-granularity request so the backend runs the word pass.
    if timestamp_granularities:
        data["timestamp_granularities"] = timestamp_granularities

    log.info("→ %s  model=%s  bytes=%d  fmt=%s  lang=%s  ts_gran=%s",
             backend, model, len(payload_data), response_format, language,
             timestamp_granularities)
    try:
        r = await client.post(f"{backend}/v1/audio/transcriptions", files=files, data=data)
    except httpx.HTTPError as e:
        log.exception("backend error")
        raise HTTPException(502, f"backend {backend} unreachable: {e}")

    if r.status_code >= 400:
        raise HTTPException(r.status_code, r.text)
    return Response(
        content=r.content,
        status_code=r.status_code,
        media_type=r.headers.get("content-type"),
    )


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=8770, log_level="info")
