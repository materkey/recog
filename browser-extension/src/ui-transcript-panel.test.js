/**
 * The transcript view: live frames, speaker corrections, history reloads and
 * the KTalk toolbar (copy, privacy, follow, BioRead).
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://ktalk.ru/"}
 */

import { jest } from '@jest/globals';

let state;
let constants;
let ui;
let vas3k;

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

  ({ state, constants } = await import('./state.js'));
  ui = await import('./ui.js');
  vas3k = await import('./ui-vas3k.js');
});

function resetState() {
  document.body.innerHTML = '';
  state.transcriptLines = [];
  state.transcriptContentElement = null;
  state.copilotContentElement = null;
  state.originalChatContent = null;
  state.currentMeetingId = null;
  state.autoScrollEnabled = true;
  state.privacyMode = false;
  state.bioRead = {
    enabled: false,
    fixation: 40,
    tailOpacity: 60,
    minWordLength: 3,
    maxHeadRatio: 0.6,
    ignoreNumbers: true,
    panelOpen: false,
  };
  localStorage.clear();
  isJitsiPlatformMock.mockReturnValue(false);
  window.dispatchEvent.mockClear();
  navigator.clipboard.writeText.mockClear();
  navigator.clipboard.writeText.mockResolvedValue(undefined);
}

function buildKtalkPanel() {
  document.body.innerHTML = `
    <div class="slide-menu-wrapper">
      <div class="content"><chat-widget class="native-chat"></chat-widget></div>
    </div>
  `;
  return ui.createTranscriptContent();
}

function lastUpdateEvent() {
  const calls = window.dispatchEvent.mock.calls
    .map((call) => call[0])
    .filter((event) => event.type === 'ktalk-transcript-update');
  return calls[calls.length - 1];
}

const bubbles = () => [...document.querySelectorAll('.ktalk-transcript-messages .ktalk-msg')];
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe('handleTranscript speaker correction', () => {
  beforeEach(resetState);

  test('an Unknown line on KTalk - a correction with the same timestamp - renames it in place', () => {
    buildKtalkPanel();
    state.transcriptLines = [
      { speaker: 'Unknown', text: 'hello world', timestamp: 'T1', time: '10:00:00', uncertain: true, uncertain_reason: 'mics_off' },
    ];

    ui.handleTranscript({ correction: true, speaker: 'Alice', text: 'hello', timestamp: 'T1' });

    expect(state.transcriptLines).toHaveLength(1);
    expect(state.transcriptLines[0]).toMatchObject({ speaker: 'Alice', uncertain: false, uncertain_reason: '' });
    expect(bubbles().map((el) => el.querySelector('.ktalk-msg-speaker').textContent)).toEqual(['Alice']);
    expect(lastUpdateEvent().detail.transcriptLines[0].speaker).toBe('Alice');
  });

  test('an uncertain correction - applied - keeps its uncertainty', () => {
    state.transcriptLines = [{ speaker: 'Unknown', text: 'hello', timestamp: 'T1' }];

    ui.handleTranscript({
      correction: true, speaker: 'Bob', text: 'hello', timestamp: 'T1', uncertain: true, uncertain_reason: 'mics_off',
    });

    expect(state.transcriptLines[0]).toMatchObject({ speaker: 'Bob', uncertain: true, uncertain_reason: 'mics_off' });
  });

  test('no Unknown line matches - a correction - changes and appends nothing', () => {
    state.transcriptLines = [
      { speaker: 'Unknown', text: 'hello', timestamp: 'T2' },
      { speaker: 'Carol', text: 'hello', timestamp: 'T1' },
      { speaker: 'Unknown', text: 'other', timestamp: 'T1' },
    ];

    ui.handleTranscript({ correction: true, speaker: 'Alice', text: 'hello', timestamp: 'T1' });

    expect(state.transcriptLines.map((line) => line.speaker)).toEqual(['Unknown', 'Carol', 'Unknown']);
    expect(lastUpdateEvent()).toBeDefined();
  });

  test('the floating panel - a correction - renames the matching Unknown line and drops its marks', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    document.body.innerHTML = `
      <div class="jitsi-transcript-content">
        <div class="jitsi-transcript-line" data-timestamp="T0">
          <span class="jitsi-transcript-speaker">Unknown</span>
          <div class="jitsi-transcript-text">hello earlier</div>
        </div>
        <div class="jitsi-transcript-line" data-timestamp="T1">
          <span class="jitsi-transcript-speaker">Unknown</span>
          <div class="jitsi-transcript-text jitsi-transcript-uncertain">hello world (🔇?)</div>
        </div>
      </div>
    `;
    state.transcriptLines = [{ speaker: 'Unknown', text: 'hello world', timestamp: 'T1' }];

    ui.handleTranscript({ correction: true, speaker: 'Alice', text: 'hello', timestamp: 'T1' });

    const [first, second] = document.querySelectorAll('.jitsi-transcript-line');
    expect(first.querySelector('.jitsi-transcript-speaker').textContent).toBe('Unknown');
    expect(second.querySelector('.jitsi-transcript-speaker').textContent).toBe('Alice');
    const text = second.querySelector('.jitsi-transcript-text');
    expect(text.textContent).toBe('hello world');
    expect(text.classList.contains('jitsi-transcript-uncertain')).toBe(false);
  });

  test('the floating panel - a correction without timestamp - renames the first Unknown line starting with the text', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    document.body.innerHTML = `
      <div class="jitsi-transcript-content">
        <div class="jitsi-transcript-line">
          <span class="jitsi-transcript-speaker">Dave</span>
          <div class="jitsi-transcript-text">hello there</div>
        </div>
        <div class="jitsi-transcript-line">
          <span class="jitsi-transcript-speaker">Unknown</span>
          <div class="jitsi-transcript-text">bye</div>
        </div>
        <div class="jitsi-transcript-line">
          <span class="jitsi-transcript-speaker">Unknown</span>
          <div class="jitsi-transcript-text">hello again (?)</div>
        </div>
        <div class="jitsi-transcript-line">
          <span class="jitsi-transcript-speaker">Unknown</span>
          <div class="jitsi-transcript-text">hello last</div>
        </div>
      </div>
    `;

    ui.handleTranscript({ correction: true, speaker: 'Alice', text: 'hello' });

    const speakers = [...document.querySelectorAll('.jitsi-transcript-speaker')].map((el) => el.textContent);
    expect(speakers).toEqual(['Dave', 'Unknown', 'Alice', 'Unknown']);
    expect(document.querySelectorAll('.jitsi-transcript-text')[2].textContent).toBe('hello again');
  });

  test('an uncertain line rendered by the floating panel - a correction - drops its mark', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    vas3k.createFloatingPanel();
    ui.handleTranscript({ speaker: 'Unknown', text: 'hello world', timestamp: 'T1', uncertain: true });
    ui.handleTranscript({
      speaker: 'Unknown', text: 'muted words', timestamp: 'T2', uncertain: true, uncertain_reason: 'mics_off',
    });

    ui.handleTranscript({ correction: true, speaker: 'Alice', text: 'hello', timestamp: 'T1' });
    ui.handleTranscript({ correction: true, speaker: 'Bob', text: 'muted', timestamp: 'T2' });

    const texts = [...document.querySelectorAll('.jitsi-transcript-text')].map((el) => el.textContent);
    expect(texts).toEqual(['hello world', 'muted words']);
  });

  test('the floating panel is missing - a correction - updates state only', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    state.transcriptLines = [{ speaker: 'Unknown', text: 'hello', timestamp: 'T1' }];

    ui.handleTranscript({ correction: true, speaker: 'Alice', text: 'hello', timestamp: 'T1' });

    expect(state.transcriptLines[0].speaker).toBe('Alice');
    expect(lastUpdateEvent().detail.transcriptLines).toHaveLength(1);
  });
});

describe('handleTranscript live lines', () => {
  beforeEach(resetState);

  test('a restored legacy line - a continuation from the same speaker - backfills its segment tracking', () => {
    state.transcriptLines = [{ speaker: 'Alice', text: 'one', segment_id: 5, source_id: '' }];

    ui.handleTranscript({ speaker: 'Alice', text: 'two', is_continuation: true, segment_id: 6 });

    expect(state.transcriptLines).toHaveLength(1);
    expect(state.transcriptLines[0]).toMatchObject({
      text: 'one two', segment_ids: [5, 6], segment_texts: { 5: 'one', 6: 'two' },
    });
  });

  test('a restored line without a segment id - a continuation - tracks only the new segment', () => {
    state.transcriptLines = [{ speaker: 'Alice', text: 'one', segment_id: 0, source_id: '' }];

    ui.handleTranscript({ speaker: 'Alice', text: 'two', is_continuation: true, segment_id: 6 });

    expect(state.transcriptLines[0]).toMatchObject({ segment_ids: [6], segment_texts: { 6: 'two' } });
  });

  test('a continuation without a segment id - merged - leaves the tracking alone', () => {
    ui.handleTranscript({ speaker: 'Alice', text: 'one', segment_id: 3, source_id: 'pid-1' });
    ui.handleTranscript({ speaker: 'Alice', text: 'two', is_continuation: true, source_id: 'pid-1' });

    expect(state.transcriptLines).toHaveLength(1);
    expect(state.transcriptLines[0]).toMatchObject({ text: 'one two', segment_ids: [3], segment_texts: { 3: 'one' } });
  });

  test('a first line - received - carries the frame fields with defaults', () => {
    ui.handleTranscript({ speaker: 'Alice', text: 'hi', timestamp: 'T1', source_id: 42 });

    expect(state.transcriptLines[0]).toMatchObject({
      speaker: 'Alice', text: 'hi', timestamp: 'T1', segment_id: 0, source_id: '',
      segment_ids: [], segment_texts: {}, is_local: false, uncertain: false, uncertain_reason: '',
    });
  });

  test('a local uncertain line - received - keeps the flags', () => {
    ui.handleTranscript({ speaker: 'Me', text: 'hi', is_local: true, uncertain: true, uncertain_reason: 'mics_off', segment_id: 9 });

    expect(state.transcriptLines[0]).toMatchObject({
      is_local: true, uncertain: true, uncertain_reason: 'mics_off', segment_ids: [9], segment_texts: { 9: 'hi' },
    });
  });

  test('a full transcript - one more line - drops the oldest', () => {
    state.transcriptLines = Array.from({ length: constants.MAX_TRANSCRIPT_LINES }, (_, i) => ({ speaker: 'S', text: `t${i}`, source_id: `s${i}` }));

    ui.handleTranscript({ speaker: 'New', text: 'last', source_id: 'new' });

    expect(state.transcriptLines).toHaveLength(constants.MAX_TRANSCRIPT_LINES);
    expect(state.transcriptLines[0].text).toBe('t1');
    expect(state.transcriptLines[state.transcriptLines.length - 1].text).toBe('last');
  });

  test('the floating panel - a line - is appended to the panel', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    vas3k.createFloatingPanel();

    ui.handleTranscript({ speaker: 'Alice', text: 'hi', segment_id: 4 });

    const line = document.querySelector('.jitsi-transcript-line[data-segment-id="4"]');
    expect(line.querySelector('.jitsi-transcript-speaker').textContent).toBe('Alice');
  });
});

describe('handleTranscriptHistory and handleNewMeeting', () => {
  beforeEach(resetState);

  test('a history frame without segments - received - records the meeting and keeps the lines', () => {
    state.transcriptLines = [{ speaker: 'A', text: 'kept' }];

    ui.handleTranscriptHistory({ meeting_id: 'm1', segments: [] });
    ui.handleTranscriptHistory({});

    expect(state.currentMeetingId).toBe('m1');
    expect(state.transcriptLines).toEqual([{ speaker: 'A', text: 'kept' }]);
  });

  test('a sparse history segment - restored - fills the defaults', () => {
    ui.handleTranscriptHistory({ segments: [{ speaker: 'A', text: 'x', ts: '2026-01-01T10:00:00Z' }] });

    expect(state.transcriptLines[0]).toMatchObject({
      speaker: 'A', text: 'x', segment_id: 0, source_id: '', privacy_id: '',
      is_local: false, uncertain: false, uncertain_reason: '',
      createdAt: Date.parse('2026-01-01T10:00:00Z'),
    });
    expect(state.transcriptLines[0].time).toMatch(/\d\d:\d\d:\d\d/);
  });

  test('a full history segment - restored - keeps every field', () => {
    ui.handleTranscriptHistory({
      segments: [{
        id: 7, speaker: 'A', text: 'x', timestamp: 'T', ts_display: '10:00:01', source_id: 'pid',
        speaker_id: 'sp', is_local: true, uncertain: true, uncertain_reason: 'mics_off',
      }],
    });

    expect(state.transcriptLines[0]).toMatchObject({
      segment_id: 7, timestamp: 'T', time: '10:00:01', source_id: 'pid', privacy_id: 'pid',
      is_local: true, uncertain: true, uncertain_reason: 'mics_off',
    });
  });

  test('a history segment with a display time only - restored - takes it as the timestamp', () => {
    const before = Date.now();
    ui.handleTranscriptHistory({ segments: [{ speaker: 'A', text: 'x', ts_display: '10:00:01', speaker_id: 'sp' }] });

    expect(state.transcriptLines[0]).toMatchObject({ timestamp: '10:00:01', privacy_id: 'sp' });
    expect(state.transcriptLines[0].createdAt).toBeGreaterThanOrEqual(before);
  });

  test('a history longer than the cap - restored - keeps the newest lines', () => {
    const segments = Array.from({ length: constants.MAX_TRANSCRIPT_LINES + 2 }, (_, i) => ({ speaker: 'A', text: `t${i}`, ts_display: 'x' }));

    ui.handleTranscriptHistory({ segments });

    expect(state.transcriptLines).toHaveLength(constants.MAX_TRANSCRIPT_LINES);
    expect(state.transcriptLines[0].text).toBe('t2');
  });

  test('the floating panel - history restored - replaces the panel lines', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    vas3k.createFloatingPanel();
    vas3k.addVas3kTranscriptLine({ speaker: 'Old', text: 'gone' });

    ui.handleTranscriptHistory({
      segments: [
        { speaker: 'A', text: 'one', ts_display: 'x' },
        { speaker: 'B', text: 'two', ts_display: 'y' },
      ],
    });

    const speakers = [...document.querySelectorAll('.jitsi-transcript-speaker')].map((el) => el.textContent);
    expect(speakers).toEqual(['A', 'B']);
    expect(lastUpdateEvent().detail.transcriptLines).toHaveLength(2);
  });

  test('the floating panel - a new meeting - empties the panel and the lines', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    vas3k.createFloatingPanel();
    vas3k.addVas3kTranscriptLine({ speaker: 'Old', text: 'gone' });
    state.transcriptLines = [{ speaker: 'Old', text: 'gone' }];

    ui.handleNewMeeting({ meeting_id: 'm2' });

    expect(state.currentMeetingId).toBe('m2');
    expect(state.transcriptLines).toEqual([]);
    expect(document.querySelector('.jitsi-transcript-line')).toBeNull();
    expect(lastUpdateEvent().detail.transcriptLines).toEqual([]);
  });

  test('a new meeting frame without id - received - clears the meeting id', () => {
    state.currentMeetingId = 'm1';

    ui.handleNewMeeting({});

    expect(state.currentMeetingId).toBeNull();
  });
});

describe('handleTranscriptRefine edge frames', () => {
  beforeEach(resetState);

  test('a frame without an update list - received - changes nothing', () => {
    state.transcriptLines = [{ speaker: 'A', text: 'x', segment_id: 1 }];

    ui.handleTranscriptRefine(null);
    ui.handleTranscriptRefine({ updates: 'x' });
    ui.handleTranscriptRefine({ updates: [null] });

    expect(state.transcriptLines[0].text).toBe('x');
    expect(lastUpdateEvent()).toBeUndefined();
  });

  test('a refine for a line without child tracking - received - replaces its text', () => {
    state.transcriptLines = [{ speaker: 'A', text: 'old', segment_id: 1, segment_ids: [] , segment_texts: {} }];

    ui.handleTranscriptRefine({ updates: [{ segment_id: 1, text: 'new' }] });

    expect(state.transcriptLines[0]).toMatchObject({ text: 'new', refined: true });
  });

  test('a merged line with an empty child - refined - joins the non-empty texts', () => {
    state.transcriptLines = [{ speaker: 'A', text: 'a b', segment_id: 1, segment_ids: [1, 2, 3], segment_texts: { 1: 'a', 2: 'b' } }];

    ui.handleTranscriptRefine({ updates: [{ segment_id: 2, text: 'B' }] });

    expect(state.transcriptLines[0].text).toBe('a B');
  });

  test('the floating panel is missing or has no text element - refined - updates state only', () => {
    isJitsiPlatformMock.mockReturnValue(true);
    state.transcriptLines = [{ speaker: 'A', text: 'old', segment_id: 1 }];

    ui.handleTranscriptRefine({ updates: [{ segment_id: 1, text: 'new' }] });
    document.body.innerHTML = '<div class="jitsi-transcript-content"><div class="jitsi-transcript-line" data-segment-id="1"></div></div>';
    ui.handleTranscriptRefine({ updates: [{ segment_id: 1, text: 'newer' }] });

    expect(state.transcriptLines[0].text).toBe('newer');
    expect(document.querySelector('.jitsi-transcript-line').textContent).toBe('');
  });

  test('KTalk bubbles without a primary id or text element - refined - updates state only', () => {
    state.transcriptLines = [
      { speaker: 'A', text: 'old', segment_id: 0, segment_ids: [4], segment_texts: { 4: 'old' } },
      { speaker: 'B', text: 'old', segment_id: 5 },
      { speaker: 'C', text: 'old', segment_id: 6 },
    ];
    document.body.innerHTML = '<div class="ktalk-transcript-messages"><div class="ktalk-msg" data-segment-id="5"></div></div>';

    ui.handleTranscriptRefine({ updates: [{ segment_id: 4, text: 'a' }, { segment_id: 5, text: 'b' }, { segment_id: 6, text: 'c' }] });

    expect(state.transcriptLines.map((line) => line.text)).toEqual(['a', 'b', 'c']);
    expect(document.querySelector('.ktalk-msg').textContent).toBe('');
  });

  test('a refined floating-panel line - after the pulse - loses the fade class', () => {
    jest.useFakeTimers();
    try {
      isJitsiPlatformMock.mockReturnValue(true);
      state.transcriptLines = [{ speaker: 'A', text: 'old', segment_id: 1 }];
      document.body.innerHTML = '<div class="jitsi-transcript-content"><div class="jitsi-transcript-line" data-segment-id="1"><div class="jitsi-transcript-text">old</div></div></div>';

      ui.handleTranscriptRefine({ updates: [{ segment_id: 1, text: 'new' }] });
      const text = document.querySelector('.jitsi-transcript-text');
      expect(text.classList.contains('refine-fade')).toBe(true);
      jest.advanceTimersByTime(200);

      expect(text.classList.contains('refine-fade')).toBe(false);
      expect(text.textContent).toBe('new');
    } finally {
      jest.useRealTimers();
    }
  });
});

describe('KTalk transcript toolbar', () => {
  beforeEach(resetState);

  test('no transcript - copy clicked - copies nothing', () => {
    const content = buildKtalkPanel();

    content.querySelector('.ktalk-transcript-copy-btn').click();

    expect(navigator.clipboard.writeText).not.toHaveBeenCalled();
  });

  test('lines with uncertainty - copy clicked - copies markdown and flashes a check mark', async () => {
    jest.useFakeTimers();
    try {
      const content = buildKtalkPanel();
      state.transcriptLines = [
        { speaker: 'Alice', text: 'one', time: 't1' },
        { speaker: 'Bob', text: 'two', time: 't2', uncertain: true, uncertain_reason: 'mics_off' },
      ];
      const btn = content.querySelector('.ktalk-transcript-copy-btn');

      btn.click();
      await Promise.resolve();
      await Promise.resolve();

      expect(navigator.clipboard.writeText).toHaveBeenCalledWith('**Alice** *[t1]*: one\n\n**Bob (🔇?)** *[t2]*: two');
      expect(btn.textContent).toBe('✓');
      expect(btn.classList.contains('copied')).toBe(true);
      jest.advanceTimersByTime(2000);
      expect(btn.textContent).toBe('Copy');
      expect(btn.classList.contains('copied')).toBe(false);
    } finally {
      jest.useRealTimers();
    }
  });

  test('privacy mode switched on - copy clicked - replaces names with aliases, not system rows', async () => {
    const content = buildKtalkPanel();
    content.querySelector('.ktalk-transcript-privacy-btn').click();
    state.transcriptLines = [
      { speaker: 'Alice', text: 'one', time: 't1', source_id: 'a' },
      { speaker: 'Pause', text: 'paused', time: 't2', source_id: '__system__' },
      { speaker: 'Bob', text: 'two', time: 't3', source_id: 'b' },
      { speaker: 'Alice', text: 'three', time: 't4', source_id: 'a' },
    ];

    content.querySelector('.ktalk-transcript-copy-btn').click();
    await flush();

    expect(state.privacyMode).toBe(true);
    expect(String(localStorage.getItem('ktalk-privacy-mode'))).toBe('true');
    expect(content.querySelector('.ktalk-transcript-privacy-btn').classList.contains('active')).toBe(true);
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(
      '**speaker_0** *[t1]*: one\n\n**Pause** *[t2]*: paused\n\n**speaker_1** *[t3]*: two\n\n**speaker_0** *[t4]*: three',
    );
  });

  test('the clipboard refuses - copy clicked - keeps the button label', async () => {
    const content = buildKtalkPanel();
    state.transcriptLines = [{ speaker: 'Alice', text: 'one', time: 't1' }];
    navigator.clipboard.writeText.mockRejectedValueOnce(new Error('denied'));
    const btn = content.querySelector('.ktalk-transcript-copy-btn');

    btn.click();
    await flush();

    expect(btn.textContent).toBe('Copy');
  });

  test('the panel is gone - copy succeeds - nothing to flash', async () => {
    buildKtalkPanel();
    const btn = document.querySelector('.ktalk-transcript-copy-btn');
    state.transcriptLines = [{ speaker: 'Alice', text: 'one', time: 't1' }];
    btn.remove();

    btn.click();
    await flush();

    expect(navigator.clipboard.writeText).toHaveBeenCalledTimes(1);
  });

  test('following - follow clicked twice - pauses, then follows and scrolls to the bottom', () => {
    const content = buildKtalkPanel();
    const btn = content.querySelector('.ktalk-transcript-follow-btn');
    const container = content.querySelector('.ktalk-transcript-messages');
    Object.defineProperty(container, 'scrollHeight', { configurable: true, value: 900 });

    btn.click();
    expect(state.autoScrollEnabled).toBe(false);
    expect(btn.textContent).toBe('Paused');
    expect(btn.classList.contains('active')).toBe(false);

    container.scrollTop = 0;
    btn.click();
    expect(state.autoScrollEnabled).toBe(true);
    expect(btn.textContent).toBe('Follow');
    expect(btn.classList.contains('active')).toBe(true);
    expect(container.scrollTop).toBe(900);
  });

  test('the scroll buttons - clicked - jump to the top and the bottom', () => {
    const content = buildKtalkPanel();
    const container = content.querySelector('.ktalk-transcript-messages');
    Object.defineProperty(container, 'scrollHeight', { configurable: true, value: 500 });

    content.querySelector('.ktalk-scroll-bottom').click();
    expect(container.scrollTop).toBe(500);
    content.querySelector('.ktalk-scroll-top').click();
    expect(container.scrollTop).toBe(0);
  });
});

describe('KTalk BioRead controls', () => {
  beforeEach(resetState);

  test('saved BioRead settings - the panel is built - applies them to the controls', () => {
    localStorage.setItem('ktalk-bioread-settings', JSON.stringify({ enabled: true, fixation: 50, tailOpacity: 70 }));

    const content = buildKtalkPanel();

    expect(state.bioRead).toMatchObject({ enabled: true, fixation: 50, tailOpacity: 70 });
    expect(content.querySelector('.ktalk-bioread-toggle').classList.contains('active')).toBe(true);
    expect(content.querySelector('[data-bioread-value="fixation"]').textContent).toBe('50%');
    expect(content.querySelector('[data-bioread-value="contrast"]').textContent).toBe('70%');
    expect(content.querySelector('.ktalk-bioread-fixation').value).toBe('50');
    expect(content.style.getPropertyValue('--bioread-tail-opacity')).toBe('0.7');
  });

  test('malformed saved settings - the panel is built - falls back to the defaults', () => {
    localStorage.setItem('ktalk-bioread-settings', '{nope');

    buildKtalkPanel();

    expect(state.bioRead).toMatchObject({ enabled: false, fixation: 40, tailOpacity: 60 });
  });

  test('a transcript line - BioRead toggled on - renders bold word heads and saves the setting', () => {
    const content = buildKtalkPanel();
    ui.handleTranscript({ speaker: 'Alice', text: 'привет', segment_id: 1 });

    content.querySelector('.ktalk-bioread-toggle').click();

    expect(state.bioRead.enabled).toBe(true);
    expect(content.querySelector('.ktalk-msg-text').innerHTML)
      .toBe('<span class="bio-word"><span class="bio-head">при</span><span class="bio-tail">вет</span></span>');
    expect(JSON.parse(localStorage.getItem('ktalk-bioread-settings'))).toEqual({
      enabled: true, fixation: 40, tailOpacity: 60, ignoreNumbers: true,
    });
  });

  test('a numeric transcript text - rendered with BioRead off - shows the number', () => {
    const content = buildKtalkPanel();

    ui.handleTranscript({ speaker: 'Alice', text: 42, segment_id: 1 });
    ui.handleTranscript({ speaker: 'Bob', text: null, segment_id: 2 });

    const texts = [...content.querySelectorAll('.ktalk-msg-text')].map((el) => el.textContent);
    expect(texts).toEqual(['42', '']);
  });

  test('the settings panel - opened, clicked inside, clicked outside - closes only on the outside click', () => {
    const content = buildKtalkPanel();
    const settingsBtn = content.querySelector('.ktalk-bioread-settings-btn');
    const panel = content.querySelector('.ktalk-bioread-panel');

    settingsBtn.click();
    expect(state.bioRead.panelOpen).toBe(true);
    expect(panel.classList.contains('active')).toBe(true);
    expect(settingsBtn.classList.contains('active')).toBe(true);

    panel.click();
    expect(state.bioRead.panelOpen).toBe(true);

    document.body.click();
    expect(state.bioRead.panelOpen).toBe(false);
    expect(panel.classList.contains('active')).toBe(false);
  });

  test('the settings panel is closed - a document click - leaves it closed', () => {
    buildKtalkPanel();

    document.body.click();

    expect(state.bioRead.panelOpen).toBe(false);
  });

  test('the settings panel is open - a click on its button bubbling to the document - does not close it', () => {
    const content = buildKtalkPanel();
    state.bioRead.panelOpen = true;

    document.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(state.bioRead.panelOpen).toBe(false);

    state.bioRead.panelOpen = true;
    const settingsBtn = content.querySelector('.ktalk-bioread-settings-btn');
    const event = new MouseEvent('click', { bubbles: true });
    Object.defineProperty(event, 'target', { value: settingsBtn });
    document.dispatchEvent(event);
    expect(state.bioRead.panelOpen).toBe(true);
  });

  test('the sliders - moved - update fixation, contrast and their labels', () => {
    const content = buildKtalkPanel();
    const fixation = content.querySelector('.ktalk-bioread-fixation');
    const contrast = content.querySelector('.ktalk-bioread-contrast');

    fixation.value = '55';
    fixation.dispatchEvent(new Event('input'));
    contrast.value = '80';
    contrast.dispatchEvent(new Event('input'));

    expect(state.bioRead).toMatchObject({ fixation: 55, tailOpacity: 80 });
    expect(content.querySelector('[data-bioread-value="fixation"]').textContent).toBe('55%');
    expect(content.querySelector('[data-bioread-value="contrast"]').textContent).toBe('80%');
    expect(content.style.getPropertyValue('--bioread-tail-opacity')).toBe('0.8');
  });

  test('Alt+B outside an input - pressed - toggles BioRead; other keys and inputs do not', () => {
    buildKtalkPanel();
    const textarea = document.createElement('textarea');
    const input = document.createElement('input');
    const editable = document.createElement('div');
    Object.defineProperty(editable, 'isContentEditable', { value: true });
    document.body.append(textarea, input, editable);

    document.body.dispatchEvent(new KeyboardEvent('keydown', { altKey: true, code: 'KeyB', bubbles: true }));
    expect(state.bioRead.enabled).toBe(true);

    document.body.dispatchEvent(new KeyboardEvent('keydown', { altKey: false, code: 'KeyB', bubbles: true }));
    document.body.dispatchEvent(new KeyboardEvent('keydown', { altKey: true, code: 'KeyC', bubbles: true }));
    textarea.dispatchEvent(new KeyboardEvent('keydown', { altKey: true, code: 'KeyB', bubbles: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { altKey: true, code: 'KeyB', bubbles: true }));
    editable.dispatchEvent(new KeyboardEvent('keydown', { altKey: true, code: 'KeyB', bubbles: true }));
    expect(state.bioRead.enabled).toBe(true);

    document.dispatchEvent(new KeyboardEvent('keydown', { altKey: true, code: 'KeyB' }));
    expect(state.bioRead.enabled).toBe(false);
  });

  test('controls removed from the panel - settings applied - nothing breaks', () => {
    const content = buildKtalkPanel();
    for (const el of content.querySelectorAll('.ktalk-bioread-settings-btn, .ktalk-bioread-panel, .ktalk-bioread-fixation, .ktalk-bioread-contrast, [data-bioread-value]')) {
      el.remove();
    }

    content.querySelector('.ktalk-bioread-toggle').click();

    expect(state.bioRead.enabled).toBe(true);
  });
});
