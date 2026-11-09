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

  it('a window whose focus cannot be replaced - installed - logs the failure and keeps going', () => {
    const win = buildFakeWindow();
    const origFocus = win.focus;
    Object.defineProperty(win, 'focus', { get: () => origFocus });

    installFocusGuard(win);

    expect(console.log).toHaveBeenCalledWith(
      '[KTalk Recog] focus guard: window.focus override failed', expect.any(TypeError)
    );
    expect(win.focus).toBe(origFocus);
    win.alert('still wrapped');
    expect(console.log).toHaveBeenCalledWith('[KTalk Recog] alert() called by page:', 'still wrapped');
  });

  it('a window without Element, Notification or dialogs - installed - leaves them absent', () => {
    const win = { focus: jest.fn(), document: null };

    installFocusGuard(win);

    expect(win.Notification).toBeUndefined();
    expect(win.alert).toBeUndefined();
    expect(win.__ktalkFocusGuardInstalled).toBe(true);
  });

  it('window and document events - fired - log focus, blur, visibility and fullscreen changes', () => {
    const winHandlers = {};
    const docHandlers = {};
    const doc = {
      visibilityState: 'hidden',
      fullscreenElement: null,
      addEventListener: (type, fn) => { docHandlers[type] = fn; },
    };
    const win = {
      ...buildFakeWindow(),
      document: doc,
      addEventListener: (type, fn) => { winHandlers[type] = fn; },
    };

    installFocusGuard(win);
    winHandlers.focus();
    winHandlers.blur();
    docHandlers.visibilitychange();
    docHandlers.fullscreenchange();
    doc.fullscreenElement = { tagName: 'VIDEO' };
    docHandlers.fullscreenchange();

    const lines = console.log.mock.calls
      .map(
        (args) => args[0]
      );
    const stamp = '\\d\\d:\\d\\d:\\d\\d\\.\\d{3}';
    expect(lines).toEqual(expect.arrayContaining([
      expect.stringMatching(new RegExp(`^\\[KTalk Recog\\] window FOCUS at ${stamp}$`)),
      expect.stringMatching(new RegExp(`^\\[KTalk Recog\\] window blur at ${stamp}$`)),
      expect.stringMatching(new RegExp(`^\\[KTalk Recog\\] visibility=hidden at ${stamp}$`)),
      expect.stringMatching(new RegExp(`^\\[KTalk Recog\\] fullscreenchange element=none at ${stamp}$`)),
      expect.stringMatching(new RegExp(`^\\[KTalk Recog\\] fullscreenchange element=VIDEO at ${stamp}$`)),
    ]));
  });

  it('a window with events but no document listener - installed - registers window events only', () => {
    const winHandlers = {};
    const win = {
      ...buildFakeWindow(),
      document: {},
      addEventListener: (type, fn) => { winHandlers[type] = fn; },
    };

    installFocusGuard(win);

    expect(Object.keys(winHandlers).sort()).toEqual(['blur', 'focus']);
  });
});
