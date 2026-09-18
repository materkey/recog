/** @jest-environment-options {"url": "https://example.ktalk.ru/artifacts/records"} */
import { jest } from '@jest/globals';

await jest.unstable_mockModule('./ws.js', () => ({ connectWebSocket: jest.fn() }));
await jest.unstable_mockModule('./platforms/index.js', () => ({
  getPlatform: () => 'ktalk',
  getPlatformDisplayName: () => 'KTalk',
  isJitsiPlatform: () => false,
  getPlatformAdapter: () => ({
    supportsTabInjection: () => true,
    isConferenceAvailable: () => false,
    waitForConference: () => new Promise(() => {}),
  }),
}));
const { state } = await import('./state.js');
const { addAiTab, getKtalkChatTabs, removeKtalkUi } = await import('./ui.js');
const { init } = await import('./init.js');
const chat = '<div class="slide-menu-wrapper"><div class="tl-tabset__content"><div class="tl-tab">General</div></div><div class="content"><chat-widget class="native-chat">Messages</chat-widget></div></div>';
const members = '<div class="slide-menu-wrapper"><div class="tl-tabset__content"><div class="tl-tab">Members</div><div class="tl-tab">Moderators</div></div><div class="content"><conference-members-modal>People</conference-members-modal></div></div>';
let observers;

beforeEach(() => {
  jest.useFakeTimers();
  document.body.innerHTML = '';
  delete window.$conf;
  state.originalChatContent = null;
  state.transcriptContentElement = null;
  state.copilotContentElement = null;
  state.debugPanelElement = null;
  state.lastAiView = 'transcript';
  state.isCopilotActive = false;
  state.wsConnected = false;
  observers = [];
  const Observer = window.MutationObserver;
  jest.spyOn(window, 'MutationObserver').mockImplementation(callback => {
    const observer = new Observer(callback);
    observers.push(observer);
    return observer;
  });
});

afterEach(() => {
  observers.forEach(observer => observer.disconnect());
  jest.restoreAllMocks();
  jest.clearAllTimers();
  jest.useRealTimers();
});

test('artifact tabs without a meeting - periodic recovery - never adds Recog', async () => {
  document.body.innerHTML = '<div class="tl-tabset__content">All / My records</div>';
  init();
  jest.advanceTimersByTime(4000);
  await Promise.resolve();
  expect(getKtalkChatTabs()).toBeNull();
  expect(document.querySelector('.ktalk-ai-tab')).toBeNull();
  expect(document.querySelector('.ktalk-ai-panel')).toBeNull();
});

test('chat-shaped sidebar without a conference - tab injection - is refused', () => {
  document.body.innerHTML = chat;
  expect(addAiTab()).toBe(false);
  expect(document.querySelector('.ktalk-ai-tab')).toBeNull();
});

test('conference without chat content - tab injection - does not create an empty destination', () => {
  window.$conf = { jitsiConf: {} };
  document.body.innerHTML = '<div class="slide-menu-wrapper"><div class="tl-tabset__content"></div></div>';
  expect(addAiTab()).toBe(false);
});

test('members sidebar in a conference - periodic recovery - never adds Recog', async () => {
  window.$conf = { jitsiConf: {} };
  document.body.innerHTML = members;
  init();
  jest.advanceTimersByTime(4000);
  await Promise.resolve();
  expect(getKtalkChatTabs()).toBeNull();
  expect(document.querySelector('.ktalk-ai-tab')).toBeNull();
  expect(document.querySelector('.ktalk-ai-panel')).toBeNull();
});

test('chat replaced by members - recovery - removes Recog tab and panel', async () => {
  window.$conf = { jitsiConf: {} };
  document.body.innerHTML = chat;
  init();
  jest.advanceTimersByTime(2000);
  await Promise.resolve();
  expect(document.querySelector('.ktalk-ai-tab')).not.toBeNull();
  const wrapper = document.querySelector('.slide-menu-wrapper');
  wrapper.querySelector('.tl-tab:not(.ktalk-ai-tab-wrapper)').textContent = 'Members';
  wrapper.querySelector('chat-widget').replaceWith(document.createElement('conference-members-modal'));
  jest.advanceTimersByTime(2000);
  await Promise.resolve();
  expect(document.querySelector('.ktalk-ai-tab')).toBeNull();
  expect(document.querySelector('.ktalk-ai-panel')).toBeNull();
});

test('meeting chat alongside artifact tabs - recovery and click - opens content only inside chat', async () => {
  window.$conf = { jitsiConf: {} };
  document.body.innerHTML = '<div id="artifacts" class="tl-tabset__content">All</div>' + chat;
  init();
  jest.advanceTimersByTime(2000);
  await Promise.resolve();
  expect(document.querySelector('#artifacts .ktalk-ai-tab')).toBeNull();
  const tab = document.querySelector('.slide-menu-wrapper .ktalk-ai-tab');
  expect(tab).not.toBeNull();
  tab.click();
  expect(document.querySelector('.ktalk-ai-panel.active .ktalk-transcript-content.active')).not.toBeNull();
  expect(document.querySelector('.native-chat').style.display).toBe('none');
  delete window.$conf;
  jest.advanceTimersByTime(2000);
  await Promise.resolve();
  expect(document.querySelector('.ktalk-ai-tab')).toBeNull();
  expect(document.querySelector('.ktalk-ai-panel')).toBeNull();
  expect(document.querySelector('.native-chat').style.display).toBe('');
  expect(state.transcriptContentElement).toBeNull();
});

test('leaving then joining another meeting - DOM recovery - creates one working tab again', async () => {
  init();
  window.$conf = { jitsiConf: {} };
  document.body.innerHTML = chat;
  await Promise.resolve();
  expect(document.querySelectorAll('.ktalk-ai-tab')).toHaveLength(1);
  delete window.$conf;
  document.body.innerHTML = '<div class="tl-tabset__content">Recordings</div>';
  jest.advanceTimersByTime(2000);
  await Promise.resolve();
  expect(document.querySelector('.ktalk-ai-tab')).toBeNull();
  window.$conf = { jitsiConf: {} };
  document.body.innerHTML = chat;
  await Promise.resolve();
  const tab = document.querySelector('.ktalk-ai-tab');
  expect(document.querySelectorAll('.ktalk-ai-tab')).toHaveLength(1);
  tab.click();
  expect(document.querySelector('.ktalk-transcript-content.active')).not.toBeNull();
});

test('stale injected artifact tab - recovery - removes the orphaned control', async () => {
  document.body.innerHTML = '<div class="tl-tabset__content"><div class="ktalk-ai-tab-wrapper"><button class="ktalk-ai-tab">Recog</button></div></div>';
  init();
  jest.advanceTimersByTime(2000);
  await Promise.resolve();
  expect(document.querySelector('.ktalk-ai-tab-wrapper')).toBeNull();
  removeKtalkUi();
  expect(document.querySelector('.tl-tabset__content')).not.toBeNull();
});
