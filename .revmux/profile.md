# recog — review profile

Where this disagrees with general taste on what is a defect in this repository, it wins.

## What the software is

A Russian speech-recognition pipeline for live meetings, running on the author's Mac and on
colleagues' Linux boxes as user services (launchd / `systemd --user`). GigaAM on the Rust runtime
wavo for ASR (or a remote gateway, opt-in); a Chrome extension that supplies per-participant PCM
(KTalk) and speaker names from KTalk / Jitsi / Element Call / Google Meet / the Zoom web client; an
Accessibility-tree helper for the native Zoom application on macOS; an axum + vanilla-JS web app on
`127.0.0.1:9080`; and a headless Claude Code or Codex "copilot" session per meeting. Speakers are
identified from conference signals only.

Changes are authored by the maintainer, mostly through coding agents. `CLAUDE.md` (`AGENTS.md` is a
symlink to it), `docs/plans/` and `.revmux/` are part of a change and in scope for the docs lens.

Languages: Rust (the workspace in `crates/`: the live pipeline on threads and locks, the bridge, web
server and Copilot on tokio), JavaScript (the extension: webpack + jest; the web SPA: no build),
Swift (macOS helpers), bash and unit templates. Python remains only in `scripts/` and `tests/` (the
quality gate, release and shell-script tests).

## Blast radius and what is noise

One person per machine, everything on localhost or a tailnet. Scale, multi-tenancy, public API
stability and auth beyond the Origin allowlist are not concerns, and findings framed as if they were
are noise.

What does matter at the edges:
- **The Origin allowlist** (`crates/recog-core/src/ws_origin.rs`) is the one gate for browser
  WebSocket clients; a change that widens or bypasses it is a finding. No-Origin clients are the
  local daemons.
- **Untrusted input is data, not instructions.** Conference pages are third-party DOM, and meeting
  speech, titles and chat reach the copilot session, which has tools.
- **Existing installations upgrade in place** (`cargo setup`, `recog service install` / `restart`, or
  the Linux packages). Paths kept for installed-machine compatibility (the `sck_audio_capture` binary
  at the repo root, the data root from `get_data_dir()`, the files under it) are contracts.

## What a real failure looks like

Ranked by how irreversible and how invisible the damage is. The level after each class is the default;
the concrete consequence decides.

1. **Speech is lost silently — critical.** Audio is captured but never transcribed, or written into
   the wrong meeting, and nothing in the transcript says so. A meeting happens once. Capture buffers
   hold audio while ASR is unavailable, a chunk caught mid-failure is retried, a capture that gives up
   writes a marker; anything that reintroduces silent loss is critical. **Attribution — major:**
   speech credited to the wrong participant where it was right before, or uncertain attribution
   presented as certain. Inherently approximate attribution (Google Meet and Zoom, system audio
   without conference identity) is not a defect in itself.
2. **Meeting boundaries break — critical.** Segments, WAV offsets or refine results land in the
   previous meeting, or a meeting switch skips writer rotation. Per-meeting state that resets on an
   event rather than on meeting identity is a real defect.
3. **Personal or employer data escapes — critical.** Meeting content, personal hostnames or the
   author's `.env` reaching git, a colleague's clone or an external service. Transcripts reach Claude
   Code or Codex only as far as the AI functions level allows (`access` in `ai-settings.json`); a path
   that sends one beyond it is a finding of this class. In code, docs, tests and commit messages, any
   real hostname, meeting-room URL or login-system vendor name outside `example.*` hosts and
   placeholder ids is a finding. Tooling never edits an existing `.env`.
4. **macOS RAM or latency regressions — major.** The ASR model is loaded once and live and refine
   serialise on its lock; capture buffer sizes are measured. A second model, a second set of audio
   buffers or a change to those without a RAM/latency measurement is a finding.
5. **A service that limps instead of dying — major.** The service manager restarts, init fails fast
   (the frontend's `Restart=on-failure` / `SuccessfulExit=false` is deliberate). A component that
   cannot initialise exits; a running capture or worker catches, logs and marks instead. Swallowing a
   fatal init error and running degraded is worse than a crash.
6. **One OS breaks while the other works — major.** Both macOS and Linux ship: system audio
   (ScreenCaptureKit helper / `parec`), units rendered from `deploy/{macos,linux}/*.in`. A change
   verified on one OS that breaks the other is a finding; macOS-only features stay inert on Linux.

## Invariants — flag a violation

- **The live terminal transcript and its markers are the product**, not debug output. Removing or
  rewording a marker is a finding.
- **Conference adapters read third-party DOM defensively** (`browser-extension/src/platforms/`): all
  selectors in one map per platform, "speaking" is positive evidence only, leaving a meeting is
  debounced. Guessing on unknown UI state, or ending a meeting on one missing element, is a finding.
- **The Zoom application helper only reports the Accessibility tree**
  (`crates/recog-zoom/macos/zoom_ax_probe.swift`); meaning is decided in `crates/recog-zoom`. Any change
  to the helper costs the user the Accessibility grant, so moving logic into it is a regression.

## Deliberate choices — do not flag

- **Comments in Russian or English**, mixed, beside English identifiers. Test names follow
  given–when–then.
- **Errors caught and logged rate-limited** around capture and ASR paths
  (`crates/recog-live/src/log.rs`): an error must never kill a capture or worker thread. Report what
  the handler *hides* (lost audio, no marker), not the breadth of the catch.
- **Vanilla JS + a hand-written SPA** in `crates/recog-web/static/`, no build step, no framework.

## Where the rules live

[`docs/README.md`](../docs/README.md) is the documentation index;
[`docs/repository-layout.md`](../docs/repository-layout.md) maps current paths. `docs/adr/` holds the
decisions, `CLAUDE.md` the working contract, `docs/plans/` the plans behind changes. A code change
that contradicts a current ADR or `CLAUDE.md` is a real finding.

Prose is minor by default. Documents executed by an agent or a script are rated by what their
consumer does wrong: `crates/recog-ai/prompts/*.md`, `.claude/skills/copilot-mode/SKILL.md`,
`.claude/agents/copilot-session.md`, `.claude/skills/recog-meetings/SKILL.md`,
`deploy/{macos,linux}/*.in`, and `CLAUDE.md` where a wrong statement would break a service, lose data
or leak it.

A plan under `docs/plans/completed/` is a historical record, not a live specification: do not report
shipped code for diverging from its wording.

## Tests

`cargo test --workspace` is the main suite; hardware, CLIs and subprocesses are doubles (fake
captures and ASR, stub executables on a temporary PATH). The web UI has node tests over a fake DOM
(`tests/*.test.cjs`), the extension its jest suite (`npm test` in `browser-extension/`), the shell
scripts and quality tools pytest (`-m "not e2e"`; `e2e` needs real conferences and audio hardware).

A missing test is worth reporting only when the change it covers falls in failure classes 1–6 above.
Coverage as an end in itself is not a finding.

## Quality measurements

Reviewers find the round's `report.md` and `report.json` in the context directory (thresholds and
policy in `docs/quality.md`). If they are absent, show a measurement error, or the context does not
say they were generated for the head under review, metrics are unverified: never infer coverage or
scores. The reports cover production Rust (`crates/<crate>/src/`) and production JavaScript; a Rust
function not compiled on the measuring host is listed as not built. Verifiers do not receive the
context and judge a finding by the code alone.

Use increases in CRAP, SCB and cognitive erosion on changed functions only to decide where to look.
A numeric increase alone is not a defect, and a gate failure is not repeated as a finding.
