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
  getFullName: jest.fn((_p, _i, fallback) => fallback),
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
} = await import('./speaker.js');

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
