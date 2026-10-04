import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadConferenceHosts, validateConferenceHosts } from '../scripts/conference-hosts.js';

const hosts = { jitsi: ['https://meet.example.org'], matrix: ['https://element.example.org'] };
let directory;
beforeEach(() => { directory = fs.mkdtempSync(path.join(os.tmpdir(), 'recog-hosts-')); });
afterEach(() => { fs.rmSync(directory, { recursive: true }); });

test('missing file - loading config - uses built-ins only', () => {
  expect(loadConferenceHosts(path.join(directory, 'absent.json'))).toEqual({ jitsi: [], matrix: [] });
});

test('custom file - loading config - returns matching runtime origins', () => {
  const file = path.join(directory, 'hosts.json');
  fs.writeFileSync(file, JSON.stringify({ ...hosts, source_aliases: { 'legacy-jitsi': 'jitsi' } }));
  expect(loadConferenceHosts(file)).toEqual(hosts);
});

test('malformed configured file - loading config - fails', () => {
  const file = path.join(directory, 'hosts.json');
  fs.writeFileSync(file, '{');
  expect(() => loadConferenceHosts(file)).toThrow();
});

test.each(['http://meet.example.org', 'https://meet.example.org/', 'https://meet.example.org/path',
  'https://meet.example.org?x=1', 'https://meet.example.org#room', 'https://u:p@meet.example.org',
  'https://*.example.org', 'https://meet.example.org:8443', 'https://meet.example.org:443',
  'https://_meet.example.org', 'https://-meet.example.org', 'https://meet-.example.org',
  'https://meet..example.org', 'https://meet.example.org.', 'https://MEET.example.org',
  'https://[::1]', 'https://mеet.example.org',
  `https://${'a'.repeat(64)}.example.org`, `https://${'a'.repeat(63)}.${'b'.repeat(63)}.${'c'.repeat(63)}.${'d'.repeat(63)}`,
  'https://ktalk.ru', 'https://sub.ktalk.ru', 'https://meet.vas3k.cloud', 'https://meet.google.com',
  'https://zoom.us', 'https://app.zoom.us', 42, null])(
  'invalid origin %s - validating config - fails', origin => {
    expect(() => validateConferenceHosts({ jitsi: [origin] })).toThrow();
  });

test.each([null, [], { jitsi: null }, { jitsi: 'https://meet.example.org' }, { typo: [] },
  { jitsi: hosts.jitsi, matrix: hosts.jitsi }, { jitsi: [...hosts.jitsi, ...hosts.jitsi] },
  { source_aliases: { old: 'unknown' } }, { source_aliases: { old: 'ktalk' } },
  { source_aliases: { ktalk: 'jitsi' } }, { source_aliases: { vas3k: 'jitsi' } },
  { source_aliases: { jitsi: 'matrix' } }, { source_aliases: { matrix: 'jitsi' } },
  { source_aliases: { gmeet: 'jitsi' } }, { source_aliases: { zoom: 'jitsi' } }])('invalid config %j - validating config - fails', value => {
  expect(() => validateConferenceHosts(value)).toThrow();
});


test('repository-relative config - loading from extension cwd - resolves against repository root', () => {
  const file = path.join(directory, 'hosts.json');
  fs.writeFileSync(file, JSON.stringify(hosts));
  const root = new URL('../../', import.meta.url).pathname;
  expect(loadConferenceHosts(path.relative(root, file))).toEqual(hosts);
});

test('home-relative config - loading file - expands tilde', () => {
  const file = path.join(directory, 'hosts.json');
  fs.writeFileSync(file, JSON.stringify(hosts));
  expect(loadConferenceHosts(`~/${path.relative(os.homedir(), file)}`)).toEqual(hosts);
});

test('canonical ASCII hostname labels - validating config - accepts hyphens and punycode', () => {
  const configured = { jitsi: ['https://meet-1.xn--e1afmkfd.xn--p1ai'], matrix: [] };
  expect(validateConferenceHosts(configured)).toEqual(configured);
});
