import { jest } from '@jest/globals';

let adapter;
jest.unstable_mockModule('./platforms/index.js', () => ({
  getPlatformAdapter: () => adapter,
  getPlatform: () => 'ktalk',
}));
const send = jest.fn();
jest.unstable_mockModule('./ws_api.js', () => ({
  sendWsMessage: send,
  sendWsPcmChunk: jest.fn(),
  sendWsPcmTransportStats: jest.fn(),
}));

const { KTalkAdapter } = await import('./platforms/ktalk.js');
const { startMeetingCheck, sendMeetingJoin, sendMeetingLeave, handleMeetingJoinRejected, getJoinRejectionStatus } = await import('./meeting.js');
const { state } = await import('./state.js');

let joined;
beforeEach(() => {
  jest.useFakeTimers();
  sendMeetingLeave();
  state.currentMeetingId = null;
  send.mockClear();
  joined = false;
  window.$conf = { jitsiConf: { isJoined: () => joined }, _dominantSpeakerId$: {} };
  adapter = new KTalkAdapter();
});
afterEach(() => {
  jest.clearAllTimers();
  jest.useRealTimers();
  delete window.$conf;
});

test('prejoin and an exited call keep the same wrapper without authorizing recording', () => {
  const wrapper = window.$conf;
  expect(adapter.isConferenceAvailable()).toBe(true);
  startMeetingCheck();
  jest.advanceTimersByTime(2000);
  expect(send).not.toHaveBeenCalled();

  joined = true;
  jest.advanceTimersByTime(2000);
  expect(send.mock.calls.filter(([frame]) => frame.type === 'meeting_join')).toHaveLength(1);

  joined = false;
  jest.advanceTimersByTime(2000);
  expect(send).toHaveBeenCalledWith({ type: 'meeting_leave', reason: 'user_left' });
  expect(window.$conf).toBe(wrapper);
  jest.advanceTimersByTime(4000);
  expect(send.mock.calls.filter(([frame]) => frame.type === 'meeting_join')).toHaveLength(1);
});

test('reconnect cannot send join while outside the call', () => {
  sendMeetingJoin();
  expect(send).not.toHaveBeenCalled();
  joined = true;
  sendMeetingJoin();
  expect(send).toHaveBeenCalledWith(expect.objectContaining({ type: 'meeting_join' }));
});

test.each(['missing', 'throws'])('unknown membership (%s) keeps recording off', (mode) => {
  if (mode === 'missing') delete window.$conf.jitsiConf.isJoined;
  else window.$conf.jitsiConf.isJoined = () => { throw new Error('disposed'); };
  sendMeetingJoin();
  startMeetingCheck();
  jest.advanceTimersByTime(6000);
  expect(send).not.toHaveBeenCalled();
});

test('capture failure clears the accepted id and retries while joined, with one notice', () => {
  joined = true;
  sendMeetingJoin();
  state.currentMeetingId = 'old-meeting';
  expect(handleMeetingJoinRejected({ reason: 'capture_unavailable' })).toContain('Не удалось включить запись');
  expect(state.currentMeetingId).toBeNull();
  startMeetingCheck();
  jest.advanceTimersByTime(2000);
  expect(send.mock.calls.filter(([frame]) => frame.type === 'meeting_join')).toHaveLength(2);
  expect(handleMeetingJoinRejected({ reason: 'capture_unavailable' })).toBeNull();
  joined = false;
  jest.advanceTimersByTime(4000);
  expect(send.mock.calls.filter(([frame]) => frame.type === 'meeting_join')).toHaveLength(2);
});

test('joined while another tab holds the recording - join refused on every retry - one notice and a status until the join is accepted', () => {
  joined = true;
  sendMeetingJoin();
  expect(getJoinRejectionStatus()).toBeNull();

  expect(handleMeetingJoinRejected({ reason: 'another_tab_active' })).toContain('другая вкладка');
  expect(getJoinRejectionStatus()).toContain('другая вкладка');

  startMeetingCheck();
  jest.advanceTimersByTime(2000);
  expect(send.mock.calls.filter(([frame]) => frame.type === 'meeting_join')).toHaveLength(2);
  expect(handleMeetingJoinRejected({ reason: 'another_tab_active' })).toBeNull();
  expect(getJoinRejectionStatus()).toContain('другая вкладка');

  // The bridge answers an accepted join with `transcript_history`.
  jest.advanceTimersByTime(2000);
  state.currentMeetingId = 'accepted-meeting';
  expect(getJoinRejectionStatus()).toBeNull();
});

test('join refused because another tab holds the recording - user leaves the call - status cleared', () => {
  joined = true;
  sendMeetingJoin();
  handleMeetingJoinRejected({ reason: 'another_tab_active' });
  startMeetingCheck();

  joined = false;
  jest.advanceTimersByTime(2000);

  expect(getJoinRejectionStatus()).toBeNull();
});

test('capture unavailable - join refused - no status line text', () => {
  joined = true;
  sendMeetingJoin();
  handleMeetingJoinRejected({ reason: 'capture_unavailable' });

  expect(getJoinRejectionStatus()).toBeNull();
});
