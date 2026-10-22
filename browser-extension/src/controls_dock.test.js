import { syncControlsDock } from './controls_dock.js';
import { ElementCallAdapter } from './platforms/matrix.js';

function renderFooter({ rendered = true } = {}) {
  document.body.innerHTML = `
    <div data-testid="footer-container">
      <div class="settings"><button data-testid="settings-bottom-left"></button></div>
      <div class="buttons"></div>
    </div>
    <button class="jitsi-transcript-toggle"></button>
    <button class="ktalk-debug-toggle">Debug</button>
  `;
  const footer = document.querySelector('[data-testid="footer-container"]');
  footer.getBoundingClientRect = () => ({ top: window.innerHeight - 136 });
  footer.firstElementChild.getClientRects = () => (rendered ? [{}] : []);
  return {
    footer,
    toggle: document.querySelector('.jitsi-transcript-toggle'),
    debug: document.querySelector('.ktalk-debug-toggle'),
  };
}

describe('syncControlsDock', () => {
  afterEach(() => {
    // The module remembers its dock and a hidden slot: detach everything,
    // let a sync without a slot drop the dock, and start the next test empty.
    document.body.innerHTML = '';
    syncControlsDock({});
    document.body.innerHTML = '';
    document.documentElement.removeAttribute('data-recog-docked');
    document.documentElement.style.removeProperty('--recog-dock-bottom');
  });

  test('Element Call footer rendered - sync - buttons move next to the settings button and panels open above the bar', () => {
    const { footer, toggle, debug } = renderFooter();

    expect(syncControlsDock(new ElementCallAdapter())).toBe(true);

    const dock = footer.firstElementChild.querySelector('.recog-controls-dock');
    expect(toggle.parentElement).toBe(dock);
    expect(debug.parentElement).toBe(dock);
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(true);
    expect(document.documentElement.style.getPropertyValue('--recog-dock-bottom')).toBe('144px');
  });

  test('docked buttons - footer slot stops rendering - buttons go back to the page body', () => {
    const { footer, toggle, debug } = renderFooter();
    const adapter = new ElementCallAdapter();
    syncControlsDock(adapter);

    footer.firstElementChild.getClientRects = () => [];
    expect(syncControlsDock(adapter, { force: true })).toBe(false);

    expect(toggle.parentElement).toBe(document.body);
    expect(debug.parentElement).toBe(document.body);
    expect(document.querySelector('.recog-controls-dock')).toBeNull();
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(false);
  });

  test('docked buttons - footer re-created - buttons follow into the new footer', () => {
    const { toggle, debug } = renderFooter();
    const adapter = new ElementCallAdapter();
    syncControlsDock(adapter);

    // React drops the old bar with the dock (and the buttons) still inside.
    document.querySelector('[data-testid="footer-container"]').remove();
    const fresh = document.createElement('div');
    fresh.setAttribute('data-testid', 'footer-container');
    fresh.innerHTML = '<div class="settings"></div>';
    fresh.getBoundingClientRect = () => ({ top: window.innerHeight - 120 });
    fresh.firstElementChild.getClientRects = () => [{}];
    document.body.appendChild(fresh);

    syncControlsDock(adapter);

    expect(toggle.parentElement.parentElement).toBe(fresh.firstElementChild);
    expect(debug.parentElement).toBe(toggle.parentElement);
    expect(document.querySelectorAll('.recog-controls-dock')).toHaveLength(1);
    expect(document.documentElement.style.getPropertyValue('--recog-dock-bottom')).toBe('128px');
  });

  test('docked buttons - bar removed before the new one renders - buttons return to the page body', () => {
    const { toggle, debug } = renderFooter();
    const adapter = new ElementCallAdapter();
    syncControlsDock(adapter);

    document.querySelector('[data-testid="footer-container"]').remove();
    syncControlsDock(adapter);

    expect(toggle.parentElement).toBe(document.body);
    expect(debug.parentElement).toBe(document.body);
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(false);
  });

  test('docked buttons - slot moves to another element while the old dock stays connected - buttons follow into the new slot', () => {
    const { footer, toggle, debug } = renderFooter();
    let slot = footer.firstElementChild;
    const adapter = { getControlsDock: () => slot, getControlsBar: () => footer };
    syncControlsDock(adapter);

    slot = footer.lastElementChild;
    slot.getClientRects = () => [{}];
    syncControlsDock(adapter);

    expect(toggle.parentElement.parentElement).toBe(slot);
    expect(debug.parentElement).toBe(toggle.parentElement);
    expect(document.querySelectorAll('.recog-controls-dock')).toHaveLength(1);
  });

  test('slot hidden - repeated syncs - no layout query until a forced recheck finds it rendered', () => {
    const { footer, toggle } = renderFooter({ rendered: false });
    const slot = footer.firstElementChild;
    const rects = jest.fn(() => []);
    slot.getClientRects = rects;
    const adapter = new ElementCallAdapter();

    expect(syncControlsDock(adapter)).toBe(false);
    syncControlsDock(adapter);
    syncControlsDock(adapter);
    expect(rects).toHaveBeenCalledTimes(1);
    expect(toggle.parentElement).toBe(document.body);

    rects.mockImplementation(() => [{}]);
    expect(syncControlsDock(adapter, { force: true })).toBe(true);
    expect(toggle.parentElement.parentElement).toBe(slot);
  });

  test('platform without a dock hook - sync - buttons stay fixed on the page body', () => {
    const { toggle } = renderFooter();

    expect(syncControlsDock({})).toBe(false);

    expect(toggle.parentElement).toBe(document.body);
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(false);
  });
});
