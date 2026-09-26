/**
 * Platform detection and adapter factory
 *
 * Supports:
 * - KTalk (ktalk.ru) - uses custom $conf wrapper with RxJS observables
 * - vas3k.cloud - uses standard Jitsi Meet APP object
 * - Configured Jitsi hosts - uses standard Jitsi Meet APP object
 * - Google Meet (meet.google.com) - DOM speaking indicators on participant tiles
 * - Configured Element Call / Matrix hosts - bundled EC iframe at
 *   /widgets/element-call/*; sources data from matrix-js-sdk + livekit
 */

import { KTalkAdapter } from './ktalk.js';
import { Vas3kAdapter } from './vas3k.js';
import { ElementCallAdapter } from './matrix.js';
import { GoogleMeetAdapter } from './gmeet.js';

const conferenceHosts = typeof __RECOG_CONFERENCE_HOSTS__ === 'undefined'
  ? { jitsi: [], matrix: [] }
  : __RECOG_CONFERENCE_HOSTS__;

let currentPlatform = null;
let currentAdapter = null;

/**
 * Detect which platform we're on based on hostname (and pathname for matrix).
 *
 * `loc` defaults to `window.location` but is injectable so tests can exercise
 * multiple URLs in one file (jsdom forbids mutating `location` after setup).
 */
export function detectPlatform(loc = window.location, hosts = conferenceHosts) {
  const { hostname, pathname = '', protocol, port, origin } = loc;
  if (protocol !== 'https:' || (port && port !== '443')) return null;

  if (hostname === 'ktalk.ru' || hostname.endsWith('.ktalk.ru')) {
    return 'ktalk';
  }
  if (hostname === 'meet.vas3k.cloud') {
    return 'vas3k';
  }
  // Host-wide: Meet navigates from /landing into a room without a reload,
  // so the adapter itself decides whether the current path is a room.
  if (hostname === 'meet.google.com') {
    return 'gmeet';
  }
  // Only the bundled call iframe gets an adapter; the Element top frame
  // must not open a second WebSocket that outlives the call.
  if (hosts.matrix.includes(origin)) {
    return pathname.startsWith('/widgets/element-call/') ? 'matrix' : null;
  }
  if (hosts.jitsi.includes(origin)) {
    return 'jitsi';
  }

  return null;
}

/**
 * Get the current platform name
 */
export function getPlatform() {
  if (!currentPlatform) {
    currentPlatform = detectPlatform();
  }
  return currentPlatform;
}

/**
 * Get the platform adapter instance
 * Creates adapter on first call, returns cached instance after
 */
export function getPlatformAdapter() {
  if (!currentAdapter) {
    const platform = getPlatform();

    switch (platform) {
      case 'ktalk':
        currentAdapter = new KTalkAdapter();
        break;
      case 'vas3k':
      case 'jitsi':
        currentAdapter = new Vas3kAdapter();
        break;
      case 'matrix':
        currentAdapter = new ElementCallAdapter();
        break;
      case 'gmeet':
        currentAdapter = new GoogleMeetAdapter();
        break;
      default:
        console.warn('[Platform] Unknown platform:', platform);
        return null;
    }

    console.log('[Platform] Using adapter:', platform);
  }

  return currentAdapter;
}

/**
 * Reset the adapter (for testing or page navigation)
 */
export function resetAdapter() {
  currentPlatform = null;
  currentAdapter = null;
}

/**
 * Get platform display name for UI
 */
export function getPlatformDisplayName(platform = getPlatform()) {
  switch (platform) {
    case 'ktalk':
      return 'KTalk';
    case 'vas3k':
      return 'Вастрик Мит';
    case 'jitsi':
      return 'Jitsi Meet';
    case 'matrix':
      return 'Element Call';
    case 'gmeet':
      return 'Google Meet';
    default:
      return 'Unknown';
  }
}

/**
 * Check if platform uses the floating-panel UI + conference retry-forever loop
 * (everything except KTalk).
 *
 * Despite the historical name, this predicate is NOT just a Jitsi check: it
 * also gates the conference retry loop (`init.js`) and 8+ `ui.js` sites, so
 * every non-KTalk platform (e.g. Element Call, whose livekit conference mounts
 * late) must pass it or the retry gives up before the conference appears.
 * Defined as "any detected platform except KTalk" so a new platform needs no
 * edit here.
 */
export function isJitsiPlatform(platform = getPlatform()) {
  return !!platform && platform !== 'ktalk';
}
