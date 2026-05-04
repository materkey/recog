import { jest } from '@jest/globals';

// Mock browser APIs not available in jsdom
Object.defineProperty(navigator, 'clipboard', {
  value: {
    writeText: jest.fn().mockResolvedValue(undefined),
    readText: jest.fn().mockResolvedValue(''),
  },
  writable: true,
});

// Mock localStorage with actual storage
const localStorageStore = {};
const localStorageMock = {
  getItem: jest.fn((key) => localStorageStore[key] ?? null),
  setItem: jest.fn((key, value) => { localStorageStore[key] = value; }),
  removeItem: jest.fn((key) => { delete localStorageStore[key]; }),
  clear: jest.fn(() => { Object.keys(localStorageStore).forEach(k => delete localStorageStore[k]); }),
};
Object.defineProperty(window, 'localStorage', { value: localStorageMock });

// Mock CustomEvent dispatch
window.dispatchEvent = jest.fn();

// Make jest available globally for tests
globalThis.jest = jest;

// Mock scrollIntoView for jsdom
Element.prototype.scrollIntoView = jest.fn();

// jsdom does not expose the CSS interface; provide a minimal CSS.escape
// polyfill so code paths that build attribute selectors (e.g. scroll-anchor
// restoration) can run under tests. Real Chrome (MV3) ships CSS.escape natively.
if (typeof globalThis.CSS === 'undefined' || typeof globalThis.CSS.escape !== 'function') {
  globalThis.CSS = globalThis.CSS || {};
  globalThis.CSS.escape = (value) => String(value).replace(/[^a-zA-Z0-9_-]/g, (ch) => `\\${ch}`);
}
