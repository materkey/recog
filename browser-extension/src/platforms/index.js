/**
 * Platform detection and adapter factory
 *
 * Supports:
 * - KTalk (ktalk.ru) - uses custom $conf wrapper with RxJS observables
 * - vas3k.cloud - uses standard Jitsi Meet APP object
 * - Configured Jitsi hosts - uses standard Jitsi Meet APP object
 * - Configured Element Call / Matrix hosts - bundled EC iframe at
 *   /widgets/element-call/*; sources data from matrix-js-sdk + livekit
 */

import { KTalkAdapter } from './ktalk.js';
import { Vas3kAdapter } from './vas3k.js';
import { ElementCallAdapter } from './matrix.js';

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
export function getPlatformDisplayName() {
  const platform = getPlatform();
  switch (platform) {
    case 'ktalk':
      return 'KTalk';
    case 'vas3k':
      return 'Вастрик Мит';
    case 'jitsi':
      return 'Jitsi Meet';
    case 'matrix':
      return 'Element Call';
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
 * matrix (Element Call, whose livekit conference mounts late) MUST be included
 * or the retry gives up before the conference appears.
 */
export function isJitsiPlatform() {
  const platform = getPlatform();
  return platform === 'vas3k' || platform === 'jitsi' || platform === 'matrix';
}
