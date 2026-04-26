# KTalk Copilot Channel Scaffold

## Overview
- Scaffold the local Claude Code Copilot channel server (MCP Streamable HTTP) and the bridge-side protocol that routes user chat messages to it. Backend tracer bullet only — no extension UI, no transcript snapshot tool, no proactive ticks. Implements the routing skeleton from `openspec/changes/ktalk-claude-copilot-chat/` design D4 with the Streamable-HTTP transport.

## Context (from discovery)
- Spec just edited in this session: `openspec/changes/ktalk-claude-copilot-chat/{proposal,design,tasks}.md` (transport switched from stdio to MCP Streamable HTTP per D4).
- Bridge dispatcher: `ktalk_bridge.py:1339` `_process_message` — long `if/elif` on `msg_type`. Model handlers to copy: `_handle_summarize` (line 862), `_handle_anchor` (line 1068).
- Existing client tracking: `self._connected_clients` (line 287, extension/general clients), `self._subscriber_clients` (line 294, read-only). `subscriber_hello` registration handler at line 1252 is the pattern to mirror for `copilot_channel_hello`.
- Cleanup happens in `_handle_client` disconnect path — channel clients must also be removed there.
- Test model: `tests/test_ktalk_bridge.py` (~469 lines). Convention: `pytest-asyncio`, instantiate `KTalkBridge()` directly, call `await bridge._process_message({...})`, assert via `unittest.mock.Mock`/`AsyncMock`. Fixture `_attach_meeting_storage(bridge)` at line 207.
- New Python module location: `recog/copilot_channel.py` (no similar file exists). `recog/` already contains `asr.py`, `live.py`, `per_speaker.py`, `refine.py`, `ai_provider.py`, `speaker_db.py`, `increment.py`, `prompt_registry.py`, `node_env.py`.
- Dependency gap: neither root `requirements.txt` nor `frontend/requirements.txt` includes `mcp`. Must be added for the Streamable-HTTP server.
- Bridge is currently WS-only (`websockets` library). No HTTP server in-process — the channel server is its own process.

## Development Approach
- **Testing approach**: Regular — add unit tests for each new bridge dispatch branch using the `test_ktalk_bridge.py` model.
- Backend only in this plan. Extension `Copilot` tab UI is a separate follow-up plan.
- No proactive `transcript_tick`, no `get_recent_transcript` tool — those are tasks 5 and 6 of the openspec change and stay out of scope here.
- Complete each task fully before moving to the next.

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Add `mcp` Python SDK dependency
- [x] Add a line `mcp>=1.2.0` to `requirements.txt` near the other Python deps (alphabetical or end of list — match existing style)
- [x] Install: `./venv-gigaam/bin/pip install -r requirements.txt`
- [x] Verify import: `./venv-gigaam/bin/python -c "from mcp.server.fastmcp import FastMCP; print('mcp ok')"`

### Task 2: Bridge — register Copilot channel clients on `copilot_channel_hello`
- [x] In `ktalk_bridge.py` `KTalkBridge.__init__`, add `self._copilot_channel_clients: set = set()` next to `_subscriber_clients` (around line 294)
- [x] In `_process_message` (line 1339+), add a new `elif msg_type == "copilot_channel_hello":` branch that adds `websocket` to `self._copilot_channel_clients` and logs at INFO level (model on subscriber_hello at line 1252)
- [x] In the disconnect / `finally` block of `_handle_client` (search for `_subscriber_clients.discard` or equivalent cleanup), also `self._copilot_channel_clients.discard(websocket)`
- [x] Add a unit test `test_copilot_channel_hello_registers_client` in `tests/test_ktalk_bridge.py` modelled on the closest existing subscriber test: send the frame via `_process_message`, assert `bridge._copilot_channel_clients` contains the mock websocket

### Task 3: Bridge — route `copilot_chat_message` to channel clients
- [x] Add a new `elif msg_type == "copilot_chat_message":` branch in `_process_message`. Validate required fields: `request_id` (non-empty str), `text` (non-empty str). On invalid, log a warning and return without raising.
- [x] Build a `copilot_user_message` outbound frame with: `type`, `request_id`, `text`, `meeting_id` (use `self._current_meeting_id` if such field exists, else best-effort from current state), `meeting_title`, `transcript_window` (pass through if present, else `None`), `created_at` (echo or `time.time() * 1000`)
- [x] Send the frame to every websocket in `self._copilot_channel_clients` (use the same `await ws.send(json.dumps(...))` pattern as elsewhere in the bridge; iterate over a copy of the set to avoid mutation-during-iteration)
- [x] If `self._copilot_channel_clients` is empty when a `copilot_chat_message` arrives, send a `copilot_status` frame back to the originating `websocket` with `status="channel_disconnected"` and a short `message`
- [x] Add unit tests:
  - `test_copilot_chat_message_forwarded_to_channel_client` — register a mock channel client, send chat_message, assert the channel client received the `copilot_user_message` frame with correct fields
  - `test_copilot_chat_message_without_channel_returns_disconnected_status` — no channel client, assert originating client receives `copilot_status` with `status="channel_disconnected"`

### Task 4: Bridge — relay `copilot_channel_reply` and `copilot_channel_status` to extension clients
- [x] Add `elif msg_type == "copilot_channel_reply":` branch. Re-emit as a `copilot_reply` frame (rename `type`) and broadcast to `self._connected_clients` (extension clients), preserving `request_id`, `kind`, `text`, `meeting_id`, `created_at`.
- [x] Add `elif msg_type == "copilot_channel_status":` branch. Re-emit as `copilot_status` and broadcast to `self._connected_clients` with `request_id`, `status`, `message`.
- [x] Use whatever broadcast helper already exists (look near where `transcript` frames are broadcast) — do not invent a new one if one is in place; otherwise inline the iteration pattern used for transcript broadcast.
- [x] Add unit tests:
  - `test_copilot_channel_reply_relayed_to_extension_clients` — register mock extension client, send `copilot_channel_reply` from a mock channel client, assert extension client received `copilot_reply` with the right `request_id`/`text`
  - `test_copilot_channel_status_relayed_to_extension_clients` — same shape for status

### Task 5: Channel server scaffold (`recog/copilot_channel.py`)
- [x] Create `recog/copilot_channel.py` with module docstring referencing ADR-0018/0019 patterns and the openspec change `ktalk-claude-copilot-chat`.
- [x] Implement two cooperating asyncio tasks:
  - **MCP Streamable HTTP server** using `mcp.server.fastmcp.FastMCP` (or `mcp.server.streamable_http`), bound to `127.0.0.1:8767`, mount path `/mcp`. If `FastMCP.run` only supports stdio, fall back to constructing the server via the lower-level `mcp.server.Server` + `streamable_http.stateless_app()` pattern. Pick whichever works in the installed `mcp` version and document the choice in a code comment.
  - **WS client** to `ws://127.0.0.1:8766` that sends `{"type": "copilot_channel_hello"}` after connect, then reads frames in a loop. Reconnect with simple exponential backoff (1s → 2s → 4s → max 30s) on disconnect. Use `websockets.connect` (already in project deps).
- [x] Expose two MCP tools:
  - `send_copilot_message(request_id: str | None, kind: str, text: str, meeting_id: str | None) -> dict` — sends `{"type": "copilot_channel_reply", "request_id": ..., "kind": ..., "text": ..., "meeting_id": ...}` over the WS client. Return `{"ok": True}` on success, `{"ok": False, "error": "..."}` on failure.
  - `set_copilot_status(request_id: str, status: str, message: str) -> dict` — sends `{"type": "copilot_channel_status", "request_id": ..., "status": ..., "message": ...}`. Same return shape.
- [x] Add module entry point: `if __name__ == "__main__": asyncio.run(main())` and define `main()` to launch both tasks via `asyncio.gather`.
- [x] Verify syntax: `./venv-gigaam/bin/python -m compileall recog/copilot_channel.py`
- [x] Verify boot: `./venv-gigaam/bin/python -m recog.copilot_channel --help` should not crash (add `argparse` with `--bridge-url`, `--mcp-host`, `--mcp-port` flags defaulting to `ws://127.0.0.1:8766`, `127.0.0.1`, `8767`)

### Task 6: Run all tests
- [ ] `./venv-gigaam/bin/python -m pytest tests/ -v`
- [ ] `./venv-gigaam/bin/python -m compileall recog frontend`
- [ ] If any pre-existing test breaks, fix root cause — do not skip or `xfail` to silence

### Task 7: Commit openspec change docs (separate atomic commit)
- [ ] `git status` — verify untracked files include `openspec/changes/ktalk-claude-copilot-chat/`
- [ ] Stage explicitly: `git add openspec/changes/ktalk-claude-copilot-chat/proposal.md openspec/changes/ktalk-claude-copilot-chat/design.md openspec/changes/ktalk-claude-copilot-chat/tasks.md openspec/changes/ktalk-claude-copilot-chat/.openspec.yaml openspec/changes/ktalk-claude-copilot-chat/specs/ktalk-copilot-chat/spec.md`
- [ ] Commit with message `docs(openspec): add ktalk-claude-copilot-chat change proposal` and a body explaining: tracer-bullet for live KTalk Copilot tab routed to a running Claude Code session via MCP Streamable HTTP; references D4 transport choice
- [ ] Do NOT push yet

### Task 8: Commit channel-server scaffold + bridge protocol (second atomic commit)
- [ ] `git status` — verify only `requirements.txt`, `ktalk_bridge.py`, `recog/copilot_channel.py`, `tests/test_ktalk_bridge.py` are modified/new
- [ ] Stage explicitly: `git add requirements.txt ktalk_bridge.py recog/copilot_channel.py tests/test_ktalk_bridge.py`
- [ ] Commit with message `feat(copilot): scaffold MCP HTTP channel server and bridge routing` and a body covering: new module `recog/copilot_channel.py` exposing `send_copilot_message`/`set_copilot_status` MCP tools over Streamable HTTP at `127.0.0.1:8767`; new bridge frame types `copilot_channel_hello`/`copilot_chat_message`/`copilot_channel_reply`/`copilot_channel_status`; isolated channel-client set; unit tests for hello/forward/relay/disconnected paths
- [ ] Push: `git push` (current branch is `main` — do not force-push; if push is rejected, stop and surface the error in the progress log)

## Post-Completion
- Manual smoke (not automated by ralphex): start `./recog-service.sh start` (or `python ktalk_bridge.py`), in another terminal run `./venv-gigaam/bin/python -m recog.copilot_channel`, then `claude mcp add --transport http copilot http://127.0.0.1:8767/mcp` once. In a Claude Code session ask Claude to call `send_copilot_message(kind="manual", text="hello from Claude")` — expect the bridge log to show a relayed `copilot_reply` broadcast.
- Next plan candidates: KTalk DOM `Copilot` tab UI (extension `ui.js`/`ws.js`/`state.js`), `get_recent_transcript` MCP tool + bridge transcript buffer, proactive `transcript_tick` scheduler.
- Spec tasks 1.1, 1.4, 2.x, 3.x, 4.x partially covered by this plan; spec tasks 5–8 untouched.
