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
