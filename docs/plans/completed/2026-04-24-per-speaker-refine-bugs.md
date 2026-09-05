# Fix four bugs in per-speaker refine pipeline

## Overview

После мержа refine-цепочки в KTalk на ветке `gigaam-upstream-mps` живой замер показал три кода-дефекта и один вопрос верификации. Фикс делает extension и webapp согласованными (1 bubble на смену спикера, refine доезжает до continuation-chunks) и добавляет gated подавление cross-pid echo между двумя remote pipeline одного физического голоса.

## Context (from discovery)

- Исходный архитектурный план: `/Users/user/.claude/plans/rippling-mixing-giraffe.md` (277 строк, прошёл GAN-review).
- `browser-extension/src/ui.js:1140-1306` — `handleTranscript` и `handleTranscriptRefine`. Merge-блок в районе 1203 не проверяет `speaker` при `is_continuation=true` → разные говорящие склеиваются в один bubble, refine на continuation-chunk контаминирует чужой текст.
- `recog/live.py:893` — batch tuple `(pid, speaker_name, is_local, _pcm, chunk_start_ms, _end_ms, pid_audio_ms_start, pid_audio_ms_end)`. `_on_pcm_batch` уже держит `_state_lock`, есть per-pid `_pcm_prev_words` / `_pcm_pid_state` / `_pcm_segment_texts`.
- `recog/live.py:307-329` — `normalize_word`, `deduplicate` для per-pid dedup; переиспользуется для phrase-token overlap.
- `recog/live.py:1371-1411` — `_handle_mic_chunk` пишет напрямую в `transcript_lines` с `is_local=True`, не проходит через `_on_pcm_batch` (ADR-0018 D9).
- `browser-extension/src/pcm_transport.js:234` — hard early-return `if (isLocal) return;` для mic.
- `browser-extension/manifest.json:4` — уже `"1.4.1"` (из коммита `BASE_REVISION`); для cache-bust bump в 1.4.2.
- `ktalk_bridge.KTalkBridge.send_transcript(...)` — точка, где добавляется optional `source_id` в WS payload.
- `recog/config.py` — место для нового gate `pcm_cross_pid_echo_suppression_enabled()`; default off.
- `tests/test_live_speaker_attribution.py` — есть fixture `_fresh_transcriber_mock`; паттерн PCM-тестов через `_on_pcm_batch` с конструированным batch tuple.

## Development Approach

- **Testing approach**: Regular. Новые Jest-тесты в `ui.test.js` и pytest-тесты в `test_live_speaker_attribution.py` добавляем вместе с кодом; каждый таск проверяется перед переходом к следующему.
- Complete each task fully before moving to the next.
- Три атомарных кода-коммита (Task 1 = extension fix, Task 2 = extension test, Task 3 = live fix). Task 4 — rebuild + manifest bump включается в один из коммитов или отдельно без тикета. Task 5 — commit + push.

## Progress Tracking

- Mark completed items with `[x]` immediately when done.
- Add newly discovered tasks with ➕ prefix.
- Document issues/blockers with ⚠️ prefix.

## Implementation Steps

### Task 1: Stable source guard on merge (Bug 1)

- [x] Add optional `source_id: str = ""` parameter to `KTalkBridge.send_transcript` in `recog/ktalk_bridge.py`; include `"source_id": source_id` in the WS payload only when non-empty. Preserve existing positional call sites.
- [x] In `recog/live.py` `_on_pcm_batch`, pass `source_id=pid` to `send_transcript(...)`; do not touch legacy / non-PCM call sites in `_handle_mic_chunk` etc.
- [x] In `browser-extension/src/ui.js` `handleTranscript` (around 1140): read `source_id` from the payload, store it on `state.transcriptLines[i].source_id` for every line (both new and continuation).
- [x] Rewrite the continuation merge predicate at `ui.js:1203`:
  ```js
  const prev = state.transcriptLines[state.transcriptLines.length - 1];
  const sameSource = source_id && prev && prev.source_id === source_id;
  const legacySameSpeaker = prev && !source_id && !prev.source_id && prev.speaker === speaker;
  if (is_continuation && state.transcriptLines.length > 0 && (sameSource || legacySameSpeaker)) {
      // source_id prevents continuation chunks from contaminating another
      // source's segment_ids/segment_texts; speaker fallback is only for
      // legacy frames without source_id.
      prev.text += ' ' + text;
      // ... existing segment_ids/segment_texts bookkeeping (keep as is) ...
  ```
- [x] Add three tests to `browser-extension/src/ui.test.js`:
  1. `'handleTranscript pushes new line when continuation source_id differs'` — Alice sid=20 `source_id="pid-a"` `is_continuation=false`, Alice sid=21 `source_id="pid-b"` `is_continuation=true`. Assert two lines, `segment_ids` are not mixed.
  2. `'handleTranscript merges continuation when source_id matches even if speaker label changes'` — Alice sid=22 `source_id="pid-a"`, "Alice Renamed" sid=23 `source_id="pid-a"` `is_continuation=true`. Assert one line, `segment_ids === [22, 23]`.
  3. Legacy fallback: no `source_id`; same speaker merges, different speaker pushes new line.
- [x] Run `cd browser-extension && npm run test` — all tests green.
- [x] Commit as `fix(extension): keep different-source chunks in separate bubbles` listing the three changed files explicitly (`recog/ktalk_bridge.py`, `recog/live.py`, `browser-extension/src/ui.js`, `browser-extension/src/ui.test.js`).

### Task 2: Regression coverage for refine on continuation-chunks (Bug 2)

- [x] Inspect existing `ui.test.js`; confirm these already exist (do not duplicate):
  - `'refine updates continuation child segment_id inside merged ktalk bubble'`
  - `'refine updates continuation child segment_id on vas3k per-chunk DOM line'`
- [x] Add one new test: `'refine on different-source continuation chunk targets its own bubble, not the previous'`
  - Alice sid=40 `source_id="pid-a"` `is_continuation=false`; Bob sid=41 `source_id="pid-b"` `is_continuation=true` (after Task 1 this is a separate bubble).
  - Call `handleTranscriptRefine({updates:[{segment_id:41, text:'...refined tail...'}]})`.
  - Assert: rewrite happens in Bob's bubble; `state.transcriptLines[0].segment_ids` does not contain `41`; `state.transcriptLines[1].segment_ids` does.
- [x] `npm run test` green.
- [x] Commit as `test(extension): cover refine targeting different-source continuation-chunk` listing only `ui.test.js`.

### Task 3: Gated cross-pid phrase-echo suppression (Bug 3) — ➕ REVERTED

> **Reverted after live replay.** Исходная «Bug 3» оказалась ошибкой интерпретации:
> описанный сценарий с двумя Chrome-вкладками одного голоса был **намеренным
> тестом разбивки bubble'ов**, а не продовой проблемой. В боевых условиях
> задвоения через два remote pipeline одного физического спикера не случается.
> Heuristic token-set overlap + аудио-окно был реализован и затем полностью
> откатан: удалены `pcm_cross_pid_echo_suppression_enabled()`, `_CrossPidPhrase`
> dataclass, `_cross_pid_recent_phrases` deque, `_is_cross_pid_echo` /
> `_audio_windows_close` / `_remember_cross_pid_phrase`, wiring в `_on_pcm_batch`,
> clear в `_invalidate_pending_chunks`, env-var `RECOG_PCM_CROSS_PID_ECHO_SUPPRESSION`,
> 9 pytest-тестов и упоминания в `CLAUDE.md`/`README.md`. Чекбоксы ниже
> оставлены ASIS как история реализации.

- [x] Add to `recog/config.py`:
  ```python
  def pcm_cross_pid_echo_suppression_enabled() -> bool:
      """True only when per-speaker PCM is enabled and cross-pid echo dropping is explicitly enabled."""
      if not pcm_transport_enabled():
          return False
      return os.environ.get("RECOG_PCM_CROSS_PID_ECHO_SUPPRESSION", "0") == "1"
  ```
- [x] Append to `.env.example`:
  ```dotenv
  # Optional, risky: suppress high-confidence duplicate remote phrases across different PCM pids.
  # Default 0 preserves canonical transcript text; set to 1 only for dual-tab echo validation.
  RECOG_PCM_CROSS_PID_ECHO_SUPPRESSION=0
  ```
- [x] In `recog/live.py`, add near `_pcm_segment_texts` initialization (around line 432):
  ```python
  @dataclass(slots=True)
  class _CrossPidPhrase:
      pid: str
      speaker: str
      text: str
      tokens: tuple[str, ...]
      chunk_start_ms: float
      chunk_end_ms: float

  # On LiveTranscriber.__init__:
  self._cross_pid_recent_phrases: deque[_CrossPidPhrase] = deque(maxlen=32)
  self._cross_pid_echo_checked_count = 0
  self._cross_pid_echo_suppressed_count = 0
  ```
  Place `_CrossPidPhrase` dataclass at module scope; add `from collections import deque` and `from dataclasses import dataclass` if not already imported.
- [x] In `_invalidate_pending_chunks` (around line 1840): add `self._cross_pid_recent_phrases.clear()`, `self._cross_pid_echo_checked_count = 0`, `self._cross_pid_echo_suppressed_count = 0` next to existing `_pcm_prev_words.clear()` / `_pcm_pid_state.clear()`.
- [x] Add helper methods to `LiveTranscriber`:
  ```python
  @staticmethod
  def _audio_windows_close(a_start_ms, a_end_ms, b_start_ms, b_end_ms, *, tolerance_ms):
      return max(a_start_ms, b_start_ms) <= min(a_end_ms, b_end_ms) + tolerance_ms

  def _remember_cross_pid_phrase(self, pid, speaker, text, tokens, chunk_start_ms, chunk_end_ms):
      self._cross_pid_recent_phrases.append(_CrossPidPhrase(
          pid=pid, speaker=speaker, text=text, tokens=tokens,
          chunk_start_ms=chunk_start_ms, chunk_end_ms=chunk_end_ms,
      ))

  def _is_cross_pid_echo(self, *, pid, speaker, text, chunk_start_ms, chunk_end_ms) -> bool:
      tokens = tuple(t for t in (normalize_word(w) for w in text.split()) if t)
      if len(tokens) < 3:
          self._remember_cross_pid_phrase(pid, speaker, text, tokens, chunk_start_ms, chunk_end_ms)
          return False
      current = set(tokens)
      self._cross_pid_echo_checked_count += 1
      for prior in self._cross_pid_recent_phrases:
          if prior.pid == pid:
              continue
          if not self._audio_windows_close(prior.chunk_start_ms, prior.chunk_end_ms,
                                            chunk_start_ms, chunk_end_ms, tolerance_ms=1500):
              continue
          overlap = len(current.intersection(prior.tokens)) / max(len(current), len(set(prior.tokens)))
          if overlap >= 0.8:
              self._remember_cross_pid_phrase(pid, speaker, text, tokens, chunk_start_ms, chunk_end_ms)
              return True
      self._remember_cross_pid_phrase(pid, speaker, text, tokens, chunk_start_ms, chunk_end_ms)
      return False
  ```
- [x] Wire into `_on_pcm_batch` per-item loop (~line 906-914), after `deduplicate()` and `speaker_label` computation, before `append_segment` / `send_transcript`:
  ```python
  if (pcm_cross_pid_echo_suppression_enabled()
          and not is_local
          and self._is_cross_pid_echo(
              pid=pid, speaker=speaker_label, text=deduped,
              chunk_start_ms=chunk_start_ms, chunk_end_ms=_end_ms)):
      self._cross_pid_echo_suppressed_count += 1
      print(
          "[PerSpeaker] cross-pid echo suppressed "
          f"pid={pid} words={len(deduped.split())} "
          f"window={chunk_start_ms:.0f}-{_end_ms:.0f}ms "
          f"suppressed_total={self._cross_pid_echo_suppressed_count}",
          file=sys.stderr,
      )
      continue
  ```
- [x] Add six pytest cases to `tests/test_live_speaker_attribution.py`. All drive `_on_pcm_batch` directly with `is_local=False`:
  1. `test_pcm_cross_pid_phrase_echo_suppressed_when_enabled` — two remote pids emit the same 3+ token phrase in close audio windows with `RECOG_PCM_CROSS_PID_ECHO_SUPPRESSION=1`; second segment is neither stored nor sent.
  2. `test_pcm_cross_pid_same_single_word_from_two_remotes_not_suppressed` — two remote pids emit the same one-word utterance ("да" or "зайчик") inside the window; both stored/sent.
  3. `test_pcm_cross_pid_different_words_from_two_remotes_not_suppressed` — two remote pids, different phrases, close audio windows; both stored/sent.
  4. `test_pcm_cross_pid_phrase_outside_audio_window_not_suppressed` — same phrase, windows farther than tolerance; both stored/sent.
  5. `test_pcm_cross_pid_echo_gate_off_does_not_suppress` — with gate `=0`, duplicate phrases are not suppressed.
  6. `test_pcm_cross_pid_same_python_batch_but_separate_audio_windows_not_suppressed` — two ASR results in one Python batch but deliberately separated `chunk_start_ms`/`_end_ms` values; must not be treated as echo.
- [x] Run `venv-gigaam/bin/python -m pytest tests/test_live_speaker_attribution.py -v` — all tests green.
- [x] Run `venv-gigaam/bin/python -m pytest tests/ -v -m "not e2e"` — no regressions.
- [x] Commit as `fix(live): suppress gated cross-pid phrase echoes` listing only `recog/config.py`, `.env.example`, `recog/live.py`, `tests/test_live_speaker_attribution.py`.

### Task 4: Rebuild extension and bump manifest

- [x] Edit `browser-extension/manifest.json:4` — bump `"version": "1.4.1"` → `"version": "1.4.2"`. Leave `package.json:3` on `"1.0.0"` (pre-existing drift, out of scope).
- [x] `cd browser-extension && npm run build` — verify `injected.js` regenerated without errors.
- [x] Amend the Task 1 extension commit (or add as a trailing commit on top of Task 1) so the manifest bump and rebuilt `injected.js` live in the same commit as the extension source changes. Do not amend if Task 2 or Task 3 commits were already pushed — in that case add a new commit `chore(extension): bump manifest 1.4.1 → 1.4.2 and rebuild`.

### Task 5: Commit and push

- [x] `git log --oneline origin/gigaam-upstream-mps..HEAD` — expect the new 3 code commits on top of existing local commits; verify each commit touches only files from its scope.
- [x] `git push` to `gigaam-upstream-mps` (current branch). No force push.

## Post-Completion

- Manual operator smoke per Task 4 of the source plan (`recog-service.sh restart`, verify `RECOG_PCM_(TRANSPORT|REFINE)` in `.env`, check `RefineWorker` in logs, subscriber_hello in frontend logs, `data-refined="true"` in DOM after ≥5 s silence).
- 30-s real-meeting replay: extension bubble count ≈ webapp row count (minus legit same-source continuation merges); refine reaches 2nd/3rd chunks of the same speaker turn.
- Optional dual-tab echo validation with `RECOG_PCM_CROSS_PID_ECHO_SUPPRESSION=1`: confirm phrase-level cross-pid duplicates ("зайчик выбегает ...") disappear; with gate `=0`, transcript text is byte-equivalent to pre-Task 3.
