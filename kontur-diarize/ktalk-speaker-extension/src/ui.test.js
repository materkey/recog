/**
 * Tests for handleTranscriptRefine — tier-2 in-place bubble rewrite.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://ktalk.ru/"}
 */

import { jest } from '@jest/globals';

let state;
let handleTranscript;
let handleTranscriptRefine;
let handleNewMeeting;

// `isJitsiPlatform` lives in `platforms/index.js` and is derived from
// `window.location.hostname`. jsdom forbids mutating `location` after setup,
// so we mock the module and flip the flag per-test.
const isJitsiPlatformMock = jest.fn(() => false);

beforeAll(async () => {
  await jest.unstable_mockModule('./platforms/index.js', () => ({
    __esModule: true,
    getPlatform: jest.fn(() => 'ktalk'),
    getPlatformAdapter: jest.fn(() => null),
    getPlatformDisplayName: jest.fn(() => 'KTalk'),
    detectPlatform: jest.fn(() => 'ktalk'),
    isJitsiPlatform: isJitsiPlatformMock,
    resetAdapter: jest.fn(),
  }));

  state = (await import('./state.js')).state;
  ({ handleTranscript, handleTranscriptRefine, handleNewMeeting } = await import('./ui.js'));
});

describe('handleTranscriptRefine', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    state.transcriptLines = [];
    state.bioRead = { enabled: false };
    state.currentMeetingId = null;
    isJitsiPlatformMock.mockReturnValue(false);
  });

  test('rewrites ktalk bubble text in place and flips refined flag in state', () => {
    state.transcriptLines = [
      { speaker: 'Alice', text: 'привет мир', time: '10:00:01', segment_id: 7 },
      { speaker: 'Bob', text: 'нет', time: '10:00:02', segment_id: 8 },
    ];
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages">
        <div class="ktalk-msg remote" data-segment-id="7">
          <div class="ktalk-msg-text">привет мир</div>
        </div>
        <div class="ktalk-msg remote" data-segment-id="8">
          <div class="ktalk-msg-text">нет</div>
        </div>
      </div>
    `;

    handleTranscriptRefine({
      type: 'transcript_refine',
      updates: [
        { segment_id: 7, text: 'привет, коллеги' },
      ],
      refined_at: '2026-04-24T10:00:02',
    });

    const bubble = document.querySelector('.ktalk-msg[data-segment-id="7"] .ktalk-msg-text');
    expect(bubble.innerHTML).toBe('привет, коллеги');
    expect(bubble.classList.contains('refine-fade')).toBe(true);
    expect(state.transcriptLines[0].text).toBe('привет, коллеги');
    expect(state.transcriptLines[0].refined).toBe(true);
    // Non-targeted bubble stays untouched.
    const other = document.querySelector('.ktalk-msg[data-segment-id="8"] .ktalk-msg-text');
    expect(other.innerHTML).toBe('нет');
    expect(state.transcriptLines[1].refined).toBeFalsy();
  });

  test('ignores update when bubble is not present (no throw)', () => {
    state.transcriptLines = [
      { speaker: 'Alice', text: 'x', time: '10:00:01', segment_id: 5 },
    ];
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;

    expect(() => handleTranscriptRefine({
      updates: [{ segment_id: 99, text: 'y' }],
    })).not.toThrow();
    // State for non-matching segment_id is untouched.
    expect(state.transcriptLines[0].text).toBe('x');
  });

  test('noop on empty updates list', () => {
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages">
        <div class="ktalk-msg" data-segment-id="1">
          <div class="ktalk-msg-text">stay</div>
        </div>
      </div>
    `;
    handleTranscriptRefine({ updates: [] });
    expect(document.querySelector('.ktalk-msg-text').innerHTML).toBe('stay');
  });

  test('drops updates with invalid segment_id or non-string text', () => {
    state.transcriptLines = [
      { speaker: 'Alice', text: 'keep', time: '10:00:01', segment_id: 3 },
    ];
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages">
        <div class="ktalk-msg" data-segment-id="3">
          <div class="ktalk-msg-text">keep</div>
        </div>
      </div>
    `;

    handleTranscriptRefine({
      updates: [
        { segment_id: 0, text: 'ignored — zero id' },
        { segment_id: 3, text: null },
        { segment_id: 'bad', text: 'ignored — non-numeric' },
      ],
    });

    expect(document.querySelector('.ktalk-msg-text').innerHTML).toBe('keep');
    expect(state.transcriptLines[0].text).toBe('keep');
  });

  test('drops refine when meeting_id differs from current meeting', () => {
    state.currentMeetingId = 'meeting-B';
    state.transcriptLines = [
      { speaker: 'Alice', text: 'B-text', time: '10:00:01', segment_id: 3 },
    ];
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages">
        <div class="ktalk-msg" data-segment-id="3">
          <div class="ktalk-msg-text">B-text</div>
        </div>
      </div>
    `;

    // Refine payload targets the PREVIOUS meeting. Segment_ids restart
    // at 1 per meeting — if we did not filter by meeting_id, this would
    // rewrite Alice's current-meeting bubble with stale text.
    handleTranscriptRefine({
      type: 'transcript_refine',
      meeting_id: 'meeting-A',
      updates: [{ segment_id: 3, text: 'stale from A' }],
    });

    expect(document.querySelector('.ktalk-msg-text').innerHTML).toBe('B-text');
    expect(state.transcriptLines[0].text).toBe('B-text');
    expect(state.transcriptLines[0].refined).toBeFalsy();
  });

  test('vas3k path rewrites .jitsi-transcript-text by data-segment-id', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    state.transcriptLines = [
      { speaker: 'Alice', text: 'raw', time: '10:00:01', segment_id: 11 },
    ];
    document.body.innerHTML = `
      <div class="jitsi-transcript-content">
        <div class="jitsi-transcript-line" data-segment-id="11">
          <div class="jitsi-transcript-line-header">
            <span class="jitsi-transcript-speaker">Alice</span>
            <span class="jitsi-transcript-time">10:00:01</span>
          </div>
          <div class="jitsi-transcript-text">raw</div>
        </div>
      </div>
    `;

    handleTranscriptRefine({
      updates: [{ segment_id: 11, text: 'refined' }],
    });

    const textEl = document.querySelector('.jitsi-transcript-line[data-segment-id="11"] .jitsi-transcript-text');
    expect(textEl.textContent).toBe('refined');
    expect(textEl.classList.contains('refine-fade')).toBe(true);
    expect(state.transcriptLines[0].refined).toBe(true);
  });

  test('refine updates continuation child segment_id inside merged ktalk bubble', () => {
    // Simulate two PCM chunks from the same speaker: second arrives as
    // is_continuation=true, so the extension merges it into the previous
    // state line. Both chunks still have their own backend segment_id.
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages"></div>
    `;
    handleTranscript({
      speaker: 'Alice',
      text: 'первая часть',
      timestamp: '10:00:01',
      is_continuation: false,
      segment_id: 20,
    });
    handleTranscript({
      speaker: 'Alice',
      text: 'вторая часть',
      timestamp: '10:00:04',
      is_continuation: true,
      segment_id: 21,
    });

    expect(state.transcriptLines).toHaveLength(1);
    const merged = state.transcriptLines[0];
    expect(merged.segment_ids).toEqual([20, 21]);
    expect(merged.segment_texts).toEqual({ 20: 'первая часть', 21: 'вторая часть' });
    expect(merged.text).toBe('первая часть вторая часть');

    // DOM exposes only primary segment_id (20) since updateTranscriptUI
    // rebuilds from state — a refine for the continuation's id (21) must
    // still rewrite that merged bubble via child-id lookup.
    handleTranscriptRefine({
      updates: [{ segment_id: 21, text: 'уточнённая вторая часть' }],
    });

    const textEl = document.querySelector('.ktalk-msg[data-segment-id="20"] .ktalk-msg-text');
    expect(textEl).not.toBeNull();
    expect(textEl.innerHTML).toBe('первая часть уточнённая вторая часть');
    expect(state.transcriptLines[0].segment_texts[21]).toBe('уточнённая вторая часть');
    expect(state.transcriptLines[0].text).toBe('первая часть уточнённая вторая часть');
    expect(state.transcriptLines[0].refined).toBe(true);
  });

  test('refine updates continuation child segment_id on vas3k per-chunk DOM line', () => {
    // Vas3k renders one DOM line per chunk (addVas3kTranscriptLine is
    // called for every transcript incl. continuations), so each child
    // segment_id has its own bubble; refine must target the specific one.
    isJitsiPlatformMock.mockReturnValue(true);
    document.body.innerHTML = `
      <div class="jitsi-transcript-content"></div>
    `;
    handleTranscript({
      speaker: 'Alice',
      text: 'one',
      timestamp: '10:00:01',
      is_continuation: false,
      segment_id: 30,
    });
    handleTranscript({
      speaker: 'Alice',
      text: 'two',
      timestamp: '10:00:04',
      is_continuation: true,
      segment_id: 31,
    });

    expect(state.transcriptLines).toHaveLength(1);
    expect(state.transcriptLines[0].segment_ids).toEqual([30, 31]);

    handleTranscriptRefine({
      updates: [{ segment_id: 31, text: 'refined-two' }],
    });

    // Vas3k DOM: child segment_id bubble rewritten.
    const el31 = document.querySelector('.jitsi-transcript-line[data-segment-id="31"] .jitsi-transcript-text');
    expect(el31).not.toBeNull();
    expect(el31.textContent).toBe('refined-two');
    // State child text updated; merged text rebuilt.
    expect(state.transcriptLines[0].segment_texts[31]).toBe('refined-two');
    expect(state.transcriptLines[0].text).toBe('one refined-two');
    expect(state.transcriptLines[0].refined).toBe(true);
  });

  test('refine on different-source continuation chunk targets its own bubble, not the previous', () => {
    // Two different PCM sources (pid-a, pid-b) — after Task 1's source_id
    // discrimination they render as separate bubbles even though Bob's chunk
    // arrives with is_continuation=true. A refine for Bob's segment_id must
    // rewrite Bob's bubble and leave Alice's bubble untouched.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    handleTranscript({
      speaker: 'Alice',
      text: 'alice начало',
      timestamp: '10:00:01',
      is_continuation: false,
      segment_id: 40,
      source_id: 'pid-a',
    });
    handleTranscript({
      speaker: 'Bob',
      text: 'bob хвост',
      timestamp: '10:00:04',
      is_continuation: true,
      segment_id: 41,
      source_id: 'pid-b',
    });

    expect(state.transcriptLines).toHaveLength(2);
    expect(state.transcriptLines[0].segment_ids).toEqual([40]);
    expect(state.transcriptLines[0].segment_ids).not.toContain(41);
    expect(state.transcriptLines[1].segment_ids).toEqual([41]);
    expect(state.transcriptLines[1].segment_ids).toContain(41);

    handleTranscriptRefine({
      updates: [{ segment_id: 41, text: 'refined bob хвост' }],
    });

    // Bob's bubble rewritten.
    const bobEl = document.querySelector('.ktalk-msg[data-segment-id="41"] .ktalk-msg-text');
    expect(bobEl).not.toBeNull();
    expect(bobEl.innerHTML).toBe('refined bob хвост');

    // Alice's bubble untouched.
    const aliceEl = document.querySelector('.ktalk-msg[data-segment-id="40"] .ktalk-msg-text');
    expect(aliceEl).not.toBeNull();
    expect(aliceEl.innerHTML).toBe('alice начало');

    // State mirrors DOM.
    expect(state.transcriptLines[0].text).toBe('alice начало');
    expect(state.transcriptLines[0].refined).toBeFalsy();
    expect(state.transcriptLines[1].text).toBe('refined bob хвост');
    expect(state.transcriptLines[1].refined).toBe(true);
  });

  test('handleTranscript pushes new line when continuation source_id differs', () => {
    // Two different PCM sources (pid-a, pid-b) both labelled "Alice" — the
    // backend may attach the same speaker name to two physical pids when an
    // operator reuses the name. Without source_id discrimination the second
    // chunk would be merged into Alice's first bubble and contaminate her
    // segment_ids/segment_texts; with source_id the bubbles stay separate.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    handleTranscript({
      speaker: 'Alice',
      text: 'first source',
      timestamp: '10:00:01',
      is_continuation: false,
      segment_id: 20,
      source_id: 'pid-a',
    });
    handleTranscript({
      speaker: 'Alice',
      text: 'second source',
      timestamp: '10:00:04',
      is_continuation: true,
      segment_id: 21,
      source_id: 'pid-b',
    });

    expect(state.transcriptLines).toHaveLength(2);
    expect(state.transcriptLines[0].segment_ids).toEqual([20]);
    expect(state.transcriptLines[1].segment_ids).toEqual([21]);
    expect(state.transcriptLines[0].source_id).toBe('pid-a');
    expect(state.transcriptLines[1].source_id).toBe('pid-b');
    expect(state.transcriptLines[0].text).toBe('first source');
    expect(state.transcriptLines[1].text).toBe('second source');
  });

  test('handleTranscript merges continuation when source_id matches even if speaker label changes', () => {
    // Same physical PCM stream (pid-a) but the extension renamed the
    // speaker mid-turn ("Alice" → "Alice Renamed" via roster update). The
    // continuation merge must follow the stable source_id, not the volatile
    // speaker label, otherwise rename mid-turn would split a single utterance.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    handleTranscript({
      speaker: 'Alice',
      text: 'часть один',
      timestamp: '10:00:01',
      is_continuation: false,
      segment_id: 22,
      source_id: 'pid-a',
    });
    handleTranscript({
      speaker: 'Alice Renamed',
      text: 'часть два',
      timestamp: '10:00:04',
      is_continuation: true,
      segment_id: 23,
      source_id: 'pid-a',
    });

    expect(state.transcriptLines).toHaveLength(1);
    expect(state.transcriptLines[0].segment_ids).toEqual([22, 23]);
    expect(state.transcriptLines[0].text).toBe('часть один часть два');
    expect(state.transcriptLines[0].segment_texts).toEqual({
      22: 'часть один',
      23: 'часть два',
    });
  });

  test('handleTranscript legacy fallback merges by speaker when source_id absent', () => {
    // Pre-source_id payloads (legacy bridge) — predicate falls back to
    // speaker equality. Same speaker → merge; different speaker → push new.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    handleTranscript({
      speaker: 'Alice',
      text: 'one',
      timestamp: '10:00:01',
      is_continuation: false,
      segment_id: 50,
    });
    handleTranscript({
      speaker: 'Alice',
      text: 'two',
      timestamp: '10:00:04',
      is_continuation: true,
      segment_id: 51,
    });

    expect(state.transcriptLines).toHaveLength(1);
    expect(state.transcriptLines[0].segment_ids).toEqual([50, 51]);
    expect(state.transcriptLines[0].text).toBe('one two');

    handleTranscript({
      speaker: 'Bob',
      text: 'three',
      timestamp: '10:00:07',
      is_continuation: true,
      segment_id: 52,
    });

    expect(state.transcriptLines).toHaveLength(2);
    expect(state.transcriptLines[1].speaker).toBe('Bob');
    expect(state.transcriptLines[1].segment_ids).toEqual([52]);
    expect(state.transcriptLines[1].text).toBe('three');
  });
});

describe('updateTranscriptUI scroll anchor', () => {
  // jsdom does not compute layout, so offsetTop returns 0 for every element by
  // default. We install a prototype-level getter that derives offsetTop from
  // the element's position among its parent's `.ktalk-msg` children
  // (idx * ROW_H). This survives innerHTML rewrites — newly-rendered elements
  // pick up the correct layout automatically. Tests that need a different
  // offset for a specific element shadow the getter via per-element
  // Object.defineProperty (this still works because the per-element descriptor
  // takes precedence over the prototype's).
  const ROW_H = 50;
  const originalOffsetTopDescriptor = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype, 'offsetTop'
  );

  beforeAll(() => {
    Object.defineProperty(HTMLElement.prototype, 'offsetTop', {
      configurable: true,
      get() {
        const parent = this.parentNode;
        if (!parent || !this.classList || !this.classList.contains('ktalk-msg')) return 0;
        const siblings = parent.querySelectorAll(':scope > .ktalk-msg');
        for (let i = 0; i < siblings.length; i++) {
          if (siblings[i] === this) return i * ROW_H;
        }
        return 0;
      },
    });
  });

  afterAll(() => {
    if (originalOffsetTopDescriptor) {
      Object.defineProperty(HTMLElement.prototype, 'offsetTop', originalOffsetTopDescriptor);
    } else {
      delete HTMLElement.prototype.offsetTop;
    }
  });

  function stampScrollMetrics(container, scrollHeight, clientHeight = 100) {
    Object.defineProperty(container, 'scrollHeight', {
      get: () => scrollHeight,
      configurable: true,
    });
    Object.defineProperty(container, 'clientHeight', {
      get: () => clientHeight,
      configurable: true,
    });
  }

  // Real Chrome resets scrollTop=0 when innerHTML is reassigned on the
  // container. jsdom does not, so the helper-restore code path silently
  // appears to "work" via the unmodified value. Wrap the container's innerHTML
  // setter to simulate Chrome behaviour: any assignment that produces new
  // markup resets scrollTop to 0 immediately. The helper is then the ONLY way
  // scrollTop returns to its captured value, which is what the assertions
  // actually verify.
  function simulateChromeScrollReset(container) {
    const proto = Object.getPrototypeOf(container);
    const desc = Object.getOwnPropertyDescriptor(proto, 'innerHTML') ||
      Object.getOwnPropertyDescriptor(Element.prototype, 'innerHTML');
    Object.defineProperty(container, 'innerHTML', {
      configurable: true,
      get() { return desc.get.call(this); },
      set(v) {
        desc.set.call(this, v);
        this.scrollTop = 0;
      },
    });
  }

  beforeEach(() => {
    document.body.innerHTML = '';
    state.transcriptLines = [];
    state.bioRead = { enabled: false };
    state.currentMeetingId = null;
    state.autoScrollEnabled = true; // reset to default; flip per-test
    isJitsiPlatformMock.mockReturnValue(false);
  });

  test('paused-mode preserves top-visible row across re-render when segment_id present', () => {
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;

    for (let i = 0; i < 5; i++) {
      handleTranscript({
        speaker: `Speaker${i}`,
        text: `text ${i}`,
        timestamp: `10:00:0${i}`,
        is_continuation: false,
        segment_id: i + 1,
        source_id: `pid-${i}`,
      });
    }
    expect(state.transcriptLines).toHaveLength(5);

    const container = document.querySelector('.ktalk-transcript-messages');
    expect(container.querySelectorAll('.ktalk-msg')).toHaveLength(5);

    // Switch to Paused mode and seed scroll position in the middle of the
    // list. With offsets [0, 50, 100, 150, 200] and scrollTop=120,
    // top - 1 = 119 → first element with offsetTop >= 119 is idx=3
    // (offsetTop=150, segment_id=4). Intentionally NOT the first element —
    // degenerate offsetTop=0 fallback would pick idx=0.
    state.autoScrollEnabled = false;
    stampScrollMetrics(container, 250);
    container.scrollTop = 120;
    simulateChromeScrollReset(container);

    const anchorBefore = container.querySelector('.ktalk-msg[data-segment-id="4"]');
    expect(anchorBefore).not.toBeNull();
    expect(anchorBefore.offsetTop).toBe(150);

    // Unshift NewSpeaker, then push another via continuation with a different
    // source_id (forces push, not merge). After re-render, segment_id=4 row
    // shifts from idx=3 to idx=4 → offsetTop 150 → 200, distinguishing the
    // compensation math from a degenerate 0+0 case.
    state.transcriptLines.unshift({
      speaker: 'NewSpeaker',
      text: 'inserted at top',
      timestamp: '09:59:59',
      time: '09:59:59',
      segment_id: 99,
      source_id: 'pid-new',
      segment_ids: [99],
      segment_texts: { 99: 'inserted at top' },
      is_local: false,
      uncertain: false,
      uncertain_reason: '',
      createdAt: Date.now(),
    });

    handleTranscript({
      speaker: 'NewSpeaker',
      text: '',
      timestamp: '09:59:59',
      is_continuation: true,
      segment_id: 100,
      source_id: 'pid-new',
    });

    const anchorAfter = container.querySelector('.ktalk-msg[data-segment-id="4"]');
    expect(anchorAfter).not.toBeNull();
    expect(anchorAfter.offsetTop).toBe(200);

    // scrollTop = oldScrollTop + (newOffsetTop - oldOffsetTop)
    //           = 120          + (200            - 150) = 170.
    expect(Math.abs(container.scrollTop - 170)).toBeLessThanOrEqual(1);
    expect(anchorAfter.offsetTop).toBeGreaterThan(0);
  });

  test('Follow-mode keeps sticky bottom across re-render', () => {
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;

    state.autoScrollEnabled = true;
    for (let i = 0; i < 5; i++) {
      handleTranscript({
        speaker: `Speaker${i}`,
        text: `text ${i}`,
        timestamp: `10:00:0${i}`,
        is_continuation: false,
        segment_id: i + 1,
        source_id: `pid-${i}`,
      });
    }

    const container = document.querySelector('.ktalk-transcript-messages');
    stampScrollMetrics(container, 300);
    container.scrollTop = 150;

    handleTranscript({
      speaker: 'Speaker5',
      text: 'text 5',
      timestamp: '10:00:05',
      is_continuation: false,
      segment_id: 6,
      source_id: 'pid-5',
    });
    expect(state.transcriptLines).toHaveLength(6);

    // Sticky-bottom contract: scrollTop === scrollHeight (300) after re-render.
    expect(container.scrollHeight).toBe(300);
    expect(container.scrollTop).toBe(container.scrollHeight);
  });

  test('Follow-mode + not-near-bottom does NOT restore anchor (documents pre-existing behavior)', () => {
    // F4 coverage: when autoScrollEnabled=true but the user has scrolled away
    // from the bottom (isNearBottom returns false), shouldAutoScroll is false
    // and captureScrollAnchor returns null in Follow mode — so neither branch
    // restores scrollTop. simulateChromeScrollReset replicates real Chrome's
    // post-innerHTML behaviour, so a passing assertion of `scrollTop === 0`
    // pins that the helper genuinely did not engage.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    state.autoScrollEnabled = true;
    for (let i = 0; i < 5; i++) {
      handleTranscript({
        speaker: `Speaker${i}`,
        text: `text ${i}`,
        timestamp: `10:00:0${i}`,
        is_continuation: false,
        segment_id: i + 1,
        source_id: `pid-${i}`,
      });
    }

    const container = document.querySelector('.ktalk-transcript-messages');
    // scrollHeight=10000, scrollTop=42, clientHeight=100 → distance = 9858 → not near bottom.
    stampScrollMetrics(container, 10000);
    container.scrollTop = 42;
    simulateChromeScrollReset(container);

    handleTranscript({
      speaker: 'Speaker5',
      text: 'new line',
      timestamp: '10:00:05',
      is_continuation: false,
      segment_id: 6,
      source_id: 'pid-5',
    });

    // Helper does not engage in Follow mode (capture returns null) and the
    // Follow-branch only sticks to bottom when isNearBottom is true. Chrome
    // simulator already reset scrollTop to 0 after innerHTML; nothing
    // re-applied a value, so it stays at 0.
    expect(container.scrollTop).toBe(0);
  });

  test('Paused-mode preserves scrollTop when anchor element disappears', () => {
    // F5 coverage: capture an anchor, then completely replace transcriptLines
    // (e.g. meeting_change / history reload) with rows whose segment_ids do
    // not overlap. restoreScrollAnchor's lookup returns null → fallback to
    // anchor.top, preserving the captured scrollTop.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    for (let i = 0; i < 5; i++) {
      handleTranscript({
        speaker: `Speaker${i}`,
        text: `text ${i}`,
        timestamp: `10:00:0${i}`,
        is_continuation: false,
        segment_id: i + 1,
        source_id: `pid-${i}`,
      });
    }

    const container = document.querySelector('.ktalk-transcript-messages');
    state.autoScrollEnabled = false;
    stampScrollMetrics(container, 250);
    container.scrollTop = 120;
    simulateChromeScrollReset(container);

    // Wipe and replace with a fresh set; segment_id deliberately disjoint
    // from any previous row.
    state.transcriptLines = [];
    handleTranscript({
      speaker: 'Other',
      text: 'fresh line',
      timestamp: '11:00:00',
      is_continuation: false,
      segment_id: 999,
      source_id: 'pid-other',
    });

    // Captured anchor.segmentId='4' is gone after replacement →
    // restoreScrollAnchor falls through to `container.scrollTop = anchor.top`.
    // Chrome simulator zeroed scrollTop after innerHTML rewrite, so the
    // assertion only passes if the helper actively restored 120.
    expect(Math.abs(container.scrollTop - 120)).toBeLessThanOrEqual(1);
  });

  test('Paused-mode does not synthesize a position when scrolled past last row', () => {
    // F6 coverage: when scrollTop exceeds every row's offsetTop, no element
    // satisfies offsetTop >= scrollTop - 1 → captureScrollAnchor returns null
    // and updateTranscriptUI does NOT call restoreScrollAnchor. Contract:
    // no crash, helper genuinely does not engage. After Chrome reset zeroes
    // scrollTop post-innerHTML, nothing re-applies a value, so it stays at 0.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    for (let i = 0; i < 5; i++) {
      handleTranscript({
        speaker: `Speaker${i}`,
        text: `text ${i}`,
        timestamp: `10:00:0${i}`,
        is_continuation: false,
        segment_id: i + 1,
        source_id: `pid-${i}`,
      });
    }

    const container = document.querySelector('.ktalk-transcript-messages');
    state.autoScrollEnabled = false;
    stampScrollMetrics(container, 250);
    container.scrollTop = 9999;
    simulateChromeScrollReset(container);

    expect(() => handleTranscript({
      speaker: 'Speaker5',
      text: 'text 5',
      timestamp: '10:00:05',
      is_continuation: false,
      segment_id: 6,
      source_id: 'pid-5',
    })).not.toThrow();

    // Helper did not engage (captureScrollAnchor returned null because no
    // anchor row matched). Chrome simulator already zeroed scrollTop.
    expect(container.scrollTop).toBe(0);
  });

  test('Paused-mode preserves scrollTop when anchor row has no data-segment-id', () => {
    // Coverage gap: rows without a valid backend segment_id (e.g. history rows
    // where segment_id=0) render WITHOUT data-segment-id. captureScrollAnchor
    // must produce an anchor with segmentId=null (the `> 0` guard kicks in),
    // and restoreScrollAnchor must fall back to anchor.top. A regression that
    // drops the `> 0` guard would silently match `[data-segment-id="0"]` and
    // break this contract.
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    for (let i = 0; i < 5; i++) {
      handleTranscript({
        speaker: `Speaker${i}`,
        text: `text ${i}`,
        timestamp: `10:00:0${i}`,
        is_continuation: false,
        segment_id: 0,
        source_id: `pid-${i}`,
      });
    }

    const container = document.querySelector('.ktalk-transcript-messages');
    expect(container.querySelectorAll('.ktalk-msg')).toHaveLength(5);
    // Render template emits data-segment-id only when segment_id > 0.
    container.querySelectorAll('.ktalk-msg').forEach(el => {
      expect(el.hasAttribute('data-segment-id')).toBe(false);
    });

    state.autoScrollEnabled = false;
    stampScrollMetrics(container, 250);
    container.scrollTop = 120;
    simulateChromeScrollReset(container);

    handleTranscript({
      speaker: 'Speaker5',
      text: 'text 5',
      timestamp: '10:00:05',
      is_continuation: false,
      segment_id: 0,
      source_id: 'pid-5',
    });

    // Anchor has segmentId=null → restoreScrollAnchor sets scrollTop=anchor.top.
    // Without the helper, Chrome simulator would leave scrollTop=0.
    expect(Math.abs(container.scrollTop - 120)).toBeLessThanOrEqual(1);
  });

  test('Paused-mode does not crash on empty transcriptLines', () => {
    document.body.innerHTML = `<div class="ktalk-transcript-messages"></div>`;
    state.autoScrollEnabled = false;
    state.transcriptLines = [];

    expect(() => handleNewMeeting({ meeting_id: 'meeting-empty' })).not.toThrow();

    const container = document.querySelector('.ktalk-transcript-messages');
    expect(container.querySelector('.ktalk-transcript-empty')).not.toBeNull();
  });

  test('paused-mode preserves anchor across refine batch when bubble above grows', () => {
    // Refine integration: handleTranscriptRefine rewrites bubble text in place.
    // When a refined bubble ABOVE the anchor grows (longer text → taller bubble),
    // browsers reflow and shift everything below it. We simulate that reflow by
    // shadowing the anchor row's offsetTop with a per-element getter that
    // returns the GROWN offset only on its second read. The first read happens
    // inside captureScrollAnchor (before the in-place rewrite); the second read
    // happens inside restoreScrollAnchor (after the rewrite, when the simulated
    // reflow has already happened). This is deterministic on the specific
    // anchor element and does not leak into other tests because it is an own
    // property — not a prototype mutation.
    state.autoScrollEnabled = false;
    state.transcriptLines = [
      { speaker: 'A', text: 'a', time: '10:00:01', segment_id: 11, segment_ids: [11], segment_texts: { 11: 'a' } },
      { speaker: 'B', text: 'b', time: '10:00:02', segment_id: 12, segment_ids: [12], segment_texts: { 12: 'b' } },
      { speaker: 'C', text: 'c', time: '10:00:03', segment_id: 13, segment_ids: [13], segment_texts: { 13: 'c' } },
      { speaker: 'D', text: 'd', time: '10:00:04', segment_id: 14, segment_ids: [14], segment_texts: { 14: 'd' } },
      { speaker: 'E', text: 'e', time: '10:00:05', segment_id: 15, segment_ids: [15], segment_texts: { 15: 'e' } },
    ];
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages">
        <div class="ktalk-msg remote" data-segment-id="11"><div class="ktalk-msg-text">a</div></div>
        <div class="ktalk-msg remote" data-segment-id="12"><div class="ktalk-msg-text">b</div></div>
        <div class="ktalk-msg remote" data-segment-id="13"><div class="ktalk-msg-text">c</div></div>
        <div class="ktalk-msg remote" data-segment-id="14"><div class="ktalk-msg-text">d</div></div>
        <div class="ktalk-msg remote" data-segment-id="15"><div class="ktalk-msg-text">e</div></div>
      </div>
    `;

    const container = document.querySelector('.ktalk-transcript-messages');
    // Default prototype getter gives offsets [0, 50, 100, 150, 200];
    // anchor (sid=13) is at idx=2 / 100. We override it on the anchor element
    // only with a 2-phase getter: captureScrollAnchor reads it twice (once in
    // the search loop, once when constructing the return) and must see 100;
    // restoreScrollAnchor reads it once afterwards and must see 200 (the
    // simulated post-reflow position).
    const anchorEl = container.querySelector('.ktalk-msg[data-segment-id="13"]');
    let anchorReadCount = 0;
    Object.defineProperty(anchorEl, 'offsetTop', {
      configurable: true,
      get() {
        anchorReadCount += 1;
        return anchorReadCount <= 2 ? 100 : 200;
      },
    });

    stampScrollMetrics(container, 250);
    container.scrollTop = 100;

    handleTranscriptRefine({
      type: 'transcript_refine',
      updates: [
        { segment_id: 11, text: 'aaa очень длинный текст' },
        { segment_id: 14, text: 'ddd более длинный текст' },
      ],
    });

    // scrollTop = 100 + (200 - 100) = 200.
    expect(Math.abs(container.scrollTop - 200)).toBeLessThanOrEqual(1);

    expect(document.querySelector('.ktalk-msg[data-segment-id="11"] .ktalk-msg-text').innerHTML)
      .toBe('aaa очень длинный текст');
    expect(document.querySelector('.ktalk-msg[data-segment-id="14"] .ktalk-msg-text').innerHTML)
      .toBe('ddd более длинный текст');
  });

  test('Follow-mode refine does not move scrollTop', () => {
    // F9 coverage: handleTranscriptRefine now calls capture+restore
    // unconditionally inside `if (container)`. In Follow mode capture returns
    // null and restore is a no-op, so scrollTop must be unchanged by refine.
    state.autoScrollEnabled = true;
    state.transcriptLines = [
      { speaker: 'A', text: 'a', time: '10:00:01', segment_id: 11, segment_ids: [11], segment_texts: { 11: 'a' } },
      { speaker: 'B', text: 'b', time: '10:00:02', segment_id: 12, segment_ids: [12], segment_texts: { 12: 'b' } },
    ];
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages">
        <div class="ktalk-msg remote" data-segment-id="11"><div class="ktalk-msg-text">a</div></div>
        <div class="ktalk-msg remote" data-segment-id="12"><div class="ktalk-msg-text">b</div></div>
      </div>
    `;

    const container = document.querySelector('.ktalk-transcript-messages');
    stampScrollMetrics(container, 100);
    container.scrollTop = 42;

    handleTranscriptRefine({
      type: 'transcript_refine',
      updates: [{ segment_id: 11, text: 'refined' }],
    });

    expect(container.scrollTop).toBe(42);
    expect(document.querySelector('.ktalk-msg[data-segment-id="11"] .ktalk-msg-text').innerHTML)
      .toBe('refined');
  });

  test('Paused-mode refine with no matching segment_id preserves scrollTop', () => {
    // F10 coverage: refine batch where no update matches an existing line —
    // touchedLines is empty, the for-loop is a no-op, but capture+restore
    // still run. Without compensation needed (no DOM mutation), scrollTop must
    // stay at the captured value.
    state.autoScrollEnabled = false;
    state.transcriptLines = [
      { speaker: 'A', text: 'a', time: '10:00:01', segment_id: 11, segment_ids: [11], segment_texts: { 11: 'a' } },
      { speaker: 'B', text: 'b', time: '10:00:02', segment_id: 12, segment_ids: [12], segment_texts: { 12: 'b' } },
    ];
    document.body.innerHTML = `
      <div class="ktalk-transcript-messages">
        <div class="ktalk-msg remote" data-segment-id="11"><div class="ktalk-msg-text">a</div></div>
        <div class="ktalk-msg remote" data-segment-id="12"><div class="ktalk-msg-text">b</div></div>
      </div>
    `;

    const container = document.querySelector('.ktalk-transcript-messages');
    stampScrollMetrics(container, 100);
    container.scrollTop = 50;

    // segment_id=999 references nothing — touchedLines stays empty.
    handleTranscriptRefine({
      type: 'transcript_refine',
      updates: [{ segment_id: 999, text: 'orphan' }],
    });

    expect(container.scrollTop).toBe(50);
    expect(document.querySelector('.ktalk-msg[data-segment-id="11"] .ktalk-msg-text').innerHTML)
      .toBe('a');
  });
});
