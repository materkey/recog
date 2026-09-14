// Shared by Chrome's content script and the Electron desktop loader.
export function bootstrapRuntime(version, initialize) {
  if (window.__recogDesktopRuntime) return window.__recogDesktopRuntime;

  const runtime = { version, state: 'initializing' };
  window.__recogDesktopRuntime = runtime;
  const start = () => {
    try {
      initialize();
      runtime.state = 'ready';
    } catch (error) {
      // Partial initialization may already own timers or a socket. Reloading
      // the page is the retry boundary; evaluation cannot safely retry it.
      runtime.state = 'failed';
      console.error('[Recog] Runtime initialization failed', error);
    }
  };
  if (document.body) start();
  else document.addEventListener('DOMContentLoaded', start, { once: true });
  return runtime;
}
