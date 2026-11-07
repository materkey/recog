# Moving recog to Rust: high-level plan

Status: proposed, nothing started. The first piece exists outside the repository:
`speexdsp-aec`, a safe Rust port of the echo canceller pyaec wraps, bit-exact with it
(private repository `materkey/speexdsp-aec`, not wired into recog).

## Goal

No Python in recog, the speech model included. The core moves to Rust: the live
pipeline, the bridge, the web backend, the Copilot supervisor and channel, and ASR.
What stays: the Chrome extension and the static web UI (they run in a browser), the
Swift helpers (`sck_audio_capture`, `RecogZoomHelper.app`, `recog_launcher`: native
macOS APIs whose signatures carry the user's permissions), and the prompts.

What it buys: no GIL (the reason the microphone runs in a child process and echo
cancellation holds the interpreter), one binary instead of a venv and the macOS
constraints file, less memory (torch is the largest part), types where today errors
surface at run time.

## Approach

Replace one part at a time behind the contracts that exist today: the bridge's
WebSocket frames, the HTTP API, the storage formats (`transcript.jsonl`, `meta.json`,
the WAVs), `config.toml` and `.env`. The installation works after every step. The
Python tests act as acceptance tests and drive the Rust parts through their protocols
where they can.

## Steps

0. **Contracts and golden sessions.** Record sessions (extension frames, microphone and
   system audio, the resulting JSONL, WAVs and bridge frames) and a player that replays
   them into either implementation for A/B comparison. `scripts/check.sh` and CI gain
   `cargo test`, clippy and `#![forbid(unsafe_code)]`.
1. **ASR experiment.** Transcribe our recordings with the current GigaAM
   (`v3_e2e_rnnt`, PyTorch) and with Rust runtimes: `wavo` (pure Rust, Metal and the
   Neural Engine, GGUF Q8_0) and `gigastt` (ONNX Runtime, CUDA on Linux), on the Mac and
   on Z270XX. Measure WER against the current text, speed and memory, word timings,
   batching of several speakers in one pass, and how fast a refine can be aborted.
   Outcome: a runtime, or "not yet".
2. **PyO3 pilot.** `speexdsp-aec` replaces pyaec in `recog/echo_cancel.py`; the crate
   moves into the repository with that commit.
3. **ASR in Rust.** First behind `build_asr()` (an adapter to the chosen runtime, the way
   the remote backend is plugged in today), then inside the Rust process. torch and
   torchaudio leave the dependencies.
4. **Audio capture.** System audio (`parec`, `sck_audio_capture`, the restart policy),
   the microphone through cpal instead of sounddevice and `mic_worker`, the capture
   buffer. Checked against the golden recordings.
5. **Live process and bridge.** `live.py`, `per_speaker.py`, `refine.py` and
   `ktalk_bridge.py` as one tokio binary. Criterion: the same JSONL and the same bridge
   frames on the golden sessions. This is where most of the gain is; stopping here is a
   reasonable outcome.
6. **Web backend.** FastAPI to axum; the static UI unchanged.
7. **Copilot.** The supervisor (state machine, tmux, `systemd-run`, screen classifier)
   and the MCP channel (rmcp).
8. **Edges.** The `zoom_app` session logic (the Swift helper stays) and the KTalk desktop
   inspector.
9. **Installation without Python.** `setup.sh` and the launchd/systemd units run
   binaries; `Recog.app` starts the Rust binary, so the permissions stay with "Recog".
   Python modules are deleted as they are replaced; each step gets its CHANGELOG entry.

## Risks

- ASR text will change: a quantized model is not the PyTorch one, so the decision is a
  measured WER, not bit-exactness.
- Refine is aborted within milliseconds today by patching the decoder loop; the runtime
  has to allow the same.
- cpal and CoreAudio behave differently from PortAudio on device errors and reconnects.
- The bridge protocol has many subtleties (meeting owner, `localMicMuted`, the merged
  Copilot status); porting it without replaying sessions is unsafe.
- The Rust ASR runtimes are young: pin the version, possibly keep a fork.
- Steps 5 to 7 are months of work.

## First step

Steps 0 and 1: the ASR experiment decides whether Python can go entirely, and the golden
sessions are needed by every step after it.
