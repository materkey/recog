/**
 * Task 7 — copilot-frame routing.
 *
 * `copilot_reply` / `copilot_status` are dispatched by ws.js to the shared
 * ui.js handlers (`handleCopilotReply` / `handleCopilotStatus`). Those handlers
 * are platform-aware: `createCopilotContent()` mounts the copilot chat DOM into
 * the floating panel on floating-panel platforms (matrix / vas3k, where the
 * adapter's `supportsTabInjection() === false`) and into the KTalk slide-menu
 * tab otherwise. This file locks BOTH directions of that routing decision in a
 * single run by flipping the platform predicate, so a regression that sends
 * copilot frames to the wrong surface fails loudly.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';

let state;
let handleCopilotReply;
let handleCopilotStatus;
let isJitsiPlatform;

// Adapter stubs mirror the real adapters' `supportsTabInjection()` contract:
// ktalk injects into the native tab, matrix/vas3k render a floating panel.
const ktalkAdapter = { supportsTabInjection: () => true };
const matrixAdapter = { supportsTabInjection: () => false };

beforeAll(async () => {
  await jest.unstable_mockModule('./platforms/index.js', () => ({
    __esModule: true,
    getPlatform: jest.fn(() => 'matrix'),
    getPlatformAdapter: jest.fn(() => null),
    getPlatformDisplayName: jest.fn(() => 'Element Call'),
    detectPlatform: jest.fn(() => 'matrix'),
    isJitsiPlatform: jest.fn(() => true),
    resetAdapter: jest.fn(),
  }));

  state = (await import('./state.js')).state;
  ({ handleCopilotReply, handleCopilotStatus } = await import('./ui.js'));
  ({ isJitsiPlatform } = await import('./platforms/index.js'));
});

function resetCopilotState() {
  state.copilotMessages = [];
  state.copilotPending = new Map();
  state.copilotProactiveMutedUntil = 0;
  state.copilotChannelStatus = 'unknown';
  state.copilotAgentState = 'offline';
  state.copilotLastProactiveAt = 0;
  state.copilotContentElement = null;
  state.transcriptContentElement = null;
  state.originalChatContent = null;
  state.isCopilotActive = false;
  state.copilotUnread = false;
  state.vas3kActiveView = 'transcript';
  state.transcriptLines = [];
  state.currentMeetingId = null;
}

beforeEach(() => {
  document.body.innerHTML = '';
  // Drop styles injected by a prior test so injection guards re-run cleanly.
  document.head.querySelectorAll('style').forEach((s) => s.remove());
  resetCopilotState();
  state.wsConnected = true;
  state.wsConnection = { readyState: WebSocket.OPEN, send: jest.fn() };
});

describe('floating-panel platform (supportsTabInjection() === false)', () => {
  beforeEach(() => {
    isJitsiPlatform.mockReturnValue(true);
  });

  test('adapter contract: floating-panel adapter does not inject into a tab', () => {
    expect(matrixAdapter.supportsTabInjection()).toBe(false);
  });

  test('copilot_reply lands in the floating-panel copilot view, not the KTalk tab', () => {
    handleCopilotReply({
      type: 'copilot_reply',
      request_id: 'unmatched',
      text: 'ответ во floating panel',
      kind: 'assistant',
    });

    const panel = document.querySelector('.jitsi-transcript-panel');
    expect(panel).not.toBeNull();
    const bubble = panel.querySelector('.jitsi-copilot-host .copilot-msg-assistant .copilot-msg-text');
    expect(bubble).not.toBeNull();
    expect(bubble.textContent).toBe('ответ во floating panel');
    // The KTalk slide-menu tab surface must never be created on this platform.
    expect(document.querySelector('.slide-menu-wrapper')).toBeNull();
    expect(document.querySelector('.ktalk-ai-panel')).toBeNull();
  });

  test('copilot_status agent_state drives the chip inside the floating panel', () => {
    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'online',
    });

    const chip = document.querySelector('.jitsi-transcript-panel .jitsi-copilot-host .ktalk-copilot-agent-chip');
    expect(chip).not.toBeNull();
    expect(chip.classList.contains('online')).toBe(true);
    expect(chip.textContent).toBe('🟢 online');
    expect(document.querySelector('.slide-menu-wrapper')).toBeNull();
  });
});

describe('KTalk platform (supportsTabInjection() === true)', () => {
  beforeEach(() => {
    isJitsiPlatform.mockReturnValue(false);
    // KTalk mounts the copilot view into the native slide-menu content area,
    // which must already exist for `ensureAiPanelBody` to attach the panel.
    document.body.innerHTML = `
      <div class="slide-menu-wrapper">
        <div class="content"><chat-widget class="native-chat"></chat-widget></div>
      </div>
    `;
  });

  test('adapter contract: KTalk adapter injects into the native tab', () => {
    expect(ktalkAdapter.supportsTabInjection()).toBe(true);
  });

  test('copilot_reply lands in the KTalk slide-menu tab, not a floating panel', () => {
    handleCopilotReply({
      type: 'copilot_reply',
      request_id: 'unmatched',
      text: 'ответ в KTalk-табе',
      kind: 'assistant',
    });

    const tabBody = document.querySelector('.slide-menu-wrapper .content .ktalk-ai-panel');
    expect(tabBody).not.toBeNull();
    const bubble = tabBody.querySelector('.copilot-msg-assistant .copilot-msg-text');
    expect(bubble).not.toBeNull();
    expect(bubble.textContent).toBe('ответ в KTalk-табе');
    // No floating panel is ever created on KTalk.
    expect(document.querySelector('.jitsi-transcript-panel')).toBeNull();
  });

  test('copilot_status agent_state drives the chip inside the KTalk tab', () => {
    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'blocked',
    });

    const chip = document.querySelector('.slide-menu-wrapper .content .ktalk-copilot-agent-chip');
    expect(chip).not.toBeNull();
    expect(chip.classList.contains('blocked')).toBe(true);
    expect(chip.textContent).toBe('🔴 blocked');
    expect(document.querySelector('.jitsi-transcript-panel')).toBeNull();
  });
});
