import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

const projectRoot = fileURLToPath(new URL('../../', import.meta.url));

export function validateConferenceHosts(value) {
  if (!value || Array.isArray(value) || typeof value !== 'object' ||
      Object.keys(value).some(key => !['jitsi', 'matrix', 'source_aliases'].includes(key))) {
    throw new Error('Conference hosts must be an object with jitsi and matrix arrays and optional source_aliases');
  }
  if (value.source_aliases !== undefined && (!value.source_aliases ||
      typeof value.source_aliases !== 'object' || Array.isArray(value.source_aliases) ||
      Object.entries(value.source_aliases).some(([source, target]) =>
        !source || ['ktalk', 'vas3k', 'jitsi', 'matrix'].includes(source) ||
        !['jitsi', 'matrix'].includes(target)))) {
    throw new Error('source_aliases must map source names to jitsi or matrix');
  }
  const hosts = { jitsi: [], matrix: [] };
  const seen = new Set();
  for (const platform of Object.keys(hosts)) {
    const origins = value[platform] === undefined ? [] : value[platform];
    if (!Array.isArray(origins)) throw new Error(`${platform} must be an array of HTTPS origins`);
    for (const origin of origins) {
      let url;
      try { url = new URL(origin); } catch { /* validated below */ }
      if (typeof origin !== 'string' || !url || url.protocol !== 'https:' ||
          url.origin !== origin || url.port || url.hostname.length > 253 ||
          url.hostname.split('.').some(label => !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label)) ||
          url.username || url.password || url.search || url.hash || url.pathname !== '/') {
        throw new Error(`Invalid ${platform} origin: expected https://host with no path, credentials or port`);
      }
      if (url.hostname === 'ktalk.ru' || url.hostname.endsWith('.ktalk.ru') ||
          url.hostname === 'meet.vas3k.cloud') {
        throw new Error('Built-in conference hosts must not be configured again');
      }
      if (seen.has(origin)) throw new Error('Conference origins must be unique across platforms');
      seen.add(origin);
      hosts[platform].push(origin);
    }
  }
  return hosts;
}

export function loadConferenceHosts(configFile = process.env.RECOG_CONFERENCE_HOSTS_FILE || '.local/conference-hosts.json') {
  const expandedPath = configFile === '~' ? os.homedir()
    : configFile.startsWith('~/') ? path.join(os.homedir(), configFile.slice(2)) : configFile;
  const filename = path.resolve(projectRoot, expandedPath);
  let contents;
  try { contents = fs.readFileSync(filename, 'utf8'); } catch (error) {
    if (error.code === 'ENOENT') return { jitsi: [], matrix: [] };
    throw error;
  }
  return validateConferenceHosts(JSON.parse(contents));
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
