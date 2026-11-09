/**
 * Frames sent when the bridge connection opens inside a meeting.
 *
 * The bridge clears participant and speaker state on `meeting_join`, so the
 * join must precede the participants and speaker frames of the same connect:
 * an adapter that reports only on change (Zoom) would not resend them.
 */

import { jest } from '@jest/globals';

const sent = [];
const conference = { platform: 'zoom' };
let inMeeting = true;

jest.unstable_mockModule('./platforms/index.js', () => ({
  getPlatformAdapter: () => ({ getConference: () => conference }),
}));
jest.unstable_mockModule('./meeting.js', () => ({
  handleMeetingJoinRejected: jest.fn(),
  isInMeeting: () => inMeeting,
  sendMeetingJoin: () => sent.push('meeting_join'),
  startMeetingCheck: jest.fn(),
}));
jest.unstable_mockModule('./speaker.js', () => ({
  getActiveSpeaker: () => null,
  sendParticipantsUpdate: () => sent.push('participants_update'),
  sendFilteredSpeakerUpdate: () => sent.push('speaker_update'),
}));
jest.unstable_mockModule('./ui.js', () => ({
  updateWsIndicatorPlatform: jest.fn(),
  handleTranscript: jest.fn(),
  handleTranscriptHistory: jest.fn(),
  handleTranscriptRefine: jest.fn(),
  handleNewMeeting: jest.fn(),
  handleCopilotReply: jest.fn(),
  handleCopilotStatus: jest.fn(),
  appendCopilotSystemMessage: jest.fn(),
  handleCopilotBridgeDisconnect: jest.fn(),
}));
jest.unstable_mockModule('./ws_api.js', () => ({
  sendWsMessage: (frame) => sent.push(frame.type),
}));

const { connectWebSocket } = await import('./ws.js');
const { state } = await import('./state.js');

class FakeWebSocket {
  static OPEN = 1;
  constructor() {
    this.readyState = 0;
  }
}

beforeEach(() => {
  sent.length = 0;
  inMeeting = true;
  state.wsConnection = null;
  global.WebSocket = FakeWebSocket;
});

afterEach(() => {
  clearInterval(state.clockSyncTimer);
  state.clockSyncTimer = null;
  delete global.WebSocket;
});

test('in a meeting - bridge connection opens - meeting_join precedes participants and speaker', () => {
  connectWebSocket();
  state.wsConnection.onopen();

  expect(sent.slice(0, 3)).toEqual(['meeting_join', 'participants_update', 'speaker_update']);
});

test('joined - bridge answers with transcript_history - participants are reported again', () => {
  connectWebSocket();
  state.currentMeetingUrl = 'https://zoom.us/j/12345678901';
  sent.length = 0;

  state.wsConnection.onmessage({ data: JSON.stringify({ type: 'transcript_history', segments: [] }) });

  expect(sent).toEqual(['participants_update']);
});

test('join refused (no current meeting url) - transcript_history - participants are not sent', () => {
  connectWebSocket();
  state.currentMeetingUrl = null;
  sent.length = 0;

  state.wsConnection.onmessage({ data: JSON.stringify({ type: 'transcript_history', segments: [] }) });

  expect(sent).toEqual([]);
});

test('outside a meeting - bridge connection opens - neither join nor participants are sent', () => {
  inMeeting = false;
  connectWebSocket();
  state.wsConnection.onopen();

  expect(sent).toEqual(['clock_sync']);
});

describe('bridge frames', () => {
  let ui;
  let meeting;

  beforeAll(async () => {
    ui = await import('./ui.js');
    meeting = await import('./meeting.js');
  });

  beforeEach(() => {
    jest.clearAllMocks();
    state.currentMeetingId = 'm1';
    connectWebSocket();
  });

  afterEach(() => {
    state.currentMeetingId = null;
  });

  function receive(frame) {
    state.wsConnection.onmessage({ data: JSON.stringify(frame) });
  }

  test.each([
    ['transcript', 'handleTranscript'],
    ['transcript_history', 'handleTranscriptHistory'],
    ['transcript_refine', 'handleTranscriptRefine'],
    ['new_meeting', 'handleNewMeeting'],
    ['copilot_reply', 'handleCopilotReply'],
    ['copilot_status', 'handleCopilotStatus'],
  ])('a %s frame - received - goes to %s', (type, handler) => {
    const frame = { type, meeting_id: 'm1', text: 'x' };

    receive(frame);

    expect(ui[handler]).toHaveBeenCalledWith(frame);
    const others = ['handleTranscript', 'handleTranscriptHistory', 'handleTranscriptRefine',
      'handleNewMeeting', 'handleCopilotReply', 'handleCopilotStatus'].filter((name) => name !== handler);
    for (const other of others) expect(ui[other]).not.toHaveBeenCalled();
  });

  test('a meeting_started frame with a title - received - announces the meeting with its title', () => {
    receive({ type: 'meeting_started', meeting_id: 'm2', meeting_title: 'Planning' });

    expect(ui.appendCopilotSystemMessage).toHaveBeenCalledWith('Встреча началась — Planning');
  });

  test('a meeting_started frame without a title - received - announces the meeting alone', () => {
    receive({ type: 'meeting_started', meeting_id: 'm2' });

    expect(ui.appendCopilotSystemMessage).toHaveBeenCalledWith('Встреча началась');
  });

  test('a meeting_ended frame of this tab\'s meeting - received - announces the end', () => {
    receive({ type: 'meeting_ended', meeting_id: 'm1' });

    expect(ui.appendCopilotSystemMessage).toHaveBeenCalledWith('Встреча закончилась');
  });

  test('a meeting_ended frame of another meeting - received - stays quiet', () => {
    receive({ type: 'meeting_ended', meeting_id: 'other' });

    expect(ui.appendCopilotSystemMessage).not.toHaveBeenCalled();
  });

  test('a meeting_join_rejected frame with a notice - received - shows the notice', () => {
    meeting.handleMeetingJoinRejected.mockReturnValueOnce('refused');
    const frame = { type: 'meeting_join_rejected', reason: 'another_tab_active' };

    receive(frame);

    expect(meeting.handleMeetingJoinRejected).toHaveBeenCalledWith(frame);
    expect(ui.appendCopilotSystemMessage).toHaveBeenCalledWith('refused');
  });

  test('a repeated meeting_join_rejected frame - received - shows nothing', () => {
    meeting.handleMeetingJoinRejected.mockReturnValueOnce(null);

    receive({ type: 'meeting_join_rejected', reason: 'another_tab_active' });

    expect(ui.appendCopilotSystemMessage).not.toHaveBeenCalled();
  });

  test('an unknown frame or broken JSON - received - reaches no handler', () => {
    const logSpy = jest.spyOn(console, 'log').mockImplementation(() => {});

    receive({ type: 'something_else' });
    state.wsConnection.onmessage({ data: '{not json' });

    for (const handler of ['handleTranscript', 'handleCopilotReply', 'appendCopilotSystemMessage']) {
      expect(ui[handler]).not.toHaveBeenCalled();
    }
    expect(logSpy).toHaveBeenCalledWith('[KTalk Transcript] Parse error:', expect.any(SyntaxError));
    logSpy.mockRestore();
  });
});

describe('bridge connection closing', () => {
  let ui;

  beforeAll(async () => {
    ui = await import('./ui.js');
  });

  beforeEach(() => {
    jest.useFakeTimers();
    jest.clearAllMocks();
    state.wsReconnectTimer = null;
  });

  afterEach(() => {
    clearTimeout(state.wsReconnectTimer);
    state.wsReconnectTimer = null;
    jest.useRealTimers();
  });

  test('an open connection - it closes - reports the drop and reconnects after 5 s', () => {
    connectWebSocket();
    const first = state.wsConnection;
    first.onopen();
    state.wsConnected = true;

    first.onclose();

    expect(state.wsConnected).toBe(false);
    expect(ui.updateWsIndicatorPlatform).toHaveBeenLastCalledWith(false);
    expect(ui.handleCopilotBridgeDisconnect).toHaveBeenCalled();
    expect(state.clockSyncTimer).toBeNull();
    jest.advanceTimersByTime(4999);
    expect(state.wsConnection).toBe(first);
    jest.advanceTimersByTime(1);
    expect(state.wsConnection).not.toBe(first);
    expect(state.wsReconnectTimer).toBeNull();
  });

  test('a reconnect already pending - the connection closes again - schedules no second one', () => {
    connectWebSocket();
    const first = state.wsConnection;
    first.onclose();
    const pending = state.wsReconnectTimer;

    first.onclose();

    expect(state.wsReconnectTimer).toBe(pending);
    expect(jest.getTimerCount()).toBe(1);
  });

  test('a failing Copilot disconnect handler - the connection closes - still reconnects', () => {
    const logSpy = jest.spyOn(console, 'log').mockImplementation(() => {});
    ui.handleCopilotBridgeDisconnect.mockImplementationOnce(() => {
      throw new Error('boom');
    });
    connectWebSocket();

    state.wsConnection.onclose();

    expect(logSpy).toHaveBeenCalledWith('[KTalk Copilot] bridge-disconnect handler failed:', expect.any(Error));
    expect(state.wsReconnectTimer).not.toBeNull();
    logSpy.mockRestore();
  });

  test('a connection error - reported - marks the bridge disconnected', () => {
    connectWebSocket();
    state.wsConnected = true;

    state.wsConnection.onerror();

    expect(state.wsConnected).toBe(false);
  });
});
