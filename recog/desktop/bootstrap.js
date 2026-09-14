// Executed only through the short-lived main-process inspector connection.
async function attachDesktop(directory, digest, statusPath, target) {
  const requireModule = typeof require === 'function' ? require
    : process.mainModule?.require.bind(process.mainModule)
      || process.getBuiltinModule('module').createRequire(process.execPath);
  const electron = requireModule('electron');
  const fs = requireModule('node:fs');
  const key = '__recogDesktopIntegration';
  if (globalThis[key]) {
    if (globalThis[key].state !== 'ready') throw new Error('Previous integration initialization did not finish');
    return {app_version: electron.app.getVersion(), bundle: globalThis[key].bundle};
  }
  const record = globalThis[key] = {state: 'initializing', bundle: digest};
  function reportFailure() {
    record.lastError = 'page_initialization_failed';
    try {
      const temporary = statusPath + '.' + process.pid + '.tmp';
      fs.writeFileSync(temporary, JSON.stringify({state: 'incompatible',
        reason: record.lastError, target, app_version: electron.app.getVersion(),
        updated_at: Date.now() / 1000}), {mode: 0o600});
      fs.renameSync(temporary, statusPath);
    } catch { /* observer still retains the initial attachment result */ }
  }
  try {
    await electron.app.whenReady();
    const code = fs.readFileSync(directory + '/injected.js', 'utf8');
    const extensions = new Map();
    async function load(session) {
      if (!session.isPersistent()) throw new Error('Non-persistent Electron session');
      if (!extensions.has(session)) {
        const api = session.extensions || session;
        if (typeof api.loadExtension !== 'function') throw new Error('Electron extension API unavailable');
        extensions.set(session, api.loadExtension(directory));
      }
      return extensions.get(session);
    }
    const eligible = (url) => {
      try {
        const u = new URL(url);
        return u.protocol === 'https:' && (!u.port || u.port === '443')
          && (u.hostname === 'ktalk.ru' || u.hostname.endsWith('.ktalk.ru'));
      } catch { return false; }
    };
    async function inject(contents) {
      if (contents.isDestroyed() || !eligible(contents.getURL())) return;
      const extension = await load(contents.session);
      if (contents.isDestroyed()) return;
      const url = 'chrome-extension://' + extension.id + '/src/audio-worklet/downsampler.js';
      const result = await contents.executeJavaScript(`(() => {
        const u = new URL(location.href);
        if (u.protocol !== 'https:' || (u.port && u.port !== '443') ||
            !(u.hostname === 'ktalk.ru' || u.hostname.endsWith('.ktalk.ru'))) return 'skipped';
        window.__PCM_WORKLET_URL__ = ${JSON.stringify(url)};
        ${code}\n
        return window.__recogDesktopRuntime?.state;
      })()`, true);
      if (result === 'failed' || !result) throw new Error('Recog page initialization failed');
    }
    // Native content scripts cover new documents. This also handles a new
    // persistent session and the narrow race while loadExtension is pending.
    const watch = (_event, contents) => {
      contents.on('dom-ready', () => {
        inject(contents).catch(reportFailure);
      });
    };
    record.watch = watch;
    electron.app.on('web-contents-created', watch);
    await load(electron.session.defaultSession);
    for (const contents of electron.webContents.getAllWebContents()) {
      if (contents.getType() !== 'window') continue;
      watch(null, contents);
      await inject(contents);
    }
    record.state = 'ready';
    return {app_version: electron.app.getVersion(), bundle: digest};
  } catch (error) {
    record.state = 'failed';
    throw error;
  }
}
