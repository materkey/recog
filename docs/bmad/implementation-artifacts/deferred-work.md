# Deferred Work

### DW-5: `copilot_channel_reply` relays `meeting_id` without type validation, so a non-string or missing value silently disables the cross-meeting staleness guard.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/1-bridge-broadcasts-copilot-frames-to-subscriber-clients.md"), 2026-07-25
location: ktalk_bridge.py:2753
reason: `meeting_id = data.get("meeting_id")` is read without a type check while its siblings `request_id`, `text` and `kind` are all type-checked and rejected; the cross-meeting guard fires only for `isinstance(meeting_id, str) and meeting_id`, so any other type falls through and is relayed verbatim.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-bridge-copilot-reply-guards

Evidence: `ktalk_bridge.py:2753` reads `meeting_id = data.get("meeting_id")` while its siblings `request_id`, `text` and `kind` are all type-checked and rejected. The cross-meeting guard at `:2762` fires only for `isinstance(meeting_id, str) and meeting_id`, so any other type falls through and is relayed verbatim in the broadcast payload. Proactive replies carry a synthetic `proactive_<uuid>` request_id never recorded in `_copilot_pending_requests`, so the stale-prefix check cannot drop them either — `recog/copilot_channel.py` declares `meeting_id: Optional[str] = None`, so an omitted id is a reachable input. Pre-existing; this story only widened the audience that sees the result.

### DW-6: Copilot fan-out awaits each recipient sequentially with no timeout, so one stalled socket delays every recipient queued behind it.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/1-bridge-broadcasts-copilot-frames-to-subscriber-clients.md"), 2026-07-25
location: ktalk_bridge.py:1115-1119
reason: Fan-out iterates `await client.send(message)` in order with no `wait_for`; a recipient applying WebSocket flow-control backpressure without raising blocks the handler for everyone queued behind it.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-bridge-fanout-send-timeout

Evidence: `ktalk_bridge.py:1115-1119` iterates `await client.send(message)` in order with no `wait_for`. The five call sites run inside `_process_message` / `_handle_client`, so a recipient applying WebSocket flow-control backpressure without raising blocks the handler. This is the same pattern `_broadcast_message` has always had, but the recipient set now spans a second process (the frontend launchd service), which is restarted routinely by `./recog-service.sh restart`. Note `send_refine` avoids this by dispatching via `run_coroutine_threadsafe` rather than awaiting.

### DW-7: `meta.json` is written non-atomically while the bridge reads it concurrently, so a torn read makes `_is_meeting_active()` answer False for a live conference.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/2-bridge-relays-manual-spawn-stop-and-tracks-manual-session-state.md"), 2026-07-25
location: meeting_storage.py:1120-1125 (`_save_json`)
reason: `_save_json` is a plain truncate-then-write `path.write_text(...)` driven from the live transcription thread while the bridge's asyncio loop reads it via `_is_meeting_active`, whose `except Exception` returns False — a torn read reports "no conference" for a running meeting. Fix belongs in `_save_json` (write-temp + `os.replace`), not in the bridge.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-atomic-meta-json-write

Evidence: `meeting_storage.py:1120-1125` `_save_json` is a plain `path.write_text(...)` (truncate-then-write) driven from the live transcription thread, while `_load_meta` (`meeting_storage.py:1110`) is read from the bridge's asyncio loop via `_is_meeting_active` (`ktalk_bridge.py:1296-1310`), whose `except Exception` returns False. A partial read therefore reports "no conference" for a meeting that is running. Pre-existing on both sides, but this story attached two new decisions to that predicate — the `conference_active` spawn rejection and the chat `meeting_id` stamp — so a transient parse failure can now forward a spawn during a live conference and un-stamp a conference chat request.

### DW-8: A `copilot_channel_reply` dropped as stale leaves the requester with no terminal status, so the asking UI waits forever.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/2-bridge-relays-manual-spawn-stop-and-tracks-manual-session-state.md"), 2026-07-25
location: ktalk_bridge.py:2923-2954
reason: Both the stale-prefix branch and the cross-meeting mismatch branch `return` after popping the pending entry, emitting nothing to the originator — unlike the `channel_disconnected` path, which does reply. Reachable whenever a conference ends between question and answer.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-bridge-copilot-reply-guards

Evidence: `ktalk_bridge.py:2923-2930` (stale-prefix branch) and `:2940-2954` (cross-meeting mismatch branch) both `return` after popping the pending entry, emitting nothing to the originator — unlike the `channel_disconnected` path, which does reply. The extension panel has always had this hang; the webapp chat panel (story 6) inherits it, and the case is reachable whenever a conference ends between question and answer. Pre-existing: this story does not change either drop branch.

### DW-9: `./recog-service.sh restart` also restarts the supervisor, so a live manual session loses its only owner and is reaped as an orphan ~90 s later — the opposite of the restart-resilience the plan claims for that command.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/3-supervisor-manual-lifecycle-autospawn-gate-narrowed.md"), 2026-07-25
location: recog-service.sh:244 (`cmd_restart`) + recog/copilot_supervisor.py (`_reconcile`)
reason: After a full restart both owners of the manual session are blank (supervisor lifecycle IDLE reports `current_meeting_id: None`, bridge `_manual_copilot_active` False), so reconcile takes the orphan branch → GRACE → Kill. Closing it needs a marker the supervisor can read back off the tmux session — a form of persistence the story's intent forbids; recoverable by pressing Start within the grace window. Belongs with plan Task 7's edge-case checklist (story 7).
status: done 2026-07-25
resolution: resolved by sweep bundle dw-manual-session-tmux-probe-readoption
decision: 2026-07-25 Build tmux-probe re-adoption — At supervisor startup, probe `tmux -L recog-copilot has-session` for the manual session name before reconciling; if it is alive and no conference meeting is active, adopt it as `"manual"` so `supervisor_hello` carries `current_meeting_id: "manual"` and the bridge re-arms `_manual_copilot_active`. The live tmux session itself is the marker — no persistence file, honoring the original no-persistence intent. Update CLAUDE.md and the plan's D5 notes to drop the deferral.

Evidence: `recog-service.sh:244` `cmd_restart` calls `stop_all_services`, which includes `SUPERVISOR_PLIST_NAME`, while `kill_copilot_tmux` is deliberately skipped so the tmux session survives. After the restart both owners are blank: the supervisor's lifecycle is IDLE so `supervisor_hello` carries `current_meeting_id: None`, and the bridge's `_manual_copilot_active` is False, so `_compute_meeting_state` answers `active=false`. `_reconcile` then takes the orphan branch (`adopt(None)` → `meeting_ended`) → GRACE → `timer_expired` → Kill. This is the pre-existing, deliberately-tested orphan contract (`tests/test_copilot_supervisor.py::test_reconcile_orphan_session_grace_then_kill`), and a manual session after a full restart is indistinguishable from an orphan. Plan D5 asserts the opposite ("covers both restart permutations … via `./recog-service.sh restart`"), as does the Post-Completion smoke.

### DW-10: A grace-expiry `Kill` can destroy a session spawned in the same tick, because `_grace_expire` computes its commands before awaiting `_execute`.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/3-supervisor-manual-lifecycle-autospawn-gate-narrowed.md"), 2026-07-25
location: recog/copilot_supervisor.py (`_grace_expire`)
reason: `_grace_expire` runs `commands = self.lifecycle.timer_expired()` (state → IDLE, `Kill` queued) and only then awaits `_execute(commands)`; a frame processed at that await boundary spawns/adopts a session that the queued `Kill` then destroys. A re-check of `lifecycle.state is State.IDLE` before executing the kill would close it.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-supervisor-kill-path-hardening

Evidence: A frame processed at the await boundary takes the IDLE branch — `meeting_started` → `Spawn` → `TmuxRunner.spawn` adopts the still-live session — after which the queued `Kill` destroys it, leaving `SPAWNING` with no session until the watchdog's `session_died` and a 5 s backoff. Pre-existing in the conference path; story 3 makes it reachable from a user-facing Start button (click landing in the exact millisecond the 90 s timer fires).

### DW-11: The `supervisor_hello.current_meeting_id` field *name* is asserted independently on both sides of the wire, so a one-sided rename passes the whole suite while silently breaking manual-session recovery.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/3-supervisor-manual-lifecycle-autospawn-gate-narrowed.md"), 2026-07-25
location: tests/test_copilot_supervisor.py + tests/test_ktalk_bridge.py
reason: The supervisor test pins the key in a hand-built expected dict and the bridge test feeds a hand-built frame using the same literal; no test wires the supervisor's produced frame into the bridge's `_process_message`, so the CAP-3 re-arm contract has no cross-module coverage.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-contract-regression-tests

Evidence: `tests/test_copilot_supervisor.py::test_supervisor_sends_hello_on_connect` pins the key in a hand-built expected dict; `tests/test_ktalk_bridge.py::test_supervisor_hello_rearms_manual_flag_before_reply` feeds the bridge a hand-built frame using the same literal. `test_manual_meeting_id_matches_bridge_constant` pins the *value* `"manual"` across modules but not the key.

### DW-1: Follow-up review still recommended for 2 after the damping cap was spent
origin: review-budget-followup
source_spec: `2-bridge-relays-manual-spawn-stop-and-tracks-manual-session-state.md`
severity: low
reason: The follow-up-review damping cap (limits.max_followup_reviews = 1) was spent with the story finalized (status: done, verify green) while the review pass still recommended an independent follow-up. The work was committed by bmad-loop run 20260724-203203-7b5e; this entry preserves the lingering recommendation for a deliberate later review.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-deferred-followup-reviews

### DW-12: A failed tmux kill is treated as success, so Stop can leave a live Copilot session orphaned after the lifecycle has moved to IDLE.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/3-supervisor-manual-lifecycle-autospawn-gate-narrowed.md"), 2026-07-25
location: recog/copilot_supervisor.py (`TmuxRunner.kill`)
reason: `TmuxRunner.kill()` awaits `tmux kill-session` but discards its return code, while `_grace_expire()` moves the machine to IDLE before executing `Kill`; `Watchdog.tick()` deliberately does not probe tmux in IDLE, so a non-zero kill result leaves the session running with no retry or visible failure.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-supervisor-kill-path-hardening

Evidence: Both behaviors are present at baseline `84c1177`; story 3 only makes the path user-triggerable through manual Stop.

### DW-13: Localhost WebSocket endpoints (frontend 9080 live-stream, bridge 8765/8766) accept connections from any browser page with no Origin check, so a malicious webpage could inject copilot frames (spawn/stop/chat) into the tool-bearing Claude session.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/5-frontend-server-inbound-copilot-frames-over-api-live-stream.md"), 2026-07-25
location: ktalk_bridge.py (`_handle_client`) + recog/web/server.py (`live_stream`)
reason: WebSockets bypass CORS; any page can open ws://127.0.0.1:8766 and speak the bridge protocol (subscriber_hello + copilot_chat_message) — the surface predates story 5, which only added a same-class relay at 9080. A defense would be an Origin allowlist at accept-time in `ktalk_bridge._handle_client` and `recog/web/server.live_stream`.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-ws-origin-allowlist

### DW-2: Follow-up review still recommended for 5 after the damping cap was spent
origin: review-budget-followup
source_spec: `5-frontend-server-inbound-copilot-frames-over-api-live-stream.md`
severity: low
reason: The follow-up-review damping cap (limits.max_followup_reviews = 1) was spent with the story finalized (status: done, verify green) while the review pass still recommended an independent follow-up. The work was committed by bmad-loop run 20260724-203203-7b5e; this entry preserves the lingering recommendation for a deliberate later review.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-deferred-followup-reviews

### DW-3: Follow-up review still recommended for 6 after the damping cap was spent
origin: review-budget-followup
source_spec: `6-webapp-ui-copilot-button-status-chip-chat-panel.md`
severity: low
reason: The follow-up-review damping cap (limits.max_followup_reviews = 1) was spent with the story finalized (status: done, verify green) while the review pass still recommended an independent follow-up. The work was committed by bmad-loop run 20260724-203203-7b5e; this entry preserves the lingering recommendation for a deliberate later review.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-deferred-followup-reviews

### DW-14: The "F5 of the tab does not kill the session" edge case has no pinning regression test — it holds only by absence of code.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/7-acceptance-verification-and-documentation.md"), 2026-07-25
location: tests/test_frontend_copilot_proxy.py (recog/web/server.py:1529-1533)
reason: No stop frame is sent from the /api/live/stream disconnect path and app.js has no beforeunload/pagehide sender, but no test asserts a disconnected live-stream client emits nothing to the bridge; a future cleanup handler in that path would make every tab reload GRACE-kill the shared manual session with the whole suite green. Test authoring was outside story 7's docs-only intent.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-contract-regression-tests

Evidence: recog/web/server.py:1529-1533 ends the receive loop with WebSocketDisconnect → pass/unregister and recog/web/static/app.js has no beforeunload/pagehide sender; no test in tests/test_frontend_copilot_proxy.py asserts a disconnected live-stream client emits nothing to the bridge (pattern would be: connect fake client, disconnect, assert FakeSubscriber.sent == []).

### DW-15: A manual chat reply that remains pending when a conference preempts the manual session can be rendered in the new conference chat.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/7-acceptance-verification-and-documentation.md"), 2026-07-25
location: ktalk_bridge.py:2849-2860, 2413-2433, 1275-1310
reason: Manual requests are stored with an empty meeting id; the conference `meeting_join` transition marks only requests matching the prior meeting id stale, and `_preempt_manual_copilot` does not invalidate pending request ids — a late reply therefore bypasses both stale guards and is broadcast after the conference starts.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-bridge-copilot-reply-guards

Evidence: Manual requests are stored with an empty meeting id at `ktalk_bridge.py:2849-2860,2915-2920`; the conference `meeting_join` transition marks only requests matching the prior meeting id stale at `ktalk_bridge.py:2413-2433`, and `_preempt_manual_copilot` at `ktalk_bridge.py:1275-1310` does not invalidate pending request ids. Pre-existing in the story 2/3 implementation; story 7 only surfaced it while verifying conference preemption.

### DW-16: The originator-only no-channel `channel_disconnected` contract has no negative-recipient regression assertion.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/7-acceptance-verification-and-documentation.md"), 2026-07-25
location: tests/test_ktalk_bridge.py:1343-1371
reason: The existing test proves the originator receives the error but registers no second subscriber and never asserts that unrelated clients receive nothing, so a regression that accidentally widens the per-request echo can keep the suite green.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-contract-regression-tests

Evidence: The separate last-channel-client-disconnect tests cover the intentionally broadcast variant.

### DW-17: The manual-session contract that proactive transcript ticks remain disabled has no lifecycle regression test.

origin: migrated from legacy ledger ("code review of spec-webapp-copilot-live/stories/7-acceptance-verification-and-documentation.md"), 2026-07-25
location: tests/test_ktalk_bridge.py:2268-2416 (ktalk_bridge.py:3119-3176, 2469-2493)
reason: Manual spawn and `supervisor_hello` re-arm only update `_manual_copilot_active` while `_copilot_tick_state` is seeded only on conference join/reconnect paths, but no test drives either manual lifecycle path and asserts that `_copilot_tick_state` stays `None` and no `transcript_tick` reaches channel clients.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-contract-regression-tests

Evidence: Existing tick tests in `tests/test_ktalk_bridge.py:2268-2416` cover active conferences and no-meeting suppression only.

### DW-4: Follow-up review still recommended for 7 after the damping cap was spent
origin: review-budget-followup
source_spec: `7-acceptance-verification-and-documentation.md`
severity: low
reason: The follow-up-review damping cap (limits.max_followup_reviews = 1) was spent with the story finalized (status: done, verify green) while the review pass still recommended an independent follow-up. The work was committed by bmad-loop run 20260724-203203-7b5e; this entry preserves the lingering recommendation for a deliberate later review.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-deferred-followup-reviews

- source_spec: `_bmad-output/implementation-artifacts/spec-bridge-copilot-reply-guards.md`
  summary: The terminal drop echoes are broadcast to all extension and subscriber clients, so a non-originating UI renders an unmatched per-request error bubble (e.g. the manual-preemption echo appears in the just-started conference's extension panel); scoping to the originator needs an intent-level decision plus originator bookkeeping in `_copilot_pending_requests`.
  evidence: Both UIs append a bubble for an unmatched per-request `error` frame (recog/web/static/app.js:1476-1478, extension src/ui.js:1922-1928); `_broadcast_copilot_frame` fans out to both audiences; CLAUDE.md's "per-request rejection echoes remain originator-only" describes the opposite policy for the adjacent spawn/stop frame category. Matches pre-existing behavior of broadcast channel-server error statuses, so it widens an existing pattern rather than inventing it.

- source_spec: `_bmad-output/implementation-artifacts/spec-bridge-copilot-reply-guards.md`
  summary: The DW-8 terminal echo fires only when a late `copilot_channel_reply` actually arrives; a manual session killed on preemption mid-generation typically never replies, so the asking UI's placeholder still hangs — emitting the terminal `copilot_status` error per stale-marked request directly in `_preempt_manual_copilot` (or a TTL sweep) would close the common case.
  evidence: The echo lives exclusively in the reply handler's drop branches (ktalk_bridge.py:~2985-3005); the channel daemon stays connected across session kills so the `channel_disconnected` unbrick path never fires; webapp `manual_preempted` handling appends a notice but does not resolve `copilotState.pending`.

- source_spec: `_bmad-output/implementation-artifacts/spec-bridge-copilot-reply-guards.md`
  summary: The stale branch of `copilot_channel_status` still swallows terminal `error`/`done` statuses for a stale-marked request silently — no terminal echo and no pop — although a status-only terminal frame is a documented real closing signal; the "tracked drop must terminally resolve the asking UI" rule adopted for the reply branches was not adopted there.
  evidence: ktalk_bridge.py stale-status branch (~3092-3097) returns without echo or pop; the handler's own comment (~3107-3111) documents that terminal statuses may arrive with no reply following; the only stale-status test uses non-terminal "working".

- source_spec: `_bmad-output/implementation-artifacts/spec-bridge-copilot-reply-guards.md`
  summary: `stale:`-prefixed entries in `_copilot_pending_requests` are garbage-collected only by an arriving reply; the channel-disconnect cleanup deliberately skips them and the stale status branch never pops, so preemption-marked entries whose session died before replying leak for the bridge process lifetime.
  evidence: Cleanup at ktalk_bridge.py:~2044-2046 `continue`s past `stale:` values; the stale status branch does not pop; `_preempt_manual_copilot` now produces such entries on every preemption with an in-flight manual chat.

- source_spec: `_bmad-output/implementation-artifacts/spec-bridge-copilot-reply-guards.md`
  summary: Manual-chat pending entries (value `""`) are invalidated only on the preemption path; the other manual-session death paths — Stop→grace→kill, crash/session_died, respawn exhaustion, and the AUTOSPAWN=0/desynced-mirror case where `_manual_copilot_active` is already false — leave `""` entries live, so a later reply with `meeting_id: None` still bypasses both guards and relays.
  evidence: `_preempt_manual_copilot` early-returns when the mirror is disarmed (pinned by `test_preempt_manual_copilot_noop_leaves_pending_untouched`); no invalidation exists on the supervisor kill/crash paths; the cross-meeting guard requires a truthy reply `meeting_id`, which manual replies do not carry.

- source_spec: `_bmad-output/implementation-artifacts/spec-bridge-copilot-reply-guards.md`
  summary: With multiple Copilot channel clients, the first terminal status or stale reply can consume the pending entry while another client still owns the same request, so a later duplicate reply becomes untracked and can relay into a new conference.
  evidence: `copilot_chat_message` fans one request out to every `_copilot_channel_clients` socket, while terminal `copilot_channel_status` and the stale-reply branch each remove the single `_copilot_pending_requests[request_id]` entry; a second client's `meeting_id: None` reply then bypasses both stale and cross-meeting guards.

### DW-18: Follow-up review still recommended for dw-bridge-fanout-send-timeout after the damping cap was spent
origin: review-budget-followup
source_spec: `spec-dw-6-bridge-fanout-send-timeout.md`
severity: low
reason: The follow-up-review damping cap (limits.max_followup_reviews = 1) was spent with the story finalized (status: done, verify green) while the review pass still recommended an independent follow-up. The work was committed by bmad-loop run 20260725-030531-720e; this entry preserves the lingering recommendation for a deliberate later review.
status: done 2026-07-25
resolution: resolved by sweep bundle dw-copilot-deferred-followup-reviews

- source_spec: `_bmad-output/implementation-artifacts/spec-dw-7-atomic-meta-json-write.md`
  summary: `recog/live.py` still rewrites an outgoing meeting's `meta.json` with truncate-in-place `write_text` during WAV-writer rotation, so concurrent readers can observe partial JSON despite the atomic `MeetingStorage._save_json` path.
  evidence: `_rotate_wav_writers` reads the old meeting metadata and calls `meta_path.write_text(json.dumps(...))` directly at `recog/live.py:2202-2219`; this pre-existing path bypasses `MeetingStorage._save_json` and can overlap frontend reads of the completed meeting.

- source_spec: `_bmad-output/implementation-artifacts/spec-dw-7-atomic-meta-json-write.md`
  summary: Atomic file replacement prevents torn JSON but does not serialize concurrent metadata read-modify-write operations, so a participant/title update can overwrite segment or refine statistics written from another thread.
  evidence: `update_participants` and `update_title` perform `_load_meta` followed by `_save_meta` outside `_segment_lock`, while `add_segment` and `update_segments` independently read and rewrite the same `meta.json`; each published file is valid, but the later stale snapshot can drop fields changed by the earlier writer.

- source_spec: `_bmad-output/implementation-artifacts/spec-supervisor-kill-path-hardening.md`
  summary: `_run_subprocess` has no timeout, so a wedged tmux invocation blocks the bridge frame loop (pre-existing) and now also parks grace-expiry and respawn tasks on `_execute_lock` until the daemon restarts; wrapping runner calls in `asyncio.wait_for` (or a timeout inside `_run_subprocess`) would bound it.
  evidence: `recog/copilot_supervisor.py` `_run_subprocess` awaits `proc.communicate()` unbounded; `handle_frame` → `_apply` → `_execute` is awaited inline in the WS receive loop, and the new `_execute_lock` serializes `_grace_expire`/`_respawn_after` behind any in-flight runner call. Pre-existing hang shape; the lock widens the blast radius.

- source_spec: `_bmad-output/implementation-artifacts/spec-supervisor-kill-path-hardening.md`
  summary: After `TmuxRunner.kill()` exhausts its retries the machine still ends IDLE with a live orphan session — the RuntimeError is logged to stderr only, no `copilot_agent_status` escalation, no re-adoption, and the watchdog's IDLE branch never probes tmux, so the orphan stays invisible to the UI chips; closing it needs a lifecycle-level decision (re-adopt / keep probing / escalate status) that the DW-12 intent did not order.
  evidence: `_grace_expire` runs `timer_expired()` (state → IDLE) before the Kill executes; `_execute_locked`'s per-command `except` only prints `command Kill() failed`; `Watchdog.tick` returns early in IDLE without probing. Baseline behavior orphaned silently on any kill failure — the hardening narrows the window to 3 verified-failed retries and makes it loud, but the end state is unchanged.

- source_spec: `_bmad-output/implementation-artifacts/spec-supervisor-kill-path-hardening.md`
  summary: In a `(Kill, Spawn)` meeting-switch sequence a kill that raises after exhausted retries is logged and execution proceeds to `spawn`, whose `alive()` gate then adopts the un-killed old session under the new meeting id — the new meeting silently inherits the old session's conversation context; no test covers a failing kill followed by a spawn.
  evidence: `_execute_locked` catches per command and continues the loop; `TmuxRunner.spawn` returns False (adopt) when `alive()` is True. Identical outcome existed at baseline (failed kill was silent, spawn adopted anyway); this story only made the kill failure visible. A fix needs a sequencing decision (skip/flag the Spawn after a failed Kill).

- source_spec: `_bmad-output/implementation-artifacts/spec-supervisor-kill-path-hardening.md`
  summary: `_respawn_after` validates staleness (`state is SPAWNING` + same `meeting_id`) before parking on `_execute_lock` and never re-checks after acquisition — the TOCTOU twin of DW-10: a state change while parked lets a stale respawn spawn a session for a dead meeting, which the next queued Kill+Spawn then destroys (self-correcting churn, not data loss).
  evidence: `recog/copilot_supervisor.py` `_respawn_after` runs its guard, then `await self._execute(commands)` which may wait on the lock; the DW-10 fix moved the equivalent check inside the lock only for `_grace_expire`. Pre-existing check-then-act shape; the lock adds a parking window.

- source_spec: `_bmad-output/implementation-artifacts/spec-supervisor-kill-path-hardening.md`
  summary: `Supervisor.stop()` cannot cancel or await a grace-expiry kill after `_grace_expire` clears `_grace_task`, so shutdown may return while the kill task is still running.
  evidence: `_grace_expire` sets `self._grace_task = None` before acquiring `_execute_lock` and executing `runner.kill()`; once that point is reached, `stop()` has no task reference to cancel, and a blocking runner leaves the captured grace task pending after `await stop()`.

- source_spec: `_bmad-output/implementation-artifacts/spec-ws-origin-allowlist.md`
  summary: The sibling WS endpoint `/api/meetings/{meeting_id}/watch` (`recog/web/server.py` `watch_meeting`) still accepts any browser Origin — a malicious page can stream stored/live meeting transcript updates cross-origin, the same CORS-bypass class DW-13 closed on `/api/live/stream` and the bridge; the DW-13 intent named only `_handle_client` and `live_stream`, so this read-only surface stayed out of scope.
  evidence: `watch_meeting` calls `websocket.accept()` with no `origin_allowed()` check while `live_stream` fifteen lines below now guards; no test in the repo connects to the `/watch` route at all (every `websocket_connect` targets `/api/live/stream`). Fix is mechanical: the same pre-accept guard plus a mirror of `test_disallowed_origin_denied_and_never_registered` asserting `transcript_watcher` never registers the socket.

- source_spec: `_bmad-output/implementation-artifacts/spec-dw-9-manual-session-tmux-probe-readoption.md`
  summary: `recog-service.sh stop` and `uninstall` can race the asynchronously terminating copilot supervisor, allowing an in-flight or newly scheduled spawn to recreate a tmux session after terminal cleanup reports success.
  evidence: `stop_all_services` issues `launchctl unload` for the supervisor and immediately calls `kill_copilot_tmux`; the existing `restart_one` comment documents that unload returns before launchd reaps the process, while the baseline at `709982e` already used the same unload-then-kill ordering, so this shutdown race predates DW-9.

- source_spec: `_bmad-output/implementation-artifacts/spec-copilot-contract-regression-tests.md`
  summary: The browser half of the DW-14 "F5 does not kill the manual session" invariant is unpinned anywhere in the repo — nothing asserts `recog/web/static/app.js` registers no unload-time (`beforeunload`/`pagehide`) `copilot_stop_request` sender, so a future "clean up on close" edit in app.js would GRACE-kill the shared manual session on every tab reload while the whole suite stays green, because the stop frame would arrive as ordinary pre-disconnect traffic the server-side DW-14 test cannot distinguish.
  evidence: The repo has no JS test harness (no test references app.js; every `websocket_connect` test targets the Python server), and the new `test_live_stream_disconnect_emits_nothing_to_bridge` pins only the server disconnect path (`recog/web/server.py` `except WebSocketDisconnect: pass` + unregister). `copilot_stop_request` is currently sent only from the explicit Stop action in app.js. A cheap repo-style pin would be a grep-based Python test asserting app.js contains no beforeunload/pagehide handler emitting `copilot_stop_request`.

- source_spec: `_bmad-output/implementation-artifacts/spec-copilot-deferred-followup-reviews.md`
  summary: The `_agent_state_primer` cache introduced by f6f4ad7 is never invalidated when the bridge link drops, so during a bridge outage every newly connecting webapp client is primed with the last pre-outage chip frame (e.g. 🟢 online) while the copilot stack is actually unreachable — the exact stale-chip state the primer exists to prevent.
  evidence: At HEAD 2b5e089, `recog/web/server.py:473` (`_agent_state_primer: Optional[str] = None`) has exactly one setter — `_cache_agent_state_frame` at `recog/web/server.py:476-486` — and no clearing path anywhere (all references to the name are lines 472-507, 1531, 1540); it is replayed unconditionally to each new client at `recog/web/server.py:1531` and `:1540`. `recog/web/bridge_subscriber.py:134-137` clears `self._ws = None` on disconnect but provides no callback the server uses to mark the cache stale. Recovery happens only after the bridge returns (its connect-time primer refreshes the cache on reconnect). Not a duplicate: no DW entry or `source_spec:` bullet touches the frontend primer cache — the surface was created by f6f4ad7 itself.

- source_spec: `_bmad-output/implementation-artifacts/spec-copilot-deferred-followup-reviews.md`
  summary: The webapp→bridge relay wired by f6f4ad7 awaits `BridgeSubscriber.send` → bare `ws.send(raw)` with no per-send timeout from inside the sending tab's live-stream receive loop, so a flow-controlled/stalled bridge socket stalls inbound-frame processing for up to ~40 s (unbounded only if the peer keeps answering keepalive pings while applying flow-control backpressure, or if keepalive is disabled) for every tab that awaits `subscriber.send()` during the stall window, and during that window no `bridge_disconnected` failure echo is produced.
  evidence: At HEAD 2b5e089, `recog/web/bridge_subscriber.py:166-171` (`ws = self._ws … await ws.send(raw)` — no per-send bound); called from `recog/web/server.py:546` (`sent = await subscriber.send(raw)`), which is awaited inline in the `/api/live/stream` receive loop at `recog/web/server.py:1547-1549`. The stall is bounded by default keepalive, not by the relay: `recog/web/bridge_subscriber.py:107` calls `websockets.connect(self.bridge_url)` with no keepalive kwargs, so the library defaults ping_interval=20/ping_timeout=20 close a stalled connection in ~40 s, the pending `ws.send` raises, `send()`'s except path returns False, and the `bridge_disconnected` echo then fires — but every frame relayed during the window gets neither delivery nor an echo until then. Blast radius: each tab's `/api/live/stream` receive loop is an independent coroutine, so only tabs that themselves await `subscriber.send()` during the stall window are blocked; tabs that send nothing are unaffected. Not a duplicate: DW-6 (done) covered the bridge-side fan-out and its fix e57141d touched only `recog/ktalk_bridge.py` and its tests — the frontend upstream socket was outside that sweep's scope, mirroring how the ledger records same-mechanism-different-site timeout gaps separately (e.g. the supervisor `_run_subprocess` no-timeout bullet under spec-supervisor-kill-path-hardening).

- source_spec: `_bmad-output/implementation-artifacts/spec-copilot-deferred-followup-reviews.md`
  summary: c83aa04's Stop-button gating derives "a conference owns the session" from the transcript view's stale `currentMeeting.source` (patched in place one-way for ktalk only), so an in-place orphan→vas3k/vkkovalev/matrix promotion leaves Stop wrongly ENABLED during the conference — a click relays an unguarded stop the supervisor silently ignores, wedging the button at "останавливается…" for the conference's lifetime.
  evidence: At HEAD 2b5e089, `recog/web/static/app.js:1143-1152` — `const meetingSource = views.live.currentMeeting && views.live.currentMeeting.source; const conferenceOwned = COPILOT_CONFERENCE_SOURCES.has(meetingSource); … stopBtn.disabled = stopping || copilotState.agentState === 'offline' || conferenceOwned;`. The only in-place source sync is the ktalk-only patch at `recog/web/static/app.js:732-735` (`if (this.currentMeeting && this.refineEnabled) { this.currentMeeting.source = 'ktalk'; … }`) — the `refine_state` frame it reacts to carries no source, only `refine_enabled: source == "ktalk"` (`recog/web/live_watcher.py:190-194`), so a case-3 in-place promotion to a non-ktalk conference platform (`ktalk_bridge.py:2547-2552` promotion comment; `on_meeting_join(url, title, platform)` with platform vas3k/matrix) leaves `source: 'orphan'` and Stop enabled. The relayed stop has no conference guard by design (`ktalk_bridge.py:3421-3423` — a stray stop is inert at the supervisor) and produces no per-request echo when delivered, and `stopPendingId` clears only when the chip reaches offline (`recog/web/static/app.js:1318-1320`), so the click wedges the button for the conference's lifetime. Not a duplicate: DW-7 concerned bridge-side `_is_meeting_active` torn reads; the spec-bridge-copilot-reply-guards bullets concern chat-reply staleness; nothing in the ledger touches webapp Stop gating or the stale view-model source.

- source_spec: `_bmad-output/implementation-artifacts/spec-copilot-deferred-followup-reviews.md`
  summary: c83aa04's same stale-`currentMeeting.source` gating fails in the opposite direction after a conference ends with no successor meeting: nothing ever nulls `currentMeeting`, so `conferenceOwned` stays true and Stop is wrongly DISABLED for a freshly spawned manual session until reload or view switch.
  evidence: At HEAD 2b5e089, the gate is the same `conferenceOwned` computation at `recog/web/static/app.js:1143-1152` over the never-cleared `views.live.currentMeeting.source`, with the shared root cause that the only in-place source sync is the one-way ktalk-only patch at `recog/web/static/app.js:732-735`. The staleness is real because no frame ever nulls `currentMeeting` on meeting end — the server sends `"meeting": None` only in a fresh connection's `init` (`recog/web/server.py:1533-1539`), and `live_watcher._check_current_meeting` broadcasts `meeting_change` only when a *new* active meeting appears (`recog/web/live_watcher.py:144-158`). Not a duplicate: same dedup rationale as the enabled-direction bullet — no ledger entry touches webapp Stop gating or the stale view-model source; split from that bullet because the failure direction, repro path, and plausible fix differ (clearing/refreshing `currentMeeting` on meeting end vs syncing source on in-place promotion).

- source_spec: `_bmad-output/implementation-artifacts/spec-copilot-deferred-followup-reviews.md`
  summary: e57141d's `copilot_chat_message` forward discards `_send_bounded`'s delivered result, so when every Copilot channel socket times out (or is already fail-fast-marked in `_timed_out_clients`) the request is silently undelivered while its pending entry stays in `_copilot_pending_requests` — the asking UI gets no echo for the bounded window until the stalled socket's reap fires the 1→0 channel-client transition (which pops pending entries and broadcasts a terminal `channel_disconnected`), and hangs permanently in the narrower case where another channel client connects before the reap so the count never reaches 0 — unlike the empty-channel branch, which replies `channel_disconnected` up front, and unlike `_relay_to_supervisors`, which uses the same boolean to drive a `supervisor_disconnected` echo.
  evidence: At HEAD 2b5e089, `ktalk_bridge.py:3097-3109` — `forward()` does `await self._send_bounded(client, outbound)` (`:3099`) with the return value ignored, then `asyncio.gather(...)` with no delivered tracking; `ktalk_bridge.py:3095` `self._copilot_pending_requests[request_id] = meeting_id or ""` is set just before and never rolled back; contrast `ktalk_bridge.py:1335` `if await self._send_bounded(client, message): delivered = True` and the zero-clients `channel_disconnected` echo at `:3052-3068`. The gap is normally bounded: `_send_bounded`'s timeout schedules `close()`, and the handler reap's 1→0 channel-client transition at `ktalk_bridge.py:2205-2221` pops all non-stale pending entries and broadcasts a terminal `channel_disconnected` `copilot_status` — but (a) between forward-failure and that reap the UI shows nothing actionable, and (b) if another channel client connects before the stalled one is reaped, the count never reaches 0, the pop never fires, and the undelivered request's pending entry stays live indefinitely. Not a duplicate: DW-8 (resolved) and the spec-bridge-copilot-reply-guards bullets all concern reply-side drops or pending-entry invalidation on session-death paths — none covers forward-time delivery failure; the pre-e57141d code had no delivered signal to check (a stalled socket hung the handler instead), so discarding the new signal is introduced by this diff.

- source_spec: `_bmad-output/implementation-artifacts/spec-copilot-deferred-followup-reviews.md`
  summary: e57141d leaves the three thread-side fan-out paths (`send_transcript`, `_maybe_emit_transcript_tick`, `send_refine`) outside the bounded-send machinery: they dispatch `client.send` via `run_coroutine_threadsafe` without consulting `_timed_out_clients` or any timeout, so a client already detected as stalled keeps accumulating one pending never-completing send task (each retaining the serialized frame) per live transcript segment/tick/refine until the handler reaps the socket — unbounded task growth on the highest-volume path, and no stall detection at all when a slow consumer receives only these frames.
  evidence: At HEAD 2b5e089, `ktalk_bridge.py:878-881`, `:969`, and `:1056-1059` all call `asyncio.run_coroutine_threadsafe(client.send(message), self._loop)` directly with `except Exception: pass`, no `BROADCAST_SEND_TIMEOUT_S` bound and no `_timed_out_clients` check, while every asyncio-side fan-out (`:1198, :1233, :1257, :1290, :1335, :1440, :3099`) goes through `_send_bounded` (`:1119`, fail-fast check at `:1135`). Test coverage: the DW-6 test block drives `_broadcast_message` / `_broadcast_copilot_frame` / `_broadcast_to_channel_clients` / `_emit_lifecycle` / `_relay_to_supervisors` / `_preempt_manual_copilot`, and `test_stalled_channel_client_does_not_block_chat_forward` (`tests/test_ktalk_bridge.py:4707-4737`) additionally drives the `copilot_chat_message` forward path with one stalled + one healthy channel client — but that test asserts only healthy-client delivery and bounded elapsed time (not `_copilot_pending_requests` rollback or an originator echo in the all-stalled case), and no stalled-client test anywhere in the file touches `send_transcript` / `_maybe_emit_transcript_tick` / `send_refine`. Not a duplicate: DW-6's mechanism was sequential-await head-of-line blocking in the asyncio fan-outs, and its evidence explicitly treated `send_refine`'s threadsafe dispatch as the non-blocking alternative — the fail-fast-bypass/task-accumulation residue is recorded nowhere in DW-5..DW-17 or the post-bundle bullets.
