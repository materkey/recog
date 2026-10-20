/**
 * Tests for the Zoom web-client adapter: the canonical meeting URL, the
 * in-call debounce, DOM speaker detection, selector-drift detection and the
 * dispose → resubscribe contract init.js relies on.
 *
 * Fixtures are built from the exported ZOOM_SELECTORS, so a Zoom redesign that
 * changes a selector keeps these tests meaningful.
 *
 * @jest-environment-options {"url": "https://app.zoom.us/wc/12345678901/join"}
 * @jest-environment jsdom
 */

import {
  ZOOM_SELECTORS,
  ZoomAdapter,
  meetingIdFromPath,
  buildMeetingUrl,
  tileName,
  ownMicMuted,
  SPEAKER_GRACE_MS,
  IN_MEETING_DEBOUNCE_MS,
  CALLBACK_THROTTLE_MS,
  NO_TILES_DRIFT_MS,
  NO_ACTIVE_DRIFT_MS,
} from './zoom.js';

const MEETING = 'https://app.zoom.us/wc/12345678901/join';
const CANONICAL = 'https://zoom.us/j/12345678901';

const [STRIP, MAIN, GALLERY] = ZOOM_SELECTORS.tile.split(', ').map((s) => s.slice(1));
const [STRIP_ACTIVE, GALLERY_ACTIVE] = ZOOM_SELECTORS.activeTile.map((s) => s.slice(1));
const ACTIVE_CLASS = { [STRIP]: STRIP_ACTIVE, [GALLERY]: GALLERY_ACTIVE };

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function addTile(doc, name, { kind = STRIP, active = false, muted = false } = {}) {
  const tile = doc.createElement('div');
  tile.className = kind;
  const [footerSelector, nameTag] = ZOOM_SELECTORS.tileName.split(' ');
  const footer = doc.createElement('div');
  footer.className = footerSelector.slice(1);
  if (muted) {
    const icon = doc.createElement('i');
    icon.className = ZOOM_SELECTORS.micMutedMarker.slice(1);
    footer.appendChild(icon);
  }
  const label = doc.createElement(nameTag);
  label.textContent = name;
  footer.appendChild(label);
  tile.appendChild(footer);
  doc.body.appendChild(tile);
  if (active) setActive(tile, true);
  return tile;
}

function setActive(tile, active) {
  const kind = tile.classList[0];
  tile.classList.toggle(ACTIVE_CLASS[kind], active);
}

function rename(tile, name) {
  tile.querySelector(ZOOM_SELECTORS.tileName).firstChild.nodeValue = name;
}

function addInCallMarker(doc) {
  const marker = doc.createElement('footer');
  marker.id = ZOOM_SELECTORS.inCallMarker.slice(1);
  doc.body.appendChild(marker);
  return marker;
}

/** The toolbar's audio button, labelled as the client labels it. */
function addAudioButton(doc, label) {
  const button = doc.createElement('button');
  button.className = ZOOM_SELECTORS.audioButton.slice(1);
  button.setAttribute('aria-label', label);
  doc.body.appendChild(button);
  return button;
}

/** An adapter in a meeting entered with the prejoin name `Me`. */
function adapterKnowingMe(doc) {
  const input = addPrejoinName(doc, 'Me');
  const adapter = new ZoomAdapter(makeWin(), doc);
  adapter.getParticipants();
  input.remove();
  addInCallMarker(doc);
  return adapter;
}

function addPrejoinName(doc, name) {
  const input = doc.createElement('input');
  input.id = ZOOM_SELECTORS.selfNameInput.slice(1);
  input.value = name;
  doc.body.appendChild(input);
  return input;
}

function makeWin(url = MEETING) {
  return { location: new URL(url), MutationObserver: window.MutationObserver };
}

function newDoc(title = 'Weekly sync') {
  return document.implementation.createHTMLDocument(title);
}

/** A document already inside the meeting. */
function meetingDoc() {
  const doc = newDoc();
  addInCallMarker(doc);
  return doc;
}

/** Let MutationObserver callbacks (microtasks) run, then fire the throttle. */
async function flush(ms = CALLBACK_THROTTLE_MS) {
  await Promise.resolve();
  await Promise.resolve();
  jest.advanceTimersByTime(ms);
}

beforeEach(() => {
  jest.useFakeTimers({ doNotFake: ['queueMicrotask', 'nextTick'] });
  jest.setSystemTime(new Date('2026-10-04T12:00:00Z'));
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
  test.each(['/wc/12345678901/join', '/wc/12345678901/start', '/wc/join/12345678901', '/wc/12345678901'])(
    'web client path %s - meetingIdFromPath - numeric meeting id',
    (path) => {
      expect(meetingIdFromPath(path)).toBe('12345678901');
    },
  );

  test.each(['/wc', '/wc/', '/wc/home', '/wc/join', '/j/12345678901', '/wc/123/join', '/wc/12345678901x/join', ''])(
    'path without a meeting id %s - meetingIdFromPath - null',
    (path) => {
      expect(meetingIdFromPath(path)).toBeNull();
    },
  );

  test('meeting id - buildMeetingUrl - canonical zoom.us/j url; none without an id', () => {
    expect(buildMeetingUrl('12345678901')).toBe(CANONICAL);
    expect(buildMeetingUrl(null)).toBeNull();
  });

  test.each([
    ['mute my microphone', false],
    ['Mute my phone', false],
    ['unmute my microphone', true],
    ['unmute my phone', true],
    ['join audio', true],
    ['join audio disabled', true],
    ['audio settings', true],
    ['audio setting', true],
    ['audio', true],
    ['microphone', null],
    [null, null],
  ])('audio button labelled %p - ownMicMuted - %p', (label, expected) => {
    const doc = meetingDoc();
    if (label !== null) addAudioButton(doc, label);
    expect(ownMicMuted(doc)).toBe(expected);
  });

  test('tile footer with surrounding whitespace - tileName - trimmed name; empty without a footer', () => {
    const doc = newDoc();
    expect(tileName(addTile(doc, '  Alice  '))).toBe('Alice');
    expect(tileName(doc.createElement('div'))).toBe('');
  });
});

// ---------------------------------------------------------------------------
// Conference lifecycle
// ---------------------------------------------------------------------------

describe('conference lifecycle', () => {
  test('regional subdomain page with a password - getMeetingUrl - canonical url without host or password', () => {
    const win = makeWin('https://us05web.zoom.us/wc/12345678901/join?pwd=secret');
    expect(new ZoomAdapter(win, newDoc()).getMeetingUrl()).toBe(CANONICAL);
  });

  test('page without a meeting id - isRoomPage / isInMeeting - false even with an in-call marker', () => {
    const adapter = new ZoomAdapter(makeWin('https://app.zoom.us/wc/home'), meetingDoc());

    expect(adapter.isRoomPage()).toBe(false);
    expect(adapter.isInMeeting()).toBe(false);
    expect(adapter.isInPrejoin()).toBe(false);
    expect(adapter.getMeetingUrl()).toBeNull();
  });

  test('prejoin or waiting room (meeting path, no in-call marker) - isInMeeting / isInPrejoin - in prejoin only', () => {
    const doc = newDoc();
    addTile(doc, 'Alice'); // tiles without the toolbar are not a meeting
    const adapter = new ZoomAdapter(makeWin(), doc);

    expect(adapter.isRoomPage()).toBe(true);
    expect(adapter.isInMeeting()).toBe(false);
    expect(adapter.isConferenceAvailable()).toBe(false);
    expect(adapter.isInPrejoin()).toBe(true);
  });

  test('toolbar without tiles (Zoom flashes it before the waiting room) - isInMeeting - false, still prejoin', () => {
    const adapter = new ZoomAdapter(makeWin(), meetingDoc());

    expect(adapter.isInMeeting()).toBe(false);
    expect(adapter.isInPrejoin()).toBe(true);
  });

  test('entered meeting, tiles vanish while the toolbar stays (screen share) - isInMeeting - still true', () => {
    const doc = meetingDoc();
    const alice = addTile(doc, 'Alice');
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.isInMeeting()).toBe(true);

    alice.remove();
    jest.advanceTimersByTime(IN_MEETING_DEBOUNCE_MS * 3);

    expect(adapter.isInMeeting()).toBe(true);
  });

  test('left the meeting - toolbar flashes again without tiles - not in meeting until a tile shows', () => {
    const doc = newDoc();
    const marker = addInCallMarker(doc);
    const alice = addTile(doc, 'Alice');
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.isInMeeting()).toBe(true);

    marker.remove();
    alice.remove();
    jest.advanceTimersByTime(IN_MEETING_DEBOUNCE_MS + 1);
    expect(adapter.isInMeeting()).toBe(false);

    addInCallMarker(doc);
    expect(adapter.isInMeeting()).toBe(false);
    addTile(doc, 'Alice');
    expect(adapter.isInMeeting()).toBe(true);
  });

  test('in-call marker vanishes for a layout rebuild - isInMeeting - true within the window, false after', () => {
    const doc = newDoc();
    const marker = addInCallMarker(doc);
    addTile(doc, 'Alice');
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.isInMeeting()).toBe(true);

    marker.remove();
    jest.advanceTimersByTime(IN_MEETING_DEBOUNCE_MS - 1);
    expect(adapter.isInMeeting()).toBe(true);
    expect(adapter.getMeetingUrl()).toBe(CANONICAL);

    jest.advanceTimersByTime(2);
    expect(adapter.isInMeeting()).toBe(false);
  });

  test('in-call marker seen for another meeting - page moves to a new meeting id - not in meeting', () => {
    const doc = newDoc();
    const marker = addInCallMarker(doc);
    addTile(doc, 'Alice');
    const win = makeWin();
    const adapter = new ZoomAdapter(win, doc);
    expect(adapter.isInMeeting()).toBe(true);

    marker.remove();
    win.location = new URL('https://app.zoom.us/wc/10987654321/join');

    expect(adapter.isInMeeting()).toBe(false);
  });

  test('marker appears after a while - waitForConference - resolves with the stable conference object', async () => {
    const doc = newDoc();
    const adapter = new ZoomAdapter(makeWin(), doc);
    const found = adapter.waitForConference(5);

    addInCallMarker(doc);
    addTile(doc, 'Alice');
    jest.advanceTimersByTime(500);

    await expect(found).resolves.toBe(adapter.getConference());
    expect(adapter.getJitsiConference()).toBe(adapter.getConference());
  });

  test('never in the meeting - waitForConference - rejects after the attempts', async () => {
    const adapter = new ZoomAdapter(makeWin(), newDoc());
    const found = adapter.waitForConference(2);
    jest.advanceTimersByTime(500);
    await expect(found).rejects.toThrow('Conference not found');
  });

  test('generic prejoin page title - getMeetingTitle - empty until inside the meeting', () => {
    const doc = newDoc('Zoom meeting on web');
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.getMeetingTitle()).toBe('');

    addInCallMarker(doc);
    doc.title = '  Weekly sync ';
    expect(adapter.getMeetingTitle()).toBe('Weekly sync');
  });

  test('tiles in the meeting - isConferenceReady - true only with tiles', () => {
    const doc = meetingDoc();
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.isConferenceReady()).toBe(false);
    addTile(doc, 'Alice');
    expect(adapter.isConferenceReady()).toBe(true);
  });

  test('floating-panel platform - UI hooks - no tab injection', () => {
    const adapter = new ZoomAdapter(makeWin(), newDoc());
    expect(adapter.supportsTabInjection()).toBe(false);
    expect(adapter.getTabContainerSelector()).toBeNull();
    expect(adapter.getContentAreaSelector()).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Participants and active speaker
// ---------------------------------------------------------------------------

describe('participants and active speaker', () => {
  test('speaker view with an active strip tile - getParticipants / getActiveSpeaker - names, speaking and muted flags', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { muted: true });
    addTile(doc, 'Bob', { active: true });
    addTile(doc, 'Bob', { kind: MAIN });
    const adapter = new ZoomAdapter(makeWin(), doc);

    expect(adapter.getParticipants()).toEqual([
      { id: 'Alice', name: 'Alice', isLocal: false, isSpeaking: false, micMuted: true },
      { id: 'Bob', name: 'Bob', isLocal: false, isSpeaking: true, micMuted: false },
    ]);
    expect(adapter.getActiveSpeaker()).toEqual({ id: 'Bob', name: 'Bob', isLocal: false });
    expect(adapter.isMicMuted('Alice')).toBe(true);
    expect(adapter.isMicMuted('Bob')).toBe(false);
    expect(adapter.anyMicOn()).toBe(true);
    expect(adapter.getParticipantName('Bob')).toBe('Bob');
    expect(adapter.getParticipantName(null)).toBe('Unknown');
  });

  test('gallery view with an active tile - getActiveSpeaker - the active gallery tile', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { kind: GALLERY });
    addTile(doc, 'Bob', { kind: GALLERY, active: true });

    expect(new ZoomAdapter(makeWin(), doc).getActiveSpeaker()?.name).toBe('Bob');
  });

  test('pinned main tile and an explicit active strip tile disagree - getActiveSpeaker - explicit active wins', () => {
    const doc = meetingDoc();
    addTile(doc, 'Pinned', { kind: MAIN });
    addTile(doc, 'Pinned');
    addTile(doc, 'Talker', { active: true });

    expect(new ZoomAdapter(makeWin(), doc).getActiveSpeaker()?.name).toBe('Talker');
  });

  test('no explicit active tile, main tile present - getActiveSpeaker - main tile name', () => {
    const doc = meetingDoc();
    addTile(doc, 'Me');
    addTile(doc, 'Bob', { kind: MAIN });

    expect(new ZoomAdapter(makeWin(), doc).getActiveSpeaker()?.name).toBe('Bob');
  });

  test('gallery view before anyone spoke - getActiveSpeaker - null', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { kind: GALLERY });
    addTile(doc, 'Bob', { kind: GALLERY });

    expect(new ZoomAdapter(makeWin(), doc).getActiveSpeaker()).toBeNull();
  });

  test('active tile removed by a layout rebuild - within grace - speaker kept, then null', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { kind: GALLERY });
    const bob = addTile(doc, 'Bob', { kind: GALLERY, active: true });
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.getActiveSpeaker()?.name).toBe('Bob');

    bob.remove();
    expect(adapter.getActiveSpeaker()?.name).toBe('Bob'); // the loss is noticed here
    jest.advanceTimersByTime(SPEAKER_GRACE_MS - 1);
    expect(adapter.getActiveSpeaker()?.name).toBe('Bob');

    jest.advanceTimersByTime(2);
    expect(adapter.getActiveSpeaker()).toBeNull();
  });

  test('same person on the strip and the main tile, both muted - getParticipants - one muted participant', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { muted: true });
    addTile(doc, 'Alice', { kind: MAIN, muted: true });
    const adapter = new ZoomAdapter(makeWin(), doc);

    expect(adapter.getParticipants()).toEqual([
      { id: 'Alice', name: 'Alice', isLocal: false, isSpeaking: true, micMuted: true },
    ]);
    expect(adapter.anyMicOn()).toBe(false);
  });

  test('namesakes, only one of them muted - isMicMuted - false, so an unmuted namesake keeps attribution', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alex', { kind: GALLERY, muted: true });
    addTile(doc, 'Alex', { kind: GALLERY });
    const adapter = new ZoomAdapter(makeWin(), doc);

    expect(adapter.getParticipants()).toHaveLength(1);
    expect(adapter.isMicMuted('Alex')).toBe(false);
  });

  test('name typed on prejoin - same document enters the meeting - that tile is local', () => {
    const doc = newDoc();
    const input = addPrejoinName(doc, ' Me ');
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.isInMeeting()).toBe(false);

    input.remove();
    addInCallMarker(doc);
    addTile(doc, 'Me', { kind: GALLERY });
    addTile(doc, 'Bob', { kind: GALLERY });

    expect(adapter.getParticipants().map((p) => [p.name, p.isLocal])).toEqual([
      ['Me', true],
      ['Bob', false],
    ]);
  });

  test('own tile unmuted, toolbar offers to unmute - getLocalMicMuted - true, the tile itself unchanged', () => {
    const doc = newDoc();
    const adapter = adapterKnowingMe(doc);
    addTile(doc, 'Me', { kind: GALLERY });
    addAudioButton(doc, 'unmute my microphone');

    expect(adapter.getLocalMicMuted()).toBe(true);
    expect(adapter.getParticipants()).toEqual([
      { id: 'Me', name: 'Me', isLocal: true, isSpeaking: false, micMuted: false },
    ]);
  });

  test('own tile muted, toolbar offers to mute - getLocalMicMuted - false', () => {
    const doc = newDoc();
    const adapter = adapterKnowingMe(doc);
    addTile(doc, 'Me', { kind: GALLERY, muted: true });
    addAudioButton(doc, 'mute my microphone');

    expect(adapter.getLocalMicMuted()).toBe(false);
  });

  test('webinar attendee: no own tile, no audio button - getLocalMicMuted - true', () => {
    const doc = newDoc();
    const adapter = adapterKnowingMe(doc);
    addTile(doc, 'Host', { kind: MAIN });

    expect(adapter.getLocalMicMuted()).toBe(true);
    expect(adapter.getParticipants().map((p) => p.name)).toEqual(['Host']);
  });

  test('own name never seen, no audio button - getLocalMicMuted - null, left to the tiles', () => {
    const doc = meetingDoc();
    addTile(doc, 'Me', { kind: GALLERY });
    addTile(doc, 'Host', { kind: MAIN });

    expect(new ZoomAdapter(makeWin(), doc).getLocalMicMuted()).toBeNull();
  });

  test('no own tile, unknown audio button label - getLocalMicMuted - null, one warning', () => {
    const doc = newDoc();
    const adapter = adapterKnowingMe(doc);
    addTile(doc, 'Host', { kind: MAIN });
    addAudioButton(doc, 'microphone');

    expect(adapter.getLocalMicMuted()).toBeNull();
    expect(console.warn).toHaveBeenCalledTimes(1);
  });

  test('own tile, unknown audio button label - getLocalMicMuted - left to the tile, one warning', () => {
    const doc = newDoc();
    const adapter = adapterKnowingMe(doc);
    addTile(doc, 'Me', { kind: GALLERY });
    addAudioButton(doc, 'microphone');

    expect(adapter.getLocalMicMuted()).toBeNull();
    expect(adapter.getLocalMicMuted()).toBeNull();
    expect(console.warn).toHaveBeenCalledTimes(1);
    expect(console.warn.mock.calls[0][0]).toContain('microphone');
  });

  test('prejoin, no toolbar yet - getLocalMicMuted - null', () => {
    const doc = newDoc();
    addPrejoinName(doc, 'Me');

    expect(new ZoomAdapter(makeWin(), doc).getLocalMicMuted()).toBeNull();
  });

  test('prejoin skipped (name never seen) - getParticipants - nobody is local', () => {
    const doc = meetingDoc();
    addTile(doc, 'Me', { kind: GALLERY });

    expect(new ZoomAdapter(makeWin(), doc).getParticipants()[0].isLocal).toBe(false);
  });

  test('tile without a name - getParticipants - skipped', () => {
    const doc = meetingDoc();
    addTile(doc, '', { kind: GALLERY });
    addTile(doc, 'Bob', { kind: GALLERY });

    expect(new ZoomAdapter(makeWin(), doc).getParticipants().map((p) => p.name)).toEqual(['Bob']);
  });

  test('no tiles at all - anyMicOn - assumed on', () => {
    expect(new ZoomAdapter(makeWin(), meetingDoc()).anyMicOn()).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

describe('subscriptions', () => {
  test('subscribed - active class moves to another tile - callback fires once and the speaker changes', async () => {
    const doc = meetingDoc();
    const alice = addTile(doc, 'Alice', { active: true });
    const bob = addTile(doc, 'Bob');
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    setActive(alice, false);
    setActive(bob, true);
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getActiveSpeaker()?.name).toBe('Bob');
  });

  test('subscribed - audio button relabelled when the user unmutes - callback fires', async () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { active: true });
    const button = addAudioButton(doc, 'unmute my microphone');
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    button.setAttribute('aria-label', 'mute my microphone');
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getLocalMicMuted()).toBe(false);
  });

  test('webinar attendee not allowed to talk: "audio settings" button, no own tile, name unknown - getLocalMicMuted - true', () => {
    const doc = meetingDoc();
    addTile(doc, 'Host', { kind: MAIN });
    addAudioButton(doc, 'audio settings');

    expect(new ZoomAdapter(makeWin(), doc).getLocalMicMuted()).toBe(true);
  });

  test('subscribed, no own tile under a known name - a button of unknown wording mounts, then goes - callback each time', async () => {
    const doc = newDoc();
    const adapter = adapterKnowingMe(doc);
    addTile(doc, 'Host', { kind: MAIN });
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);
    expect(adapter.getLocalMicMuted()).toBe(true);

    const button = addAudioButton(doc, 'microphone');
    await flush();
    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getLocalMicMuted()).toBeNull();

    button.remove();
    await flush();
    expect(cb).toHaveBeenCalledTimes(2);
    expect(adapter.getLocalMicMuted()).toBe(true);
  });

  test('subscribed - audio button replaced by a new one - callback fires', async () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { active: true });
    const button = addAudioButton(doc, 'join audio');
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    button.remove();
    addAudioButton(doc, 'mute my microphone');
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getLocalMicMuted()).toBe(false);
  });

  test('subscribed - main tile renamed in place - callback fires', async () => {
    const doc = meetingDoc();
    addTile(doc, 'Me');
    const main = addTile(doc, 'Alice', { kind: MAIN });
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    rename(main, 'Bob');
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getActiveSpeaker()?.name).toBe('Bob');
  });

  test('subscribed - mutation outside tiles - callback does not fire', async () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { active: true });
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    const chat = doc.createElement('div');
    chat.className = 'chat-message';
    doc.body.appendChild(chat);
    chat.classList.add('chat-message--new');
    await flush();

    expect(cb).not.toHaveBeenCalled();
  });

  test('subscribed - tile class changes that keep the speaker - callback does not fire', async () => {
    const doc = meetingDoc();
    const alice = addTile(doc, 'Alice', { active: true });
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    alice.classList.add('react-draggable-dragging');
    await flush();

    expect(cb).not.toHaveBeenCalled();
  });

  test('mutation burst - subscribed - callback fires once per throttle window', async () => {
    const doc = meetingDoc();
    const alice = addTile(doc, 'Alice', { active: true });
    const bob = addTile(doc, 'Bob');
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    for (let i = 0; i < 5; i++) {
      setActive(alice, i % 2 === 1);
      setActive(bob, i % 2 === 0);
    }
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
  });

  test('subscribed - a new participant tile appears - join callback gets the name', async () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice');
    const adapter = new ZoomAdapter(makeWin(), doc);
    const joined = jest.fn();
    adapter.subscribeToSpeakerChanges(jest.fn());
    adapter.onParticipantJoined(joined);

    addTile(doc, 'Bob');
    await flush();

    expect(joined).toHaveBeenCalledTimes(1);
    expect(joined).toHaveBeenCalledWith('Bob');
  });

  test('subscribed - in-call marker is removed - callback fires so the leave is noticed promptly', async () => {
    const doc = newDoc();
    const marker = addInCallMarker(doc);
    const alice = addTile(doc, 'Alice', { active: true });
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    marker.remove();
    alice.remove();
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
  });

  test('dispose then resubscribe - active tile changes - callback fires and the speaker survived dispose', async () => {
    const doc = meetingDoc();
    const alice = addTile(doc, 'Alice', { active: true });
    const bob = addTile(doc, 'Bob');
    const adapter = new ZoomAdapter(makeWin(), doc);
    const first = jest.fn();
    adapter.subscribeToSpeakerChanges(first);
    const conference = adapter.getConference();

    adapter.dispose();
    expect(adapter.getConference()).toBe(conference);
    expect(adapter.getActiveSpeaker()?.name).toBe('Alice');

    const second = jest.fn();
    adapter.subscribeToSpeakerChanges(second);
    setActive(alice, false);
    setActive(bob, true);
    await flush();

    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  test('active tile removed while subscribed - grace expires - callback fires again to clear the speaker', async () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { kind: GALLERY });
    const bob = addTile(doc, 'Bob', { kind: GALLERY, active: true });
    const adapter = new ZoomAdapter(makeWin(), doc);
    const cb = jest.fn(() => adapter.getActiveSpeaker());
    adapter.subscribeToSpeakerChanges(cb);

    bob.remove();
    await flush();
    expect(cb).toHaveBeenCalledTimes(1);
    expect(adapter.getActiveSpeaker()?.name).toBe('Bob');

    jest.advanceTimersByTime(SPEAKER_GRACE_MS + CALLBACK_THROTTLE_MS);
    expect(cb).toHaveBeenCalledTimes(2);
    expect(adapter.getActiveSpeaker()).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Selector drift
// ---------------------------------------------------------------------------

describe('selector drift', () => {
  test('active tile without a name - getDiagnostic - set, one warning, no speaker', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { kind: GALLERY });
    addTile(doc, '', { kind: GALLERY, active: true });
    const adapter = new ZoomAdapter(makeWin(), doc);

    expect(adapter.getDiagnostic()).toMatch(/нет имени/);
    expect(adapter.getDiagnostic()).toMatch(/нет имени/);
    expect(adapter.getActiveSpeaker()).toBeNull();
    expect(console.warn).toHaveBeenCalledTimes(1);
  });

  test('entered meeting whose tiles are gone - getDiagnostic - set only after the window', () => {
    const doc = meetingDoc();
    const alice = addTile(doc, 'Alice');
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.getDiagnostic()).toBeNull();
    alice.remove();
    expect(adapter.getDiagnostic()).toBeNull();

    jest.advanceTimersByTime(NO_TILES_DRIFT_MS + 1);
    expect(adapter.getDiagnostic()).toMatch(/плитки участников не найдены/);
  });

  test('two named tiles and no active tile - getDiagnostic - set only after the window, cleared by a speaker', () => {
    const doc = meetingDoc();
    addTile(doc, 'Alice', { kind: GALLERY });
    const bob = addTile(doc, 'Bob', { kind: GALLERY });
    const adapter = new ZoomAdapter(makeWin(), doc);
    expect(adapter.getDiagnostic()).toBeNull();

    jest.advanceTimersByTime(NO_ACTIVE_DRIFT_MS + 1);
    expect(adapter.getDiagnostic()).toMatch(/говорящий не отмечен/);

    setActive(bob, true);
    expect(adapter.getDiagnostic()).toBeNull();
  });

  test('alone in the meeting with no active tile - getDiagnostic - null however long', () => {
    const doc = meetingDoc();
    addTile(doc, 'Me', { kind: GALLERY });
    const adapter = new ZoomAdapter(makeWin(), doc);
    adapter.getDiagnostic();

    jest.advanceTimersByTime(NO_ACTIVE_DRIFT_MS + 1);
    expect(adapter.getDiagnostic()).toBeNull();
  });

  test('prejoin with no tiles - getDiagnostic - never reports drift', () => {
    const adapter = new ZoomAdapter(makeWin(), newDoc());
    adapter.getDiagnostic();

    jest.advanceTimersByTime(NO_TILES_DRIFT_MS + NO_ACTIVE_DRIFT_MS);
    expect(adapter.getDiagnostic()).toBeNull();
  });

  test('time-based drift while subscribed - window elapses - subscribers are notified', async () => {
    const doc = meetingDoc();
    const alice = addTile(doc, 'Alice');
    const adapter = new ZoomAdapter(makeWin(), doc);
    adapter.isInMeeting();
    alice.remove();
    const cb = jest.fn();
    adapter.subscribeToSpeakerChanges(cb);

    jest.advanceTimersByTime(NO_TILES_DRIFT_MS + 1);
    adapter.getDiagnostic();
    await flush();

    expect(cb).toHaveBeenCalledTimes(1);
  });
});
