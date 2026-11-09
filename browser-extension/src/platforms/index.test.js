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
const { detectPlatform, getPlatform, getPlatformAdapter, getPlatformDisplayName, isJitsiPlatform, resetAdapter } = await import('./index.js');
const { ElementCallAdapter } = await import('./matrix.js');
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

  test('Google Meet room path - detecting platform - resolves to gmeet', () => {
    expect(detectPlatform(loc('meet.google.com', '/qrs-tuvw-xyz'))).toBe('gmeet');
  });

  test('Google Meet landing page - detecting platform - still resolves to gmeet (host-wide)', () => {
    expect(detectPlatform(loc('meet.google.com', '/landing'))).toBe('gmeet');
  });

  test('Google Meet lookalike host - detecting platform - rejects it', () => {
    expect(detectPlatform(loc('meet.google.com.evil.test', '/qrs-tuvw-xyz'))).toBeNull();
  });

  test.each([
    ['app.zoom.us', '/wc/12345678901/join'],
    ['us05web.zoom.us', '/wc/join/12345678901'],
    ['zoom.us', '/wc/12345678901/start'],
    ['app.zoom.us', '/wc/home'],
  ])('Zoom web client path on %s%s - detecting platform - resolves to zoom', (host, path) => {
    expect(detectPlatform(loc(host, path))).toBe('zoom');
  });

  test.each([
    ['zoom.us', '/j/12345678901'],
    ['zoom.us', '/profile'],
    ['app.zoom.us', '/wc'],
  ])('Zoom page outside the web client %s%s - detecting platform - rejects it', (host, path) => {
    expect(detectPlatform(loc(host, path))).toBeNull();
  });

  test.each(['evilzoom.us', 'zoom.us.evil.test', 'app-zoom.us'])(
    'Zoom lookalike host %s - detecting platform - rejects it',
    (host) => {
      expect(detectPlatform(loc(host, '/wc/12345678901/join'))).toBeNull();
    },
  );

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

describe('isJitsiPlatform / display name per platform', () => {
  test.each([
    ['gmeet', true], ['zoom', true], ['matrix', true], ['vas3k', true], ['jitsi', true],
    ['ktalk', false], [null, false],
  ])('platform %s - checking floating-panel predicate - returns %s', (platform, expected) => {
    expect(isJitsiPlatform(platform)).toBe(expected);
  });

  test('gmeet platform - getting display name - returns Google Meet', () => {
    expect(getPlatformDisplayName('gmeet')).toBe('Google Meet');
  });

  test('zoom platform - getting display name - returns Zoom', () => {
    expect(getPlatformDisplayName('zoom')).toBe('Zoom');
  });
});

describe('detectPlatform with an explicit default port', () => {
  test('location with port 443 spelled out - detecting platform - accepts it', () => {
    const location = { hostname: 'meet.ktalk.ru', pathname: '/room', protocol: 'https:', port: '443', origin: 'https://meet.ktalk.ru' };

    expect(detectPlatform(location)).toBe('ktalk');
  });

  test('location without a path - detecting the Element iframe - gets no adapter', () => {
    const location = { hostname: 'element.example.org', protocol: 'https:', port: '', origin: 'https://element.example.org' };

    expect(detectPlatform(location, hosts)).toBeNull();
  });
});

describe('getPlatformAdapter', () => {
  beforeEach(() => resetAdapter());

  test('Element Call iframe page - getting the adapter twice - creates one ElementCallAdapter', () => {
    const adapter = getPlatformAdapter();

    expect(adapter).toBeInstanceOf(ElementCallAdapter);
    expect(getPlatformAdapter()).toBe(adapter);
  });

  test('a created adapter - reset - the next call creates a fresh one', () => {
    const first = getPlatformAdapter();

    resetAdapter();

    expect(getPlatformAdapter()).not.toBe(first);
  });
});
