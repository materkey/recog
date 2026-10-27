import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const projectRoot = fileURLToPath(new URL('../../', import.meta.url));

// The installation config has one parser, recog/install_config.py; the build
// asks it for the configured hosts instead of validating the file again.
export function loadConferenceHosts(env = process.env) {
  const venvPython = path.join(projectRoot, 'venv-gigaam/bin/python');
  const python = env.RECOG_PYTHON || (fs.existsSync(venvPython) ? venvPython : 'python3');
  const output = execFileSync(python, ['-m', 'recog.install_config'], {
    cwd: projectRoot, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'],
  });
  return JSON.parse(output);
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
