# AI Functions Switch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One level in `ai-settings.json` (`unasked` / `off` / `manual` / `automatic`) decides whether recog starts `claude` or `codex` at all, only on a click, or also after each meeting.

**Architecture:** The level lives in the AI settings store next to the scenarios and is resolved from the same snapshot as each scenario (`ResolvedTask.access`). `ProviderFactory::create` refuses below `manual`; the two automatic paths and the Copilot autostart also require `automatic`; the Copilot's on/off already gates the bridge and the supervisor, so it is computed as "switch and level". The live process and the web server create a missing settings file once at startup, from whether an earlier run left a finished meeting: without the field for an existing installation (which a file without it already means), with `unasked` for a new one; an existing file is never rewritten. From that commit on no file at all reads as `unasked`, so a failed settle fails closed.

**Tech Stack:** Rust workspace (`recog-ai`, `recog-bridge`, `recog-copilot`, `recog-live`, `recog-web`), the web UI's plain JS (`crates/recog-web/static/`), `node --test` UI tests in `tests/*.test.cjs`.

**Spec:** `docs/plans/2026-11-23-ai-functions-switch-design.md`

## Global Constraints

- Field: top-level `"access"` of `ai-settings.json`; values exactly `"unasked"`, `"off"`, `"manual"`, `"automatic"`. Absent from a file means `automatic` (a file from before this change). No file at all means `automatic` until Task 7 and `unasked` from Task 7 on (the startup settle has not run or failed: fail closed), so no earlier commit leaves a new installation refused with no question to answer.
- The field is written only by a level chosen in the UI and by creating a file where there was none (the settle of a new installation; from Task 7 also a save after a failed settle, which keeps `unasked`); nothing rewrites an existing file to add it, so a build from before this change (a rollback, or an older binary on the same data dir) keeps reading every other file.
- Refusal text, everywhere a user can see it: `AI-функции выключены в настройках recog.`
- Level labels in the UI, exactly: «Выключено», «Только по кнопке», «По кнопке и после каждой встречи».
- The UI never writes `unasked`; only settling at startup does.
- Test names follow given – when – then (`file_without_access_when_resolved_then_automatic` in Rust, `'unasked - click on an AI button - question opens first'` in JS).
- Quality gate (`docs/quality.md`): no new function over CRAP 10 or with cyclomatic/cognitive complexity over 10; an existing function fails when it ends above CRAP 10 having risen by more than 1, or, once complex, when its mass grows. The functions this plan extends (`on_meeting_frame`, `refresh_enabled`, `generate`, `generate_all`, `render`, `save`) get the new branches in small helpers, not inline. A JS callback starts on its own line. Fast check: `python3 scripts/quality.py --style-only --base origin/main`; the full gate runs once at the end (Task 8).
- `CHANGELOG.md` gets its entry in the commit that changes the behaviour.

## Review Focus

1. Live process and web server settle at the same start: the second, waiting on the lock, must read under it, find the file the first created and leave it (and its revision) alone; a settle never rewrites an existing file; the web server settles before it accepts a connection; a failing settle never stops transcription. Pinned in Task 1 and Task 7.
2. An unreadable `ai-settings.json`: automatic paths must skip (fail closed), not run with defaults. Pinned in Task 3.
3. Switching `automatic` → `manual` during a conference: the running Copilot keeps running, also through the GRACE a bridge restart puts it in, and the meeting's end still ends it; below `automatic` no frame spawns a conference session (a manual session survives a supervisor reconnect during a conference); `manual` → `automatic` during a conference starts its Copilot. Pinned in Task 4.
4. A tab opened before the level was switched off elsewhere: its AI Analysis, Ask, Generate Title and Regenerate clicks reach the server and must get the refusal text, not a provider error, and Regenerate must claim no version. Pinned in Task 3 (the claim) and Task 5.
5. Two settings tabs: saving the level in one bumps the revision, and the other tab's next save must show the existing conflict banner; a level save that fails or cannot be made leaves the radio on the saved level. Pinned in Task 6.
6. No settings file and a settle that failed: every path refuses and the UI asks (no file reads as `unasked`). Pinned in Task 7.

---

### Task 1: The level in the settings store

**Files:**
- Create: `crates/recog-ai/src/access.rs` (the enum, its parsing, the finished-meeting scan)
- Modify: `crates/recog-ai/src/lib.rs` (module), `crates/recog-ai/src/settings.rs` (`validate_raw`, `validate_header`, `ResolvedTask`, `View`, `resolve_task`, `view_of`, `mutate`, new `write_access`, `settle_access`)
- Test: `crates/recog-ai/tests/access.rs`

**Interfaces:**
- Produces:
  - `pub enum Access { Unasked, Off, Manual, Automatic }` (`Clone, Copy, Debug, PartialEq, Eq`) with `fn as_str(self) -> &'static str`, `fn parse(text: &str) -> Option<Access>`, `fn allows_manual(self) -> bool` (Manual | Automatic), `fn allows_automatic(self) -> bool` (Automatic).
  - `pub fn access_of(snapshot: &Snapshot) -> Access` in `settings.rs` (absent → `Automatic`; the snapshot is validated, so parsing cannot fail).
  - `ResolvedTask.access: Access` (not in `ResolvedTask::to_json`); `View.access: Access`, serialized by `View::to_json` as `"access"` after `"revision"`.
  - `AiSettingsStore::write_access(&self, access: Access, expected_revision: Option<&str>) -> Result<View>`: `Unasked` → `AiError::Value("Invalid AI access")`.
  - `AiSettingsStore::settle_access(&self, meetings_dir: &Path) -> Result<Access>`, all under one write lock: a file exists → its level (`access_of`), nothing written; no file → it creates one with a new revision, empty `models` and `tasks`: without `access` when `has_finished_meeting` (→ `Automatic`; the four-field file a build from before this change reads), with `"access": "unasked"` otherwise (→ `Unasked`).
  - `pub fn has_finished_meeting(meetings_dir: &Path) -> bool` in `access.rs`: true at the first `<dir>/meta.json` whose `ended_at` is a non-empty string; a missing directory, an unreadable or malformed meta counts as none.

- [ ] **Step 1: Write the failing tests** in `crates/recog-ai/tests/access.rs` (tempdir data dir, store `AiSettingsStore::new(&data, None)`):
  - `file_without_access_when_resolved_then_automatic`: write a valid file without `"access"`; `resolve_task("title", …).access == Access::Automatic`, `view().to_json()["access"] == "automatic"`.
  - `no_file_and_no_meetings_when_settled_then_unasked_is_written_and_kept`: `settle_access` → `Unasked`; the file now has `"access": "unasked"`; then create `meetings/a/meta.json` with `ended_at` and settle again → still `Unasked`.
  - `finished_meeting_of_an_earlier_run_when_settled_then_automatic_without_the_field`: `meetings/a/meta.json` = `{"ended_at": "2026-11-01T10:00:00"}` → `Automatic`; the created file's keys are exactly `version`, `revision`, `models`, `tasks`.
  - `only_an_open_recording_when_settled_then_unasked`: `meta.json` without `ended_at` (and one with `"ended_at": null`) → `Unasked`.
  - `access_present_when_settled_again_then_file_and_revision_unchanged`: settle twice; the second leaves `revision` equal.
  - `existing_file_without_access_when_settled_then_automatic_and_bytes_unchanged`: a valid four-field file and no meetings → `Automatic`; the file's bytes are the ones written before.
  - `settle_waiting_on_the_lock_when_another_process_writes_meanwhile_then_its_file_is_kept`: the test holds `ai-settings.lock` (flock on its own descriptor), starts `settle_access` in a thread, waits 100 ms, writes a valid file with `"access": "off"`, unlocks; the thread returns `Off` and the file's bytes are the test's (a settle that read before locking would overwrite it with `unasked`).
  - `write_access_off_when_viewed_then_off_and_new_revision`; `write_access_unasked_when_saved_then_invalid_ai_access`.
  - `unknown_access_value_in_file_when_read_then_settings_read_error`: `"access": "sometimes"` → `AiError::SettingsRead`.
- [ ] **Step 2: Run** `cargo test -p recog-ai --test access` — expected: does not compile (`Access`, `settle_access` missing).
- [ ] **Step 3: Implement.** `validate_raw` accepts the optional fifth key `access`; `validate_header` requires exactly the four fields plus at most `access`, and `access` one of the four strings (message `Invalid AI access`). `settle_access` takes the write lock once and reads only under it (never before), returns the file's level when a file exists, else decides with `has_finished_meeting` and creates the file as above. Split `mutate` so the locked part (`revision check → update → new revision → validate → view → replace`) can run under a lock already held: never take `write_lock` twice in one process (flock on a second descriptor blocks).
- [ ] **Step 4: Run** `cargo test -p recog-ai` — expected: PASS, the existing settings tests unchanged.
- [ ] **Step 5: Commit** `Keep an AI functions level in the AI settings`.

### Task 2: No provider below `manual`

**Files:**
- Modify: `crates/recog-ai/src/error.rs` (`AiError::Disabled(String)`, `pub const DISABLED: &str`), `crates/recog-ai/src/provider.rs` (`ProviderFactory`), every `impl ProviderFactory` (`crates/recog-ai/src/provider.rs`, `crates/recog-ai/tests/common/mod.rs` ×2, `crates/recog-ai/tests/increment.rs` ×2, `crates/recog-ai/tests/title.rs`, `crates/recog-web/tests/support/mod.rs`)
- Test: `crates/recog-ai/tests/provider_access.rs`

**Interfaces:**
- Consumes: `ResolvedTask.access`, `Access::allows_manual` (Task 1).
- Produces: `trait ProviderFactory { fn build(&self, settings, task, provider) -> Result<Box<dyn AiProvider>>; fn create(…) -> Result<…> /* provided */ }`, where `create` returns `Err(AiError::Disabled(DISABLED.into()))` unless `settings.access.allows_manual()`, else `self.build(…)`. `DISABLED = "AI-функции выключены в настройках recog."`. Every existing caller keeps calling `create`.

- [ ] **Step 1: Write the failing tests:** `level_off_when_completing_in_order_then_disabled_and_nothing_built` (a counting fake; `complete_in_order` → `Err(Disabled)` with the exact text, build count 0); `level_unasked_when_creating_then_disabled`; `level_manual_when_creating_then_built`.
- [ ] **Step 2: Run** `cargo test -p recog-ai --test provider_access` — expected: does not compile (`build`, `Disabled`).
- [ ] **Step 3: Implement**; rename `create` → `build` in each implementation; `Display` of `Disabled` is its message.
- [ ] **Step 4: Run** `cargo test -p recog-ai -p recog-web` — expected: PASS.
- [ ] **Step 5: Commit** `Start no AI provider while the AI functions are off`.

### Task 3: The automatic paths wait for `automatic`

**Files:**
- Modify: `crates/recog-ai/src/increment.rs` (`generate_all`, `generate`), `crates/recog-ai/src/title.rs` (`try_meeting_title`)
- Test: `crates/recog-ai/tests/increment.rs`, `crates/recog-ai/tests/title.rs`

**Interfaces:**
- Consumes: `access_of`, `Access::{allows_manual, allows_automatic}` (Task 1); `AiError::Disabled`, `DISABLED` (Task 2).

- [ ] **Step 1: Write the failing tests:**
  - `level_manual_when_meeting_ends_then_no_summary_version_and_no_provider_call` (settings file with `"access": "manual"`, a finished meeting with segments; `generate_all` → `Ok(())`, no `increments/` directory, fake not called).
  - `unreadable_settings_when_meeting_ends_then_no_summary` (`ai-settings.json` = `{` → `Ok(())`, nothing generated).
  - `level_manual_when_recording_closes_then_no_title_and_no_provider_call` (`meeting_title_update` → `None`, fake not called).
  - `level_off_when_regenerating_then_disabled_and_no_version_claimed` (`generate(…, force = true, None)` on a meeting with segments → `Err(AiError::Disabled)` with the exact text; no `increments/` directory; fake not called).
- [ ] **Step 2: Run** `cargo test -p recog-ai --test increment --test title` — expected: the four FAIL.
- [ ] **Step 3: Implement.** `generate_all` reads the store once before scanning prompts; a level below `automatic` prints `[increment] skipped: AI functions after meetings are off (<level>)`, an unreadable file prints `[increment] skipped: <error>`; both return `Ok(())`. `try_meeting_title` resolves the `title` scenario before reading the transcript and returns `Ok(None)` below `automatic`, printing `Title generation skipped: AI functions after meetings are off`. `generate` (Regenerate, a participant's «Создать сохранённый результат», and each prompt of `generate_all`) reads the level before `claim_version` and returns `Err(AiError::Disabled(DISABLED.into()))` below `manual`, so a refused click neither claims nor stores a failed version. `generate_title` is not touched (its route checks the level, Task 5).
- [ ] **Step 4: Run** `cargo test -p recog-ai` — expected: PASS.
- [ ] **Step 5: Commit** `Generate summaries and titles after meetings only at the automatic level`.

### Task 4: The Copilot follows the level

**Files:**
- Modify: `crates/recog-copilot/src/ai_settings.rs` (`CopilotTask.automatic`, `resolve`), `crates/recog-copilot/src/supervisor/lifecycle.rs` (`SessionLifecycle::meeting_resumed`, `reconcile_without_autostart`), `crates/recog-copilot/src/supervisor/glue.rs` (`on_meeting_frame`, a `read_automatic`, `refresh_enabled`, `Inner.automatic`), `crates/recog-copilot/tests/common/mod.rs` and `crates/recog-copilot/tests/settings.rs` (literals), `crates/recog-bridge/src/ai.rs` (`read_policy`), `crates/recog-bridge/src/relay.rs` (`on_recent_transcript_request`), `crates/recog-live/src/cli/install.rs` (`copilot_launch`)
- Test: `crates/recog-copilot/tests/settings.rs`, `crates/recog-copilot/tests/lifecycle.rs`, `crates/recog-copilot/tests/supervisor.rs`, the bridge's relay tests (`crates/recog-bridge/tests/`, next to the existing `get_recent_transcript` cases), the bridge's `AiSettingsPolicy` test

**Interfaces:**
- Consumes: `ResolvedTask.access`, `Access::{allows_manual, allows_automatic}` (Task 1).
- Produces: `CopilotTask { enabled, automatic, … }` where `enabled = task.enabled.unwrap_or(true) && task.access.allows_manual()` and `automatic = task.access.allows_automatic()`; `CopilotPolicy.enabled` computed the same way as `enabled`.
  - `SessionLifecycle::meeting_resumed(&mut self, meeting_id: &str) -> Vec<Command>`: the same-meeting branch of `meeting_started` alone (resolves an unknown id after adopt, GRACE → RUNNING); while IDLE or bound to another meeting it changes nothing; it never returns a command.
  - `pub fn reconcile_without_autostart(machine: &mut SessionLifecycle, active: bool, meeting_id: Option<&str>, session_alive: bool) -> Vec<Command>`: `reconcile` with `meeting_resumed` wherever `reconcile` calls `meeting_started` (no session → nothing; IDLE with a live session → adopt, as today). `reconcile` itself is unchanged.

- [ ] **Step 1: Write the failing tests:**
  - settings: `access_manual_when_resolved_then_enabled_and_not_automatic`; `access_off_when_resolved_then_disabled`.
  - lifecycle: `meeting_resumed_for_the_same_meeting_in_grace_then_running_without_commands`; `meeting_resumed_while_idle_or_for_another_meeting_then_nothing_changes`; `reconcile_without_autostart_active_meeting_and_no_session_then_no_spawn`; `reconcile_without_autostart_manual_session_and_a_conference_active_then_no_kill_and_no_spawn`.
  - supervisor: `manual_level_when_meeting_started_then_no_spawn`; `manual_level_when_meeting_state_active_without_session_then_no_spawn`; `manual_level_and_live_conference_session_when_meeting_ended_then_grace_then_kill`; `manual_level_when_spawn_requested_then_manual_session_spawns`; `manual_level_and_a_manual_session_when_a_reconnect_reports_a_conference_then_it_keeps_running` (no Kill, no Spawn); `manual_level_and_a_conference_session_put_in_grace_by_a_bridge_restart_when_the_same_meeting_starts_then_it_keeps_running` (the grace timer is cancelled); `manual_level_then_automatic_during_a_conference_when_settings_change_then_the_conference_session_spawns`.
  - bridge: `copilot_off_when_channel_asks_recent_transcript_then_no_segments` (buffered segments exist, policy disabled → `segments == []`); `settings_access_off_when_policy_read_then_disabled`.
  - `recog config copilot-launch` (`copilot_launch`): `access_off_when_copilot_launch_then_refused_with_disabled_text`.
- [ ] **Step 2: Run** `cargo test -p recog-copilot -p recog-bridge -p recog-live` — expected: the new tests FAIL (the lifecycle ones do not compile: `meeting_resumed`, `reconcile_without_autostart` missing).
- [ ] **Step 3: Implement.** In `on_meeting_frame` (after the existing `autospawn` gate), with `automatic = read_automatic()`: `MEETING_STARTED` calls `meeting_started` when `automatic`, else `meeting_resumed`, so a same-id rejoin still revives a session in GRACE; `MEETING_STATE` calls `reconcile` when `automatic` or the frame's id is `MANUAL_MEETING_ID`, else `reconcile_without_autostart`, so a running session is still adopted and ended but none is spawned, killed or replaced for a conference; `MEETING_ENDED` unchanged. `read_automatic` is false when the settings cannot be read. `refresh_enabled` keeps `Inner.automatic` beside `Inner.enabled` and replays `latest_meeting_state` when either turns on (the manual id excluded, as today), so `manual` → `automatic` during a conference starts its Copilot. The level's choices go into helpers so `on_meeting_frame` and `refresh_enabled` stay within the gate. `copilot_launch` refuses with `DISABLED` when the level is below `manual` (before the existing Copilot-switch check).
- [ ] **Step 4: Run** `cargo test -p recog-copilot -p recog-bridge -p recog-live` — expected: PASS.
- [ ] **Step 5: Commit** `Run the Copilot only as far as the AI functions level allows`.

### Task 5: The level over the web API

**Files:**
- Modify: `crates/recog-web/src/router.rs` (`PUT /api/ai-settings/access` → `Route::SaveAiAccess`, doc table), `crates/recog-web/src/routes/mod.rs` (the route; `ai_failure`: `AiError::Disabled` → 403 with its text, any other error as `fail`), `crates/recog-web/src/routes/settings.rs` (`save_ai_access`; `ai_write_error` unchanged: settings saves never meet `Disabled`), `crates/recog-web/src/routes/increments.rs` (`generate_increment` maps `generate`'s error through `ai_failure`; `generate_title` refuses through it right after `task_settings("title")` when `!settings.access.allows_manual()`, before any provider call), `crates/recog-web/src/ai_settings.rs` (`AiStore::write_access`), `crates/recog-web/tests/ai_settings_api.rs` (`PausingStore`)
- Test: `crates/recog-web/tests/ai_settings_api.rs`, `crates/recog-web/tests/analysis_api.rs`, `crates/recog-web/tests/increment_routes.rs`

**Interfaces:**
- Consumes: `write_access`, `Access::parse` (Task 1); the factory gate (Task 2); `generate`'s refusal (Task 3).
- Produces: `PUT /api/ai-settings/access` with body `{"access": "off"|"manual"|"automatic", "revision": "<uuid>"}` → the view (with `"access"`); 422 `Invalid AI access` for any other value including `unasked`; 409 on a stale revision; the save goes through `AiSettingsCoordinator::write`, so the bridge gets `ai_settings_refresh`. `GET /api/ai-settings` carries `"access"`. Below `manual`, `POST /api/meetings/{meeting_id}/increments/{prompt_id}/generate` (Regenerate, participant) and `POST /api/meetings/{meeting_id}/generate-title` answer 403 with `DISABLED`. AI Analysis, Ask and the analysis stream keep their status and already pass the error's text (`Display` of `Disabled` is `DISABLED`).

- [ ] **Step 1: Write the failing tests:** `access_off_when_saved_then_view_off_and_bridge_refreshed`; `access_unasked_when_saved_then_422`; `stale_revision_when_access_saved_then_409`; in `analysis_api.rs` `level_off_when_analysis_requested_then_refusal_text_and_no_provider` (the page's request returns the `DISABLED` text); in `increment_routes.rs` `level_off_when_title_requested_then_403_refusal_text_and_no_provider` and `level_off_when_regenerate_requested_then_403_refusal_text_and_no_version`.
- [ ] **Step 2: Run** `cargo test -p recog-web --test ai_settings_api --test analysis_api --test increment_routes` — expected: FAIL.
- [ ] **Step 3: Implement** the route as `save_ai_task` does, with `field("access", Ty::Str, true)` and `field("revision", Ty::Str, true)`.
- [ ] **Step 4: Run** `cargo test -p recog-web` — expected: PASS.
- [ ] **Step 5: Commit** `Save the AI functions level from the web UI`.

### Task 6: The web UI asks and hides

**Files:**
- Create: `crates/recog-web/static/ai-access.js`
- Modify: `crates/recog-web/static/index.html` (the level's radio group at the top of `ai-settings-dialog`, radio ids `ai-settings-access-off|manual|automatic`, which the hand-written harness of `tests/ai_settings_ui.test.cjs` picks up from the markup; the question `<dialog id="ai-access-dialog">`; the script tag before `ai-settings.js`; `ai-entry` on every control that starts an AI path: `#meeting-chat-btn`, `#copilot-btn`, the «Новый анализ» tab (`data-ai-mode="new"`) and `#ai-run-button`, but not on the AI Analysis button, which opens the saved summaries), `crates/recog-web/static/app.js` (`ai-entry` on the Generate Title button, Regenerate and «Создать сохранённый результат»; at `off` `showAiHistory` stays on the saved view instead of switching an empty history to «Новый анализ»), `crates/recog-web/static/ai-settings.js` (render and save the level), `crates/recog-web/static/style.css`, `tests/fake_dom.cjs` (capture phase: `addEventListener(type, listener, options)` keeps listeners with `options === true || options?.capture` apart, as does `removeEventListener`, on elements and on the document; `dispatchEvent` runs them from the document down to the target before the bubble walk, and `stopPropagation` there stops the target's own listeners and its inline handler too), `tests/web_page.cjs` (`ALL_SCRIPTS` gains `ai-access.js` in the order of the script tags)
- Test: `tests/ai_access_ui.test.cjs`, `tests/ai_settings_ui.test.cjs`

**Interfaces:**
- Consumes: `GET /api/ai-settings` (`access`, `revision`), `PUT /api/ai-settings/access` (Task 5).
- Produces: `window.recogAiAccess = { level(): string, set(level: string): void }`; `set` writes `document.documentElement.dataset.aiAccess`.

Copy (exact): the question's title «Когда recog может обращаться к AI?»; its text «AI-анализ, вопросы по встрече, названия встреч и Copilot отправляют транскрипт в Claude Code или Codex под вашей подпиской. Выбор можно изменить в «Промпты и модели».»; buttons «Выключено», «Только по кнопке», «По кнопке и после каждой встречи», «Позже». The settings group's legend «AI-функции».

- [ ] **Step 1: Write the failing tests** (`tests/ai_access_ui.test.cjs` with `loadPage` of `tests/web_page.cjs`: the real index.html and every script, so the buttons' own handlers are there to be held back):
  - `'capture listener on the document - click on a button with an inline onclick - the listener runs first and stopPropagation keeps the onclick from running'` (the harness itself).
  - `'unasked - click on an AI button - question opens and the button handler does not run'` (for `#meeting-chat-btn` and `#ai-run-button`).
  - `'question answered manual - saved with a fresh revision - the original click runs once'`.
  - `'question answered off - root marked off - the click does not run'`.
  - `'level off - page loads - root carries data-ai-access off'`.
  - `'unasked and Later - page reloads in the same session - no automatic question'`.
  - `'unasked with the profile welcome open - welcome closes - question opens'`.
  - `'level off - AI Analysis opened on a meeting with saved summaries - they are shown and every control that generates carries ai-entry'` (the tab, the run button, Regenerate).
  - `'level off and nothing saved - AI Analysis opened - the saved view stays'`.
  - in `tests/ai_settings_ui.test.cjs`: `'level radio changed - access saved with the snapshot revision - view replaces the snapshot'`; `'level save answered 409 - conflict banner shown'`; `'level save fails - radio back on the saved level and page root unchanged'`; `'settings busy or in conflict - level radios disabled'`; `'focus refresh with another level - radio and page root follow it'`.
- [ ] **Step 2: Run** `node --test tests/ai_access_ui.test.cjs tests/ai_settings_ui.test.cjs` — expected: FAIL.
- [ ] **Step 3: Implement.** One capture-phase `click` listener on `document` catches clicks inside `.ai-entry` while the level is `unasked` (stop propagation, prevent default, open the question; replay `target.click()` after a choice of `manual` or `automatic`). The automatic question waits for `#profile-dialog` to close when it is open; «Позже» sets `sessionStorage['recog.ai-access.later'] = '1'` (storage errors ignored). CSS: `:root[data-ai-access="off"] .ai-entry { display: none !important; }`. `ai-settings.js`: the level has no draft, so `render()` checks the radio of `data.access` on every snapshot it shows, whatever the preserve flags, and calls `window.recogAiAccess?.set(data.access)`; `controls()` disables the radios with the save buttons (`busy || !snapshot || conflict`), so a change can never be left unsaved; a change calls `save('access')` (`PUT /access` with the snapshot's revision; it clears no draft flag, so drafts survive as after a models save), and a save that fails puts the radio back on `snapshot.access`.
- [ ] **Step 4: Run** `node --test tests/*.test.cjs` and `python3 scripts/quality.py --style-only --base origin/main` — expected: PASS.
- [ ] **Step 5: Commit** `Ask for and show the AI functions level in the web UI`.

### Task 7: Settle the level at startup

**Files:**
- Modify: `crates/recog-live/src/live/session.rs` (`run`, before `setup_meeting_storage`), `crates/recog-web/src/main.rs` (`run`, before `TcpListener::bind`), `crates/recog-ai/src/settings.rs` (`default_snapshot` carries `"access": "unasked"`), `crates/recog-ai/tests/common/mod.rs` and `crates/recog-web/tests/support/mod.rs` (`pre_change_settings`), the recog-ai and recog-web tests that reach a provider with no settings file
- Test: `crates/recog-ai/tests/access.rs`, `crates/recog-web/tests/server_binary.rs`, `crates/recog-live/tests/live_policy.rs`

**Interfaces:**
- Consumes: `AiSettingsStore::settle_access(&data_dir.join("meetings"))` (Task 1).
- Produces: `pub fn pre_change_settings(data_dir: &Path)` in both test helpers: writes `{"version": 1, "revision": <new uuid>, "models": {}, "tasks": {}}`, an installation from before this change (`automatic`). The web test `App::with` calls it; `App::without_settings()` keeps the data dir empty for the absent-file tests of `ai_settings_api.rs` (those sending revision `"defaults"`).

- [ ] **Step 1: Write the failing tests:**
  - `access.rs`: `no_file_when_resolved_then_unasked_and_no_provider_built` (`view().to_json()["access"] == "unasked"`; `complete_in_order` with a counting fake → `Err(Disabled)`, build count 0).
  - `server_binary.rs`: `fresh_data_dir_when_server_starts_then_access_unasked_is_saved` (run the binary as the neighbouring tests do, data dir empty; once `wait_listening` returns, `ai-settings.json` has `"access": "unasked"`, with no further wait); `data_dir_with_a_finished_meeting_when_server_starts_then_a_file_without_access_is_saved`.
  - `live_policy.rs`: `unusable_ai_settings_lock_when_the_session_starts_then_the_main_loop_runs_and_the_failure_is_logged` (`ai-settings.lock` is a directory in the rig's data dir; `session::run` in a thread as in `an_hour_in_the_main_loop_running_the_session_deletes_audio_past_retention`; the main loop starts, `run` returns `Ok`, the log has `[ai] cannot settle the AI functions level:`).
- [ ] **Step 2: Run** `cargo test -p recog-ai --test access && cargo test -p recog-web --test server_binary && cargo test -p recog-live --test live_policy` — expected: FAIL.
- [ ] **Step 3: Implement.** The web server settles before it binds, so a client that can connect finds the file settled. The live process settles in `session::run` without `?`. Both log `[ai] AI functions: <level>` on success and `[ai] cannot settle the AI functions level: <error>` on failure, and go on: transcription never stops for it. `default_snapshot()` gains `"access": "unasked"`, so no file reads as `unasked` everywhere (the bridge's policy, `generate_all`, the factory, the UI). Fixtures: every test that ran AI with no settings file now fails with the `DISABLED` text; each gets `pre_change_settings` (the web `App` already does).
- [ ] **Step 4: Run** `cargo test --workspace` — expected: PASS.
- [ ] **Step 5: Commit** `Ask new installations before the AI functions run`, with the `CHANGELOG.md` entry (Task 8's text for it).

### Task 8: Documents

**Files:**
- Modify: `CHANGELOG.md` (in Task 7's commit), `README.md` (the picture's `alt` at line 37 and the paragraph at line 45), `docs/usage.md` (the AI settings section around "Copilot включён" and the Copilot section), `CLAUDE.md` (the `recog-ai/src/settings.rs` bullet: one sentence on the level)

CHANGELOG, under `## 2026-11-23` → `### Changed`:

```
- AI functions have one level in **Промпты и модели**: off, only on a click
  (AI Analysis, Ask, Generate Title, Copilot started by hand), or also after
  each meeting (the summary, the title and the Copilot autostart). A new
  installation asks before any of them sends a transcript to Claude Code or
  Codex; an existing one keeps everything it did, at the last level.
```

- [ ] **Step 1:** Write the texts; the README says summaries and titles after a meeting run only at the last level.
- [ ] **Step 2: Run** `python3 -m pytest tests/test_repo_hygiene.py -q` — expected: PASS.
- [ ] **Step 3: Commit** `Describe the AI functions level`.
- [ ] **Step 4: Run the full gate** on the committed branch: `python3 scripts/quality.py --base origin/main --output .local/quality` — expected: PASS, `report.md` in the `run-*` directory it prints. A failing function is fixed in the commit of the task that introduced it (`git commit --fixup=<sha>`, then `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash origin/main`), and the gate is run again; a measurement that cannot complete is reported as unverified, never as passed.
