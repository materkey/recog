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
