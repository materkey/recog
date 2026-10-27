import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadConferenceHosts } from '../scripts/conference-hosts.js';

const hosts = { jitsi: ['https://meet.example.org'], matrix: ['https://element.example.org'] };
let directory;
beforeEach(() => { directory = fs.mkdtempSync(path.join(os.tmpdir(), 'recog-hosts-')); });
afterEach(() => { fs.rmSync(directory, { recursive: true }); });

function environment(config) {
  const file = path.join(directory, 'config.toml');
  fs.writeFileSync(file, config);
  return { ...process.env, RECOG_CONFIG_FILE: file,
    RECOG_CONFERENCE_HOSTS_FILE: path.join(directory, 'absent.json') };
}

test('configured hosts - loading for the build - returns them from the installation config', () => {
  const env = environment([
    '[conferences]',
    'jitsi = ["https://meet.example.org"]',
    'matrix = ["https://element.example.org"]',
    '[sources.aliases]',
    'legacy-jitsi = "jitsi"',
  ].join('\n'));
  expect(loadConferenceHosts(env)).toEqual(hosts);
});

test('no installation config - loading for the build - uses built-ins only', () => {
  const env = { ...process.env, RECOG_CONFIG_FILE: path.join(directory, 'absent.toml'),
    RECOG_CONFERENCE_HOSTS_FILE: path.join(directory, 'absent.json') };
  expect(loadConferenceHosts(env)).toEqual({ jitsi: [], matrix: [] });
});

test('invalid installation config - loading for the build - fails', () => {
  const env = environment('[conferences]\njitsi = ["https://*.example.org"]\n');
  expect(() => loadConferenceHosts(env)).toThrow();
});
