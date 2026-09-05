/**
 * Tests for the focus guard: window.focus suppression, pass-through
 * logging wrappers, idempotent install.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://ktalk.ru/"}
 */

import { jest } from '@jest/globals';
import { installFocusGuard } from './focus_guard.js';

function buildFakeWindow() {
  class FakeNotification {
    constructor(title) {
      this.title = title;
    }
    static requestPermission() {
      return Promise.resolve('granted');
    }
  }
  FakeNotification.permission = 'default';

  return {
    focus: jest.fn(),
    alert: jest.fn(),
    confirm: jest.fn(() => true),
    prompt: jest.fn(() => 'answer'),
    Notification: FakeNotification,
    Element: class {},
    document: { visibilityState: 'visible' }
  };
}

describe('installFocusGuard', () => {
  beforeEach(() => {
    jest.spyOn(console, 'log').mockImplementation(() => {});
  });

  afterEach(() => {
    console.log.mockRestore();
  });

  it('suppresses window.focus', () => {
    const win = buildFakeWindow();
    const origFocus = win.focus;

    installFocusGuard(win);
    win.focus();

    expect(origFocus).not.toHaveBeenCalled();
  });

  it('passes dialogs through and preserves return values', () => {
    const win = buildFakeWindow();

    installFocusGuard(win);

    win.alert('hello');
    expect(win.confirm('sure?')).toBe(true);
    expect(win.prompt('name?')).toBe('answer');
  });

  it('keeps Notification constructible with statics intact', () => {
    const win = buildFakeWindow();

    installFocusGuard(win);
    const n = new win.Notification('ping');

    expect(n.title).toBe('ping');
    expect(win.Notification.permission).toBe('default');
    return expect(win.Notification.requestPermission()).resolves.toBe('granted');
  });

  it('is idempotent: second install does not re-wrap', () => {
    const win = buildFakeWindow();

    installFocusGuard(win);
    const wrappedFocus = win.focus;
    installFocusGuard(win);

    expect(win.focus).toBe(wrappedFocus);
  });

  it('logs fullscreen requests and forwards to the original', () => {
    const win = buildFakeWindow();
    const origRequestFullscreen = jest.fn(() => Promise.resolve());
    win.Element.prototype.requestFullscreen = origRequestFullscreen;

    installFocusGuard(win);
    const el = new win.Element();
    el.tagName = 'VIDEO';
    el.requestFullscreen = win.Element.prototype.requestFullscreen;

    return el.requestFullscreen().then(() => {
      expect(origRequestFullscreen).toHaveBeenCalled();
    });
  });
});
