/**
 * Platform detection and adapter factory
 *
 * Supports:
 * - KTalk (ktalk.ru) - uses custom $conf wrapper with RxJS observables
 * - vas3k.cloud - uses standard Jitsi Meet APP object
 * - vkkovalev.work - uses standard Jitsi Meet APP object
 * - Element Call / Matrix (element.vkkovalev.work) - bundled EC iframe at
 *   /widgets/element-call/*; sources data from matrix-js-sdk + livekit
 */

import { KTalkAdapter } from './ktalk.js';
import { Vas3kAdapter } from './vas3k.js';

let currentPlatform = null;
let currentAdapter = null;

/**
 * Detect which platform we're on based on hostname (and pathname for matrix).
 *
 * `loc` defaults to `window.location` but is injectable so tests can exercise
 * multiple URLs in one file (jsdom forbids mutating `location` after setup).
 */
export function detectPlatform(loc = window.location) {
  const hostname = loc.hostname;
  const pathname = loc.pathname || '';

  if (hostname.includes('ktalk.ru')) {
    return 'ktalk';
  }
  if (hostname.includes('vas3k.cloud')) {
    return 'vas3k';
  }
  // Element Call is bundled inside Element Web as a same-origin iframe served
  // from /widgets/element-call/*. This check MUST come before the generic
  // vkkovalev.work host check below, which would otherwise misroute
  // element.* to the Jitsi (vkkovalev) adapter. Only the EC iframe path gets
  // an adapter — the Element top frame (path '/') returns null so the
  // extension does not open a second stray WS there.
  if (hostname.includes('element.vkkovalev.work')) {
    if (pathname.startsWith('/widgets/element-call/')) {
      return 'matrix';
    }
    return null;
  }
  if (hostname.includes('vkkovalev.work')) {
    return 'vkkovalev';
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
      case 'vkkovalev':
        currentAdapter = new Vas3kAdapter();
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
    case 'vkkovalev':
      return 'VK Meet';
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
  return platform === 'vas3k' || platform === 'vkkovalev' || platform === 'matrix';
}
