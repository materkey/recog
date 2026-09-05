/**
 * Regression tests for the "empty Copilot tab" bug on floating-panel
 * platforms (matrix / vas3k).
 *
 * Real load order on those platforms: connectWebSocket fires 1 s after
 * injection, and the bridge sends `copilot_status` primers immediately on
 * connect (`_handle_client` in ktalk_bridge.py) — BEFORE init.js's 2 s
 * interval ever runs setupUI(). handleCopilotStatus → ensureCopilotContent
 * → createFloatingPanel() creates the panel as a side effect, so the
 * DOM-presence gate in init.js used to skip setupUI()/initPlatformUI()
 * forever, leaving the Copilot sub-tab's activate handler unregistered:
 * clicking the tab switched the view but never built/activated the content
 * (`.ktalk-copilot-content` without `.active` is display:none) — an empty,
 * collapsed panel.
 *
 * This file must NOT call initPlatformUI() before the tests: the whole
 * point is exercising the state where the panel exists but platform UI
 * init has not run (which is why it can't live in ui-copilot-panel.test.js
 * — the module-level activate handler would already be registered).
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://element.example.org/widgets/element-call/index.html"}
 */

import { jest } from '@jest/globals';

let state;
let handleCopilotStatus;
let init;

const fakeAdapter = {
  supportsTabInjection: () => false,
  isInPrejoin: () => false,
  isConferenceAvailable: () => false,
  waitForConference: () => new Promise(() => {}),
  getConference: () => null,
  getJitsiConference: () => null,
  dispose: () => {},
};

beforeAll(async () => {
  await jest.unstable_mockModule('./platforms/index.js', () => ({
    __esModule: true,
    getPlatform: jest.fn(() => 'matrix'),
    getPlatformAdapter: jest.fn(() => fakeAdapter),
    getPlatformDisplayName: jest.fn(() => 'Element Call'),
    detectPlatform: jest.fn(() => 'matrix'),
    isJitsiPlatform: jest.fn(() => true),
    resetAdapter: jest.fn(),
  }));
  // Keep init() from opening a real WebSocket to the bridge.
  await jest.unstable_mockModule('./ws.js', () => ({
    __esModule: true,
    connectWebSocket: jest.fn(),
  }));

  state = (await import('./state.js')).state;
  ({ handleCopilotStatus } = await import('./ui.js'));
  ({ init } = await import('./init.js'));
});

beforeEach(() => {
  document.body.innerHTML = '';
  document.head.querySelectorAll('style').forEach((s) => s.remove());
  state.copilotMessages = [];
  state.copilotPending = new Map();
  state.copilotContentElement = null;
  state.debugPanelElement = null;
  state.isCopilotActive = false;
  state.copilotUnread = false;
  state.vas3kActiveView = 'transcript';
  state.transcriptLines = [];
  state.wsConnected = true;
  state.wsConnection = { readyState: 1, send: jest.fn() };
});

test('Copilot tab activates even when the panel was first created by an early bridge frame', () => {
  // Bridge primer arrives before any setupUI()/initPlatformUI() call and
  // creates the floating panel through ensureCopilotContent.
  handleCopilotStatus({ type: 'copilot_status', status: 'agent_state', agent_state: 'online' });

  const panel = document.querySelector('.jitsi-transcript-panel');
  expect(panel).not.toBeNull();

  panel.querySelector('.jitsi-panel-tab[data-view="copilot"]').click();

  const content = panel.querySelector('.jitsi-copilot-host .ktalk-copilot-content');
  expect(content).not.toBeNull();
  expect(content.classList.contains('active')).toBe(true);
  expect(state.isCopilotActive).toBe(true);
});

test('init interval still runs setupUI when the panel already exists (side-created)', () => {
  jest.useFakeTimers();
  try {
    // Panel side-created by an early bridge frame, as above.
    handleCopilotStatus({ type: 'copilot_status', status: 'agent_state', agent_state: 'online' });
    expect(document.querySelector('.jitsi-transcript-panel')).not.toBeNull();

    init();
    jest.advanceTimersByTime(2000);

    // setupUI must have run despite the pre-existing panel — the debug panel
    // is only ever mounted from setupUI on floating-panel platforms.
    expect(document.querySelector('.ktalk-debug-panel')).not.toBeNull();
  } finally {
    jest.useRealTimers();
  }
});
