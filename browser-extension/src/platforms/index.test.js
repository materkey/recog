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
 * @jest-environment-options {"url": "https://element.example.org/widgets/element-call/index.html"}
 */

const hosts = { jitsi: ['https://meet.example.org'], matrix: ['https://element.example.org'] };
globalThis.__RECOG_CONFERENCE_HOSTS__ = hosts;
const { detectPlatform, getPlatform, getPlatformDisplayName, isJitsiPlatform, resetAdapter } = await import('./index.js');
afterAll(() => { delete globalThis.__RECOG_CONFERENCE_HOSTS__; });

const loc = (hostname, pathname = '/') => new URL(`https://${hostname}${pathname}`);

describe('detectPlatform', () => {
  test('EC iframe location on element.example.org resolves to matrix', () => {
    expect(detectPlatform(loc('element.example.org', '/widgets/element-call/index.html'))).toBe('matrix');
  });

  test('deep EC iframe subpaths still resolve to matrix', () => {
    expect(detectPlatform(loc('element.example.org', '/widgets/element-call/index.html#/room/!abc'))).toBe('matrix');
  });

  test('Element top frame (path "/") gets NO adapter', () => {
    expect(detectPlatform(loc('element.example.org', '/'))).toBeNull();
  });

  test('Element top frame on an arbitrary non-EC path gets NO adapter', () => {
    expect(detectPlatform(loc('element.example.org', '/#/room/!abc'))).toBeNull();
  });

  test('configured Jitsi host - detecting platform - resolves to jitsi', () => {
    expect(detectPlatform(loc('meet.example.org', '/somemeeting'))).toBe('jitsi');
  });

  test('ktalk detection unchanged', () => {
    expect(detectPlatform(loc('meet.ktalk.ru', '/room'))).toBe('ktalk');
  });

  test('vas3k detection unchanged', () => {
    expect(detectPlatform(loc('meet.vas3k.cloud', '/room'))).toBe('vas3k');
  });

  test.each(['https://element.example.org.evil.test/widgets/element-call/index.html',
    'https://evilktalk.ru/', 'http://meet.example.org/', 'https://meet.example.org:8443/',
    'https://sub.meet.example.org/'])('unapproved URL %s - detecting platform - rejects it', url => {
    expect(detectPlatform(new URL(url), hosts)).toBeNull();
  });

  test('empty config - detecting custom host - rejects it', () => {
    expect(detectPlatform(loc('meet.example.org'), { jitsi: [], matrix: [] })).toBeNull();
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
