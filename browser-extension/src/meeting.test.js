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

const { getMeetingUrl, sendMeetingJoin, sendMeetingLeave, handleMeetingJoinRejected, startMeetingCheck } = await import('./meeting.js');
const { state } = await import('./state.js');

const MATRIX_URL = 'https://element.example.org/#/room/!spike:matrix.example.org';

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

  test('falls back to window.location.href for ktalk/vas3k/jitsi', () => {
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

  test('a stale title retry from meeting A does not lock meeting B to document.title', () => {
    document.title = 'Element Call';
    const urlA = 'https://element.example.org/#/room/!A:matrix.example.org';
    const urlB = 'https://element.example.org/#/room/!B:matrix.example.org';
    let currentUrl = urlA;
    const titleByUrl = { [urlA]: '', [urlB]: '' }; // neither room name synced yet
    mockAdapter = {
      getMeetingUrl: () => currentUrl,
      getMeetingTitle: () => titleByUrl[currentUrl] || '',
      isConferenceAvailable: () => true,
    };
    mockPlatform = 'matrix';

    // Join A; run its retries almost to exhaustion while A's name stays unresolved.
    // After 5 ticks attempts 0..4 have run and the final (attempt 5) retry is
    // still PENDING — the stale chain that must not leak into meeting B.
    sendMeetingJoin();
    jest.advanceTimersByTime(2000 * 5);

    // Switch to B before A's chain exhausts.
    sendMeetingLeave('changed_meeting');
    currentUrl = urlB;
    sendMeetingJoin();

    // Next tick: A's pending attempt-5 fires (URL now B). Without URL scoping it
    // would give up and lock B to document.title. B's name resolves before B's
    // OWN chain exhausts, so a correctly-scoped B chain still emits the room name.
    jest.advanceTimersByTime(2000);
    titleByUrl[urlB] = 'Room B';
    jest.advanceTimersByTime(2000 * 2);

    const titles = mockSendWsMessage.mock.calls
      .map((c) => c[0])
      .filter((m) => m.type === 'meeting_title_update')
      .map((m) => m.title);
    expect(titles).not.toContain('Element Call');
    expect(titles).toContain('Room B');
  });

  test('a stale title retry after a same-room rejoin does not lock the room to document.title', () => {
    document.title = 'Element Call';
    // Distinct room url so a prior test's module-level `titleSentForUrl` lock
    // does not short-circuit this one.
    const url = 'https://element.example.org/#/room/!rejoin:matrix.example.org';
    let roomName = ''; // room name not synced yet
    mockAdapter = {
      getMeetingUrl: () => url,
      getMeetingTitle: () => roomName,
      isConferenceAvailable: () => true,
    };
    mockPlatform = 'matrix';

    // Join; run its retries almost to exhaustion while the room name stays empty.
    // After 5 ticks attempts 0..4 have run and the final (attempt 5) retry is
    // still PENDING.
    sendMeetingJoin();
    jest.advanceTimersByTime(2000 * 5);

    // Bridge rejects (another tab owns the meeting); we clear and re-join the
    // SAME url on the next poll. URL scoping alone cannot tell the stale chain
    // from the fresh one — both carry this url.
    handleMeetingJoinRejected({ active_meeting_id: 'other-tab' });
    sendMeetingJoin();

    // The stale attempt-5 fires now. Without a per-join generation token it would
    // give up (attempt 5 == TITLE_MAX_RETRIES), lock document.title, and block the
    // fresh chain. With the token it is invalidated; the fresh chain survives and
    // emits the room name once it resolves.
    jest.advanceTimersByTime(2000);
    roomName = 'Real Room';
    jest.advanceTimersByTime(2000 * 2);

    const titles = mockSendWsMessage.mock.calls
      .map((c) => c[0])
      .filter((m) => m.type === 'meeting_title_update')
      .map((m) => m.title);
    expect(titles).not.toContain('Element Call');
    expect(titles).toContain('Real Room');
  });

  test('falls back to document.title after exhausting matrix title retries', () => {
    document.title = 'Element Call';
    // Distinct room url so the module-level titleSentForUrl lock from the prior
    // test does not short-circuit this one.
    const otherUrl = 'https://element.example.org/#/room/!other:matrix.example.org';
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
