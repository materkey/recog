# recog — review profile

Where this disagrees with general taste on what is a defect in this repository, it wins.

## What the software is

A Russian speech-recognition pipeline for live meetings, running on the author's Mac and on
colleagues' Linux boxes as user services (launchd / `systemd --user`). GigaAM (`v3_e2e_rnnt`) for ASR,
a Chrome extension that supplies per-participant PCM (KTalk) and speaker names from KTalk / Jitsi /
Element Call / Google Meet / the Zoom web client, an Accessibility-tree helper for the native Zoom
application on macOS, a FastAPI + vanilla-JS web app on `127.0.0.1:9080`, and a headless Claude
"copilot" session spawned per meeting. Speakers are identified from conference signals only; there
is no voice-embedding model in the pipeline.

Changes are authored by the maintainer, mostly through coding agents. `CLAUDE.md` (`AGENTS.md` is a
symlink to it), `docs/plans/` and `.revmux/` are part of a change and in scope for the docs lens.

Languages: Python 3.12 on threads and queues (read questions about goroutines and channels as threads
and queues; Python has no race detector, so concurrency is shown by a deterministic test), JavaScript
(the extension: webpack + jest; the web SPA: no build), Swift (macOS helpers), bash and unit templates
(setup and service scripts).

## Blast radius and what is noise

One person per machine, everything on localhost or a tailnet. Scale, multi-tenancy, public API
stability and auth beyond the Origin allowlist are not concerns, and findings framed as if they were
are noise.

What does matter at the edges:
- **The Origin allowlist** (`recog/ws_origin.py`) is the one gate for browser WebSocket clients; a
  change that widens it or bypasses it is a finding. No-Origin clients are local Python daemons.
- **Untrusted input is data, not instructions.** Conference pages are third-party DOM, and meeting
  speech, titles and chat reach the copilot session, which has tools. Treating any of it as trusted is
  a finding.
- **Existing installations upgrade in place** via `./setup.sh` and `./recog-service.sh install` /
  `restart`. Paths kept for installed-machine compatibility (`venv-gigaam`, the `sck_audio_capture`
  binary at the repo root, the data root from `get_data_dir()`) are contracts.

## What a real failure looks like

Ranked by how irreversible and how invisible the damage is. The level after each class is the default;
the concrete consequence decides.

1. **Speech is lost silently — critical.** Audio is captured but never transcribed, or written into
   the wrong meeting, and nothing in the transcript says so. A meeting happens once; there is no
   re-run. This is the defect class the architecture is built around: capture buffers hold audio while
   ASR is unavailable, a chunk caught mid-failure is retried rather than dropped, a capture that gives
   up writes a marker into the transcript. Anything that reintroduces silent loss is critical even when
   the code looks tidy. **Attribution — major:** a change that credits speech to the wrong participant
   where it was right before, or presents uncertain attribution as certain. Attribution that is
   inherently approximate (Google Meet and Zoom are Tier 1, system audio without conference identity
   is uncertain) is not a defect in itself.
2. **Meeting boundaries break — critical.** Segments, WAV offsets or refine results land in the
   previous meeting, or a meeting switch skips writer rotation ("No audio file available for this
   meeting"). The `_meeting_generation` counter and `current_meeting_path` are the identity mechanism;
   a per-meeting state reset that keys on an event rather than on meeting identity is a real defect.
3. **Personal or employer data escapes — critical.** Meeting content, the legacy `speakers*.json`
   (embeddings left from the removed diarization; still backed up and migrated, never edited by
   tooling), personal hostnames or the author's `.env` reaching git, a colleague's clone, or an
   external service. `meetings/`, `speakers*.json`, generated binaries and rendered service units are
   untracked on purpose; tooling never edits an existing `.env`. In code, docs, tests and commit
   messages, any real hostname, meeting-room URL or login-system vendor name outside `example.*` hosts
   and placeholder ids is a finding: examples use `example.ktalk.ru`, `sso.example.org` and ids like
   `abcd1234wxyz`.
4. **macOS RAM or latency regressions — major.** MPS with the FP16 encoder, the shared
   `ASRModule._model_lock` (an RLock — live and refine serialise on it), `_mps_autoreleasepool`, the
   MPS cache-release cadence (`_MPS_EMPTY_CACHE_EVERY`) and the capture buffer sizes are measured
   contracts. Changing one of them without a RAM/latency measurement, adding a second model or a second
   set of audio buffers, or moving the ML stack outside `deploy/python/constraints/macos.txt` is a
   finding regardless of how clean the code is.
5. **A service that limps instead of dying — major.** Restart by the service manager + fail-fast init
   is the design (`Restart=always` / `KeepAlive`; the frontend uses `Restart=on-failure` on purpose, a
   clean exit there is intentional). The boundary: a component that cannot initialise must exit so the
   service manager restarts it; a running capture or worker thread catches, logs and marks instead
   (see broad `except` below). Code that swallows a fatal init error and keeps running degraded is
   worse than a crash.
6. **One OS breaks while the other works — major.** Both macOS and Linux are shipped: system audio
   (ScreenCaptureKit helper / `parec`), service units (launchd / `systemd --user`, rendered from
   `deploy/{macos,linux}/*.in`), ASR device (MPS / CUDA / CPU). A change verified on one OS that breaks
   the other is a real finding; macOS-only features (the Zoom application) must stay inert on Linux.

## Invariants — flag a violation

- **`print()` to stdout in the live path is the product**, not debug output — the terminal transcript is
  a user-facing surface, and its markers are part of the contract. Removing or rewording a marker is a
  finding.
- **`recog/config.py` stays free of third-party imports.** `scripts/deps.sh` imports it with the host
  interpreter before the venv exists; `tests/test_repo_hygiene.py` pins this. It hand-rolls dotenv
  parsing for that reason.
- **Conference adapters read third-party DOM defensively** (`browser-extension/src/platforms/`).
  Every selector sits in one map per platform (`GMEET_SELECTORS`, `ZOOM_SELECTORS`); "speaking" is
  positive evidence only, so an unknown indicator state means silent rather than a guess; leaving a
  meeting is debounced (~6 s) so a transient DOM gap (Picture-in-Picture handover, a re-render) does not
  send `meeting_leave`. A change that guesses on unknown UI state, or ends a meeting on a single
  missing element, is a finding.
- **The Zoom application helper only reports the Accessibility tree** (`recog/native/macos/zoom_ax_probe.swift`);
  what a tab group means is decided in Python (`recog/zoom_app.py`). Any change to the helper's code
  changes its signature and costs the user the Accessibility grant, so moving logic into the helper is
  a regression, not a cleanup. Rebuild freshness is the source's sha256 kept beside the bundle, not
  mtime and not a stamp inside it (the signature would seal it).

## Deliberate choices — do not flag

- **Comments and docstrings in Russian or English**, mixed even within one change, beside English
  identifiers. Test names follow given–when–then; a test docstring explaining what breaks without the
  fix is the house style.
- **Threads and locks, not asyncio**, in the live pipeline. `_state_lock` guards meeting state,
  `_model_lock` serialises inference. Compare-and-set under one lock is intentional where it appears.
- **Broad `except Exception` with rate-limited logging** (`recog/log_utils.py`) around capture and ASR
  paths: an exception must never kill a capture or worker thread. Report what the handler *hides*
  (lost audio, no marker), not the breadth of the catch.
- **`os._exit(0)` in teardown** after state is flushed: daemon threads and MPS make a clean interpreter
  shutdown unreliable.
- **Vanilla JS + a hand-written SPA** in `recog/web/static/`, no build step, no framework. The Chrome
  extension is the only thing with a webpack build.

## Where the rules live

[`docs/README.md`](../docs/README.md) is the documentation index;
[`docs/repository-layout.md`](../docs/repository-layout.md) maps current paths and upgrade steps.
`docs/adr/` holds the decisions, `CLAUDE.md` the working contract, `docs/plans/` the plans behind
changes. A code change that contradicts a current ADR or `CLAUDE.md` is a real finding.

Prose is minor by default. These documents are executed by an agent or a script and are rated by what
their consumer does wrong:
- `recog/prompts/*.md` — LLM prompts for meeting analysis;
- `.claude/skills/copilot-mode/SKILL.md` and `.claude/agents/copilot-session.md` — the copilot
  session's playbook;
- `.claude/skills/recog-meetings/SKILL.md`;
- `deploy/{macos,linux}/*.in` — rendered into service units;
- `CLAUDE.md`, where an agent following the wrong statement would break a service, lose data or leak
  it; elsewhere a stale `CLAUDE.md` statement is minor.

A plan under `docs/plans/completed/` is a historical record of one change, not a live specification:
do not report a shipped implementation for diverging from a completed plan's wording. Its explicitly
stated constraints (the RAM and macOS contracts above) are the exception — those were carried into
`CLAUDE.md` deliberately.

## Tests

`pytest`, and `-m "not e2e"` is the set that runs anywhere. The `e2e` marker means real KTalk, a
browser and audio hardware; `needs_audio_loopback` additionally reroutes the machine's audio output.
Hardware and subprocesses are mocked (`Mock(spec=LiveTranscriber)`, injected `run`, stubbed PATH), and a
test that would need a microphone, a GPU or a network call is written against a double instead.
The extension has its own jest suite (`browser-extension/src/**/*.test.js`, `npm test`, the
`extension` CI job); a test for an adapter or other extension code belongs there, not in pytest.

A missing test is worth reporting only when the change it covers falls in failure classes 1–6 above
or is a shipped script's arithmetic. Coverage as an end in itself is not a finding here.

## Quality measurements

Reviewers find the round's `report.md` and `report.json` in the context directory (thresholds and
policy in `docs/quality.md`). If they are absent, show a measurement error, or the context does not
say they were generated for the head under review, metrics are unverified: never infer coverage or
scores. The reports cover production Rust (`crates/<crate>/src/`, test code left out) and production
JavaScript; a Rust function not compiled on the measuring host is listed as not built and has no
score there. Verifiers do not receive the context and judge a
finding by the code alone.

Use increases in CRAP, SCB and cognitive erosion on changed functions only to decide where to look:
uncovered branches, new state transitions, added synchronization. A numeric increase alone is not a
defect, and a gate failure is not repeated as a finding.
