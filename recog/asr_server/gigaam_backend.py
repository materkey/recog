"""GigaAM v3 HTTP backend with optional pyannote diarization (default: 127.0.0.1:9002).

OpenAI-Whisper-compatible POST /v1/audio/transcriptions.
  Diarization is enabled when `model` contains "diarized" (e.g. `gigaam-diarized`)
  OR when `response_format` is "diarized_json".

Response formats: json, text, srt, vtt, verbose_json, diarized_json.
Verbose/diarized JSON includes a "speaker" field per segment when diarization ran.

Word-level timestamps (refine path, ADR-0019/0020): pass the OpenAI-compatible
`timestamp_granularities` form field containing "word" with
`response_format=verbose_json` (or `diarized_json`). Words come from the same
`transcribe_longform(word_timestamps=True)` pass (no length limit), and the
response carries a top-level "words" array of {word, start, end} in FLOAT
SECONDS (OpenAI shape); the recog client (recog/remote_asr.py) normalizes to
integer ms on its side. Segment-level fields are unchanged.

Requires GigaAM >= 6e4b027 (LongformTranscriptionResult object API +
word_timestamps support). Adapted to that object shape in transcribe().
"""
import logging
import os
import subprocess
import tempfile
from contextlib import asynccontextmanager
from pathlib import Path

import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import JSONResponse, PlainTextResponse

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("gigaam")

VARIANT = os.environ.get("GIGAAM_VARIANT", "v3_e2e_rnnt")
DIAR_MODEL = os.environ.get("DIAR_MODEL", "pyannote/speaker-diarization-community-1")
HOST = os.environ.get("GIGAAM_HOST", "127.0.0.1")
PORT = int(os.environ.get("GIGAAM_PORT", "9002"))
DEVICE = os.environ.get("GIGAAM_DEVICE", "cuda")
FP16_ENCODER = os.environ.get(
    "GIGAAM_FP16_ENCODER", "1" if DEVICE.startswith("cuda") else "0"
) == "1"
DIARIZATION_ENABLED = os.environ.get("GIGAAM_DIARIZATION", "1") == "1"
HF_TOKEN_PATH = Path(os.environ.get(
    "HF_TOKEN_PATH", "~/.cache/huggingface/token"
)).expanduser()


state: dict = {}


@asynccontextmanager
async def lifespan(app: FastAPI):
    if HF_TOKEN_PATH.exists():
        os.environ.setdefault("HF_TOKEN", HF_TOKEN_PATH.read_text().strip())

    import torch
    import torch.serialization

    torch.serialization.add_safe_globals([torch.torch_version.TorchVersion])
    orig_load = torch.load
    torch.load = lambda *a, **kw: orig_load(*a, **{**kw, "weights_only": False})

    log.info("loading gigaam %s", VARIANT)
    import gigaam

    state["model"] = gigaam.load_model(VARIANT, device=DEVICE, fp16_encoder=FP16_ENCODER)

    pipeline = None
    if DIARIZATION_ENABLED:
        log.info("loading diarization pipeline %s", DIAR_MODEL)
        from pyannote.audio import Pipeline

        # pyannote 4.0 dropped `use_auth_token`; reads HF_TOKEN from env automatically.
        pipeline = Pipeline.from_pretrained(DIAR_MODEL)
        if pipeline is None:
            log.warning("pyannote pipeline failed to load — diarization will be disabled")
        else:
            pipeline.to(torch.device(DEVICE))
    state["diar"] = pipeline
    log.info("ready")
    yield
    state.clear()


app = FastAPI(lifespan=lifespan, title="gigaam-backend")


@app.get("/healthz")
def healthz():
    return {
        "ok": "model" in state,
        "variant": VARIANT,
        "diarization": state.get("diar") is not None,
    }


@app.post("/v1/audio/transcriptions")
async def transcribe(
    file: UploadFile = File(...),
    model: str = Form("gigaam"),
    language: str | None = Form(None),
    response_format: str = Form("json"),
    prompt: str | None = Form(None),
    temperature: float | None = Form(None),
    timestamp_granularities: str | None = Form(None),
):
    if "model" not in state:
        raise HTTPException(503, "model loading")

    fmt = (response_format or "json").lower()
    want_diar = "diarized" in model.lower() or fmt == "diarized_json"
    want_words = "word" in (timestamp_granularities or "").lower()

    suffix = Path(file.filename or "audio.wav").suffix or ".wav"
    with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as tf:
        data = await file.read()
        tf.write(data)
        src_path = tf.name

    wav_path = src_path + ".16k.wav"
    words: list | None = None
    try:
        r = subprocess.run(
            ["ffmpeg", "-y", "-i", src_path, "-ar", "16000", "-ac", "1",
             "-c:a", "pcm_s16le", wav_path],
            capture_output=True,
        )
        if r.returncode != 0:
            log.error("ffmpeg failed (%d): %s", r.returncode, r.stderr.decode()[-500:])
            raise HTTPException(400, "ffmpeg could not decode the audio")

        log.info("transcoded %s (%d bytes) → wav (%d bytes), diar=%s, words=%s",
                 file.filename, len(data), os.path.getsize(wav_path), want_diar,
                 want_words)

        lf = state["model"].transcribe_longform(wav_path, word_timestamps=want_words)
        # New GigaAM (6e4b027) returns a LongformTranscriptionResult of Segment
        # objects; adapt to the {transcription, boundaries} dict shape the rest
        # of this backend (speaker assignment, formatters) already expects.
        result = [
            {"transcription": s.text, "boundaries": (s.start, s.end)}
            for s in lf.segments
        ]
        log.info("gigaam returned %d segment(s)", len(result))

        if want_diar:
            pipeline = state.get("diar")
            if pipeline is None:
                raise HTTPException(503, "diarization pipeline unavailable")
            diar = pipeline(wav_path).exclusive_speaker_diarization
            n_spk = len(diar.labels())
            log.info("diarization: %d speaker(s)", n_spk)
            _assign_speakers(result, diar)

        if want_words:
            # LongformTranscriptionResult.words flattens per-segment words with
            # absolute (segment-offset-adjusted) start/end in FLOAT SECONDS.
            # Done in the same longform pass — no separate (length-limited)
            # short-form transcribe() call.
            words = [
                {"word": w.text, "start": float(w.start), "end": float(w.end)}
                for w in lf.words
            ]
            log.info("word timestamps: %d word(s)", len(words))
    finally:
        for p in (src_path, wav_path):
            try:
                os.unlink(p)
            except OSError:
                pass

    text = " ".join(seg["transcription"].strip() for seg in result)
    return _format_response(fmt, text, result, language or "ru", want_diar, words)


def _assign_speakers(segments: list, diarization) -> None:
    """Annotate each gigaam segment with the speaker holding max overlap in its window."""
    from pyannote.core import Segment

    for seg in segments:
        start, end = float(seg["boundaries"][0]), float(seg["boundaries"][1])
        if end <= start:
            seg["speaker"] = None
            continue
        sub = diarization.crop(Segment(start, end))
        durations: dict[str, float] = {}
        for turn, _track, spk in sub.itertracks(yield_label=True):
            durations[spk] = durations.get(spk, 0.0) + turn.duration
        seg["speaker"] = max(durations, key=durations.get) if durations else None


def _format_response(fmt: str, text: str, segments: list, language: str, diar: bool,
                     words: list | None = None):
    total = float(segments[-1]["boundaries"][1]) if segments else 0.0

    if fmt == "text":
        return PlainTextResponse(text)
    if fmt == "srt":
        return PlainTextResponse(_to_srt(segments, diar), media_type="application/x-subrip")
    if fmt == "vtt":
        return PlainTextResponse(_to_vtt(segments, diar), media_type="text/vtt")
    if fmt in ("verbose_json", "diarized_json"):
        body = {
            "task": "transcribe",
            "language": language,
            "duration": total,
            "text": text,
            "segments": [
                {
                    "id": i,
                    "seek": 0,
                    "start": float(s["boundaries"][0]),
                    "end": float(s["boundaries"][1]),
                    "text": s["transcription"].strip(),
                    "speaker": s.get("speaker"),
                    "tokens": [],
                    "temperature": 0.0,
                    "avg_logprob": 0.0,
                    "compression_ratio": 0.0,
                    "no_speech_prob": 0.0,
                }
                for i, s in enumerate(segments)
            ],
        }
        if words is not None:
            body["words"] = words
        return JSONResponse(body)
    return JSONResponse({"text": text})


def _ts_srt(seconds: float) -> str:
    h = int(seconds // 3600); m = int((seconds % 3600) // 60)
    s = int(seconds % 60); ms = int((seconds - int(seconds)) * 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def _ts_vtt(seconds: float) -> str:
    return _ts_srt(seconds).replace(",", ".")


def _line(seg: dict, diar: bool) -> str:
    text = seg["transcription"].strip()
    if diar and seg.get("speaker"):
        return f"[{seg['speaker']}] {text}"
    return text


def _to_srt(segments: list, diar: bool) -> str:
    out = []
    for i, s in enumerate(segments, 1):
        out.append(
            f"{i}\n{_ts_srt(s['boundaries'][0])} --> {_ts_srt(s['boundaries'][1])}\n"
            f"{_line(s, diar)}\n"
        )
    return "\n".join(out)


def _to_vtt(segments: list, diar: bool) -> str:
    lines = ["WEBVTT", ""]
    for s in segments:
        lines.append(f"{_ts_vtt(s['boundaries'][0])} --> {_ts_vtt(s['boundaries'][1])}")
        lines.append(_line(s, diar))
        lines.append("")
    return "\n".join(lines)


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT, log_level="info")
