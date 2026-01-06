import { jest } from '@jest/globals';

// Mock browser APIs not available in jsdom
Object.defineProperty(navigator, 'clipboard', {
  value: {
    writeText: jest.fn().mockResolvedValue(undefined),
    readText: jest.fn().mockResolvedValue(''),
  },
  writable: true,
});

// Mock localStorage
const localStorageMock = {
  getItem: jest.fn(),
  setItem: jest.fn(),
  removeItem: jest.fn(),
  clear: jest.fn(),
};
Object.defineProperty(window, 'localStorage', { value: localStorageMock });

// Mock CustomEvent dispatch
window.dispatchEvent = jest.fn();

// Make jest available globally for tests
globalThis.jest = jest;

// Mock scrollIntoView for jsdom
Element.prototype.scrollIntoView = jest.fn();
