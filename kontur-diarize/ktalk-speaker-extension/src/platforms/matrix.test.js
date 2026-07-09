/**
 * Tests for the Element Call (Matrix) adapter pure helpers and the adapter's
 * conference-object contract.
 *
 * Pure helpers (parseRtcIdentity, resolveDisplayName, membershipToParticipant,
 * buildMeetingUrl, buildMeetingTitle) and the React fiber walk
 * (getFiberFromNode, walkFiberForProps, findLivekitRoom, findTrackRefs) are
 * unit-tested here; the live fiber-walk against a real Element Call bundle is
 * covered by the manual e2e task.
 *
 * @jest-environment jsdom
 */

import {
  parseRtcIdentity,
  resolveDisplayName,
  membershipToParticipant,
  buildMeetingUrl,
  buildMeetingTitle,
  getFiberFromNode,
  walkFiberForProps,
  findLivekitRoom,
  findTrackRefs,
  ElementCallAdapter,
} from './matrix.js';

// ---------------------------------------------------------------------------
// parseRtcIdentity
// ---------------------------------------------------------------------------

describe('parseRtcIdentity', () => {
  test('parses the legacy plain `${userId}:${deviceId}` format', () => {
    expect(parseRtcIdentity('@test2:matrix.vkkovalev.work:IZFUKTQLPX')).toEqual({
      userId: '@test2:matrix.vkkovalev.work',
      deviceId: 'IZFUKTQLPX',
    });
  });

  test('parses a legacy identity whose homeserver carries a port', () => {
    expect(parseRtcIdentity('@user:matrix.example.com:8448:DEVICEABC')).toEqual({
      userId: '@user:matrix.example.com:8448',
      deviceId: 'DEVICEABC',
    });
  });

  test('falls back to a memberships match for a sha256-hash identity', () => {
    const memberships = [
      { userId: '@other:s', deviceId: 'D1', rtcBackendIdentity: 'hashOTHER' },
      { userId: '@u:matrix.vkkovalev.work', deviceId: 'D2', rtcBackendIdentity: 'deadbeefhash' },
    ];
    expect(parseRtcIdentity('deadbeefhash', memberships)).toEqual({
      userId: '@u:matrix.vkkovalev.work',
      deviceId: 'D2',
    });
  });

  test('returns null for malformed / empty / non-matching input', () => {
    expect(parseRtcIdentity('')).toBeNull();
    expect(parseRtcIdentity(null)).toBeNull();
    expect(parseRtcIdentity(undefined)).toBeNull();
    // a bare userId with no device suffix cannot be split
    expect(parseRtcIdentity('@onlyuser:server')).toBeNull();
    // hash format with no membership to match against
    expect(parseRtcIdentity('unknownhash', [])).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// resolveDisplayName
// ---------------------------------------------------------------------------

describe('resolveDisplayName', () => {
  const makeClient = (name) => ({
    getRoom: () => ({ getMember: () => (name ? { name } : null) }),
  });

  test('resolves the matrix room-member display name', () => {
    expect(resolveDisplayName(makeClient('Test Two'), '!room', '@test2:s')).toBe('Test Two');
  });

  test('falls back to the userId when the member has no name', () => {
    expect(resolveDisplayName(makeClient(null), '!room', '@test2:s')).toBe('@test2:s');
  });

  test('falls back to the userId when the client/room is missing', () => {
    expect(resolveDisplayName(null, '!room', '@test2:s')).toBe('@test2:s');
    expect(resolveDisplayName({}, '!room', '@test2:s')).toBe('@test2:s');
  });

  test('returns Unknown for an empty userId', () => {
    expect(resolveDisplayName(makeClient('X'), '!room', '')).toBe('Unknown');
  });
});

// ---------------------------------------------------------------------------
// membershipToParticipant
// ---------------------------------------------------------------------------

describe('membershipToParticipant', () => {
  const matrixclient = {
    getRoom: () => ({
      getMember: (uid) => ({ name: uid === '@me:s' ? 'Me Local' : 'Remote Person' }),
    }),
  };

  test('maps a remote membership with resolved display name', () => {
    const p = membershipToParticipant(
      { userId: '@remote:s', deviceId: 'DR' },
      { matrixclient, roomId: '!room', localUserId: '@me:s' }
    );
    expect(p).toMatchObject({ id: '@remote:s', name: 'Remote Person', isLocal: false });
  });

  test('flags the local user via matrixclient.getUserId()', () => {
    const p = membershipToParticipant(
      { userId: '@me:s', deviceId: 'DM' },
      { matrixclient, roomId: '!room', localUserId: '@me:s', localDeviceId: 'DM' }
    );
    expect(p).toMatchObject({ id: '@me:s', name: 'Me Local', isLocal: true });
  });

  test('is not local when the deviceId differs from the local device', () => {
    const p = membershipToParticipant(
      { userId: '@me:s', deviceId: 'OTHER_DEVICE' },
      { matrixclient, roomId: '!room', localUserId: '@me:s', localDeviceId: 'DM' }
    );
    expect(p.isLocal).toBe(false);
  });

  test('marks isSpeaking from the injected speaking set', () => {
    const p = membershipToParticipant(
      { userId: '@remote:s', deviceId: 'DR' },
      { matrixclient, roomId: '!room', localUserId: '@me:s', speakingUserIds: new Set(['@remote:s']) }
    );
    expect(p.isSpeaking).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// URL / title builders
// ---------------------------------------------------------------------------

describe('buildMeetingUrl / buildMeetingTitle', () => {
  test('synthesizes a canonical #/room/<roomId> Element URL', () => {
    expect(buildMeetingUrl('!uNYpZ:matrix.vkkovalev.work')).toBe(
      'https://element.vkkovalev.work/#/room/!uNYpZ:matrix.vkkovalev.work'
    );
  });

  test('returns null for a missing roomId', () => {
    expect(buildMeetingUrl('')).toBeNull();
    expect(buildMeetingUrl(null)).toBeNull();
  });

  test('reads the room name from matrixclient for the title', () => {
    const mx = { getRoom: () => ({ name: 'Recog Spike' }) };
    expect(buildMeetingTitle(mx, '!room')).toBe('Recog Spike');
  });

  test('returns empty string when the title cannot be resolved', () => {
    expect(buildMeetingTitle(null, '!room')).toBe('');
    expect(buildMeetingTitle({ getRoom: () => null }, '!room')).toBe('');
    expect(buildMeetingTitle({ getRoom: () => ({ name: 'X' }) }, '')).toBe('');
  });
});

// ---------------------------------------------------------------------------
// Fiber walk
// ---------------------------------------------------------------------------

/** Build a fiber chain: nodes[0] is the leaf, linked upward via `.return`. */
function chain(...propsList) {
  const nodes = propsList.map((p) => ({ memoizedProps: p, return: null }));
  for (let i = 0; i < nodes.length - 1; i++) nodes[i].return = nodes[i + 1];
  return nodes[0];
}

describe('walkFiberForProps', () => {
  test('collects trackRef and livekitRoom found at different hops', () => {
    const trackRef = { participant: { identity: '@a:s:D', isSpeaking: true } };
    const livekitRoom = { remoteParticipants: new Map() };
    const leaf = chain(null, null, { trackRef }, null, { livekitRoom });
    expect(walkFiberForProps(leaf)).toEqual({ trackRef, livekitRoom });
  });

  test('respects the hop cap (props beyond the cap are not found)', () => {
    const livekitRoom = { r: 1 };
    // 60-node chain, livekitRoom only at index 55 → unreachable within 50 hops
    const props = new Array(60).fill(null);
    props[55] = { livekitRoom };
    const leaf = chain(...props);
    expect(walkFiberForProps(leaf, 50).livekitRoom).toBeNull();
  });

  test('returns nulls for a chain with no relevant props', () => {
    expect(walkFiberForProps(chain(null, null, null))).toEqual({ trackRef: null, livekitRoom: null });
    expect(walkFiberForProps(null)).toEqual({ trackRef: null, livekitRoom: null });
  });
});

describe('getFiberFromNode / findLivekitRoom / findTrackRefs', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  function makeAudioWithFiber(fiber) {
    const audio = document.createElement('audio');
    Object.defineProperty(audio, 'srcObject', { value: {}, configurable: true });
    audio['__reactFiber$abc123'] = fiber;
    document.body.appendChild(audio);
    return audio;
  }

  test('getFiberFromNode reads the __reactFiber$* property', () => {
    const fiber = { memoizedProps: {} };
    const audio = makeAudioWithFiber(fiber);
    expect(getFiberFromNode(audio)).toBe(fiber);
    expect(getFiberFromNode(null)).toBeNull();
    expect(getFiberFromNode(document.createElement('div'))).toBeNull();
  });

  test('findLivekitRoom walks the fiber of an <audio> with srcObject', () => {
    const livekitRoom = { remoteParticipants: new Map(), tag: 'ROOM' };
    const leaf = chain(null, { livekitRoom });
    makeAudioWithFiber(leaf);
    expect(findLivekitRoom(document)).toBe(livekitRoom);
  });

  test('findLivekitRoom ignores <audio> elements without srcObject', () => {
    const audio = document.createElement('audio');
    audio['__reactFiber$abc123'] = chain({ livekitRoom: { tag: 'ROOM' } });
    document.body.appendChild(audio); // no srcObject
    expect(findLivekitRoom(document)).toBeNull();
  });

  test('findTrackRefs collects trackRefs from all decorated audio elements', () => {
    const t1 = { participant: { identity: '@a:s:D' } };
    const t2 = { participant: { identity: '@b:s:E' } };
    makeAudioWithFiber(chain({ trackRef: t1 }));
    makeAudioWithFiber(chain(null, { trackRef: t2 }));
    expect(findTrackRefs(document)).toEqual([t1, t2]);
  });
});

// ---------------------------------------------------------------------------
// ElementCallAdapter contract
// ---------------------------------------------------------------------------

describe('ElementCallAdapter', () => {
  let adapter;

  function makeMatrixClient() {
    return {
      getUserId: () => '@me:matrix.vkkovalev.work',
      getDeviceId: () => 'MYDEVICE',
      getRoom: () => ({
        name: 'Recog Spike',
        getMember: (uid) => ({ name: uid === '@me:matrix.vkkovalev.work' ? 'Me' : 'Test Two' }),
      }),
    };
  }

  function makeRtcSession() {
    return {
      room: { roomId: '!spike:matrix.vkkovalev.work' },
      memberships: [
        { userId: '@me:matrix.vkkovalev.work', deviceId: 'MYDEVICE' },
        { userId: '@test2:matrix.vkkovalev.work', deviceId: 'IZFUKTQLPX' },
      ],
    };
  }

  /** Mount an <audio> whose fiber exposes `room` so `_ensureRoom` finds it. */
  function mountRoom(room) {
    const audio = document.createElement('audio');
    Object.defineProperty(audio, 'srcObject', { value: {}, configurable: true });
    audio['__reactFiber$xyz'] = chain(null, { livekitRoom: room });
    document.body.appendChild(audio);
    return room;
  }

  beforeEach(() => {
    document.body.innerHTML = '';
    window.matrixclient = makeMatrixClient();
    window.rtcSession = makeRtcSession();
    adapter = new ElementCallAdapter(window, document);
  });

  afterEach(() => {
    adapter.dispose();
    delete window.matrixclient;
    delete window.rtcSession;
  });

  test('isConferenceAvailable requires both globals', () => {
    expect(adapter.isConferenceAvailable()).toBe(true);
    delete window.rtcSession;
    expect(adapter.isConferenceAvailable()).toBe(false);
  });

  test('getConference returns the rtcSession, getMeetingUrl the synthesized url', () => {
    expect(adapter.getConference()).toBe(window.rtcSession);
    expect(adapter.getMeetingUrl()).toBe(
      'https://element.vkkovalev.work/#/room/!spike:matrix.vkkovalev.work'
    );
  });

  test('getJitsiConference is null before the livekit Room is found, then the cached Room', () => {
    // No <audio> with srcObject yet → Room not discoverable
    expect(adapter.getJitsiConference()).toBeNull();

    // Mount an <audio> whose fiber exposes a livekit Room
    const livekitRoom = {
      remoteParticipants: new Map(),
      localParticipant: null,
      activeSpeakers: [],
      on() {},
      off() {},
    };
    const leaf = chain(null, { livekitRoom });
    const audio = document.createElement('audio');
    Object.defineProperty(audio, 'srcObject', { value: {}, configurable: true });
    audio['__reactFiber$xyz'] = leaf;
    document.body.appendChild(audio);

    const found = adapter.getJitsiConference();
    expect(found).toBe(livekitRoom);
    // identity is stable (cached) on subsequent calls → flips null→Room exactly once
    expect(adapter.getJitsiConference()).toBe(found);
  });

  test('getParticipants maps memberships with names and isLocal', () => {
    const parts = adapter.getParticipants();
    expect(parts).toHaveLength(2);
    const me = parts.find((p) => p.id === '@me:matrix.vkkovalev.work');
    const other = parts.find((p) => p.id === '@test2:matrix.vkkovalev.work');
    expect(me).toMatchObject({ name: 'Me', isLocal: true });
    expect(other).toMatchObject({ name: 'Test Two', isLocal: false });
  });

  test('getParticipantName resolves via matrixclient', () => {
    expect(adapter.getParticipantName('@test2:matrix.vkkovalev.work')).toBe('Test Two');
  });

  test('getActiveSpeaker reads the livekit active speaker and resolves its name', () => {
    const speaking = { identity: '@test2:matrix.vkkovalev.work:IZFUKTQLPX', isSpeaking: true };
    const livekitRoom = {
      remoteParticipants: new Map(),
      localParticipant: null,
      activeSpeakers: [speaking],
      on() {},
      off() {},
    };
    const leaf = chain(null, { livekitRoom });
    const audio = document.createElement('audio');
    Object.defineProperty(audio, 'srcObject', { value: {}, configurable: true });
    audio['__reactFiber$xyz'] = leaf;
    document.body.appendChild(audio);

    expect(adapter.getActiveSpeaker()).toEqual({
      id: '@test2:matrix.vkkovalev.work',
      name: 'Test Two',
      isLocal: false,
    });
  });

  test('getActiveSpeaker returns null before the livekit Room is found', () => {
    expect(adapter.getActiveSpeaker()).toBeNull();
  });

  test('supportsTabInjection is false (floating panel platform)', () => {
    expect(adapter.supportsTabInjection()).toBe(false);
  });

  test('getMeetingTitle returns the Matrix room name', () => {
    expect(adapter.getMeetingTitle()).toBe('Recog Spike');
  });

  test('getMeetingTitle returns empty string when the room name is unresolved', () => {
    window.matrixclient.getRoom = () => null;
    expect(adapter.getMeetingTitle()).toBe('');
  });

  test('getParticipants reflects livekit speaking/muted state keyed by userId', () => {
    mountRoom({
      localParticipant: null,
      remoteParticipants: new Map([
        ['a', { identity: '@test2:matrix.vkkovalev.work:IZFUKTQLPX', isSpeaking: true, isMicrophoneEnabled: true }],
        ['b', { identity: '@me:matrix.vkkovalev.work:MYDEVICE', isSpeaking: false, isMicrophoneEnabled: false }],
      ]),
      activeSpeakers: [],
      on() {},
      off() {},
    });

    const parts = adapter.getParticipants();
    const other = parts.find((p) => p.id === '@test2:matrix.vkkovalev.work');
    const me = parts.find((p) => p.id === '@me:matrix.vkkovalev.work');
    expect(other).toMatchObject({ isSpeaking: true, micMuted: false });
    expect(me).toMatchObject({ isSpeaking: false, micMuted: true });
    expect(adapter.isMicMuted('@me:matrix.vkkovalev.work')).toBe(true);
    expect(adapter.anyMicOn()).toBe(true);
  });

  test('reattach after dispose wires exactly one participantConnected join handler', () => {
    // Count live on()/off() registrations per event so a join-handler double
    // (dispose failing to clear _joinCallback) is observable.
    const registered = {};
    mountRoom({
      localParticipant: null,
      remoteParticipants: new Map(),
      activeSpeakers: [],
      on(event, handler) {
        (registered[event] = registered[event] || new Set()).add(handler);
      },
      off(event, handler) {
        registered[event]?.delete(handler);
      },
    });

    // Cycle 1: fresh subscribe + join wiring on the (now cached) Room.
    adapter.subscribeToSpeakerChanges(() => {});
    adapter.onParticipantJoined(() => {});
    // participantConnected carries one speaker handler + one join handler.
    expect(registered.participantConnected.size).toBe(2);

    // Reattach: init.js disposes then re-subscribes against the same Room.
    adapter.dispose();
    adapter.subscribeToSpeakerChanges(() => {});
    adapter.onParticipantJoined(() => {});
    expect(registered.participantConnected.size).toBe(2);
  });
});
