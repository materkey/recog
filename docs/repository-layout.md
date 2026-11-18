# Repository layout

The programs are a Rust workspace: its manifest `Cargo.toml` is at the root and
`crates/` holds one crate per responsibility
([the move to Rust](plans/2026-11-07-rust-migration.md)). A crate keeps what
belongs to it beside its code, whatever the language: the static web UI, the
prompts, the Swift sources and the desktop bootstrap script. Browser code has its own
top-level directory. Installation inputs, development tools and design records
are grouped separately, making it clear where a new file belongs.

| Path | Responsibility |
| --- | --- |
| `crates/` | `recog-live` (the `recog` binary: live process with the bridge, `meetings`, `replay`, the installation helpers), `recog-web`, `recog-copilot` (supervisor and MCP channel), `recog-desktop`, `recog-mic`, `recog-asr-server` (the optional ASR server's gateway and GigaAM backend, launched independently of the client services), and the libraries `recog-install` (service definitions), `recog-core`, `recog-ai`, `recog-bridge`, `recog-zoom`, `speexdsp-aec` |
| `bin/` | Gitignored: the programs `cargo setup` installs from `target/release` and the units run |
| `crates/recog-web/static/` | The web UI the `recog-web` binary serves |
| `crates/recog-desktop/bootstrap.js` | The script the KTalk desktop integration evaluates in the application |
| `crates/recog-ai/prompts/` | Shared AI summary templates |
| `crates/recog-live/macos/`, `crates/recog-zoom/macos/` | Swift sources: ScreenCaptureKit helper and Recog.app launcher; Zoom helper |
| `browser-extension/` | Chrome extension source, platform adapters, build and tests |
| `deploy/python/` | Requirements of the quality tools |
| `deploy/node/` | Pinned JavaScript quality tools (ESLint) for `scripts/quality.py` |
| `deploy/macos/`, `deploy/linux/` | launchd and systemd service templates |
| `deploy/ci/runner/` | Image of the self-hosted Linux CI runner (`docs/ci-runners.md`) |
| `scripts/` | Release packaging, development checks and benchmarks |
| `scripts/benchmarks/` | Manually run benchmark and experiment programs |
| `tests/` | Unit tests and E2E tools/fixtures |
| `assets/readme/` | Images embedded in the main README |
| `docs/` | [Guides, decisions, plans, research and experiment reports](README.md) |
| `docs/bmad/`, `openspec/` | BMAD artifacts and OpenSpec specifications/change records |
| `.local/examples/` | Gitignored sample recordings and transcripts |

The root keeps the Cargo workspace and alias for `cargo setup`, project
configuration, environment examples and the main README/agent instructions.
`AGENTS.md` points to `CLAUDE.md`. BMAD still expects `_bmad/` and
`_bmad-output`; the latter is a relative symlink to `docs/bmad/`. Other agent
configuration directories retain their tool-required locations.

The generated `sck_audio_capture` binary remains at the root to preserve
installed paths. An existing `.env` remains local
configuration. Meetings, logs and speaker databases belong in the data root
`bin/recog config data-dir` prints (normally `~/.recog`, or `RECOG_DATA_DIR`). Moving code does not migrate that data; the separate
[data migration procedure](usage.md#personal-data-outside-the-checkout)
handles legacy installations.

## Updating an existing checkout

After updating the checkout, re-render all four service units so they use the
new module paths, then restart them:

```bash
recog service install
recog service restart
recog service status
```

`install` registers units without starting them; `restart` runs the existing
dependency readiness checks before activation. These steps reuse the current
venv and model cache. The directory reorganization itself does not install or
upgrade dependencies, change the ML stack, or enable the optional ASR server.
For a full dependency/setup update, use `cargo setup` as described in the main
README.

Run manual commands from the repository root:

```bash
bin/recog live --ktalk --system-audio auto
bin/recog-web --port 9080
```

Update shortcuts or custom service definitions that used `mp4_notes.py` or
`frontend/server.py` to those commands.

In Chrome, reload the unpacked extension from `browser-extension/dist/`. If the
registration still points to the old `kontur-diarize/ktalk-speaker-extension/`
path and cannot reload, remove that registration and use **Load unpacked** with
`browser-extension/dist/`. A symlink does not preserve the registration's ID:
Chrome resolves its target and can leave the old card stuck at `Reloading…`
while registering the new path separately. Keep only the registration loaded
directly from `browser-extension/dist/`.
After source changes, rebuild with `npm run build` in `browser-extension/`
before reloading.

Existing optional ASR server deployments also need their launch commands
replaced by the `recog-asr-gateway` and `recog-asr-gigaam` binaries; see the
[ASR server guide](asr-server.md).

## Checking the layout

`./scripts/check.sh` runs the repository's automated checks without starting
services, recording audio or loading model weights. Validate service startup,
extension connection and live transcription on the target machine after
applying the upgrade; automated path/import checks alone do not establish
audio or inference performance.

Local retired environments and old root bytecode are preserved under `.local/retired/`.
Archived virtualenvs retain their original absolute paths; restore their original
location or recreate them before use. The active `venv-gigaam` is unchanged.

Conference addresses belong in [local host configuration](conference-hosts.md),
which is shared by the backend and the generated extension build.
