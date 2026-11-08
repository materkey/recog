# Moving recog to Rust: high-level plan

Status: in progress.
- Step 1 is done: [the ASR experiment](../experiments/2026-11-07-rust-asr-runtimes.md)
  picked `wavo`, on Metal on the Mac and on Vulkan on Linux.
- Step 0 is designed: [golden sessions](2026-11-07-golden-sessions-design.md).
- Step 2 is done: the echo stage runs `speexdsp-aec` (`rust/`) instead of pyaec, and
  `scripts/check.sh` and CI check the Rust workspace.
- Step 3: `wavo` behind `build_asr()` is the default on macOS (`"auto"`).
- Step 4 has the microphone: the opt-in worker `rust/recog-mic` behind
  `[live] mic_worker`.
- Steps 4 and 5 have the lower layer of the live process ported, not yet wired:
  `rust/recog-live` (capture, echo stage, ASR on `wavo`, per-speaker ingest, refine,
  golden recorder).
- Step 5 has the bridge ported, not yet wired: `rust/recog-bridge`.
- Step 8 has the Zoom application source ported, not yet wired: `rust/recog-zoom`.
- Step 6 has the web backend ported, not yet wired: `rust/recog-web`.

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
   them into either implementation for A/B comparison
   ([design](2026-11-07-golden-sessions-design.md)). `scripts/check.sh` and CI gain
   `cargo test`, clippy and `#![forbid(unsafe_code)]` with the first Rust code in the
   repository (step 2).
1. **ASR experiment.** Transcribe our recordings with the current GigaAM
   (`v3_e2e_rnnt`, PyTorch) and with Rust runtimes: `wavo` (pure Rust, Metal and the
   Neural Engine, GGUF Q8_0) and `gigastt` (ONNX Runtime, CUDA on Linux), on the Mac and
   on Z270XX. Measure WER against the current text, speed and memory, word timings,
   batching of several speakers in one pass, and how fast a refine can be aborted.
   Outcome: a runtime, or "not yet".
   **Done 2026-11-07** ([report](../experiments/2026-11-07-rust-asr-runtimes.md)).
   - On the Mac, `wavo` 0.4.0 on Metal matches PyTorch's WER within 0.06 points and
     gives the same word timings. It is 6-12× faster, and a 60-100 ms refine needs no
     abort hook.
   - On Linux, `wavo` on Vulkan has the same WER and is 4.5× faster than PyTorch on
     CUDA. `gigastt` matches PyTorch on CPU, but its CUDA build has no Blackwell
     kernels.
2. **PyO3 pilot.** `speexdsp-aec` replaces pyaec in `recog/echo_cancel.py`; the crate
   moves into the repository with that commit. **Done 2026-11-07.**
   - The module is bit-exact with pyaec and three times faster per block, and runs
     without the interpreter lock.
   - `setup.sh` builds it on macOS when cargo is present; without cargo, echo
     cancellation stays unavailable.
   - The golden-data generators stay in the port's repository.
3. **ASR in Rust.** First behind `build_asr()` (an adapter to the chosen runtime, the way
   the remote backend is plugged in today), then inside the Rust process. torch and
   torchaudio leave the dependencies. `wavo` has no batch and no abort; both are
   unnecessary at its speed, so the adapter runs clips one after another and lets a
   refine finish.
   - **Adapter done 2026-11-07**, opt-in: `RECOG_ASR_BACKEND=wavo` (`recog/wavo_backend.py`,
     module from `rust/wavo-asr-py`). Through recog's own refine call on 80 labelled
     windows it gives the same text as PyTorch on 69, the same word starts and ends
     (p95 Δ 0 ms), in 84 ms against 865 ms.
   - The live process on `wavo` no longer imports torch, torchaudio or gigaam: the
     local module is imported only when a PyTorch backend is built.
   - **Default on macOS 2026-11-08** (`[asr] backend = "auto"`), after a KTalk test
     call on it; without the built module "auto" runs on PyTorch and says so.
   - Still to do: measure `wavo` on a Linux host without a GPU, then make it the
     Linux default too; drop torch from the dependencies with the Python ASR path.
4. **Audio capture.** System audio (`parec`, `sck_audio_capture`, the restart policy),
   the microphone through cpal instead of sounddevice and `mic_worker`, the capture
   buffer. Checked against the golden recordings.
   - **Microphone worker done 2026-11-08**, opt-in: `[live] mic_worker = "rust"` runs
     `rust/recog-mic` (cpal) with the Python worker's arguments, frames and exit codes.
     Under a LaunchAgent, 30 minutes, two of each side by side: Python 1 and 3 holes
     (9 and 26 ms, each an "IOWorkLoop: skipping cycle due to overload" in the system
     log), Rust none. cpal changes the device's rate and physical format to what a
     stream asks for, so the worker opens the device as it is and resamples itself.
   - Still to do: make it the default after a real meeting on it.
   - **System audio and the capture buffer ported 2026-11-08**, not wired:
     `rust/recog-live` (`capture`). Its differential tests feed the same blocks, byte
     streams cut at random points and scripted `pactl` answers to Python and Rust and
     get the same chunks, capture times, frames and log lines; the restart policy runs
     beside Python's on the same stub child with the same backoff timeline.
5. **Live process and bridge.** `live.py`, `per_speaker.py`, `refine.py` and
   `ktalk_bridge.py` as one tokio binary. Criterion: the same JSONL and the same bridge
   frames on the golden sessions. This is where most of the gain is; stopping here is a
   reasonable outcome.
   - **Lower layer ported 2026-11-08**, not wired: `rust/recog-live` has the echo stage
     on `speexdsp-aec` directly, ASR on the `wavo` crate, the per-speaker ingest, the
     refine worker and the golden recorder. Against Python on the same input: the
     ingest's batches, refine windows and watchdog on 400 random scenarios and the
     golden files byte for byte; the echo stage is bit-exact wherever it passes the
     microphone raw and within one float32 step elsewhere (the FFT library rounds
     differently from numpy's); `wavo` in Rust gives the text and word timings of
     `wavo_asr` on every compared clip.
   - Still to do: the main loop (`live.py`, `cli.py`) and the bridge in the same
     process.
   - **Bridge ported 2026-11-08**, not wired: `rust/recog-bridge`, a library with the
     boundary toward the live process as traits (the callbacks `LiveTranscriber`
     registers today). The bridge cases of the Python suites run against it, and its
     differential tests run `recog/ktalk_bridge.py` and the port on the same scripted
     sessions (extensions with Origin headers, the webapp, the supervisor, the channel,
     a real storage, one scenario over real WebSockets) and compare every frame, live
     call, storage file and log line. The five local golden sessions replayed through
     both gave the same.
     The Copilot policy and the increments come from `recog-ai`.
   - Still to do: `recog-live` hosting it.
6. **Web backend.** FastAPI to axum; the static UI unchanged.
   - **Ported 2026-11-08**, not wired: `rust/recog-web`, binary `recog-web`, every
     route of `recog/web/server.py`, the live stream and the bridge subscriber, on
     `recog-core` and `recog-ai`. Its differential tests run the Python server and the
     Rust one on copies of the same synthetic data (stub CLIs, a fake bridge each) and
     compare 197 requests and two browser and bridge sessions: status codes, the
     headers that matter, bodies, audio bytes, prompts, files written and frames. The
     KTalk desktop integration stays Python (its status is read from its files).
   - Still to do: a setting that runs it as the frontend service, and a real meeting on
     it.
7. **Copilot.** The supervisor (state machine, tmux, `systemd-run`, screen classifier)
   and the MCP channel (rmcp).
   - **Supervisor ported 2026-11-08**, not wired: `rust/recog-copilot`, binary
     `recog-copilot-supervisor`. Its differential tests feed the same lifecycle events,
     frames, panes and settings files to both implementations and compare the output;
     its protocol tests run both against a fake bridge and a fake tmux. It reads its
     settings from the environment, `.env` and `[copilot]` of `config.toml` until
     `recog-core` provides them.
   - Still to do: the MCP channel; then a setting that runs the Rust supervisor, and a
     real meeting on it.
8. **Edges.** The `zoom_app` session logic (the Swift helper stays) and the KTalk desktop
   inspector.
   - **`zoom_app` ported 2026-11-08**, not wired: `rust/recog-zoom` (library and the
     standalone `recog-zoom-app`). Its tests run `recog/zoom_app.py` on the same input
     and compare exactly: the Python test scenarios, sequences rebuilt from recorded
     Accessibility dumps and 400 seeded random runs (about 12 800 events), the
     installed Zoom's wording, and (on request, two Swift builds) a real helper build,
     whose bundle is identical file by file to the Python one's.
   - Still to do: start it from the live process instead of the Python thread.
9. **Installation without Python.** `setup.sh` and the launchd/systemd units run
   binaries; `Recog.app` starts the Rust binary, so the permissions stay with "Recog".
   Python modules are deleted as they are replaced; each step gets its CHANGELOG entry.

## Target layout

Crates under `rust/`, one decision per crate, dependencies pointing down the list:

| Crate | Replaces | Kind |
|---|---|---|
| `recog-core` | `install_config.py`, `config.py` (data directory), `user_profile.py`, `meeting_storage.py`, `ws_origin.py`, `privacy.py` | library, ported (parity-tested against Python; no caller yet) |
| `recog-ai` | `ai_provider.py`, `ai_settings.py`, `ai_defaults.py`, `prompt_registry.py`, `increment.py`, `title_generator.py`, `meeting_chat.py` | library |
| `recog-zoom` | `zoom_app.py` (the Swift helper stays) | library, ported, not wired |
| `recog-bridge` | `ktalk_bridge.py` | library, ported, not wired |
| `recog-live` | `live.py`, `per_speaker.py`, `refine.py`, `audio_capture.py`, `system_audio.py`, `mic_capture.py`, `echo_cancel.py`, `golden.py`, `cli.py`; ASR through the `wavo` crate | binary `recog`, the bridge in the same tokio process; the modules below the main loop ported (parity-tested against Python; no caller yet) |
| `recog-web` | `recog/web/*.py` (the static UI stays as it is) | binary |
| `recog-bridge` | `ktalk_bridge.py` | library |
| `recog-live` | `live.py`, `per_speaker.py`, `refine.py`, `audio_capture.py`, `system_audio.py`, `mic_capture.py`, `echo_cancel.py`, `golden.py`, `cli.py`; ASR through the `wavo` crate | binary `recog`, the bridge in the same tokio process |
| `recog-web` | `recog/web/*.py` (the static UI stays as it is) | binary, ported, not wired |
| `recog-copilot` | `copilot_supervisor.py`, `copilot_channel.py` (MCP through rmcp) | binaries |
| `recog-mic` | `mic_worker.py` | binary, done |

Shared rules: edition 2024, `#![forbid(unsafe_code)]` in our code, clippy with
`-D warnings`, tokio for anything asynchronous, serde for every file and frame format.
Log lines keep the Python prefixes (`[KTalk Bridge]`, `[mic]`): tools and people grep
them. A Rust part is accepted by the same files and frames the Python one produces:
parity tests run both on the same input and compare the output, and the live process
by golden replay. Python stays the default until its Rust replacement has passed that
and a real meeting; each switch is a setting first, then the default, then the Python
module goes.

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
