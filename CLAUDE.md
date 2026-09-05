# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Recog** is a Russian speech recognition system with live speaker identification via KTalk. It combines:
- **GigaAM** (v3_e2e_rnnt) for Russian ASR
- **SpeechBrain ECAPA-TDNN** for speaker embeddings

## Commands

### Setup
```bash
# Full setup on macOS and Linux (venv, deps, GigaAM from a git pin, config
# files, extension build, weight cache, service units). Idempotent — this is
# also how you update after a git pull.
./setup.sh

# Flags: --no-services | --no-prefetch | --no-extension
# Call-shell env (NOT .env): RECOG_TORCH_INDEX_URL, RECOG_GIGAAM_URL/REF,
# RECOG_GIGAAM_EDITABLE (local checkout installed with pip install -e)
RECOG_TORCH_INDEX_URL=https://download.pytorch.org/whl/cu124 ./setup.sh
```

Dependencies: `requirements.txt` + `frontend/requirements.txt`, plus
`requirements-macos.txt` on darwin. **Every macOS pip step runs under
`-c constraints/macos.txt`** (the verified torch/torchaudio/numpy/pyobjc set) —
install into `venv-gigaam` with the same constraint file, because a bare
`pip install` can bump the ML stack the MPS contracts were measured against.
GigaAM is deliberately not in `requirements.txt`: it comes from the git pin in
`setup.sh` (`GIGAAM_REF`), mirrored in `.github/workflows/ci.yml`.

### Running
```bash
# Always use venv-gigaam for main CLI
source venv-gigaam/bin/activate

# Live with the conference Chrome extension + system audio (backend per OS)
python mp4_notes.py live --ktalk --system-audio auto

# Explicit backends: screencapture (macOS) | pulse (Linux) | device:<id> | none
python mp4_notes.py live --system-audio pulse --speaker-db speakers_ecapa.json

# List audio devices (on Linux also prints `pactl list short sources`)
python mp4_notes.py live --list-devices

# Add speaker to database (records 5 seconds from mic)
python add_speaker_live.py "Speaker Name"
```

`--screencapture` and `--system-device <id>` remain as hidden aliases of
`--system-audio screencapture` / `--system-audio device:<id>`.

### Frontend (web UI)
```bash
# Deps are already installed into venv-gigaam by ./setup.sh; normally the
# frontend runs as a service unit and this is only for debugging.
source venv-gigaam/bin/activate
python frontend/server.py  # http://127.0.0.1:9080
```

### Services (launchd / systemd --user)
```bash
./recog-service.sh install            # Render + register the 4 units (live + frontend + copilot-channel + copilot-supervisor) WITHOUT starting them
./recog-service.sh start              # Start all installed services (readiness-gated)
./recog-service.sh status             # Show service status
./recog-service.sh logs               # Tail live transcription logs
./recog-service.sh restart            # Restart after code changes
./recog-service.sh restart-supervisor # Restart copilot supervisor daemon only
./recog-service.sh logs-supervisor    # Tail copilot supervisor daemon logs
./recog-service.sh render --dry-run D # Render units into D only (tests/CI)
```

The backend is chosen from `uname -s` (`RECOG_SERVICE_OS` overrides it, which
only changes which templates/verbs are used, so a Mac can render Linux units):
launchd on macOS, `systemd --user` on Linux. Units are rendered from
`deploy/{launchd,systemd}/*.in` by `scripts/service/{common,launchd,systemd}.sh`
behind the `svc_*` contract. `install` never activates anything — that split is
what lets `setup.sh` verify dependencies before starting a pipeline that cannot
serve; `start`/`restart`/`restart-*` run the same probes (`scripts/deps.sh`) and
refuse to activate an incomplete install rather than stop a working one
(`RECOG_SKIP_READINESS=1` bypasses the gate). On Linux the
services need `loginctl enable-linger $USER` to survive logout (`status` prints
the reminder).

**Important**: After modifying `frontend/server.py` or other backend code, run `./recog-service.sh restart` to apply changes. After modifying `recog/copilot_supervisor.py`, run `./recog-service.sh restart-supervisor`.

Manual copilot launch (`./recog-service.sh claude`) stays available for debugging; it now passes the same `--strict-mcp-config --mcp-config` flags the supervisor uses, so the `copilot` MCP server is registered.

### Chrome Extension (KTalk speaker detection)
```bash
cd kontur-diarize/ktalk-speaker-extension
npm install        # First time only
npm run build      # Rebuild after changes to src/
```
After rebuild, reload in Chrome: `chrome://extensions` → Developer Mode → Reload.

The extension connects to `ws://localhost:8765` (ktalk_bridge.py) and sends speaker signals from conferences on multiple platforms: KTalk, Jitsi (`vas3k`/`vkkovalev`), and Matrix/Element Call (`matrix`). Each platform has an adapter in `src/platforms/` (`ktalk.js`, `vas3k.js`, `matrix.js`) selected by `detectPlatform()` in `src/platforms/index.js`; `manifest.json` declares one `content_scripts` entry per platform host.

**Element Call (matrix)** — Element Call is bundled inside Element Web as a same-origin iframe (`https://element.vkkovalev.work/widgets/element-call/index.html`); the standalone `call.element.*` host is unused for room calls. The manifest scopes the Element match to `/widgets/element-call/*` with `"all_frames": true`, so the content script injects **only into the EC iframe, never the Element top frame** (a top-frame injection would open a stray second WS that outlives the call). `ElementCallAdapter` (`src/platforms/matrix.js`) sources lifecycle/participants/names from page globals `window.rtcSession` (MatrixRTCSession) + `window.matrixclient` (matrix-js-sdk), and active-speaker events from a React fiber-walk (`__reactFiber$*` → `.return` up to the livekit `Room`). Participant identity is the legacy plain format `${userId}:${deviceId}` (e.g. `@test2:matrix.vkkovalev.work:IZFUKTQLPX`), parsed by `parseRtcIdentity()` with a sha256-hash fallback for future MatrixRTC. Display names come from `matrixclient.getRoom(roomId).getMember(userId).name`. The meeting URL is a **synthesized** `https://element.vkkovalev.work/#/room/<roomId>` (via the optional `getMeetingUrl()` adapter hook) so the backend derives a stable `live_id` from the room fragment instead of `index.html`. Matrix uses the same mixed-audio path + floating panel (transcript + copilot chat view) as the Jitsi platforms; per-participant PCM/refine (Tier 2) is out of scope. See ADR/plan `docs/plans/completed/20260709-element-call-matrix-platform.md`.

### System audio backends
Both backends are the same `SubprocessPCMCapture` (`recog/system_audio.py`): a
child process writes raw mono float32 PCM at 16 kHz to stdout, a reader thread
appends it to the shared `BufferedAudioCapture`. `resolve_system_audio_backend`
turns `auto` into `screencapture` on darwin and `pulse` on linux (and rejects a
backend asked for on the wrong OS); `create_system_capture` builds it.

- **macOS — `screencapture`**: the ScreenCaptureKit Swift helper
  `sck_audio_capture`. `find_or_build_sck_utility()` compiles it on demand from
  `sck_audio_capture.swift` (both live at the repo root, outside the package,
  and the binary is gitignored). Manual rebuild:
  ```bash
  swiftc -o sck_audio_capture sck_audio_capture.swift \
      -framework ScreenCaptureKit -framework CoreMedia -framework AVFoundation
  ```
- **Linux — `pulse`**: `PulseAudioCapture` runs `parec --raw
  --format=float32le --rate=16000 --channels=1 --latency-msec=100
  --device=<monitor>` (PulseAudio or PipeWire via `pipewire-pulse`). A
  non-empty `RECOG_PULSE_MONITOR_SOURCE` pins the source and turns the follow
  logic off; otherwise `start()` resolves the default sink itself and pins
  `parec` to that concrete `<sink>.monitor` (falling back to
  `@DEFAULT_MONITOR@` when `pactl` is unavailable — the first successful poll
  then reconnects to a concrete monitor instead of adopting the current sink
  as its baseline), then **polls
  `pactl get-default-sink`** and explicitly reconnects when the default sink
  changes — the `@DEFAULT_MONITOR@` name alone is resolved once at stream
  creation and does not follow, and taking the *first poll* as the baseline
  would silently adopt a sink switched between the spawn and that poll,
  leaving the capture on the old output. A planned reconnect marks the source
  boundary (`mark_source_boundary()`: buffer dropped, `source_epoch` bumped, so
  no overlap is glued across the gap and `_process_chunk` tells the WAV writer
  to write the first post-switch chunk whole instead of cutting 0.5 s that
  never reached the file) and does not spend the crash-retry budget;
  a transient `pactl` error is rate-limited-logged and retried. Without
  `--device`, `parec` would record the default *source* (the mic), so the
  monitor name is mandatory.

Restart policy is shared: a died child is respawned with a bounded backoff
(1/2/5/10 s) charged **before every respawn attempt**, not only before a retry
after a failed one — a child that starts fine and dies half a second later
otherwise burned the whole budget in ~3 s without a single pause. The wait
**does not leave the reader loop while `running`** (and a `stop()` arriving
during it aborts the respawn instead of orphaning a fresh child); only after
`MAX_RESTARTS` does the capture give up, set `_dead`/`running=False`, and let
`recog/live.py` write one system-capture-lost marker (with the child's last
stderr lines appended). That marker is
separate from `_emit_pcm_transition_marker` on purpose: a mixed-capture death
must not claim a PCM→mixed transition or pause refine.

## Architecture

### Single-venv Architecture
The project uses a single `venv-gigaam` virtual environment. Speaker identification is handled via the KTalk Chrome extension (real-time) and SpeechBrain ECAPA-TDNN embeddings (enrollment). The former dual-venv pyannote subprocess architecture (ADR-0006) is obsolete.

### Remote ASR offload (opt-in)
`FallbackASR` implements all four GigaAM paths (`transcribe_batch` per-speaker, `transcribe_simple` offline, `transcribe_with_timestamps` refine, and the direct mic/mixed `transcribe_chunk`) over a network gateway (Z270XX, OpenAI-Whisper-compatible). Three are reached today through `build_asr()` (the single constructor, `recog/remote_asr.py`) via the live pipeline; the offline `transcribe_simple` path is implemented but not yet wired to a CLI command (`mp4_notes.py` has only `live`/`meetings`). When constructed, ASR comes exclusively from `build_asr()`: `RECOG_ASR_BACKEND="local"` (committed default — repo behavior unchanged) returns a plain `ASRModule`; `"remote"` returns a `FallbackASR` wrapping a `RemoteASRClient`. `FallbackASR` constructs the local `ASRModule` eagerly but loads its weights **lazily in a background thread** — triggered by the first remote failure. While neither backend can serve (circuit open + local still loading) it raises `ASRTemporarilyUnavailable` and exposes `available=False`; consumers (`_process_chunk`, `PerSpeakerIngest.tick`, `RefineWorker`) gate on it and keep their audio buffered instead of dropping chunks (see `docs/plans/2026-06-02-asr-fallback-buffering-design.md`). The circuit breaker is thread-safe and trips after `RECOG_ASR_CIRCUIT_FAILS` consecutive failures (open for `RECOG_ASR_CIRCUIT_OPEN_S`, then one half-open probe). Live chunk/batch requests use the shorter `RECOG_ASR_LIVE_TIMEOUT_S`; refine/offline keep `RECOG_ASR_REMOTE_TIMEOUT_S`. It delegates `model` / `device` / `_model_lock` / `_maybe_empty_mps_cache` to the local module so `RefineWorker` keeps working; refine skips the abortable-decode monkey-patch when `asr.using_remote`. Gateway patch + deploy steps live in-repo at `deploy/z270xx/`.

### ASR fallback buffering (always-on)
No speech is lost when ASR is temporarily unavailable (remote down + local loading): capture buffers (`SingleDeviceCapture`, `SubprocessPCMCapture`) are not consumed while `asr.available` is False (capped at `ASR_PENDING_BUFFER_CAP_S`), `get_chunk()` returns `(chunk, capture_start_ts)` so backlog segments get spoken-time timestamps (while the buffer holds ≤ `CAPTURE_RESYNC_MAX_S` the start timestamp is re-anchored to wall-clock on every captured block — sample counting alone drifts forever because the 0.5 s overlap never lets the buffer empty), and a chunk caught mid-failure is held in a single-slot retry (`SourceState.pending_chunk`, mirrors `PerSpeakerIngest._pending_retry_batch`) and re-dispatched first once ASR recovers — without re-writing it to the meeting WAV. Live dedup applies only within `DEDUP_MAX_GAP_S` of the previous segment (a phrase repeated after a pause is real speech). Swallowed ASR exceptions are logged via `recog/log_utils.py` (rate-limited).

### Meeting audio recording (24h retention, on by default)
`live` records per-meeting WAVs (`audio_mic.wav` / `audio_system.wav`, 16 kHz mono int16) for segment playback via `GET /api/meetings/{id}/audio/{segment_index}`; disable with `--no-save-audio`. Load-conscious design:
- Silent chunks (`rms < VAD_RMS_THRESHOLD`) are not written — EXCEPT while PCM-primary, where PCM segment offsets are extrapolated from a wall-clock↔WAV anchor and need a continuous real-time timeline in `audio_system.wav`. Mixed-path segments capture their offset at write time, so gaps are safe for them.
- PCM-primary segment offsets (`_on_pcm_batch`): `audio_offset_ms` is extrapolated from `_system_wav_anchor` = `(wall_ms of the last system write's first sample, WAV offset before it)`, updated on every `_process_chunk` system write (`write_chunk` returns the skipped-overlap sample count). Offset = `anchor_wav + (chunk_start_ms - anchor_wall)`, keeping consecutive segments' offsets tracking their real capture times. The old `wav_offset_now - chunk_age` formula drifted 0–5 s (the WAV sample count advances in `chunk_duration` steps while chunk age is continuous), collapsing consecutive segments onto one offset. Fallback to the old formula when the anchor is absent (before the first write) or stale (`> PCM_WAV_ANCHOR_MAX_AGE_MS` — degraded-mode silence-skip broke the timeline ahead of it).
- WAV headers are re-patched after every chunk (`IncrementalWavWriter._patch_header_sizes`), so a SIGKILL (a service-manager restart — launchd or `systemd --user` — does not run teardown; sessions end as `stale_session`) never leaves a header claiming 0 data bytes. Per-file cap `MAX_WAV_DATA_BYTES` (2 GiB) protects the format's 32-bit sizes.
- Retention: `MeetingStorage.cleanup_old_audio(retention_hours=24)` deletes WAVs by file mtime (an actively-written file is always fresh — the current meeting is safe) with a stat-only scan (meta.json parsed only for dirs that still have audio). Runs at `start_session` and hourly from the live loop.
- Every meeting switch must rotate writers via `_on_meeting_change`: the KTalk bridge callback does it, and `_check_silence_split` does it for orphan silence-splits (with `continuation=True` + re-writing the triggering chunk so the first segment points into the new meeting's own file). A switch path that skips rotation reproduces "No audio file available for this meeting". Writer rotation also resets `_system_wav_anchor` (the new WAV starts a fresh timeline); the first system write of the new meeting re-seeds it.
- Playback slice duration is bounded per track (`compute_segment_duration_ms` in `frontend/server.py`): mic/system WAV timelines diverge under silence-skip, so only same-`is_local` segments bound each other.

### Per-speaker ASR path (ADR-0018)
For any active KTalk meeting the pipeline routes through per-participant PCM streams (no mixed-audio fallback as a configurable mode):

- Chrome extension attaches `AudioWorkletNode` to each remote `MediaStream` (`kontur-diarize/ktalk-speaker-extension/src/audio-worklet/downsampler.js`, `src/pcm_transport.js`), downsamples 48 kHz → 16 kHz Int16 PCM and sends chunks via WS (`type: "pcm_chunk"`) to `ktalk_bridge.py`.
- Bridge dispatches chunks to `PerSpeakerIngest` (`recog/per_speaker.py`) which per-pid reorders by `seq`, batches across speakers (200 ms window, max 8), and calls `ASRModule.transcribe_batch()` (`recog/asr.py`). Attribution comes from `speaker_name` already in the frame — no `get_dominant_speaker_for_period()` call.
- Watchdog: if PCM is silent for `PCM_SILENCE_WATCHDOG_S` (30 s) during an active meeting, `PerSpeakerIngest._degraded` flips true, `LiveTranscriber._process_chunk` falls back to the existing mixed-path with `source="fallback_mixed"` and a gap-marker written to the transcript. Recovery marker is inserted when PCM returns.
- Local mic (`MIC_SPEAKER_NAME`) stays on `sounddevice` — it is not routed through the worklet (D9).

See `docs/plans/2026-04-22-per-speaker-asr-pipeline.md` for the decisions table and task breakdown.

### Per-speaker refine contour
A tier-2 refine pass re-transcribes per-pid PCM windows with word-level timestamps and replaces live-segment text in `transcript.jsonl` with `refined: true`:

- State ownership (design.md D11):
  - `recog/refine.py` — `RefineWorker` daemon thread + queue + `make_abortable_decode` decoder monkey-patch + `RefineTask` / `RefineResult` dataclasses.
  - `recog/per_speaker.py` — per-pid PCM ring (`_pcm_rings`), monotonic audio counter (`_pid_audio_ms`), silence/continuous-speech counters (`_silence_ms`, `_continuous_speech_ms`), refine watermark. Trigger via `get_refine_candidate(pid)`.
  - `recog/live.py` — per-pid live-segment registry `_pcm_live_segments[pid] = [(segment_id, start_ms, end_ms), ...]`, `_pcm_segment_texts` cache for identical-text skip, `_apply_refine` alignment + `update_segments` call.
  - `meeting_storage.py` — `TranscriptSegment.refined: bool = False`; `update_segments(...)` accepts optional `text` per row, sets `refined=True` when present.
- Flow: PCM arrives via `PerSpeakerIngest.enqueue` → per-pid ring + counters update → tick loop calls `get_refine_candidate(pid)` → gap (`SPEAKER_TURN_GAP=3s` silence) or monolog cap (`PCM_REFINE_MONOLOG_CAP_S=15s`) assembles a 20 s trailing window → `RefineWorker.submit` → worker acquires `asr._model_lock`, runs `transcribe_with_timestamps` → `_apply_refine` rebases words via `task.offset_base_ms`, intersects with live segments under `_state_lock` + meeting-generation check → `update_segments` rewrites JSONL rows.
- Shared model lock: `ASRModule._model_lock` is a `threading.RLock` (not `Lock`). The refine worker acquires it externally to serialise with the live batched-forward path; `transcribe_with_timestamps` re-acquires internally — recursion must not deadlock.
- Cooperative abort: default `abort_mode="cooperative"`. `LiveTranscriber._refine_transcribe_fn` installs `make_abortable_decode` on `model.decoding.decode` for the duration of one refine call; the hook polls `should_abort()` at the top of the RNN-T time-step loop and raises `RefineAborted`. Worker catches, releases the lock, re-enqueues the task. `LiveTranscriber._on_pcm_batch` calls `refine_worker.set_live_pending()` / `clear_live_pending()` around `asr.transcribe_batch` so live gets priority. Per the spike (`experiments/2026-04-24-refine-abort-spike.md`), cooperative abort saves ~770 ms per preempted refine on MPS (~80 % of inference cost).

On refine arrival the webapp renders tier-1 (opacity 0.6, dashed border) → tier-2 (solid) segments via `data-refined` on `.transcript-segment`; the KTalk extension overlay does a plain text swap with a brief `.refine-fade` pulse.

See `docs/plans/2026-04-24-per-speaker-refine-backend.md`, `openspec/changes/per-speaker-stream-refine/`, and ADR-0019 for the decisions table and task breakdown.

### Auto-spawn Copilot supervisor (meeting-triggered; code default on, `.env.example` ships it off)
A service daemon (`deploy/launchd/com.recog.copilot-supervisor.plist.in` or `deploy/systemd/recog-copilot-supervisor.service.in` → `recog/copilot_supervisor.py`) spawns a fresh headless Claude Code copilot session per conference meeting (any `is_conference_source`: KTalk / Jitsi vas3k·vkkovalev / Matrix) and tears it down after the meeting, so the manual `./recog-service.sh claude` step disappears. Lifecycle model is "по триггеру" (variant B): fresh session per meeting, zero cost between meetings, ~6–10 s cold start.

- `claude -p` is impossible (`--dangerously-load-development-channels` isn't parsed by print mode, `channel_enable` needs a marketplace plugin) — the only path is an interactive TUI in a PTY. The supervisor runs it in a detached tmux on a dedicated socket (`tmux -L recog-copilot`) so its env is deterministic (from the service unit) and isolated from the personal tmux.
- **State machine** (`SessionLifecycle`, pure/no-I/O): `IDLE → SPAWNING → RUNNING → GRACE`. Inputs `meeting_started(id)/meeting_ended(id)/timer/session_died/stop_now(id)` yield commands `spawn(id)/kill/none`. A flap (`meeting_ended` then `meeting_started` with the **same** id within grace — guaranteed by `meeting_storage._find_same_day_meeting`) keeps the session alive; a different id kills + respawns. `RECOG_COPILOT_GRACE_S` (default 90) is the kill delay after `meeting_ended`; an explicit webapp Stop (`stop_now`) kills immediately with no grace — the grace window only shields conference rejoin flaps.
- **BridgeClient**: WS to `ktalk_bridge` (default `ws://127.0.0.1:8766`) with reconnect/backoff. On connect sends `supervisor_hello`; bridge replies `meeting_state {active, meeting_id, meeting_title}` for reconciliation. `active` means a copilot session should exist: either an `is_conference_source(meta["source"])` meeting (ktalk / vas3k / vkkovalev / matrix) with a live extension client, or the bridge's armed manual-session mirror — never merely `current_meeting_path`, which is always populated. Reconcile branches: live session + no meeting → synthetic `meeting_ended` → GRACE (not instant kill, so a bridge restart mid-meeting doesn't kill the copilot — rejoin returns the same id); no session + active meeting → spawn; live session + active meeting → adopt (no respawn).
- **Watchdog** (~2 s while a session should live): `classify_screen(text) → spawning|online|working|blocked|offline` by fixed-order rules; the known startup dialogs (dev-channels, workspace-trust) are auto-dismissed tmux-natively (`send-keys 1`, per-dialog latch); `spawning` past a 60 s deadline → `blocked` (fail-visible). Reports `copilot_agent_status {status, meeting_id}` on change + a ~5 s heartbeat; `offline` feeds `session_died` → respawn with backoff (5/15/30 s, ≤3/meeting).
- **Status merge lives in bridge (D4)**: two authors — supervisor (screen) via `copilot_agent_status`, and `recog/copilot_channel.py` (MCP-session count) via `claude_sessions {count|null}` (`null` = unknown, not a confident 0). Bridge computes the effective status (`offline` if the supervisor is silent > 10 s OR a debounced `sessions==0` while either a conference or manual session should be alive) and broadcasts `copilot_status {status: "agent_state", ...}` to extension and subscriber clients only on effective-status change. The extension and webapp render their chips (🟢 online / 🟡 working / 🔴 blocked / ⚫ offline) independently of per-request indicators.
- **Linux session lifecycle (independent of the supervisor)**: on Linux `TmuxRunner` wraps `tmux new-session -d` in a transient user unit — `systemd-run --user --collect --unit=recog-copilot-session --property=Type=oneshot --property=RemainAfterExit=yes --property=KillMode=control-group` — so the session lives outside the supervisor's cgroup and `systemctl --user restart recog-copilot-supervisor` does not take a live Copilot down with it; the restarted supervisor re-adopts it through the usual `alive()` probe. `Type=oneshot` + `RemainAfterExit=yes` match `tmux new-session -d` (it forks the server and returns, so without them the unit would be considered finished and garbage-collected the moment tmux detaches); no `Restart=` — respawn is the supervisor's decision. Env the user manager would not otherwise pass is forwarded explicitly (`SESSION_ENV_KEYS`: PATH, HOME, TERM, SHELL, LANG). A `RemainAfterExit` unit stays *loaded* after its session dies and `systemd-run` refuses to reuse a loaded name, so `_reset_session_unit()` stops it and waits for the name to drop before the next spawn. The unit is never installed or enabled; `restart`/`restart-supervisor` do not touch it, explicit Stop/`uninstall` clear it plus both tmux names. Detection is `platform.system() == "Linux" and shutil.which("systemd-run")` (`_linux_session_manager`), so a systemd-less Linux falls back to plain tmux; macOS keeps the plain tmux path unchanged. The watchdog still probes tmux, not the oneshot unit's `active` state.
- **Master switch** `RECOG_COPILOT_AUTOSPAWN=1|0` (default 1) keeps the daemon connected; `0` makes only the meeting frames (`meeting_started`/`meeting_state`/`meeting_ended`) inert — manual spawn/stop and crash-respawn (`on_session_died`) work regardless of the flag (webapp plan D3).
- **Manual sessions from the webapp** (`#/live`): a "Recog Copilot" button with agent-state chip + chat panel in `frontend/static/app.js`. Spawn/stop travel as `copilot_spawn_request {request_id}` / `copilot_stop_request {request_id}` (browser → `/api/live/stream` → frontend server → bidirectional `BridgeSubscriber` → bridge → supervisor); the supervisor maps them onto the constant synthetic lifecycle id `"manual"` (`MANUAL_MEETING_ID`, mirrored in both `ktalk_bridge.py` and `recog/copilot_supervisor.py`), so a repeated spawn is the existing same-id flap (an explicit click also resets the respawn backoff budget when no retry is pending — Start revives a session parked after 3 exhausted respawn attempts) and Stop is `stop_now` → immediate kill (no grace; the webapp reports a stop as lost if no ⚫ offline arrives within 15 s). Closing or reloading the tab does NOT stop the session — the browser holds no session state and no stop frame is sent on WS close. Error echoes bound to the originating click's `request_id`: `bridge_disconnected` (frontend server, bridge link down), `supervisor_disconnected` (bridge, no supervisor client), `conference_active` (bridge rejects spawn — never forwarded while a conference is already active; a conference starting mid-relay may see the frame delivered, but the manual flag is not armed and the originator still gets `conference_active`). Shared-audience Copilot broadcasts (`copilot_reply` plus broadcast `copilot_status` frames) fan out to `_subscriber_clients` as well as extension clients; per-request rejection echoes remain originator-only and `manual_preempted` is subscriber-only. Manual chat captures `meeting_id` as None when no conference is active, so an orphan-meeting silence-split rotation cannot drop the reply as stale.
- **Conference precedence & known v1 desync**: with meeting-driven autospawn enabled, a conference `meeting_started` preempts a live manual session (kill + respawn under the meeting); the bridge clears `_manual_copilot_active` and broadcasts `copilot_status {status: "manual_preempted"}` to subscriber clients. With `AUTOSPAWN=0` the supervisor ignores meeting frames, so on preemption the bridge still clears its flag and emits the notice but the `"manual"` tmux session lingers untracked — only killable via explicit Stop (documented v1 limitation).
- **Manual-session restart recovery (webapp plan D5)**: manual-session truth has one runtime owner — the supervisor (it physically holds the tmux session). `supervisor_hello` carries `current_meeting_id` — the id of a session the supervisor intends to keep alive (SPAWNING/RUNNING; GRACE and IDLE report null); a hello with `"manual"` and no active conference re-arms the bridge's `_manual_copilot_active` BEFORE `meeting_state` is computed, so reconcile replies `{active: true, meeting_id: "manual"}` and adopts instead of killing. This covers a bridge-only restart while the supervisor stays alive. A full `./recog-service.sh restart` is covered too (DW-9): the manual session carries its kind in its tmux session name — manual spawns create `copilot-manual`, every meeting-driven spawn keeps `copilot` — and the fresh supervisor probes the manual name at startup and reads the type back: `copilot-manual` alive (and `copilot` dead) → `lifecycle.adopt("manual")` + runner rebinds, so hello reports `"manual"`; both names alive → conference wins and the stale `copilot-manual` is killed. `tmux -L recog-copilot ls` therefore shows `copilot` or `copilot-manual` depending on the session kind; `./recog-service.sh stop`/`uninstall` reap both names. (The former residual — a supervisor restart during the manual-stop grace window resurrecting a stopped session — is gone: manual Stop kills immediately, so a stopped session leaves no tmux marker to re-adopt.)
- **No proactive ticks in manual sessions (webapp plan D4)**: `_copilot_tick_state` is seeded only on conference join paths (the `meeting_started` emit and the same-meeting reconnect re-arm) — never by manual frames, so a manual session gets manual chat only; transcript context still flows via `get_recent_transcript`.

See `docs/plans/completed/2026-07-02-auto-copilot-session.md` for the state machine diagram, decisions table (D1–D7), and known v1 limitations (one copilot per machine, unknown startup screen → `blocked`, positive classifiers frozen on claude 2.1.197). See `docs/plans/completed/20260724-webapp-copilot-live.md` for the webapp manual-session decisions (D1–D5) and protocol frames.

### Module Organization
- `recog/` - Core Python package
  - `ai_provider.py` - AIProvider ABC, CodexCliProvider, resolve_cli() — shared AI provider interface used by increment.py and title_generator.py
  - `asr.py` - GigaAM ASR wrapper
  - `speaker_db.py` - ECAPA-TDNN embeddings matching
  - `audio_capture.py` - `BufferedAudioCapture`: the buffer/overlap/re-anchor machinery shared by the mic (`SingleDeviceCapture`) and the system-audio subprocess captures. Owns `_append_locked`, `clear_buffer`, `get_chunk() -> (chunk, capture_start_ts)`; the contract is 1-D mono float32
  - `system_audio.py` - `SubprocessPCMCapture` (generic child-process PCM reader) + `PulseAudioCapture` (Linux `parec`, follows the default sink) + `find_or_build_sck_utility` (macOS ScreenCaptureKit helper, `sck_audio_capture` resolved from `REPO_ROOT` — the binary and its `.swift` source stay at the repo root, outside the package) + `resolve_system_audio_backend` / `create_system_capture`. Manual smoke: `python -m recog.system_audio`
  - `live.py` - Real-time transcription (LiveTranscriber with decomposed methods: _setup_meeting_storage, _setup_ktalk_bridge, _start_audio_captures, _process_chunk, _handle_mic_chunk, _handle_ktalk_chunk, _handle_embedding_chunk, _process_pending, _teardown_session; module-level helpers: normalize_word, deduplicate)
  - `increment.py` - Auto-generate versioned AI summaries on ktalk_leave; multi-prompt support via generate_all(), stores per-prompt results in meetings/<id>/increments/<prompt-id>/vN.md
  - `prompt_registry.py` - Load and filter prompt definitions from prompts/*.md files (YAML frontmatter + template body)
  - `node_env.py` - Node/nvm PATH utilities for subprocess calls under the service manager (launchd / `systemd --user`) (shared by frontend/, ktalk_bridge.py, and title_generator.py)
  - `copilot_supervisor.py` - Auto-spawn Copilot supervisor: asyncio daemon (`SessionLifecycle` state machine + `TmuxRunner` + `Watchdog`) that spawns a headless Claude session per conference meeting (`is_conference_source`: ktalk/vas3k/vkkovalev/matrix) via `tmux -L recog-copilot`, drives lifecycle from bridge `meeting_started/ended`, and reports `copilot_agent_status` to the bridge
  - `copilot_channel.py` - copilot MCP daemon (channel push); `SessionCountReporter` polls `session_manager._server_instances` and reports `claude_sessions {count}` to the bridge for the status merge
  - `ws_origin.py` - Shared WS Origin allowlist (`origin_allowed`): conference hosts mirroring the extension `manifest.json` (`*.ktalk.ru` wildcard via hostname parse, exact Jitsi/Element hosts) + webapp origin (9080); no-Origin clients (Python daemons) pass, `Origin: null` and everything else is rejected. Used by `ktalk_bridge.py` and `frontend/server.py`
- `mp4_notes.py` - Main CLI entry point (commands: live, meetings)
- `ktalk_bridge.py` - WebSocket server receiving speaker signals from Chrome extension; merges `copilot_agent_status` (supervisor) + `claude_sessions` (daemon) into the effective agent status broadcast to extension and subscriber clients; fans out `copilot_reply` and broadcast `copilot_status` frames to `_subscriber_clients` while keeping per-request rejection echoes originator-only, relays webapp `copilot_spawn_request`/`copilot_stop_request` to the supervisor, and owns `_manual_copilot_active` (conference precedence, `supervisor_hello.current_meeting_id` re-arm). WS connections are gated at accept-time by the Origin allowlist (`recog/ws_origin.py`): browser pages must come from a conference host or the webapp, no-Origin Python clients pass. A meeting ends (`_end_meeting_on_extension_gone`) when the extension WS that sent `meeting_join` drops — `_meeting_owner_ws` — not only when the last extension client is gone: the content script runs on every conference host, so an idle `example.ktalk.ru` tab holds a WS with no call and would otherwise pin the meeting LIVE forever
- `frontend/` - FastAPI server + SPA (WebSocket for live updates); `/api/live/stream` is gated by the same Origin allowlist (`recog/ws_origin.py`) — handshake denied before `accept()` for disallowed browser origins
  - `bridge_subscriber.py` - bidirectional WS client that connects to `ktalk_bridge` (default `ws://localhost:8766`, overridable via `RECOG_BRIDGE_WS_URL`), forwards `FORWARD_TYPES` frames (`transcript_refine`, `copilot_reply`, `copilot_status`) to webapp live-stream clients on port 9080, and exposes best-effort `send(raw)` for webapp→bridge copilot frames (returns False while disconnected — no queueing across reconnects). See D10 in `openspec/changes/per-speaker-stream-refine/design.md`.
  - `live_watcher.py` - JSONL tail watcher with inode + size-shrink rewrite detection and a seen-id watermark. Survives atomic `os.replace` rewrites from `MeetingStorage.update_segments` without stalling or re-broadcasting previously-seen rows.
- `kontur-diarize/ktalk-speaker-extension/` - Chrome extension for KTalk, Jitsi (vas3k/vkkovalev), and Matrix/Element Call
  - `src/` - Source modules (edit these)
  - `src/platforms/` - Per-platform adapters (`ktalk.js`, `vas3k.js`, `matrix.js`) + `index.js` (`detectPlatform`, `isJitsiPlatform` — which now includes `matrix`, gating the floating panel and the conference retry-forever loop)
  - `injected.js` - Built bundle (generated by webpack)
  - `content.js` - Injects bundle into the conference page (KTalk top frame, or the Element Call iframe scoped via `all_frames`)
  - `manifest.json` - Extension manifest (Manifest V3); one `content_scripts` entry per platform host

### Coding Style
- Python 3.12, 4-space indentation
- CLI wiring in `mp4_notes.py`, reusable logic in `recog/`
- `snake_case` for functions/vars, `PascalCase` for classes
- `*_worker.py` suffix for subprocess helpers
- Prefer `pathlib.Path` and type hints for new code

## Environment Variables

Copy `.env.example` → `.env` and `frontend/.env.example` → `frontend/.env` (`setup.sh` does both). `RECOG_TORCH_INDEX_URL`, `RECOG_GIGAAM_URL`/`RECOG_GIGAAM_REF`/`RECOG_GIGAAM_EDITABLE` are deliberately **not** in `.env`: `.env` is read by Python (`load_dotenv` in `recog/config.py`), no shell script reads it, and `setup.sh` needs those variables before it creates `.env` — they are call-shell variables (`RECOG_TORCH_INDEX_URL=… ./setup.sh`).

- `HF_TOKEN` - HuggingFace token (optional, was required for pyannote diarization which is now removed)
- `AI_PROVIDER` - `claude` | `codex` | `openrouter` (for transcript analysis)
- `CLAUDE_MODEL` - `haiku` | `sonnet` | `opus`
- `OPENROUTER_API_KEY` - Required if using OpenRouter
- `MIC_SPEAKER_NAME` - Speaker label for microphone input in live mode (default: `Me`)
- `RECOG_ASR_DEVICE` - `auto` (cuda > mps > cpu) | `cuda` | `mps` | `cpu` (default `auto`). An explicitly requested device that is unavailable raises instead of quietly landing on CPU; only an auto-resolved `cuda` is demoted to `cpu` on OOM.
- `RECOG_TORCH_THREADS` - Optional `torch.set_num_threads()` cap for CPU inference (live and refine share one `_model_lock` and all cores). Best-effort — an unparsable value is logged and ignored.
- `RECOG_PULSE_MONITOR_SOURCE` - Pins the PulseAudio/PipeWire monitor for the `pulse` backend (Linux) and disables follow-the-default-sink. Empty (default) = follow the current default sink.
- `RECOG_EXTRA_WS_ORIGINS` - Comma-separated extra exact origins allowed to open the bridge / live-stream WS. Kept as a separate set so `ALLOWED_ORIGINS` in `recog/ws_origin.py` stays a literal mirror of the extension's `manifest.json` (a test asserts that equality); a new conference host must be added here **and** to `manifest.json` (separate rebuild).
- `RECOG_BRIDGE_WS_URL` - Overrides the bridge WebSocket URL the frontend subscriber connects to (default `ws://localhost:8766`). Used by `frontend/bridge_subscriber.py` to forward `transcript_refine` events from `ktalk_bridge.py` to webapp clients (D10 — JSONL tail cannot see refine rewrites).
- `RECOG_ASR_BACKEND` - `local` (default — repo behavior unchanged) | `remote` (offload GigaAM to the Z270XX gateway via `recog/remote_asr.py`'s `build_asr()`, with lazy local fallback + circuit breaker).
- `RECOG_ASR_REMOTE_URL` - Gateway base URL. **No default**: `RECOG_ASR_BACKEND=remote` with an empty URL makes `build_asr()` raise at startup instead of silently serving from the local model (different RAM and latency). `scripts/deps.sh` checks the pair before any service is activated.
- `RECOG_ASR_REMOTE_MODEL` - Model alias sent to the gateway (default `gigaam`).
- `RECOG_ASR_REMOTE_TIMEOUT_S` - Per-request HTTP timeout (default `15`).
- `RECOG_ASR_CIRCUIT_FAILS` - Consecutive remote failures before the circuit opens (default `3`).
- `RECOG_ASR_CIRCUIT_OPEN_S` - Seconds the circuit stays open before a half-open probe (default `60`).
- `RECOG_ASR_LIVE_TIMEOUT_S` - Remote timeout for live chunk/batch requests (default `5`). Refine/offline keep `RECOG_ASR_REMOTE_TIMEOUT_S`.
- `DEDUP_MAX_GAP_S` - Live dedup window (default `2.0`): last-words dedup applies only to chunks captured within this many seconds of the previous segment.
- `ASR_PENDING_BUFFER_CAP_S` - Max audio buffered in memory while ASR is unavailable (default `600`).
- `RECOG_COPILOT_AUTOSPAWN` - Gate for meeting-driven auto-spawn only (code default `1`, but `.env.example` ships `0` so a fresh install never spawns Claude unattended | `0` keeps the daemon connected and makes meeting frames inert; manual webapp spawn/stop and crash-respawn work regardless — webapp plan D3). Read by `recog/copilot_supervisor.py`.
- `RECOG_COPILOT_GRACE_S` - Seconds the supervisor waits after `meeting_ended` before killing the copilot tmux session (default `90`); cancelled by a same-id `meeting_started` (flap). Not used for an explicit webapp Stop — that kills immediately.
- `RECOG_COPILOT_MCP_URL` - URL of the copilot MCP daemon the spawned Claude session connects to (default `http://127.0.0.1:8767/mcp`).

### Call-shell variables (read by the scripts, never from `.env`)

- `RECOG_SKIP_READINESS=1` - bypass the `scripts/deps.sh` gate that `recog-service.sh start` / `restart` / `restart-*` run before touching a service. For deliberately running a subset, and for tests driving stub service managers.
- `RECOG_SERVICE_OS` - overrides the `uname -s` backend choice (which templates and `svc_*` verbs are used), so a Mac can render Linux units.
- `RECOG_PYTHON` - the interpreter baked into the rendered units (`scripts/service/common.sh`). Deliberately **not** `DEPS_PYTHON`, which only selects the interpreter `scripts/deps.sh` uses for its library probes.
- `RECOG_HOST_OS` - overrides `uname -s` for the `scripts/deps.sh` probes (lets a Mac exercise the Linux ones).
- `RECOG_CHECK_PYTHON` - interpreter override for `scripts/check.sh`.
- `RECOG_TORCH_INDEX_URL`, `RECOG_GIGAAM_URL` / `RECOG_GIGAAM_REF` / `RECOG_GIGAAM_EDITABLE` - see Setup above.

## Data Files

- `speakers_ecapa.json` - Speaker embedding database (gitignored — real voice embeddings; `setup.sh` seeds it from `speakers_ecapa.json.example`)
- `meetings/` - Stored meeting transcripts (JSONL format, gitignored)
- `prompts/` - Prompt definitions for AI analysis (.md files with YAML frontmatter); prompt ID = filename stem
- `logs/` - Service logs (gitignored)

## Testing

```bash
./scripts/check.sh                        # everything below; exactly what CI runs (macOS + Linux)

python -m pytest tests/ -q -m "not e2e"   # unit tests (e2e needs playwright + a real conference)
python -m compileall recog frontend       # syntax check
python mp4_notes.py --help                # CLI smoke test
./recog-service.sh render --dry-run "$(mktemp -d)"   # unit templates, both backends via RECOG_SERVICE_OS
```

## ADR Documents

Architecture Decision Records in `adr/`:
- **ADR-0006**: Dual venv subprocess (obsolete — pyannote diarization removed, single venv now)
- **ADR-0010**: Live transcription mode (audio-capture part superseded by ADR-0021)
- **ADR-0011**: Meeting storage
- **ADR-0013**: AI transcript analysis
- **ADR-0018**: Per-speaker ASR pipeline (PCM transport from Chrome extension; always-on for KTalk meetings)
- **ADR-0019**: Stream-refine — tier-2 re-transcription with 20 s context (always-on)
- **ADR-0020**: Remote GigaAM offload with lazy local fallback (opt-in)
- **ADR-0021**: Cross-platform capture and services (parec system audio, `systemd --user`, rendered unit templates)
