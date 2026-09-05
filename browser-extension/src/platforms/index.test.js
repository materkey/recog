/**
 * Tests for platform detection (detectPlatform) and the Jitsi/floating-panel
 * predicate (isJitsiPlatform), focused on the new Element Call (matrix) case.
 *
 * The jsdom url is pinned to the EC iframe path so that the cached
 * getPlatform()/isJitsiPlatform() path resolves to 'matrix'. detectPlatform()
 * itself takes an injectable `loc`, so the per-host cases pass explicit
 * location-like objects (jsdom forbids mutating window.location after setup).
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://element.vkkovalev.work/widgets/element-call/index.html"}
 */

import { detectPlatform, getPlatform, getPlatformDisplayName, isJitsiPlatform, resetAdapter } from './index.js';

const loc = (hostname, pathname = '/') => ({ hostname, pathname });

describe('detectPlatform', () => {
  test('EC iframe location on element.vkkovalev.work resolves to matrix', () => {
    expect(detectPlatform(loc('element.vkkovalev.work', '/widgets/element-call/index.html'))).toBe('matrix');
  });

  test('deep EC iframe subpaths still resolve to matrix', () => {
    expect(detectPlatform(loc('element.vkkovalev.work', '/widgets/element-call/index.html#/room/!abc'))).toBe('matrix');
  });

  test('Element top frame (path "/") gets NO adapter', () => {
    expect(detectPlatform(loc('element.vkkovalev.work', '/'))).toBeNull();
  });

  test('Element top frame on an arbitrary non-EC path gets NO adapter', () => {
    expect(detectPlatform(loc('element.vkkovalev.work', '/#/room/!abc'))).toBeNull();
  });

  test('meet.vkkovalev.work still resolves to vkkovalev (not misrouted to matrix)', () => {
    expect(detectPlatform(loc('meet.vkkovalev.work', '/somemeeting'))).toBe('vkkovalev');
  });

  test('ktalk detection unchanged', () => {
    expect(detectPlatform(loc('meet.ktalk.ru', '/room'))).toBe('ktalk');
  });

  test('vas3k detection unchanged', () => {
    expect(detectPlatform(loc('meet.vas3k.cloud', '/room'))).toBe('vas3k');
  });

  test('unknown host resolves to null', () => {
    expect(detectPlatform(loc('example.com', '/'))).toBeNull();
  });
});

describe('isJitsiPlatform / display name for matrix', () => {
  beforeEach(() => resetAdapter());

  test('matrix (EC iframe url) is treated as a Jitsi/floating-panel platform', () => {
    // getPlatform() reads window.location, pinned to the EC iframe url above.
    expect(getPlatform()).toBe('matrix');
    expect(isJitsiPlatform()).toBe(true);
  });

  test('matrix has its own display name', () => {
    expect(getPlatformDisplayName()).toBe('Element Call');
  });
});
