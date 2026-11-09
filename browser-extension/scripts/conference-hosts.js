import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const projectRoot = fileURLToPath(new URL('../../', import.meta.url));

function isExecutable(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

function expandHome(file, env) {
  const home = env.HOME || os.homedir();
  if (file === '~') {
    return home;
  }
  return file.startsWith('~/') ? path.join(home, file.slice(2)) : file;
}

// Where the recog binary would look: RECOG_CONFIG_FILE (relative to the
// checkout), else $XDG_CONFIG_HOME/recog/config.toml, and the legacy hosts file.
function configFiles(env, root) {
  const inRoot = file => (path.isAbsolute(file) ? file : path.join(root, file));
  const config = env.RECOG_CONFIG_FILE
    ? inRoot(expandHome(env.RECOG_CONFIG_FILE, env))
    : path.join(expandHome(env.XDG_CONFIG_HOME || '~/.config', env), 'recog', 'config.toml');
  const legacy = inRoot(expandHome(env.RECOG_CONFERENCE_HOSTS_FILE || '.local/conference-hosts.json', env));
  return [config, legacy];
}

// The installation config has one parser, the recog binary's; the build asks
// bin/recog for the configured hosts instead of validating the file again. A
// checkout setup.sh has not built (CI) has no bin/recog: with no configuration
// file either, the built-in hosts are all there is.
export function loadConferenceHosts(env = process.env, root = projectRoot) {
  const recog = path.join(root, 'bin/recog');
  if (isExecutable(recog)) {
    const output = execFileSync(recog, ['config', 'conferences'], {
      cwd: root, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'],
    });
    return JSON.parse(output);
  }
  const present = configFiles(env, root).filter(file => fs.existsSync(file));
  if (present.length) {
    throw new Error(`${present[0]} needs ${recog} to be read: run ./setup.sh first`);
  }
  return { jitsi: [], matrix: [] };
}

export function createManifest(template, hosts) {
  const manifest = JSON.parse(JSON.stringify(template));
  const jitsiPatterns = hosts.jitsi.map(origin => `${origin}/*`);
  const matrixPatterns = hosts.matrix.map(origin => `${origin}/widgets/element-call/*`);
  manifest.host_permissions.push(...jitsiPatterns, ...matrixPatterns);
  manifest.content_scripts[0].matches.push(...jitsiPatterns);
  if (matrixPatterns.length) {
    manifest.content_scripts.push({
      matches: matrixPatterns,
      js: ['content.js'],
      all_frames: true,
      run_at: 'document_idle',
    });
  }
  manifest.web_accessible_resources[0].matches.push(
    ...jitsiPatterns, ...hosts.matrix.map(origin => `${origin}/*`),
  );
  return manifest;
}
