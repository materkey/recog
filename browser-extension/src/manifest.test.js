/**
 * Manifest scoping tests.
 *
 * The Element Call content script must match ONLY the bundled EC iframe path
 * (/widgets/element-call/*) and NEVER the Element top frame (path "/"), so the
 * extension does not open a second stray WS in the top frame (which would break
 * last-extension-gone meeting teardown).
 */

import { readFileSync } from 'node:fs';

import { createManifest } from '../scripts/conference-hosts.js';

const template = JSON.parse(
  readFileSync(new URL('../manifest.json', import.meta.url), 'utf8'),
);

const manifest = createManifest(template, { jitsi: ['https://meet.example.org'], matrix: ['https://element.example.org'] });

const EC_PATTERN = 'https://element.example.org/widgets/element-call/*';
const EC_IFRAME_URL = 'https://element.example.org/widgets/element-call/index.html';
const EC_IFRAME_URL_HASH = 'https://element.example.org/widgets/element-call/index.html#/room/!abc';
const ELEMENT_TOP_FRAME_URL = 'https://element.example.org/';
const ELEMENT_TOP_FRAME_ROOM_URL = 'https://element.example.org/#/room/!abc';

/**
 * Minimal Chrome match-pattern → boolean matcher.
 * <scheme>://<host><path>, where `*` in host is a subdomain wildcard and `*`
 * in the path matches any sequence of characters. The `#fragment` never
 * participates in match-pattern matching (Chrome ignores it), so we strip it.
 */
function matchPatternMatches(pattern, url) {
  const pm = /^(\*|https?|file|ftp):\/\/([^/]*)(\/.*)$/.exec(pattern);
  if (!pm) throw new Error(`bad match pattern: ${pattern}`);
  const [, scheme, host, path] = pm;

  const um = /^([a-z]+):\/\/([^/]+)([^#]*)/.exec(url);
  if (!um) throw new Error(`bad url: ${url}`);
  const [, uScheme, uHost, uPath] = um;

  // scheme
  if (scheme !== '*' && scheme !== uScheme) return false;

  // host
  if (host === '*') {
    // any host
  } else if (host.startsWith('*.')) {
    const base = host.slice(2);
    if (uHost !== base && !uHost.endsWith('.' + base)) return false;
  } else if (host !== uHost) {
    return false;
  }

  // path (glob: only `*` is special)
  const pathRe = new RegExp(
    '^' + path.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*') + '$',
  );
  return pathRe.test(uPath || '/');
}

describe('matchPatternMatches helper (sanity)', () => {
  test('EC pattern matches the EC iframe path but not the top frame', () => {
    expect(matchPatternMatches(EC_PATTERN, EC_IFRAME_URL)).toBe(true);
    expect(matchPatternMatches(EC_PATTERN, ELEMENT_TOP_FRAME_URL)).toBe(false);
  });
});

describe('manifest Element Call scoping', () => {
  const contentScripts = manifest.content_scripts || [];

  const ecEntry = contentScripts.find((cs) => (cs.matches || []).includes(EC_PATTERN));

  test('an EC content-script entry exists and runs in all frames', () => {
    expect(ecEntry).toBeDefined();
    expect(ecEntry.all_frames).toBe(true);
    expect(ecEntry.js).toContain('content.js');
  });

  test('EC content-script match covers the EC iframe path (with and without fragment)', () => {
    expect(matchPatternMatches(EC_PATTERN, EC_IFRAME_URL)).toBe(true);
    expect(matchPatternMatches(EC_PATTERN, EC_IFRAME_URL_HASH)).toBe(true);
  });

  test('NO content-script match covers the Element top frame', () => {
    for (const cs of contentScripts) {
      for (const pattern of cs.matches || []) {
        expect(matchPatternMatches(pattern, ELEMENT_TOP_FRAME_URL)).toBe(false);
        expect(matchPatternMatches(pattern, ELEMENT_TOP_FRAME_ROOM_URL)).toBe(false);
      }
    }
  });

  test('host_permissions are scoped to the EC iframe path', () => {
    expect(manifest.host_permissions).toContain(EC_PATTERN);
  });

  test('web_accessible_resources use the origin-wide Element pattern', () => {
    // Chrome rejects path-scoped match patterns in web_accessible_resources[].matches
    // ("Invalid match pattern" — only "<origin>/*" is allowed there), so WAR cannot
    // mirror the /widgets/element-call/* scoping. That is fine: WAR only controls which
    // origins may fetch injected.js; top-frame injection is prevented by content_scripts
    // (asserted above), not by WAR.
    const war = manifest.web_accessible_resources[0];
    expect(war.matches).toContain('https://element.example.org/*');
    expect(war.matches).not.toContain(EC_PATTERN);
  });
});


describe('manifest configuration boundaries', () => {
  test('empty config - building manifest - retains built-ins only', () => {
    expect(createManifest(template, { jitsi: [], matrix: [] })).toEqual(template);
    expect(template.host_permissions).toEqual([
      'https://*.ktalk.ru/*', 'https://meet.vas3k.cloud/*', 'https://meet.google.com/*',
    ]);
  });

  test('built-in manifest - matching Google Meet URLs - covers rooms and landing, not lookalikes', () => {
    const patterns = template.content_scripts.flatMap(entry => entry.matches);
    const covered = url => patterns.some(pattern => matchPatternMatches(pattern, url));
    expect(covered('https://meet.google.com/qrs-tuvw-xyz')).toBe(true);
    expect(covered('https://meet.google.com/landing')).toBe(true);
    expect(covered('https://meet.google.com.evil.test/qrs-tuvw-xyz')).toBe(false);
    expect(template.web_accessible_resources[0].matches).toContain('https://meet.google.com/*');
  });

  test('custom Jitsi config - building manifest - allows only the exact host', () => {
    const patterns = manifest.content_scripts.flatMap(entry => entry.matches);
    expect(patterns.some(pattern => matchPatternMatches(pattern, 'https://meet.example.org/room'))).toBe(true);
    expect(patterns.some(pattern => matchPatternMatches(pattern, 'https://other.example.org/room'))).toBe(false);
    expect(patterns.some(pattern => matchPatternMatches(pattern, 'https://meet.example.org.evil.test/room'))).toBe(false);
    expect(patterns).not.toContain('<all_urls>');
  });
});
