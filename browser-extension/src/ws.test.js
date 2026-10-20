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
