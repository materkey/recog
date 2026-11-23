/**
 * Tests for the Copilot DOM panel: rendering, reply / status handling,
 * proactive cooldown and mute.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://ktalk.ru/"}
 */

import { jest } from '@jest/globals';

let state;
let renderCopilotMessage;
let createCopilotContent;
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
    createCopilotContent,
    handleCopilotReply,
    handleCopilotStatus,
    appendCopilotSystemMessage,
    handleCopilotBridgeDisconnect,
  } = await import('./ui.js'));
});

function buildHostDom() {
  document.body.innerHTML = `
    <div class="slide-menu-wrapper">
      <div class="content"><chat-widget class="native-chat"></chat-widget></div>
    </div>
  `;
}

function resetCopilotState() {
  state.copilotMessages = [];
  state.copilotPending = new Map();
  state.copilotProactiveMutedUntil = 0;
  state.copilotChannelStatus = 'unknown';
  state.copilotAgentState = 'offline';
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

  test('renderCopilotMessage creates a kind-tagged element with copy only for assistant', () => {
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
    expect(el.querySelectorAll('.copilot-msg-action-btn')).toHaveLength(1);
  });

  test('renderCopilotMessage for user kind has no action buttons', () => {
    const el = renderCopilotMessage({
      id: 'u1',
      kind: 'user',
      text: 'hi',
      time: '10:00:00',
    });
    expect(el.classList.contains('copilot-msg-user')).toBe(true);
    expect(el.querySelectorAll('.copilot-msg-action-btn')).toHaveLength(0);
  });

  test('recreating the panel after the host SPA re-renders the sidebar replays message history', () => {
    // Mirror the production append flow: render first, push after (appendCopilotMessage).
    for (const msg of [
      { id: 'u1', kind: 'user', text: 'вопрос', time: '10:00:00' },
      { id: 'a1', kind: 'assistant', text: 'ответ', time: '10:00:01' },
    ]) {
      renderCopilotMessage(msg);
      state.copilotMessages.push(msg);
    }
    expect(document.querySelectorAll('.copilot-msg')).toHaveLength(2);

    // KTalk SPA re-render drops the injected panel subtree entirely.
    buildHostDom();
    expect(document.querySelector('.ktalk-copilot-content')).toBeNull();

    const el = createCopilotContent();
    const texts = [...el.querySelectorAll('.copilot-msg-text')].map((n) => n.textContent);
    expect(texts).toEqual(['вопрос', 'ответ']);
    expect(el.querySelector('.ktalk-copilot-empty')).toBeNull();
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
    // The Copy row that the assistant placeholder owned must be
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

  test('handleCopilotStatus channel_connected resets indicator from disconnected', () => {
    // Without a recovery path the indicator stays stuck on "channel_disconnected"
    // forever once the channel has dropped. The bridge emits `channel_connected`
    // on the 0→1 channel-client transition (and as a primer for late-joining
    // extensions); the handler must accept it and flip the state back.
    state.copilotChannelStatus = 'channel_disconnected';
    handleCopilotStatus({
      type: 'copilot_status',
      request_id: '',
      status: 'channel_connected',
    });
    expect(state.copilotChannelStatus).toBe('connected');
  });

  test('agent_state chip renders offline by default when the panel is created', () => {
    // createCopilotContent runs via ensureCopilotContent inside the handler;
    // before any frame the chip reflects the default state.copilotAgentState.
    handleCopilotStatus({
      type: 'copilot_status',
      request_id: '',
      status: 'agent_state',
      agent_state: 'offline',
    });
    const chip = document.querySelector('.ktalk-copilot-agent-chip');
    expect(chip).not.toBeNull();
    expect(chip.classList.contains('offline')).toBe(true);
    expect(chip.textContent).toBe('⚫ offline');
  });

  test('agent_state chip renders every known lifecycle state', () => {
    const cases = [
      { agent_state: 'online', cls: 'online', text: '🟢 online' },
      { agent_state: 'working', cls: 'working', text: '🟡 working' },
      { agent_state: 'spawning', cls: 'working', text: '🟡 spawning' },
      { agent_state: 'blocked', cls: 'blocked', text: '🔴 blocked' },
      { agent_state: 'offline', cls: 'offline', text: '⚫ offline' },
    ];
    for (const c of cases) {
      handleCopilotStatus({
        type: 'copilot_status',
        request_id: '',
        status: 'agent_state',
        agent_state: c.agent_state,
      });
      const chip = document.querySelector('.ktalk-copilot-agent-chip');
      expect(chip.classList.contains(c.cls)).toBe(true);
      expect(chip.textContent).toBe(c.text);
      expect(state.copilotAgentState).toBe(c.agent_state);
    }
  });

  test('agent_state chip transitions swap the single active class', () => {
    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'working',
    });
    let chip = document.querySelector('.ktalk-copilot-agent-chip');
    expect(chip.classList.contains('working')).toBe(true);

    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'blocked',
    });
    chip = document.querySelector('.ktalk-copilot-agent-chip');
    expect(chip.classList.contains('blocked')).toBe(true);
    // Previous state's class must not linger — otherwise the chip color would
    // be ambiguous between working and blocked.
    expect(chip.classList.contains('working')).toBe(false);
  });

  test('agent_state offline frame resets the chip from a live state', () => {
    // Reset is driven by an explicit bridge-merge offline frame, NOT by
    // meeting_ended: the last-extension-gone path emits meeting_ended after
    // the extension is already gone, so the chip must not depend on it.
    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'online',
    });
    expect(document.querySelector('.ktalk-copilot-agent-chip').classList.contains('online')).toBe(true);

    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'offline',
    });
    const chip = document.querySelector('.ktalk-copilot-agent-chip');
    expect(chip.classList.contains('offline')).toBe(true);
    expect(chip.textContent).toBe('⚫ offline');
    expect(state.copilotAgentState).toBe('offline');
  });

  test('unknown agent_state value does not break the handler or change the chip', () => {
    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'online',
    });
    expect(state.copilotAgentState).toBe('online');

    // SDK drift could send an unrecognized token — the handler must ignore it
    // and keep the last known chip rather than throwing or blanking out.
    expect(() =>
      handleCopilotStatus({
        type: 'copilot_status',
        status: 'agent_state',
        agent_state: 'quantum-flux',
      })
    ).not.toThrow();
    const chip = document.querySelector('.ktalk-copilot-agent-chip');
    expect(state.copilotAgentState).toBe('online');
    expect(chip.classList.contains('online')).toBe(true);
    expect(chip.textContent).toBe('🟢 online');
  });

  test('agent_state does not touch the channel connectivity indicator', () => {
    // The chip is independent from state.copilotChannelStatus, which drives
    // the .ktalk-copilot-status label (channel / per-request health).
    state.copilotChannelStatus = 'connected';
    handleCopilotStatus({
      type: 'copilot_status',
      status: 'agent_state',
      agent_state: 'blocked',
    });
    expect(state.copilotChannelStatus).toBe('connected');
    const indicator = document.querySelector('.ktalk-copilot-status');
    expect(indicator.classList.contains('connected')).toBe(true);
  });

  test('handleCopilotStatus channel_disconnected without request_id flips all pending placeholders', () => {
    // Bridge emits this signal when the LAST channel client drops mid-flight.
    // Multiple placeholders may be in-flight (concurrent chat turns); without
    // the all-pending fan-out, only the per-request signal path would fire
    // and we'd get a single error bubble while every placeholder stays on
    // "Жду ответа Claude…" forever.
    state.copilotMessages = [
      { id: 'a1', kind: 'assistant', text: '…', request_id: 'req-1', status: 'working', time: '10:00:00' },
      { id: 'a2', kind: 'assistant', text: '…', request_id: 'req-2', status: 'working', time: '10:00:01' },
    ];
    state.copilotPending.set('req-1', { userId: null, assistantId: 'a1', createdAt: Date.now() });
    state.copilotPending.set('req-2', { userId: null, assistantId: 'a2', createdAt: Date.now() });
    state.copilotMessages.forEach(renderCopilotMessage);

    handleCopilotStatus({
      type: 'copilot_status',
      request_id: '',
      status: 'channel_disconnected',
    });

    expect(state.copilotPending.size).toBe(0);
    const a1 = state.copilotMessages.find((m) => m.id === 'a1');
    const a2 = state.copilotMessages.find((m) => m.id === 'a2');
    expect(a1.kind).toBe('error');
    expect(a2.kind).toBe('error');
    expect(a1.text).toMatch(/offline/i);
    expect(a2.text).toMatch(/offline/i);
    expect(state.copilotChannelStatus).toBe('channel_disconnected');
  });
});
