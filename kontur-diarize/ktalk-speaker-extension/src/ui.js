import { state, constants } from './state.js';
import { formatBioReadText, normalizeBioReadSettings, BIOREAD_LIMITS, BIOREAD_SETTINGS_KEY } from './bioread.js';
import { getUncertainSuffix, renderMarkdown } from './utils.js';
import { sendWsMessage } from './ws_api.js';

let selectedModel = localStorage.getItem('ktalk-ai-model') || 'gemini-2.5-flash';
let meetingTitle = document.title || '';
let bioReadHotkeyBound = false;
let bioReadPanelListenerBound = false;
let summaryMenuOpen = false;
let summaryMenuListenerBound = false;

export function injectStyles() {
  if (document.querySelector('#ktalk-transcript-styles')) return;

  const styles = document.createElement('style');
  styles.id = 'ktalk-transcript-styles';
  styles.textContent = `
      /* Transcript tab - exact match of KTalk's PRIVATE button */
      .ktalk-transcript-tab-wrapper.tl-tab {
        display: inline-block !important;
        height: 32px;
        padding: 0;
        margin: 0;
      }

      .ktalk-transcript-tab.link {
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
        transition: color 0.15s;
      }
      .ktalk-transcript-tab.link:hover {
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-transcript-tab.link._active {
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-transcript-tab .tab-content {
        display: flex;
        align-items: center;
        height: 32px;
        line-height: 16px;
        gap: 4px;
      }
      .ktalk-transcript-tab .underline {
        position: absolute;
        bottom: 0;
        left: 6px;
        right: 6px;
        height: 2px;
        background-color: rgb(0, 138, 255);
        opacity: 0;
        transition: opacity 0.15s;
      }
      .ktalk-transcript-tab._active .underline {
        opacity: 1;
      }
      .ktalk-transcript-tab .ws-dot {
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: #22c55e;
        flex-shrink: 0;
      }
      .ktalk-transcript-tab .ws-dot:not(.connected) {
        background: #ef4444;
      }

      /* Transcript content area - match KTalk's native chat style */
      .ktalk-transcript-content {
        display: none;
        flex-direction: column;
        height: 100%;
        background: rgb(52, 52, 52);
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
        background: rgb(60, 60, 60);
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
        justify-content: flex-start;
        padding: 8px 12px;
        background: rgb(56, 56, 56);
        border-bottom: 1px solid rgba(255,255,255,0.1);
        flex-wrap: wrap;
        gap: 6px 8px;
        position: relative;
      }
      .ktalk-transcript-toolbar-left {
        display: flex;
        gap: 6px;
        flex-wrap: wrap;
        align-items: center;
      }
      .ktalk-transcript-toolbar-actions {
        margin-left: auto;
        justify-content: flex-end;
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
      .ktalk-transcript-summary-btn {
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
      .ktalk-transcript-summary-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-summary-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }
      .ktalk-transcript-summary-btn.loading {
        animation: ktalk-loading 1s infinite;
      }
      @keyframes ktalk-loading {
        0% { opacity: 0.5; }
        50% { opacity: 1; }
        100% { opacity: 0.5; }
      }
      .ktalk-summary-menu {
        position: relative;
        display: inline-flex;
      }
      .ktalk-summary-menu-toggle {
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
        gap: 4px;
      }
      .ktalk-summary-menu-toggle:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-summary-menu-toggle.active {
        background: rgba(255,255,255,0.25);
      }
      .ktalk-summary-menu-panel {
        position: absolute;
        top: calc(100% + 6px);
        right: 0;
        min-width: 160px;
        padding: 6px;
        background: rgb(44, 44, 44);
        border: 1px solid rgba(255,255,255,0.1);
        border-radius: 8px;
        box-shadow: 0 12px 24px rgba(0,0,0,0.35);
        display: none;
        flex-direction: column;
        gap: 4px;
        z-index: 5;
      }
      .ktalk-summary-menu-panel.active {
        display: flex;
      }
      .ktalk-summary-menu-panel .ktalk-transcript-summary-btn {
        width: 100%;
        justify-content: flex-start;
        gap: 8px;
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
        background: rgb(44, 44, 44);
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

      /* Summary Floating Window */
      .ktalk-summary-modal {
        position: fixed;
        top: 50px;
        right: 50px;
        width: 480px;
        background: rgb(40, 40, 40);
        border: 1px solid rgba(255,255,255,0.1);
        border-radius: 12px;
        box-shadow: 0 20px 40px rgba(0,0,0,0.4);
        z-index: 9999;
        display: none;
        font-family: "Lab Grotesque", Arial, sans-serif;
      }
      .ktalk-summary-modal.active {
        display: block;
      }
      .ktalk-summary-modal-content {
        display: flex;
        flex-direction: column;
        max-height: 80vh;
      }
      .ktalk-summary-modal-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 16px 20px;
        border-bottom: 1px solid rgba(255,255,255,0.1);
        cursor: move;
      }
      .ktalk-summary-modal-title {
        font-size: 16px;
        font-weight: 600;
        color: #fff;
      }
      .ktalk-summary-modal-close {
        background: none;
        border: none;
        color: rgba(255,255,255,0.6);
        font-size: 20px;
        cursor: pointer;
        padding: 0;
      }
      .ktalk-summary-modal-close:hover {
        color: #fff;
      }
      .ktalk-summary-modal-body {
        padding: 20px;
        overflow-y: auto;
        color: rgba(255,255,255,0.9);
        font-size: 14px;
        line-height: 1.6;
      }
      .ktalk-summary-modal-body.loading {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        min-height: 200px;
        gap: 12px;
      }
      .ktalk-summary-spinner {
        width: 32px;
        height: 32px;
        border: 3px solid rgba(255,255,255,0.2);
        border-top: 3px solid #60a5fa;
        border-radius: 50%;
        animation: ktalk-spin 0.8s linear infinite;
      }
      @keyframes ktalk-spin {
        0% { transform: rotate(0deg); }
        100% { transform: rotate(360deg); }
      }
      .ktalk-summary-modal-footer {
        display: flex;
        justify-content: flex-end;
        padding: 12px 20px;
        border-top: 1px solid rgba(255,255,255,0.1);
      }
      .ktalk-summary-copy-btn {
        padding: 6px 12px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 6px;
        color: rgba(255,255,255,0.8);
        font-size: 12px;
        cursor: pointer;
      }
      .ktalk-summary-copy-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-summary-copy-btn.copied {
        background: #22c55e;
      }
      .ktalk-summary-error {
        color: #f87171;
        font-weight: 500;
      }
      .ktalk-summary-question-input {
        display: flex;
        gap: 8px;
        padding: 0 20px 12px 20px;
      }
      .ktalk-question-field {
        flex: 1;
        padding: 8px 12px;
        background: rgba(255,255,255,0.08);
        border: 1px solid rgba(255,255,255,0.15);
        border-radius: 6px;
        color: #fff;
        font-size: 13px;
      }
      .ktalk-question-field:focus {
        outline: none;
        border-color: #60a5fa;
      }
      .ktalk-question-field::placeholder {
        color: rgba(255,255,255,0.4);
      }
      .ktalk-question-submit {
        padding: 8px 12px;
        background: #60a5fa;
        border: none;
        border-radius: 6px;
        color: #fff;
        font-size: 13px;
        cursor: pointer;
      }
      .ktalk-question-submit:hover {
        background: #3b82f6;
      }
      .ktalk-question-submit:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }
      .ktalk-summary-text h3, .ktalk-summary-text h4 {
        color: #fff;
        margin: 12px 0 8px 0;
      }
      .ktalk-summary-text h3:first-child {
        margin-top: 0;
      }
      .ktalk-summary-text ul {
        margin: 8px 0;
        padding-left: 20px;
      }
      .ktalk-summary-text li {
        margin: 4px 0;
      }
      .ktalk-summary-text strong {
        color: #fff;
      }
      .ktalk-summary-text em {
        color: rgba(255,255,255,0.8);
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

    `;
  document.head.appendChild(styles);
}

export function addTranscriptTab() {
  if (document.querySelector('.ktalk-transcript-tab')) {
    return true;
  }

  const tabsContainer = document.querySelector('.tl-tabset__content');
  if (!tabsContainer) {
    return false;
  }

  console.log('[KTalk Transcript] Adding tab to tabset');

  const tab = document.createElement('div');
  tab.className = 'tl-tab _switchable ktalk-transcript-tab-wrapper';
  tab.innerHTML = `
      <button type="button" class="ktalk-transcript-tab link">
        <span class="tab-content">
          <span class="title">Transcript</span>
          <span class="ws-dot ${state.wsConnected ? 'connected' : ''}"></span>
        </span>
        <span class="underline" style="background-color: rgb(0, 138, 255);"></span>
      </button>
    `;

  tabsContainer.appendChild(tab);

  tab.querySelector('button').addEventListener('click', () => {
    activateTranscriptTab();
  });

  state.transcriptTabAdded = true;
  return true;
}

export function createTranscriptContent() {
  if (document.querySelector('.ktalk-transcript-content')) {
    return document.querySelector('.ktalk-transcript-content');
  }

  const contentArea = document.querySelector('.slide-menu-wrapper .content');
  if (!contentArea) {
    console.log('[KTalk Transcript] Content area not found');
    return null;
  }

  state.originalChatContent = contentArea.firstElementChild;

  state.transcriptContentElement = document.createElement('div');
  state.transcriptContentElement.className = 'ktalk-transcript-content';
  loadBioReadSettings();
  state.transcriptContentElement.innerHTML = `
      <div class="ktalk-transcript-speaker-bar">
        <div class="ktalk-speaker-dot"></div>
        <span class="ktalk-speaker-name">No one speaking</span>
      </div>
      <div class="ktalk-transcript-toolbar">
        <div class="ktalk-transcript-toolbar-left">
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-top" title="Scroll to top">↑</button>
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-bottom" title="Scroll to bottom">↓</button>
        </div>
        <div class="ktalk-transcript-toolbar-left ktalk-transcript-toolbar-actions">
          <div class="ktalk-summary-menu">
            <button class="ktalk-summary-menu-toggle" title="Summary actions">
              Summary <span class="ktalk-summary-caret">▾</span>
            </button>
            <div class="ktalk-summary-menu-panel">
              <button class="ktalk-transcript-summary-btn" data-type="catchup" title="Catch up - включиться в дискуссию">🎯 Catch up</button>
              <button class="ktalk-transcript-summary-btn" data-type="brief" title="Краткое за 2 мин">📋 Brief</button>
              <button class="ktalk-transcript-summary-btn" data-type="full" title="Вся встреча + вопрос">💬 Full</button>
            </div>
          </div>
          <button class="ktalk-transcript-follow-btn" title="Auto-follow latest">Follow</button>
          <button class="ktalk-transcript-copy-btn" title="Copy all transcript">Copy</button>
          <button class="ktalk-bioread-toggle" title="BioRead">Bio</button>
          <button class="ktalk-bioread-settings-btn" title="BioRead settings">⚙</button>
        </div>
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
            python mp4_notes.py live --screencapture --ktalk
          </div>
        </div>
      </div>
    `;

  state.transcriptContentElement.querySelector('.ktalk-transcript-copy-btn').addEventListener('click', copyAllTranscript);
  state.transcriptContentElement.querySelector('.ktalk-transcript-follow-btn').addEventListener('click', toggleAutoFollow);
  updateFollowButton();
  state.transcriptContentElement.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      requestSummary(btn.dataset.type);
      closeSummaryMenu();
    });
  });
  state.transcriptContentElement.querySelector('.ktalk-scroll-top').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = 0;
  });
  state.transcriptContentElement.querySelector('.ktalk-scroll-bottom').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = container.scrollHeight;
  });

  setupSummaryMenu(state.transcriptContentElement);
  setupBioReadControls(state.transcriptContentElement);
  createSummaryModal();

  contentArea.appendChild(state.transcriptContentElement);

  console.log('[KTalk Transcript] Content element created');
  return state.transcriptContentElement;
}

function activateTranscriptTab() {
  const transcriptTabBtn = document.querySelector('.ktalk-transcript-tab');
  if (!transcriptTabBtn) return;

  const tab = transcriptTabBtn.closest('.tl-tab');
  if (tab) {
    tab.classList.add('_active');
    transcriptTabBtn.classList.add('_active');
  }

  if (state.originalChatContent) {
    state.originalChatContent.style.display = 'none';
  }
  if (state.transcriptContentElement) {
    state.transcriptContentElement.classList.add('active');
  }

  state.isTranscriptActive = true;
}

function deactivateTranscriptTab() {
  const transcriptTabBtn = document.querySelector('.ktalk-transcript-tab');
  if (!transcriptTabBtn) return;

  const tab = transcriptTabBtn.closest('.tl-tab');
  if (tab) {
    tab.classList.remove('_active');
    transcriptTabBtn.classList.remove('_active');
  }

  if (state.originalChatContent) {
    state.originalChatContent.style.display = '';
  }
  if (state.transcriptContentElement) {
    state.transcriptContentElement.classList.remove('active');
  }

  state.isTranscriptActive = false;
}

export function watchTabChanges() {
  const tabsContainer = document.querySelector('.tl-tabset__content');
  if (!tabsContainer) return;

  tabsContainer.addEventListener('click', (e) => {
    const clickedTab = e.target.closest('.tl-tab');
    if (!clickedTab) return;

    const isTranscriptTab = clickedTab.querySelector('.ktalk-transcript-tab');
    if (!isTranscriptTab) {
      deactivateTranscriptTab();
    }
  });
}

export function updateWsIndicator(connected) {
  const dot = document.querySelector('.ktalk-transcript-tab-wrapper .ws-dot');
  if (dot) {
    dot.classList.toggle('connected', connected);
  }
}

export function handleTranscriptHistory(data) {
  console.log('[KTalk Transcript] Received history:', data.segments?.length || 0, 'segments');

  if (!data.segments || data.segments.length === 0) {
    return;
  }

  state.transcriptLines = [];

  for (const seg of data.segments) {
    state.transcriptLines.push({
      speaker: seg.speaker,
      text: seg.text,
      timestamp: seg.timestamp,
      is_local: seg.is_local || false,
      uncertain: seg.uncertain || false,
      uncertain_reason: seg.uncertain_reason || "",
      time: seg.ts_display || new Date(seg.ts).toLocaleTimeString('ru-RU', {
        hour: '2-digit', minute: '2-digit', second: '2-digit'
      }),
      createdAt: seg.ts ? new Date(seg.ts).getTime() : Date.now()
    });
  }

  if (state.transcriptLines.length > constants.MAX_TRANSCRIPT_LINES) {
    state.transcriptLines = state.transcriptLines.slice(-constants.MAX_TRANSCRIPT_LINES);
  }

  updateTranscriptUI();

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: state.transcriptLines.slice() }
  }));
}

export function handleNewMeeting(data) {
  console.log('[KTalk Transcript] New meeting:', data.meeting_id);

  state.transcriptLines = [];
  updateTranscriptUI();

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: [] }
  }));
}

export function handleTranscript(data) {
  const { speaker, text, timestamp, is_continuation, is_local, uncertain, uncertain_reason } = data;

  if (is_continuation && state.transcriptLines.length > 0) {
    state.transcriptLines[state.transcriptLines.length - 1].text += ' ' + text;
  } else {
    state.transcriptLines.push({
      speaker,
      text,
      timestamp,
      is_local: is_local || false,
      uncertain: uncertain || false,
      uncertain_reason: uncertain_reason || '',
      time: new Date().toLocaleTimeString('ru-RU', {
        hour: '2-digit', minute: '2-digit', second: '2-digit'
      }),
      createdAt: Date.now()
    });

    if (state.transcriptLines.length > constants.MAX_TRANSCRIPT_LINES) {
      state.transcriptLines.shift();
    }
  }

  updateTranscriptUI();

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: state.transcriptLines.slice() }
  }));
}

function copyAllTranscript() {
  if (state.transcriptLines.length === 0) return;

  const text = state.transcriptLines.map(line => {
    const speaker = line.speaker + getUncertainSuffix(line);
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

  const shouldAutoScroll = state.autoScrollEnabled && isNearBottom(container);

  updateFollowButton();

  if (state.transcriptLines.length === 0) {
    container.innerHTML = `
        <div class="ktalk-transcript-empty">
          <div class="ktalk-transcript-empty-icon">📝</div>
          <div class="ktalk-transcript-empty-text">
            Waiting for transcript...<br>
            Start recog with:
          </div>
          <div class="ktalk-transcript-empty-cmd">
            python mp4_notes.py live --screencapture --ktalk
          </div>
        </div>
      `;
    return;
  }

  container.innerHTML = state.transcriptLines.map(line => {
    const classes = [
      'ktalk-msg',
      line.is_local ? 'local' : 'remote',
      line.uncertain ? 'uncertain' : '',
      line.uncertain_reason === 'mics_off' ? 'mics-off' : ''
    ].filter(Boolean).join(' ');

    const speakerDisplay = line.speaker + getUncertainSuffix(line);

    const messageText = formatTranscriptText(line.text);

    return `<div class="${classes}">
        <div class="ktalk-msg-header">
          <span class="ktalk-msg-speaker">${speakerDisplay}</span>
          <span class="ktalk-msg-time">${line.time}</span>
        </div>
        <div class="ktalk-msg-text">${messageText}</div>
      </div>`;
  }).join('');

  if (shouldAutoScroll) {
    container.scrollTop = container.scrollHeight;
  }
}

function isNearBottom(container) {
  const thresholdPx = 80;
  const distance = container.scrollHeight - (container.scrollTop + container.clientHeight);
  return distance <= thresholdPx;
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

function updateSummaryMenu() {
  if (!state.transcriptContentElement) return;
  const toggle = state.transcriptContentElement.querySelector('.ktalk-summary-menu-toggle');
  const panel = state.transcriptContentElement.querySelector('.ktalk-summary-menu-panel');
  if (toggle) toggle.classList.toggle('active', summaryMenuOpen);
  if (panel) panel.classList.toggle('active', summaryMenuOpen);
}

function closeSummaryMenu() {
  if (!summaryMenuOpen) return;
  summaryMenuOpen = false;
  updateSummaryMenu();
}

function setupSummaryMenu(container) {
  const toggle = container.querySelector('.ktalk-summary-menu-toggle');
  const panel = container.querySelector('.ktalk-summary-menu-panel');

  summaryMenuOpen = false;

  if (toggle) {
    toggle.addEventListener('click', (event) => {
      event.stopPropagation();
      summaryMenuOpen = !summaryMenuOpen;
      updateSummaryMenu();
    });
  }

  if (panel) {
    panel.addEventListener('click', (event) => event.stopPropagation());
  }

  if (!summaryMenuListenerBound) {
    summaryMenuListenerBound = true;
    document.addEventListener('click', (event) => {
      if (!summaryMenuOpen) return;
      const activePanel = state.transcriptContentElement?.querySelector('.ktalk-summary-menu-panel');
      const activeToggle = state.transcriptContentElement?.querySelector('.ktalk-summary-menu-toggle');
      if (activePanel && activePanel.contains(event.target)) return;
      if (activeToggle && activeToggle.contains(event.target)) return;
      closeSummaryMenu();
    });
  }

  updateSummaryMenu();
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

function setupBioReadControls(container) {
  const toggleBtn = container.querySelector('.ktalk-bioread-toggle');
  const settingsBtn = container.querySelector('.ktalk-bioread-settings-btn');
  const panel = container.querySelector('.ktalk-bioread-panel');
  const fixationSlider = container.querySelector('.ktalk-bioread-fixation');
  const contrastSlider = container.querySelector('.ktalk-bioread-contrast');

  if (toggleBtn) {
    toggleBtn.addEventListener('click', () => {
      state.bioRead.enabled = !state.bioRead.enabled;
      applyBioReadSettings();
    });
  }

  if (settingsBtn) {
    settingsBtn.addEventListener('click', (event) => {
      event.stopPropagation();
      state.bioRead.panelOpen = !state.bioRead.panelOpen;
      updateBioReadControls();
    });
  }

  if (panel) {
    panel.addEventListener('click', (event) => event.stopPropagation());
  }

  if (fixationSlider) {
    fixationSlider.addEventListener('input', () => {
      state.bioRead.fixation = parseInt(fixationSlider.value, 10);
      applyBioReadSettings();
    });
  }

  if (contrastSlider) {
    contrastSlider.addEventListener('input', () => {
      state.bioRead.tailOpacity = parseInt(contrastSlider.value, 10);
      applyBioReadSettings();
    });
  }

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


function createSummaryModal() {
  if (document.querySelector('.ktalk-summary-modal')) return;

  state.summaryModalElement = document.createElement('div');
  state.summaryModalElement.className = 'ktalk-summary-modal';
  state.summaryModalElement.innerHTML = `
      <div class="ktalk-summary-modal-content">
        <div class="ktalk-summary-modal-header">
          <span class="ktalk-summary-modal-title">✨ Catch Up</span>
          <select class="ktalk-model-select">
            <option value="gemini-2.5-flash">2.5 Flash</option>
            <option value="gemini-2.5-flash-lite">2.5 Flash Lite</option>
            <option value="gemini-2.5-pro">2.5 Pro</option>
            <option value="gemini-2.0-flash">2.0 Flash</option>
            <option value="gemini-2.0-flash-lite">2.0 Lite</option>
            <option value="gemini-3-flash-preview">3.0 Flash</option>
            <option value="gemini-3-pro-preview">3.0 Pro</option>
          </select>
          <button class="ktalk-summary-modal-close">×</button>
        </div>
        <div class="ktalk-summary-modal-body loading">
          <div class="ktalk-summary-spinner"></div>
          <span>Анализирую транскрипт...</span>
        </div>
        <div class="ktalk-summary-question-input" style="display: none;">
          <input type="text" class="ktalk-question-field" placeholder="Задай уточняющий вопрос..." />
          <button class="ktalk-question-submit">→</button>
        </div>
        <div class="ktalk-summary-modal-footer">
          <button class="ktalk-summary-copy-btn">📋 Копировать</button>
        </div>
      </div>
    `;

  state.summaryModalElement.querySelector('.ktalk-summary-modal-close').addEventListener('click', closeSummaryModal);
  state.summaryModalElement.querySelector('.ktalk-summary-copy-btn').addEventListener('click', copySummary);

  state.summaryModalElement.querySelector('.ktalk-question-submit').addEventListener('click', askFollowUpQuestion);
  state.summaryModalElement.querySelector('.ktalk-question-field').addEventListener('keypress', (e) => {
    if (e.key === 'Enter') askFollowUpQuestion();
  });

  const modelSelect = state.summaryModalElement.querySelector('.ktalk-model-select');
  modelSelect.value = selectedModel;
  modelSelect.addEventListener('change', (e) => {
    selectedModel = e.target.value;
    localStorage.setItem('ktalk-ai-model', selectedModel);
  });

  const header = state.summaryModalElement.querySelector('.ktalk-summary-modal-header');
  let isDragging = false;
  let dragOffsetX = 0;
  let dragOffsetY = 0;

  header.addEventListener('mousedown', (e) => {
    if (e.target.classList.contains('ktalk-summary-modal-close')) return;
    isDragging = true;
    const rect = state.summaryModalElement.getBoundingClientRect();
    dragOffsetX = e.clientX - rect.left;
    dragOffsetY = e.clientY - rect.top;
    header.style.cursor = 'grabbing';
  });

  document.addEventListener('mousemove', (e) => {
    if (!isDragging) return;
    e.preventDefault();
    const x = e.clientX - dragOffsetX;
    const y = e.clientY - dragOffsetY;
    state.summaryModalElement.style.left = x + 'px';
    state.summaryModalElement.style.top = y + 'px';
    state.summaryModalElement.style.right = 'auto';
    state.summaryModalElement.style.bottom = 'auto';
  });

  document.addEventListener('mouseup', () => {
    if (isDragging) {
      isDragging = false;
      header.style.cursor = 'move';
    }
  });

  document.body.appendChild(state.summaryModalElement);
}

function showSummaryModal(summaryType = 'catchup') {
  if (!state.summaryModalElement) {
    createSummaryModal();
  }

  const titles = {
    catchup: '🎯 Catch Up',
    brief: '📋 Краткое (2 мин)',
    full: '💬 Вся встреча'
  };
  state.summaryModalElement.querySelector('.ktalk-summary-modal-title').textContent = titles[summaryType] || titles.catchup;

  const questionInput = state.summaryModalElement.querySelector('.ktalk-summary-question-input');
  questionInput.style.display = summaryType === 'full' ? 'flex' : 'none';

  state.summaryModalElement.classList.add('active');
  const body = state.summaryModalElement.querySelector('.ktalk-summary-modal-body');
  body.className = 'ktalk-summary-modal-body loading';
  body.innerHTML = `
      <div class="ktalk-summary-spinner"></div>
      <span>Анализирую транскрипт...</span>
    `;
}

function closeSummaryModal() {
  if (state.summaryModalElement) {
    state.summaryModalElement.classList.remove('active');
  }
}

function updateSummaryContent(text, isError = false) {
  if (!state.summaryModalElement) return;

  state.currentSummaryText = text;
  const body = state.summaryModalElement.querySelector('.ktalk-summary-modal-body');
  body.className = 'ktalk-summary-modal-body';

  if (isError) {
    body.innerHTML = `<div class="ktalk-summary-error">${text}</div>`;
  } else {
    body.innerHTML = `<div class="ktalk-summary-text">${renderMarkdown(text)}</div>`;
  }
}

function copySummary() {
  if (!state.currentSummaryText) return;

  navigator.clipboard.writeText(state.currentSummaryText).then(() => {
    const btn = state.summaryModalElement.querySelector('.ktalk-summary-copy-btn');
    if (btn) {
      btn.classList.add('copied');
      btn.textContent = '✓ Скопировано';
      setTimeout(() => {
        btn.classList.remove('copied');
        btn.textContent = '📋 Копировать';
      }, 2000);
    }
  });
}

function requestSummary(summaryType = 'catchup') {
  if (state.summaryRequestPending) return;
  if (state.transcriptLines.length === 0) {
    alert('Нет транскрипта для анализа');
    return;
  }

  state.currentSummaryType = summaryType;
  let linesToSend;

  if (summaryType === 'catchup') {
    const oneMinuteAgo = Date.now() - 60000;
    const recentLines = state.transcriptLines.filter(line => line.createdAt && line.createdAt >= oneMinuteAgo);
    linesToSend = recentLines.length > 0 ? recentLines : state.transcriptLines.slice(-10);
  } else if (summaryType === 'brief') {
    const twoMinutesAgo = Date.now() - 120000;
    const recentLines = state.transcriptLines.filter(line => line.createdAt && line.createdAt >= twoMinutesAgo);
    linesToSend = recentLines.length > 0 ? recentLines : state.transcriptLines.slice(-20);
  } else {
    linesToSend = state.transcriptLines;
  }

  if (linesToSend.length === 0) {
    alert('Нет данных для анализа');
    return;
  }

  const transcriptText = linesToSend.map(line => {
    const speaker = line.speaker + getUncertainSuffix(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  showSummaryModal(summaryType);
  state.summaryRequestPending = true;

  document.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.classList.add('loading');
    btn.disabled = true;
  });

  sendWsMessage({
    type: 'summarize_request',
    summary_type: summaryType,
    transcript: transcriptText,
    lines_count: linesToSend.length,
    model: selectedModel,
    meeting_title: meetingTitle
  });
}

function askFollowUpQuestion() {
  const input = state.summaryModalElement.querySelector('.ktalk-question-field');
  const question = input.value.trim();
  if (!question) return;

  const transcriptText = state.transcriptLines.map(line => {
    const speaker = line.speaker + getUncertainSuffix(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  const body = state.summaryModalElement.querySelector('.ktalk-summary-modal-body');
  body.innerHTML = `
      <div class="ktalk-summary-spinner"></div>
      <span>Отвечаю на вопрос...</span>
    `;
  body.classList.add('loading');

  const submitBtn = state.summaryModalElement.querySelector('.ktalk-question-submit');
  submitBtn.disabled = true;

  sendWsMessage({
    type: 'summarize_request',
    summary_type: 'question',
    transcript: transcriptText,
    question: question,
    previous_summary: state.currentSummaryText,
    model: selectedModel,
    meeting_title: meetingTitle
  });

  input.value = '';
}

export function handleSummaryResponse(data) {
  state.summaryRequestPending = false;

  document.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.classList.remove('loading');
    btn.disabled = false;
  });

  const submitBtn = state.summaryModalElement?.querySelector('.ktalk-question-submit');
  if (submitBtn) submitBtn.disabled = false;

  if (data.error) {
    updateSummaryContent('Ошибка: ' + data.error, true);
  } else if (data.summary) {
    updateSummaryContent(data.summary);
  }
}
