# recog — review profile

Read this before the diff. Where it disagrees with general taste, it wins.

## What the software is

A Russian speech-recognition pipeline for live meetings, running on the author's Mac and on
colleagues' Linux boxes as user services (launchd / `systemd --user`). GigaAM (`v3_e2e_rnnt`) for ASR,
SpeechBrain ECAPA-TDNN for speaker embeddings, a Chrome extension that supplies per-participant PCM and
speaker names from KTalk / Jitsi / Element Call, a FastAPI + vanilla-JS web app on `127.0.0.1:9080`, and
a headless Claude "copilot" session spawned per meeting.

It is not a service with users. One person per machine, everything on localhost or a tailnet, no
untrusted callers: browser WebSocket clients are gated by an Origin allowlist, everything else is a
local Python daemon. Scale, multi-tenancy, auth hardening and API compatibility are not concerns here,
and findings framed as if they were are noise.

## What a real failure looks like

In descending order of what actually hurts:

1. **Speech is lost silently.** Audio is captured but never transcribed, never attributed, or written
   into the wrong meeting — and nothing in the transcript says so. A meeting happens once; there is no
   re-run. This is the defect class the architecture is built around: capture buffers hold audio while
   ASR is unavailable, a chunk caught mid-failure is retried rather than dropped, a capture that gives
   up writes a marker into the transcript. Anything that reintroduces silent loss is critical or major
   even when the code looks tidy.
2. **Meeting boundaries break.** Segments, WAV offsets or refine results land in the previous meeting,
   or a meeting switch skips writer rotation ("No audio file available for this meeting"). The
   `_meeting_generation` counter and `current_meeting_path` are the identity mechanism; a per-meeting
   state reset that keys on an event rather than on meeting identity is a real defect.
3. **Personal data escapes.** Voice embeddings, meeting content, personal hostnames or the author's
   `.env` reaching git, a colleague's clone, or an external service. `speakers*.json`, `meetings/`,
   generated binaries and rendered service units are untracked on purpose. Tooling never edits an
   existing `.env` or speaker database.
4. **macOS RAM or latency regressions.** MPS with the FP16 encoder, the shared `ASRModule._model_lock`
   (an RLock — live and refine serialise on it), `_mps_autoreleasepool`, `_MPS_EMPTY_CACHE_EVERY=100`
   and the existing capture buffer sizes are contractual. A second model, a second set of audio
   buffers, or a bumped ML-stack version is a finding regardless of how clean the code is.
5. **A service that limps instead of dying.** `Restart=always` + fail-fast init is the design: a
   component that cannot initialise must exit so the supervisor restarts it. Code that swallows a fatal
   init error and keeps running degraded is worse than a crash.

## Deliberate conventions — not defects

- **Russian comments, docstrings and test docstrings** beside English identifiers. Test names follow
  given–when–then; a Russian docstring explaining what breaks without the fix is the house style.
- **Threads and locks, not asyncio**, in the live pipeline. `_state_lock` guards meeting state,
  `_model_lock` serialises inference. Compare-and-set under one lock is intentional where it appears.
- **Broad `except Exception` with rate-limited logging** (`recog/log_utils.py`) around capture and ASR
  paths: an exception must never kill a capture or worker thread. Report what the handler *hides*
  (lost audio, no marker), not the breadth of the catch.
- **`print()` to stdout in the live path is the product**, not debug output — the terminal transcript is
  a user-facing surface, and its markers are part of the contract.
- **`recog/config.py` must stay free of third-party imports.** `scripts/deps.sh` imports it with the
  host interpreter before the venv exists; `tests/test_repo_hygiene.py` pins this. It hand-rolls
  dotenv parsing for that reason.
- **`os._exit(0)` in teardown** after state is flushed: daemon threads and MPS make a clean interpreter
  shutdown unreliable.
- **Vanilla JS + a hand-written SPA** in `recog/web/static/`, no build step, no framework. The Chrome
  extension is the only thing with a webpack build.
- **`python -m recog` is the CLI entry point** (`recog/__main__.py` delegates to
  `recog/cli.py`); reusable logic lives in `recog/`. The web UI runs as
  `python -m recog.web.server`, from the repository root.

## The design record

[`docs/README.md`](../docs/README.md) is the documentation index;
[`docs/repository-layout.md`](../docs/repository-layout.md) maps current paths and upgrade steps.
`docs/adr/` holds the decisions (ADR-0010 live mode, 0011 meeting storage, 0018 per-speaker ASR, 0019
stream-refine, 0020 remote ASR, 0021 cross-platform capture and services), `CLAUDE.md` the working
contract, `docs/plans/` the plans behind changes. BMAD artifacts live in `docs/bmad/` (the tool-facing
`_bmad-output` symlink points there), while `openspec/` retains its tool-required layout.
A code change that contradicts a current ADR or
`CLAUDE.md` is a real finding — usually minor, since prose executes nothing, except where a document is
the contract an agent or script executes against.

A plan under `docs/plans/completed/` is a historical record of one change, not a live specification:
do not report a shipped implementation for diverging from a completed plan's wording. Its explicitly
stated constraints (the RAM and macOS contracts above) are the exception — those were carried into
`CLAUDE.md` deliberately.

## Tests

`pytest`, and `-m "not e2e"` is the set that runs anywhere. The `e2e` marker means real KTalk, a
browser and audio hardware; `needs_audio_loopback` additionally reroutes the machine's audio output.
Hardware and subprocesses are mocked (`Mock(spec=LiveTranscriber)`, injected `run`, stubbed PATH), and a
test that would need a microphone, a GPU or a network call is written against a double instead.

A missing test is worth reporting only when the change it covers is the kind above — silent loss,
meeting identity, personal data, or a shipped script's arithmetic. Coverage as an end in itself is not
a finding here.
