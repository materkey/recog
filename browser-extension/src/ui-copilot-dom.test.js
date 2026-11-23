/**
 * The Copilot chat DOM on KTalk: message buttons, the input's send paths and
 * how a placeholder bubble follows the request's statuses.
 *
 * @jest-environment jsdom
 * @jest-environment-options {"url": "https://ktalk.ru/"}
 */

import { jest } from '@jest/globals';

let state;
let ui;

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
  ui = await import('./ui.js');
});

const realClipboard = navigator.clipboard;

function reset() {
  document.body.innerHTML = `
    <div class="slide-menu-wrapper">
      <div class="content"><chat-widget class="native-chat"></chat-widget></div>
    </div>
  `;
  state.copilotMessages = [];
  state.copilotPending = new Map();
  state.copilotProactiveMutedUntil = 0;
  state.copilotChannelStatus = 'unknown';
  state.copilotAgentState = 'offline';
  state.copilotLastProactiveAt = 0;
  state.copilotContentElement = null;
  state.transcriptContentElement = null;
  state.isCopilotActive = false;
  state.originalChatContent = null;
  state.transcriptLines = [];
  state.currentMeetingId = null;
  state.wsConnected = false;
  state.wsConnection = null;
  navigator.clipboard = realClipboard;
  navigator.clipboard.writeText.mockClear();
  navigator.clipboard.writeText.mockResolvedValue(undefined);
}

function connectWs(send = jest.fn()) {
  state.wsConnected = true;
  state.wsConnection = { readyState: WebSocket.OPEN, send };
  return send;
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const messages = () => [...document.querySelectorAll('.ktalk-copilot-messages .copilot-msg')];
const texts = () => messages().map((el) => el.querySelector('.copilot-msg-text').textContent);

function sendFromInput(content, text) {
  const input = content.querySelector('.ktalk-copilot-input');
  input.value = text;
  content.querySelector('.ktalk-copilot-send-btn').click();
  return input;
}

describe('Copilot message buttons', () => {
  beforeEach(reset);

  test('an assistant message - copy clicked - copies its text and flashes Copied', async () => {
    jest.useFakeTimers();
    try {
      const el = ui.renderCopilotMessage({ id: 'a1', kind: 'assistant', text: 'ответ', time: '10:00:00' });
      const btn = el.querySelector('.copilot-msg-copy-btn');

      btn.click();
      await Promise.resolve();
      await Promise.resolve();

      expect(navigator.clipboard.writeText).toHaveBeenCalledWith('ответ');
      expect(btn.textContent).toBe('Copied');
      expect(btn.classList.contains('copied')).toBe(true);
      jest.advanceTimersByTime(1500);
      expect(btn.textContent).toBe('Copy');
      expect(btn.classList.contains('copied')).toBe(false);
    } finally {
      jest.useRealTimers();
    }
  });

  test('a proactive message without text - copy clicked and refused - keeps the label', async () => {
    navigator.clipboard.writeText.mockRejectedValueOnce(new Error('denied'));
    const el = ui.renderCopilotMessage({ id: 'p1', kind: 'proactive' });
    const btn = el.querySelector('.copilot-msg-copy-btn');

    btn.click();
    await flush();

    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('');
    expect(btn.textContent).toBe('Copy');
  });

  test('no clipboard API - copy clicked - does nothing', () => {
    navigator.clipboard = undefined;
    const el = ui.renderCopilotMessage({ id: 'a1', kind: 'assistant', text: 'x' });

    expect(() => el.querySelector('.copilot-msg-copy-btn').click()).not.toThrow();
    expect(el.querySelector('.copilot-msg-copy-btn').textContent).toBe('Copy');
  });

  test('a pending message without text or time - rendered - shows an ellipsis and the current time', () => {
    const el = ui.renderCopilotMessage({ id: 'w1', kind: 'assistant', status: 'working', request_id: 'r1' });

    const text = el.querySelector('.copilot-msg-text');
    expect(text.textContent).toBe('…');
    expect(text.classList.contains('pending')).toBe(true);
    expect(el.dataset.copilotRequestId).toBe('r1');
    expect(el.querySelector('.copilot-msg-time').textContent).toMatch(/\d\d:\d\d:\d\d/);
  });

  test('a message of an unknown kind - rendered - labels it with the kind', () => {
    const el = ui.renderCopilotMessage({ kind: 'custom', text: 'x' });

    expect(el.querySelector('.copilot-msg-role').textContent).toBe('custom');
    expect(el.dataset.copilotId).toBeUndefined();
    expect(el.querySelector('.copilot-msg-actions')).toBeNull();
  });
});

describe('Copilot input', () => {
  beforeEach(reset);

  test('text typed - Enter pressed - sends it; Shift+Enter and other keys do not', () => {
    const send = connectWs();
    const content = ui.createCopilotContent();
    const input = content.querySelector('.ktalk-copilot-input');
    input.value = 'вопрос';

    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', shiftKey: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'a' }));
    expect(send).not.toHaveBeenCalled();

    const enter = new KeyboardEvent('keydown', { key: 'Enter', cancelable: true });
    input.dispatchEvent(enter);
    expect(enter.defaultPrevented).toBe(true);
    expect(send).toHaveBeenCalledTimes(1);
    expect(JSON.parse(send.mock.calls[0][0])).toMatchObject({ type: 'copilot_chat_message', text: 'вопрос' });
  });

  test('a blank input - send clicked - sends nothing and adds no bubble', () => {
    const send = connectWs();
    const content = ui.createCopilotContent();

    sendFromInput(content, '   ');

    expect(send).not.toHaveBeenCalled();
    expect(messages()).toHaveLength(0);
  });

  test('the bridge is offline - send clicked - shows an error and keeps the text', () => {
    const content = ui.createCopilotContent();

    const input = sendFromInput(content, 'вопрос');

    expect(texts()).toEqual(['Bridge offline: сообщение не отправлено.']);
    expect(messages()[0].classList.contains('copilot-msg-error')).toBe(true);
    expect(input.value).toBe('вопрос');
  });

  test('the socket is closing - send clicked - shows the offline error', () => {
    state.wsConnected = true;
    state.wsConnection = { readyState: WebSocket.CLOSING, send: jest.fn() };
    const content = ui.createCopilotContent();

    sendFromInput(content, 'вопрос');

    expect(texts()).toEqual(['Bridge offline: сообщение не отправлено.']);
    expect(state.wsConnection.send).not.toHaveBeenCalled();
  });

  test('the socket throws on send - send clicked - shows a send error and tracks nothing', () => {
    connectWs(jest.fn(() => { throw new Error('boom'); }));
    const content = ui.createCopilotContent();

    sendFromInput(content, 'вопрос');

    expect(texts()).toEqual(['Не удалось отправить сообщение в Copilot.']);
    expect(state.copilotPending.size).toBe(0);
  });

  test('the bridge is up - send clicked - shows the question and a waiting placeholder', () => {
    const send = connectWs();
    const content = ui.createCopilotContent();

    const input = sendFromInput(content, '  вопрос  ');

    const frame = JSON.parse(send.mock.calls[0][0]);
    expect(frame.text).toBe('вопрос');
    expect(texts()).toEqual(['вопрос', 'Жду ответа Claude…']);
    const [user, placeholder] = messages();
    expect(user.classList.contains('copilot-msg-user')).toBe(true);
    expect(placeholder.querySelector('.copilot-msg-text').classList.contains('pending')).toBe(true);
    expect(user.dataset.copilotRequestId).toBe(frame.request_id);
    expect([...state.copilotPending.keys()]).toEqual([frame.request_id]);
    expect(input.value).toBe('');
  });

  test('transcript lines with missing fields - send clicked - sends the window with empty defaults', () => {
    const send = connectWs();
    const content = ui.createCopilotContent();
    state.transcriptLines = [
      { speaker: 'A', text: 'x', time: 't', segment_id: 3 },
      { timestamp: 'T' },
      {},
    ];

    sendFromInput(content, 'вопрос');

    expect(JSON.parse(send.mock.calls[0][0]).transcript_window).toEqual([
      { speaker: 'A', text: 'x', timestamp: 't', segment_id: 3 },
      { speaker: '', text: '', timestamp: 'T', segment_id: 0 },
      { speaker: '', text: '', timestamp: '', segment_id: 0 },
    ]);
  });

  test('transcript lines that are not a list - send clicked - sends an empty window', () => {
    const send = connectWs();
    const content = ui.createCopilotContent();
    state.transcriptLines = null;

    sendFromInput(content, 'вопрос');

    expect(JSON.parse(send.mock.calls[0][0]).transcript_window).toEqual([]);
  });
});

describe('Copilot placeholder updates', () => {
  let requestId;

  beforeEach(() => {
    reset();
    const send = connectWs();
    const content = ui.createCopilotContent();
    sendFromInput(content, 'вопрос');
    requestId = JSON.parse(send.mock.calls[0][0]).request_id;
  });

  const placeholder = () => messages()[1];

  test('a working status - received - says Claude is thinking and stays pending', () => {
    ui.handleCopilotStatus({ status: 'working', request_id: requestId });

    expect(placeholder().querySelector('.copilot-msg-text').textContent).toBe('Claude думает…');
    expect(placeholder().querySelector('.copilot-msg-text').classList.contains('pending')).toBe(true);
  });

  test('another status - received - shows it and drops the pending look', () => {
    ui.handleCopilotStatus({ status: 'queued', request_id: requestId });

    expect(placeholder().querySelector('.copilot-msg-text').textContent).toBe('Status: queued');
    expect(placeholder().querySelector('.copilot-msg-text').classList.contains('pending')).toBe(false);
  });

  test('a status frame without a status - received - shows it as unknown', () => {
    ui.handleCopilotStatus({ request_id: requestId });

    expect(placeholder().querySelector('.copilot-msg-text').textContent).toBe('Status: unknown');
  });

  test('a status for another request - received - leaves the placeholder', () => {
    ui.handleCopilotStatus({ status: 'working', request_id: 'other' });
    ui.handleCopilotStatus(null);

    expect(placeholder().querySelector('.copilot-msg-text').textContent).toBe('Жду ответа Claude…');
  });

  test('a done status with a message - received - turns the placeholder into a status without buttons', () => {
    ui.handleCopilotStatus({ status: 'done', request_id: requestId, message: 'готово' });

    const el = placeholder();
    expect(el.classList.contains('copilot-msg-status')).toBe(true);
    expect(el.classList.contains('copilot-msg-assistant')).toBe(false);
    expect(el.querySelector('.copilot-msg-role').textContent).toBe('Status');
    expect(el.querySelector('.copilot-msg-text').textContent).toBe('готово');
    expect(el.querySelector('.copilot-msg-actions')).toBeNull();
    expect(state.copilotPending.size).toBe(0);
  });

  test('a done status for an unknown request - received - adds nothing', () => {
    ui.handleCopilotStatus({ status: 'done', request_id: 'other' });

    expect(messages()).toHaveLength(2);
    expect(state.copilotPending.size).toBe(1);
  });

  test('an error status without a message - received - shows the default error', () => {
    ui.handleCopilotStatus({ status: 'error', request_id: requestId });

    expect(placeholder().querySelector('.copilot-msg-role').textContent).toBe('Error');
    expect(placeholder().querySelector('.copilot-msg-text').textContent).toBe('Claude не смог ответить.');
  });

  test('an error status for no request - received - appends an error bubble', () => {
    ui.handleCopilotStatus({ status: 'error', message: 'сбой' });

    expect(texts()).toEqual(['вопрос', 'Жду ответа Claude…', 'сбой']);
    expect(state.copilotPending.size).toBe(1);
  });

  test('channel offline with nothing pending - received - appends one offline bubble', () => {
    state.copilotPending.clear();

    ui.handleCopilotStatus({ status: 'channel_disconnected' });

    expect(texts()[2]).toBe('Channel offline: Claude не подключён.');
    expect(state.copilotChannelStatus).toBe('channel_disconnected');
  });

  test('a pending entry without a placeholder id - channel offline - is dropped silently', () => {
    state.copilotPending.set('ghost', {});

    ui.handleCopilotStatus({ status: 'channel_disconnected' });

    expect(state.copilotPending.size).toBe(0);
    expect(placeholder().querySelector('.copilot-msg-text').textContent).toBe('Channel offline: Claude не подключён.');
  });

  test('a reply - received - fills the placeholder and copy then copies the reply', async () => {
    ui.handleCopilotReply({ request_id: requestId, text: 'ответ' });

    const el = placeholder();
    expect(el.querySelector('.copilot-msg-text').textContent).toBe('ответ');
    expect(el.querySelector('.copilot-msg-text').classList.contains('pending')).toBe(false);
    expect(el.querySelectorAll('.copilot-msg-actions')).toHaveLength(1);
    el.querySelector('.copilot-msg-copy-btn').click();
    await flush();
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('ответ');
  });

  test('a bridge disconnect with a pending entry lacking its placeholder id - received - clears it', () => {
    state.copilotPending.set('ghost', null);

    ui.handleCopilotBridgeDisconnect();

    expect(state.copilotPending.size).toBe(0);
    expect(placeholder().querySelector('.copilot-msg-text').textContent).toBe('Bridge offline: соединение прервано.');
  });
});
