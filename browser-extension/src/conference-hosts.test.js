import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { loadConferenceHosts } from '../scripts/conference-hosts.js';

let directory;
beforeEach(() => { directory = fs.mkdtempSync(path.join(os.tmpdir(), 'recog-hosts-')); });
afterEach(() => { fs.rmSync(directory, { recursive: true }); });

function checkout() {
  const root = path.join(directory, 'checkout');
  fs.mkdirSync(root, { recursive: true });
  return root;
}

function installedRecog(script) {
  const root = checkout();
  fs.mkdirSync(path.join(root, 'bin'), { recursive: true });
  const recog = path.join(root, 'bin/recog');
  fs.writeFileSync(recog, `#!/bin/sh\n${script}\n`);
  fs.chmodSync(recog, 0o755);
  return root;
}

function environment(files) {
  return { ...process.env, HOME: directory, XDG_CONFIG_HOME: '',
    RECOG_CONFIG_FILE: path.join(directory, files.config || 'absent.toml'),
    RECOG_CONFERENCE_HOSTS_FILE: path.join(directory, files.legacy || 'absent.json') };
}

test('an installed recog - loading for the build - asks bin/recog', () => {
  const root = installedRecog([
    'printf "%s" "$*" > "$(dirname "$0")/args"',
    'echo \'{"jitsi": ["https://meet.example.org"], "matrix": []}\'',
  ].join('\n'));
  expect(loadConferenceHosts(environment({}), root)).toEqual({ jitsi: ['https://meet.example.org'], matrix: [] });
  expect(fs.readFileSync(path.join(root, 'bin/args'), 'utf8')).toBe('config conferences');
});

test('an installed recog and an invalid config - loading for the build - fails', () => {
  const root = installedRecog('echo "[conferences] must be a table" >&2; exit 1');
  expect(() => loadConferenceHosts(environment({}), root)).toThrow();
});

test('no recog and no installation config - loading for the build - uses built-ins only', () => {
  expect(loadConferenceHosts(environment({}), checkout())).toEqual({ jitsi: [], matrix: [] });
});

test('no recog but an installation config - loading for the build - fails naming setup', () => {
  fs.writeFileSync(path.join(directory, 'config.toml'), '[conferences]\n');
  expect(() => loadConferenceHosts(environment({ config: 'config.toml' }), checkout())).toThrow(/setup\.sh/);
});

test('no recog but a legacy hosts file - loading for the build - fails naming setup', () => {
  fs.writeFileSync(path.join(directory, 'hosts.json'), '{}');
  expect(() => loadConferenceHosts(environment({ legacy: 'hosts.json' }), checkout())).toThrow(/setup\.sh/);
});
