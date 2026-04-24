/**
 * Tests for handleTranscriptRefine — tier-2 in-place bubble rewrite.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://ktalk.ru/"}
 */

import { jest } from '@jest/globals';

let state;
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
  ({ handleTranscriptRefine } = await import('./ui.js'));
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
});
