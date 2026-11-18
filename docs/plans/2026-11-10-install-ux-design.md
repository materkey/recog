# Installing and updating recog: one program, no shell scripts

Status: proposed on 2026-11-10, nothing built. This document sets the route and designs
its first step; steps 2 and 3 get their own designs when their turn comes.

## Purpose

The owner shows recog to more and more people. Someone who saw it should get from "I
want to try" to the first transcript on screen without awkward places, and afterwards
update with one command. The shell scripts go away as a consequence: what they do moves
into the `recog` program.

## What a newcomer meets today

Checked on 2026-11-10:

- Six things to install by hand before `./setup.sh`: Xcode Command Line Tools, rustup,
  ffmpeg, Node.js, tmux, and Claude Code signed in.
- tmux and Claude Code stop the installation, though the README calls Copilot optional;
  the way around is `RECOG_SKIP_COPILOT_CHECK=1`.
- The first build takes minutes (fat LTO, six programs) and about 3 GB.
- The repository is private, and the README shows
  `git clone https://github.com/materkey/recog.git`.
- The macOS permissions get one line at the end of the installation.
- The commands work only from the checkout (`./recog-service.sh …`).
- An update is four actions: read `CHANGELOG.md`, `git pull && ./setup.sh`, a restart,
  Reload of the extension.

## Target

```
brew install materkey/apps/recog
recog setup
```

Afterwards, from any directory:

```
recog update                                          # updates files and reports any Chrome reload needed
recog doctor                                          # what is missing, on one screen
recog service start|stop|restart|status|logs [name]
```

From a checkout (developers; Linux until archives exist) the first command is
`cargo setup`, and the rest is the same.

## Goals, non-goals, anti-goals

Goals:

- On macOS two commands guide installation, including the manual Chrome steps.
  One command updates the installation and prompts for Reload if the extension changed.
- No shell script takes part in installing, updating or running recog.
- One list of checks says what is missing: in the terminal (`recog doctor`), later in
  the web UI.
- Copilot is optional in fact: without tmux or Claude Code the rest installs and runs.
- The commands work from any directory.

Non-goals:

- A graphical installer or a menu-bar application.
- Windows.
- Another service model: the four services under launchd / `systemd --user` stay.
- Merging the six programs into one file.
- Installing the optional ASR server.
- Publishing the extension in the Chrome Web Store.

Anti-goals:

- An update that loses the macOS permissions.
- An update that changes anything under a running conference.
- A setup that takes a working installation down on its way to failing (today's
  guarantee, kept).
- A second reader of the configuration.
- Anything that writes to an existing `.env`.

## Approaches

1. **Polish the installation from sources.** Cheap, but the six dependencies and the
   long build stay in view.
2. **One program that Homebrew installs.** Homebrew brings the dependencies, `recog
   setup` and `recog update` do the rest. Linux gets an archive or the checkout.
3. **A macOS application** with windows and a menu-bar item. The smoothest, but a new
   code base, a paid Apple signature and nothing for Linux. The product needs Developer
   mode in Chrome and Claude Code anyway, so its audience does not avoid a terminal.

Chosen: the second (the owner, 2026-11-10), with the formula in a tap shared by all his
tools, as `umputun/homebrew-apps` has it: the repository `materkey/homebrew-apps`, the
file `Formula/recog.rb`, hence `brew install materkey/apps/recog`. A tap per tool (the
existing `materkey/homebrew-ccs`) repeats the name: `materkey/ccs/ccs`.

### Release distribution

On 2026-11-18 the owner chose public distribution for the first release. Publish the source in
`materkey/recog`. For each release tag, build one binary archive per supported
operating system and architecture and attach the archives to that tag's GitHub Release
in `materkey/recog`.

The public tap `materkey/homebrew-apps` carries the formula. At release time, update
it with the versioned archive URLs and checksums. Homebrew installs the matching
archive on both first install and upgrade. Users can download it anonymously and
install the Rust programs without a local Rust build.

On macOS, `recog setup` builds the small `Recog.app` launcher locally with `swiftc`
from the Command Line Tools, as today. The launcher is rebuilt only when its source
changes, so ordinary binary updates preserve the macOS permission grants.

## Route

The first public release ships once steps 1–3 work together. The release archive
includes the built extension. Setup prints its stable installation path and guides
the user through Developer mode and Load unpacked at `chrome://extensions`.

| Step | What | Needs from the owner |
|------|------|----------------------|
| 1 | Everything the scripts do moves into `recog`; the installation is still a checkout. Designed below. | Nothing |
| 2 | Public source and tagged binary releases in `materkey/recog`; installation from the release archives without a checkout. | Create the public tap `materkey/homebrew-apps` |
| 3 | A guided first run: the permissions one at a time in `recog setup`, a readiness list in the web UI from the checks of `recog doctor`. | Nothing |

Known for step 2 already: `recog_launcher` takes the program to run as its argument, so
`Recog.app` can sit at a stable path outside Homebrew's versioned directory and its
bytes do not change with an update. The permissions then survive without a paid
signature.

### First public release acceptance on macOS

On a clean Mac, install the release through Homebrew and run `recog setup`. Verify
the following through the installed service launched by `Recog.app`:

- Setup asks for Microphone and Screen Recording one at a time. Each step names
  **Recog** as the app to authorize and explains what it captures. Screen Recording
  is needed to hear the other side of a call through system audio.
- Deny each permission in turn and rerun setup. It identifies the missing permission,
  shows the matching System Settings pane and entry, and lets the user continue
  after granting access. Check microphone input with a spoken phrase and system
  audio with a playing clip. Setup, `recog doctor` and the web readiness list report
  the two results separately.
- Enable native Zoom integration and grant Accessibility to **Recog Zoom Helper**.
  Verify that the helper picks up the grant. With native Zoom disabled, basic
  transcription works without that grant.
- Load the unpacked extension from the path printed by setup and join a supported
  call. Verify that the first transcript appears. Update to a release with changed
  extension files and unchanged helper signatures. Follow the Reload prompt in
  Chrome and repeat the capture checks with the existing grants.

## Step 1: the scripts' work moves into `recog`

### Commands

| Today | Becomes |
|-------|---------|
| `./setup.sh [flags]` | `recog setup [the same flags]`; the first time `cargo setup` |
| `./recog-service.sh install\|uninstall\|start\|stop\|status` | `recog service install\|uninstall\|start\|stop\|status` |
| `restart`, `restart-web`, `restart-live`, `restart-copilot`, `restart-supervisor` | `recog service restart [name]`, the names being `live`, `frontend`, `copilot-channel`, `copilot-supervisor` |
| `logs`, `logs-out`, `logs-web`, `logs-copilot`, `logs-supervisor` | `recog service logs [name] [--stdout]`, `live` by default |
| `render --dry-run <dir>` | `recog service render <dir>` |
| `debug` | `recog service debug` |
| `claude` | `recog copilot` |
| `scripts/backup-data.sh` | `recog backup` |
| `scripts/migrate-data.py` | `recog migrate-data [--apply]` |
| `scripts/data-dir.sh`, `scripts/config-path.sh` | Deleted: `recog config data-dir\|config-path` exist |
| `scripts/deps.sh` | The checks inside `recog`, printed by `recog doctor` |

Two commands are new:

- **`recog update`**, in a checkout: refuses when the branch cannot fast-forward, runs
  `git pull --ff-only`, prints the `CHANGELOG.md` entries the pull brought, then runs
  `cargo setup`.
- **`recog doctor`**: the dependency table of setup, the readiness problems of the start
  gate and the state of the services. It changes nothing and exits non-zero when the
  pipeline cannot serve.

### What changes in behaviour

Everything not listed here is carried over as it is: the scripts and their tests are the
specification.

1. **Copilot does not stop the installation.** A missing tmux or Claude Code is a
   warning row. `RECOG_SKIP_COPILOT_CHECK` is accepted and does nothing.
2. **Nothing changes under a conference.** A conference is on when the live service
   runs and the meeting storage lists a meeting that has not ended and whose source is
   a conference's (`list_active_meetings`, `is_conference_source` of `recog-core`).
   Then `recog update` stops before the pull, and `recog setup` after the build and the
   configuration check, before it installs anything. Both say that running the command
   again after the call finishes the job; `--force` goes on regardless.
   - Replacing the files and only skipping the restart would not be enough. The running
     live process starts `bin/recog-mic` and `sck_audio_capture` by path whenever they
     die, setup recompiles that helper on every run, and the web server reads its pages
     from the checkout on every request; none of them carries a version.
   - A recording without a conference (the microphone alone) does not hold an update
     back: the service listens all day.
   - An explicit `recog service restart` always restarts.
3. **One reader of the configuration.** `[install] runtime` comes from
   `recog_core::install_config`; the awk reader and the comparison of the two disappear.
   `"python"` is refused with today's message.
4. **The first run.** `cargo setup` is an alias in `.cargo/config.toml` for
   `run --release --bin recog -- setup`. A `recog setup` started from an older `bin/recog`
   builds first and hands the rest to the freshly built program, so the steps after the
   build are always the new code.
5. **`recog` on `PATH`.** Setup links `~/.local/bin/recog` to `bin/recog` when that
   directory is on `PATH` and holds no other `recog`; otherwise it prints the line to
   add. The program resolves the link before it looks for its checkout.
6. **The mark of a checkout.** `installed_repo_of` recognises one by `recog-service.sh`
   today; it needs a file every checkout has and an installation without one will not.
7. **`recog copilot`** runs the supervisor's own command (`build_claude_command`) in the
   foreground, through `bin/recog-copilot-supervisor` as `recog desktop` goes through
   `bin/recog-desktop`, so the `recog` binary does not take on the Copilot crate. The
   person at the keyboard answers the startup dialog; `expect` is not needed.
8. **`recog service debug`** prints `/api/debug` formatted, without curl and jq.
9. **Overrides for tests.** `RECOG_SERVICE_OS` and `RECOG_SKIP_READINESS` stay.
   `RECOG_HOST_OS` and `RECOG_HOST_ARCH` existed to drive shell probes from another OS
   and go: the tests name the OS directly.

### Where the code lives

- A new library crate `crates/recog-install` holds the checks, the unit rendering, the
  service managers, the setup steps, the backup and the data migration. It may depend
  on `recog-core`, `recog-ai` (the configuration check) and `recog-zoom` (the bundle
  builder of `Recog.app`), never on `recog-live`: what only the live crate has (the
  model download) the command line passes in. The `recog` binary stays the only entry
  point: `crates/recog-live/src/cli/` parses the arguments (in its own hand-written
  parser) and calls the crate.
- The unit templates stay in `deploy/{macos,linux}/*.in` and are compiled into the
  program (`include_str!`), so step 2 needs no files beside it. Rendering is string
  substitution with today's escaping per format; a surviving `@TOKEN@` is still an error.
- Every external command (`launchctl`, `systemctl`, `cargo`, `npm`, `swiftc`, `git`,
  `tmux`) goes through one seam, so tests replace the service manager with a fake inside
  the process, as the pytest suites do with stub programs on `PATH`.
- A check is a row: its name, what needs it (the pipeline, Copilot, the extension build,
  a convenience), the probe, and the hint per package manager. Setup's table, the start
  gate and `recog doctor` read the same rows.

### Tests

- The pytest suites of the scripts (`tests/test_setup.py`, `test_service_templates.py`,
  `test_install_without_python.py`, `test_profile_backup.py`, `test_data_migration.py`,
  about 1900 lines) become Rust tests of `recog-install`, case for case where the
  behaviour is kept.
- Before the scripts are deleted, the units of both backends are rendered by the shell
  and by Rust and compared byte for byte; the shell's output stays as fixtures.
- The quality gate measures Rust and never measured shell, so the port arrives with its
  tests in the same commits and within the gate's limits per function.

### What stays shell

`scripts/check.sh` (development and CI; moving it into a `cargo xtask` is a separate
piece of work), `scripts/benchmarks/rust_asr/run.sh`, the two scripts of the extension's
test stand, and the nine-line entrypoint of the CI runner's container. Python stays for
the quality tools only.

### Existing installations

The units already run `<checkout>/bin/…`, so nothing is registered anew. `CHANGELOG.md`
gets an "Action required" entry: after the pull run `cargo setup` once (later `recog
update`); `./recog-service.sh X` is now `recog service X`. No script is left behind to
point at the new command.

### Documentation

`README.md` (the installation section becomes the two forms above), `docs/usage.md`,
`CLAUDE.md`, `docs/repository-layout.md`, `docs/configuration.md` and the other current
documents that name the scripts. Plans and ADRs that record history keep their wording.

### Left to the builder

The modules inside `recog-install`; the wording of the output (today's lines where a
test asserts them); how the handover to the fresh program is detected; the file that
marks a checkout; the order of the commits (each one leaves a working installation: the
port first, the deletion of the scripts last).

## Decisions that are the owner's

None of them blocks step 1.

- Creating `materkey/homebrew-apps` (step 2).
- A paid Apple Developer ID, only if `Recog.app` is to be shipped ready (then a cask).
