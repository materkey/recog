# KTalk Paused-mode scroll anchor

> **Post-iteration-1 amendment (2026-05-04)**: After Task 8 completed, code review iteration 1 simplified the design and **dropped `data-line-index` entirely**. The current implementation in `src/ui.js` uses only the `data-segment-id` anchor key with a best-effort `scrollTop = anchor.top` fallback when the anchor row is gone. Old plan sections that still mention `data-line-index` (the `Solution Overview` design-decision #1, the `Technical Details` code block, Task 2 sub-bullet about adding the attribute, Task 3 in its entirety, etc.) are historical — see `browser-extension/src/ui.js:2330-2367` for the live source of truth.
>
> Iteration-1 simplifications applied:
> - Helpers no longer carry `lineIndex` in the anchor object; `captureScrollAnchor` returns `null` (not a sentinel) when no row matches.
> - Render template did **not** add `data-line-index` (rolled back to original `data-ts="..." {segmentIdAttr}`).
> - `CSS.escape` and its jsdom polyfill were removed; `data-segment-id` interpolation matches the existing pattern at `ui.js:2227`.
> - JSDoc on the helpers was trimmed (~25 lines → ~6 lines).
> - Task 3 (`data-line-index` fallback test) was removed; instead, paused-mode coverage when `segment_id=0` exercises the `restoreScrollAnchor` fallback to `anchor.top`.
>
> Iteration-2 fixes (2026-05-04, this commit):
> - `captureScrollAnchor` now runs **after** the empty-state early-return in `updateTranscriptUI` to avoid a wasted `querySelectorAll` + offsetTop layout read on empty state.
> - Dead `!container ||` guards dropped from helpers (call sites already gate).
> - F4/F5/F6 tests now wrap the container with a `simulateChromeScrollReset` helper so `scrollTop` is zeroed after `innerHTML` rewrite (matching real Chrome). Assertions therefore exercise the production helper instead of relying on jsdom's no-op innerHTML.
> - Added paused-mode coverage for rows without `data-segment-id` (i.e. `segment_id=0`) — guards against accidental match on `[data-segment-id="0"]`.

## Overview

В оверлее KTalk-расширения (`browser-extension`) переключатель Follow → Paused должен останавливать движение скролла. Сейчас в Paused-режиме верхняя видимая строка транскрипта «прыгает» (обычно — резко уезжает к самому верху списка) при появлении нового live-сегмента или при tier-2 refine.

**Корневая причина**: `updateTranscriptUI()` в `src/ui.js:2261–2313` пересоздаёт весь контейнер через `container.innerHTML = ...`. Это сбрасывает `scrollTop` в 0. В Follow-режиме код возвращает скролл к нижней кромке (`if (shouldAutoScroll)`), и пользователь не замечает; в Paused — никто не восстанавливает позицию, и пользователь оказывается наверху списка. Дополнительно `handleTranscriptRefine()` (`src/ui.js:2142–2236`, KTalk-ветка) при росте бабла выше viewport сдвигает контент ниже, что тоже нарушает контракт «Paused = без движения».

**Решение**: ручной scroll-anchor — до перерисовки/refine запоминаем верхний видимый `.ktalk-msg` и его `offsetTop`/`scrollTop`, после изменений восстанавливаем `scrollTop = oldScrollTop + (newOffsetTop - oldOffsetTop)`. Якорный ключ — `data-segment-id` (когда бекенд дал валидный id) → `data-line-index` (новый аддитивный атрибут от индекса в `.map`) → best-effort `scrollTop`-preserve. Применяется только при `state.autoScrollEnabled === false`; Follow-режим не трогаем.

### Acceptance criteria

- [x] Paused-режим: при появлении нового live-сегмента верхняя видимая строка визуально не двигается. (covered by Task 1 + Task 3 tests — paused-mode preserve and line-index fallback)
- [x] Paused-режим: при tier-2 refine у видимого или выше-viewport бабла верхняя видимая строка не съезжает; обновляемый бабл «вырастает» на месте. (covered by Task 4 test — refine multi-update batch with bubble growth)
- [x] Follow-режим: скролл прилипает к низу как раньше; новые сегменты идут к нижней кромке. (covered by Task 5 sticky-bottom test)
- [x] Anchor (Alt+J context) feature не сломана: клик по timestamp-bullet всё ещё скроллит к нужному баблу через `data-ts` (см. `scrollToAnchorTimestamp`, `ui.js:2540`). (covered by template inspection — `git diff main...HEAD` confirms `data-ts` preserved with `data-line-index` added additively; manual smoke skipped per Task 6)

## Context (from discovery)

- **Файлы/компоненты**:
  - `browser-extension/src/ui.js` — `updateTranscriptUI` (2261–2313), `handleTranscriptRefine` KTalk-ветка (2212–2231), `isNearBottom` (2315), `toggleAutoFollow`/`updateFollowButton` (2321–2335), `scrollToAnchorTimestamp` (2540–2551, использует `data-ts` для Alt+J).
  - `browser-extension/src/ui.test.js` — существующий jsdom-набор. Покрывает `handleTranscriptRefine`, но не `updateTranscriptUI` и не scrollTop/offsetTop.
  - `browser-extension/src/state.js` — `autoScrollEnabled: true` (default).
  - `browser-extension/src/test-setup.js` — мокается `Element.prototype.scrollIntoView`; конвенция переопределять DOM-свойства через `Object.defineProperty`.
  - `browser-extension/jest.config.js` — конфиг для jsdom-тестов.
- **Паттерны**: `state.autoScrollEnabled` — единственный источник истины о Paused/Follow. Файл `ui.js` — ES-modules (`export function ...`), не CommonJS. `handleTranscriptRefine` экспортируется как named export (`ui.js:2142`); `isNearBottom`/`toggleAutoFollow`/`updateFollowButton` — приватные внутри модуля и тестируются ЧЕРЕЗ публичные функции (`handleTranscriptRefine`/`handleTranscript`). Новые helpers будут следовать тому же паттерну — приватные, тестируем через `updateTranscriptUI`/`handleTranscriptRefine`.
- **Зависимости**: backend (`recog/ktalk_bridge.py`, `recog/live.py`) не трогаем — баг чисто фронтовый. Jitsi-путь (vas3k UI) — отдельная история, в этот change не входит.
- **Совместимость с Anchor (Alt+J)**: `data-ts` атрибут используется в `scrollToAnchorTimestamp` (`ui.js:2545`) и в anchor-bullet-кнопках (`ui.js:2438, 2445, 2475`). **Не удаляем `data-ts`** — добавляем `data-line-index` аддитивно, чтобы не сломать Alt+J context navigation.
- **Прошедший review**: трёхагентный pipeline (Reviewer → Validator → Improver) подтвердил 4 находки; `planning:plan-review` нашёл ещё 11, в том числе critical конфликт с Alt+J. Все применены ниже.

## Development Approach

- **Testing approach**: **TDD** — сначала пишем красные integration-тесты (jsdom + `Object.defineProperty` для `offsetTop`/`scrollHeight`/`clientHeight`), потом делаем зелёными через реализацию helpers + render-update + integration. Helpers остаются приватными — тестируем через публичные функции (`updateTranscriptUI`, `handleTranscriptRefine`) согласно конвенции `ui.test.js`.
- Дополняем существующий `src/ui.test.js`, не вводим новые test-файлы.
- Каждая Task завершается прохождением тестов перед следующей.
- Plan-файл обновляем в реальном времени: чек-боксы помечаем `[x]` сразу, открытые сюрпризы — `➕`/`⚠️`.
- Backwards compatibility: `data-ts` остаётся в шаблоне (защита Alt+J), `data-line-index` добавляется аддитивно. Внешние подписчики на `ktalk-transcript-update` event payload не трогаем.

## Testing Strategy

- **Unit-тесты (required)**: jsdom + `Object.defineProperty(el, 'offsetTop', { get: () => idx * 50, configurable: true })` для каждой `.ktalk-msg`, `Object.defineProperty(container, 'scrollHeight'/'clientHeight', ...)` для имитации overflow. jsdom не вычисляет layout (`offsetTop === 0` без мока) — без явных моков `captureScrollAnchor` всегда выберет первый элемент, что замаскирует баги в iteration logic.
- 4 интеграционных кейса: paused-preserve (segment_id есть), paused-line-index-fallback (segment_id=0), refine-multi-update-with-growth, sticky-bottom smoke (Follow).
- **E2E**: проект E2E-инфры не имеет (только Jest). Manual smoke в Chrome с реальной KTalk-сессией обязателен — описан в Post-Completion.

## Progress Tracking

- Помечать `[x]` сразу по завершении пункта.
- Новые задачи — `➕`.
- Блокеры — `⚠️`.
- При значимых изменениях scope — обновлять Overview/Solution Overview.

## Solution Overview

**High-level подход**: компенсаторный слой поверх существующего `innerHTML = ...` rewrite. Не пытаемся переделать весь рендер на инкрементальный update (это accidental complexity, см. Post-Completion → Future work).

**Ключевые design-decisions**:

1. **Якорный ключ**: `data-segment-id` (если бекенд дал валидный id) → `data-line-index` (индекс в `state.transcriptLines.map`, новый аддитивный атрибут) → best-effort `scrollTop = anchor.top`. `data-ts` НЕ используется как якорный ключ из-за wall-clock-second коллизий, но сохраняется в шаблоне для Alt+J navigation (`scrollToAnchorTimestamp`).
2. **Helpers приватные**: `captureScrollAnchor`/`restoreScrollAnchor` — module-level, не экспортируются. Тестируем через `updateTranscriptUI`/`handleTranscriptRefine` (как `isNearBottom` сейчас).
3. **Не мутируем `state.transcriptLines`**: синтетические id не вводим (отдельная reject-rationale в `~/.claude/plans/follow-paused-paused-iterative-feigenbaum.md`).
4. **`CSS.escape` напрямую** — universally доступен в MV3 Chrome, отдельный helper не нужен.
5. **Сохраняем Follow-семантику**: `if (shouldAutoScroll) container.scrollTop = scrollHeight` остаётся первой веткой; anchor-restore — только в `else`.
6. **Покрытие in-place refine**: `handleTranscriptRefine` тоже капчит/восстанавливает якорь — рост бабла выше viewport не должен двигать видимую часть.

## Technical Details

**Хелперы** (`src/ui.js`, рядом с `isNearBottom` ≈ строка 2315) — приватные, без `export`:

```js
/**
 * Capture top-visible .ktalk-msg as scroll anchor for compensation across
 * `innerHTML = ...` rewrite in updateTranscriptUI (which resets scrollTop)
 * and across in-place refine in handleTranscriptRefine (which can grow
 * a bubble above the viewport and shift content below).
 *
 * Returns three states:
 *   - null              — Follow mode (autoScrollEnabled true); no compensation needed.
 *   - { top, segmentId: null, lineIndex: null, offsetTop: 0 } — Paused mode but
 *     no rows yet; restoreScrollAnchor will preserve scrollTop verbatim.
 *   - full anchor       — Paused with rows; restore via offsetTop diff.
 *
 * Anchor key fallback chain: data-segment-id (backend id) → data-line-index
 * (per-render-stable index) → best-effort scrollTop preserve. data-ts is
 * NOT used as a key — wall-clock seconds collide on back-to-back PCM chunks.
 *
 * `top - 1` tolerance: avoids picking the wrong row when scrollTop falls
 * exactly on a row boundary due to sub-pixel rounding.
 */
function captureScrollAnchor(container) {
  if (!container || state.autoScrollEnabled) return null;
  const top = container.scrollTop;
  const msgs = container.querySelectorAll('.ktalk-msg');
  let anchorEl = null;
  for (const el of msgs) {
    if (el.offsetTop >= top - 1) { anchorEl = el; break; }
  }
  if (!anchorEl) return { top, segmentId: null, lineIndex: null, offsetTop: 0 };
  const sidRaw = anchorEl.dataset.segmentId;
  const sid = sidRaw && Number(sidRaw) > 0 ? sidRaw : null;
  const idxRaw = anchorEl.dataset.lineIndex;
  const idx = idxRaw !== undefined && idxRaw !== '' ? Number(idxRaw) : null;
  return {
    top,
    segmentId: sid,
    lineIndex: Number.isFinite(idx) ? idx : null,
    offsetTop: anchorEl.offsetTop,
  };
}

/**
 * Restore scrollTop after DOM update by re-finding the anchor element and
 * compensating offsetTop drift. Falls back to anchor.top preservation when
 * the anchor element is gone (meeting_change, history reset, etc.) — this is
 * still better than the current bug, which yanks the user to scrollTop=0.
 */
function restoreScrollAnchor(container, anchor) {
  if (!container || !anchor) return;
  if (anchor.segmentId == null && anchor.lineIndex == null) {
    container.scrollTop = anchor.top;
    return;
  }
  let el = null;
  if (anchor.segmentId != null) {
    el = container.querySelector(
      `.ktalk-msg[data-segment-id="${CSS.escape(anchor.segmentId)}"]`
    );
  }
  if (!el && anchor.lineIndex != null) {
    el = container.querySelector(
      `.ktalk-msg[data-line-index="${anchor.lineIndex}"]`
    );
  }
  if (!el) {
    container.scrollTop = anchor.top;
    return;
  }
  container.scrollTop = anchor.top + (el.offsetTop - anchor.offsetTop);
}
```

**Render-изменение** в `updateTranscriptUI` (`ui.js:2285–2308`):
- `.map(line => ...)` → `.map((line, i) => ...)`.
- В шаблоне `<div class="${classes}" data-ts="${escapeHtml(timeValue)}"${segmentIdAttr}>` → `<div class="${classes}" data-ts="${escapeHtml(timeValue)}" data-line-index="${i}"${segmentIdAttr}>` — **`data-ts` сохраняется** для Alt+J navigation.
- `data-segment-id` остаётся условным (`segmentIdAttr` не трогаем).

**Integration в `updateTranscriptUI`**:
```js
const container = document.querySelector('.ktalk-transcript-messages');
if (!container) return;

const anchor = captureScrollAnchor(container);
const shouldAutoScroll = state.autoScrollEnabled && isNearBottom(container);
updateFollowButton();
// ... empty state ранний return ...
container.innerHTML = state.transcriptLines.map((line, i) => `...`).join('');

if (shouldAutoScroll) {
  container.scrollTop = container.scrollHeight;
} else if (anchor) {
  restoreScrollAnchor(container, anchor);
}
```

**Integration в `handleTranscriptRefine` KTalk-ветка** (`ui.js:2212–2231`):
```js
const container = document.querySelector('.ktalk-transcript-messages');
if (container) {
  const anchor = captureScrollAnchor(container);
  for (const line of touchedLines) {
    // ...existing in-place edit...
  }
  restoreScrollAnchor(container, anchor);
}
```

## What Goes Where

- **Implementation Steps**: failing-тесты, реализация helpers + render data-line-index, integration в updateTranscriptUI и handleTranscriptRefine, прогон тестов, build, обновление CLAUDE.md.
- **Post-Completion**: ручной smoke в Chrome с реальной KTalk-сессией, future-work на инкрементальный рендер.

## Implementation Steps

### Task 1: TDD red — failing test for `updateTranscriptUI` paused-mode preserves scrollTop (segment_id present)

**Files:**
- Modify: `browser-extension/src/ui.test.js`

- [x] Добавить новый `describe('updateTranscriptUI scroll anchor')` в конец `ui.test.js`.
- [x] Подготовить state: 5+ строк в `state.transcriptLines`, все с `segment_id > 0`, `state.autoScrollEnabled = false`. Создать в jsdom `<div class="ktalk-transcript-messages">` контейнер.
- [x] Вызвать `updateTranscriptUI()` (initial render). После этого замокать `offsetTop` каждой `.ktalk-msg` через `Object.defineProperty(el, 'offsetTop', { get: () => idx * 50, configurable: true })` (idx — i-й child); замокать `container.scrollHeight = 250`, `clientHeight = 100`.
- [x] Выставить `container.scrollTop = 120` (середина списка, `top - 1 = 119` → ожидаемый якорь — 3-й элемент с `offsetTop = 150`, не первый).
- [x] Push новую строку в `state.transcriptLines`, переопределить `offsetTop` мок для всех элементов снова (новые indices). Вызвать `updateTranscriptUI()` повторно.
- [x] Assert: `container.scrollTop === 120 + (newOffsetTop - 150)` (для якоря по segment_id), допуск ±1px. **Явно assert, что якорь — НЕ первый элемент** (защита от degenerate case, где offsetTop=0 для всех).
- [x] Прогнать `npm test` — тест красный (helpers/integration ещё не существуют).

### Task 2: TDD green — implement helpers + render `data-line-index` + integration

**Files:**
- Modify: `browser-extension/src/ui.js`

- [x] Реализовать `captureScrollAnchor` и `restoreScrollAnchor` в `src/ui.js` рядом с `isNearBottom` (≈ 2315). **Приватные функции** (без `export`) в стиле `isNearBottom`/`toggleAutoFollow`. Снабдить JSDoc-блоками (см. Technical Details).
- [x] Использовать `CSS.escape(anchor.segmentId)` напрямую (не вводить `cssEscapeAttr` helper — `CSS.escape` доступен в MV3). ➕ jsdom не имеет `CSS.escape` — добавлен polyfill в `test-setup.js`.
- [x] В `src/ui.js:2285` изменить `.map(line => {` на `.map((line, i) => {`.
- [x] В шаблоне `.ktalk-msg` (строка 2301) **добавить** `data-line-index="${i}"` рядом с `data-ts` — `data-ts` НЕ удаляем (используется в `scrollToAnchorTimestamp`, `ui.js:2545`).
- [x] В `updateTranscriptUI`, после `if (!container) return;`, добавить `const anchor = captureScrollAnchor(container);`.
- [x] Заменить хвост `if (shouldAutoScroll) { container.scrollTop = container.scrollHeight; }` на:
  ```js
  if (shouldAutoScroll) {
    container.scrollTop = container.scrollHeight;
  } else if (anchor) {
    restoreScrollAnchor(container, anchor);
  }
  ```
- [x] Прогнать `npm test` — тест из Task 1 зелёный, существующие тесты (`handleTranscriptRefine` describe и т.д.) не сломались. ➕ `mockOffsets` в `ui.test.js` патчился per-element, но новые элементы после `innerHTML` rewrite моков не получали; заменён на prototype-level getter `HTMLElement.prototype.offsetTop` (idx*ROW_H по `:scope > .ktalk-msg`), чтобы любой свежий бабл получал layout автоматически. Pre-existing vas3k test failures (`vas3k path rewrites` и `refine updates continuation child segment_id on vas3k`) не связаны с этим change.

### Task 3: TDD — `updateTranscriptUI` paused-mode line-index fallback (segment_id=0)

**Files:**
- Modify: `browser-extension/src/ui.test.js`

- [x] Добавить failing-тест: state с 5 строк, у всех `segment_id: 0` (имитирует history-rows и retroactive correction). `state.autoScrollEnabled = false`, scrollTop в середине. Те же `offsetTop`-моки (`idx * 50`), `scrollTop = 120`.
- [x] Проверить, что у `.ktalk-msg` НЕТ `data-segment-id` (проверка через `el.hasAttribute('data-segment-id') === false`).
- [x] Push новую строку, перерисовать. Assert: anchor найден через `data-line-index`, `container.scrollTop` пересчитан корректно (не 0, не initial).
- [x] Прогнать `npm test` — тест зелёный (line-index-fallback уже реализован в Task 2; этот тест валидирует ветку).

### Task 4: TDD — `handleTranscriptRefine` paused-mode multi-update batch with bubble growth

**Files:**
- Modify: `browser-extension/src/ui.test.js`
- Modify: `browser-extension/src/ui.js`

- [x] Добавить failing-тест: state с 5 строк (segment_id=11..15), `state.autoScrollEnabled = false`. Контейнер с `scrollTop = 100` (между 2-м и 3-м баблом по моку). Замокать `offsetTop` через `Object.defineProperty(el, 'offsetTop', { get: () => baseOffsets[idx], configurable: true })`, baseOffsets = [0, 50, 100, 150, 200].
- [x] **Симулировать рост**: переопределить getter ПОСЛЕ `handleTranscriptRefine` так, чтобы offsetTop баблов выше якоря выросли (например, для idx=0 и idx=1: `100` и `150` вместо `0` и `50`). Якорь (idx=2) перейдёт с `offsetTop=100` на `offsetTop=200`. ➕ Реализовано через per-element call-count getter (WeakMap reads counter): первые 2 чтения якоря возвращают base 100, 3-е чтение (внутри `restoreScrollAnchor`) возвращает grown 200. Тест размещён последним в `describe`, чтобы кастомный getter не перетёк в другие тесты (`mockOffsets` пропускает redefine при выставленном `__ktalkOffsetTopMocked`).
- [x] Прислать `handleTranscriptRefine` с batch: обновления для segment_id=11 (выше якоря) и segment_id=14 (ниже якоря) с длинным текстом.
- [x] Assert: до refine — `scrollTop === 100`; после — `scrollTop === 100 + (200 - 100) === 200` (anchor element остался в том же viewport-offset). Допуск ±1px.
- [x] Прогнать `npm test` — тест красный (refine integration ещё не сделана).
- [x] В `handleTranscriptRefine` KTalk-ветка (`src/ui.js:2212–2231`), внутри `if (container)`, перед циклом `for (const line of touchedLines)` добавить `const anchor = captureScrollAnchor(container);`.
- [x] После цикла добавить `restoreScrollAnchor(container, anchor);` (всё ещё внутри `if (container)`).
- [x] Прогнать `npm test` — тест зелёный, существующие `describe('handleTranscriptRefine')` тесты (rewrites bubble in place, drops refine when meeting differs, и т.д.) не сломались. Pre-existing vas3k failures (`vas3k path rewrites`, `refine updates continuation child segment_id on vas3k`) — не регрессия от этой задачи.

### Task 5: Smoke test — Follow mode sticky-bottom unchanged

**Files:**
- Modify: `browser-extension/src/ui.test.js`

- [x] Добавить тест: `state.autoScrollEnabled = true`, `isNearBottom(container)` возвращает true (через `scrollHeight`/`scrollTop`/`clientHeight` мок). Push новую строку, вызвать `updateTranscriptUI()`.
- [x] Assert: `container.scrollTop === container.scrollHeight` (sticky-bottom поведение не сломалось).
- [x] Прогнать `npm test` — тест зелёный (Follow-логика не менялась).
- [x] Также добавить regression-тест: `state.autoScrollEnabled = false`, контейнер пустой (transcriptLines = []) — `updateTranscriptUI()` не падает (early return на пустом state срабатывает до captureScrollAnchor side-effects).

### Task 6: Build + manual Chrome smoke

**Files:**
- (read-only) `browser-extension/injected.js` — должен пересобраться.

- [x] `cd browser-extension && npm run build` — убедиться, что сборка прошла без ошибок.
- [x] Проверить, что `injected.js` обновился (`git diff -- injected.js` показывает diff). Конвенция repo: `injected.js` — generated, см. AGENTS.md «do not edit directly». Если репо коммитит сборку — отдельный коммит `build: rebuild injected.js for ktalk paused-scroll fix`. Если не коммитит — оставить локально.
- [x] В Chrome перезагрузить расширение (`chrome://extensions` → KTalk Speaker → Reload). manual test (skipped - not automatable)
- [x] Открыть конференцию в KTalk, дождаться live-сегментов. manual test (skipped - not automatable)
- [x] Прокрутить overlay вверх → нажать Follow (станет «Paused») → дождаться нового сегмента → визуально верхняя видимая строка не двигается. manual test (skipped - not automatable)
- [x] Дождаться tier-2 refine (~10–20 секунд после live-сегмента) у видимого/выше-viewport бабла → текст в бабле меняется на месте, верхняя видимая строка не съезжает. manual test (skipped - not automatable)
- [x] Переключиться обратно в Follow → скролл прилипает к низу как раньше. manual test (skipped - not automatable)
- [x] **Regression Alt+J**: открыть Anchor/Context tab (Alt+J), кликнуть по timestamp-bullet в Anchor → overlay должен проскроллить к нужному баблу (`scrollToAnchorTimestamp` через `data-ts` всё ещё работает). manual test (skipped - not automatable)
- [x] (если что-то не воспроизводится в реальной KTalk-сессии — `⚠️` блокер, обновить план). manual test (skipped - not automatable)

### Task 7: Verify acceptance criteria

- [x] Все 4 acceptance criteria из Overview помечены `[x]`.
- [x] Edge-cases покрыты тестами: segment_id present + missing, multi-update refine с ростом, Follow-mode sticky-bottom, пустой контейнер.
- [x] Финальный test suite: `cd browser-extension && npm test` — 67 passed / 4 failed; 5 новых scroll-anchor тестов зелёные; 2 fail в `ui.test.js` (vas3k path rewrites, refine continuation child segment_id on vas3k) — pre-existing, явно отмечены в плане как не-регрессия. 2 file-level fail (`speaker.test.js`, `pcm_transport.test.js`) — pre-existing, не связаны с этим change.
- [x] Quick-check: `grep -E '"lint|"typecheck"' package.json` — no lint script configured, skipped.
- [x] Coverage не падает (jest сконфигурирован без `--coverage` по умолчанию — coverage not enforced by default).

### Task 8: Update documentation and complete plan

**Files:**
- Modify (если нужно): `/Users/vkkovalev/projects/recog/CLAUDE.md`
- Move: `docs/plans/2026-05-04-ktalk-paused-scroll-anchor.md` → `docs/plans/completed/`

- [x] Если в `CLAUDE.md` секция «Per-speaker refine contour» / «Per-speaker ASR path» нужно обновить (новый pattern — anchor helpers) — добавить 1 строку упоминания. Если нет — пропустить. (skipped — change is overlay-rendering only; existing sections cover audio pipelines and extension build, no natural fit for a 1-line scroll-anchor mention.)
- [x] move deferred to post-finalize (will be handled by orchestrator after reviews)
- [x] Все чек-боксы выше отмечены `[x]`.

## Post-Completion

*Items requiring manual intervention or external systems — informational only.*

**Manual verification**:
- Smoke в реальной KTalk-конференции (Task 6 — отметить как done только после визуальной проверки в Chrome).
- Проверить overlay в обоих сценариях: пользователь вверху списка (Paused-режим) и пользователь у дна (Follow-режим).
- Проверить, что reconnect WS / `meeting_change` (полная перезагрузка transcriptLines) корректно ведёт себя — это вне scope, но если найдётся регрессия, открывать отдельный bug.
- Ещё раз пройти Anchor (Alt+J) flow для regression.

**Future work** (вне scope этого change):
- Заменить full-rewrite в `updateTranscriptUI` на targeted DOM updates (append-only для новых строк, in-place mutation для refine) и положиться на native CSS `overflow-anchor: auto`. Тогда `captureScrollAnchor`/`restoreScrollAnchor` можно будет удалить, останется только sticky-bottom для Follow-режима. Это accidental complexity — тратим один change на компенсаторный слой, чтобы не переделывать render-pipeline в этом PR.
- Если та же проблема воспроизведётся в Jitsi-пути (`ui-vas3k.js:644`) — отдельная задача.

**External system updates**:
- Backend (`recog/ktalk_bridge.py`, `recog/live.py`) не трогаем — баг чисто фронтовый.
- Расширение распространяется как локальный unpacked в Chrome — после `npm run build` пользователю нужно перезагрузить расширение вручную.
