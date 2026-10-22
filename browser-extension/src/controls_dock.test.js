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
  return footer;
}

describe('syncControlsDock', () => {
  afterEach(() => {
    document.documentElement.removeAttribute('data-recog-docked');
    document.documentElement.style.removeProperty('--recog-dock-bottom');
  });

  test('Element Call footer rendered - sync - buttons move next to the settings button and panels open above the bar', () => {
    const footer = renderFooter();

    expect(syncControlsDock(new ElementCallAdapter())).toBe(true);

    const dock = footer.firstElementChild.querySelector('.recog-controls-dock');
    expect(dock.querySelector('.jitsi-transcript-toggle')).not.toBeNull();
    expect(dock.querySelector('.ktalk-debug-toggle')).not.toBeNull();
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(true);
    expect(document.documentElement.style.getPropertyValue('--recog-dock-bottom')).toBe('144px');
  });

  test('docked buttons - footer slot stops rendering - buttons go back to the page body', () => {
    const footer = renderFooter();
    const adapter = new ElementCallAdapter();
    syncControlsDock(adapter);

    footer.firstElementChild.getClientRects = () => [];
    expect(syncControlsDock(adapter, { force: true })).toBe(false);

    expect(document.querySelector('.jitsi-transcript-toggle').parentElement).toBe(document.body);
    expect(document.querySelector('.ktalk-debug-toggle').parentElement).toBe(document.body);
    expect(document.querySelector('.recog-controls-dock')).toBeNull();
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(false);
  });

  test('docked buttons - footer re-created - buttons follow into the new footer', () => {
    renderFooter();
    const adapter = new ElementCallAdapter();
    syncControlsDock(adapter);
    const toggle = document.querySelector('.jitsi-transcript-toggle');
    const debug = document.querySelector('.ktalk-debug-toggle');

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
    renderFooter();
    const adapter = new ElementCallAdapter();
    syncControlsDock(adapter);

    document.querySelector('[data-testid="footer-container"]').remove();
    syncControlsDock(adapter);

    expect(document.querySelector('.jitsi-transcript-toggle').parentElement).toBe(document.body);
    expect(document.querySelector('.ktalk-debug-toggle').parentElement).toBe(document.body);
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(false);
  });

  test('platform without a dock hook - sync - buttons stay fixed on the page body', () => {
    renderFooter();

    expect(syncControlsDock({})).toBe(false);

    expect(document.querySelector('.jitsi-transcript-toggle').parentElement).toBe(document.body);
    expect(document.documentElement.hasAttribute('data-recog-docked')).toBe(false);
  });
});
