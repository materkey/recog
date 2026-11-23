# Documentation

Start with the [project README](../README.md) for installation and operation,
or the [repository layout](repository-layout.md) to find code and update an
existing checkout after the directory moves. [CLAUDE.md](../CLAUDE.md) records
the current implementation contracts and commands used by coding agents.

| Location | What belongs here |
| --- | --- |
| [Changelog](../CHANGELOG.md) | What an update of `main` asks of an existing installation, newest first |
| [Usage reference](usage.md) | Detailed installation, CLI, services, configuration and troubleshooting |
| [Architecture decisions](adr/) | Why a design was chosen, its constraints and superseding decisions; check each ADR's status |
| [Implementation plans](plans/) | Proposed work and its execution steps; [completed plans](plans/completed/) are historical records |
| [Experiments](experiments/) | Measurements, benchmark reports and supporting reference data; runnable benchmark scripts live in [`scripts/benchmarks/`](../scripts/benchmarks/) |
| [Research](research/) | Investigations and exploratory notes that have not become architecture decisions |
| [BMAD artifacts](bmad/) | BMAD brainstorming, specs, stories, reviews and deferred-work records; `_bmad-output` points here for tool compatibility |
| [OpenSpec](../openspec/) | Tool-managed [specifications](../openspec/specs/) and [change proposals](../openspec/changes/), including [archived changes](../openspec/changes/archive/) |
| [ASR server guide](asr-server.md) | Optional gateway/backend installation, configuration and transcription checks |
| [Installation configuration](configuration.md) | The per-user `config.toml`: location, sections, moving from the old hosts file |
| [Conference hosts](conference-hosts.md) | Local Jitsi/Element addresses, extension build and existing-recording aliases |
| [Desktop KTalk](desktop-ktalk.md) | Enable, inspect and disable integration with the official desktop app |
| [Zoom application](zoom-app.md) | Enable speaker detection for the native Zoom application on macOS; permission and limits |
| [Quality gates](quality.md) | Run merge-base regression checks and interpret CRAP, duplication and structural reports |
| [Linux package repositories](linux-packages.md) | How releases serve apt and pacman, sign their indexes on the Mac, and move each user's services to an upgraded package |
| [CI runners](ci-runners.md) | Set up the self-hosted Linux container and macOS runner the workflow selects by label |

Completed plans and experiment reports describe the implementation and paths
at the time of the work. Use the repository layout and current source for
today's commands; preserve historical evidence when moving files.
