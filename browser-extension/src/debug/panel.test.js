/**
 * Tests for the debug panel: toggle, rendered rows, JSON payload, copy.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';

let state;
let ensureDebugPanel;
let updateDebugPanel;

beforeAll(async () => {
  state = (await import('../state.js')).state;
  ({ ensureDebugPanel, updateDebugPanel } = await import('./panel.js'));
});

beforeEach(() => {
  document.body.innerHTML = '';
  state.debugPanelElement = null;
  state.debugVisible = false;
  state.wsConnected = false;
  state.conferenceReady = false;
  state.currentMeetingUrl = null;
  state.lastActiveSpeaker = null;
  state.lastParticipants = [];
  state.transcriptLines = [];
  state.speakerHistory = [];
  state.audioFilterSuppressCount = 0;
  state.audioFilterLastFlatness = null;
  state.audioFilterLastReason = null;
});

function rows() {
  const out = {};
  for (const row of document.querySelectorAll('.ktalk-debug-row')) {
    out[row.querySelector('.ktalk-debug-key').textContent] = row.querySelector('.ktalk-debug-value').textContent;
  }
  return out;
}

describe('ensureDebugPanel', () => {
  test('no panel yet - ensured twice - adds one toggle and one panel', () => {
    ensureDebugPanel();
    ensureDebugPanel();
    expect(document.querySelectorAll('.ktalk-debug-toggle')).toHaveLength(1);
    expect(document.querySelectorAll('.ktalk-debug-panel')).toHaveLength(1);
    expect(document.querySelector('.ktalk-debug-title').textContent).toBe('KTalk Debug');
  });

  test('the toggle - clicked twice - shows then hides the panel', () => {
    ensureDebugPanel();
    const toggle = document.querySelector('.ktalk-debug-toggle');
    const panel = document.querySelector('.ktalk-debug-panel');
    toggle.click();
    expect(panel.classList.contains('active')).toBe(true);
    toggle.click();
    expect(panel.classList.contains('active')).toBe(false);
    state.debugPanelElement = null;
    toggle.click();
    expect(state.debugVisible).toBe(true);
  });
});

describe('updateDebugPanel', () => {
  test('no panel - updated - renders nothing', () => {
    updateDebugPanel();
    expect(document.body.innerHTML).toBe('');
  });

  test('an empty state - updated - shows dashes, zeros and false', () => {
    ensureDebugPanel();
    updateDebugPanel();
    expect(rows()).toEqual({
      wsConnected: 'false',
      conferenceReady: 'false',
      meetingUrl: '—',
      activeSpeaker: '—',
      activeSpeakerId: '—',
      participants: '0',
      participantsMap: '0',
      jitsiParticipants: '0',
      jitsiParticipantsMap: '0',
      transcriptLines: '0',
      speakerHistory: '0',
      noiseFilter: '0 suppressed',
      flatness: '—',
      filterReason: '—',
      pcmActive: '0',
      pcmSilent: '0',
      pcmRebinds: '0',
      pcmPcCount: '0',
      pcmInboundDelta: '0',
    });
  });

  test('a live meeting - updated - shows speaker, counts and the JSON payload', () => {
    state.wsConnected = true;
    state.conferenceReady = true;
    state.currentMeetingUrl = 'https://example.org/room';
    state.lastActiveSpeaker = { id: 'p1', name: 'Alice' };
    state.lastParticipants = null;
    state.transcriptLines = [{}, {}];
    state.speakerHistory = [{}];
    state.audioFilterSuppressCount = 3;
    state.audioFilterLastFlatness = 0.12345;
    state.audioFilterLastReason = 'speech';
    ensureDebugPanel();
    updateDebugPanel();
    const shown = rows();
    expect(shown.wsConnected).toBe('true');
    expect(shown.meetingUrl).toBe('https://example.org/room');
    expect(shown.activeSpeaker).toBe('Alice');
    expect(shown.activeSpeakerId).toBe('p1');
    expect(shown.participants).toBe('0');
    expect(shown.transcriptLines).toBe('2');
    expect(shown.noiseFilter).toBe('3 suppressed');
    expect(shown.flatness).toBe('0.123');
    expect(shown.filterReason).toBe('speech');
    const payload = JSON.parse(document.querySelector('.ktalk-debug-json').textContent);
    expect(payload).toMatchObject({
      wsConnected: true,
      currentMeetingUrl: 'https://example.org/room',
      activeSpeaker: { id: 'p1', name: 'Alice' },
      participants: [],
      transcriptLines: 2,
      speakerHistory: 1,
      pcmTransport: { active_count: 0 },
    });
  });

  test('the Copy button - clicked - copies the payload and flashes Copied', async () => {
    jest.useFakeTimers();
    try {
      ensureDebugPanel();
      updateDebugPanel();
      navigator.clipboard.writeText.mockClear();
      const button = document.querySelector('.ktalk-debug-copy-btn');
      button.click();
      await Promise.resolve();
      await Promise.resolve();
      const copied = navigator.clipboard.writeText.mock.calls[0][0];
      expect(JSON.parse(copied).wsConnected).toBe(false);
      expect(button.textContent).toBe('Copied');
      expect(button.classList.contains('copied')).toBe(true);
      jest.advanceTimersByTime(1200);
      expect(button.textContent).toBe('Copy');
      expect(button.classList.contains('copied')).toBe(false);
    } finally {
      jest.useRealTimers();
    }
  });
});

describe('updateDebugPanel with markup in page-supplied values', () => {
  test('a speaker name and meeting URL holding markup - rendered - shown as text, no element created', () => {
    ensureDebugPanel();
    state.lastActiveSpeaker = { name: '<img src=x onerror="window.pwned=1">', id: '<b>id</b>' };
    state.currentMeetingUrl = 'https://example.test/<script>x</script>';
    updateDebugPanel();
    const panel = document.querySelector('.ktalk-debug-panel');
    expect(panel.querySelector('img')).toBeNull();
    expect(panel.querySelector('.ktalk-debug-row b')).toBeNull();
    expect(panel.querySelector('.ktalk-debug-row script')).toBeNull();
    expect(rows().activeSpeaker).toBe('<img src=x onerror="window.pwned=1">');
    expect(rows().activeSpeakerId).toBe('<b>id</b>');
    expect(rows().meetingUrl).toBe('https://example.test/<script>x</script>');
  });
});
