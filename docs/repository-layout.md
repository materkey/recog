# Repository layout

Application code lives under one Python package, `recog/`, so its CLI, web UI,
bridge and storage import each other through package paths. Browser code has
its own top-level directory. Installation inputs, development tools and design
records are grouped separately, making it clear where a new file belongs.

| Path | Responsibility |
| --- | --- |
| `recog/__main__.py`, `recog/cli.py` | `python -m recog` command entry point and argument handling |
| `recog/` | Live capture, ASR, speakers, meeting storage, conference bridge and Copilot |
| `recog/web/` | FastAPI server, live watchers and static SPA; `python -m recog.web.server` |
| `recog/asr_server/` | Optional gateway and GigaAM server, launched independently of the client services |
| `recog/prompts/` | Shared AI summary templates |
| `recog/native/macos/` | ScreenCaptureKit Swift helper source |
| `browser-extension/` | Chrome extension source, platform adapters, build and tests |
| `deploy/python/` | Client/server requirements and verified macOS dependency constraints |
| `deploy/macos/`, `deploy/linux/` | launchd and systemd service templates |
| `deploy/examples/` | Non-personal seed data for setup |
| `scripts/` | Setup/service helpers, data migration, enrollment and checks |
| `scripts/benchmarks/` | Manually run benchmark and experiment programs |
| `tests/` | Unit tests and E2E tools/fixtures |
| `docs/` | [Guides, decisions, plans, research and experiment reports](README.md) |
| `docs/bmad/`, `openspec/` | BMAD artifacts and OpenSpec specifications/change records |
| `.local/examples/` | Gitignored sample recordings and transcripts |

The root keeps the entry scripts `setup.sh` and `recog-service.sh`, project
configuration, environment examples and the main README/agent instructions.
`AGENTS.md` points to `CLAUDE.md`. BMAD still expects `_bmad/` and
`_bmad-output`; the latter is a relative symlink to `docs/bmad/`. Other agent
configuration directories retain their tool-required locations.

The generated `sck_audio_capture` binary and local `venv-gigaam` remain at the
root to preserve installed paths. Existing `.env` and `recog/web/.env` files
remain local configuration. Meetings, logs and speaker databases belong in the
data root resolved by `recog.config.get_data_dir()` (normally `~/.recog`, or
`RECOG_DATA_DIR`). Moving code does not migrate that data; the separate
[data migration procedure](../README.md#personal-data-outside-the-checkout)
handles legacy installations.

## Updating an existing checkout

After updating the checkout, re-render all four service units so they use the
new module paths, then restart them:

```bash
./recog-service.sh install
./recog-service.sh restart
./recog-service.sh status
```

`install` registers units without starting them; `restart` runs the existing
dependency readiness checks before activation. These steps reuse the current
venv and model cache. The directory reorganization itself does not install or
upgrade dependencies, change the ML stack, or enable the optional ASR server.
For a full dependency/setup update, use `./setup.sh` as described in the main
README; macOS pip operations retain the committed dependency constraints.

Run manual commands from the repository root with `venv-gigaam` activated:

```bash
source venv-gigaam/bin/activate
python -m recog live --ktalk --system-audio auto
python -m recog.web.server
```

Update shortcuts or custom service definitions that used `mp4_notes.py` or
`frontend/server.py` to those module commands. The bridge and storage modules
are now imported as `recog.ktalk_bridge` and `recog.meeting_storage`.

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
updated to `python -m recog.asr_server.gateway` and
`python -m recog.asr_server.gigaam_backend`, with the repository root as their
working directory. See the [ASR server guide](asr-server.md) for their separate
environment and dependencies.

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
