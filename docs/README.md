# Documentation

Start with the [project README](../README.md) for installation and operation,
or the [repository layout](repository-layout.md) to find code and update an
existing checkout after the directory moves. [CLAUDE.md](../CLAUDE.md) records
the current implementation contracts and commands used by coding agents.

| Location | What belongs here |
| --- | --- |
| [Architecture decisions](adr/) | Why a design was chosen, its constraints and superseding decisions; check each ADR's status |
| [Implementation plans](plans/) | Proposed work and its execution steps; [completed plans](plans/completed/) are historical records |
| [Experiments](experiments/) | Measurements, benchmark reports and supporting reference data; runnable benchmark scripts live in [`scripts/benchmarks/`](../scripts/benchmarks/) |
| [Research](research/) | Investigations and exploratory notes that have not become architecture decisions |
| [BMAD artifacts](bmad/) | BMAD brainstorming, specs, stories, reviews and deferred-work records; `_bmad-output` points here for tool compatibility |
| [OpenSpec](../openspec/) | Tool-managed [specifications](../openspec/specs/) and [change proposals](../openspec/changes/), including [archived changes](../openspec/changes/archive/) |
| [ASR server guide](asr-server.md) | Optional gateway/backend installation, configuration and transcription checks |

Completed plans and experiment reports describe the implementation and paths
at the time of the work. Use the repository layout and current source for
today's commands; preserve historical evidence when moving files.
