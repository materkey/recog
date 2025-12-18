# Repository Guidelines

## Project Structure & Module Organization
- `recog/`: core Python package (ASR, diarization client, speaker DB, alignment, live pipeline).
- `mp4_notes.py`: main CLI entry point (`transcribe`, `live`, `meetings`, `enroll-speaker`, `build-speaker-db`).
- `frontend/`: FastAPI server + SPA assets in `frontend/static/` (WebSocket streams live transcripts).
- `kontur-diarize/ktalk-speaker-extension/`: Chrome extension for KTalk speaker signals.
- `adr/`: Architecture Decision Records (start with `adr/0006-dual-venv-subprocess.md`).
- Runtime output: `meetings/` and `logs/` (gitignored).

## Build, Test, and Development Commands
- Bootstrap (macOS/Python 3.12): `./setup_venvs.sh` (creates `venv-gigaam/`, `venv-pyannote/`, and `.env`).
- Activate ASR env: `source venv-gigaam/bin/activate`.
- Transcribe file: `python mp4_notes.py transcribe input.mp4 --speaker-db speakers_ecapa.json -o out.md`.
- Live mode: `python mp4_notes.py live --list-devices` then `python mp4_notes.py live --system-device 0 --speaker-db speakers_ecapa.json`.
- Web UI: `cd frontend && pip install -r requirements.txt && python server.py` (http://127.0.0.1:8080).
- launchd services (macOS): `./recog-service.sh install|start|stop|status`.
- Note: diarization runs in a separate venv due to `torch` version constraints; keep both venvs working.
- If you change `sck_audio_capture.swift`, rebuild the checked-in binary:
  `swiftc -o sck_audio_capture sck_audio_capture.swift -framework ScreenCaptureKit -framework CoreMedia -framework AVFoundation`.

## Coding Style & Naming Conventions
- Python 3.12, 4-space indentation; keep CLI wiring in `mp4_notes.py` and reusable logic in `recog/`.
- Prefer `pathlib.Path`, type hints for new code, and small single-purpose functions.
- Naming: `snake_case` for functions/vars, `PascalCase` for classes, `*_worker.py` for subprocess helpers.

## Testing Guidelines
- No automated test suite yet. Before opening a PR, run `python -m compileall recog frontend` and do a smoke run relevant to your change (CLI help, short transcribe, or `frontend/server.py`).

## Commit & Pull Request Guidelines
- Commits are short and imperative (e.g., `Add ...`, `Fix ...`, `Refactor ...`); include scope when helpful (`frontend: ...`).
- PRs should include: what/why, how to verify, and screenshots for UI changes. Call out new env vars, model requirements, or macOS permissions needed.

## Security & Configuration
- Never commit secrets: copy `.env.example` → `.env` (HF token) and `frontend/.env.example` → `frontend/.env` (AI provider keys).
- Large media, logs, and transcripts belong in `logs/`/`meetings/` and are intentionally ignored by git.
