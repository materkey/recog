/**
 * init.js on a KTalk page without a platform adapter (detectPlatform() found
 * nothing): the conference is the page's `window.$conf`, its events come from
 * the Jitsi conference inside it.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://example.ktalk.ru/room"}
 */

import { jest } from '@jest/globals';

await jest.unstable_mockModule('./ws.js', () => ({ connectWebSocket: jest.fn() }));
await jest.unstable_mockModule('./platforms/index.js', () => ({
  getPlatform: () => null,
  getPlatformDisplayName: () => 'Unknown',
  isJitsiPlatform: () => false,
  getPlatformAdapter: () => null,
}));
const { state } = await import('./state.js');
const { init } = await import('./init.js');

let frames;
let observers;

function audioTrack(participantId, type = 'audio') {
  return { getType: () => type, getParticipantId: () => participantId, isMuted: () => false };
}

function makeLegacyConf({ tracks = [], localTracks = [] } = {}) {
  const handlers = {};
  const participant = {
    _id: 'p1',
    name: 'Alice',
    _activeSpeaker$: { value: false, subscribe: jest.fn() },
    _audioMuted$: { value: false },
  };
  const conf = {
    _participantsMap: new Map([['p1', participant]]),
    localParticipant: null,
    _dominantSpeakerId$: { value: null, subscribe: jest.fn() },
    jitsiConf: {
      on: (event, callback) => {
        handlers[event] = callback;
      },
      getParticipants: () => [{ _id: 'p1', getTracks: () => tracks }],
      getLocalTracks: () => localTracks,
    },
  };
  return { conf, handlers, participant };
}

function sentTypes() {
  return frames.map((frame) => frame.type);
}

function statusEvents() {
  return window.dispatchEvent.mock.calls
    .map(([event]) => event)
    .filter((event) => event.type === 'ktalk-connection-status')
    .map((event) => event.detail.connected);
}

beforeEach(() => {
  jest.useFakeTimers();
  jest.spyOn(console, 'log').mockImplementation(() => {});
  document.body.innerHTML = '';
  delete window.$conf;
  window.dispatchEvent.mockClear();
  frames = [];
  state.wsConnection = {
    readyState: WebSocket.OPEN,
    send: (raw) => frames.push(JSON.parse(raw)),
  };
  state.conferenceReady = false;
  state.activeConfRef = null;
  state.activeJitsiConf = null;
  observers = [];
  const Observer = window.MutationObserver;
  jest.spyOn(window, 'MutationObserver').mockImplementation((callback) => {
    const observer = new Observer(callback);
    observers.push(observer);
    return observer;
  });
});

afterEach(() => {
  observers.forEach((observer) => observer.disconnect());
  jest.restoreAllMocks();
  jest.clearAllTimers();
  jest.useRealTimers();
  state.wsConnection = null;
});

test('no conference object - waiting gives up after 100 checks - reports the connection lost', () => {
  init();

  jest.advanceTimersByTime(99 * 500 - 1);
  expect(statusEvents()).toEqual([]);
  jest.advanceTimersByTime(1);

  expect(statusEvents()).toEqual([false]);
  expect(frames).toContainEqual({ type: 'connection_status', connected: false });
});

test('conference object appears - waiting finds it - subscribes to its speaker events and reports participants', () => {
  const { conf, handlers, participant } = makeLegacyConf();
  init();
  jest.advanceTimersByTime(1000);

  window.$conf = conf;
  jest.advanceTimersByTime(500);

  expect(statusEvents()).toEqual([true]);
  expect(sentTypes()).toEqual(expect.arrayContaining(['connection_status', 'participants_update', 'speaker_update']));
  expect(conf._dominantSpeakerId$.subscribe).toHaveBeenCalledTimes(1);
  expect(participant._activeSpeaker$.subscribe).toHaveBeenCalledTimes(1);
  expect(Object.keys(handlers).sort()).toEqual([
    'conference.dominantSpeaker',
    'conference.trackAdded',
    'conference.trackMuteChanged',
    'conference.trackRemoved',
    'conference.userJoined',
    'conference.userLeft',
  ]);
});

test('a Jitsi participant with an audio track - conference found - is ready at once', () => {
  window.$conf = makeLegacyConf({ tracks: [audioTrack('p1')] }).conf;

  init();

  expect(state.conferenceReady).toBe(true);
});

test('only a local audio track - conference found - is ready at once', () => {
  window.$conf = makeLegacyConf({ localTracks: [audioTrack('me')] }).conf;

  init();

  expect(state.conferenceReady).toBe(true);
});

test('no audio track yet - conference found - becomes ready 5 s after the wait finds it', () => {
  // The wait finds it at once; the 2 s ticks leave the attached conference
  // and its readiness timeout alone.
  window.$conf = makeLegacyConf({ tracks: [audioTrack('p1', 'video')] }).conf;

  init();
  expect(state.conferenceReady).toBe(false);
  jest.advanceTimersByTime(4999);
  expect(state.conferenceReady).toBe(false);
  jest.advanceTimersByTime(1);

  expect(state.conferenceReady).toBe(true);
});

test('a conference the wait found - the interval ticks - is subscribed to once', () => {
  const { conf, participant } = makeLegacyConf();
  window.$conf = conf;

  init();
  jest.advanceTimersByTime(4000);

  expect(conf._dominantSpeakerId$.subscribe).toHaveBeenCalledTimes(1);
  expect(participant._activeSpeaker$.subscribe).toHaveBeenCalledTimes(1);
  expect(statusEvents()).toEqual([true]);
});

test('a conference the interval attached first - the wait finds it - is subscribed to once', () => {
  const { conf } = makeLegacyConf();
  init();
  jest.advanceTimersByTime(1999);
  window.$conf = conf;

  // At 2 s the interval runs before the wait's check scheduled at 1.5 s.
  jest.advanceTimersByTime(1);
  expect(state.activeConfRef).toBe(conf);
  jest.advanceTimersByTime(2000);

  expect(conf._dominantSpeakerId$.subscribe).toHaveBeenCalledTimes(1);
  expect(statusEvents()).toEqual([true]);
});

test('a subscribed conference - speaker observables fire - the speaker is reported again', () => {
  const { conf, participant } = makeLegacyConf();
  window.$conf = conf;
  init();
  frames = [];

  conf._dominantSpeakerId$.subscribe.mock.calls[0][0]();
  participant._activeSpeaker$.subscribe.mock.calls[0][0]();

  expect(sentTypes()).toEqual(['speaker_update', 'speaker_update']);
});

test('a subscribed conference - the dominant speaker changes - the speaker is reported again', () => {
  const { conf, handlers } = makeLegacyConf();
  window.$conf = conf;
  init();
  frames = [];

  handlers['conference.dominantSpeaker']('p1');

  expect(sentTypes()).toEqual(['speaker_update']);
});

test('a subscribed conference - an audio track is added - reports participants and subscribes to its owner', () => {
  const { conf, handlers, participant } = makeLegacyConf();
  window.$conf = conf;
  init();
  frames = [];

  handlers['conference.trackAdded'](audioTrack('p1'));
  handlers['conference.trackAdded'](audioTrack('unknown'));
  handlers['conference.trackAdded']({ getType: () => 'audio' });

  expect(sentTypes()).toEqual([
    'speaker_update', 'participants_update',
    'speaker_update', 'participants_update',
    'speaker_update', 'participants_update',
  ]);
  expect(participant._activeSpeaker$.subscribe).toHaveBeenCalledTimes(2);
});

test('a subscribed conference - video tracks come and go - nothing is reported', () => {
  const { conf, handlers } = makeLegacyConf();
  window.$conf = conf;
  init();
  frames = [];

  handlers['conference.trackAdded'](audioTrack('p1', 'video'));
  handlers['conference.trackRemoved'](audioTrack('p1', 'video'));
  handlers['conference.trackMuteChanged'](audioTrack('p1', 'video'));

  expect(frames).toEqual([]);
});

test('a subscribed conference - an audio track is removed or muted - reports speaker and participants', () => {
  const { conf, handlers } = makeLegacyConf();
  window.$conf = conf;
  init();
  frames = [];

  handlers['conference.trackRemoved'](audioTrack('p1'));
  handlers['conference.trackMuteChanged'](audioTrack('p1'));

  expect(sentTypes()).toEqual(['speaker_update', 'participants_update', 'speaker_update', 'participants_update']);
});

test('a subscribed conference - a user joins - subscribes to them and reports after a second', () => {
  const { conf, handlers, participant } = makeLegacyConf();
  window.$conf = conf;
  init();
  frames = [];

  handlers['conference.userJoined']('p1');
  handlers['conference.userJoined']('nobody');
  jest.advanceTimersByTime(999);
  expect(frames).toEqual([]);
  jest.advanceTimersByTime(1);

  expect(participant._activeSpeaker$.subscribe).toHaveBeenCalledTimes(2);
  expect(sentTypes().slice(0, 2)).toEqual(['speaker_update', 'participants_update']);
});

test('a subscribed conference - a user leaves - reports after half a second', () => {
  const { conf, handlers } = makeLegacyConf();
  window.$conf = conf;
  init();
  frames = [];

  handlers['conference.userLeft']('p1');
  jest.advanceTimersByTime(499);
  expect(frames).toEqual([]);
  jest.advanceTimersByTime(1);

  expect(sentTypes().slice(0, 2)).toEqual(['speaker_update', 'participants_update']);
});

test('an attached conference - the page drops it - the interval forgets it', () => {
  const { conf } = makeLegacyConf();
  init();
  window.$conf = conf;
  jest.advanceTimersByTime(2000);
  expect(state.activeConfRef).toBe(conf);
  expect(state.activeJitsiConf).toBe(conf.jitsiConf);
  state.conferenceReady = true;

  window.$conf = { jitsiConf: conf.jitsiConf };
  jest.advanceTimersByTime(2000);

  expect(state.activeConfRef).toBeNull();
  expect(state.activeJitsiConf).toBeNull();
  expect(state.conferenceReady).toBe(false);
});

test('an attached conference - the page replaces it - the interval attaches the new one', () => {
  const first = makeLegacyConf().conf;
  const second = makeLegacyConf();
  init();
  window.$conf = first;
  jest.advanceTimersByTime(2000);

  window.$conf = second.conf;
  jest.advanceTimersByTime(2000);

  expect(state.activeConfRef).toBe(second.conf);
  expect(second.conf._dominantSpeakerId$.subscribe).toHaveBeenCalled();
});
