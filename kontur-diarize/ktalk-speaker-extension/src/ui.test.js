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
  ({ handleTranscript, handleTranscriptRefine } = await import('./ui.js'));
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
