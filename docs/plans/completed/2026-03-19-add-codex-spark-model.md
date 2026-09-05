# Add Codex Spark model to Chrome extension model selector

## Overview
- Add `gpt-5.3-codex-spark` as a selectable AI model in the KTalk Chrome extension
- Label: "Codex Spark" in both ui.js (KTalk platform) and ui-vas3k.js (Vas3k platform)
- Ensure ktalk_bridge.py correctly handles spark model (skip `model_reasoning_effort` flag)

## Context (from discovery)
- Files involved:
  - `browser-extension/src/ui.js` — `getAnchorModelOptions()` + summary modal HTML
  - `browser-extension/src/ui-vas3k.js` — summary modal HTML with `<select>` options
  - `recog/ktalk_bridge.py` — `_call_codex_cli()` passes `model_reasoning_effort=xhigh` for all codex models, spark doesn't support reasoning
  - `browser-extension/src/ui-vas3k.test.js` — existing tests for model selector
- Model format: `codex:gpt-5.3-codex-spark` (parsed by ktalk_bridge.py as `codex_model = "gpt-5.3-codex-spark"`)
- Default remains `codex` (GPT-5.4), spark is just an additional option

## Development Approach
- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- Run tests after each change

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Add Codex Spark to ui.js model lists
- [x] Add `{ value: 'codex:gpt-5.3-codex-spark', label: 'Codex Spark' }` to `getAnchorModelOptions()` in `src/ui.js` (after the GPT-4.1 entry)
- [x] Add matching `<option>` to summary modal `<select>` HTML in `src/ui.js` (~line 1652)
- [x] Verify both selectors render the new option

### Task 2: Add Codex Spark to ui-vas3k.js model list
- [x] Add `<option value="codex:gpt-5.3-codex-spark">Codex Spark</option>` to summary modal `<select>` in `src/ui-vas3k.js` (~line 666, after GPT-4.1)

### Task 3: Fix ktalk_bridge.py — skip reasoning effort for spark
- [x] In `_call_codex_cli()`, conditionally skip `-c model_reasoning_effort=xhigh` when model contains "spark"
- [x] Verify other codex models still get reasoning effort flag

### Task 4: Update tests
- [x] Update `ui-vas3k.test.js` — add assertion for spark option in model selector
- [x] Run `npm test` in extension directory — must pass

### Task 5: Build and verify
- [x] Run `npm run build` in `browser-extension/`
- [x] Verify `injected.js` bundle is regenerated

## Technical Details
- Value format: `codex:gpt-5.3-codex-spark` — ktalk_bridge.py splits on `:` and passes `gpt-5.3-codex-spark` to codex CLI
- Spark model doesn't support `model_reasoning_effort` config — passing it may cause errors or be silently ignored
- Position in dropdown: after "Codex (GPT-4.1)", before Claude options

## Post-Completion
- Reload Chrome extension in `chrome://extensions` → Developer Mode → Reload
- Test with a live KTalk session to verify spark model works end-to-end
