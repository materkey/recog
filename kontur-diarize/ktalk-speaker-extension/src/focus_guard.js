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

  // PiP-on-hide is the Space-yank vector: KTalk opens a PiP window when
  // the tab loses visibility (a macOS Space switch qualifies), and the
  // PiP window activates Chrome. Block PiP only while the document is
  // hidden — manual PiP from a visible page keeps working, and the
  // extension's own PiP button can grant a one-shot pass
  // (window.__ktalkAllowPipOnce) to open KTalk's native popup on demand.
  const pipBlockedWhileHidden = (api) => {
    console.log(`[KTalk Recog] ${api} blocked while hidden (auto-PiP guard)`);
    return Promise.reject(new DOMException('Blocked by KTalk Recog focus guard', 'NotAllowedError'));
  };
  const isPipBlocked = () => {
    if (win.__ktalkAllowPipOnce) {
      win.__ktalkAllowPipOnce = false;
      console.log('[KTalk Recog] PiP allowed once (manual open)');
      return false;
    }
    return win.document && win.document.visibilityState === 'hidden';
  };

  const videoProto = win.HTMLVideoElement && win.HTMLVideoElement.prototype;
  if (videoProto && typeof videoProto.requestPictureInPicture === 'function') {
    const origRequestPip = videoProto.requestPictureInPicture;
    videoProto.requestPictureInPicture = function (...args) {
      if (isPipBlocked()) {
        return pipBlockedWhileHidden('requestPictureInPicture');
      }
      console.log('[KTalk Recog] requestPictureInPicture called');
      return origRequestPip.apply(this, args);
    };
  }

  // Document Picture-in-Picture — the path KTalk actually uses on
  // visibilitychange (its PiP open does not go through
  // HTMLVideoElement.requestPictureInPicture).
  const docPip = win.documentPictureInPicture;
  if (docPip && typeof docPip.requestWindow === 'function') {
    const origRequestWindow = docPip.requestWindow.bind(docPip);
    docPip.requestWindow = function (...args) {
      if (isPipBlocked()) {
        return pipBlockedWhileHidden('documentPictureInPicture.requestWindow');
      }
      console.log('[KTalk Recog] documentPictureInPicture.requestWindow called');
      return origRequestWindow(...args);
    };
  }

  // Auto-PiP kill switch. Chrome's "Automatic picture-in-picture" for
  // video-call sites fires when the tab loses visibility (including a
  // macOS Space switch), and the PiP window activates Chrome — which
  // yanks the user back to Chrome's Space. Eligibility requires the
  // site to register a mediaSession "enterpictureinpicture" handler:
  // drop existing and future registrations.
  const msProto = win.MediaSession && win.MediaSession.prototype;
  if (msProto && typeof msProto.setActionHandler === 'function') {
    const origSetActionHandler = msProto.setActionHandler;
    msProto.setActionHandler = function (action, handler) {
      if (action === 'enterpictureinpicture') {
        console.log('[KTalk Recog] mediaSession enterpictureinpicture registration suppressed (auto-PiP guard)');
        return undefined;
      }
      return origSetActionHandler.call(this, action, handler);
    };
    const mediaSession = win.navigator && win.navigator.mediaSession;
    if (mediaSession) {
      try {
        origSetActionHandler.call(mediaSession, 'enterpictureinpicture', null);
        console.log('[KTalk Recog] existing enterpictureinpicture handler cleared (auto-PiP guard)');
      } catch (e) {
        void e;
      }
    }
  }

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
