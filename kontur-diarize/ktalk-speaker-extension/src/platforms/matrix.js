/**
 * Element Call (Matrix) Platform Adapter
 *
 * Element Call is bundled inside Element Web as a same-origin iframe served from
 * /widgets/element-call/*. This adapter runs ONLY inside that iframe (manifest
 * scope) and sources data from Element Call internals:
 *
 * - `window.matrixclient` (matrix-js-sdk client) — display names, local userId.
 * - `window.rtcSession` (MatrixRTCSession; mounts on call join, removed on
 *   leave) — room id + `memberships[]` (plain `userId` + `deviceId`).
 * - a React fiber-walk from any decoded `<audio>` element up to the livekit
 *   `Room` (`memoizedProps.livekitRoom`) for active-speaker events. The Room
 *   mounts late (first unmute), so it is discovered lazily and cached.
 *
 * Identity note: on this deploy `participant.identity` is the LEGACY plain
 * format `${userId}:${deviceId}` (e.g. `@test2:matrix.vkkovalev.work:IZFUKTQLPX`),
 * not the sha256 hash. `parseRtcIdentity` parses it directly and keeps a
 * memberships (`rtcBackendIdentity`) fallback for the future sticky-event
 * MatrixRTC hash format.
 *
 * Assumption: a single LiveKit focus per call (personal single-server deploy) —
 * the first `livekitRoom` found in the fiber tree is used.
 */

export const ELEMENT_BASE_URL = 'https://element.vkkovalev.work';

const FIBER_KEY_PREFIX = '__reactFiber$';
const MAX_FIBER_HOPS = 50;
const ROOM_RETRY_INTERVAL_MS = 2000;

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested in matrix.test.js)
// ---------------------------------------------------------------------------

/**
 * Parse a livekit participant identity into `{ userId, deviceId }`.
 *
 * Legacy plain format: `${userId}:${deviceId}` where `userId` is itself
 * `@localpart:homeserver` (so a valid legacy identity has ≥2 colons; the
 * deviceId is the segment after the LAST colon). Falls back to matching the
 * identity against `membership.rtcBackendIdentity` (future sha256-hash form).
 * Returns null for malformed / non-matching input.
 */
export function parseRtcIdentity(identity, memberships = []) {
  if (typeof identity !== 'string' || !identity) return null;

  if (identity.startsWith('@')) {
    const firstColon = identity.indexOf(':');
    const lastColon = identity.lastIndexOf(':');
    // firstColon separates localpart:homeserver; lastColon separates :deviceId.
    if (firstColon > 0 && lastColon > firstColon) {
      return {
        userId: identity.slice(0, lastColon),
        deviceId: identity.slice(lastColon + 1),
      };
    }
  }

  for (const m of memberships || []) {
    if (m && m.rtcBackendIdentity === identity) {
      return { userId: m.userId, deviceId: m.deviceId };
    }
  }

  return null;
}

/** Resolve a matrix userId to its room display name (fallback: the userId). */
export function resolveDisplayName(matrixclient, roomId, userId) {
  if (!userId) return 'Unknown';
  try {
    const member = matrixclient?.getRoom?.(roomId)?.getMember?.(userId);
    return member?.name || userId;
  } catch (e) {
    return userId;
  }
}

/**
 * Map an rtcSession membership to the participant shape the UI/speaker code
 * expects (`{ id, name, isLocal, isSpeaking, micMuted }`).
 */
export function membershipToParticipant(membership, ctx = {}) {
  const {
    matrixclient,
    roomId,
    localUserId,
    localDeviceId = null,
    speakingUserIds = new Set(),
    mutedUserIds = new Set(),
  } = ctx;
  const userId = membership?.userId;
  const deviceId = membership?.deviceId ?? null;
  const name = resolveDisplayName(matrixclient, roomId, userId);
  const isLocal =
    !!userId &&
    userId === localUserId &&
    (localDeviceId == null || deviceId == null || deviceId === localDeviceId);
  return {
    id: userId,
    name,
    isLocal,
    isSpeaking: speakingUserIds.has(userId),
    micMuted: mutedUserIds.has(userId),
  };
}

/** Synthesize the canonical Element deep-link `#/room/<roomId>`. */
export function buildMeetingUrl(roomId) {
  if (!roomId) return null;
  return `${ELEMENT_BASE_URL}/#/room/${roomId}`;
}

/** Resolve the room name from matrixclient (fallback: empty string). */
export function buildMeetingTitle(matrixclient, roomId) {
  if (!roomId) return '';
  try {
    return matrixclient?.getRoom?.(roomId)?.name || '';
  } catch (e) {
    return '';
  }
}

// ---------------------------------------------------------------------------
// Fiber walk
// ---------------------------------------------------------------------------

/** Read the React fiber attached to a DOM node (`__reactFiber$*` property). */
export function getFiberFromNode(node) {
  if (!node) return null;
  const key = Object.keys(node).find((k) => k.startsWith(FIBER_KEY_PREFIX));
  return key ? node[key] : null;
}

/**
 * Walk a fiber chain upward via `.return`, collecting `memoizedProps.trackRef`
 * and `memoizedProps.livekitRoom`. Stops once both are found or the hop cap is
 * reached.
 */
export function walkFiberForProps(fiber, maxHops = MAX_FIBER_HOPS) {
  let current = fiber;
  let hops = 0;
  let trackRef = null;
  let livekitRoom = null;
  while (current && hops < maxHops) {
    const props = current.memoizedProps;
    if (props) {
      if (!trackRef && props.trackRef) trackRef = props.trackRef;
      if (!livekitRoom && props.livekitRoom) livekitRoom = props.livekitRoom;
    }
    if (trackRef && livekitRoom) break;
    current = current.return;
    hops++;
  }
  return { trackRef, livekitRoom };
}

function decodedAudioElements(doc) {
  return [...doc.querySelectorAll('audio')].filter((a) => a.srcObject);
}

/** Find the livekit `Room` by walking the fiber of any decoded `<audio>`. */
export function findLivekitRoom(doc = document) {
  for (const audio of decodedAudioElements(doc)) {
    const fiber = getFiberFromNode(audio);
    if (!fiber) continue;
    const { livekitRoom } = walkFiberForProps(fiber);
    if (livekitRoom) return livekitRoom;
  }
  return null;
}

/** Collect every `trackRef` reachable from decoded `<audio>` fibers. */
export function findTrackRefs(doc = document) {
  const refs = [];
  for (const audio of decodedAudioElements(doc)) {
    const fiber = getFiberFromNode(audio);
    if (!fiber) continue;
    const { trackRef } = walkFiberForProps(fiber);
    if (trackRef) refs.push(trackRef);
  }
  return refs;
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

const ROOM_SPEAKER_EVENTS = [
  'activeSpeakersChanged',
  'participantConnected',
  'participantDisconnected',
  'trackMuted',
  'trackUnmuted',
];

export class ElementCallAdapter {
  constructor(win = window, doc = document) {
    this.name = 'matrix';
    this._win = win;
    this._doc = doc;
    this._room = null; // cached livekit Room (found via fiber-walk)
    this._roomRetryTimer = null;
    this._roomEventsAttached = false;
    this._speakerHandlers = [];
    this._speakerCallback = null;
    this._joinCallback = null;
  }

  _rtcSession() {
    return this._win.rtcSession || null;
  }

  _matrixClient() {
    return this._win.matrixclient || null;
  }

  _roomId() {
    return this._rtcSession()?.room?.roomId || null;
  }

  _memberships() {
    return this._rtcSession()?.memberships || [];
  }

  // -- Conference lifecycle --------------------------------------------------

  isInPrejoin() {
    // Element Call renders its own lobby; nothing to surface to the panel yet.
    return false;
  }

  isConferenceAvailable() {
    return !!(this._rtcSession() && this._matrixClient());
  }

  waitForConference(maxAttempts = 200) {
    return new Promise((resolve, reject) => {
      let attempts = 0;
      const check = () => {
        attempts++;
        if (this.isConferenceAvailable()) {
          console.log('[Matrix Adapter] Conference found (rtcSession + matrixclient)');
          resolve(this.getConference());
          return;
        }
        if (attempts < maxAttempts) {
          setTimeout(check, 500);
        } else {
          reject(new Error('Conference not found'));
        }
      };
      check();
    });
  }

  /** rtcSession drives participants + lifecycle. */
  getConference() {
    return this._rtcSession();
  }

  /**
   * Returns the livekit `Room` — `null` until the fiber-walk finds it, then the
   * cached `Room`. init.js reattaches active-speaker subscriptions only when
   * this object identity changes; the Room mounts late (first unmute) so the
   * single `null → Room` flip is what drives that reattach.
   */
  getJitsiConference() {
    return this._ensureRoom();
  }

  _ensureRoom() {
    if (this._room) return this._room;
    const room = findLivekitRoom(this._doc);
    if (room) {
      this._room = room;
      this._stopRoomRetry();
      // Attach speaker events now that the Room exists (if a subscriber waits).
      if (this._speakerCallback) this._attachRoomSpeakerEvents();
    } else {
      this._startRoomRetry();
    }
    return this._room;
  }

  _startRoomRetry() {
    if (this._roomRetryTimer) return;
    this._roomRetryTimer = setInterval(() => {
      this._ensureRoom();
    }, ROOM_RETRY_INTERVAL_MS);
  }

  _stopRoomRetry() {
    if (this._roomRetryTimer) {
      clearInterval(this._roomRetryTimer);
      this._roomRetryTimer = null;
    }
  }

  isConferenceReady() {
    // Ready once the livekit Room is found (tracks decoded) or memberships
    // exist — the mixed-audio path works regardless; active-speaker degrades
    // gracefully until the Room is found.
    if (this._ensureRoom()) return true;
    return this._memberships().length > 0;
  }

  // -- Participants ----------------------------------------------------------

  _livekitParticipants() {
    const room = this._room;
    const out = [];
    if (!room) return out;
    if (room.localParticipant) out.push(room.localParticipant);
    room.remoteParticipants?.forEach?.((p) => out.push(p));
    return out;
  }

  _speakingUserIds() {
    const set = new Set();
    const memberships = this._memberships();
    for (const p of this._livekitParticipants()) {
      if (p?.isSpeaking) {
        const parsed = parseRtcIdentity(p.identity, memberships);
        if (parsed) set.add(parsed.userId);
      }
    }
    return set;
  }

  _mutedUserIds() {
    const set = new Set();
    const memberships = this._memberships();
    for (const p of this._livekitParticipants()) {
      if (p?.isMicrophoneEnabled === false) {
        const parsed = parseRtcIdentity(p.identity, memberships);
        if (parsed) set.add(parsed.userId);
      }
    }
    return set;
  }

  getParticipants() {
    const matrixclient = this._matrixClient();
    const roomId = this._roomId();
    this._ensureRoom(); // populate livekit state for speaking/muted flags
    const ctx = {
      matrixclient,
      roomId,
      localUserId: matrixclient?.getUserId?.() || null,
      localDeviceId: matrixclient?.getDeviceId?.() || null,
      speakingUserIds: this._speakingUserIds(),
      mutedUserIds: this._mutedUserIds(),
    };
    return this._memberships().map((m) => membershipToParticipant(m, ctx));
  }

  getParticipantName(participantId) {
    if (!participantId) return 'Unknown';
    return resolveDisplayName(this._matrixClient(), this._roomId(), participantId);
  }

  // -- Active speaker --------------------------------------------------------

  getActiveSpeaker() {
    const room = this._ensureRoom();
    if (!room) return null;

    let speaking = null;
    const active = room.activeSpeakers || [];
    speaking = active.find((p) => p && p.isSpeaking) || active[0] || null;
    if (!speaking) {
      for (const p of this._livekitParticipants()) {
        if (p?.isSpeaking) {
          speaking = p;
          break;
        }
      }
    }
    if (!speaking) return null;

    const parsed = parseRtcIdentity(speaking.identity, this._memberships());
    if (!parsed) return null;

    const matrixclient = this._matrixClient();
    return {
      id: parsed.userId,
      name: resolveDisplayName(matrixclient, this._roomId(), parsed.userId),
      isLocal: parsed.userId === matrixclient?.getUserId?.(),
    };
  }

  _attachRoomSpeakerEvents() {
    const room = this._room;
    if (!room || typeof room.on !== 'function' || this._roomEventsAttached) return;
    const handler = () => this._speakerCallback?.();
    for (const event of ROOM_SPEAKER_EVENTS) {
      room.on(event, handler);
      this._speakerHandlers.push({ event, handler });
    }
    if (this._joinCallback) {
      const joinHandler = (p) => setTimeout(() => this._joinCallback(p?.identity), 500);
      room.on('participantConnected', joinHandler);
      this._speakerHandlers.push({ event: 'participantConnected', handler: joinHandler });
    }
    this._roomEventsAttached = true;
  }

  subscribeToSpeakerChanges(callback) {
    this._speakerCallback = callback;
    // Attach now if the Room already exists; otherwise _ensureRoom's retry
    // attaches once the Room mounts (first unmute).
    this._ensureRoom();
    this._attachRoomSpeakerEvents();
  }

  onParticipantJoined(callback) {
    this._joinCallback = callback;
    // If events are already attached, wire the join listener immediately.
    if (this._room && this._roomEventsAttached && typeof this._room.on === 'function') {
      const joinHandler = (p) => setTimeout(() => callback(p?.identity), 500);
      this._room.on('participantConnected', joinHandler);
      this._speakerHandlers.push({ event: 'participantConnected', handler: joinHandler });
    }
  }

  // -- Mic state -------------------------------------------------------------

  isMicMuted(participantId) {
    const memberships = this._memberships();
    const p = this._livekitParticipants().find((lp) => {
      const parsed = parseRtcIdentity(lp.identity, memberships);
      return parsed && parsed.userId === participantId;
    });
    if (!p) return false; // unknown → assume on; mixed audio is captured regardless
    if (typeof p.isMicrophoneEnabled === 'boolean') return !p.isMicrophoneEnabled;
    return false;
  }

  anyMicOn() {
    return this._livekitParticipants().some((p) => p.isMicrophoneEnabled === true);
  }

  // -- Meeting url -----------------------------------------------------------

  /**
   * Canonical synthesized meeting url. Inside the EC iframe `location.href` is
   * `…/widgets/element-call/index.html#…` (path-derived live_id would be
   * "index.html"), so both meeting_join and the change-detection tick must use
   * this synthesized `#/room/<roomId>` url instead.
   */
  getMeetingUrl() {
    return buildMeetingUrl(this._roomId());
  }

  /**
   * Authoritative meeting title = the Matrix room name. Inside the EC iframe
   * `document.title` is Element Call's own page title, not the room, so
   * meeting.js prefers this hook over `document.title` (mirrors getMeetingUrl).
   * Returns '' when the room name is not yet resolved → caller falls back.
   */
  getMeetingTitle() {
    return buildMeetingTitle(this._matrixClient(), this._roomId());
  }

  // -- UI hooks --------------------------------------------------------------

  getTabContainerSelector() {
    return null; // floating-panel platform, no native tabs
  }

  getContentAreaSelector() {
    return null;
  }

  supportsTabInjection() {
    return false; // use the floating panel
  }

  // -- Cleanup ---------------------------------------------------------------

  dispose() {
    const room = this._room;
    if (room && typeof room.off === 'function') {
      this._speakerHandlers.forEach(({ event, handler }) => room.off(event, handler));
    }
    this._speakerHandlers = [];
    this._roomEventsAttached = false;
    this._stopRoomRetry();
    // Clear the join callback so a reattach re-wires it from exactly one site.
    // If it survived, `_attachRoomSpeakerEvents` (which runs first, via
    // subscribeToSpeakerChanges) would attach a join listener because
    // `_joinCallback` is still truthy, and `onParticipantJoined` would then
    // attach a second one — double-firing `sendParticipantsUpdate` per join.
    this._joinCallback = null;
    // Note: the cached Room is intentionally kept across reattach — its object
    // identity must stay stable so init.js does not thrash subscriptions.
  }
}
