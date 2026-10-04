/**
 * Zoom Web Client Platform Adapter
 *
 * The web client exposes no conference object to the page, so everything comes
 * from the DOM of the top frame (observed on client 7.2, 2026-10-04):
 *
 * - participant tiles: the speaker-view strip and main tile, and gallery tiles.
 *   No attribute carries a participant id, so the display name
 *   (`.video-avatar__avatar-footer span`) is the id; namesakes merge into one
 *   participant;
 * - the active speaker: an explicit `--active` class on a strip or gallery
 *   tile. Zoom keeps it on the last speaker during silence and never puts it
 *   on the local participant. The structural main tile is the last resort,
 *   because under pin / spotlight it shows the pinned participant;
 * - the meeting itself: the path keeps the form `/wc/<id>/join` from prejoin
 *   through the waiting room into the meeting. `#wc-footer` exists inside the
 *   meeting, but Zoom also mounts it for a second or two, with no tiles, right
 *   after "Join" and before the waiting room. Entering therefore needs the
 *   footer together with a participant tile; once entered, the footer alone
 *   keeps the meeting (tiles can vanish, e.g. under a screen share).
 *
 * The meeting URL is synthesized as `https://zoom.us/j/<id>`: the backend takes
 * the last path segment as the room id, the client runs on several hosts, and
 * the native-application source reports the same canonical form.
 *
 * Class names change with Zoom releases: every selector lives in
 * `ZOOM_SELECTORS`, and selector drift is reported once via `console.warn`
 * plus `getDiagnostic()`.
 *
 * Audio is the mixed system capture only (Tier 1).
 */

export const ZOOM_SELECTORS = {
  tile: [
    '.speaker-bar-container__video-frame',
    '.speaker-active-container__video-frame',
    '.gallery-video-container__video-frame',
  ].join(', '),
  // Most trustworthy first; the first match that has a name wins.
  activeTile: [
    '.speaker-bar-container__video-frame--active',
    '.gallery-video-container__video-frame--active',
    '.speaker-active-container__video-frame',
  ],
  tileName: '.video-avatar__avatar-footer span',
  // The meeting toolbar; kept in the DOM while auto-hidden. Also mounted
  // briefly before the waiting room, hence the tile requirement on entry.
  inCallMarker: '#wc-footer',
  // Muted-microphone icon inside a tile footer. Absent → not muted.
  micMutedMarker: '.video-avatar__avatar-footer--view-mute-computer',
  // Prejoin name field: the only place the local display name is readable.
  selfNameInput: '#input-for-name',
};

const WC_PATH_RE = /^\/wc\/(?:join\/)?(\d{9,11})(?:\/|$)/;

export const SPEAKER_GRACE_MS = 1000;
const REEVALUATE_MARGIN_MS = 50;
export const IN_MEETING_DEBOUNCE_MS = 6000;
export const CALLBACK_THROTTLE_MS = 100;
export const NO_TILES_DRIFT_MS = 30000;
export const NO_ACTIVE_DRIFT_MS = 120000;

const DRIFT_MESSAGES = {
  no_tiles: 'Zoom: плитки участников не найдены — определение говорящего не работает',
  active_without_name: 'Zoom: у активной плитки нет имени — определение говорящего не работает',
  no_active_tile: 'Zoom: говорящий не отмечен дольше 2 минут — определение говорящего может не работать',
};

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested in zoom.test.js)
// ---------------------------------------------------------------------------

/** Numeric meeting id from a web-client path (`/wc/<id>/join`, `/wc/join/<id>`), else null. */
export function meetingIdFromPath(pathname) {
  return WC_PATH_RE.exec(pathname || '')?.[1] || null;
}

/** Canonical meeting URL shared by every Zoom source, or null without an id. */
export function buildMeetingUrl(meetingId) {
  return meetingId ? `https://zoom.us/j/${meetingId}` : null;
}

export function tileName(tile) {
  return (tile?.querySelector(ZOOM_SELECTORS.tileName)?.textContent || '').trim();
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

export class ZoomAdapter {
  constructor(win = window, doc = document) {
    this.name = 'zoom';
    this._win = win;
    this._doc = doc;
    // Stable conference object: init.js reattaches subscriptions whenever the
    // identity of getConference()/getJitsiConference() changes.
    this._conference = { platform: 'zoom' };

    // Kept across dispose() so the post-join reattach is lossless.
    this._selfName = null;
    this._currentSpeaker = null; // name of the last reported speaker
    this._speakerLostAt = null; // ts the active tile vanished, null while present
    this._snapshot = { present: new Map(), inCall: false, tileCount: 0, activeName: null };

    // isInMeeting(): the meeting entered (footer + a tile seen) and its debounce.
    this._enteredId = null;
    this._inCallLastSeenAt = null;
    this._inCallLastSeenId = null;

    // Drift detection.
    this._noTilesSince = null;
    this._noActiveSince = null;
    this._drift = null;
    this._driftWarned = new Set();

    // Live subscriptions (recreated by subscribeToSpeakerChanges).
    this._speakerCallback = null;
    this._joinCallback = null;
    this._announced = new Set();
    this._observer = null;
    this._throttleTimer = null;
    this._reevaluateTimer = null;
    this._lastChangeSignature = null;
  }

  _meetingId() {
    return meetingIdFromPath(this._win.location.pathname);
  }

  // -- DOM scan --------------------------------------------------------------

  /**
   * Scan the document: participants by name, the active tile, the in-call
   * marker, and update the speaker grace, the isInMeeting() debounce and drift
   * detection.
   */
  _refresh(now = Date.now()) {
    const sel = ZOOM_SELECTORS;
    const doc = this._doc;

    const typedName = doc.querySelector(sel.selfNameInput)?.value?.trim();
    if (typedName) this._selfName = typedName;

    const present = new Map(); // name → { micMuted, isLocal }
    let tileCount = 0;
    for (const tile of doc.querySelectorAll(sel.tile)) {
      tileCount++;
      const name = tileName(tile);
      if (!name) continue;
      const muted = !!tile.querySelector(sel.micMutedMarker);
      const entry = present.get(name);
      // One person shows up on several tiles (strip + main tile), and namesakes
      // share an entry: muted only when every tile of that name says so.
      if (entry) entry.micMuted = entry.micMuted && muted;
      else present.set(name, { micMuted: muted, isLocal: name === this._selfName });
    }

    const footer = !!doc.querySelector(sel.inCallMarker);
    const meetingId = this._meetingId();
    if (footer && tileCount > 0 && meetingId) this._enteredId = meetingId;
    const inCall = footer && meetingId !== null && this._enteredId === meetingId;
    if (inCall) {
      this._inCallLastSeenAt = now;
      this._inCallLastSeenId = meetingId;
    }

    let activeName = null;
    let activeWithoutName = false;
    for (const selector of sel.activeTile) {
      const tile = doc.querySelector(selector);
      if (!tile) continue;
      const name = tileName(tile);
      if (name) {
        activeName = name;
        break;
      }
      activeWithoutName = true;
    }

    if (activeName) {
      this._currentSpeaker = activeName;
      this._speakerLostAt = null;
    } else if (this._currentSpeaker && this._speakerLostAt === null) {
      this._speakerLostAt = now;
    }

    this._snapshot = { present, inCall, tileCount, activeName };
    this._updateDrift(now, { present, inCall, tileCount, activeName, activeWithoutName });
    return this._snapshot;
  }

  _updateDrift(now, { present, inCall, tileCount, activeName, activeWithoutName }) {
    if (!inCall) {
      // Prejoin, waiting room and post-leave pages never report drift.
      this._noTilesSince = null;
      this._noActiveSince = null;
      this._drift = null;
      return;
    }

    if (tileCount === 0) {
      if (this._noTilesSince === null) this._noTilesSince = now;
    } else {
      this._noTilesSince = null;
    }

    if (present.size >= 2 && !activeName) {
      if (this._noActiveSince === null) this._noActiveSince = now;
    } else {
      this._noActiveSince = null;
    }

    let drift = null;
    if (activeWithoutName && !activeName) drift = 'active_without_name';
    else if (this._noTilesSince !== null && now - this._noTilesSince > NO_TILES_DRIFT_MS) {
      drift = 'no_tiles';
    } else if (this._noActiveSince !== null && now - this._noActiveSince > NO_ACTIVE_DRIFT_MS) {
      drift = 'no_active_tile';
    }

    // Time-based drift begins and ends with no DOM mutation; notify subscribers
    // so the status line follows it.
    if (drift !== this._drift && this._speakerCallback) this._scheduleChange();
    this._drift = drift;
    if (drift && !this._driftWarned.has(drift)) {
      this._driftWarned.add(drift);
      console.warn(`[recog][zoom] selector drift (${drift}): ${DRIFT_MESSAGES[drift]}`);
    }
  }

  /** Short user-facing text while selector drift is detected, else null. */
  getDiagnostic() {
    this._refresh();
    return this._drift ? DRIFT_MESSAGES[this._drift] : null;
  }

  // -- Conference lifecycle --------------------------------------------------

  /** Optional hook: false off a meeting path (`/wc/home`, the launcher). */
  isRoomPage() {
    return this._meetingId() !== null;
  }

  /**
   * Meeting path AND the meeting entered and its toolbar present now, or seen
   * for this same meeting within the last IN_MEETING_DEBOUNCE_MS (absorbs a
   * layout rebuild). Never true without a meeting id, so getMeetingUrl() is
   * non-null whenever this is true.
   */
  isInMeeting() {
    const meetingId = this._meetingId();
    if (!meetingId) return false;
    const now = Date.now();
    if (this._refresh(now).inCall) return true;
    const recent =
      this._inCallLastSeenAt !== null &&
      this._inCallLastSeenId === meetingId &&
      now - this._inCallLastSeenAt < IN_MEETING_DEBOUNCE_MS;
    // Left: the next toolbar without tiles is the pre-waiting-room flash again.
    if (!recent) this._enteredId = null;
    return recent;
  }

  isConferenceAvailable() {
    return this.isInMeeting();
  }

  /** Prejoin and the waiting room alike: a meeting path, not in the meeting. */
  isInPrejoin() {
    return this.isRoomPage() && !this.isInMeeting();
  }

  waitForConference(maxAttempts = 200) {
    return new Promise((resolve, reject) => {
      let attempts = 0;
      const check = () => {
        attempts++;
        if (this.isConferenceAvailable()) {
          console.log('[Zoom Adapter] Conference found (in-call marker)');
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

  getConference() {
    return this._conference;
  }

  getJitsiConference() {
    return this._conference;
  }

  isConferenceReady() {
    return this._refresh().tileCount > 0;
  }

  // -- Participants ----------------------------------------------------------

  getParticipants() {
    const { present, activeName } = this._refresh();
    return [...present].map(([name, entry]) => ({
      id: name,
      name,
      isLocal: entry.isLocal,
      isSpeaking: name === activeName,
      micMuted: entry.micMuted,
    }));
  }

  getParticipantName(participantId) {
    return participantId || 'Unknown';
  }

  // -- Active speaker --------------------------------------------------------

  _speakerInfo(name) {
    return { id: name, name, isLocal: name === this._selfName };
  }

  getActiveSpeaker() {
    const now = Date.now();
    const { activeName } = this._refresh(now);
    if (activeName) return this._speakerInfo(activeName);

    // The active tile vanished (layout rebuild): keep the speaker briefly.
    if (this._currentSpeaker && this._speakerLostAt !== null) {
      const remaining = this._speakerLostAt + SPEAKER_GRACE_MS - now;
      if (remaining > 0) {
        this._scheduleReevaluate(remaining + REEVALUATE_MARGIN_MS);
        return this._speakerInfo(this._currentSpeaker);
      }
      this._currentSpeaker = null;
    }
    return null;
  }

  _scheduleReevaluate(delayMs) {
    if (this._reevaluateTimer || !this._speakerCallback) return;
    this._reevaluateTimer = setTimeout(() => {
      this._reevaluateTimer = null;
      this._speakerCallback?.();
    }, delayMs);
  }

  // -- Subscriptions ---------------------------------------------------------

  _isRelevantMutation(record) {
    const sel = ZOOM_SELECTORS;
    const watched = `${sel.tile}, ${sel.inCallMarker}`;
    // A class flip on a tile, a renamed footer, a mute icon: all inside a tile.
    const target = record.target.nodeType === 1 ? record.target : record.target.parentElement;
    if (target?.closest?.(sel.tile)) return true;
    if (record.type !== 'childList') return false;
    const touchesWatched = (node) =>
      node.nodeType === 1 && (node.matches(watched) || !!node.querySelector(watched));
    return [...record.addedNodes, ...record.removedNodes].some(touchesWatched);
  }

  _onMutations(records) {
    if (records.some((r) => this._isRelevantMutation(r))) this._scheduleChange();
  }

  /** Trailing throttle: one callback per CALLBACK_THROTTLE_MS window. */
  _scheduleChange() {
    if (this._throttleTimer) return;
    this._throttleTimer = setTimeout(() => {
      this._throttleTimer = null;
      this._handleChange();
    }, CALLBACK_THROTTLE_MS);
  }

  /** Everything the speaker callback reports; unrelated tile mutations leave it unchanged. */
  _changeSignature({ present, activeName }) {
    const rows = [...present].map(([name, e]) => `${name}\u0001${+e.isLocal}${+e.micMuted}`);
    return `${this._drift || ''}\u0002${activeName || ''}\u0002${rows.join('\u0002')}`;
  }

  _handleChange() {
    const snapshot = this._refresh();
    const joined = [...snapshot.present.keys()].filter((name) => !this._announced.has(name));
    joined.forEach((name) => this._announced.add(name));
    const signature = this._changeSignature(snapshot);
    if (signature === this._lastChangeSignature) return;
    this._lastChangeSignature = signature;
    this._speakerCallback?.();
    if (this._joinCallback) joined.forEach((name) => this._joinCallback?.(name));
  }

  subscribeToSpeakerChanges(callback) {
    this._speakerCallback = callback;
    // init.js reports the current state itself right before subscribing, so the
    // baseline is "now": only a real change calls back.
    const snapshot = this._refresh();
    this._lastChangeSignature = this._changeSignature(snapshot);
    this._announced = new Set(snapshot.present.keys());
    if (this._observer) return;
    const Observer = this._doc.defaultView?.MutationObserver || this._win.MutationObserver;
    if (!Observer) return;
    this._observer = new Observer((records) => this._onMutations(records));
    this._observer.observe(this._doc.documentElement || this._doc, {
      attributes: true,
      attributeFilter: ['class'],
      characterData: true,
      childList: true,
      subtree: true,
    });
  }

  onParticipantJoined(callback) {
    this._joinCallback = callback;
  }

  // -- Mic state -------------------------------------------------------------

  isMicMuted(participantId) {
    return this._refresh().present.get(participantId)?.micMuted === true;
  }

  anyMicOn() {
    const { present } = this._refresh();
    if (present.size === 0) return true; // unknown → assume on
    return [...present.values()].some((entry) => !entry.micMuted);
  }

  // -- Meeting url / title ---------------------------------------------------

  getMeetingUrl() {
    return buildMeetingUrl(this._meetingId());
  }

  /**
   * Inside the meeting `document.title` is the meeting topic; before that it
   * is a generic, localized page title, so nothing is reported.
   */
  getMeetingTitle() {
    if (!this._doc.querySelector(ZOOM_SELECTORS.inCallMarker)) return '';
    return (this._doc.title || '').trim();
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
    this._observer?.disconnect();
    this._observer = null;
    clearTimeout(this._throttleTimer);
    this._throttleTimer = null;
    clearTimeout(this._reevaluateTimer);
    this._reevaluateTimer = null;
    this._speakerCallback = null;
    // Clear the join callback so a reattach re-wires it from exactly one site.
    // The speaker, the self name and the stable conference object are kept.
    this._joinCallback = null;
  }
}
