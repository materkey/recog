/**
 * Tests for the Copilot chat view inside the floating panel (Jitsi / matrix
 * platforms). The panel reuses the generic ui.js copilot subsystem
 * (renderCopilotMessage / handleCopilotReply / handleCopilotStatus /
 * submitCopilotInput) by mounting the `.ktalk-copilot-content` DOM into the
 * floating panel instead of the KTalk tab. This file exercises the
 * floating-panel-specific pieces: the Transcript | Copilot view toggle, the
 * send path (sendCopilotChatMessage with meeting context), reply rendering,
 * the agent-status chip, and pending/unread bookkeeping.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://element.example.org/widgets/element-call/index.html"}
 */

import { jest } from '@jest/globals';

let state;
let createCopilotContent;
let handleCopilotReply;
let handleCopilotStatus;
let initPlatformUI;
let ensureVas3kCopilotHost;
let setVas3kView;

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
  ({ createCopilotContent, handleCopilotReply, handleCopilotStatus, initPlatformUI } = await import('./ui.js'));
  ({ ensureVas3kCopilotHost, setVas3kView } = await import('./ui-vas3k.js'));
});

function resetCopilotState() {
  state.copilotMessages = [];
  state.copilotPending = new Map();
  state.copilotProactiveMutedUntil = 0;
  state.copilotChannelStatus = 'unknown';
  state.copilotAgentState = 'offline';
  state.copilotLastProactiveAt = 0;
  state.copilotContentElement = null;
  state.isCopilotActive = false;
  state.copilotUnread = false;
  state.vas3kActiveView = 'transcript';
  state.transcriptLines = [];
  state.currentMeetingId = null;
}

function connectFakeWs() {
  const send = jest.fn();
  state.wsConnected = true;
  state.wsConnection = { readyState: WebSocket.OPEN, send };
  return send;
}

describe('Floating-panel Copilot view', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    // Drop any style tags injected by a prior test so the guards re-run.
    document.head.querySelectorAll('style').forEach((s) => s.remove());
    resetCopilotState();
    connectFakeWs();
  });

  test('createCopilotContent mounts the copilot DOM inside the floating panel, not the KTalk tab', () => {
    initPlatformUI();
    const el = createCopilotContent();
    expect(el).not.toBeNull();
    expect(el.classList.contains('ktalk-copilot-content')).toBe(true);
    // It must live inside the floating panel host, never the KTalk slide-menu.
    const panel = document.querySelector('.jitsi-transcript-panel');
    expect(panel).not.toBeNull();
    expect(panel.contains(el)).toBe(true);
    expect(document.querySelector('.slide-menu-wrapper')).toBeNull();
    // Input + send + chip come along from the shared template.
    expect(el.querySelector('.ktalk-copilot-input')).not.toBeNull();
    expect(el.querySelector('.ktalk-copilot-send-btn')).not.toBeNull();
    expect(el.querySelector('.ktalk-copilot-agent-chip')).not.toBeNull();
  });

  test('Transcript | Copilot toggle shows/hides the two views', () => {
    initPlatformUI();
    createCopilotContent();
    const panel = document.querySelector('.jitsi-transcript-panel');
    const transcript = panel.querySelector('.jitsi-transcript-content');
    const host = panel.querySelector('.jitsi-copilot-host');
    const copilotContent = host.querySelector('.ktalk-copilot-content');

    // Default view is transcript.
    setVas3kView('transcript');
    expect(host.style.display).toBe('none');
    expect(transcript.style.display).not.toBe('none');
    expect(copilotContent.classList.contains('active')).toBe(false);
    expect(state.isCopilotActive).toBe(false);

    setVas3kView('copilot');
    expect(host.style.display).not.toBe('none');
    expect(transcript.style.display).toBe('none');
    expect(copilotContent.classList.contains('active')).toBe(true);
    expect(state.isCopilotActive).toBe(true);
  });

  test('clicking the Copilot tab button switches to the copilot view', () => {
    initPlatformUI();
    const panel = document.querySelector('.jitsi-transcript-panel');
    const copilotTab = panel.querySelector('.jitsi-panel-tab[data-view="copilot"]');
    expect(copilotTab).not.toBeNull();

    copilotTab.click();

    expect(state.vas3kActiveView).toBe('copilot');
    expect(state.isCopilotActive).toBe(true);
    // Content is created on activation so the user can type immediately.
    const host = panel.querySelector('.jitsi-copilot-host');
    expect(host.querySelector('.ktalk-copilot-content')).not.toBeNull();
  });

  test('submitting input sends copilot_chat_message with meeting context', () => {
    const send = connectFakeWs();
    initPlatformUI();
    const el = createCopilotContent();
    state.currentMeetingId = 'meeting-42';
    document.title = 'Element spike call';
    state.transcriptLines = [
      { speaker: 'Alice', text: 'привет', time: '10:00:00', segment_id: 3 },
      { speaker: 'Bob', text: 'как дела', time: '10:00:05', segment_id: 4 },
    ];

    const input = el.querySelector('.ktalk-copilot-input');
    input.value = 'подытожь звонок';
    el.querySelector('.ktalk-copilot-send-btn').click();

    const chatCall = send.mock.calls
      .map((c) => JSON.parse(c[0]))
      .find((m) => m.type === 'copilot_chat_message');
    expect(chatCall).toBeDefined();
    expect(chatCall.text).toBe('подытожь звонок');
    expect(chatCall.meeting_id).toBe('meeting-42');
    expect(chatCall.meeting_title).toBe('Element spike call');
    expect(chatCall.transcript_window).toHaveLength(2);
    // A pending placeholder is tracked so the reply can resolve it.
    expect(state.copilotPending.size).toBe(1);
    // Input is cleared after send.
    expect(input.value).toBe('');
  });

  test('copilot_reply resolves the pending placeholder in the panel', () => {
    const send = connectFakeWs();
    initPlatformUI();
    const el = createCopilotContent();
    const input = el.querySelector('.ktalk-copilot-input');
    input.value = 'вопрос';
    el.querySelector('.ktalk-copilot-send-btn').click();

    const chatCall = send.mock.calls
      .map((c) => JSON.parse(c[0]))
      .find((m) => m.type === 'copilot_chat_message');
    const requestId = chatCall.request_id;

    handleCopilotReply({
      type: 'copilot_reply',
      request_id: requestId,
      text: 'ответ Claude',
      kind: 'assistant',
    });

    expect(state.copilotPending.has(requestId)).toBe(false);
    const texts = [...el.querySelectorAll('.copilot-msg-assistant .copilot-msg-text')].map((n) => n.textContent);
    expect(texts).toContain('ответ Claude');
  });

  test('copilot_reply renders a fresh assistant bubble when no pending request', () => {
    initPlatformUI();
    const el = createCopilotContent();

    handleCopilotReply({
      type: 'copilot_reply',
      request_id: 'unmatched',
      text: 'внезапный ответ',
      kind: 'assistant',
    });

    const bubble = el.querySelector('.copilot-msg-assistant .copilot-msg-text');
    expect(bubble).not.toBeNull();
    expect(bubble.textContent).toBe('внезапный ответ');
  });

  test('copilot_status agent_state drives the chip through every lifecycle state', () => {
    initPlatformUI();
    createCopilotContent();
    const cases = [
      { agent_state: 'online', cls: 'online', text: '🟢 online' },
      { agent_state: 'working', cls: 'working', text: '🟡 working' },
      { agent_state: 'blocked', cls: 'blocked', text: '🔴 blocked' },
      { agent_state: 'offline', cls: 'offline', text: '⚫ offline' },
    ];
    for (const c of cases) {
      handleCopilotStatus({ type: 'copilot_status', status: 'agent_state', agent_state: c.agent_state });
      const chip = document.querySelector('.jitsi-transcript-panel .ktalk-copilot-agent-chip');
      expect(chip.classList.contains(c.cls)).toBe(true);
      expect(chip.textContent).toBe(c.text);
    }
  });

  test('a proactive reply while on transcript view marks the Copilot tab unread; switching clears it', () => {
    initPlatformUI();
    createCopilotContent();
    // Stay on transcript view — copilot is not the active view.
    setVas3kView('transcript');
    expect(state.isCopilotActive).toBe(false);

    handleCopilotReply({ type: 'copilot_reply', kind: 'proactive', text: 'важная подсказка' });

    expect(state.copilotUnread).toBe(true);
    const dot = document.querySelector('.jitsi-panel-tab[data-view="copilot"] .ktalk-ai-unread');
    expect(dot).not.toBeNull();
    expect(dot.hidden).toBe(false);

    // Switching to the copilot view clears the unread marker.
    setVas3kView('copilot');
    expect(state.copilotUnread).toBe(false);
    expect(dot.hidden).toBe(true);
  });
});
