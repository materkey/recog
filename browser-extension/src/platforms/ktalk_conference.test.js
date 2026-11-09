/**
 * What the KTalk adapter reads from the page's `window.$conf`: when the
 * conference counts as found, who speaks, whose microphone is open, the
 * participant list, and the subscriptions that follow late joiners.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';

const pcm = {
  pcmAttachParticipant: jest.fn(),
  pcmDetachParticipant: jest.fn(),
  pcmAttachAllFromConf: jest.fn(),
  pcmTransportDispose: jest.fn(),
  pcmUpdateParticipantName: jest.fn(),
};
const analyzer = {
  attachParticipant: jest.fn(),
  detachParticipant: jest.fn(),
  attachAllFromConf: jest.fn(),
  dispose: jest.fn(),
  isSpeech: jest.fn(),
  getFlatness: jest.fn(),
};

jest.unstable_mockModule('../pcm_transport.js', () => pcm);
jest.unstable_mockModule('../audio-analyzer.js', () => analyzer);

const { KTalkAdapter } = await import('./ktalk.js');

function observable(value) {
  const listeners = [];
  return {
    value,
    listeners,
    subscribe: jest.fn((listener) => {
      listeners.push(listener);
      return { unsubscribe: jest.fn() };
    }),
  };
}

function participant(id, extra = {}) {
  return {
    _id: id,
    _activeSpeaker$: observable(false),
    _audioMuted$: { value: true },
    ...extra,
  };
}

function audio(muted) {
  return { getType: () => 'audio', isMuted: () => muted };
}

function makeConf({ participants = [], local = null, jitsiParticipants = [], localTracks = [] } = {}) {
  const handlers = {};
  return {
    conf: {
      _participantsMap: new Map(participants.map((p) => [p._id, p])),
      localParticipant: local,
      _dominantSpeakerId$: observable(null),
      jitsiConf: {
        on: (event, handler) => {
          handlers[event] = handler;
        },
        off: () => {},
        getParticipants: () => jitsiParticipants,
        getLocalTracks: () => localTracks,
      },
    },
    handlers,
  };
}

let adapter;

beforeEach(() => {
  jest.clearAllMocks();
  jest.spyOn(console, 'log').mockImplementation(() => {});
  adapter = new KTalkAdapter();
});

afterEach(() => {
  delete window.$conf;
  jest.restoreAllMocks();
  jest.useRealTimers();
});

describe('waitForConference', () => {
  beforeEach(() => {
    jest.useFakeTimers();
  });

  test('a conference already on the page - waiting - resolves with it at once', async () => {
    window.$conf = makeConf().conf;

    await expect(adapter.waitForConference(3)).resolves.toBe(window.$conf);
    expect(adapter.conf).toBe(window.$conf);
  });

  test('a wrapper without its Jitsi conference - it gains one on the second check - resolves then', async () => {
    const { conf } = makeConf();
    window.$conf = { _dominantSpeakerId$: conf._dominantSpeakerId$ };
    const waiting = adapter.waitForConference(3);

    window.$conf = conf;
    jest.advanceTimersByTime(500);

    await expect(waiting).resolves.toBe(conf);
  });

  test('no conference - waiting the given checks - rejects', async () => {
    const waiting = adapter.waitForConference(3);
    jest.advanceTimersByTime(1000);

    await expect(waiting).rejects.toThrow('Conference not found');
  });
});

describe('getActiveSpeaker', () => {
  test('no conference - asking for the speaker - returns null', () => {
    expect(adapter.getActiveSpeaker()).toBeNull();
  });

  test('two participants flagged speaking - asking for the speaker - returns the last with its full name', () => {
    window.$conf = makeConf({
      participants: [
        participant('a', { _activeSpeaker$: observable(true), name: 'Plain A' }),
        participant('b', {
          _activeSpeaker$: observable(true),
          local: true,
          _participantInfo$: { value: { firstName: 'Grace', lastName: 'Hopper' } },
        }),
        participant('c'),
      ],
    }).conf;

    expect(adapter.getActiveSpeaker()).toEqual({ id: 'b', name: 'Grace Hopper', isLocal: true });
  });

  test('a speaking participant without names - asking for the speaker - names them by the map key', () => {
    window.$conf = makeConf({ participants: [participant('k', { _activeSpeaker$: observable(true) })] }).conf;

    expect(adapter.getActiveSpeaker().name).toBe('k');
  });
});

describe('anyMicOn', () => {
  test('no conference - checking the mics - returns false', () => {
    expect(adapter.anyMicOn()).toBe(false);
  });

  test('every microphone closed - checking the mics - returns false', () => {
    window.$conf = makeConf({
      participants: [participant('a'), participant('b', { _audioMuted$: undefined, _audioMuted: true })],
      jitsiParticipants: [{ getTracks: () => [audio(true)] }, { _tracks: [{ type: 'video' }] }],
      localTracks: [audio(true)],
    }).conf;

    expect(adapter.anyMicOn()).toBe(false);
  });

  test.each([
    ['a remote Jitsi audio track', { jitsiParticipants: [{ _tracks: [{ type: 'audio', isMuted: () => false }] }] }],
    ['the local audio track', { localTracks: [audio(false)] }],
    ['a participant observable', { participants: [participant('a', { _audioMuted$: { value: false } })] }],
    ['a participant flag', { participants: [participant('a', { _audioMuted$: undefined, _audioMuted: false })] }],
  ])('a microphone open by %s - checking the mics - returns true', (_name, setup) => {
    window.$conf = makeConf(setup).conf;

    expect(adapter.anyMicOn()).toBe(true);
  });
});

describe('getParticipants', () => {
  test('no conference or an empty map - listing - returns nobody', () => {
    expect(adapter.getParticipants()).toEqual([]);
    window.$conf = makeConf({ jitsiParticipants: [{ _id: 'j1' }] }).conf;

    expect(adapter.getParticipants()).toEqual([]);
  });

  test('local, remote and Jitsi-only participants - listing - names and flags each once', () => {
    window.$conf = makeConf({
      local: { id: 'me', _userInfo$: { value: { firstName: 'Kate' } } },
      participants: [
        participant('me', { _activeSpeaker$: observable(true) }),
        participant('r1', {
          _audioMuted$: { value: false },
          _userInfo$: { value: { firstName: 'Ada', lastName: 'Lovelace' } },
        }),
        participant('r2', { displayName: 'Display Two' }),
        participant('r3', { _participantInfo$: { value: { firstName: 'Alan' } } }),
        participant('r4'),
      ],
      jitsiParticipants: [
        { _id: 'r1', getDisplayName: () => 'Duplicate' },
        { getId: () => 'j1', getDisplayName: () => 'Jitsi One' },
        { id: 'j2', _displayName: 'Jitsi Two' },
        { _id: 'j3', displayName: 'Jitsi Three' },
        { _id: 'j4', _name: 'Jitsi Four' },
        { _id: 'j5' },
        {},
      ],
      localTracks: [audio(false)],
    }).conf;

    expect(adapter.getParticipants()).toEqual([
      { id: 'me', name: 'Kate', isLocal: true, isSpeaking: true, micMuted: false },
      { id: 'r1', name: 'Ada Lovelace', isLocal: false, isSpeaking: false, micMuted: false },
      { id: 'r2', name: 'Display Two', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'r3', name: 'Alan', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'r4', name: 'r4', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'j1', name: 'Jitsi One', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'j2', name: 'Jitsi Two', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'j3', name: 'Jitsi Three', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'j4', name: 'Jitsi Four', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'j5', name: 'j5', isLocal: false, isSpeaking: false, micMuted: true },
    ]);
  });

  test('a local participant without names - listing - calls them You', () => {
    window.$conf = makeConf({ local: { id: 'me' }, participants: [participant('me')] }).conf;

    expect(adapter.getParticipants()[0].name).toBe('You');
  });
});

describe('subscribeToSpeakerChanges', () => {
  test('participants on the page - subscribing - follows their speaking state and remote names', () => {
    const remote = participant('r1', { _userInfo$: observable({ firstName: 'Ada' }) });
    const local = participant('me', { local: true, _userInfo$: observable({ firstName: 'Me' }) });
    const quiet = { _id: 'q1', local: false };
    const { conf } = makeConf({ participants: [remote, local, quiet] });
    window.$conf = conf;
    const callback = jest.fn();

    adapter.subscribeToSpeakerChanges(callback);
    remote._activeSpeaker$.listeners[0]();
    conf._dominantSpeakerId$.listeners[0]();
    remote._userInfo$.listeners[0]();

    expect(callback).toHaveBeenCalledTimes(2);
    expect(local._activeSpeaker$.subscribe).toHaveBeenCalled();
    expect(local._userInfo$.subscribe).not.toHaveBeenCalled();
    expect(pcm.pcmUpdateParticipantName).toHaveBeenCalledWith('r1', 'Ada');
    expect(analyzer.attachAllFromConf).toHaveBeenCalledWith(conf);
    expect(pcm.pcmAttachAllFromConf).toHaveBeenCalledWith(conf);
  });

  test('a subscribed conference - users join - refreshes remote names after a second and reports', () => {
    jest.useFakeTimers();
    const late = participant('late', { _participantInfo$: observable({ firstName: 'Late' }) });
    const localLate = participant('me', { local: true, _participantInfo$: observable({ firstName: 'Me' }) });
    const { conf, handlers } = makeConf();
    window.$conf = conf;
    const callback = jest.fn();
    adapter.subscribeToSpeakerChanges(callback);
    conf._participantsMap.set('late', late);
    conf._participantsMap.set('me', localLate);

    handlers['conference.userJoined']('late');
    handlers['conference.userJoined']('me');
    handlers['conference.userJoined']('unknown');
    handlers['conference.userJoined'](null);
    jest.advanceTimersByTime(999);
    expect(callback).not.toHaveBeenCalled();
    jest.advanceTimersByTime(1);

    expect(callback).toHaveBeenCalledTimes(4);
    expect(late._participantInfo$.subscribe).toHaveBeenCalledTimes(1);
    expect(localLate._participantInfo$.subscribe).not.toHaveBeenCalled();
  });
});

describe('onParticipantJoined', () => {
  test('a conference - users join - subscribes to known ones and reports each id after a second', () => {
    jest.useFakeTimers();
    const joined = participant('j1');
    const silent = { _id: 'j2' };
    const { conf, handlers } = makeConf();
    window.$conf = conf;
    const callback = jest.fn();
    adapter.onParticipantJoined(callback);
    conf._participantsMap.set('j1', joined);
    conf._participantsMap.set('j2', silent);

    handlers['conference.userJoined']('j1');
    handlers['conference.userJoined']('j2');
    handlers['conference.userJoined']('unknown');
    jest.advanceTimersByTime(1000);

    expect(callback.mock.calls).toEqual([['j1'], ['j2'], ['unknown']]);
    expect(joined._activeSpeaker$.subscribe).toHaveBeenCalledTimes(1);
    expect(adapter.subscriptions).toHaveLength(1);
  });
});
