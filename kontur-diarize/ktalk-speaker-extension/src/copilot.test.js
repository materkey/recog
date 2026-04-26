/**
 * Tests for the Copilot DOM panel: rendering, reply / status handling,
 * proactive cooldown and mute, dismiss.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://ktalk.ru/"}
 */

import { jest } from '@jest/globals';

let state;
let renderCopilotMessage;
let handleCopilotReply;
let handleCopilotStatus;
let appendCopilotSystemMessage;
let handleCopilotBridgeDisconnect;

beforeAll(async () => {
  await jest.unstable_mockModule('./platforms/index.js', () => ({
    __esModule: true,
    getPlatform: jest.fn(() => 'ktalk'),
    getPlatformAdapter: jest.fn(() => null),
    getPlatformDisplayName: jest.fn(() => 'KTalk'),
    detectPlatform: jest.fn(() => 'ktalk'),
    isJitsiPlatform: jest.fn(() => false),
    resetAdapter: jest.fn(),
  }));

  state = (await import('./state.js')).state;
  ({
    renderCopilotMessage,
    handleCopilotReply,
    handleCopilotStatus,
    appendCopilotSystemMessage,
    handleCopilotBridgeDisconnect,
  } = await import('./ui.js'));
});

function buildHostDom() {
  document.body.innerHTML = `
    <div class="slide-menu-wrapper">
      <div class="content"><div class="native-chat"></div></div>
    </div>
  `;
}

function resetCopilotState() {
  state.copilotMessages = [];
  state.copilotPending = new Map();
  state.copilotProactiveMutedUntil = 0;
  state.copilotChannelStatus = 'unknown';
  state.copilotLastProactiveAt = 0;
  state.copilotContentElement = null;
  state.isCopilotActive = false;
  state.originalChatContent = null;
  state.transcriptLines = [];
}

describe('Copilot DOM panel', () => {
  beforeEach(() => {
    buildHostDom();
    resetCopilotState();
  });

  test('renderCopilotMessage creates a kind-tagged element with copy/dismiss for assistant', () => {
    const el = renderCopilotMessage({
      id: 'a1',
      kind: 'assistant',
      text: 'привет',
      time: '10:00:01',
    });
    expect(el).not.toBeNull();
    expect(el.classList.contains('copilot-msg')).toBe(true);
    expect(el.classList.contains('copilot-msg-assistant')).toBe(true);
    expect(el.querySelector('.copilot-msg-text').textContent).toBe('привет');
    expect(el.querySelector('.copilot-msg-copy-btn')).not.toBeNull();
    expect(el.querySelector('.copilot-msg-dismiss-btn')).not.toBeNull();
  });

  test('renderCopilotMessage for user kind has no action buttons', () => {
    const el = renderCopilotMessage({
      id: 'u1',
      kind: 'user',
      text: 'hi',
      time: '10:00:00',
    });
    expect(el.classList.contains('copilot-msg-user')).toBe(true);
    expect(el.querySelector('.copilot-msg-copy-btn')).toBeNull();
    expect(el.querySelector('.copilot-msg-dismiss-btn')).toBeNull();
  });

  test('handleCopilotReply replaces the pending assistant placeholder', () => {
    const requestId = 'req-123';
    state.copilotMessages = [
      { id: 'u1', kind: 'user', text: 'q', request_id: requestId, time: '10:00:00' },
      { id: 'a1', kind: 'assistant', text: '…', request_id: requestId, status: 'working', time: '10:00:00' },
    ];
    state.copilotPending.set(requestId, { userId: 'u1', assistantId: 'a1', createdAt: Date.now() });
    renderCopilotMessage(state.copilotMessages[0]);
    renderCopilotMessage(state.copilotMessages[1]);

    handleCopilotReply({
      type: 'copilot_reply',
      request_id: requestId,
      text: 'ответ от Claude',
      kind: 'assistant',
    });

    const assistant = state.copilotMessages.find((m) => m.id === 'a1');
    expect(assistant.text).toBe('ответ от Claude');
    expect(assistant.status).toBe('done');
    expect(state.copilotPending.has(requestId)).toBe(false);

    const dom = document.querySelector('[data-copilot-id="a1"] .copilot-msg-text');
    expect(dom.textContent).toBe('ответ от Claude');
    expect(dom.classList.contains('pending')).toBe(false);
  });

  test('handleCopilotReply proactive respects mute', () => {
    state.copilotProactiveMutedUntil = Date.now() + 60000;
    handleCopilotReply({
      type: 'copilot_reply',
      kind: 'proactive',
      text: 'should drop',
    });
    expect(state.copilotMessages.filter((m) => m.kind === 'proactive')).toHaveLength(0);
  });

  test('handleCopilotReply proactive respects 60s cooldown', () => {
    state.copilotLastProactiveAt = Date.now() - 5000;
    handleCopilotReply({
      type: 'copilot_reply',
      kind: 'proactive',
      text: 'should drop',
    });
    expect(state.copilotMessages.filter((m) => m.kind === 'proactive')).toHaveLength(0);
  });

  test('handleCopilotReply proactive renders when allowed and stamps cooldown', () => {
    state.copilotLastProactiveAt = 0;
    handleCopilotReply({
      type: 'copilot_reply',
      kind: 'proactive',
      text: 'важная подсказка',
    });
    const proactives = state.copilotMessages.filter((m) => m.kind === 'proactive');
    expect(proactives).toHaveLength(1);
    expect(proactives[0].text).toBe('важная подсказка');
    expect(state.copilotLastProactiveAt).toBeGreaterThan(0);

    const dom = document.querySelector('.copilot-msg-proactive .copilot-msg-text');
    expect(dom).not.toBeNull();
    expect(dom.textContent).toBe('важная подсказка');
  });

  test('handleCopilotStatus channel_disconnected flips pending placeholder to error', () => {
    const requestId = 'req-x';
    state.copilotMessages = [
      { id: 'a-x', kind: 'assistant', text: '…', request_id: requestId, status: 'working', time: '10:00:00' },
    ];
    state.copilotPending.set(requestId, { userId: null, assistantId: 'a-x', createdAt: Date.now() });
    renderCopilotMessage(state.copilotMessages[0]);

    handleCopilotStatus({
      type: 'copilot_status',
      request_id: requestId,
      status: 'channel_disconnected',
    });

    const assistant = state.copilotMessages.find((m) => m.id === 'a-x');
    expect(assistant.kind).toBe('error');
    expect(assistant.text).toMatch(/offline/i);
    expect(state.copilotPending.has(requestId)).toBe(false);
    // The Copy/Dismiss row that the assistant placeholder owned must be
    // removed when the kind flips to error — otherwise the user can copy
    // the now-stale spinner text.
    const dom = document.querySelector('[data-copilot-id="a-x"]');
    expect(dom.querySelector('.copilot-msg-actions')).toBeNull();
  });

  test('handleCopilotStatus terminal error flips placeholder, drops pending, re-enables input', () => {
    // Without symmetric handling for status === 'error', the bridge could
    // emit a terminal error and the input would stay disabled forever
    // — handleCopilotStatus would only update the placeholder text and
    // leave state.copilotPending populated, blocking setCopilotInputBusy
    // from running.
    const requestId = 'req-err';
    state.copilotMessages = [
      { id: 'a-err', kind: 'assistant', text: '…', request_id: requestId, status: 'working', time: '10:00:00' },
    ];
    state.copilotPending.set(requestId, { userId: null, assistantId: 'a-err', createdAt: Date.now() });
    renderCopilotMessage(state.copilotMessages[0]);

    handleCopilotStatus({
      type: 'copilot_status',
      request_id: requestId,
      status: 'error',
      message: 'Claude crashed',
    });

    const assistant = state.copilotMessages.find((m) => m.id === 'a-err');
    expect(assistant.kind).toBe('error');
    expect(assistant.text).toBe('Claude crashed');
    expect(state.copilotPending.has(requestId)).toBe(false);
  });

  test('appendCopilotSystemMessage skips state push when Copilot DOM is missing', async () => {
    // Drop the Copilot host so `createCopilotContent` returns null and
    // `renderCopilotMessage` cannot attach the bubble. Without the gate
    // in `appendCopilotMessage`, every meeting_started/ended chip would
    // accumulate in state forever even though no DOM exists.
    document.body.innerHTML = '';
    state.copilotContentElement = null;
    state.copilotMessages = [];

    appendCopilotSystemMessage('orphan event');

    expect(state.copilotMessages).toHaveLength(0);
  });

  test('appendCopilotSystemMessage pushes a status entry into the panel', () => {
    appendCopilotSystemMessage('встреча началась');
    const last = state.copilotMessages[state.copilotMessages.length - 1];
    expect(last.kind).toBe('status');
    expect(last.text).toBe('встреча началась');
    const dom = document.querySelector('.copilot-msg-status .copilot-msg-text');
    expect(dom.textContent).toBe('встреча началась');
  });

  test('dismiss button removes the message from state and DOM', () => {
    const el = renderCopilotMessage({
      id: 'a-dismiss',
      kind: 'assistant',
      text: 'дроп меня',
      time: '10:00:00',
    });
    state.copilotMessages.push({ id: 'a-dismiss', kind: 'assistant', text: 'дроп меня' });

    const btn = el.querySelector('.copilot-msg-dismiss-btn');
    btn.click();

    expect(state.copilotMessages.find((m) => m.id === 'a-dismiss')).toBeUndefined();
    expect(document.querySelector('[data-copilot-id="a-dismiss"]')).toBeNull();
  });

  test('handleCopilotStatus terminal done without prior reply releases pending and re-enables input', () => {
    // Claude can call `set_copilot_status('done')` standalone without a
    // matching `send_copilot_message`. The bridge already pops its
    // bookkeeping for done/error (ktalk_bridge.py:2349); without symmetric
    // handling here, the placeholder would stay on "Жду ответа Claude…"
    // and `setCopilotInputBusy(false)` would never fire.
    const requestId = 'req-done-only';
    state.copilotMessages = [
      { id: 'a-done', kind: 'assistant', text: '…', request_id: requestId, status: 'working', time: '10:00:00' },
    ];
    state.copilotPending.set(requestId, { userId: null, assistantId: 'a-done', createdAt: Date.now() });
    renderCopilotMessage(state.copilotMessages[0]);

    handleCopilotStatus({
      type: 'copilot_status',
      request_id: requestId,
      status: 'done',
    });

    const assistant = state.copilotMessages.find((m) => m.id === 'a-done');
    expect(assistant.kind).toBe('status');
    expect(assistant.text).toMatch(/завершён/i);
    expect(state.copilotPending.has(requestId)).toBe(false);
  });

  test('handleCopilotStatus per-request status does not overwrite channel indicator', () => {
    // `state.copilotChannelStatus` drives the connectivity indicator label.
    // It must reflect channel HEALTH (connected / channel offline), not the
    // current chat turn's status. Without this guard, after one round-trip
    // the indicator would freeze on "done" or "working…" even though the
    // channel is still live, and a stale 'working' from a prior request
    // would mask a real `channel_disconnected` event.
    state.copilotChannelStatus = 'unknown';
    handleCopilotStatus({
      type: 'copilot_status',
      request_id: 'req-progress',
      status: 'working',
    });
    expect(state.copilotChannelStatus).toBe('unknown');

    handleCopilotStatus({
      type: 'copilot_status',
      request_id: 'req-progress',
      status: 'done',
    });
    expect(state.copilotChannelStatus).toBe('unknown');

    handleCopilotStatus({
      type: 'copilot_status',
      request_id: 'req-progress',
      status: 'channel_disconnected',
    });
    expect(state.copilotChannelStatus).toBe('channel_disconnected');
  });

  test('handleCopilotBridgeDisconnect flips pending placeholders to error and re-enables input', () => {
    // Without surfacing the bridge drop, an in-flight chat would leave the
    // textarea disabled and the placeholder stuck on "Жду ответа Claude…"
    // until the page is reloaded — `copilot_reply` / `copilot_status` are
    // the only re-enable paths and neither will fire from a closed socket.
    const requestId = 'req-disc';
    state.copilotMessages = [
      { id: 'a-disc', kind: 'assistant', text: '…', request_id: requestId, status: 'working', time: '10:00:00' },
    ];
    state.copilotPending.set(requestId, { userId: null, assistantId: 'a-disc', createdAt: Date.now() });
    renderCopilotMessage(state.copilotMessages[0]);

    handleCopilotBridgeDisconnect();

    const assistant = state.copilotMessages.find((m) => m.id === 'a-disc');
    expect(assistant.kind).toBe('error');
    expect(assistant.text).toMatch(/offline/i);
    expect(state.copilotPending.size).toBe(0);
    expect(state.copilotChannelStatus).toBe('channel_disconnected');
  });
});
