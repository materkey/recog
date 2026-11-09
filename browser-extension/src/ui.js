import { state, constants } from './state.js';
import { formatBioReadText, normalizeBioReadSettings, BIOREAD_LIMITS, BIOREAD_SETTINGS_KEY } from './bioread.js';
import { getUncertainSuffix, renderMarkdown, escapeHtml, toTrustedHtml } from './utils.js';
import { sendWsMessage, sendCopilotChatMessage } from './ws_api.js';
import { getPlatform, getPlatformAdapter, isJitsiPlatform } from './platforms/index.js';
import { initVas3kUI, updateVas3kWsIndicator, addVas3kTranscriptLine, clearVas3kTranscript, ensureVas3kCopilotHost, setVas3kCopilotActivateHandler } from './ui-vas3k.js';

let meetingTitle = document.title || '';
let bioReadHotkeyBound = false;
let bioReadPanelListenerBound = false;

export function injectStyles() {
  if (document.querySelector('#ktalk-transcript-styles')) return;

  const styles = document.createElement('style');
  styles.id = 'ktalk-transcript-styles';
  styles.textContent = `
      /* AI tab - exact match of KTalk's PRIVATE button */
      .ktalk-ai-tab-wrapper.tl-tab {
        display: inline-block !important;
        height: 32px;
        padding: 0;
        margin: 0;
      }

      .ktalk-ai-tab.link {
        display: block;
        height: 32px;
        padding: 0;
        margin: 0;
        cursor: pointer;
        position: relative;
        color: rgba(255, 255, 255, 0.6);
        font-size: 13px;
        font-weight: 600;
        font-family: "Lab Grotesque", Arial, Helvetica, "Tahoma sans-serif";
        text-transform: uppercase;
        line-height: 16px;
        letter-spacing: normal;
        border: none;
        border-radius: 6px;
        background: transparent;
        vertical-align: middle;
        transition: color 0.2s ease-in-out, background-color 0.15s ease-in-out;
      }
      .ktalk-ai-tab.link:hover {
        color: rgba(255, 255, 255, 0.85);
        background: rgba(255, 255, 255, 0.03);
      }
      .ktalk-ai-tab.link._active {
        color: rgba(255, 255, 255, 0.85);
        background: rgba(255, 255, 255, 0.05);
      }
      /* While the Recog tab is active, KTalk still marks its own tab (chat/private)
         as _active — the panel merely covers its content. Suppress that highlight
         so only one tab looks selected; it comes back as soon as Recog deactivates. */
      .tl-tabset__content:has(.ktalk-ai-tab._active) .tl-tab:not(.ktalk-ai-tab-wrapper) .link._active {
        background: transparent;
        color: rgba(255, 255, 255, 0.6);
      }
      .tl-tabset__content:has(.ktalk-ai-tab._active) .tl-tab:not(.ktalk-ai-tab-wrapper) .link._active:hover {
        background: rgba(255, 255, 255, 0.03);
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-ai-tab .tab-content {
        display: flex;
        align-items: center;
        height: 32px;
        line-height: 16px;
        padding: 8px 6px;
        gap: 5px;
      }
      .ktalk-ai-unread {
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: #4ade80;
        display: inline-block;
      }
      .ktalk-ai-unread[hidden] {
        display: none;
      }

      /* AI panel: sub-navigation + active view (Transcript | Copilot) */
      .ktalk-ai-panel {
        display: none;
        flex-direction: column;
        height: 100%;
        background: rgb(45, 45, 45);
      }
      .ktalk-ai-panel.active {
        display: flex;
      }
      .ktalk-ai-subnav {
        display: flex;
        gap: 4px;
        padding: 8px 12px;
        background: rgb(52, 52, 52);
        border-bottom: 1px solid rgba(255,255,255,0.1);
        flex: 0 0 auto;
      }
      .ktalk-ai-subtab {
        display: inline-flex;
        align-items: center;
        gap: 6px;
        padding: 0 12px;
        height: 26px;
        border: none;
        border-radius: 13px;
        background: transparent;
        color: rgba(255, 255, 255, 0.6);
        font-size: 12px;
        font-weight: 600;
        font-family: "Lab Grotesque", Arial, Helvetica, "Tahoma sans-serif";
        cursor: pointer;
        transition: background 0.15s, color 0.15s;
      }
      .ktalk-ai-subtab:hover {
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-ai-subtab.active {
        background: rgba(255, 255, 255, 0.12);
        color: rgba(255, 255, 255, 0.9);
      }
      .ktalk-ai-panel-body {
        flex: 1;
        min-height: 0;
        display: flex;
        flex-direction: column;
        position: relative;
      }

      /* Transcript content area - match KTalk's native chat style */
      .ktalk-transcript-content {
        display: none;
        flex-direction: column;
        height: 100%;
        background: rgb(45, 45, 45);
        color: rgba(255, 255, 255, 0.8);
        user-select: text;
        --bioread-tail-opacity: 0.6;
      }
      .ktalk-transcript-content.active {
        display: flex;
      }
      .ktalk-transcript-speaker-bar {
        display: flex;
        align-items: center;
        padding: 10px 16px;
        background: rgb(52, 52, 52);
        border-bottom: 1px solid rgba(255,255,255,0.1);
        gap: 8px;
      }
      .ktalk-speaker-dot {
        width: 10px;
        height: 10px;
        border-radius: 50%;
        background: #9ca3af;
        transition: background 0.3s;
      }
      .ktalk-speaker-dot.speaking {
        background: #4ade80;
        animation: ktalk-pulse 1.5s infinite;
      }
      @keyframes ktalk-pulse {
        0% { box-shadow: 0 0 0 0 rgba(74, 222, 128, 0.7); }
        70% { box-shadow: 0 0 0 6px rgba(74, 222, 128, 0); }
        100% { box-shadow: 0 0 0 0 rgba(74, 222, 128, 0); }
      }
      .ktalk-speaker-name {
        font-size: 13px;
        font-weight: 600;
      }
      .ktalk-transcript-toolbar {
        display: flex;
        align-items: center;
        justify-content: flex-start;
        padding: 8px 12px;
        background: rgb(52, 52, 52);
        border-bottom: 1px solid rgba(255,255,255,0.1);
        flex-wrap: wrap;
        gap: 6px;
        position: relative;
      }
      .ktalk-speaker-bar-nav {
        margin-left: auto;
        display: flex;
        gap: 6px;
      }
      .ktalk-transcript-scroll-btn {
        padding: 0 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s;
        height: 22px;
        line-height: 22px;
        white-space: nowrap;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 4px;
      }
      .ktalk-transcript-scroll-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-copy-btn {
        padding: 0 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s;
        height: 22px;
        line-height: 22px;
        white-space: nowrap;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 4px;
      }
      .ktalk-transcript-copy-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-copy-btn.copied {
        background: #22c55e;
      }
      .ktalk-transcript-privacy-btn {
        padding: 0 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s;
        height: 22px;
        line-height: 22px;
        white-space: nowrap;
      }
      .ktalk-transcript-privacy-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-privacy-btn.active {
        background: rgba(0, 138, 255, 0.8);
        color: #fff;
      }
      .ktalk-transcript-follow-btn {
        padding: 0 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s;
        min-width: 56px;
        height: 22px;
        line-height: 22px;
        white-space: nowrap;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 4px;
      }
      .ktalk-transcript-follow-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-follow-btn.active {
        background: #0ea5e9;
        color: #fff;
      }
      .ktalk-bioread-toggle,
      .ktalk-bioread-settings-btn {
        padding: 0 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s, color 0.15s;
        height: 22px;
        line-height: 22px;
        white-space: nowrap;
        display: inline-flex;
        align-items: center;
        justify-content: center;
      }
      .ktalk-bioread-toggle:hover,
      .ktalk-bioread-settings-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-bioread-toggle.active {
        background: rgba(0, 138, 255, 0.8);
        color: #fff;
      }
      .ktalk-bioread-settings-btn.active {
        background: rgba(255,255,255,0.25);
      }
      .ktalk-bioread-panel {
        position: absolute;
        top: calc(100% + 6px);
        right: 12px;
        min-width: 220px;
        padding: 10px 12px;
        display: none;
        flex-direction: column;
        gap: 8px;
        background: rgb(64, 63, 63);
        border: 1px solid rgba(255,255,255,0.1);
        border-radius: 8px;
        box-shadow: 0 12px 24px rgba(0,0,0,0.35);
        z-index: 5;
      }
      .ktalk-bioread-panel.active {
        display: flex;
      }
      .ktalk-bioread-panel-header {
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 0.6px;
        opacity: 0.6;
      }
      .ktalk-bioread-control {
        display: grid;
        grid-template-columns: 70px 1fr 40px;
        gap: 8px;
        align-items: center;
        font-size: 11px;
        color: rgba(255,255,255,0.75);
      }
      .ktalk-bioread-control input[type="range"] {
        width: 100%;
      }
      .ktalk-bioread-value {
        text-align: right;
        font-variant-numeric: tabular-nums;
        color: rgba(255,255,255,0.9);
      }

      /* Transcript messages */
      .ktalk-transcript-messages {
        flex: 1;
        overflow-y: auto;
        padding: 16px;
        display: flex;
        flex-direction: column;
        gap: 12px;
        user-select: text;
      }
      .ktalk-msg {
        background: rgba(255,255,255,0.05);
        border-radius: 8px;
        padding: 10px 12px;
        border-left: 3px solid transparent;
      }
      .ktalk-msg.local {
        border-left-color: #60a5fa;
      }
      .ktalk-msg.remote {
        border-left-color: #34d399;
      }
      /* Uncertain messages - yellow (local speaker echo) */
      .ktalk-msg.uncertain {
        border-left-color: #fbbf24;
      }
      .ktalk-msg.uncertain .ktalk-msg-speaker {
        color: #fbbf24;
      }
      /* Uncertain messages - orange (all mics off) */
      .ktalk-msg.uncertain.mics-off {
        border-left-color: #fb923c;
      }
      .ktalk-msg.uncertain.mics-off .ktalk-msg-speaker {
        color: #fb923c;
      }
      .ktalk-msg-header {
        display: flex;
        justify-content: space-between;
        margin-bottom: 4px;
        font-size: 12px;
      }
      .ktalk-msg-speaker {
        font-weight: 600;
        color: #fff;
      }
      .ktalk-msg-time {
        color: rgba(255,255,255,0.5);
      }
      .ktalk-msg-text {
        font-size: 13px;
        line-height: 1.4;
        user-select: text;
      }
      /* Tier-2 refine pulse: brief fade to hint that text was rewritten. */
      .ktalk-msg-text.refine-fade {
        transition: opacity 100ms;
        opacity: 0.3;
      }
      .bio-word {
        display: inline;
      }
      .bio-head {
        font-weight: 700;
      }
      .bio-tail {
        opacity: var(--bioread-tail-opacity, 0.6);
      }
      .ktalk-transcript-empty {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        height: 100%;
        color: rgba(255,255,255,0.4);
        text-align: center;
        padding: 20px;
      }
      .ktalk-transcript-empty-icon {
        font-size: 48px;
        margin-bottom: 16px;
        opacity: 0.4;
      }
      .ktalk-transcript-empty-text {
        font-size: 13px;
        line-height: 1.6;
      }
      .ktalk-transcript-empty-cmd {
        font-family: monospace;
        font-size: 11px;
        background: rgba(255,255,255,0.06);
        padding: 8px 12px;
        border-radius: 4px;
        margin-top: 12px;
      }

      /* Copilot panel */
      .ktalk-copilot-content {
        display: none;
        flex-direction: column;
        height: 100%;
        background: rgb(45, 45, 45);
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-copilot-content.active {
        display: flex;
      }
      .ktalk-copilot-header {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 10px 16px;
        background: rgb(52, 52, 52);
        border-bottom: 1px solid rgba(255, 255, 255, 0.1);
      }
      .ktalk-copilot-title {
        font-size: 13px;
        font-weight: 600;
      }
      .ktalk-copilot-status {
        font-size: 11px;
        color: rgba(255, 255, 255, 0.55);
        text-transform: uppercase;
        letter-spacing: 0.06em;
      }
      .ktalk-copilot-status.connected {
        color: #4ade80;
      }
      .ktalk-copilot-status.disconnected {
        color: #f87171;
      }
      /* Shared pill geometry for the header controls. The fixed height only
         works with a single-line label — a wrapped one spills outside the
         box — so nowrap belongs here, next to the height that requires it. */
      .ktalk-copilot-agent-chip,
      .ktalk-copilot-mute-btn {
        font-size: 11px;
        padding: 0 8px;
        height: 22px;
        line-height: 22px;
        border-radius: 4px;
        white-space: nowrap;
        flex: 0 0 auto;
      }
      /* Agent lifecycle chip — reflects the auto-spawned copilot session
         health (supervisor screen state merged with daemon MCP-session
         count in the bridge). Independent from .ktalk-copilot-status, which
         describes channel / per-request health. */
      .ktalk-copilot-agent-chip {
        background: rgba(255, 255, 255, 0.06);
        color: rgba(255, 255, 255, 0.75);
      }
      .ktalk-copilot-agent-chip.online {
        color: #4ade80;
      }
      .ktalk-copilot-agent-chip.working {
        color: #fbbf24;
      }
      .ktalk-copilot-agent-chip.blocked {
        color: #f87171;
      }
      .ktalk-copilot-agent-chip.offline {
        color: rgba(255, 255, 255, 0.5);
      }
      .ktalk-copilot-actions {
        margin-left: auto;
        display: flex;
        align-items: center;
        gap: 8px;
      }
      .ktalk-copilot-mute-btn {
        background: rgba(255, 255, 255, 0.1);
        border: 1px solid rgba(255, 255, 255, 0.2);
        color: rgba(255, 255, 255, 0.85);
        cursor: pointer;
      }
      .ktalk-copilot-mute-btn:hover {
        background: rgba(255, 255, 255, 0.2);
      }
      .ktalk-copilot-mute-btn.active {
        background: rgba(248, 113, 113, 0.2);
        border-color: rgba(248, 113, 113, 0.6);
        color: #f87171;
      }
      .ktalk-copilot-messages {
        flex: 1;
        overflow-y: auto;
        padding: 12px 16px;
        display: flex;
        flex-direction: column;
        gap: 10px;
        user-select: text;
      }
      .ktalk-copilot-empty {
        color: rgba(255, 255, 255, 0.4);
        text-align: center;
        font-size: 12px;
        padding: 32px 16px;
      }
      .copilot-msg {
        background: rgba(255, 255, 255, 0.05);
        border-radius: 8px;
        padding: 8px 12px;
        border-left: 3px solid transparent;
        font-size: 13px;
        line-height: 1.4;
      }
      .copilot-msg-user {
        border-left-color: #60a5fa;
      }
      .copilot-msg-assistant {
        border-left-color: #34d399;
      }
      .copilot-msg-proactive {
        border-left-color: #f59e0b;
        background: rgba(245, 158, 11, 0.08);
      }
      .copilot-msg-status {
        border-left-color: rgba(255, 255, 255, 0.3);
        background: rgba(255, 255, 255, 0.03);
        color: rgba(255, 255, 255, 0.6);
        font-style: italic;
        font-size: 12px;
      }
      .copilot-msg-error {
        border-left-color: #f87171;
        background: rgba(248, 113, 113, 0.08);
        color: #fca5a5;
      }
      .copilot-msg-header {
        display: flex;
        align-items: center;
        gap: 8px;
        font-size: 11px;
        margin-bottom: 4px;
        color: rgba(255, 255, 255, 0.6);
      }
      .copilot-msg-role {
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.06em;
      }
      .copilot-msg-time {
        margin-left: auto;
        font-variant-numeric: tabular-nums;
      }
      .copilot-msg-text {
        color: rgba(255, 255, 255, 0.9);
        white-space: pre-wrap;
        word-break: break-word;
        user-select: text;
      }
      .copilot-msg-text.pending {
        color: rgba(255, 255, 255, 0.5);
        font-style: italic;
      }
      .copilot-msg-actions {
        display: flex;
        gap: 6px;
        margin-top: 6px;
      }
      .copilot-msg-action-btn {
        padding: 0 6px;
        background: rgba(255, 255, 255, 0.08);
        border: 1px solid rgba(255, 255, 255, 0.15);
        border-radius: 4px;
        color: rgba(255, 255, 255, 0.7);
        font-size: 10px;
        cursor: pointer;
        height: 20px;
        line-height: 20px;
      }
      .copilot-msg-action-btn:hover {
        background: rgba(255, 255, 255, 0.16);
        color: rgba(255, 255, 255, 0.95);
      }
      .copilot-msg-action-btn.copied {
        background: #22c55e;
        border-color: #22c55e;
        color: #fff;
      }
      .ktalk-copilot-input-area {
        display: flex;
        flex-direction: column;
        gap: 6px;
        padding: 10px 12px;
        background: rgb(52, 52, 52);
        border-top: 1px solid rgba(255, 255, 255, 0.1);
      }
      .ktalk-copilot-input {
        width: 100%;
        min-height: 56px;
        max-height: 160px;
        padding: 8px 10px;
        background: rgba(255, 255, 255, 0.08);
        border: 1px solid rgba(255, 255, 255, 0.15);
        border-radius: 6px;
        color: #fff;
        font-size: 13px;
        font-family: inherit;
        resize: vertical;
        box-sizing: border-box;
      }
      .ktalk-copilot-input:focus {
        outline: none;
        border-color: #60a5fa;
      }
      .ktalk-copilot-input:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }
      .ktalk-copilot-input-row {
        display: flex;
        justify-content: flex-end;
        gap: 6px;
      }
      .ktalk-copilot-send-btn {
        padding: 6px 14px;
        background: #60a5fa;
        border: none;
        border-radius: 6px;
        color: #fff;
        font-size: 12px;
        font-weight: 600;
        cursor: pointer;
      }
      .ktalk-copilot-send-btn:hover {
        background: #3b82f6;
      }
      .ktalk-copilot-send-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }

    `;
  document.head.appendChild(styles);
}

// The Members sidebar reuses .slide-menu-wrapper with its own tabset and
// .content (one of them in the header), so only chat-widget identifies the chat.
function getKtalkChatContentArea() {
  return document.querySelector('.slide-menu-wrapper chat-widget')?.parentElement ?? null;
}

export function getKtalkChatTabs() {
  if (!window.$conf?.jitsiConf) return null;
  const chat = document.querySelector('.slide-menu-wrapper');
  if (!chat?.querySelector('chat-widget')) return null;
  return chat.querySelector('.tl-tabset__content');
}

export function removeKtalkUi() {
  deactivateCustomTabs();
  document.querySelectorAll('.ktalk-ai-tab-wrapper, .ktalk-ai-panel').forEach(el => el.remove());
  state.transcriptContentElement = null;
  state.copilotContentElement = null;
  state.originalChatContent = null;
}

export function addAiTab() {
  const tabsContainer = getKtalkChatTabs();
  if (!tabsContainer) {
    return false;
  }
  if (tabsContainer.querySelector('.ktalk-ai-tab')) return true;

  console.log('[KTalk Transcript] Adding AI tab to tabset');

  const tab = document.createElement('div');
  tab.className = 'tl-tab _switchable ktalk-ai-tab-wrapper';
  tab.innerHTML = toTrustedHtml(`
      <button type="button" class="ktalk-ai-tab link">
        <span class="tab-content">
          <span class="title">Recog</span>
          <span class="ktalk-ai-unread" hidden></span>
        </span>
      </button>
    `);

  tab.querySelector('.ktalk-ai-unread').hidden = !state.copilotUnread;
  tabsContainer.appendChild(tab);

  tab.querySelector('button').addEventListener('click', () => {
    activateSubView(state.lastAiView);
  });

  return true;
}

function captureOriginalChatContent(contentArea) {
  if (state.originalChatContent && document.body.contains(state.originalChatContent)) return;
  const first = contentArea.firstElementChild;
  if (first && !first.classList.contains('ktalk-ai-panel')) {
    state.originalChatContent = first;
  }
}

function activateSubView(view) {
  if (view === 'copilot') {
    activateCopilotTab();
  } else {
    setActiveView('transcript');
  }
}

function ensureAiPanelBody(contentArea) {
  if (!contentArea) return null;

  captureOriginalChatContent(contentArea);

  let panel = contentArea.querySelector('.ktalk-ai-panel');
  if (!panel) {
    panel = document.createElement('div');
    panel.className = 'ktalk-ai-panel';
    panel.innerHTML = toTrustedHtml(`
        <div class="ktalk-ai-subnav">
          <button type="button" class="ktalk-ai-subtab" data-view="transcript">Transcript</button>
          <button type="button" class="ktalk-ai-subtab" data-view="copilot">Copilot<span class="ktalk-ai-unread" hidden></span></button>
        </div>
        <div class="ktalk-ai-panel-body"></div>
      `);
    panel.querySelectorAll('.ktalk-ai-subtab').forEach((btn) => {
      btn.addEventListener('click', () => activateSubView(btn.dataset.view));
    });
    panel.querySelector('.ktalk-ai-unread').hidden = !state.copilotUnread;
    contentArea.appendChild(panel);
  }
  return panel.querySelector('.ktalk-ai-panel-body');
}

export function createTranscriptContent() {
  if (document.querySelector('.ktalk-transcript-content')) {
    return document.querySelector('.ktalk-transcript-content');
  }

  const contentArea = getKtalkChatContentArea();
  if (!contentArea) {
    console.log('[KTalk Transcript] Content area not found');
    return null;
  }

  const panelBody = ensureAiPanelBody(contentArea);
  if (!panelBody) return null;

  state.transcriptContentElement = document.createElement('div');
  state.transcriptContentElement.className = 'ktalk-transcript-content';
  loadBioReadSettings();
  state.transcriptContentElement.innerHTML = toTrustedHtml(`
      <div class="ktalk-transcript-speaker-bar">
        <div class="ktalk-speaker-dot"></div>
        <span class="ktalk-speaker-name">No one speaking</span>
        <div class="ktalk-speaker-bar-nav">
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-top" title="Scroll to top">↑</button>
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-bottom" title="Scroll to bottom">↓</button>
        </div>
      </div>
      <div class="ktalk-transcript-toolbar">
        <button class="ktalk-transcript-follow-btn" title="Auto-follow latest">Follow</button>
        <button class="ktalk-transcript-copy-btn" title="Copy all transcript">Copy</button>
        <button class="ktalk-transcript-privacy-btn" title="Replace names with speaker_N when copying">Privacy</button>
        <button class="ktalk-bioread-toggle" title="BioRead">Bio</button>
        <button class="ktalk-bioread-settings-btn" title="BioRead settings">⚙</button>
        <div class="ktalk-bioread-panel">
          <div class="ktalk-bioread-panel-header">BioRead</div>
          <label class="ktalk-bioread-control">
            <span>Fixation</span>
            <input
              type="range"
              class="ktalk-bioread-fixation"
              min="${BIOREAD_LIMITS.fixation.min}"
              max="${BIOREAD_LIMITS.fixation.max}"
              step="${BIOREAD_LIMITS.fixation.step}"
              value="${state.bioRead.fixation}"
            />
            <span class="ktalk-bioread-value" data-bioread-value="fixation"></span>
          </label>
          <label class="ktalk-bioread-control">
            <span>Contrast</span>
            <input
              type="range"
              class="ktalk-bioread-contrast"
              min="${BIOREAD_LIMITS.tailOpacity.min}"
              max="${BIOREAD_LIMITS.tailOpacity.max}"
              step="${BIOREAD_LIMITS.tailOpacity.step}"
              value="${state.bioRead.tailOpacity}"
            />
            <span class="ktalk-bioread-value" data-bioread-value="contrast"></span>
          </label>
        </div>
      </div>
      <div class="ktalk-transcript-messages">
        <div class="ktalk-transcript-empty">
          <div class="ktalk-transcript-empty-icon">📝</div>
          <div class="ktalk-transcript-empty-text">
            Waiting for transcript...<br>
            Start recog with:
          </div>
          <div class="ktalk-transcript-empty-cmd">
            ./recog-service.sh start
          </div>
        </div>
      </div>
    `);

  state.transcriptContentElement.querySelector('.ktalk-transcript-copy-btn').addEventListener('click', copyAllTranscript);
  const privacyBtn = state.transcriptContentElement.querySelector('.ktalk-transcript-privacy-btn');
  privacyBtn.classList.toggle('active', state.privacyMode);
  privacyBtn.addEventListener('click', () => {
    state.privacyMode = !state.privacyMode;
    localStorage.setItem('ktalk-privacy-mode', state.privacyMode);
    privacyBtn.classList.toggle('active', state.privacyMode);
  });
  state.transcriptContentElement.querySelector('.ktalk-transcript-follow-btn').addEventListener('click', toggleAutoFollow);
  updateFollowButton();
  state.transcriptContentElement.querySelector('.ktalk-scroll-top').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = 0;
  });
  state.transcriptContentElement.querySelector('.ktalk-scroll-bottom').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = container.scrollHeight;
  });

  setupBioReadControls(state.transcriptContentElement);

  panelBody.appendChild(state.transcriptContentElement);

  console.log('[KTalk Transcript] Content element created');
  return state.transcriptContentElement;
}

const COPILOT_PROACTIVE_MUTE_MS = 10 * 60 * 1000;
const COPILOT_TRANSCRIPT_WINDOW_SIZE = 20;

function nowDisplay() {
  const d = new Date();
  return d.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function buildCopilotTranscriptWindow() {
  if (!Array.isArray(state.transcriptLines)) return [];
  const tail = state.transcriptLines.slice(-COPILOT_TRANSCRIPT_WINDOW_SIZE);
  return tail.map((line) => ({
    speaker: line.speaker || '',
    text: line.text || '',
    timestamp: line.time || line.timestamp || '',
    segment_id: line.segment_id || 0
  }));
}

export function createCopilotContent() {
  if (document.querySelector('.ktalk-copilot-content')) {
    state.copilotContentElement = document.querySelector('.ktalk-copilot-content');
    return state.copilotContentElement;
  }

  let panelBody;
  if (isJitsiPlatform()) {
    // Off a room path (Meet /, /home, /landing) an early bridge frame must not
    // side-create the floating panel; the content is built once a room is open.
    if (!document.querySelector('.jitsi-transcript-panel') &&
        getPlatformAdapter()?.isRoomPage?.() === false) {
      return null;
    }
    // Floating-panel platforms (vas3k / matrix): mount the copilot chat inside
    // the panel's copilot host instead of the KTalk slide-menu tab. The shared
    // copilot message/chip styles live in ui.js injectStyles().
    injectStyles();
    // The panel may be created right here (ensureVas3kCopilotHost →
    // createFloatingPanel) by an early bridge frame — the copilot_status
    // primers arrive ~1 s after WS connect, before init.js's 2 s interval
    // ever runs setupUI()/initPlatformUI(). Register the activate handler
    // now so a Copilot tab click can always build/activate this content;
    // otherwise the tab switches views but the content stays display:none.
    setVas3kCopilotActivateHandler(activateVas3kView);
    panelBody = ensureVas3kCopilotHost();
  } else {
    const contentArea = getKtalkChatContentArea();
    if (!contentArea) {
      console.log('[KTalk Transcript] Content area not found for copilot');
      return null;
    }
    panelBody = ensureAiPanelBody(contentArea);
  }
  if (!panelBody) return null;

  state.copilotContentElement = document.createElement('div');
  state.copilotContentElement.className = 'ktalk-copilot-content';
  state.copilotContentElement.innerHTML = toTrustedHtml(`
      <div class="ktalk-copilot-header">
        <span class="ktalk-copilot-title">Copilot</span>
        <span class="ktalk-copilot-status">…</span>
        <div class="ktalk-copilot-actions">
          <span class="ktalk-copilot-agent-chip offline" title="Copilot session: offline">⚫ offline</span>
          <button type="button" class="ktalk-copilot-mute-btn" title="Mute proactive messages for 10 minutes"></button>
        </div>
      </div>
      <div class="ktalk-copilot-messages">
        <div class="ktalk-copilot-empty">Сообщений пока нет.</div>
      </div>
      <div class="ktalk-copilot-input-area">
        <textarea class="ktalk-copilot-input" rows="2" placeholder="Спросить Claude..."></textarea>
        <div class="ktalk-copilot-input-row">
          <button type="button" class="ktalk-copilot-send-btn">Send</button>
        </div>
      </div>
    `);

  const muteBtn = state.copilotContentElement.querySelector('.ktalk-copilot-mute-btn');
  muteBtn.addEventListener('click', () => toggleCopilotProactiveMute());
  refreshCopilotMuteButton();

  const input = state.copilotContentElement.querySelector('.ktalk-copilot-input');
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      submitCopilotInput();
    }
  });

  const sendBtn = state.copilotContentElement.querySelector('.ktalk-copilot-send-btn');
  sendBtn.addEventListener('click', () => submitCopilotInput());

  panelBody.appendChild(state.copilotContentElement);
  // Replay accumulated history: the host SPA (KTalk sidebar re-render on
  // channel switch, panel close/reopen) drops the injected DOM, but
  // state.copilotMessages survives — without a replay the rebuilt panel
  // shows the empty placeholder and past messages are never seen again.
  for (const msg of state.copilotMessages) {
    renderCopilotMessage(msg);
  }
  updateCopilotStatusIndicator();
  updateCopilotAgentChip();
  console.log('[KTalk Transcript] Copilot element created');
  return state.copilotContentElement;
}

function ensureCopilotContent() {
  if (!state.copilotContentElement || !document.body.contains(state.copilotContentElement)) {
    createCopilotContent();
  }
}

function getCopilotMessagesEl() {
  return state.copilotContentElement?.querySelector('.ktalk-copilot-messages') || null;
}

function getCopilotInputEl() {
  return state.copilotContentElement?.querySelector('.ktalk-copilot-input') || null;
}

function getCopilotSendBtn() {
  return state.copilotContentElement?.querySelector('.ktalk-copilot-send-btn') || null;
}

function refreshCopilotMuteButton() {
  const btn = state.copilotContentElement?.querySelector('.ktalk-copilot-mute-btn');
  if (!btn) return;
  const muted = state.copilotProactiveMutedUntil > Date.now();
  btn.classList.toggle('active', muted);
  btn.textContent = muted ? 'Unmute' : 'Mute 10m';
}

function toggleCopilotProactiveMute() {
  if (state.copilotProactiveMutedUntil > Date.now()) {
    state.copilotProactiveMutedUntil = 0;
  } else {
    state.copilotProactiveMutedUntil = Date.now() + COPILOT_PROACTIVE_MUTE_MS;
  }
  refreshCopilotMuteButton();
}

export function isCopilotProactiveMuted() {
  return state.copilotProactiveMutedUntil > Date.now();
}

function updateCopilotStatusIndicator() {
  const el = state.copilotContentElement?.querySelector('.ktalk-copilot-status');
  if (!el) return;
  const status = state.copilotChannelStatus || 'unknown';
  el.classList.remove('connected', 'disconnected');
  if (status === 'connected') {
    el.classList.add('connected');
    el.textContent = 'connected';
  } else if (status === 'channel_disconnected' || status === 'disconnected') {
    el.classList.add('disconnected');
    el.textContent = 'channel offline';
  } else if (status === 'working') {
    el.textContent = 'working…';
  } else {
    el.textContent = status;
  }
}

// Agent lifecycle chip states (from the bridge merge — D4). `spawning` is a
// transitional working-variant so a cold start shows 🟡 rather than ⚫.
const COPILOT_AGENT_CHIP = {
  online: { icon: '🟢', label: 'online', cls: 'online' },
  working: { icon: '🟡', label: 'working', cls: 'working' },
  spawning: { icon: '🟡', label: 'spawning', cls: 'working' },
  blocked: { icon: '🔴', label: 'blocked', cls: 'blocked' },
  offline: { icon: '⚫', label: 'offline', cls: 'offline' },
};

function updateCopilotAgentChip() {
  const el = state.copilotContentElement?.querySelector('.ktalk-copilot-agent-chip');
  if (!el) return;
  const meta = COPILOT_AGENT_CHIP[state.copilotAgentState] || COPILOT_AGENT_CHIP.offline;
  el.classList.remove('online', 'working', 'blocked', 'offline');
  el.classList.add(meta.cls);
  el.textContent = `${meta.icon} ${meta.label}`;
  el.title = `Copilot session: ${meta.label}`;
}

function escapeCopilotText(value) {
  return escapeHtml(value == null ? '' : String(value));
}

function setCopilotInputBusy(busy) {
  // No-op by design: typing while a previous request is pending is
  // allowed. Each request carries its own request_id and is tracked in
  // state.copilotPending (a Map), so concurrent in-flight messages
  // don't conflict — replies resolve to their own placeholder by id.
  // The visual "pending" state is conveyed by the "Жду ответа Claude…"
  // placeholder rendered in the message list.
  void busy;
}

function clearCopilotEmptyPlaceholder() {
  const messagesEl = getCopilotMessagesEl();
  if (!messagesEl) return;
  const empty = messagesEl.querySelector('.ktalk-copilot-empty');
  if (empty) empty.remove();
}

function showCopilotEmptyIfNoneLeft() {
  const messagesEl = getCopilotMessagesEl();
  if (!messagesEl) return;
  if (state.copilotMessages.length === 0 && !messagesEl.querySelector('.ktalk-copilot-empty')) {
    const div = document.createElement('div');
    div.className = 'ktalk-copilot-empty';
    div.textContent = 'Сообщений пока нет.';
    messagesEl.appendChild(div);
  }
}

function scrollCopilotToBottom() {
  const messagesEl = getCopilotMessagesEl();
  if (!messagesEl) return;
  messagesEl.scrollTop = messagesEl.scrollHeight;
}

function getCopilotRoleLabel(kind) {
  if (kind === 'user') return 'Вы';
  if (kind === 'assistant') return 'Claude';
  if (kind === 'proactive') return 'Claude · proactive';
  if (kind === 'status') return 'Status';
  if (kind === 'error') return 'Error';
  return kind;
}

export function renderCopilotMessage(msg) {
  ensureCopilotContent();
  const messagesEl = getCopilotMessagesEl();
  if (!messagesEl) return null;
  clearCopilotEmptyPlaceholder();

  const wrapper = document.createElement('div');
  wrapper.className = `copilot-msg copilot-msg-${msg.kind}`;
  if (msg.id) wrapper.dataset.copilotId = msg.id;
  if (msg.request_id) wrapper.dataset.copilotRequestId = msg.request_id;

  const headerHtml = `
      <div class="copilot-msg-header">
        <span class="copilot-msg-role">${escapeCopilotText(getCopilotRoleLabel(msg.kind))}</span>
        <span class="copilot-msg-time">${escapeCopilotText(msg.time || nowDisplay())}</span>
      </div>
    `;
  const isPending = msg.status === 'working';
  const textHtml = `<div class="copilot-msg-text${isPending ? ' pending' : ''}">${escapeCopilotText(msg.text || (isPending ? '…' : ''))}</div>`;

  let actionsHtml = '';
  if (copilotKindHasActions(msg.kind)) {
    actionsHtml = `
        <div class="copilot-msg-actions">
          <button type="button" class="copilot-msg-action-btn copilot-msg-copy-btn" title="Copy">Copy</button>
          <button type="button" class="copilot-msg-action-btn copilot-msg-dismiss-btn" title="Dismiss">Dismiss</button>
        </div>
      `;
  }

  wrapper.innerHTML = toTrustedHtml(headerHtml + textHtml + actionsHtml);

  const copyBtn = wrapper.querySelector('.copilot-msg-copy-btn');
  if (copyBtn) {
    copyBtn.addEventListener('click', () => {
      const text = msg.text || '';
      if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
        navigator.clipboard.writeText(text).then(() => {
          copyBtn.classList.add('copied');
          copyBtn.textContent = 'Copied';
          setTimeout(() => {
            copyBtn.classList.remove('copied');
            copyBtn.textContent = 'Copy';
          }, 1500);
        }).catch((err) => {
          console.log('[KTalk Copilot] Copy failed', err);
        });
      }
    });
  }
  const dismissBtn = wrapper.querySelector('.copilot-msg-dismiss-btn');
  if (dismissBtn) {
    dismissBtn.addEventListener('click', () => dismissCopilotMessage(msg.id));
  }

  messagesEl.appendChild(wrapper);
  scrollCopilotToBottom();
  return wrapper;
}

function appendCopilotMessage(msg) {
  // Render first, push only on success — on platforms without the
  // Copilot DOM (e.g. vas3k) `renderCopilotMessage` returns null and a
  // pre-render push would let `state.copilotMessages` accumulate
  // unbounded across every `meeting_started`/`meeting_ended` chip.
  const rendered = renderCopilotMessage(msg);
  if (!rendered) return;
  state.copilotMessages.push(msg);
  noteCopilotUnread(msg.kind);
}

function findCopilotMessageElement(id) {
  const messagesEl = getCopilotMessagesEl();
  if (!messagesEl || !id) return null;
  return messagesEl.querySelector(`[data-copilot-id="${id}"]`);
}

function dismissCopilotMessage(id) {
  state.copilotMessages = state.copilotMessages.filter((m) => m.id !== id);
  const el = findCopilotMessageElement(id);
  if (el) el.remove();
  showCopilotEmptyIfNoneLeft();
}

function copilotKindHasActions(kind) {
  return kind === 'assistant' || kind === 'proactive';
}

function setCopilotMessageText(el, text, status) {
  const textEl = el.querySelector('.copilot-msg-text');
  if (textEl) {
    textEl.textContent = text;
    textEl.classList.toggle('pending', status === 'working');
  }
}

function setCopilotMessageTime(el, time) {
  const timeEl = el.querySelector('.copilot-msg-time');
  if (timeEl) timeEl.textContent = time;
}

function setCopilotMessageKind(el, kind) {
  el.classList.remove(
    'copilot-msg-user',
    'copilot-msg-assistant',
    'copilot-msg-proactive',
    'copilot-msg-status',
    'copilot-msg-error'
  );
  el.classList.add(`copilot-msg-${kind}`);
  const roleEl = el.querySelector('.copilot-msg-role');
  if (roleEl) roleEl.textContent = getCopilotRoleLabel(kind);
  // Drop the copy/dismiss row when the kind flips to one that does not
  // own actions (status/error/user), so an assistant bubble downgraded
  // to error does not keep a Copy button that copies stale text. A
  // placeholder never flips back to assistant/proactive: every flip away
  // from assistant also settles its pending request.
  if (!copilotKindHasActions(kind)) {
    const existingActions = el.querySelector('.copilot-msg-actions');
    if (existingActions) existingActions.remove();
  }
}

function updateCopilotMessageDom(id, updates) {
  const el = findCopilotMessageElement(id);
  if (!el) return;
  if (updates.text != null) setCopilotMessageText(el, updates.text, updates.status);
  if (updates.time != null) setCopilotMessageTime(el, updates.time);
  if (updates.kind) setCopilotMessageKind(el, updates.kind);
}

function updateCopilotMessage(id, updates) {
  const msg = state.copilotMessages.find((m) => m.id === id);
  if (msg) Object.assign(msg, updates);
  updateCopilotMessageDom(id, updates);
}

function nextCopilotId() {
  return 'copilot-msg-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 8);
}

function submitCopilotInput() {
  const input = getCopilotInputEl();
  if (!input) return;
  const text = (input.value || '').trim();
  if (!text) return;

  // Bail before mutating the input or queueing a placeholder if the WS
  // is down — `sendWsMessage` silently no-ops on a closed socket, and
  // the user would otherwise see a permanently disabled input with a
  // spinner that can never resolve.
  const ws = state.wsConnection;
  if (!state.wsConnected || !ws || ws.readyState !== WebSocket.OPEN) {
    appendCopilotMessage({
      id: nextCopilotId(),
      kind: 'error',
      text: 'Bridge offline: сообщение не отправлено.',
      time: nowDisplay()
    });
    return;
  }

  const meetingTitleNow = document.title || meetingTitle || '';
  let requestId;
  try {
    requestId = sendCopilotChatMessage({
      text,
      meetingId: state.currentMeetingId,
      meetingTitle: meetingTitleNow,
      transcriptWindow: buildCopilotTranscriptWindow()
    });
  } catch (err) {
    console.log('[KTalk Copilot] send failed', err);
    appendCopilotMessage({
      id: nextCopilotId(),
      kind: 'error',
      text: 'Не удалось отправить сообщение в Copilot.',
      time: nowDisplay()
    });
    return;
  }

  const userId = nextCopilotId();
  const assistantId = nextCopilotId();
  appendCopilotMessage({
    id: userId,
    kind: 'user',
    text,
    request_id: requestId,
    time: nowDisplay()
  });
  appendCopilotMessage({
    id: assistantId,
    kind: 'assistant',
    text: 'Жду ответа Claude…',
    request_id: requestId,
    status: 'working',
    time: nowDisplay()
  });
  state.copilotPending.set(requestId, { userId, assistantId, createdAt: Date.now() });

  input.value = '';
  setCopilotInputBusy(true);
}

function findPendingAssistantId(requestId) {
  if (!requestId) return null;
  const pending = state.copilotPending.get(requestId);
  return pending ? pending.assistantId : null;
}

export function handleCopilotReply(data) {
  ensureCopilotContent();
  const requestId = data && data.request_id;
  const replyText = (data && (data.text || data.message || '')) || '';
  const kind = data && data.kind === 'proactive' ? 'proactive' : 'assistant';
  const time = nowDisplay();

  if (kind === 'proactive') {
    if (state.copilotProactiveMutedUntil > Date.now()) {
      console.log('[KTalk Copilot] Proactive reply muted, dropping.');
      return;
    }
    if (Date.now() - (state.copilotLastProactiveAt || 0) < 60000) {
      console.log('[KTalk Copilot] Proactive cooldown active, dropping.');
      return;
    }
    appendCopilotMessage({
      id: nextCopilotId(),
      kind: 'proactive',
      text: replyText,
      request_id: requestId || null,
      time
    });
    state.copilotLastProactiveAt = Date.now();
    return;
  }

  const pendingId = findPendingAssistantId(requestId);
  if (pendingId) {
    updateCopilotMessage(pendingId, {
      kind: 'assistant',
      text: replyText,
      status: 'done',
      time
    });
    state.copilotPending.delete(requestId);
    setCopilotInputBusy(false);
    noteCopilotUnread('assistant');
    return;
  }
  appendCopilotMessage({
    id: nextCopilotId(),
    kind: 'assistant',
    text: replyText,
    request_id: requestId || null,
    time
  });
}

function appendCopilotError(text) {
  appendCopilotMessage({
    id: nextCopilotId(),
    kind: 'error',
    text,
    time: nowDisplay()
  });
}

function failCopilotPlaceholder(assistantId, text) {
  updateCopilotMessage(assistantId, {
    kind: 'error',
    text,
    status: 'error',
    time: nowDisplay()
  });
}

function settleCopilotRequest(requestId) {
  state.copilotPending.delete(requestId);
  setCopilotInputBusy(false);
}

function applyCopilotAgentState(data) {
  // Agent lifecycle status from the bridge merge (supervisor screen state +
  // daemon MCP-session count). Drives the standalone chip only — independent
  // from the channel/per-request indicator. Reset to ⚫ is delivered as an
  // explicit `agent_state: offline` frame (never on `meeting_ended`, whose
  // last-extension-gone path fires after the extension is already gone).
  const next = data && data.agent_state;
  if (COPILOT_AGENT_CHIP[next]) {
    state.copilotAgentState = next;
    updateCopilotAgentChip();
  }
  // Unknown agent_state (SDK drift) — ignore, keep the last known chip.
}

function applyCopilotChannelConnected() {
  // Channel-level recovery signal — bridge emits this on the 0→1 channel
  // client transition (or as a primer when an extension late-joins a
  // bridge whose channel was already up). Without it, once the indicator
  // flipped to `channel_disconnected` it would stay stuck even after the
  // channel came back online.
  state.copilotChannelStatus = 'connected';
  updateCopilotStatusIndicator();
}

function applyCopilotChannelDisconnected(data, requestId) {
  // Channel-level event — surface in the connectivity indicator. Per-
  // request statuses (queued/working/done/error) below describe a single
  // chat turn and must not overwrite the indicator: otherwise the label
  // would read "done" or "working…" long after the per-request bubble
  // already settled, masking actual channel health.
  state.copilotChannelStatus = 'channel_disconnected';
  updateCopilotStatusIndicator();
  const text = 'Channel offline: Claude не подключён.';
  const pendingId = findPendingAssistantId(requestId);
  if (pendingId) {
    failCopilotPlaceholder(pendingId, text);
    settleCopilotRequest(requestId);
  } else if (state.copilotPending && state.copilotPending.size > 0) {
    // Channel-level signal (no specific request_id match) with pending
    // requests in flight — the bridge fires this when the last channel
    // client drops mid-flight. Flip every placeholder to error and re-
    // enable input rather than appending a single bubble that says
    // nothing about the stranded turns.
    state.copilotPending.forEach((pending) => {
      if (pending && pending.assistantId) {
        failCopilotPlaceholder(pending.assistantId, text);
      }
    });
    state.copilotPending.clear();
    setCopilotInputBusy(false);
  } else {
    appendCopilotError(text);
  }
}

// Terminal `error` from the channel server (e.g. Claude failed) — without
// a corresponding `copilot_reply` the input would stay disabled forever
// and the placeholder would still claim "working". Symmetrize with
// channel_disconnected: flip the placeholder to an error bubble, drop the
// pending entry, and re-enable input so the user can retry.
function applyCopilotError(data, requestId) {
  const pendingId = findPendingAssistantId(requestId);
  const message = (data && data.message) || 'Claude не смог ответить.';
  if (pendingId) {
    failCopilotPlaceholder(pendingId, message);
    settleCopilotRequest(requestId);
  } else {
    appendCopilotError(message);
  }
}

// Terminal `done` — the bridge already pops the pending bookkeeping
// (ktalk_bridge.py:2349). If a matching `copilot_reply` arrived first,
// `findPendingAssistantId` returns null and there is nothing to do.
// If `done` arrived without a preceding reply (Claude called
// `set_copilot_status('done')` standalone), the placeholder would stay
// on "Жду ответа Claude…" and `setCopilotInputBusy(false)` would never
// run, stranding the textarea disabled until reload.
function applyCopilotDone(data, requestId) {
  const pendingId = findPendingAssistantId(requestId);
  if (pendingId) {
    const message = (data && data.message) || 'Запрос завершён без ответа.';
    updateCopilotMessage(pendingId, {
      kind: 'status',
      text: message,
      status: 'done',
      time: nowDisplay()
    });
    settleCopilotRequest(requestId);
  }
}

function applyCopilotRequestStatus(data, requestId, status) {
  const pendingId = findPendingAssistantId(requestId);
  if (pendingId) {
    updateCopilotMessage(pendingId, {
      status,
      text: status === 'working' ? 'Claude думает…' : `Status: ${status}`,
      time: nowDisplay()
    });
  }
}

// A Map, not an object: a status such as `toString` must reach the
// per-request branch, not a prototype method.
const COPILOT_STATUS_HANDLERS = new Map([
  ['agent_state', applyCopilotAgentState],
  ['channel_connected', applyCopilotChannelConnected],
  ['channel_disconnected', applyCopilotChannelDisconnected],
  ['error', applyCopilotError],
  ['done', applyCopilotDone]
]);

export function handleCopilotStatus(data) {
  ensureCopilotContent();
  const requestId = data && data.request_id;
  const status = (data && data.status) || 'unknown';
  const apply = COPILOT_STATUS_HANDLERS.get(status) || applyCopilotRequestStatus;
  apply(data, requestId, status);
}

export function appendCopilotSystemMessage(text) {
  ensureCopilotContent();
  appendCopilotMessage({
    id: nextCopilotId(),
    kind: 'status',
    text: text || '',
    time: nowDisplay()
  });
}

export function handleCopilotBridgeDisconnect() {
  // Bridge WS dropped — flip every in-flight chat placeholder to error and
  // re-enable input. Without this, `submitCopilotInput` would leave the
  // textarea disabled forever (only `copilot_reply` / `copilot_status` re-
  // enable it, and neither will arrive from a closed socket).
  if (state.copilotPending && state.copilotPending.size > 0) {
    state.copilotPending.forEach((pending) => {
      if (pending && pending.assistantId) {
        updateCopilotMessage(pending.assistantId, {
          kind: 'error',
          text: 'Bridge offline: соединение прервано.',
          status: 'error',
          time: nowDisplay()
        });
      }
    });
    state.copilotPending.clear();
  }
  setCopilotInputBusy(false);
  state.copilotChannelStatus = 'channel_disconnected';
  updateCopilotStatusIndicator();
}

function setAiTabActive(isAnyActive) {
  const aiTabBtn = document.querySelector('.ktalk-ai-tab');
  const aiTab = aiTabBtn?.closest('.tl-tab');

  if (aiTab) aiTab.classList.toggle('_active', isAnyActive);
  if (aiTabBtn) aiTabBtn.classList.toggle('_active', isAnyActive);
}

function setAiPanelView(view, isAnyActive) {
  const panel = document.querySelector('.ktalk-ai-panel');
  if (panel) {
    panel.classList.toggle('active', isAnyActive);
    panel.querySelectorAll('.ktalk-ai-subtab').forEach((btn) => {
      btn.classList.toggle('active', btn.dataset.view === view);
    });
  }
}

function setAiContentVisibility(isTranscript, isCopilot) {
  if (state.originalChatContent) {
    state.originalChatContent.style.display = isTranscript || isCopilot ? 'none' : '';
  }
  if (state.transcriptContentElement) {
    state.transcriptContentElement.classList.toggle('active', isTranscript);
  }
  if (state.copilotContentElement) {
    state.copilotContentElement.classList.toggle('active', isCopilot);
  }
}

function rememberAiView(view) {
  state.lastAiView = view;
  try {
    localStorage.setItem(constants.AI_LAST_VIEW_KEY, view);
  } catch (err) {
    void err;
  }
}

function setActiveView(view) {
  const isTranscript = view === 'transcript';
  const isCopilot = view === 'copilot';
  const isAnyActive = isTranscript || isCopilot;

  setAiTabActive(isAnyActive);
  setAiPanelView(view, isAnyActive);
  setAiContentVisibility(isTranscript, isCopilot);

  state.isCopilotActive = isCopilot;

  if (isAnyActive) rememberAiView(view);
  if (isCopilot) setCopilotUnread(false);
}

function setCopilotUnread(unread) {
  if (state.copilotUnread === unread) return;
  state.copilotUnread = unread;
  document.querySelectorAll('.ktalk-ai-unread').forEach((dot) => {
    dot.hidden = !unread;
  });
}

function noteCopilotUnread(kind) {
  if (!state.isCopilotActive && (kind === 'proactive' || kind === 'assistant')) {
    setCopilotUnread(true);
  }
}

function activateCopilotTab() {
  ensureCopilotContent();
  setActiveView('copilot');
  const input = getCopilotInputEl();
  if (input) input.focus();
}

function deactivateCustomTabs() {
  setActiveView(null);
}

// Handler registered with ui-vas3k so a floating-panel view switch drives the
// copilot subsystem (lazy content build, active class, unread reset, focus).
// Panel-level DOM (tab highlight, content visibility) stays in ui-vas3k.
function activateVas3kView(view) {
  const isCopilot = view === 'copilot';
  if (isCopilot) ensureCopilotContent();
  if (state.copilotContentElement) {
    state.copilotContentElement.classList.toggle('active', isCopilot);
  }
  state.isCopilotActive = isCopilot;
  if (isCopilot) {
    setCopilotUnread(false);
    const input = getCopilotInputEl();
    if (input) input.focus();
  }
}

export function watchTabChanges() {
  const tabsContainer = getKtalkChatTabs();
  if (!tabsContainer) return;

  tabsContainer.addEventListener('click', (e) => {
    const clickedTab = e.target.closest('.tl-tab');
    if (!clickedTab) return;

    if (!clickedTab.querySelector('.ktalk-ai-tab')) {
      deactivateCustomTabs();
    }
  });
}

function dispatchTranscriptUpdate() {
  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: state.transcriptLines.slice() }
  }));
}

// A frame tagged with another meeting than this tab's. Frames without the
// tag (older bridge builds) and a tab that has no meeting yet pass.
function isOtherMeetingFrame(data) {
  return !!(data && data.meeting_id && state.currentMeetingId
    && data.meeting_id !== state.currentMeetingId);
}

function transcriptFlags({ is_local, uncertain, uncertain_reason }) {
  return {
    is_local: is_local || false,
    uncertain: uncertain || false,
    uncertain_reason: uncertain_reason || ''
  };
}

function capTranscriptLines(lines) {
  if (lines.length > constants.MAX_TRANSCRIPT_LINES) {
    return lines.slice(-constants.MAX_TRANSCRIPT_LINES);
  }
  return lines;
}

function historyTime(seg) {
  return seg.ts_display || new Date(seg.ts).toLocaleTimeString('ru-RU', {
    hour: '2-digit', minute: '2-digit', second: '2-digit'
  });
}

function historySegmentToLine(seg) {
  return {
    speaker: seg.speaker,
    text: seg.text,
    timestamp: seg.timestamp || seg.ts_display,
    segment_id: seg.id || 0,
    // Preserve real source_id semantics: it must stay empty for legacy /
    // no-source streams (local mic, mixed call sites) so the next live
    // continuation still takes handleTranscript's `!prev.source_id` merge
    // path. Fabricating one from speaker_id breaks that merge.
    source_id: seg.source_id || '',
    // Stable identity used ONLY for privacy aliasing, kept separate from
    // source_id so duplicate/Unknown labels don't collapse after a reload.
    privacy_id: seg.source_id || seg.speaker_id || '',
    ...transcriptFlags(seg),
    time: historyTime(seg),
    createdAt: seg.ts ? new Date(seg.ts).getTime() : Date.now()
  };
}

function renderAllTranscriptLines() {
  if (isJitsiPlatform()) {
    clearVas3kTranscript();
    for (const line of state.transcriptLines) {
      addVas3kTranscriptLine(line);
    }
  } else {
    updateTranscriptUI();
  }
}

export function handleTranscriptHistory(data) {
  console.log('[KTalk Transcript] Received history:', data.segments?.length || 0, 'segments');

  if (data && data.meeting_id) {
    state.currentMeetingId = data.meeting_id;
  }

  if (!data.segments || data.segments.length === 0) {
    return;
  }

  state.transcriptLines = capTranscriptLines(data.segments.map(historySegmentToLine));

  // Re-render using the appropriate platform renderer
  renderAllTranscriptLines();

  dispatchTranscriptUpdate();
}

export function handleNewMeeting(data) {
  console.log('[KTalk Transcript] New meeting:', data.meeting_id);

  state.currentMeetingId = data && data.meeting_id ? data.meeting_id : null;
  state.transcriptLines = [];
  if (isJitsiPlatform()) {
    clearVas3kTranscript();
  } else {
    updateTranscriptUI();
  }

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: [] }
  }));
}

function buildLiveTranscriptLine(data) {
  const { speaker, text, timestamp, segment_id, source_id } = data;
  const sid = segment_id || 0;
  return {
    speaker,
    text,
    timestamp,
    segment_id: sid,
    // Stable per-source identifier (e.g. per-pid PCM stream). Used by the
    // continuation-merge predicate below so chunks from different sources
    // are not glued into a single bubble even if `is_continuation=true`.
    source_id: typeof source_id === 'string' ? source_id : '',
    // Per-child segment tracking so tier-2 refine can target continuation
    // chunks merged into this line. Backend issues a fresh segment_id for
    // every 3-s PCM chunk including continuations; without this, refines
    // for any chunk after the first in a merged bubble are silently dropped.
    segment_ids: sid > 0 ? [sid] : [],
    segment_texts: sid > 0 ? { [sid]: text } : {},
    ...transcriptFlags(data),
    time: new Date().toLocaleTimeString('ru-RU', {
      hour: '2-digit', minute: '2-digit', second: '2-digit'
    }),
    createdAt: Date.now()
  };
}

function stripUncertainMarks(text) {
  return text.replace(/ \(\?\)$/, '').replace(/ \(🔇\?\)$/, '').trim();
}

function isUnknownVas3kLine(lineEl, text, timestamp) {
  const speakerEl = lineEl.querySelector('.jitsi-transcript-speaker');
  const textEl = lineEl.querySelector('.jitsi-transcript-text');
  return speakerEl && speakerEl.textContent === 'Unknown' &&
    textEl && stripUncertainMarks(textEl.textContent).startsWith(text) &&
    (!timestamp || lineEl.dataset.timestamp === timestamp);
}

// Find the DOM element with Unknown speaker matching this text and update in place
function correctVas3kUnknownLine(speaker, text, timestamp) {
  const content = document.querySelector('.jitsi-transcript-content');
  if (!content) return;
  for (const lineEl of content.querySelectorAll('.jitsi-transcript-line')) {
    if (isUnknownVas3kLine(lineEl, text, timestamp)) {
      lineEl.querySelector('.jitsi-transcript-speaker').textContent = speaker;
      const textEl = lineEl.querySelector('.jitsi-transcript-text');
      // Remove uncertainty markers from corrected line
      textEl.classList.remove('jitsi-transcript-uncertain');
      textEl.textContent = stripUncertainMarks(textEl.textContent);
      break;
    }
  }
}

function applySpeakerCorrection({ speaker, text, timestamp, uncertain, uncertain_reason }) {
  // Retroactive correction: find and update existing Unknown line by timestamp+text
  const idx = state.transcriptLines.findIndex(
    line => line.speaker === 'Unknown' && line.text.startsWith(text) && line.timestamp === timestamp
  );
  if (idx !== -1) {
    state.transcriptLines[idx].speaker = speaker;
    state.transcriptLines[idx].uncertain = uncertain || false;
    state.transcriptLines[idx].uncertain_reason = uncertain_reason || '';
  }
  // Re-render UI for correction (don't fall through to append which would duplicate)
  if (isJitsiPlatform()) {
    correctVas3kUnknownLine(speaker, text, timestamp);
  } else {
    updateTranscriptUI();
  }
  dispatchTranscriptUpdate();
}

function lastTranscriptLine() {
  const lines = state.transcriptLines;
  return lines.length > 0 ? lines[lines.length - 1] : null;
}

// source_id prevents continuation chunks from contaminating another
// source's segment_ids/segment_texts; speaker fallback is only for
// legacy frames without source_id.
function continuesLine(prev, line) {
  if (line.source_id) return prev.source_id === line.source_id;
  return !prev.source_id && prev.speaker === line.speaker;
}

// Backfill tracking for legacy lines pushed before this change so the
// refine handler can find them by either primary or child segment_id.
function backfillSegmentTracking(prev, text) {
  if (!prev.segment_ids) {
    prev.segment_ids = prev.segment_id > 0 ? [prev.segment_id] : [];
  }
  if (!prev.segment_texts) {
    prev.segment_texts = prev.segment_id > 0
      ? { [prev.segment_id]: prev.text.slice(0, -(text.length + 1)) }
      : {};
  }
}

function appendContinuation(prev, line) {
  const { text, segment_id: sid } = line;
  prev.text += ' ' + text;
  if (sid > 0) {
    backfillSegmentTracking(prev, text);
    prev.segment_ids.push(sid);
    prev.segment_texts[sid] = text;
  }
}

function pushTranscriptLine(line) {
  state.transcriptLines.push(line);

  if (state.transcriptLines.length > constants.MAX_TRANSCRIPT_LINES) {
    state.transcriptLines.shift();
  }
}

export function handleTranscript(data) {
  // Drop segments produced from a sibling tab's audio when two KTalk
  // tabs are open at the same time. Mirrors the cross-meeting guard in
  // `handleTranscriptRefine`. Older bridge builds that don't tag the
  // frame still pass through (missing field == no filter).
  if (isOtherMeetingFrame(data)) {
    return;
  }

  if (data.correction) {
    applySpeakerCorrection(data);
    return;
  }

  const newLine = buildLiveTranscriptLine(data);
  const prev = lastTranscriptLine();
  if (data.is_continuation && prev && continuesLine(prev, newLine)) {
    appendContinuation(prev, newLine);
  } else {
    pushTranscriptLine(newLine);
  }

  // Update platform-specific UI
  if (isJitsiPlatform()) {
    addVas3kTranscriptLine(newLine);
  } else {
    updateTranscriptUI();
  }

  dispatchTranscriptUpdate();
}

function refineTextsById(updates) {
  const byId = new Map();
  for (const upd of updates) {
    if (!upd) continue;
    const sid = Number(upd.segment_id);
    if (!Number.isFinite(sid) || sid <= 0) continue;
    if (typeof upd.text !== 'string') continue;
    byId.set(sid, upd.text);
  }
  return byId;
}

function findLineBySegmentId(sid) {
  return state.transcriptLines.find(
    (line) => (Array.isArray(line.segment_ids) && line.segment_ids.includes(sid)) || line.segment_id === sid
  );
}

function applyRefinedText(target, sid, newText) {
  if (target.segment_texts && Array.isArray(target.segment_ids)
      && target.segment_ids.length > 0) {
    target.segment_texts[sid] = newText;
    target.text = target.segment_ids
      .map(id => target.segment_texts[id] || '')
      .filter(Boolean)
      .join(' ');
  } else {
    target.text = newText;
  }
}

// Resolve each refined segment_id to its containing state line. A state
// line may own multiple segment_ids when consecutive PCM chunks were
// merged as continuations (handleTranscript above) — we look up via both
// `segment_ids` (child list) and `segment_id` (legacy primary).
function applyRefinesToState(byId) {
  const touchedLines = new Set();
  for (const [sid, newText] of byId) {
    const target = findLineBySegmentId(sid);
    if (!target) continue;
    applyRefinedText(target, sid, newText);
    target.refined = true;
    touchedLines.add(target);
  }
  return touchedLines;
}

function pulseRefined(textEl) {
  textEl.classList.add('refine-fade');
  setTimeout(() => textEl.classList.remove('refine-fade'), 200);
}

function refineVas3kLines(byId) {
  const content = document.querySelector('.jitsi-transcript-content');
  if (!content) return;
  // Vas3k path renders one DOM line per chunk (addVas3kTranscriptLine is
  // called for every transcript, including continuations), so each
  // refined segment_id has its own bubble to target.
  for (const [sid, newText] of byId) {
    const lineEl = content.querySelector(`.jitsi-transcript-line[data-segment-id="${sid}"]`);
    if (!lineEl) continue;
    const textEl = lineEl.querySelector('.jitsi-transcript-text');
    if (!textEl) continue;
    textEl.textContent = newText;
    pulseRefined(textEl);
  }
}

function refineKtalkBubbles(touchedLines) {
  const container = document.querySelector('.ktalk-transcript-messages');
  if (!container) return;
  // Paused-mode anchor compensation for in-place bubble growth (no-op in Follow).
  const anchor = captureScrollAnchor(container);
  // Ktalk path rebuilds the container from state.transcriptLines, so
  // merged continuation bubbles only expose `data-segment-id` of the
  // primary (first) chunk. Look up by primary id but render the
  // rebuilt merged text (already updated in state above).
  for (const line of touchedLines) {
    const primaryId = line.segment_id;
    if (!primaryId) continue;
    const msgEl = container.querySelector(`.ktalk-msg[data-segment-id="${primaryId}"]`);
    if (!msgEl) continue;
    const textEl = msgEl.querySelector('.ktalk-msg-text');
    if (!textEl) continue;
    textEl.innerHTML = toTrustedHtml(formatTranscriptText(line.text));
    pulseRefined(textEl);
  }
  restoreScrollAnchor(container, anchor);
}

/**
 * Handle tier-2 refined transcript updates. The backend tier-1 (live) path
 * already populated bubbles via `handleTranscript`; when word-timestamp
 * refine rewrites a segment, the bridge fans out a `transcript_refine`
 * message. We swap text in place so scroll position is preserved, add a
 * short `.refine-fade` pulse, and mirror the change into
 * `state.transcriptLines` so any full re-render (history reload, new
 * meeting) keeps the refined text.
 *
 * Missing bubbles are ignored silently — extension overlay only renders
 * recent lines, so a refine for a segment that was pushed out of view (or
 * for a segment the extension never saw because it reconnected mid-meeting)
 * should not error.
 */
export function handleTranscriptRefine(data) {
  const updates = Array.isArray(data && data.updates) ? data.updates : [];
  if (updates.length === 0) return;

  // Segment_ids restart at 1 per meeting, so a refine that slipped past a
  // meeting switch would otherwise rewrite a DIFFERENT speaker's bubble in
  // the new meeting with stale text from the old one.
  if (isOtherMeetingFrame(data)) {
    return;
  }

  const byId = refineTextsById(updates);
  if (byId.size === 0) return;

  const touchedLines = applyRefinesToState(byId);

  if (isJitsiPlatform()) {
    refineVas3kLines(byId);
  } else {
    refineKtalkBubbles(touchedLines);
  }

  dispatchTranscriptUpdate();
}

// Fail closed: if an alias lookup misses while privacy is on, use a generic
// label rather than leaking the real speaker name.
const PRIVACY_ALIAS_FALLBACK = 'speaker_?';

// System-event rows (pause/resume/join markers, restored from transcript
// history with speaker_id "__system__") are not participants: they must not be
// anonymized or consume a speaker_N index.
const SYSTEM_SOURCE_ID = '__system__';
function isSystemRow(line) {
  return line.source_id === SYSTEM_SOURCE_ID
    || line.privacy_id === SYSTEM_SOURCE_ID
    || line.speaker_id === SYSTEM_SOURCE_ID;
}

// Identity key for privacy aliasing. Live rows carry source_id; restored
// history rows carry privacy_id (their persisted speaker id). Display name is
// the last-resort fallback for legacy/no-source frames.
function aliasKey(line) {
  return line.source_id || line.privacy_id || line.speaker;
}

// Build identity -> speaker_N aliases in order of first appearance.
// Exported as a pure function so it can be unit-tested without the DOM.
export function buildSpeakerAliasMap(lines) {
  const map = new Map();
  let n = 0;
  for (const line of lines) {
    if (isSystemRow(line)) continue;
    const key = aliasKey(line);
    if (key == null) continue;
    if (!map.has(key)) map.set(key, `speaker_${n++}`);
  }
  return map;
}

function copyAllTranscript() {
  if (state.transcriptLines.length === 0) return;

  const alias = state.privacyMode ? buildSpeakerAliasMap(state.transcriptLines) : null;

  const text = state.transcriptLines.map(line => {
    const base = (alias && !isSystemRow(line))
      ? (alias.get(aliasKey(line)) ?? PRIVACY_ALIAS_FALLBACK)
      : line.speaker;
    const speaker = base + getUncertainSuffix(line);
    return `**${speaker}** *[${line.time}]*: ${line.text}`;
  }).join('\n\n');

  navigator.clipboard.writeText(text).then(() => {
    const btn = document.querySelector('.ktalk-transcript-copy-btn');
    if (btn) {
      btn.classList.add('copied');
      btn.textContent = '✓';
      setTimeout(() => {
        btn.classList.remove('copied');
        btn.textContent = 'Copy';
      }, 2000);
    }
  }).catch(err => {
    console.log('[KTalk Transcript] Copy failed:', err);
  });
}

function updateTranscriptUI() {
  const container = document.querySelector('.ktalk-transcript-messages');
  if (!container) return;

  updateFollowButton();

  if (state.transcriptLines.length === 0) {
    container.innerHTML = toTrustedHtml(`
        <div class="ktalk-transcript-empty">
          <div class="ktalk-transcript-empty-icon">📝</div>
          <div class="ktalk-transcript-empty-text">
            Waiting for transcript...<br>
            Start recog with:
          </div>
          <div class="ktalk-transcript-empty-cmd">
            ./recog-service.sh start
          </div>
        </div>
      `);
    return;
  }

  const anchor = captureScrollAnchor(container);
  const shouldAutoScroll = state.autoScrollEnabled && isNearBottom(container);

  container.innerHTML = toTrustedHtml(state.transcriptLines.map(line => {
    const classes = [
      'ktalk-msg',
      line.is_local ? 'local' : 'remote',
      line.uncertain ? 'uncertain' : '',
      line.uncertain_reason === 'mics_off' ? 'mics-off' : ''
    ].filter(Boolean).join(' ');

    const speakerDisplay = line.speaker + getUncertainSuffix(line);

    const messageText = formatTranscriptText(line.text);
    const timeValue = line.time || '';
    const segmentIdAttr = line.segment_id > 0
      ? ` data-segment-id="${line.segment_id}"`
      : '';

    return `<div class="${classes}" data-ts="${escapeHtml(timeValue)}"${segmentIdAttr}>
        <div class="ktalk-msg-header">
          <span class="ktalk-msg-speaker">${speakerDisplay}</span>
          <span class="ktalk-msg-time">${line.time}</span>
        </div>
        <div class="ktalk-msg-text">${messageText}</div>
      </div>`;
  }).join(''));

  if (shouldAutoScroll) {
    container.scrollTop = container.scrollHeight;
  } else if (anchor) {
    restoreScrollAnchor(container, anchor);
  }
}

function isNearBottom(container) {
  const thresholdPx = 80;
  const distance = container.scrollHeight - (container.scrollTop + container.clientHeight);
  return distance <= thresholdPx;
}

/**
 * Capture the .ktalk-msg row whose vertical extent contains the top of the
 * viewport (i.e. the last row whose offsetTop is at or above scrollTop). This
 * is the row a paused user is reading; restoring it after a DOM update keeps
 * the visible content stable. Returns null in Follow mode or when no rows
 * exist. `subPixelTolerancePx` accepts a row whose offsetTop sits a sub-pixel
 * fraction below scrollTop instead of falling back to the previous row.
 */
function captureScrollAnchor(container) {
  if (state.autoScrollEnabled) return null;
  const subPixelTolerancePx = 1;
  const top = container.scrollTop;
  const msgs = container.querySelectorAll('.ktalk-msg');
  let anchorEl = null;
  // msgs are in DOM order = vertical order. Walk from the end and pick the
  // first row whose top is at or above scrollTop — that row contains (or
  // begins at) the viewport top. Falls back to the first row when scrollTop
  // is above all rows (e.g. negative scroll on overscroll, or empty top gap).
  for (let i = msgs.length - 1; i >= 0; i--) {
    if (msgs[i].offsetTop <= top + subPixelTolerancePx) { anchorEl = msgs[i]; break; }
  }
  if (!anchorEl && msgs.length > 0) anchorEl = msgs[0];
  if (!anchorEl) return null;
  const sidRaw = anchorEl.dataset.segmentId;
  return {
    top,
    segmentId: sidRaw && Number(sidRaw) > 0 ? sidRaw : null,
    offsetTop: anchorEl.offsetTop,
  };
}

/**
 * Restore scrollTop after a DOM update by re-finding the anchor and applying
 * offsetTop drift. Falls back to anchor.top when the anchor row is gone.
 */
function restoreScrollAnchor(container, anchor) {
  if (!anchor) return;
  let el = null;
  if (anchor.segmentId != null) {
    el = container.querySelector(`.ktalk-msg[data-segment-id="${anchor.segmentId}"]`);
  }
  if (!el) {
    container.scrollTop = anchor.top;
    return;
  }
  container.scrollTop = anchor.top + (el.offsetTop - anchor.offsetTop);
}

function toggleAutoFollow() {
  state.autoScrollEnabled = !state.autoScrollEnabled;
  updateFollowButton();
  if (state.autoScrollEnabled) {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = container.scrollHeight;
  }
}

function updateFollowButton() {
  const btn = document.querySelector('.ktalk-transcript-follow-btn');
  if (!btn) return;
  btn.classList.toggle('active', state.autoScrollEnabled);
  btn.textContent = state.autoScrollEnabled ? 'Follow' : 'Paused';
}

function loadBioReadSettings() {
  const fallback = normalizeBioReadSettings(state.bioRead);
  const saved = localStorage.getItem(BIOREAD_SETTINGS_KEY);
  if (!saved) {
    Object.assign(state.bioRead, fallback);
    return;
  }

  try {
    const parsed = JSON.parse(saved);
    Object.assign(state.bioRead, normalizeBioReadSettings({ ...fallback, ...parsed }));
  } catch (e) {
    console.log('[KTalk Transcript] BioRead settings load failed:', e);
    Object.assign(state.bioRead, fallback);
  }
}

function saveBioReadSettings() {
  const payload = {
    enabled: state.bioRead.enabled,
    fixation: state.bioRead.fixation,
    tailOpacity: state.bioRead.tailOpacity,
    ignoreNumbers: state.bioRead.ignoreNumbers
  };
  localStorage.setItem(BIOREAD_SETTINGS_KEY, JSON.stringify(payload));
}

function applyBioReadSettings({ persist = true, rerender = true } = {}) {
  Object.assign(state.bioRead, normalizeBioReadSettings(state.bioRead));

  if (state.transcriptContentElement) {
    state.transcriptContentElement.style.setProperty(
      '--bioread-tail-opacity',
      (state.bioRead.tailOpacity / 100).toString()
    );
  }

  updateBioReadControls();

  if (persist) {
    saveBioReadSettings();
  }
  if (rerender) {
    updateTranscriptUI();
  }
}

function updateBioReadControls() {
  if (!state.transcriptContentElement) return;

  const toggleBtn = state.transcriptContentElement.querySelector('.ktalk-bioread-toggle');
  const settingsBtn = state.transcriptContentElement.querySelector('.ktalk-bioread-settings-btn');
  const panel = state.transcriptContentElement.querySelector('.ktalk-bioread-panel');
  const fixationSlider = state.transcriptContentElement.querySelector('.ktalk-bioread-fixation');
  const contrastSlider = state.transcriptContentElement.querySelector('.ktalk-bioread-contrast');
  const fixationValue = state.transcriptContentElement.querySelector('[data-bioread-value="fixation"]');
  const contrastValue = state.transcriptContentElement.querySelector('[data-bioread-value="contrast"]');

  if (toggleBtn) toggleBtn.classList.toggle('active', state.bioRead.enabled);
  if (settingsBtn) settingsBtn.classList.toggle('active', state.bioRead.panelOpen);
  if (panel) panel.classList.toggle('active', state.bioRead.panelOpen);

  if (fixationSlider) fixationSlider.value = String(state.bioRead.fixation);
  if (contrastSlider) contrastSlider.value = String(state.bioRead.tailOpacity);
  if (fixationValue) fixationValue.textContent = `${state.bioRead.fixation}%`;
  if (contrastValue) contrastValue.textContent = `${state.bioRead.tailOpacity}%`;
}

function onBioReadControl(el, type, listener) {
  if (el) el.addEventListener(type, listener);
}

function setupBioReadControls(container) {
  const toggleBtn = container.querySelector('.ktalk-bioread-toggle');
  const settingsBtn = container.querySelector('.ktalk-bioread-settings-btn');
  const panel = container.querySelector('.ktalk-bioread-panel');
  const fixationSlider = container.querySelector('.ktalk-bioread-fixation');
  const contrastSlider = container.querySelector('.ktalk-bioread-contrast');

  onBioReadControl(toggleBtn, 'click', () => {
    state.bioRead.enabled = !state.bioRead.enabled;
    applyBioReadSettings();
  });

  onBioReadControl(settingsBtn, 'click', (event) => {
    event.stopPropagation();
    state.bioRead.panelOpen = !state.bioRead.panelOpen;
    updateBioReadControls();
  });

  onBioReadControl(panel, 'click', (event) => event.stopPropagation());

  onBioReadControl(fixationSlider, 'input', () => {
    state.bioRead.fixation = parseInt(fixationSlider.value, 10);
    applyBioReadSettings();
  });

  onBioReadControl(contrastSlider, 'input', () => {
    state.bioRead.tailOpacity = parseInt(contrastSlider.value, 10);
    applyBioReadSettings();
  });

  if (!bioReadHotkeyBound) {
    bioReadHotkeyBound = true;
    document.addEventListener('keydown', (event) => {
      if (!event.altKey || event.code !== 'KeyB') return;
      if (isEditableTarget(event.target)) return;
      event.preventDefault();
      state.bioRead.enabled = !state.bioRead.enabled;
      applyBioReadSettings();
    });
  }

  if (!bioReadPanelListenerBound) {
    bioReadPanelListenerBound = true;
    document.addEventListener('click', (event) => {
      if (!state.bioRead.panelOpen) return;
      const activePanel = state.transcriptContentElement?.querySelector('.ktalk-bioread-panel');
      const activeSettingsBtn = state.transcriptContentElement?.querySelector('.ktalk-bioread-settings-btn');
      if (activePanel && activePanel.contains(event.target)) return;
      if (activeSettingsBtn && activeSettingsBtn.contains(event.target)) return;
      state.bioRead.panelOpen = false;
      updateBioReadControls();
    });
  }

  applyBioReadSettings({ persist: false, rerender: false });
}

function isEditableTarget(target) {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || target.isContentEditable;
}

function formatTranscriptText(text) {
  const safeText = typeof text === 'string' ? text : String(text ?? '');
  if (!state.bioRead.enabled) {
    return safeText;
  }
  return formatBioReadText(safeText, state.bioRead);
}


export function initPlatformUI() {
  if (isJitsiPlatform()) {
    // Let the floating panel drive the copilot subsystem on view switches.
    setVas3kCopilotActivateHandler(activateVas3kView);
    return initVas3kUI();
  }

  // For KTalk, return false - tabs are handled by addAiTab
  return false;
}

/**
 * Platform-aware WebSocket indicator update
 */
export function updateWsIndicatorPlatform(connected) {
  if (isJitsiPlatform()) {
    updateVas3kWsIndicator(connected);
  }
}

/**
 * Platform-aware transcript line addition
 */
export function addTranscriptLinePlatform(line) {
  if (isJitsiPlatform()) {
    addVas3kTranscriptLine(line);
  } else {
    addTranscriptLine(line);
  }
}
