/**
 * Tests for speaker detection logic — sendFilteredSpeakerUpdate, getActiveSpeaker, isMicMuted.
 *
 * Key scenario: Jitsi keeps _activeSpeaker$ pointing at the last dominant speaker even
 * after they mute.  sendFilteredSpeakerUpdate must suppress such ghost speakers.
 */

import { jest } from '@jest/globals';

// ---------------------------------------------------------------------------
// Mocks — declared before imports so jest.unstable_mockModule can intercept
// ---------------------------------------------------------------------------

const mockSendWsSpeakerUpdate = jest.fn();
const mockSendWsParticipants = jest.fn();

jest.unstable_mockModule('./ws_api.js', () => ({
  sendWsSpeakerUpdate: mockSendWsSpeakerUpdate,
  sendWsParticipants: mockSendWsParticipants,
  sendWsMessage: jest.fn(),
}));

jest.unstable_mockModule('./utils.js', () => ({
  sendUpdate: jest.fn(),
}));

jest.unstable_mockModule('./ui-vas3k.js', () => ({
  updateVas3kSpeakerStatus: jest.fn(),
  showVas3kWaitingState: jest.fn(),
}));

const mockIsSpeech = jest.fn(() => ({ speech: true, reason: 'speech' }));
const mockGetFlatness = jest.fn(() => 0.05);

jest.unstable_mockModule('./audio-analyzer.js', () => ({
  isSpeech: mockIsSpeech,
  getFlatness: mockGetFlatness,
}));

// Platform adapter mock — returned by getPlatformAdapter
let mockAdapter = null;

jest.unstable_mockModule('./platforms/index.js', () => ({
  getPlatformAdapter: jest.fn(() => mockAdapter),
  getPlatform: jest.fn(() => 'ktalk'),
  isJitsiPlatform: jest.fn(() => false),
}));

// ---------------------------------------------------------------------------
// Import the module under test (after all mocks are registered)
// ---------------------------------------------------------------------------

const {
  sendFilteredSpeakerUpdate,
  getActiveSpeaker,
  isMicMuted,
  anyMicOn,
  sendParticipantsUpdate,
  getParticipantsList,
  updateSpeakerStatus,
} = await import('./speaker.js');
const { isJitsiPlatform } = await import('./platforms/index.js');
const { updateVas3kSpeakerStatus, showVas3kWaitingState } = await import('./ui-vas3k.js');

const { state, constants } = await import('./state.js');

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeSpeaker({ id = 'p1', name = 'Speaker 1', isLocal = false } = {}) {
  return { id, name, isLocal };
}

/**
 * Build a minimal KTalk-like conference object.
 *
 * participants: array of { id, name, isLocal, isSpeaking, micMuted }
 */
function makeConf(participants = []) {
  const map = new Map();
  let localParticipant = null;

  for (const p of participants) {
    const entry = {
      _id: p.id,
      name: p.name,
      local: p.isLocal ?? false,
      _activeSpeaker$: { value: p.isSpeaking ?? false },
      _audioMuted$: { value: p.micMuted ?? true },
      _participantInfo$: { value: null },
      _userInfo$: { value: null },
    };
    map.set(p.id, entry);
    if (p.isLocal) {
      localParticipant = { id: p.id, name: p.name, displayName: p.name };
    }
  }

  return {
    _participantsMap: map,
    localParticipant,
    jitsiConf: {
      getParticipants: () => [],
      getLocalTracks: () => [],
    },
  };
}

/**
 * Build a mock platform adapter backed by a conference object.
 * Mirrors the real adapter API (isMicMuted, getConference, anyMicOn, getActiveSpeaker).
 */
function makeAdapterForConf(conf) {
  return {
    getConference: () => conf,
    isMicMuted: (participantId) => {
      const p = conf?._participantsMap?.get(participantId);
      if (p?._audioMuted$?.value !== undefined) return p._audioMuted$.value === true;
      // Fallback: if actively speaking, not muted
      if (p?._activeSpeaker$?.value === true) return false;
      return true;
    },
    anyMicOn: () => {
      let found = false;
      conf?._participantsMap?.forEach((p) => {
        if (p._audioMuted$?.value === false) found = true;
      });
      return found;
    },
    getActiveSpeaker: () => {
      let active = null;
      conf?._participantsMap?.forEach((p, key) => {
        if (p._activeSpeaker$?.value === true) {
          active = { id: p._id, name: p.name || key, isLocal: p.local };
        }
      });
      return active;
    },
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe('sendFilteredSpeakerUpdate', () => {
  beforeEach(() => {
    mockSendWsSpeakerUpdate.mockClear();
    mockIsSpeech.mockReset();
    mockIsSpeech.mockReturnValue({ speech: true, reason: 'speech' });
    mockGetFlatness.mockReset();
    mockGetFlatness.mockReturnValue(0.05);
    mockAdapter = null;
    state.audioFilterSuppressCount = 0;
    state.audioFilterLastFlatness = null;
    state.audioFilterLastReason = null;
    constants.AUDIO_FILTER_ENABLED = true;
    delete window.$conf;
  });

  test('given muted speaker — sends null instead of speaker', () => {
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', micMuted: true, isSpeaking: true },
      { id: 'p2', name: 'Speaker 2', micMuted: false, isSpeaking: false },
    ]);
    mockAdapter = makeAdapterForConf(conf);

    sendFilteredSpeakerUpdate(makeSpeaker({ id: 'p1', name: 'Speaker 1' }));

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledTimes(1);
    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(null);
  });

  test('given unmuted speaker — sends speaker through', () => {
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', micMuted: false, isSpeaking: true },
    ]);
    mockAdapter = makeAdapterForConf(conf);

    const speaker = makeSpeaker({ id: 'p1', name: 'Speaker 1' });
    sendFilteredSpeakerUpdate(speaker);

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledTimes(1);
    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(speaker);
  });

  test('given null speaker — sends null (no crash)', () => {
    sendFilteredSpeakerUpdate(null);

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledTimes(1);
    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(null);
  });

  test('given unmuted speaker with noise — audio filter suppresses', () => {
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', micMuted: false, isSpeaking: true },
    ]);
    mockAdapter = makeAdapterForConf(conf);
    mockIsSpeech.mockReturnValueOnce({ speech: false, reason: 'noise' });

    sendFilteredSpeakerUpdate(makeSpeaker({ id: 'p1', name: 'Speaker 1' }));

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(null);
    expect(state.audioFilterSuppressCount).toBe(1);
  });

  test('given unmuted speaker with speech — audio filter passes through', () => {
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', micMuted: false, isSpeaking: true },
    ]);
    mockAdapter = makeAdapterForConf(conf);

    const speaker = makeSpeaker({ id: 'p1', name: 'Speaker 1' });
    sendFilteredSpeakerUpdate(speaker);

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(speaker);
  });

  test('given audio filter disabled — sends unmuted speaker without isSpeech check', () => {
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', micMuted: false, isSpeaking: true },
    ]);
    mockAdapter = makeAdapterForConf(conf);
    constants.AUDIO_FILTER_ENABLED = false;

    const speaker = makeSpeaker({ id: 'p1', name: 'Speaker 1' });
    sendFilteredSpeakerUpdate(speaker);

    expect(mockIsSpeech).not.toHaveBeenCalled();
    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(speaker);
  });

  test('given no adapter — falls back to window.$conf for mute check', () => {
    mockAdapter = null;

    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', micMuted: true, isSpeaking: true },
    ]);
    window.$conf = conf;

    sendFilteredSpeakerUpdate(makeSpeaker({ id: 'p1', name: 'Speaker 1' }));

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(null);
  });

  test('given no adapter and no window.$conf — sends speaker (no conf to check mute)', () => {
    mockAdapter = null;

    const speaker = makeSpeaker({ id: 'p1', name: 'Speaker 1' });
    sendFilteredSpeakerUpdate(speaker);

    // No conf → can't check mute → falls through to audio filter → speech:true → sends speaker
    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(speaker);
  });

  test('mute check runs before audio filter — muted speaker never reaches isSpeech', () => {
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', micMuted: true, isSpeaking: true },
    ]);
    mockAdapter = makeAdapterForConf(conf);

    sendFilteredSpeakerUpdate(makeSpeaker({ id: 'p1' }));

    expect(mockIsSpeech).not.toHaveBeenCalled();
    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(null);
  });
});

describe('isMicMuted (legacy fallback, no adapter)', () => {
  beforeEach(() => {
    mockAdapter = null;
  });

  test('given participant with _audioMuted$ true — returns true', () => {
    const conf = makeConf([{ id: 'p1', name: 'A', micMuted: true }]);
    expect(isMicMuted(conf, 'p1')).toBe(true);
  });

  test('given participant with _audioMuted$ false — returns false', () => {
    const conf = makeConf([{ id: 'p1', name: 'A', micMuted: false }]);
    expect(isMicMuted(conf, 'p1')).toBe(false);
  });

  test('given actively speaking participant without _audioMuted$ — returns false', () => {
    const conf = makeConf([{ id: 'p1', name: 'A', isSpeaking: true, micMuted: false }]);
    const participant = conf._participantsMap.get('p1');
    delete participant._audioMuted$;
    delete participant._audioMuted;

    expect(isMicMuted(conf, 'p1')).toBe(false);
  });

  test('given unknown participant — returns true (defensive)', () => {
    const conf = makeConf([]);
    expect(isMicMuted(conf, 'unknown')).toBe(true);
  });
});

describe('getActiveSpeaker', () => {
  beforeEach(() => {
    mockAdapter = null;
  });

  test('given adapter — delegates to adapter.getActiveSpeaker', () => {
    const expected = { id: 'p1', name: 'Speaker 1', isLocal: false };
    mockAdapter = { getActiveSpeaker: jest.fn(() => expected) };

    const result = getActiveSpeaker({});

    expect(mockAdapter.getActiveSpeaker).toHaveBeenCalled();
    expect(result).toBe(expected);
  });

  test('given no adapter — finds active speaker from _participantsMap', () => {
    mockAdapter = null;
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', isSpeaking: false },
      { id: 'p2', name: 'Speaker 2', isSpeaking: true },
    ]);

    const result = getActiveSpeaker(conf);

    expect(result).not.toBeNull();
    expect(result.id).toBe('p2');
  });

  test('given no active speaker — returns null', () => {
    mockAdapter = null;
    const conf = makeConf([
      { id: 'p1', name: 'Speaker 1', isSpeaking: false },
    ]);

    const result = getActiveSpeaker(conf);
    expect(result).toBeNull();
  });
});

describe('sendParticipantsUpdate', () => {
  beforeEach(() => {
    mockSendWsParticipants.mockClear();
  });

  test('given adapter with getLocalMicMuted — sends its answer with the participants', () => {
    const participants = [{ id: 'Host', name: 'Host', isLocal: false, micMuted: false }];
    mockAdapter = { getParticipants: () => participants, getLocalMicMuted: () => true };

    sendParticipantsUpdate({});

    expect(mockSendWsParticipants).toHaveBeenCalledWith(participants, true);
  });

  test('given adapter without getLocalMicMuted — sends no own-microphone state', () => {
    mockAdapter = { getParticipants: () => [] };

    sendParticipantsUpdate({});

    expect(mockSendWsParticipants).toHaveBeenCalledWith([], undefined);
  });
});

describe('anyMicOn', () => {
  beforeEach(() => {
    mockAdapter = null;
  });

  test('given adapter — delegates to adapter.anyMicOn', () => {
    mockAdapter = { anyMicOn: jest.fn(() => true) };

    expect(anyMicOn({})).toBe(true);
    expect(mockAdapter.anyMicOn).toHaveBeenCalled();
  });

  test('given no adapter and all participants muted — returns false when conferenceReady', () => {
    mockAdapter = null;
    state.conferenceReady = true;
    const conf = makeConf([
      { id: 'p1', name: 'A', micMuted: true },
      { id: 'p2', name: 'B', micMuted: true },
    ]);
    expect(anyMicOn(conf)).toBe(false);
  });

  test('given no adapter and all muted but conference not ready — returns true (defensive default)', () => {
    mockAdapter = null;
    state.conferenceReady = false;
    const conf = makeConf([
      { id: 'p1', name: 'A', micMuted: true },
    ]);
    // Legacy fallback: no audio tracks found + not ready → assumes mics are on
    expect(anyMicOn(conf)).toBe(true);
  });

  test('given no adapter and one participant unmuted — returns true', () => {
    mockAdapter = null;
    const conf = makeConf([
      { id: 'p1', name: 'A', micMuted: true },
      { id: 'p2', name: 'B', micMuted: false },
    ]);
    expect(anyMicOn(conf)).toBe(true);
  });
});

describe('Scenario: muted ghost speaker attributed instead of real speaker', () => {
  beforeEach(() => {
    mockSendWsSpeakerUpdate.mockClear();
    mockIsSpeech.mockReset();
    mockIsSpeech.mockReturnValue({ speech: true, reason: 'speech' });
    mockGetFlatness.mockReset();
    mockGetFlatness.mockReturnValue(0.05);
    constants.AUDIO_FILTER_ENABLED = true;
    state.audioFilterSuppressCount = 0;
    delete window.$conf;
  });

  test('Jitsi reports muted participant as active — sendFilteredSpeakerUpdate sends null', () => {
    // Jitsi's _activeSpeaker$ still points at the last dominant speaker (muted_remote)
    // even though they muted. Another participant is speaking but Jitsi hasn't switched yet.
    const conf = makeConf([
      { id: 'muted_remote', name: 'Muted Remote', micMuted: true, isSpeaking: true },
      { id: 'local_user', name: 'Local User', micMuted: false, isLocal: true, isSpeaking: false },
      { id: 'real_speaker', name: 'Real Speaker', micMuted: false, isSpeaking: false },
    ]);
    mockAdapter = makeAdapterForConf(conf);

    sendFilteredSpeakerUpdate({ id: 'muted_remote', name: 'Muted Remote', isLocal: false });

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(null);
  });

  test('Jitsi switches to real speaker — sendFilteredSpeakerUpdate sends them', () => {
    // The real speaker speaks loudly enough → Jitsi switches dominant speaker
    const conf = makeConf([
      { id: 'muted_remote', name: 'Muted Remote', micMuted: true, isSpeaking: false },
      { id: 'real_speaker', name: 'Real Speaker', micMuted: false, isSpeaking: true },
    ]);
    mockAdapter = makeAdapterForConf(conf);

    const speaker = { id: 'real_speaker', name: 'Real Speaker', isLocal: false };
    sendFilteredSpeakerUpdate(speaker);

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(speaker);
  });

  test('all mics off — even active speaker is suppressed', () => {
    const conf = makeConf([
      { id: 'muted_remote', name: 'Muted Remote', micMuted: true, isSpeaking: true },
      { id: 'local_user', name: 'Local User', micMuted: true, isLocal: true, isSpeaking: false },
    ]);
    mockAdapter = makeAdapterForConf(conf);

    sendFilteredSpeakerUpdate({ id: 'muted_remote', name: 'Muted Remote', isLocal: false });

    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith(null);
  });
});

describe('getParticipantsList (legacy fallback, no adapter)', () => {
  beforeEach(() => {
    mockAdapter = null;
  });

  function participant(id, extra = {}) {
    return {
      _id: id,
      _activeSpeaker$: { value: false },
      _audioMuted$: { value: true },
      ...extra,
    };
  }

  test('given an adapter - listing participants - returns its list and records the size', () => {
    const listed = [{ id: 'a' }, { id: 'b' }];
    mockAdapter = { getParticipants: () => listed };

    expect(getParticipantsList({})).toBe(listed);
    expect(state.lastParticipantsMapSize).toBe(2);
  });

  test('given an empty participants map - listing participants - returns nobody', () => {
    const conf = { _participantsMap: new Map(), jitsiConf: { getParticipants: () => [{ _id: 'j1' }] } };

    expect(getParticipantsList(conf)).toEqual([]);
    expect(state.lastParticipantsMapSize).toBe(0);
  });

  test('given local and remote participants - listing - names them from user info, participant info, name and key', () => {
    const map = new Map([
      ['me', participant('me', { _activeSpeaker$: { value: true }, _audioMuted$: { value: false } })],
      ['full', participant('full', { _userInfo$: { value: { firstName: 'Ada', lastName: 'Lovelace' } } })],
      ['first', participant('first', { _userInfo$: { value: { firstName: 'Alan' } } })],
      ['info', participant('info', {
        _userInfo$: { value: { lastName: 'Only' } },
        _participantInfo$: { value: { firstName: 'Grace', lastName: 'Hopper' } },
      })],
      ['infoFirst', participant('infoFirst', { _participantInfo$: { value: { firstName: 'Edsger' } } })],
      ['named', participant('named', { name: 'Plain Name' })],
      ['display', participant('display', { displayName: 'Display Name' })],
      ['key-only', participant('key-only')],
    ]);
    const conf = {
      _participantsMap: map,
      localParticipant: { id: 'me' },
      jitsiConf: { getParticipants: () => [] },
    };

    expect(getParticipantsList(conf)).toEqual([
      { id: 'me', name: 'You', isLocal: true, isSpeaking: true, micMuted: false },
      { id: 'full', name: 'Ada Lovelace', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'first', name: 'Alan', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'info', name: 'Grace Hopper', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'infoFirst', name: 'Edsger', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'named', name: 'Plain Name', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'display', name: 'Display Name', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'key-only', name: 'key-only', isLocal: false, isSpeaking: false, micMuted: true },
    ]);
    expect(state.lastParticipantsMapSize).toBe(8);
  });

  test('given a local participant with names of its own - listing - prefers user info, then its name', () => {
    const map = new Map([['me', participant('me')]]);
    const named = { _participantsMap: map, localParticipant: { id: 'me', displayName: 'Me Display' } };
    const known = {
      _participantsMap: map,
      localParticipant: { id: 'me', name: 'Me', _userInfo$: { value: { firstName: 'Kate', lastName: 'Bell' } } },
    };

    expect(getParticipantsList(named)[0].name).toBe('Me Display');
    expect(getParticipantsList(known)[0].name).toBe('Kate Bell');
  });

  test('given Jitsi participants missing from the map - listing - adds them once with their display names', () => {
    const map = new Map([['m1', participant('m1', { name: 'Mapped' })]]);
    const jitsiList = [
      { _id: 'm1', getDisplayName: () => 'Duplicate' },
      { getId: () => 'j1', getDisplayName: () => 'Jitsi One' },
      { id: 'j2', _displayName: 'Jitsi Two' },
      { _id: 'j3', displayName: 'Jitsi Three' },
      { _id: 'j4', _name: 'Jitsi Four' },
      { _id: 'j5' },
      {},
    ];
    const conf = {
      _participantsMap: map,
      jitsiConf: {
        getParticipants: () => jitsiList,
        participants: new Map([
          ['j1', { _id: 'j1' }],
          ['j6', { _id: 'j6', getDisplayName: () => 'From Map' }],
          ['j7', { getId: () => 'j7', _name: 'Map Name' }],
          ['nil', null],
        ]),
      },
    };

    const listed = getParticipantsList(conf);

    expect(listed.map((p) => [p.id, p.name])).toEqual([
      ['m1', 'Mapped'],
      ['j1', 'Jitsi One'],
      ['j2', 'Jitsi Two'],
      ['j3', 'Jitsi Three'],
      ['j4', 'Jitsi Four'],
      ['j5', 'j5'],
      ['j6', 'From Map'],
      ['j7', 'Map Name'],
    ]);
    expect(listed.every((p) => p.isLocal === false && p.isSpeaking === false && p.micMuted === true)).toBe(true);
    expect(state.lastJitsiParticipantsCount).toBe(7);
    expect(state.lastJitsiParticipantsMapCount).toBe(4);
  });

  test('given Jitsi participants kept in a plain object - listing - reads its values', () => {
    const map = new Map([['m1', participant('m1')]]);
    const conf = {
      _participantsMap: map,
      jitsiConf: { _participants: { a: { id: 'o1', displayName: 'Object One' } } },
    };

    expect(getParticipantsList(conf).map((p) => p.name)).toEqual(['m1', 'Object One']);
    expect(state.lastJitsiParticipantsCount).toBe(0);
    expect(state.lastJitsiParticipantsMapCount).toBe(1);
  });
});

describe('updateSpeakerStatus', () => {
  let dot;
  let nameEl;
  let now;
  let dateSpy;

  beforeEach(() => {
    mockAdapter = null;
    mockSendWsSpeakerUpdate.mockClear();
    constants.AUDIO_FILTER_ENABLED = false;
    state.conferenceReady = true;
    state.currentSpeaker = null;
    state.speakingStartTime = null;
    state.speakerHistory = [];
    document.body.innerHTML = '<span class="ktalk-speaker-dot"></span><span class="ktalk-speaker-name"></span>';
    dot = document.querySelector('.ktalk-speaker-dot');
    nameEl = document.querySelector('.ktalk-speaker-name');
    now = 1_000_000;
    dateSpy = jest.spyOn(Date, 'now').mockImplementation(() => now);
    delete window.$conf;
  });

  afterEach(() => {
    dateSpy.mockRestore();
    constants.AUDIO_FILTER_ENABLED = true;
    jest.mocked(isJitsiPlatform).mockReturnValue(false);
  });

  test('given an unmuted active speaker - updating - marks the dot speaking, shows the name and reports them', () => {
    const conf = makeConf([{ id: 'p1', name: 'Alice', isSpeaking: true, micMuted: false }]);

    updateSpeakerStatus(conf);

    expect(dot.classList.contains('speaking')).toBe(true);
    expect(nameEl.textContent).toBe('Alice');
    expect(state.currentSpeaker).toBe('p1');
    expect(state.speakingStartTime).toBe(now);
    expect(state.lastActiveSpeaker).toEqual({ id: 'p1', name: 'Alice', isLocal: false });
    expect(state.lastParticipants.map((p) => p.id)).toEqual(['p1']);
    expect(mockSendWsSpeakerUpdate).toHaveBeenCalledWith({ id: 'p1', name: 'Alice', isLocal: false });
  });

  test('given a muted active speaker and another open mic - updating - shows no one speaking', () => {
    dot.classList.add('speaking');
    const conf = makeConf([
      { id: 'p1', name: 'Alice', isSpeaking: true, micMuted: true },
      { id: 'p2', name: 'Bob', micMuted: false },
    ]);

    updateSpeakerStatus(conf);

    expect(dot.classList.contains('speaking')).toBe(false);
    expect(nameEl.textContent).toBe('No one speaking');
  });

  test('given a muted active speaker and every mic closed - updating - shows all mics off', () => {
    const conf = makeConf([{ id: 'p1', name: 'Alice', isSpeaking: true, micMuted: true }]);

    updateSpeakerStatus(conf);

    expect(nameEl.textContent).toBe('All mics off');
  });

  test('given nobody speaking - updating - shows all mics off or no one speaking by the mics', () => {
    const logSpy = jest.spyOn(console, 'log').mockImplementation(() => {});
    const closed = makeConf([{ id: 'p1', name: 'Alice', micMuted: true }]);
    closed.jitsiConf = {
      getParticipants: () => [{ _id: 'j1', getTracks: () => [{ getType: () => 'audio', isMuted: () => true }] }],
      getLocalTracks: () => [{ getType: () => 'audio', isMuted: () => true }],
    };

    updateSpeakerStatus(closed);
    expect(nameEl.textContent).toBe('All mics off');
    expect(logSpy).toHaveBeenCalledWith('[KTalk Debug] All mics off detected:', expect.objectContaining({
      jitsiParticipantsCount: 1,
      localTracksCount: 1,
      participants: [{ id: 'j1', tracks: [{ type: 'audio', muted: true }] }],
      localTracks: [{ type: 'audio', muted: true }],
    }));

    updateSpeakerStatus(makeConf([{ id: 'p1', name: 'Alice', micMuted: false }]));
    expect(nameEl.textContent).toBe('No one speaking');
    expect(mockSendWsSpeakerUpdate).toHaveBeenLastCalledWith(null);
    logSpy.mockRestore();
  });

  test('given a speaker who talked long enough - another takes over - records the first in the history', () => {
    const conf = makeConf([
      { id: 'p1', name: 'Alice', isSpeaking: true, micMuted: false },
      { id: 'p2', name: 'Bob', micMuted: false },
    ]);
    updateSpeakerStatus(conf);
    now += 2000;
    conf._participantsMap.get('p1')._activeSpeaker$.value = false;
    conf._participantsMap.get('p2')._activeSpeaker$.value = true;

    updateSpeakerStatus(conf);

    expect(state.currentSpeaker).toBe('p2');
    expect(state.speakerHistory).toEqual([
      expect.objectContaining({ name: 'Alice', id: 'p1', durationMs: 2000 }),
    ]);
  });

  test('given a speaker who talked too briefly - they stop - records nothing and clears the speaker', () => {
    const conf = makeConf([{ id: 'p1', name: 'Alice', isSpeaking: true, micMuted: false }]);
    updateSpeakerStatus(conf);
    now += constants.SPEAKING_THRESHOLD_MS - 1;
    conf._participantsMap.get('p1')._activeSpeaker$.value = false;

    updateSpeakerStatus(conf);

    expect(state.speakerHistory).toEqual([]);
    expect(state.currentSpeaker).toBeNull();
    expect(state.speakingStartTime).toBeNull();
  });

  test('given a local speaker - they stop - records them under the local name', () => {
    const conf = makeConf([{ id: 'me', name: 'Me', isLocal: true, isSpeaking: true, micMuted: false }]);
    updateSpeakerStatus(conf);
    now += 1000;
    conf._participantsMap.get('me')._activeSpeaker$.value = false;

    updateSpeakerStatus(conf);

    expect(state.speakerHistory.map((h) => h.name)).toEqual(['Me']);
  });

  test('given a speaker who left the map - they stop - records them under their id', () => {
    const conf = makeConf([{ id: 'p1', name: 'Alice', isSpeaking: true, micMuted: false }]);
    updateSpeakerStatus(conf);
    now += 1000;
    conf._participantsMap.delete('p1');
    conf._participantsMap.set('p2', { _id: 'p2', _activeSpeaker$: { value: false }, _audioMuted$: { value: false } });

    updateSpeakerStatus(conf);

    expect(state.speakerHistory.map((h) => h.name)).toEqual(['p1']);
  });

  test('given a full history - another speaker ends - drops the oldest entry', () => {
    state.speakerHistory = Array.from({ length: constants.MAX_HISTORY }, (_, i) => ({ name: `old${i}` }));
    const conf = makeConf([{ id: 'p1', name: 'Alice', isSpeaking: true, micMuted: false }]);
    updateSpeakerStatus(conf);
    now += 1000;
    conf._participantsMap.get('p1')._activeSpeaker$.value = false;

    updateSpeakerStatus(conf);

    expect(state.speakerHistory).toHaveLength(constants.MAX_HISTORY);
    expect(state.speakerHistory[0].name).toBe('Alice');
    expect(state.speakerHistory.at(-1).name).toBe(`old${constants.MAX_HISTORY - 2}`);
  });

  test('given an adapter - a speaker ends - takes the name from the adapter', () => {
    const speaker = { id: 'p1', name: 'Alice', isLocal: false };
    mockAdapter = {
      getActiveSpeaker: jest.fn(() => speaker),
      anyMicOn: () => true,
      isMicMuted: () => false,
      getParticipants: () => [],
      getParticipantName: jest.fn(() => 'Adapter Alice'),
      getConference: () => null,
    };
    updateSpeakerStatus({});
    now += 1000;
    mockAdapter.getActiveSpeaker.mockReturnValue(null);

    updateSpeakerStatus({});

    expect(mockAdapter.getParticipantName).toHaveBeenCalledWith('p1');
    expect(state.speakerHistory.map((h) => h.name)).toEqual(['Adapter Alice']);
  });

  test('given a floating-panel platform with a diagnostic - updating - shows the diagnostic over the speaker', () => {
    jest.mocked(isJitsiPlatform).mockReturnValue(true);
    const speaker = { id: 'p1', name: 'Alice', isLocal: false };
    mockAdapter = {
      getActiveSpeaker: () => speaker,
      anyMicOn: () => true,
      isMicMuted: () => false,
      getParticipants: () => [],
      getConference: () => null,
      getDiagnostic: () => 'selectors drifted',
    };

    updateSpeakerStatus({});

    expect(updateVas3kSpeakerStatus).toHaveBeenCalledWith(speaker, true);
    expect(showVas3kWaitingState).toHaveBeenCalledWith('selectors drifted');
  });

  test('given a floating-panel platform without a diagnostic - updating - leaves the speaker line alone', () => {
    jest.mocked(isJitsiPlatform).mockReturnValue(true);
    jest.mocked(showVas3kWaitingState).mockClear();
    mockAdapter = {
      getActiveSpeaker: () => null,
      anyMicOn: () => false,
      isMicMuted: () => true,
      getParticipants: () => [],
      getConference: () => null,
    };

    updateSpeakerStatus({});

    expect(updateVas3kSpeakerStatus).toHaveBeenCalledWith(null, false);
    expect(showVas3kWaitingState).not.toHaveBeenCalled();
  });
});
