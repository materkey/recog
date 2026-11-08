import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
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

// The installation config has one parser, recog/install_config.py, and its Rust
// port in the recog binary; the build asks it for the configured hosts instead
// of validating the file again. RECOG_PYTHON names an interpreter; otherwise an
// installation without Python ([install] runtime = "rust") answers from bin/recog,
// any other from the venv.
export function loadConferenceHosts(env = process.env, root = projectRoot) {
  const run = (program, args) => execFileSync(program, args, {
    cwd: root, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'],
  });
  const recog = path.join(root, 'bin/recog');
  if (!env.RECOG_PYTHON && isExecutable(recog)) {
    return JSON.parse(run(recog, ['config', 'conferences']));
  }
  const venvPython = path.join(root, 'venv-gigaam/bin/python');
  const python = env.RECOG_PYTHON || (fs.existsSync(venvPython) ? venvPython : 'python3');
  return JSON.parse(run(python, ['-m', 'recog.install_config']));
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
