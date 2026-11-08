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

function installedRecog(script) {
  const root = path.join(directory, 'checkout');
  fs.mkdirSync(path.join(root, 'bin'), { recursive: true });
  const recog = path.join(root, 'bin/recog');
  fs.writeFileSync(recog, `#!/bin/sh\n${script}\n`);
  fs.chmodSync(recog, 0o755);
  return root;
}

test('installation without Python - loading for the build - asks bin/recog', () => {
  const root = installedRecog([
    'printf "%s" "$*" > "$(dirname "$0")/args"',
    'echo \'{"jitsi": ["https://meet.example.org"], "matrix": []}\'',
  ].join('\n'));
  const env = { ...process.env, RECOG_PYTHON: '' };
  expect(loadConferenceHosts(env, root)).toEqual({ jitsi: ['https://meet.example.org'], matrix: [] });
  expect(fs.readFileSync(path.join(root, 'bin/args'), 'utf8')).toBe('config conferences');
});

test('installation without Python and an invalid config - loading for the build - fails', () => {
  const root = installedRecog('echo "[conferences] must be a table" >&2; exit 1');
  expect(() => loadConferenceHosts({ ...process.env, RECOG_PYTHON: '' }, root)).toThrow();
});

test('an interpreter named by RECOG_PYTHON - loading for the build - wins over bin/recog', () => {
  const root = installedRecog('echo \'{"jitsi": ["https://wrong.example.org"], "matrix": []}\'');
  const python = path.join(directory, 'python');
  fs.writeFileSync(python, '#!/bin/sh\necho \'{"jitsi": [], "matrix": ["https://element.example.org"]}\'\n');
  fs.chmodSync(python, 0o755);
  expect(loadConferenceHosts({ ...process.env, RECOG_PYTHON: python }, root))
    .toEqual({ jitsi: [], matrix: ['https://element.example.org'] });
});

test('invalid installation config - loading for the build - fails', () => {
  const env = environment('[conferences]\njitsi = ["https://*.example.org"]\n');
  expect(() => loadConferenceHosts(env)).toThrow();
});
