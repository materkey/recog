# The Python side of rust/recog-asr-server/tests/backend_differential.rs:
# recog/asr_server/gigaam_backend.py under uvicorn on the given port, its lifespan
# replaced so that no torch, GigaAM or pyannote is imported. The model is a fake that
# answers by the number of frames of the WAV ffmpeg wrote, from the scenarios file
# the Rust fake reads too; with --no-model the state stays empty ("model loading").
# Diarization is never loaded, as in the Rust backend. ffmpeg comes from PATH (the
# test puts a fake first) and temporary files go to TMPDIR.
#
#     python -I run_backend.py <repo> <port> <scenarios.json> [--no-model]
import json
import sys
import wave
from contextlib import asynccontextmanager
from types import SimpleNamespace

repo, port, scenarios_path = sys.argv[1], int(sys.argv[2]), sys.argv[3]
with_model = "--no-model" not in sys.argv[4:]
sys.path.insert(0, repo)

from recog.asr_server import gigaam_backend as backend  # noqa: E402

with open(scenarios_path, encoding="utf-8") as f:
    SCENARIOS = json.load(f)


def _spans(rows):
    return [SimpleNamespace(text=text, start=start, end=end) for text, start, end in rows]


class FakeModel:
    def transcribe_longform(self, path, word_timestamps=False):
        with wave.open(path, "rb") as w:
            frames = w.getnframes()
        scenario = SCENARIOS[str(frames)]
        if "fail" in scenario:
            raise RuntimeError(scenario["fail"])
        return SimpleNamespace(
            segments=_spans(scenario["segments"]),
            words=_spans(scenario.get("words", [])),
        )


@asynccontextmanager
async def lifespan(app):
    if with_model:
        backend.state["model"] = FakeModel()
    backend.state["diar"] = None
    yield
    backend.state.clear()


backend.app.router.lifespan_context = lifespan

import uvicorn  # noqa: E402

uvicorn.run(backend.app, host="127.0.0.1", port=port, log_level="warning")
