# Fix node not found in PATH for auto-generation under launchd

## Overview
- Auto-generation (title via Codex CLI) fails with `env: node: No such file or directory` under launchd
- Root cause: `codex` has `#!/usr/bin/env node` shebang, launchd PATH doesn't include nvm-managed node
- Manual web UI works because server process inherits full PATH from interactive shell
- `ktalk_bridge.py` already has working `_get_node_env()` — extract and share it

## Context
- `_get_node_env()` in `ktalk_bridge.py:685-705` — reference implementation
- `_resolve_cli()` in `frontend/ai_provider.py:17-35` — resolves binary path but not node runtime
- `com.recog.live.plist` — missing NVM paths (unlike `com.recog.frontend.plist`)
- Affected subprocess calls: `title_generator.py:51`, `ai_provider.py:243,284` (CodexCliProvider), `ai_provider.py:153,172` (ClaudeCliProvider)

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Small, focused changes
- Run tests after each change

## Implementation Steps

### Task 1: Add `_get_node_env()` to `frontend/ai_provider.py`
- [x] Add `_get_node_env()` function next to `_resolve_cli()` in `frontend/ai_provider.py`, ported from `ktalk_bridge.py:685-705`
- [x] Pass `env=_get_node_env()` in `CodexCliProvider.complete()` subprocess call (line 243)
- [x] Pass `env=_get_node_env()` in `CodexCliProvider.stream()` subprocess call (line 284)
- [x] Pass `env=_get_node_env()` in `ClaudeCliProvider.complete()` subprocess call (line 153)
- [x] Pass `env=_get_node_env()` in `ClaudeCliProvider.stream()` subprocess call (line 172)
- [x] run tests - `python -m pytest tests/ -v`

### Task 2: Fix `title_generator.py` subprocess call
- [x] Import `_get_node_env` from `frontend.ai_provider` in `recog/title_generator.py`
- [x] Pass `env=_get_node_env()` to `create_subprocess_exec` call (line 51)
- [x] run tests - `python -m pytest tests/ -v`

### Task 3: Deduplicate `ktalk_bridge.py`
- [x] Replace `_get_node_env()` method in `ktalk_bridge.py` with import from `frontend.ai_provider`
- [x] Update all callers in `ktalk_bridge.py` (`_call_codex_cli`, `_call_claude_cli`) to use imported function
- [x] run tests - `python -m pytest tests/ -v`

### Task 4: Verify acceptance criteria
- [x] `python -m pytest tests/ -v` — all pass
- [x] `python -m compileall recog frontend ktalk_bridge.py` — no syntax errors

## Post-Completion
- Restart service: `./recog-service.sh restart`
- Trigger a meeting end to verify auto-title-generation works under launchd
