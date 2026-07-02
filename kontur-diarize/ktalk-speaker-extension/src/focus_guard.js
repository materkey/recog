/**
 * Focus guard.
 *
 * On macOS an activated browser window yanks the user back to Chrome's
 * Space ("When switching to an application, switch to a Space with open
 * windows for the application"). A conference page has a handful of
 * vectors that can activate the window while the user works elsewhere.
 *
 * Suppressed:
 * - window.focus() — on a meeting page its only purpose is "bring the
 *   meeting back", which is exactly the behavior being disabled.
 *
 * Logged but not suppressed (they have legitimate in-meeting uses, and
 * the log identifies the culprit if the yank persists):
 * - Element.requestFullscreen
 * - new Notification(...)
 * - alert / confirm / prompt (native dialogs activate the window)
 */
export function installFocusGuard(win = window) {
  if (win.__ktalkFocusGuardInstalled) return;
  win.__ktalkFocusGuardInstalled = true;

  try {
    win.focus = function focusSuppressed() {
      console.log('[KTalk Recog] window.focus() suppressed by focus guard');
    };
  } catch (e) {
    console.log('[KTalk Recog] focus guard: window.focus override failed', e);
  }

  const elementProto = win.Element && win.Element.prototype;
  if (elementProto && typeof elementProto.requestFullscreen === 'function') {
    const origRequestFullscreen = elementProto.requestFullscreen;
    elementProto.requestFullscreen = function (...args) {
      console.log('[KTalk Recog] requestFullscreen:', this.tagName);
      return origRequestFullscreen.apply(this, args);
    };
  }

  if (typeof win.Notification === 'function') {
    win.Notification = new Proxy(win.Notification, {
      construct(target, args) {
        console.log('[KTalk Recog] Notification created:', args[0]);
        return Reflect.construct(target, args);
      }
    });
  }

  ['alert', 'confirm', 'prompt'].forEach((name) => {
    if (typeof win[name] !== 'function') return;
    const orig = win[name].bind(win);
    win[name] = function (...args) {
      console.log(`[KTalk Recog] ${name}() called by page:`, args[0]);
      return orig(...args);
    };
  });

  // Timestamped window-activation trace: when macOS yanks the user back
  // to Chrome's Space, this window gains focus — the log pinpoints the
  // moment so it can be correlated with front-app-monitor.sh and the
  // vector logs above (see test-stand/).
  if (typeof win.addEventListener === 'function') {
    const stamp = () => {
      const d = new Date();
      return d.toTimeString().slice(0, 8) + '.' + String(d.getMilliseconds()).padStart(3, '0');
    };
    win.addEventListener('focus', () => console.log(`[KTalk Recog] window FOCUS at ${stamp()}`));
    win.addEventListener('blur', () => console.log(`[KTalk Recog] window blur at ${stamp()}`));
    const doc = win.document;
    if (doc && typeof doc.addEventListener === 'function') {
      doc.addEventListener('visibilitychange', () => {
        console.log(`[KTalk Recog] visibility=${doc.visibilityState} at ${stamp()}`);
      });
      doc.addEventListener('fullscreenchange', () => {
        const el = doc.fullscreenElement;
        console.log(`[KTalk Recog] fullscreenchange element=${el ? el.tagName : 'none'} at ${stamp()}`);
      });
    }
  }
}
