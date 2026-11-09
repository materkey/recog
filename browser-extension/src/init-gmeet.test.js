/**
 * init.js wiring for adapters with the optional `isRoomPage()` /
 * `getDiagnostic()` hooks (Google Meet).
 *
 * The content script runs on every meet.google.com page, including `/`,
 * `/home` and `/landing`: there the 2 s interval must not mount the floating
 * panel and a failed `waitForConference` must not keep retrying — the
 * interval's tryAttachConference() picks the conference up once a room is
 * entered. A selector-drift diagnostic from the adapter is shown in the
 * existing panel status line.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://meet.google.com/landing"}
 */

import { jest } from '@jest/globals';

let init;
let handleCopilotStatus;
let updateSpeakerStatus;
let handleMeetingJoinRejected;
let sendMeetingLeave;
let state;

const conference = { platform: 'gmeet' };

const fakeAdapter = {
  roomPage: false,
  diagnostic: null,
  inMeeting: false,
  supportsTabInjection: () => false,
  isRoomPage: () => fakeAdapter.roomPage,
  getDiagnostic: () => fakeAdapter.diagnostic,
  prejoin: false,
  isInPrejoin: () => fakeAdapter.prejoin,
  isConferenceAvailable: () => fakeAdapter.inMeeting,
  waitForConference: jest.fn(() => new Promise(() => {})),
  getConference: () => (fakeAdapter.inMeeting ? conference : null),
  getJitsiConference: () => (fakeAdapter.inMeeting ? conference : null),
  isConferenceReady: () => true,
  subscribeToSpeakerChanges: jest.fn(),
  onParticipantJoined: jest.fn(),
  getActiveSpeaker: () => null,
  anyMicOn: () => true,
  getParticipants: () => [],
  getParticipantName: (id) => id,
  isMicMuted: () => false,
  dispose: () => {},
};

beforeAll(async () => {
  await jest.unstable_mockModule('./platforms/index.js', () => ({
    __esModule: true,
    getPlatform: jest.fn(() => 'gmeet'),
    getPlatformAdapter: jest.fn(() => fakeAdapter),
    getPlatformDisplayName: jest.fn(() => 'Google Meet'),
    detectPlatform: jest.fn(() => 'gmeet'),
    isJitsiPlatform: jest.fn(() => true),
    resetAdapter: jest.fn(),
  }));
  // Keep init() from opening a real WebSocket to the bridge.
  await jest.unstable_mockModule('./ws.js', () => ({
    __esModule: true,
    connectWebSocket: jest.fn(),
  }));

  ({ init } = await import('./init.js'));
  ({ handleCopilotStatus } = await import('./ui.js'));
  ({ updateSpeakerStatus } = await import('./speaker.js'));
  ({ handleMeetingJoinRejected, sendMeetingLeave } = await import('./meeting.js'));
  ({ state } = await import('./state.js'));
});

beforeEach(() => {
  jest.useFakeTimers();
  document.body.innerHTML = '';
  document.head.querySelectorAll('style').forEach((s) => s.remove());
  fakeAdapter.roomPage = false;
  fakeAdapter.diagnostic = null;
  fakeAdapter.inMeeting = false;
  fakeAdapter.prejoin = false;
  fakeAdapter.subscribeToSpeakerChanges.mockClear();
  fakeAdapter.waitForConference.mockReset();
  fakeAdapter.waitForConference.mockImplementation(() => new Promise(() => {}));
  sendMeetingLeave();
  state.currentMeetingId = null;
});

afterEach(() => {
  jest.clearAllTimers();
  jest.useRealTimers();
});

test('landing page - init interval ticks - no floating panel is created', () => {
  init();
  jest.advanceTimersByTime(6000);

  expect(document.querySelector('.jitsi-transcript-panel')).toBeNull();
});

test('room page - init interval ticks - floating panel is created', () => {
  fakeAdapter.roomPage = true;

  init();
  jest.advanceTimersByTime(2000);

  expect(document.querySelector('.jitsi-transcript-panel')).not.toBeNull();
});

test('landing page - waitForConference gives up - no retry is scheduled', async () => {
  fakeAdapter.waitForConference.mockImplementation(() => Promise.reject(new Error('Conference not found')));

  init();
  await Promise.resolve();
  await Promise.resolve();
  jest.advanceTimersByTime(10000);

  expect(fakeAdapter.waitForConference).toHaveBeenCalledTimes(1);
});

test('room page - waitForConference gives up - retry is scheduled', async () => {
  fakeAdapter.roomPage = true;
  fakeAdapter.waitForConference.mockImplementation(() => Promise.reject(new Error('Conference not found')));

  init();
  await Promise.resolve();
  await Promise.resolve();
  jest.advanceTimersByTime(2000);

  expect(fakeAdapter.waitForConference).toHaveBeenCalledTimes(2);
});

test('room page with selector drift - init interval ticks - diagnostic appears in the panel status line', () => {
  fakeAdapter.roomPage = true;
  fakeAdapter.diagnostic = 'Google Meet: не удаётся определить говорящего';

  init();
  jest.advanceTimersByTime(2000);

  expect(document.querySelector('.jitsi-speaker-name').textContent)
    .toBe('Google Meet: не удаётся определить говорящего');
});

test('landing page - early copilot_status bridge frame - no floating panel is created', () => {
  init();
  handleCopilotStatus({ type: 'copilot_status', status: 'agent_state', agent_state: 'online' });
  jest.advanceTimersByTime(6000);

  expect(document.querySelector('.jitsi-transcript-panel')).toBeNull();
});

test('landing page - user enters a room and the call starts - panel created and speaker changes subscribed', () => {
  init();
  jest.advanceTimersByTime(2000);
  expect(fakeAdapter.subscribeToSpeakerChanges).not.toHaveBeenCalled();

  fakeAdapter.roomPage = true;
  fakeAdapter.inMeeting = true;
  jest.advanceTimersByTime(4000);

  expect(document.querySelector('.jitsi-transcript-panel')).not.toBeNull();
  expect(fakeAdapter.subscribeToSpeakerChanges).toHaveBeenCalledTimes(1);
});

test('room page with selector drift - speaker status update - diagnostic kept until drift clears', () => {
  fakeAdapter.roomPage = true;
  fakeAdapter.diagnostic = 'Google Meet: не удаётся определить говорящего';
  init();
  jest.advanceTimersByTime(2000);

  updateSpeakerStatus(conference);
  expect(document.querySelector('.jitsi-speaker-name').textContent)
    .toBe('Google Meet: не удаётся определить говорящего');

  fakeAdapter.diagnostic = null;
  updateSpeakerStatus(conference);
  expect(document.querySelector('.jitsi-speaker-name').textContent).toBe('Никто не говорит');
});

test('room page in a call - join refused because another tab holds the recording - status line says so until the join is accepted', () => {
  fakeAdapter.roomPage = true;
  fakeAdapter.inMeeting = true;
  init();
  jest.advanceTimersByTime(2000);

  handleMeetingJoinRejected({ reason: 'another_tab_active' });
  jest.advanceTimersByTime(2000);
  expect(document.querySelector('.jitsi-speaker-name').textContent)
    .toBe('Запись ведёт другая вкладка с конференцией');

  updateSpeakerStatus(conference);
  expect(document.querySelector('.jitsi-speaker-name').textContent)
    .toBe('Запись ведёт другая вкладка с конференцией');

  state.currentMeetingId = 'accepted-meeting';
  jest.advanceTimersByTime(2000);
  expect(document.querySelector('.jitsi-speaker-name').textContent).toBe('Никто не говорит');
});

test('room page before joining - init interval ticks - status line asks to join', () => {
  fakeAdapter.roomPage = true;
  fakeAdapter.prejoin = true;

  init();
  jest.advanceTimersByTime(2000);

  expect(document.querySelector('.jitsi-speaker-name').textContent).toBe('Нажмите "Присоединиться" для начала');
});

test('room page outside a call - init interval ticks - status line waits for the connection', () => {
  fakeAdapter.roomPage = true;

  init();
  jest.advanceTimersByTime(2000);

  expect(document.querySelector('.jitsi-speaker-name').textContent).toBe('Ожидание подключения...');
});

test('an attached call - the conference goes away - the next tick forgets it', () => {
  fakeAdapter.roomPage = true;
  fakeAdapter.inMeeting = true;
  init();
  jest.advanceTimersByTime(2000);
  expect(state.activeConfRef).toBe(conference);
  state.conferenceReady = true;

  fakeAdapter.inMeeting = false;
  jest.advanceTimersByTime(2000);

  expect(state.activeConfRef).toBeNull();
  expect(state.activeJitsiConf).toBeNull();
  expect(state.conferenceReady).toBe(false);
});
