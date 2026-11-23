# Copilot on Codex

## Overview

Copilot, the agent that follows a live meeting and answers in the conference's
Copilot tab, runs only as Claude Code (`claude --agent copilot-session` in tmux,
events pushed over Claude Code's MCP channels). A user whose Claude subscription
has ended and who keeps only Codex (a ChatGPT subscription) has no Copilot at
all, while titles, summaries and meeting questions already fall back to Codex
(commit d0c95a4c).

This plan adds a second Copilot harness: a small recog program that runs Codex's
`app-server` and turns Copilot events into Codex turns. The supervisor, tmux
session, bridge protocol, status chips, channel MCP server and extension stay
as they are; only what runs inside the tmux session changes.

The Copilot scenario's provider order decides which harness starts: by default
Claude Code when `claude` is installed, otherwise Codex.

## Context (from discovery)

- `crates/recog-copilot/src/supervisor/tmux.rs`: `TmuxRunner` starts
  `claude_arguments()` in `tmux -L recog-copilot` (a transient systemd unit on
  Linux); `Runner` exposes `spawn/kill/alive/capture/send_keys`.
- `crates/recog-copilot/src/supervisor/screen.rs` + `watchdog.rs`: the status
  (`spawning|online|working|blocked|offline`) is read from the pane text, with
  Claude Code's markers; startup dialogs are answered with keys.
- `crates/recog-copilot/src/channel/`: MCP server on `127.0.0.1:8767/mcp` with
  `send_copilot_message`, `set_copilot_status`, `get_recent_transcript` and
  `await_copilot_event` (a long-poll of the same events the channel pushes,
  rendered with the same context block and privacy masking).
- `.claude/skills/copilot-mode/SKILL.md` (playbook) and
  `.claude/agents/copilot-session.md` (preload + initial prompt).
- `crates/recog-ai/src/settings.rs`: the `copilot` scenario allows only Claude
  (`providers_value`, `models_value`, `reasoning_effort_value`, `TaskShape`).
- `crates/recog-ai/src/provider.rs` `build_codex_exec_command`: how automation
  already isolates Codex (`--ignore-user-config` plus `-c` switches).

Codex `app-server` facts (read from `~/projects/codex` at 02390c8d, codex 0.171.0
installed; `app-server` is marked experimental):

- stdio, one JSON object per line, `{id, method, params}` without `"jsonrpc"`;
  `initialize` first; stdin EOF ends the process.
- `thread/start` takes `model`, `cwd`, `approvalPolicy`, `sandbox`, `config`
  (dotted keys like `-c`), `developerInstructions`, `ephemeral`; the response has
  `instructionSources`.
- `turn/start {threadId, input:[{type:"text",text}]}`; while a turn runs it
  steers the running turn instead of starting a new one. `turn/interrupt`.
- Notifications: `turn/started`, `item/completed` (agentMessage),
  `turn/completed {turn:{status, error}}`, `error {error:{codexErrorInfo},
  willRetry}` with `usageLimitExceeded`, `rateLimitExceeded`, `unauthorized`,
  `contextWindowExceeded`. `account/read` gives `account: null` when logged out.
- MCP over streamable HTTP via `mcp_servers.<name>.url`. Under approval
  `never` + sandbox `read-only` an MCP tool call is refused unless the server's
  `default_tools_approval_mode = "approve"`. No server push into a thread.
- Personal `AGENTS.md` is read from `CODEX_HOME`; the login lives there too
  (`auth.json`, or the keyring keyed by the CODEX_HOME path).

## Development Approach

- **testing approach**: TDD (the project's ub-mode practice): failing test first.
- complete each task fully before moving to the next; small focused commits.
- every task includes new or updated tests; all tests pass before the next task.
- update this plan when scope changes.
- the Claude Code path keeps working unchanged; an installation with both CLIs
  sees no change unless the user picks Codex for Copilot.

## Testing Strategy

- unit tests in `crates/recog-copilot` (protocol client against a scripted fake
  `app-server`, pure harness state machine, screen classifier fixtures, command
  building) and `crates/recog-ai` (settings).
- settings UI: `tests/ai_settings_ui.test.cjs`.
- no automated end-to-end with a real Codex: a real call with Codex only is a
  manual check (Post-Completion), like the Claude path's.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- keep the plan in sync with the work

## Solution Overview

**D1. The harness runs inside the existing tmux session.** `TmuxRunner` starts
`bin/recog-copilot-codex` instead of `claude` when the Copilot provider is
Codex. Spawn, kill, alive, adoption after a supervisor restart, the Linux
transient unit and `tmux attach` for a human all stay as they are.

**D2. Status through the pane, as for Claude.** The harness keeps its last
line as `recog-copilot-codex: <state>[: <reason>]`; `classify_screen` checks
for that line before Claude's markers. `spawning` until the thread exists,
`online` idle, `working` during a turn, `blocked` on `usageLimitExceeded`,
`unauthorized`, a missing login (`account/read` → `account: null`) or a
refused tool. The pane dying is `offline` as today. The harness answers no
dialogs, so the dialog latch never fires for it.

**D3. Events come from `await_copilot_event`.** The harness is an MCP client of
the channel and long-polls `await_copilot_event`; each event becomes a
`turn/start` with the event's rendered text, which already carries the context
block, the current Copilot instructions and privacy masking. Rejected
alternative: a second bridge WebSocket client in the harness, which would
duplicate the channel's rendering and masking.

**D4. One turn at a time, decided by a pure state machine.** A user message
while a turn runs steers that turn (Codex's own behaviour, the reply carries its
`request_id`); a transcript tick while a turn runs is held, only the latest,
and sent when the turn completes; `meeting_ended` is sent and the harness exits
after its turn. `contextWindowExceeded` starts a fresh thread with the same
instructions (a long meeting outgrows the context; Codex compacts on its own
before that).

**D5. Isolation as for automation, verified at start.** The thread runs with
`approvalPolicy: "never"`, `sandbox: "read-only"`, `ephemeral: true`, a cwd
without `AGENTS.md` under the data directory, and the `config` overrides of
`build_codex_exec_command` plus
`mcp_servers.copilot.url = <RECOG_COPILOT_MCP_URL>` and
`mcp_servers.copilot.default_tools_approval_mode = "approve"`. The playbook and
the agent's prompt go in as `developerInstructions`, with one sentence saying
that events arrive as user turns rather than channel blocks. `app-server` has no
`--ignore-user-config`, so the harness runs Codex with its own `CODEX_HOME`
under the data directory (`<data>/copilot/codex-home`) whose `auth.json` links
to the user's (Codex follows the link on refresh, tested upstream but
undocumented); a login kept in the keyring instead shows as `account: null`
and the harness reports `blocked: codex login`. The model is passed explicitly:
without the user's config Codex falls back to its own default.

**D6. Provider choice.** The `copilot` scenario allows Claude and Codex;
default order `["claude", "codex"]`. At spawn the supervisor takes the first
provider in that order whose CLI is found (`which`), so nothing changes where
Claude Code is installed. A user who saves one provider gets only that one.
Codex's model and reasoning effort for Copilot come from the same settings.

## Technical Details

- New module `crates/recog-copilot/src/codex/`:
  - `app_server.rs`: child process, line framing, request ids, typed
    notifications (`TurnStarted`, `TurnCompleted{status, error}`,
    `AgentMessage{text}`, `Error{info, will_retry}`), `initialize`,
    `account/read`, `thread/start`, `turn/start`, `turn/interrupt`.
  - `harness.rs`: the pure state machine of D4 (input: channel event or
    app-server notification; output: commands and the status line).
  - `events.rs`: the MCP client loop over `await_copilot_event`.
  - `instructions.rs`: developer instructions from the playbook files.
- New binary `crates/recog-copilot/src/bin/recog-copilot-codex.rs`:
  `--repo-root`, `--mcp-url`, `--meeting-id`; reads the Copilot settings once.
- `rmcp` gains the `client` and streamable-HTTP client features.
- Status line written with `\r\x1b[2K` on the last line so the pane always
  ends with the current state; the dialog stays above it for a human reader.

## What Goes Where

- **Implementation Steps**: code, tests and docs in this repository.
- **Post-Completion**: a real call with Codex only; Codex version drift.

## Implementation Steps

### Task 1: Spike the app-server contract on a real Codex

**Files:**
- Create: `crates/recog-copilot/tests/fixtures/codex/` (recorded lines)
- Modify: `docs/plans/2026-11-23-codex-copilot.md`

- [x] start `codex app-server` with the D5 overrides; record `initialize`,
      `account/read`, `thread/start` (with `instructionSources`) and one turn
      calling `get_recent_transcript` on a running channel
- [x] confirm the MCP tool runs without an approval request under D5
- [x] decide D5's CODEX_HOME question from `instructionSources`; write the
      answer into this plan
- [ ] keep the recorded lines as fixtures (no meeting content: a synthetic
      transcript), with paths and the account replaced, in Task 2

Results (2026-11-23, codex-cli 0.171.0, scratch run, not in the repository):
`app-server` has no `--ignore-user-config`. With a separate `CODEX_HOME`
holding only a link to `~/.codex/auth.json`: `account/read` gave the ChatGPT
account; `thread/start` with `approvalPolicy: never`, `sandbox: read-only`,
`ephemeral` and the MCP server in its `config` map gave
`instructionSources: []` (no personal `AGENTS.md`, none of the user's 13 MCP
servers) and the default model `gpt-6.2-sol`; one turn called
`copilot/get_recent_transcript` with no approval request (the isolated channel
had no bridge, so it returned "bridge not connected"); closing stdin ended the
process with 0. The stream also carries `mcpServer/startupStatus/updated`,
`thread/status/changed`, `account/rateLimits/updated` and
`remoteControl/status/changed`, which the client ignores.

### Task 2: app-server protocol client

**Files:**
- Create: `crates/recog-copilot/src/codex/mod.rs`, `app_server.rs`
- Create: `crates/recog-copilot/tests/codex_app_server.rs`

- [ ] write failing tests against a scripted fake server (a shell or Rust stub
      replaying the fixtures): handshake order, request/response matching,
      notification parsing, error with `codexErrorInfo`, EOF as exit
- [ ] implement the client
- [ ] tests for malformed lines and an early child exit
- [ ] run `cargo test -p recog-copilot` - must pass before task 3

### Task 3: Harness state machine

**Files:**
- Create: `crates/recog-copilot/src/codex/harness.rs`

- [ ] failing tests, one per rule of D4: idle event → turn; user message during
      a turn → steer; ticks during a turn → only the latest after completion;
      `meeting_ended` → exit after its turn; `usageLimitExceeded` /
      `unauthorized` / no login → blocked with reason; `contextWindowExceeded`
      → new thread
- [ ] implement the pure state machine (no I/O)
- [ ] run tests - must pass before task 4

### Task 4: Events from the channel

**Files:**
- Create: `crates/recog-copilot/src/codex/events.rs`
- Modify: `crates/recog-copilot/Cargo.toml`

- [ ] failing test: an in-process channel MCP server delivers a queued bridge
      event through `await_copilot_event` to the client loop; a timeout yields
      nothing; a lost server is reported and retried
- [ ] implement the loop with rmcp's streamable-HTTP client
- [ ] run tests - must pass before task 5

### Task 5: `recog-copilot-codex` binary and instructions

**Files:**
- Create: `crates/recog-copilot/src/bin/recog-copilot-codex.rs`,
  `crates/recog-copilot/src/codex/instructions.rs`
- Modify: `crates/recog-install` (programs copied into `bin/`)

- [ ] failing tests: instructions built from the playbook and agent files with
      the user-turn sentence; status line format; settings read once
- [ ] wire client, events and state machine; write the status line
- [ ] `cargo setup` installs the binary like the other Copilot programs
- [ ] run tests - must pass before task 6

### Task 6: Copilot settings accept Codex

**Files:**
- Modify: `crates/recog-ai/src/settings.rs`, `crates/recog-ai/tests/settings.rs`
- Modify: `crates/recog-copilot/src/ai_settings.rs`
- Modify: `crates/recog-web/static/ai-settings.js`, `tests/ai_settings_ui.test.cjs`

- [ ] failing tests: `copilot` allows `["claude", "codex"]`, default order
      `["claude", "codex"]`, Codex model and effort accepted; a saved single
      provider stays single
- [ ] implement; the UI shows the provider choice and fallback for Copilot
- [ ] run tests - must pass before task 7

### Task 7: Supervisor starts the chosen harness

**Files:**
- Modify: `crates/recog-copilot/src/supervisor/tmux.rs`
- Modify: `crates/recog-install/src/readiness.rs`

- [ ] failing tests: the first provider of the order whose CLI is found picks
      the command; Codex builds `recog-copilot-codex --repo-root … --mcp-url …
      --meeting-id …`; Claude's command unchanged
- [ ] implement; readiness says Copilot needs tmux and Claude Code or Codex
- [ ] run tests - must pass before task 8

### Task 8: Screen classifier knows the harness

**Files:**
- Modify: `crates/recog-copilot/src/supervisor/screen.rs`

- [ ] failing fixture tests: each status line maps to its state; a Claude
      screen is classified as before
- [ ] implement the rule ahead of Claude's markers
- [ ] run tests - must pass before task 9

### Task 9: Verify acceptance criteria

- [ ] with `claude` off `PATH`, a manual session from the web UI starts the
      Codex harness, answers a chat message and reports 🟢/🟡
- [ ] with both installed, Copilot still starts Claude Code
- [ ] `./scripts/check.sh`; the quality gate on Linux shows no new CRAP,
      erosion or duplicates

### Task 10: [Final] Update documentation

- [ ] `docs/usage.md` Copilot section, README requirements, CHANGELOG
- [ ] CLAUDE.md: the Copilot supervisor section names the Codex harness
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

**Manual verification:**
- a real test call (Jitsi or a personal KTalk room) with Codex only: a typed
  question gets a reply, proactive notes follow the instructions, a spent
  subscription shows 🔴 with the reason.

**Risks:**
- `app-server` is experimental; a Codex update may rename methods or fields.
  The fixtures pin the version they were recorded on; re-record after updates.
- Every transcript tick is a Codex turn on the user's subscription, as with
  Claude; the existing tick cadence and the instructions' silence rules apply.
