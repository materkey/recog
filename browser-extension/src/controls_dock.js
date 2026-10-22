/**
 * Docks the floating-panel toggle and the Debug button into the platform's own
 * control bar.
 *
 * Both buttons are `position: fixed` in the bottom-right corner, which on
 * Element Call is where the layout switcher sits. A platform that has a free
 * spot in its control bar returns it from the optional adapter hook
 * `getControlsDock()`; the buttons then live there as ordinary flex items and
 * follow the bar (its hiding included). The transcript and debug panels open
 * above the bar instead of over it. Without a slot, or with the slot not
 * rendered (narrow layouts), the buttons go back to `document.body` and the
 * fixed placement applies again.
 */

const DOCK_CLASS = 'recog-controls-dock';
const DOCKED_ATTR = 'data-recog-docked';
const PANEL_GAP_PX = 8;
const DOCKABLE = ['.jitsi-transcript-toggle', '.ktalk-debug-toggle'];

// The dock last created. The platform re-creates its bar (Element Call's lobby
// gives way to the call view) and takes the dock out of the document with
// everything in it, where `document.querySelector` no longer sees the buttons.
let currentDock = null;

function injectDockStyles() {
  if (document.querySelector('#recog-controls-dock-styles')) return;

  const styles = document.createElement('style');
  styles.id = 'recog-controls-dock-styles';
  styles.textContent = `
    .${DOCK_CLASS} {
      display: flex;
      align-items: center;
      gap: var(--cpd-space-3x, 12px);
    }
    .${DOCK_CLASS} .jitsi-transcript-toggle,
    .${DOCK_CLASS} .ktalk-debug-toggle {
      position: static;
      box-shadow: none;
    }
    .${DOCK_CLASS} .jitsi-transcript-toggle {
      width: 32px;
      height: 32px;
    }
    .${DOCK_CLASS} .jitsi-transcript-toggle svg {
      width: 18px;
      height: 18px;
    }
    html[${DOCKED_ATTR}] .jitsi-transcript-panel {
      bottom: var(--recog-dock-bottom, 80px);
    }
    html[${DOCKED_ATTR}] .ktalk-debug-panel {
      left: 24px;
      right: auto;
      bottom: var(--recog-dock-bottom, 112px);
    }
  `;

  document.head.appendChild(styles);
}

function isRendered(el) {
  return !!el && el.isConnected && el.getClientRects().length > 0;
}

function rescueDetachedButtons() {
  if (!currentDock || currentDock.isConnected) return;
  [...currentDock.children].forEach((el) => document.body.appendChild(el));
  currentDock = null;
}

function undock(root) {
  root.removeAttribute(DOCKED_ATTR);
  root.style.removeProperty('--recog-dock-bottom');
  DOCKABLE.forEach((selector) => {
    const el = document.querySelector(selector);
    if (el && el.parentElement !== document.body) document.body.appendChild(el);
  });
  document.querySelectorAll(`.${DOCK_CLASS}`).forEach((dock) => dock.remove());
}

function isDocked(dock) {
  return !!dock && DOCKABLE.every((selector) => {
    const el = document.querySelector(selector);
    return !el || el.parentElement === dock;
  });
}

/**
 * Put the buttons where they belong right now. Runs from a MutationObserver on
 * a page that mutates constantly, so with everything in place it returns
 * before any layout query; `force` (window resize: the bar's height and the
 * slot's visibility follow media queries) checks and measures anyway.
 */
export function syncControlsDock(adapter, { force = false } = {}) {
  rescueDetachedButtons();
  const root = document.documentElement;
  const slot = adapter?.getControlsDock?.() || null;
  if (!force && slot && isDocked(slot.querySelector(`:scope > .${DOCK_CLASS}`))) return true;
  if (!isRendered(slot)) {
    if (root.hasAttribute(DOCKED_ATTR)) undock(root);
    return false;
  }

  injectDockStyles();
  let dock = slot.querySelector(`:scope > .${DOCK_CLASS}`);
  if (!dock) {
    // A dock left in a previous bar (the bar was re-created) is dropped.
    document.querySelectorAll(`.${DOCK_CLASS}`).forEach((stale) => stale.remove());
    dock = document.createElement('div');
    dock.className = DOCK_CLASS;
    slot.appendChild(dock);
    currentDock = dock;
  }
  DOCKABLE.forEach((selector) => {
    const el = document.querySelector(selector);
    if (el && el.parentElement !== dock) dock.appendChild(el);
  });

  const bar = adapter.getControlsBar?.() || slot;
  const bottom = Math.max(0, window.innerHeight - bar.getBoundingClientRect().top) + PANEL_GAP_PX;
  const value = `${Math.round(bottom)}px`;
  if (root.style.getPropertyValue('--recog-dock-bottom') !== value) {
    root.style.setProperty('--recog-dock-bottom', value);
  }
  if (!root.hasAttribute(DOCKED_ATTR)) root.setAttribute(DOCKED_ATTR, '');
  return true;
}

/**
 * Keep the dock in sync with the platform's re-renders. A no-op for platforms
 * without the hook.
 */
export function watchControlsDock(adapter) {
  if (typeof adapter?.getControlsDock !== 'function') return null;
  const observer = new MutationObserver(() => syncControlsDock(adapter));
  observer.observe(document.body, { childList: true, subtree: true });
  window.addEventListener('resize', () => syncControlsDock(adapter, { force: true }));
  syncControlsDock(adapter);
  return observer;
}
