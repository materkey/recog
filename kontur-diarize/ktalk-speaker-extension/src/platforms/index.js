/**
 * Platform detection and adapter factory
 *
 * Supports:
 * - KTalk (ktalk.ru) - uses custom $conf wrapper with RxJS observables
 * - vas3k.cloud - uses standard Jitsi Meet APP object
 * - vkkovalev.work - uses standard Jitsi Meet APP object
 */

import { KTalkAdapter } from './ktalk.js';
import { Vas3kAdapter } from './vas3k.js';

let currentPlatform = null;
let currentAdapter = null;

/**
 * Detect which platform we're on based on hostname
 */
export function detectPlatform() {
  const hostname = window.location.hostname;

  if (hostname.includes('ktalk.ru')) {
    return 'ktalk';
  }
  if (hostname.includes('vas3k.cloud')) {
    return 'vas3k';
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
    default:
      return 'Unknown';
  }
}

/**
 * Check if platform uses standard Jitsi Meet (not KTalk)
 */
export function isJitsiPlatform() {
  const platform = getPlatform();
  return platform === 'vas3k' || platform === 'vkkovalev';
}
