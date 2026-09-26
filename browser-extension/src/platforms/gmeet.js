/**
 * Google Meet Platform Adapter
 *
 * Meet exposes no usable conference object to the page, so everything comes
 * from the DOM:
 *
 * - participant tiles `[data-participant-id="spaces/<space>/devices/<n>"]`
 *   (stable per-device id; display name = first non-empty text line);
 * - the speaking indicator `[jsname="QgSmzd"]` inside a tile: `silentClass`
 *   while quiet, one of `speakingClasses` (volume levels) while speaking.
 *   Speaking is positive evidence only — an indicator carrying none of the
 *   known classes is "unknown" and counts as silent (and flags selector drift);
 * - Document Picture-in-Picture: with the Meet tab hidden the tiles move into
 *   `documentPictureInPicture.window.document`, so both documents are scanned
 *   and observed, and the handover (a moment with no tiles anywhere) is
 *   absorbed by the time-based `isInMeeting()` debounce.
 *
 * Class names are obfuscated and change with Meet releases: every selector
 * lives in `GMEET_SELECTORS`, and selector drift is reported once via
 * `console.warn` plus `getDiagnostic()`.
 *
 * Audio is the mixed system capture only (Tier 1): Meet forwards a few
 * "loudest speaker" streams rather than one per participant, so there is no
 * per-participant PCM.
 */

export const GMEET_SELECTORS = {
  tile: '[data-participant-id]',
  tileIdAttr: 'data-participant-id',
  speaking: '[jsname="QgSmzd"]',
  silentClass: 'gjg47c',
  speakingClasses: ['wEsLMd', 'Oaajhc'],
  // Leave-call control: present only while actually in the call (not in the
  // lobby or on the post-call screen). Best-effort, verify on a live call.
  inCallMarker: 'button[jsname="CQylAd"], button[aria-label="Leave call"]',
  // Controls Meet renders only on the local participant's own tile.
  // UI-language dependent, best-effort.
  selfTileMarker: 'button[aria-label="Reframe"]',
  // Muted-microphone marker inside a tile. Best-effort; unknown → not muted.
  micMutedMarker: '[data-is-muted="true"]',
};

const ROOM_PATH_RE = /^\/[a-z]{3}-[a-z]{4}-[a-z]{3}$/;
const TITLE_PREFIX_RE = /^\s*Meet\s*[–—-]\s*/;

export const SPEAKER_GRACE_MS = 1000;
const REEVALUATE_MARGIN_MS = 50;
export const IN_MEETING_DEBOUNCE_MS = 6000;
export const CALLBACK_THROTTLE_MS = 100;
export const DRIFT_DURATION_MS = 30000;

const DRIFT_MESSAGES = {
  no_tiles: 'Google Meet: плитки участников не найдены — определение говорящего не работает',
  no_indicator: 'Google Meet: индикатор речи не найден — определение говорящего не работает',
  unknown_indicator: 'Google Meet: неизвестное состояние индикатора речи — определение говорящего отключено',
  all_speaking: 'Google Meet: все участники «говорят» дольше 30 с — определение говорящего отключено',
};

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested in gmeet.test.js)
// ---------------------------------------------------------------------------

/** `origin + pathname` for a Meet room path (`/abc-defg-hij`), else null. */
export function buildMeetingUrl(location) {
  if (!location || !ROOM_PATH_RE.test(location.pathname || '')) return null;
  return `${location.origin}${location.pathname}`;
}

/**
 * Display name = first non-empty line of the tile's `innerText` (rendering
 * aware: hidden text is excluded).
 */
export function tileDisplayName(tile) {
  return (tile?.innerText || '').split('\n').map((s) => s.trim()).find(Boolean) || '';
}

/** `'speaking' | 'silent' | 'unknown'` for one speaking-indicator element. */
export function indicatorState(el) {
  const cls = el.classList;
  if (GMEET_SELECTORS.speakingClasses.some((c) => cls.contains(c))) return 'speaking';
  if (cls.contains(GMEET_SELECTORS.silentClass)) return 'silent';
  return 'unknown';
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

export class GoogleMeetAdapter {
  constructor(win = window, doc = document) {
    this.name = 'gmeet';
    this._win = win;
    this._doc = doc;
    // Stable conference object: init.js reattaches subscriptions whenever the
    // identity of getConference()/getJitsiConference() changes.
    this._conference = { platform: 'gmeet' };

    // Per-pid state, kept across dispose() so the post-join reattach is lossless.
    this._names = new Map(); // pid → last seen display name
    this._speaking = new Map(); // pid → merged speaking state (true/false)
    this._speakingSince = new Map(); // pid → ts of the last silent→speaking transition
    this._silentSince = new Map(); // pid → ts of the last speaking→silent transition
    this._absentSince = new Map(); // pid → ts it was first seen with no tile anywhere
    this._currentSpeakerId = null;
    this._snapshot = { present: new Map(), inCall: false, tileCount: 0 };

    // isInMeeting() debounce.
    this._tilesLastSeenAt = null;
    this._tilesLastSeenPath = null;

    // Drift detection.
    this._inCallNoTilesSince = null;
    this._allSpeakingSince = null;
    this._drift = null;
    this._driftWarned = new Set();

    // Live subscriptions (recreated by subscribeToSpeakerChanges).
    this._speakerCallback = null;
    this._joinCallback = null;
    this._announcedPids = new Set();
    this._observers = new Map(); // document → MutationObserver
    this._pipEnterHandler = null;
    this._pipWindow = null;
    this._pipPagehideHandler = null;
    this._throttleTimer = null;
    this._reevaluateTimer = null;
    this._lastChangeSignature = null;
  }

  _pipDocument() {
    try {
      return this._win.documentPictureInPicture?.window?.document || null;
    } catch (e) {
      return null;
    }
  }

  /** Main document plus the Document PiP document when one is open. */
  _docs() {
    const docs = [this._doc];
    const pip = this._pipDocument();
    if (pip && pip !== this._doc) docs.push(pip);
    return docs;
  }

  // -- DOM scan --------------------------------------------------------------

  /**
   * Scan every document, merge tiles per pid and update the per-pid speaking
   * transitions, the isInMeeting() debounce and drift detection.
   */
  _refresh(now = Date.now()) {
    const sel = GMEET_SELECTORS;
    const present = new Map(); // pid → { speaking, isLocal, micMuted }
    let tileCount = 0;
    let indicatorCount = 0;
    let unknownIndicator = false;
    let inCall = false;

    for (const doc of this._docs()) {
      if (doc.querySelector(sel.inCallMarker)) inCall = true;
      for (const tile of doc.querySelectorAll(sel.tile)) {
        const pid = tile.getAttribute(sel.tileIdAttr);
        if (!pid) continue;
        tileCount++;
        const entry = present.get(pid) || { speaking: false, isLocal: false, micMuted: false };
        for (const indicator of tile.querySelectorAll(sel.speaking)) {
          indicatorCount++;
          const state = indicatorState(indicator);
          if (state === 'speaking') entry.speaking = true;
          else if (state === 'unknown') unknownIndicator = true;
        }
        if (tile.querySelector(sel.selfTileMarker)) entry.isLocal = true;
        if (tile.querySelector(sel.micMutedMarker)) entry.micMuted = true;
        const name = tileDisplayName(tile);
        if (name) this._names.set(pid, name);
        present.set(pid, entry);
      }
    }

    for (const [pid, entry] of present) {
      const prev = this._speaking.get(pid);
      if (entry.speaking && prev !== true) this._speakingSince.set(pid, now);
      if (!entry.speaking && prev === true) this._silentSince.set(pid, now);
      this._speaking.set(pid, entry.speaking);
      this._absentSince.delete(pid);
    }
    // A pid with no tile anywhere (PiP handover, left the call) keeps its
    // speaking state, so a re-added tile does not restamp speakingSince.
    for (const pid of this._speaking.keys()) {
      if (!present.has(pid) && !this._absentSince.has(pid)) this._absentSince.set(pid, now);
    }

    const path = this._win.location.pathname;
    if (tileCount > 0) {
      this._tilesLastSeenAt = now;
      this._tilesLastSeenPath = path;
    }

    this._snapshot = { present, inCall, tileCount };
    this._updateDrift(now, { present, inCall, tileCount, indicatorCount, unknownIndicator });
    return this._snapshot;
  }

  _updateDrift(now, { present, inCall, tileCount, indicatorCount, unknownIndicator }) {
    if (!inCall) {
      // Lobby / post-call pages never report drift.
      this._inCallNoTilesSince = null;
      this._allSpeakingSince = null;
      this._drift = null;
      return;
    }

    if (tileCount === 0) {
      if (this._inCallNoTilesSince === null) this._inCallNoTilesSince = now;
    } else {
      this._inCallNoTilesSince = null;
    }

    const allSpeaking =
      present.size >= 2 && [...present.values()].every((entry) => entry.speaking);
    if (allSpeaking) {
      if (this._allSpeakingSince === null) this._allSpeakingSince = now;
    } else {
      this._allSpeakingSince = null;
    }

    let drift = null;
    if (unknownIndicator) drift = 'unknown_indicator';
    else if (this._allSpeakingSince !== null && now - this._allSpeakingSince > DRIFT_DURATION_MS) {
      drift = 'all_speaking';
    } else if (tileCount > 0 && indicatorCount === 0) drift = 'no_indicator';
    else if (
      this._inCallNoTilesSince !== null &&
      now - this._inCallNoTilesSince > DRIFT_DURATION_MS
    ) {
      drift = 'no_tiles';
    }

    // Time-based drift (all_speaking, no_tiles) begins and ends with no DOM
    // mutation; notify subscribers so the speaker update (null while drift
    // suspends attribution) reaches the bridge, not only the status line.
    if (drift !== this._drift && this._speakerCallback) this._scheduleChange();
    this._drift = drift;
    if (drift && !this._driftWarned.has(drift)) {
      this._driftWarned.add(drift);
      console.warn(`[recog][gmeet] selector drift (${drift}): ${DRIFT_MESSAGES[drift]}`);
    }
  }

  /** Short user-facing text while selector drift is detected, else null. */
  getDiagnostic() {
    this._refresh();
    return this._drift ? DRIFT_MESSAGES[this._drift] : null;
  }

  // -- Conference lifecycle --------------------------------------------------

  /** Optional hook: false outside a room path (`/`, `/home`, `/landing`). */
  isRoomPage() {
    return this.getMeetingUrl() !== null;
  }

  /**
   * Room path AND tiles present now, or seen on this same room path within the
   * last IN_MEETING_DEBOUNCE_MS (absorbs the PiP handover, when for a moment no
   * document has tiles). Time-based so several callers cannot skew it.
   */
  isInMeeting() {
    if (!this.isRoomPage()) return false;
    const now = Date.now();
    const { tileCount } = this._refresh(now);
    if (tileCount > 0) return true;
    return (
      this._tilesLastSeenAt !== null &&
      this._tilesLastSeenPath === this._win.location.pathname &&
      now - this._tilesLastSeenAt < IN_MEETING_DEBOUNCE_MS
    );
  }

  isConferenceAvailable() {
    return this.isInMeeting();
  }

  isInPrejoin() {
    return this.isRoomPage() && !this.isInMeeting();
  }

  waitForConference(maxAttempts = 200) {
    return new Promise((resolve, reject) => {
      let attempts = 0;
      const check = () => {
        attempts++;
        if (this.isConferenceAvailable()) {
          console.log('[Google Meet Adapter] Conference found (participant tiles)');
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
    const { present } = this._refresh();
    return [...present].map(([pid, entry]) => ({
      id: pid,
      name: this.getParticipantName(pid),
      isLocal: entry.isLocal,
      isSpeaking: entry.speaking,
      micMuted: entry.micMuted,
    }));
  }

  getParticipantName(participantId) {
    if (!participantId) return 'Unknown';
    return this._names.get(participantId) || participantId;
  }

  // -- Active speaker --------------------------------------------------------

  _speakingNow(pid) {
    return this._snapshot.present.get(pid)?.speaking === true;
  }

  /** When `pid` stopped counting as speaking (null while it still speaks). */
  _quietSince(pid) {
    if (this._speakingNow(pid)) return null;
    if (this._speaking.get(pid) === false) return this._silentSince.get(pid) ?? 0;
    return this._absentSince.get(pid) ?? 0;
  }

  _speakerInfo(pid) {
    return {
      id: pid,
      name: this.getParticipantName(pid),
      isLocal: this._snapshot.present.get(pid)?.isLocal === true,
    };
  }

  getActiveSpeaker() {
    const now = Date.now();
    this._refresh(now);
    // Speaking state cannot be trusted while the indicator classes look wrong.
    if (this._drift === 'unknown_indicator' || this._drift === 'all_speaking') {
      this._currentSpeakerId = null;
      return null;
    }

    const current = this._currentSpeakerId;
    if (current) {
      const quietSince = this._quietSince(current);
      if (quietSince === null) return this._speakerInfo(current);
      const remaining = quietSince + SPEAKER_GRACE_MS - now;
      if (remaining > 0) {
        this._scheduleReevaluate(remaining + REEVALUATE_MARGIN_MS);
        return this._speakerInfo(current);
      }
    }

    let next = null;
    for (const [pid, entry] of this._snapshot.present) {
      if (!entry.speaking) continue;
      if (next === null || (this._speakingSince.get(pid) ?? 0) > (this._speakingSince.get(next) ?? 0)) {
        next = pid;
      }
    }
    this._currentSpeakerId = next;
    return next ? this._speakerInfo(next) : null;
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
    const sel = GMEET_SELECTORS;
    if (record.type === 'attributes') {
      return record.target?.matches?.(sel.speaking) === true;
    }
    // An indicator re-rendered inside an existing tile (removed or replaced)
    // changes speaking state without touching the tile itself.
    const touchesTileOrIndicator = (node) =>
      node.nodeType === 1 &&
      (node.matches(sel.tile) ||
        node.matches(sel.speaking) ||
        !!node.querySelector(`${sel.tile}, ${sel.speaking}`));
    return [...record.addedNodes, ...record.removedNodes].some(touchesTileOrIndicator);
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

  /**
   * Everything the speaker callback reports. The indicator flips between
   * volume-level classes the whole time someone talks; those mutations leave
   * the signature unchanged and must not resend speaker/participant updates.
   */
  _changeSignature(present) {
    const rows = [...present].map(
      ([pid, e]) => `${pid}\u0001${this._names.get(pid) || ''}\u0001${+e.speaking}${+e.isLocal}${+e.micMuted}`,
    );
    return `${this._drift || ''}\u0002${rows.join('\u0002')}`;
  }

  _handleChange() {
    const { present } = this._refresh();
    const joined = [...present.keys()].filter((pid) => !this._announcedPids.has(pid));
    joined.forEach((pid) => this._announcedPids.add(pid));
    const signature = this._changeSignature(present);
    if (signature === this._lastChangeSignature) return;
    this._lastChangeSignature = signature;
    this._speakerCallback?.();
    if (this._joinCallback) joined.forEach((pid) => this._joinCallback?.(pid));
  }

  _observe(doc) {
    if (!doc || this._observers.has(doc)) return;
    const Observer = doc.defaultView?.MutationObserver || this._win.MutationObserver;
    if (!Observer) return;
    const observer = new Observer((records) => this._onMutations(records));
    observer.observe(doc.documentElement || doc, {
      attributes: true,
      attributeFilter: ['class'],
      childList: true,
      subtree: true,
    });
    this._observers.set(doc, observer);
  }

  _unobserve(doc) {
    this._observers.get(doc)?.disconnect();
    this._observers.delete(doc);
  }

  _attachPipWindow(pipWindow) {
    if (!pipWindow || pipWindow === this._pipWindow) return;
    this._detachPipWindow();
    this._pipWindow = pipWindow;
    const pipDoc = pipWindow.document;
    this._pipPagehideHandler = () => {
      this._unobserve(pipDoc);
      if (this._pipWindow === pipWindow) this._detachPipWindow();
    };
    pipWindow.addEventListener?.('pagehide', this._pipPagehideHandler);
    this._observe(pipDoc);
    // Tiles may already have moved in before the observer was attached.
    this._scheduleChange();
  }

  _detachPipWindow() {
    if (this._pipWindow && this._pipPagehideHandler) {
      this._pipWindow.removeEventListener?.('pagehide', this._pipPagehideHandler);
    }
    this._pipWindow = null;
    this._pipPagehideHandler = null;
  }

  /**
   * Creates every live subscription: an observer per document existing now
   * (an already-open PiP included), the PiP `enter` listener that observes a
   * new PiP document, and its `pagehide` listener that drops that observer.
   */
  subscribeToSpeakerChanges(callback) {
    this._speakerCallback = callback;
    this._lastChangeSignature = null;
    this._announcedPids = new Set(this._refresh().present.keys());
    this._observe(this._doc);

    const dpip = this._win.documentPictureInPicture;
    if (dpip?.addEventListener && !this._pipEnterHandler) {
      this._pipEnterHandler = (event) => this._attachPipWindow(event?.window || dpip.window);
      dpip.addEventListener('enter', this._pipEnterHandler);
    }
    if (dpip?.window) this._attachPipWindow(dpip.window);
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
    return buildMeetingUrl(this._win.location);
  }

  /**
   * `document.title` is "Meet – <code>" (or "Meet – <calendar title>" for a
   * scheduled event); fall back to the meeting code from the path.
   */
  getMeetingTitle() {
    const title = (this._doc.title || '').trim();
    if (TITLE_PREFIX_RE.test(title)) {
      const rest = title.replace(TITLE_PREFIX_RE, '').trim();
      if (rest) return rest;
    }
    return this.isRoomPage() ? this._win.location.pathname.slice(1) : '';
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
    for (const observer of this._observers.values()) observer.disconnect();
    this._observers.clear();
    const dpip = this._win.documentPictureInPicture;
    if (this._pipEnterHandler) {
      dpip?.removeEventListener?.('enter', this._pipEnterHandler);
      this._pipEnterHandler = null;
    }
    this._detachPipWindow();
    clearTimeout(this._throttleTimer);
    this._throttleTimer = null;
    clearTimeout(this._reevaluateTimer);
    this._reevaluateTimer = null;
    this._speakerCallback = null;
    // Clear the join callback so a reattach re-wires it from exactly one site
    // (mirrors ElementCallAdapter.dispose()). Per-pid speaking state, the
    // current speaker and the stable conference object are intentionally kept.
    this._joinCallback = null;
  }
}
