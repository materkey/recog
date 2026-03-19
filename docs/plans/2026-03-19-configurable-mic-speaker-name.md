# Configurable Mic Speaker Name

## Overview
- Replace hardcoded `"Me"` speaker name with a configurable value from `.env`
- New env var `MIC_SPEAKER_NAME` with default `"Vyacheslav Kovalev"`
- Affects live transcription output — both terminal and meeting storage

## Context (from discovery)
- Files involved:
  - `mp4_notes.py:140` — CLI `--mic-speaker-name` default `"Me"`
  - `recog/live.py:280` — `LiveAudioCapture.__init__` default `"Me"`
  - `recog/live.py:1135` — `live_pipeline()` default `"Me"`
  - `recog/config.py` — loads `.env` on import, defines constants
  - `.env` / `.env.example` — env var files
- `recog/config.py` already calls `load_dotenv()` at import time, and `mp4_notes.py` imports from `recog` before argparse setup — so env vars from `.env` are available when defaults are evaluated
- The launchd plist (`com.recog.live.plist`) doesn't pass `--mic-speaker-name` — it will pick up the new default automatically
- Frontend (`server.py`) doesn't start live transcription — only CLI does

## Development Approach
- Complete each task fully before moving to the next
- Make small, focused changes
- Run syntax check after changes

## Implementation Steps

### Task 1: Add MIC_SPEAKER_NAME to config and env files
- [x] Add `MIC_SPEAKER_NAME` constant to `recog/config.py` reading from `os.environ.get("MIC_SPEAKER_NAME", "Me")`
- [x] Add `MIC_SPEAKER_NAME=Vyacheslav Kovalev` to `.env`
- [x] Add `MIC_SPEAKER_NAME=Me` to `.env.example` with comment
- [x] Run `python -m compileall recog` — must pass

### Task 2: Wire config into CLI and live pipeline
- [ ] In `mp4_notes.py:140`, change default from `"Me"` to `MIC_SPEAKER_NAME` (import from recog)
- [ ] In `recog/live.py:280`, change `LiveAudioCapture.__init__` default to `None`, use `MIC_SPEAKER_NAME` inside
- [ ] In `recog/live.py:1135`, change `live_pipeline()` default to `None`, use `MIC_SPEAKER_NAME` inside
- [ ] Run `python -m compileall recog mp4_notes.py` — must pass
- [ ] Run `python mp4_notes.py --help` — verify `--mic-speaker-name` help shows new default

### Task 3: Verify acceptance criteria
- [ ] Verify `--mic-speaker-name` CLI arg still overrides the env var
- [ ] Verify existing meetings with "Me" are unaffected (read-only data)
- [ ] Run `python -m compileall recog frontend` — must pass
- [ ] Run `python mp4_notes.py --help` — CLI smoke test

## Technical Details
- `MIC_SPEAKER_NAME` env var → `recog/config.py` constant → used as default in CLI and pipeline
- CLI `--mic-speaker-name` still allows per-invocation override
- No migration needed — existing meeting JSONL files retain their original speaker names

## Post-Completion
- Run `./recog-service.sh restart` to apply changes to running launchd service
