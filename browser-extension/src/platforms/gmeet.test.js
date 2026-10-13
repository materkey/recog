/**
 * Tests for the Google Meet adapter: DOM speaker detection, Document PiP,
 * the isInMeeting() debounce, selector-drift detection and the
 * dispose → resubscribe contract init.js relies on.
 *
 * Fixtures are built from the exported GMEET_SELECTORS, so a Meet redesign
 * that changes a selector keeps these tests meaningful.
 *
 * @jest-environment-options {"url": "https://meet.google.com/qrs-tuvw-xyz"}
 * @jest-environment jsdom
 */

import {
  GMEET_SELECTORS,
  GoogleMeetAdapter,
  buildMeetingUrl,
  tileDisplayName,
  indicatorState,
  SPEAKER_GRACE_MS,
  IN_MEETING_DEBOUNCE_MS,
  CALLBACK_THROTTLE_MS,
  DRIFT_DURATION_MS,
} from './gmeet.js';

const ROOM = 'https://meet.google.com/qrs-tuvw-xyz';
const [SPEAKING, SPEAKING_LOUD] = GMEET_SELECTORS.speakingClasses;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/** Create an element from a simple `tag.class[attr="value"]…` selector (first alternative). */
function fromSelector(doc, selector) {
  const first = selector.split(',')[0].trim();
  const tag = first.match(/^[a-z]+/i)?.[0] || 'div';
  const el = doc.createElement(tag);
  for (const [, cls] of first.replace(/\[[^\]]*\]/g, '').matchAll(/\.([\w-]+)/g)) {
    el.classList.add(cls);
  }
  for (const [, attr, value] of first.matchAll(/\[([^=\]]+)="([^"]*)"\]/g)) {
    el.setAttribute(attr, value);
  }
  return el;
}

function addTile(doc, pid, name, { state = GMEET_SELECTORS.silentClass, self = false, muted = false } = {}) {
  const tile = doc.createElement('div');
  tile.setAttribute(GMEET_SELECTORS.tileIdAttr, pid);
  // Meet puts an empty icon holder of the same selector next to the name tag.
  for (const text of [name, '']) {
    const label = fromSelector(doc, GMEET_SELECTORS.name);
    label.textContent = text;
    tile.appendChild(label);
  }
  const indicator = fromSelector(doc, GMEET_SELECTORS.speaking);
  indicator.className = state;
  tile.appendChild(indicator);
  if (self) tile.appendChild(fromSelector(doc, GMEET_SELECTORS.selfTileMarker));
  if (muted) tile.appendChild(fromSelector(doc, GMEET_SELECTORS.micMutedMarker));
  doc.body.appendChild(tile);
  return tile;
}

function setState(tile, state) {
  tile.querySelector(GMEET_SELECTORS.speaking).className = state;
}

function addInCallMarker(doc) {
  const marker = fromSelector(doc, GMEET_SELECTORS.inCallMarker);
  doc.body.appendChild(marker);
  return marker;
}

class FakePip extends EventTarget {
  constructor() {
    super();
    this.window = null;
  }

  open(pipDoc) {
    const pipWindow = new EventTarget();
    pipWindow.document = pipDoc;
    this.window = pipWindow;
    const event = new Event('enter');
    event.window = pipWindow;
    this.dispatchEvent(event);
    return pipWindow;
  }

  close() {
    const pipWindow = this.window;
    this.window = null;
    pipWindow.dispatchEvent(new Event('pagehide'));
  }
}

function makeWin(url = ROOM, pip = null) {
  return {
    location: new URL(url),
    MutationObserver: window.MutationObserver,
    documentPictureInPicture: pip,
  };
}

function newDoc() {
  return document.implementation.createHTMLDocument('meet');
}

/** Let MutationObserver callbacks (microtasks) run, then fire the throttle. */
async function flush(ms = CALLBACK_THROTTLE_MS) {
  await Promise.resolve();
  await Promise.resolve();
  jest.advanceTimersByTime(ms);
}

beforeEach(() => {
  jest.useFakeTimers({ doNotFake: ['queueMicrotask', 'nextTick'] });
  jest.setSystemTime(new Date('2026-09-26T12:00:00Z'));
  jest.spyOn(console, 'warn').mockImplementation(() => {});
  jest.spyOn(console, 'log').mockImplementation(() => {});
});

afterEach(() => {
  jest.useRealTimers();
  jest.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

describe('pure helpers', () => {
  test('room path - buildMeetingUrl - origin + pathname without query', () => {
    expect(buildMeetingUrl(new URL('https://meet.google.com/qrs-tuvw-xyz?authuser=0'))).toBe(ROOM);
  });

  test.each(['/', '/home', '/landing', '/qrs-tuvw-xyz/extra', '/QRS-TUVW-XYZ'])(
    'non-room path %s - buildMeetingUrl - null',
    (path) => {
      expect(buildMeetingUrl(new URL(`https://meet.google.com${path}`))).toBeNull();
    },
  );

  test('tile with a padded name tag - tileDisplayName - trimmed name', () => {
    const doc = newDoc();
    const tile = addTile(doc, 'spaces/s/devices/1', '  Alice  ');
    expect(tileDisplayName(tile)).toBe('Alice');
  });

  test('tile with hover controls and a paused-video notice before the name tag - tileDisplayName - name tag only', () => {
    const doc = newDoc();
    const tile = addTile(doc, 'spaces/s/devices/1', 'Alice');
    const notice = doc.createElement('div');
    notice.textContent = 'This video is paused due to problems with your network';
    const icon = doc.createElement('i');
    icon.className = 'google-symbols notranslate';
    icon.textContent = 'keep_outline';
    tile.prepend(notice, icon);
    expect(tileDisplayName(tile)).toBe('Alice');
  });

  test('presentation tile with controls but no name tag - tileDisplayName - empty string', () => {
    const doc = newDoc();
    const tile = doc.createElement('div');
    const icon = doc.createElement('i');
    icon.className = 'google-symbols notranslate';
    icon.textContent = 'zoom_in';
    tile.appendChild(icon);
    expect(tileDisplayName(tile)).toBe('');
  });

  test.each(['wEsLMd', 'Oaajhc', 'OgVli', 'HX2H7'])(
    'indicator at volume level %s - indicatorState - speaking',
    (level) => {
      const el = document.createElement('div');
      el.className = `x ${level} y`;
      expect(indicatorState(el)).toBe('speaking');
    },
  );

  test('indicator classes - indicatorState - speaking, silent or unknown', () => {
    const el = document.createElement('div');
    el.className = `x ${SPEAKING_LOUD}`;
    expect(indicatorState(el)).toBe('speaking');
    el.className = `x ${GMEET_SELECTORS.silentClass}`;
    expect(indicatorState(el)).toBe('silent');
    el.className = 'x renamedSilent';
    expect(indicatorState(el)).toBe('unknown');
  });
});

// ---------------------------------------------------------------------------
// Participants and active speaker
// ---------------------------------------------------------------------------

describe('participants and active speaker', () => {
  test('two tiles, one self and muted - getParticipants - names, local and muted flags', () => {
    const doc = newDoc();
    addTile(doc, 'spaces/s/devices/1', 'Alice', { self: true, muted: true });
    addTile(doc, 'spaces/s/devices/2', 'Bob', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    expect(adapter.getParticipants()).toEqual([
      { id: 'spaces/s/devices/1', name: 'Alice', isLocal: true, isSpeaking: false, micMuted: true },
      { id: 'spaces/s/devices/2', name: 'Bob', isLocal: false, isSpeaking: true, micMuted: false },
    ]);
    expect(adapter.isMicMuted('spaces/s/devices/1')).toBe(true);
    expect(adapter.anyMicOn()).toBe(true);
    expect(adapter.getParticipantName('spaces/s/devices/2')).toBe('Bob');
  });

  test('silent tile subscribed - indicator flips to speaking - callback fires and speaker is reported', async () => {
    const doc = newDoc();
    const bob = addTile(doc, 'spaces/s/devices/2', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    expect(adapter.getActiveSpeaker()).toBeNull();

    setState(bob, SPEAKING);
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getActiveSpeaker()).toEqual({ id: 'spaces/s/devices/2', name: 'Bob', isLocal: false });
  });

  test('no tiles - anyMicOn - unknown counts as on', () => {
    const adapter = new GoogleMeetAdapter(makeWin(), newDoc());
    expect(adapter.anyMicOn()).toBe(true);
  });

  test('every tile muted - anyMicOn - false', () => {
    const doc = newDoc();
    addTile(doc, 'a', 'Alice', { muted: true });
    addTile(doc, 'b', 'Bob', { muted: true });
    expect(new GoogleMeetAdapter(makeWin(), doc).anyMicOn()).toBe(false);
  });

  test('current speaker tile removed - after grace - the other speaker takes over', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    expect(adapter.getActiveSpeaker().id).toBe('a');
    jest.advanceTimersByTime(10);
    addTile(doc, 'b', 'Bob', { state: SPEAKING });
    expect(adapter.getActiveSpeaker().id).toBe('a');

    alice.remove();
    expect(adapter.getActiveSpeaker().id).toBe('a');
    jest.advanceTimersByTime(SPEAKER_GRACE_MS + 1);
    expect(adapter.getActiveSpeaker().id).toBe('b');
  });

  test('no current speaker - two speakers start one after another - most recent transition wins', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice');
    const bob = addTile(doc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    adapter.getParticipants();

    setState(alice, SPEAKING);
    adapter.getParticipants(); // alice transition observed
    jest.advanceTimersByTime(500);
    setState(bob, SPEAKING);

    expect(adapter.getActiveSpeaker().id).toBe('b');
  });

  test('current speaker - another starts while current still speaks - current is kept', () => {
    const doc = newDoc();
    addTile(doc, 'a', 'Alice', { state: SPEAKING });
    const bob = addTile(doc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    expect(adapter.getActiveSpeaker().id).toBe('a');

    jest.advanceTimersByTime(500);
    setState(bob, SPEAKING);
    expect(adapter.getActiveSpeaker().id).toBe('a');
  });

  test('current speaker goes silent while another speaks - within grace kept, then the timer switches', async () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice', { state: SPEAKING });
    const bob = addTile(doc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const cb = jest.fn(() => adapter.getActiveSpeaker());
    adapter.subscribeToSpeakerChanges(cb);
    expect(adapter.getActiveSpeaker().id).toBe('a');

    setState(bob, SPEAKING);
    setState(alice, GMEET_SELECTORS.silentClass);
    await flush();
    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb.mock.results[0].value.id).toBe('a');

    jest.advanceTimersByTime(SPEAKER_GRACE_MS - CALLBACK_THROTTLE_MS - 100);
    expect(adapter.getActiveSpeaker().id).toBe('a');
    expect(cb).toHaveBeenCalledTimes(1);

    jest.advanceTimersByTime(500);
    expect(cb).toHaveBeenCalledTimes(2);
    expect(cb.mock.results[1].value.id).toBe('b');
  });

  test('current speaker goes silent and nobody else speaks - after grace - null', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    expect(adapter.getActiveSpeaker().id).toBe('a');

    setState(alice, GMEET_SELECTORS.silentClass);
    expect(adapter.getActiveSpeaker().id).toBe('a');
    jest.advanceTimersByTime(SPEAKER_GRACE_MS + 1);
    expect(adapter.getActiveSpeaker()).toBeNull();
  });

  test('two speakers, later one current - earlier oscillates between volume levels - winner unchanged', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice');
    const bob = addTile(doc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    setState(alice, SPEAKING);
    adapter.getParticipants();
    jest.advanceTimersByTime(500);
    setState(bob, SPEAKING);
    expect(adapter.getActiveSpeaker().id).toBe('b');

    // Bob stops; while Alice's level oscillates the most recent transition is still Bob's.
    for (const state of [SPEAKING_LOUD, SPEAKING, SPEAKING_LOUD]) {
      jest.advanceTimersByTime(100);
      setState(alice, state);
      adapter.getParticipants();
    }
    setState(bob, GMEET_SELECTORS.silentClass);
    jest.advanceTimersByTime(100);
    setState(bob, SPEAKING);
    adapter.getParticipants();
    jest.advanceTimersByTime(100);
    setState(alice, SPEAKING);
    expect(adapter.getActiveSpeaker().id).toBe('b');
  });

  test('level oscillation of the only earlier speaker - no current speaker - later speaker still wins', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice');
    const bob = addTile(doc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    setState(alice, SPEAKING);
    adapter.getParticipants();
    jest.advanceTimersByTime(500);
    setState(bob, SPEAKING);
    adapter.getParticipants();
    jest.advanceTimersByTime(100);
    setState(alice, SPEAKING_LOUD);
    adapter.getParticipants();

    expect(adapter.getActiveSpeaker().id).toBe('b');
  });

  test('speaking tile moves into PiP - re-added there - speakingSince not restamped, winner unchanged', () => {
    const doc = newDoc();
    const pipDoc = newDoc();
    const pip = new FakePip();
    const alice = addTile(doc, 'a', 'Alice');
    const bob = addTile(doc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(ROOM, pip), doc);

    setState(alice, SPEAKING);
    adapter.getParticipants();
    jest.advanceTimersByTime(500);
    setState(bob, SPEAKING);
    adapter.getParticipants();

    // Handover: tiles vanish from the main document, then appear in PiP.
    alice.remove();
    bob.remove();
    adapter.getParticipants();
    jest.advanceTimersByTime(300);
    pip.open(pipDoc);
    addTile(pipDoc, 'a', 'Alice', { state: SPEAKING });
    addTile(pipDoc, 'b', 'Bob', { state: SPEAKING });

    expect(adapter.getActiveSpeaker().id).toBe('b');
  });

  test('same pid in main and PiP documents - getParticipants - counted once, speaking in either', () => {
    const doc = newDoc();
    const pipDoc = newDoc();
    const pip = new FakePip();
    addTile(doc, 'a', 'Alice');
    const adapter = new GoogleMeetAdapter(makeWin(ROOM, pip), doc);
    pip.open(pipDoc);
    addTile(pipDoc, 'a', 'Alice', { state: SPEAKING });

    const participants = adapter.getParticipants();
    expect(participants).toHaveLength(1);
    expect(participants[0].isSpeaking).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

describe('subscriptions', () => {
  test('subscribed - burst of indicator flips - callback fires once per throttle window', async () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice');
    const bob = addTile(doc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    setState(alice, SPEAKING);
    await flush(0);
    setState(bob, SPEAKING);
    await flush(0);
    setState(alice, SPEAKING_LOUD);
    await flush(0);
    expect(cb).not.toHaveBeenCalled();

    jest.advanceTimersByTime(CALLBACK_THROTTLE_MS);
    expect(cb).toHaveBeenCalledTimes(1);
  });

  test('subscribed speaker - indicator only switches volume level - no callback', async () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    setState(alice, SPEAKING_LOUD);
    await flush();
    cb.mockClear();

    setState(alice, SPEAKING);
    await flush();
    setState(alice, SPEAKING_LOUD);
    await flush();
    expect(cb).not.toHaveBeenCalled();

    setState(alice, GMEET_SELECTORS.silentClass);
    await flush();
    expect(cb).toHaveBeenCalledTimes(1);
  });

  test('subscribed - unrelated DOM mutation - no callback', async () => {
    const doc = newDoc();
    addTile(doc, 'a', 'Alice');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    const other = doc.createElement('div');
    doc.body.appendChild(other);
    other.className = 'animating';
    await flush();

    expect(cb).not.toHaveBeenCalled();
  });

  test('subscribed speaker - indicator replaced by a silent one inside the tile - callback fires, no speaker', async () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    expect(adapter.getActiveSpeaker().id).toBe('a');

    const replacement = fromSelector(doc, GMEET_SELECTORS.speaking);
    replacement.className = GMEET_SELECTORS.silentClass;
    alice.querySelector(GMEET_SELECTORS.speaking).replaceWith(replacement);
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
    jest.advanceTimersByTime(SPEAKER_GRACE_MS + 1);
    expect(adapter.getActiveSpeaker()).toBeNull();
  });

  test('subscribed with join callback - new tile appears - join callback gets its pid', async () => {
    const doc = newDoc();
    addTile(doc, 'a', 'Alice');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const joined = jest.fn();
    adapter.subscribeToSpeakerChanges(jest.fn());
    adapter.onParticipantJoined(joined);

    addTile(doc, 'b', 'Bob');
    await flush();

    expect(joined).toHaveBeenCalledTimes(1);
    expect(joined).toHaveBeenCalledWith('b');
  });

  test('PiP already open at subscribe - its indicator flips - callback fires', async () => {
    const doc = newDoc();
    const pipDoc = newDoc();
    const pip = new FakePip();
    pip.open(pipDoc);
    const alice = addTile(pipDoc, 'a', 'Alice');
    const adapter = new GoogleMeetAdapter(makeWin(ROOM, pip), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    await flush();
    cb.mockClear();

    setState(alice, SPEAKING);
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getActiveSpeaker().id).toBe('a');
  });

  test('PiP opened after subscribe via enter - its indicator flips - callback fires', async () => {
    const doc = newDoc();
    const pipDoc = newDoc();
    const pip = new FakePip();
    const adapter = new GoogleMeetAdapter(makeWin(ROOM, pip), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    pip.open(pipDoc);
    const alice = addTile(pipDoc, 'a', 'Alice');
    await flush();
    cb.mockClear();

    setState(alice, SPEAKING);
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
  });

  test('PiP window fires pagehide - its document mutates - no callback', async () => {
    const doc = newDoc();
    const pipDoc = newDoc();
    const pip = new FakePip();
    const adapter = new GoogleMeetAdapter(makeWin(ROOM, pip), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    pip.open(pipDoc);
    const alice = addTile(pipDoc, 'a', 'Alice');
    await flush();

    pip.close();
    cb.mockClear();
    setState(alice, SPEAKING);
    await flush();

    expect(cb).not.toHaveBeenCalled();
  });

  test('dispose then resubscribe - PiP indicator flips - callback fires', async () => {
    const doc = newDoc();
    const pipDoc = newDoc();
    const pip = new FakePip();
    const adapter = new GoogleMeetAdapter(makeWin(ROOM, pip), doc);
    adapter.subscribeToSpeakerChanges(jest.fn());
    adapter.dispose();

    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    pip.open(pipDoc);
    const alice = addTile(pipDoc, 'a', 'Alice');
    await flush();
    cb.mockClear();

    setState(alice, SPEAKING);
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
  });

  test('dispose alone - indicator flips in either document - no callback', async () => {
    const doc = newDoc();
    const pipDoc = newDoc();
    const pip = new FakePip();
    pip.open(pipDoc);
    const alice = addTile(doc, 'a', 'Alice');
    const bob = addTile(pipDoc, 'b', 'Bob');
    const adapter = new GoogleMeetAdapter(makeWin(ROOM, pip), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    await flush();
    cb.mockClear();

    adapter.dispose();
    setState(alice, SPEAKING);
    setState(bob, SPEAKING);
    pip.open(newDoc());
    await flush(SPEAKER_GRACE_MS * 2);

    expect(cb).not.toHaveBeenCalled();
  });

  test('current speaker kept across dispose - resubscribe - still the active speaker', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice');
    addTile(doc, 'b', 'Bob', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    adapter.subscribeToSpeakerChanges(jest.fn());
    expect(adapter.getActiveSpeaker().id).toBe('b');

    adapter.dispose();
    adapter.subscribeToSpeakerChanges(jest.fn());
    setState(alice, SPEAKING);

    expect(adapter.getActiveSpeaker().id).toBe('b');
  });
});

// ---------------------------------------------------------------------------
// Conference lifecycle
// ---------------------------------------------------------------------------

describe('conference lifecycle', () => {
  test('room path with tiles - tiles vanish during PiP handover - isInMeeting stays true within the window', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice');
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    expect(adapter.isInMeeting()).toBe(true);

    alice.remove();
    jest.advanceTimersByTime(2000);
    expect(adapter.isInMeeting()).toBe(true);
    jest.advanceTimersByTime(2000);
    expect(adapter.isInMeeting()).toBe(true);

    jest.advanceTimersByTime(IN_MEETING_DEBOUNCE_MS);
    expect(adapter.isInMeeting()).toBe(false);
    expect(adapter.isInPrejoin()).toBe(true);
  });

  test('home page with tiles - isInMeeting - false', () => {
    const doc = newDoc();
    addTile(doc, 'a', 'Alice');
    const adapter = new GoogleMeetAdapter(makeWin('https://meet.google.com/home'), doc);

    expect(adapter.isInMeeting()).toBe(false);
    expect(adapter.isConferenceAvailable()).toBe(false);
    expect(adapter.isRoomPage()).toBe(false);
    expect(adapter.isInPrejoin()).toBe(false);
    expect(adapter.getMeetingUrl()).toBeNull();
  });

  test('tiles seen in one room - navigated to another room without tiles - isInMeeting false at once', () => {
    const doc = newDoc();
    const alice = addTile(doc, 'a', 'Alice');
    const win = makeWin();
    const adapter = new GoogleMeetAdapter(win, doc);
    expect(adapter.isInMeeting()).toBe(true);

    alice.remove();
    win.location = new URL('https://meet.google.com/abc-defg-hij');
    expect(adapter.isInMeeting()).toBe(false);
  });

  test('room page before tiles - isInMeeting / isInPrejoin - lobby state', () => {
    const adapter = new GoogleMeetAdapter(makeWin(`${ROOM}?authuser=0`), newDoc());

    expect(adapter.isRoomPage()).toBe(true);
    expect(adapter.getMeetingUrl()).toBe(ROOM);
    expect(adapter.isInMeeting()).toBe(false);
    expect(adapter.isInPrejoin()).toBe(true);
    expect(adapter.isConferenceReady()).toBe(false);
  });

  test('adapter lifetime - getConference and getJitsiConference - the same stable object', () => {
    const adapter = new GoogleMeetAdapter(makeWin(), newDoc());
    const conf = adapter.getConference();

    expect(conf).toBeTruthy();
    expect(adapter.getJitsiConference()).toBe(conf);
    adapter.dispose();
    expect(adapter.getConference()).toBe(conf);
    expect(adapter.supportsTabInjection()).toBe(false);
  });

  test('tiles appear later - waitForConference - resolves with the stable conference', async () => {
    const doc = newDoc();
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const promise = adapter.waitForConference(10);

    addTile(doc, 'a', 'Alice');
    jest.advanceTimersByTime(500);

    await expect(promise).resolves.toBe(adapter.getConference());
  });

  test('document title with a meeting name - getMeetingTitle - text after the Meet prefix', () => {
    const doc = newDoc();
    doc.title = 'Meet – Weekly sync';
    expect(new GoogleMeetAdapter(makeWin(), doc).getMeetingTitle()).toBe('Weekly sync');
  });

  test('document title without the Meet prefix - getMeetingTitle - meeting code', () => {
    const doc = newDoc();
    doc.title = 'Google Meet';
    expect(new GoogleMeetAdapter(makeWin(), doc).getMeetingTitle()).toBe('qrs-tuvw-xyz');
  });
});

// ---------------------------------------------------------------------------
// Selector drift
// ---------------------------------------------------------------------------

describe('selector drift', () => {
  test('in call with a renamed silent class - indicator unknown - not speaking, no speaker, diagnostic set', () => {
    const doc = newDoc();
    addInCallMarker(doc);
    addTile(doc, 'a', 'Alice', { state: 'renamedSilent' });
    addTile(doc, 'b', 'Bob', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    expect(adapter.getParticipants()[0].isSpeaking).toBe(false);
    expect(adapter.getActiveSpeaker()).toBeNull();
    expect(adapter.getDiagnostic()).toMatch(/Google Meet/);
    expect(console.warn).toHaveBeenCalledTimes(1);
    expect(console.warn.mock.calls[0][0]).toMatch(/^\[recog\]\[gmeet\]/);

    adapter.getDiagnostic();
    expect(console.warn).toHaveBeenCalledTimes(1);
  });

  test('unknown indicator drift - silent class restored - diagnostic cleared and speaker reported again', () => {
    const doc = newDoc();
    addInCallMarker(doc);
    const alice = addTile(doc, 'a', 'Alice', { state: 'renamedSilent' });
    addTile(doc, 'b', 'Bob', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    expect(adapter.getActiveSpeaker()).toBeNull();

    setState(alice, GMEET_SELECTORS.silentClass);

    expect(adapter.getDiagnostic()).toBeNull();
    expect(adapter.getActiveSpeaker().id).toBe('b');
  });

  test('lobby page without the in-call marker - renamed silent class - no diagnostic', () => {
    const doc = newDoc();
    addTile(doc, 'a', 'Alice', { state: 'renamedSilent' });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    expect(adapter.getDiagnostic()).toBeNull();
    expect(console.warn).not.toHaveBeenCalled();
  });

  test('in call with tiles but no speaking indicator - getDiagnostic - set', () => {
    const doc = newDoc();
    addInCallMarker(doc);
    const tile = addTile(doc, 'a', 'Alice');
    tile.querySelector(GMEET_SELECTORS.speaking).remove();
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    expect(adapter.getDiagnostic()).toMatch(/индикатор/);
  });

  test('in call with no tiles anywhere - over 30 s - diagnostic set only after the window', () => {
    const doc = newDoc();
    addInCallMarker(doc);
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    expect(adapter.getDiagnostic()).toBeNull();
    jest.advanceTimersByTime(DRIFT_DURATION_MS + 1);
    expect(adapter.getDiagnostic()).toMatch(/плитки/);
  });

  test('in call, every tile speaking - over 30 s - no speaker and diagnostic set', () => {
    const doc = newDoc();
    addInCallMarker(doc);
    addTile(doc, 'a', 'Alice', { state: SPEAKING });
    addTile(doc, 'b', 'Bob', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);

    expect(adapter.getActiveSpeaker().id).toBe('a');
    expect(adapter.getDiagnostic()).toBeNull();
    jest.advanceTimersByTime(DRIFT_DURATION_MS + 1);
    expect(adapter.getActiveSpeaker()).toBeNull();
    expect(adapter.getDiagnostic()).toMatch(/30/);
  });

  test('subscribed, every tile speaking with no further mutations - drift window expires - callback fires with no speaker', async () => {
    const doc = newDoc();
    addInCallMarker(doc);
    addTile(doc, 'a', 'Alice', { state: SPEAKING });
    addTile(doc, 'b', 'Bob', { state: SPEAKING });
    const adapter = new GoogleMeetAdapter(makeWin(), doc);
    const speakers = [];
    adapter.subscribeToSpeakerChanges(() => speakers.push(adapter.getActiveSpeaker()));
    expect(adapter.getActiveSpeaker().id).toBe('a');

    jest.advanceTimersByTime(DRIFT_DURATION_MS + 1);
    // The periodic diagnostic poll is the only thing that notices the expiry.
    expect(adapter.getDiagnostic()).toMatch(/30/);
    await flush();

    expect(speakers).toEqual([null]);
  });
});
