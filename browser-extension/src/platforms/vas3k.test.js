/**
 * What the Jitsi Meet adapter (vas3k and configured Jitsi hosts) reads from
 * the page: the conference from the Redux store or APP.conference, the
 * speaker and participants from the store, and its speaker-change feed.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';
import { Vas3kAdapter } from './vas3k.js';

function makeRoom({ localTracks = [], participants = [] } = {}) {
  const handlers = {};
  return {
    handlers,
    on: jest.fn((event, handler) => {
      handlers[event] = handler;
    }),
    off: jest.fn(),
    getLocalTracks: () => localTracks,
    getParticipants: () => participants,
  };
}

function installApp({ room = null, participants = undefined, prejoin = false, legacyRoom = null } = {}) {
  const listeners = [];
  const store = {
    state: {
      'features/base/conference': { conference: room },
      'features/base/participants': participants,
      'features/prejoin': { showPrejoin: prejoin },
    },
    getState: () => store.state,
    subscribe: jest.fn((listener) => {
      listeners.push(listener);
      return jest.fn();
    }),
    listeners,
  };
  window.APP = { store, conference: { _room: legacyRoom } };
  return store;
}

let adapter;

beforeEach(() => {
  jest.spyOn(console, 'log').mockImplementation(() => {});
  adapter = new Vas3kAdapter();
});

afterEach(() => {
  delete window.APP;
  jest.restoreAllMocks();
  jest.useRealTimers();
});

describe('waitForConference', () => {
  beforeEach(() => {
    jest.useFakeTimers();
  });

  test('a joined conference in the store - waiting - resolves with it at once', async () => {
    const room = makeRoom();
    installApp({ room });

    await expect(adapter.waitForConference(3)).resolves.toBe(room);
    expect(adapter.room).toBe(room);
  });

  test('only the older APP.conference room - waiting - resolves with it', async () => {
    const legacyRoom = makeRoom();
    installApp({ legacyRoom });

    await expect(adapter.waitForConference(3)).resolves.toBe(legacyRoom);
  });

  test('the prejoin screen for 20 checks - waiting - logs the prejoin state and keeps waiting', () => {
    installApp({ prejoin: true });

    adapter.waitForConference(30);
    jest.advanceTimersByTime(19 * 500);

    expect(console.log).toHaveBeenCalledWith('[Vas3k Adapter] Waiting for conference... attempt 20, inPrejoin: true');
  });

  test('no conference - waiting the given checks - rejects', async () => {
    const waiting = adapter.waitForConference(3);
    jest.advanceTimersByTime(1000);

    await expect(waiting).rejects.toThrow('Conference not found');
  });
});

describe('getActiveSpeaker from remote participants', () => {
  test('a remote dominant speaker - asking - returns them with their id and name', () => {
    installApp({
      participants: {
        local: { id: 'me', dominantSpeaker: false },
        remote: new Map([
          ['r1', { id: 'r1', name: 'Quiet' }],
          ['r2', { id: 'r2', name: 'Bob', dominantSpeaker: true }],
        ]),
      },
    });

    expect(adapter.getActiveSpeaker()).toEqual({ id: 'r2', name: 'Bob', isLocal: false });
  });

  test('a remote dominant speaker without id or name - asking - names them by the map key', () => {
    installApp({ participants: { remote: new Map([['key1', { dominantSpeaker: true }]]) } });

    expect(adapter.getActiveSpeaker()).toEqual({ id: 'key1', name: 'key1', isLocal: false });
  });
});

describe('getParticipants', () => {
  test('local and remote participants - listing - names, speaking state and mics from the room', () => {
    const room = makeRoom({
      localTracks: [{ getType: () => 'audio', isMuted: () => false }],
      participants: [{ getId: () => 'r1', getTracks: () => [{ type: 'audio', isMuted: () => true }] }],
    });
    installApp({
      room,
      participants: {
        local: { id: 'me' },
        remote: new Map([
          ['r1', { id: 'r1', name: 'Bob', dominantSpeaker: true }],
          ['k2', {}],
        ]),
      },
    });

    expect(adapter.getParticipants()).toEqual([
      { id: 'me', name: 'You', isLocal: true, isSpeaking: false, micMuted: false },
      { id: 'r1', name: 'Bob', isLocal: false, isSpeaking: true, micMuted: true },
      { id: 'k2', name: 'k2', isLocal: false, isSpeaking: false, micMuted: true },
    ]);
  });
});

describe('subscribeToSpeakerChanges through the store', () => {
  test('the dominant speaker moves in the store - store updates - reports each change once', () => {
    const store = installApp({ participants: { local: { id: 'me' }, remote: new Map() } });
    const callback = jest.fn();
    adapter.subscribeToSpeakerChanges(callback);
    const update = (participants) => {
      store.state = { ...store.state, 'features/base/participants': participants };
      store.listeners[0]();
    };

    update({ local: { id: 'me' }, remote: new Map([['r1', { id: 'r1', dominantSpeaker: true }]]) });
    update({ local: { id: 'me' }, remote: new Map([['r1', { id: 'r1', dominantSpeaker: true }]]) });
    update({ local: { id: 'me', dominantSpeaker: true }, remote: new Map() });
    update({ local: { id: 'me' }, remote: new Map([['key2', { dominantSpeaker: true }]]) });
    update(undefined);

    expect(callback).toHaveBeenCalledTimes(4);
    expect(adapter.lastDominantSpeaker).toBeNull();
  });

  test('a conference room - subscribing - forwards its audio and user events', () => {
    jest.useFakeTimers();
    const room = makeRoom();
    installApp({ room });
    const callback = jest.fn();

    adapter.subscribeToSpeakerChanges(callback);
    room.handlers['conference.dominantSpeaker']('r1');
    room.handlers['conference.trackAdded']({ getType: () => 'audio' });
    room.handlers['conference.trackMuteChanged']({ getType: () => 'video' });
    room.handlers['conference.userLeft']('r1');
    expect(callback).toHaveBeenCalledTimes(2);
    jest.advanceTimersByTime(500);

    expect(callback).toHaveBeenCalledTimes(3);
  });
});
