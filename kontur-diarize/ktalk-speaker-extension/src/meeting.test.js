/**
 * Tests for the canonical meeting-URL helper introduced for Element Call.
 *
 * The invariant under test: the URL sent in `meeting_join` / stored in
 * `state.currentMeetingUrl` MUST equal the URL `startMeetingCheck()` compares
 * against on every 2 s tick. For matrix the adapter synthesizes a
 * `#/room/<roomId>` URL (its real location is `…/index.html#…`); if the two
 * sides read different sources, the check would flap leave/join every tick.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';

let mockAdapter = null;
let mockPlatform = 'ktalk';

jest.unstable_mockModule('./platforms/index.js', () => ({
  getPlatformAdapter: jest.fn(() => mockAdapter),
  getPlatform: jest.fn(() => mockPlatform),
}));

const mockSendWsMessage = jest.fn();
jest.unstable_mockModule('./ws_api.js', () => ({
  sendWsMessage: mockSendWsMessage,
}));

const { getMeetingUrl, sendMeetingJoin, sendMeetingLeave, startMeetingCheck } = await import('./meeting.js');
const { state } = await import('./state.js');

const MATRIX_URL = 'https://element.vkkovalev.work/#/room/!spike:matrix.vkkovalev.work';

const matrixAdapter = {
  getMeetingUrl: () => MATRIX_URL,
  isConferenceAvailable: () => true,
};

const ktalkAdapter = {
  // no getMeetingUrl → helper falls back to window.location.href
  isConferenceAvailable: () => true,
};

beforeEach(() => {
  mockAdapter = null;
  mockPlatform = 'ktalk';
  mockSendWsMessage.mockClear();
  state.currentMeetingUrl = null;
  state.currentMeetingId = null;
  if (state.meetingCheckInterval) {
    clearInterval(state.meetingCheckInterval);
    state.meetingCheckInterval = null;
  }
});

describe('getMeetingUrl', () => {
  test('returns the adapter synthesized url for matrix', () => {
    mockAdapter = matrixAdapter;
    mockPlatform = 'matrix';
    expect(getMeetingUrl()).toBe(MATRIX_URL);
  });

  test('falls back to window.location.href for ktalk/vas3k/vkkovalev', () => {
    mockAdapter = ktalkAdapter;
    mockPlatform = 'ktalk';
    expect(getMeetingUrl()).toBe(window.location.href);
  });

  test('falls back to window.location.href when there is no adapter', () => {
    mockAdapter = null;
    expect(getMeetingUrl()).toBe(window.location.href);
  });
});

describe('sendMeetingJoin uses the canonical url', () => {
  test('stores and sends the synthesized url for matrix', () => {
    mockAdapter = matrixAdapter;
    mockPlatform = 'matrix';

    sendMeetingJoin();

    expect(state.currentMeetingUrl).toBe(MATRIX_URL);
    const joinMsg = mockSendWsMessage.mock.calls.map((c) => c[0]).find((m) => m.type === 'meeting_join');
    expect(joinMsg).toMatchObject({ type: 'meeting_join', url: MATRIX_URL, platform: 'matrix' });
  });
});

describe('no-flap invariant', () => {
  beforeEach(() => jest.useFakeTimers());
  afterEach(() => jest.useRealTimers());

  test('startMeetingCheck does not re-join/leave when the stored url matches the canonical url (matrix)', () => {
    mockAdapter = matrixAdapter;
    mockPlatform = 'matrix';

    sendMeetingJoin();
    mockSendWsMessage.mockClear();

    startMeetingCheck();
    // advance a couple of ticks
    jest.advanceTimersByTime(6000);

    const types = mockSendWsMessage.mock.calls.map((c) => c[0].type);
    expect(types).not.toContain('meeting_leave');
    expect(types).not.toContain('meeting_join');
    // the stored url is exactly what the check compares against
    expect(state.currentMeetingUrl).toBe(getMeetingUrl());
  });
});

describe('sendDelayedTitle prefers the adapter title', () => {
  beforeEach(() => jest.useFakeTimers());
  afterEach(() => jest.useRealTimers());

  test('sends the matrix room name from getMeetingTitle, not document.title', () => {
    document.title = 'Element Call'; // the non-authoritative EC iframe page title
    mockAdapter = {
      getMeetingUrl: () => MATRIX_URL,
      getMeetingTitle: () => 'Recog Spike',
      isConferenceAvailable: () => true,
    };
    mockPlatform = 'matrix';

    sendMeetingJoin();
    jest.advanceTimersByTime(2000);

    const titleMsg = mockSendWsMessage.mock.calls.map((c) => c[0]).find((m) => m.type === 'meeting_title_update');
    expect(titleMsg).toMatchObject({ type: 'meeting_title_update', title: 'Recog Spike' });
  });

  test('falls back to document.title when the adapter has no getMeetingTitle (ktalk)', () => {
    document.title = 'KTalk Room';
    mockAdapter = ktalkAdapter;
    mockPlatform = 'ktalk';

    sendMeetingJoin();
    jest.advanceTimersByTime(2000);

    const titleMsg = mockSendWsMessage.mock.calls.map((c) => c[0]).find((m) => m.type === 'meeting_title_update');
    expect(titleMsg).toMatchObject({ type: 'meeting_title_update', title: 'KTalk Room' });
  });

  test('retries the empty matrix room name instead of locking in document.title', () => {
    document.title = 'Element Call'; // the non-authoritative EC iframe page title
    let roomName = ''; // room not yet synced when we first look
    mockAdapter = {
      getMeetingUrl: () => MATRIX_URL,
      getMeetingTitle: () => roomName,
      isConferenceAvailable: () => true,
    };
    mockPlatform = 'matrix';

    sendMeetingJoin();
    // First look at +2s: room name still empty → nothing sent yet, no lock.
    jest.advanceTimersByTime(2000);
    expect(mockSendWsMessage.mock.calls.map((c) => c[0]).find((m) => m.type === 'meeting_title_update')).toBeUndefined();

    // Room name resolves; the next retry tick picks it up.
    roomName = 'Recog Spike';
    jest.advanceTimersByTime(2000);

    const titleMsg = mockSendWsMessage.mock.calls.map((c) => c[0]).find((m) => m.type === 'meeting_title_update');
    expect(titleMsg).toMatchObject({ type: 'meeting_title_update', title: 'Recog Spike' });
  });

  test('falls back to document.title after exhausting matrix title retries', () => {
    document.title = 'Element Call';
    // Distinct room url so the module-level titleSentForUrl lock from the prior
    // test does not short-circuit this one.
    const otherUrl = 'https://element.vkkovalev.work/#/room/!other:matrix.vkkovalev.work';
    mockAdapter = {
      getMeetingUrl: () => otherUrl,
      getMeetingTitle: () => '', // never resolves
      isConferenceAvailable: () => true,
    };
    mockPlatform = 'matrix';

    sendMeetingJoin();
    // Initial attempt + 5 retries, 2 s apart → advance well past the budget.
    jest.advanceTimersByTime(2000 * 8);

    const titleMsg = mockSendWsMessage.mock.calls.map((c) => c[0]).find((m) => m.type === 'meeting_title_update');
    expect(titleMsg).toMatchObject({ type: 'meeting_title_update', title: 'Element Call' });
  });
});
