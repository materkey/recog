/**
 * Floating Panel UI for vas3k.cloud (Standard Jitsi Meet)
 *
 * Creates a draggable floating panel for transcript display
 * since vas3k.cloud doesn't have KTalk's tab system.
 */

import { state } from './state.js';
import { getUncertainSuffix, escapeHtml } from './utils.js';
import { sendWsMessage } from './ws_api.js';

let panelVisible = true;
let isDragging = false;
let dragOffset = { x: 0, y: 0 };

/**
 * Inject styles for the floating panel
 */
export function injectVas3kStyles() {
  if (document.querySelector('#jitsi-transcript-styles')) return;

  const styles = document.createElement('style');
  styles.id = 'jitsi-transcript-styles';
  styles.textContent = `
    /* Floating panel container */
    .jitsi-transcript-panel {
      position: fixed;
      bottom: 80px;
      right: 20px;
      width: 360px;
      max-height: 400px;
      background: rgba(30, 30, 30, 0.95);
      border-radius: 12px;
      box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
      z-index: 10000;
      display: flex;
      flex-direction: column;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      color: rgba(255, 255, 255, 0.9);
      overflow: hidden;
      transition: opacity 0.2s, transform 0.2s;
    }

    .jitsi-transcript-panel.collapsed {
      max-height: 44px;
    }

    .jitsi-transcript-panel.hidden {
      opacity: 0;
      transform: translateY(20px);
      pointer-events: none;
    }

    /* Panel header - draggable */
    .jitsi-transcript-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 10px 14px;
      background: rgba(50, 50, 50, 0.9);
      border-bottom: 1px solid rgba(255, 255, 255, 0.1);
      cursor: move;
      user-select: none;
    }

    .jitsi-transcript-header h3 {
      margin: 0;
      font-size: 13px;
      font-weight: 600;
      display: flex;
      align-items: center;
      gap: 8px;
    }

    .jitsi-transcript-header-controls {
      display: flex;
      gap: 6px;
    }

    .jitsi-transcript-header button {
      background: transparent;
      border: none;
      color: rgba(255, 255, 255, 0.7);
      cursor: pointer;
      padding: 4px 8px;
      border-radius: 4px;
      font-size: 12px;
      transition: background 0.15s, color 0.15s;
    }

    .jitsi-transcript-header button:hover {
      background: rgba(255, 255, 255, 0.1);
      color: rgba(255, 255, 255, 0.95);
    }

    .jitsi-transcript-header button.copied {
      background: rgba(76, 175, 80, 0.3);
      color: #4CAF50;
    }

    .jitsi-transcript-header button.active {
      background: rgba(100, 181, 246, 0.3);
      color: #64B5F6;
    }

    /* Highlighted transcript line */
    .jitsi-transcript-line.highlighted {
      background: rgba(255, 235, 59, 0.2);
      border-left: 3px solid #FFEB3B;
    }

    /* Summary Modal */
    .jitsi-summary-modal {
      position: fixed;
      top: 50%;
      left: 50%;
      transform: translate(-50%, -50%);
      width: 450px;
      max-height: 500px;
      background: rgba(30, 30, 30, 0.98);
      border-radius: 12px;
      box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
      z-index: 10001;
      display: flex;
      flex-direction: column;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      color: rgba(255, 255, 255, 0.9);
      overflow: hidden;
    }

    .jitsi-summary-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 12px 16px;
      background: rgba(50, 50, 50, 0.9);
      border-bottom: 1px solid rgba(255, 255, 255, 0.1);
      cursor: move;
    }

    .jitsi-summary-header h3 {
      margin: 0;
      font-size: 14px;
      font-weight: 600;
    }

    .jitsi-summary-close {
      background: transparent;
      border: none;
      color: rgba(255, 255, 255, 0.7);
      cursor: pointer;
      font-size: 18px;
      padding: 4px 8px;
    }

    .jitsi-summary-types {
      display: flex;
      gap: 8px;
      padding: 12px 16px;
      border-bottom: 1px solid rgba(255, 255, 255, 0.1);
    }

    .jitsi-summary-btn {
      flex: 1;
      padding: 8px 12px;
      background: rgba(255, 255, 255, 0.1);
      border: none;
      border-radius: 6px;
      color: rgba(255, 255, 255, 0.8);
      cursor: pointer;
      font-size: 12px;
      transition: background 0.15s;
    }

    .jitsi-summary-btn:hover {
      background: rgba(255, 255, 255, 0.2);
    }

    .jitsi-summary-btn.active {
      background: rgba(100, 181, 246, 0.3);
      color: #64B5F6;
    }

    .jitsi-summary-content {
      flex: 1;
      padding: 16px;
      overflow-y: auto;
      max-height: 300px;
      font-size: 13px;
      line-height: 1.5;
    }

    .jitsi-summary-footer {
      display: flex;
      gap: 8px;
      padding: 12px 16px;
      border-top: 1px solid rgba(255, 255, 255, 0.1);
    }

    .jitsi-summary-copy-btn {
      padding: 8px 16px;
      background: rgba(76, 175, 80, 0.2);
      border: none;
      border-radius: 6px;
      color: #4CAF50;
      cursor: pointer;
      font-size: 12px;
    }

    .jitsi-model-selector {
      flex: 1;
      padding: 8px 12px;
      background: rgba(255, 255, 255, 0.1);
      border: 1px solid rgba(255, 255, 255, 0.2);
      border-radius: 6px;
      color: rgba(255, 255, 255, 0.9);
      font-size: 12px;
      cursor: pointer;
    }

    .jitsi-model-selector option {
      background: #1a1a2e;
      color: rgba(255, 255, 255, 0.9);
    }

    .jitsi-summary-question-wrapper {
      display: none;
      padding: 12px 16px;
      border-top: 1px solid rgba(255, 255, 255, 0.1);
    }

    .jitsi-summary-question-wrapper.visible {
      display: block;
    }

    .jitsi-summary-question-input {
      width: 100%;
      padding: 8px 12px;
      background: rgba(255, 255, 255, 0.1);
      border: 1px solid rgba(255, 255, 255, 0.2);
      border-radius: 6px;
      color: rgba(255, 255, 255, 0.9);
      font-size: 13px;
    }

    .jitsi-summary-question-input::placeholder {
      color: rgba(255, 255, 255, 0.4);
    }

    /* Connection indicator */
    .jitsi-ws-indicator {
      width: 8px;
      height: 8px;
      border-radius: 50%;
      background: #ff4444;
    }

    .jitsi-ws-indicator.connected {
      background: #44ff44;
    }

    /* Speaker status */
    .jitsi-speaker-status {
      display: flex;
      align-items: center;
      gap: 8px;
      padding: 8px 14px;
      background: rgba(40, 40, 40, 0.9);
      border-bottom: 1px solid rgba(255, 255, 255, 0.05);
      font-size: 12px;
    }

    .jitsi-speaker-dot {
      width: 10px;
      height: 10px;
      border-radius: 50%;
      background: rgba(255, 255, 255, 0.3);
      transition: background 0.2s, box-shadow 0.2s;
    }

    .jitsi-speaker-dot.speaking {
      background: #4CAF50;
      box-shadow: 0 0 8px rgba(76, 175, 80, 0.6);
    }

    .jitsi-speaker-name {
      flex: 1;
      color: rgba(255, 255, 255, 0.8);
    }

    /* Transcript content area */
    .jitsi-transcript-content {
      flex: 1;
      overflow-y: auto;
      padding: 10px;
      max-height: 300px;
    }

    .jitsi-transcript-content::-webkit-scrollbar {
      width: 6px;
    }

    .jitsi-transcript-content::-webkit-scrollbar-track {
      background: transparent;
    }

    .jitsi-transcript-content::-webkit-scrollbar-thumb {
      background: rgba(255, 255, 255, 0.2);
      border-radius: 3px;
    }

    /* Transcript line */
    .jitsi-transcript-line {
      margin-bottom: 10px;
      padding: 8px 10px;
      background: rgba(255, 255, 255, 0.05);
      border-radius: 6px;
      font-size: 13px;
      line-height: 1.4;
    }

    .jitsi-transcript-line-header {
      display: flex;
      align-items: baseline;
      gap: 8px;
      margin-bottom: 4px;
    }

    .jitsi-transcript-speaker {
      font-weight: 600;
      color: #64B5F6;
    }

    .jitsi-transcript-time {
      font-size: 11px;
      color: rgba(255, 255, 255, 0.4);
    }

    .jitsi-transcript-text {
      color: rgba(255, 255, 255, 0.85);
    }

    .jitsi-transcript-uncertain {
      color: rgba(255, 255, 255, 0.5);
    }

    /* Empty state */
    .jitsi-transcript-empty {
      text-align: center;
      padding: 30px 20px;
      color: rgba(255, 255, 255, 0.4);
      font-size: 13px;
    }

    /* Toggle button (when panel is hidden) */
    .jitsi-transcript-toggle {
      position: fixed;
      bottom: 80px;
      right: 20px;
      width: 48px;
      height: 48px;
      border-radius: 50%;
      background: rgba(30, 30, 30, 0.95);
      border: none;
      color: rgba(255, 255, 255, 0.8);
      cursor: pointer;
      z-index: 9999;
      display: flex;
      align-items: center;
      justify-content: center;
      box-shadow: 0 2px 10px rgba(0, 0, 0, 0.3);
      transition: background 0.15s, transform 0.15s;
    }

    .jitsi-transcript-toggle:hover {
      background: rgba(50, 50, 50, 0.95);
      transform: scale(1.05);
    }

    .jitsi-transcript-toggle svg {
      width: 24px;
      height: 24px;
    }
  `;

  document.head.appendChild(styles);
}

/**
 * Create the floating panel
 */
export function createFloatingPanel() {
  if (document.querySelector('.jitsi-transcript-panel')) return true;

  injectVas3kStyles();

  const panel = document.createElement('div');
  panel.className = 'jitsi-transcript-panel';
  panel.innerHTML = `
    <div class="jitsi-transcript-header">
      <h3>
        <span class="jitsi-ws-indicator"></span>
        Транскрипт
      </h3>
      <div class="jitsi-transcript-header-controls">
        <button class="jitsi-copy-btn" title="Копировать">Copy</button>
        <button class="jitsi-ai-btn" title="AI Summary">AI</button>
        <button class="jitsi-follow-btn" title="Автоскролл">Follow</button>
        <button class="jitsi-scroll-up-btn" title="В начало">⬆</button>
        <button class="jitsi-scroll-down-btn" title="В конец">⬇</button>
        <button class="jitsi-collapse-btn" title="Свернуть">−</button>
        <button class="jitsi-close-btn" title="Скрыть">×</button>
      </div>
    </div>
    <div class="jitsi-speaker-status">
      <span class="jitsi-speaker-dot"></span>
      <span class="jitsi-speaker-name">Ожидание подключения...</span>
    </div>
    <div class="jitsi-transcript-content">
      <div class="jitsi-transcript-empty">
        Транскрипт появится здесь, когда начнётся речь
      </div>
    </div>
  `;

  document.body.appendChild(panel);

  // Create toggle button (hidden initially)
  const toggleBtn = document.createElement('button');
  toggleBtn.className = 'jitsi-transcript-toggle';
  toggleBtn.style.display = 'none';
  toggleBtn.innerHTML = `
    <svg viewBox="0 0 24 24" fill="currentColor">
      <path d="M20 2H4c-1.1 0-2 .9-2 2v18l4-4h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm0 14H6l-2 2V4h16v12z"/>
    </svg>
  `;
  toggleBtn.title = 'Показать транскрипт';
  document.body.appendChild(toggleBtn);

  // Set up drag functionality
  setupDrag(panel);

  // Set up button handlers
  const collapseBtn = panel.querySelector('.jitsi-collapse-btn');
  const closeBtn = panel.querySelector('.jitsi-close-btn');

  collapseBtn.addEventListener('click', () => {
    panel.classList.toggle('collapsed');
    collapseBtn.textContent = panel.classList.contains('collapsed') ? '+' : '−';
  });

  const copyBtn = panel.querySelector('.jitsi-copy-btn');
  copyBtn.addEventListener('click', () => {
    copyVas3kTranscript();
  });

  const aiBtn = panel.querySelector('.jitsi-ai-btn');
  aiBtn.addEventListener('click', () => {
    createVas3kSummaryModal();
  });

  const followBtn = panel.querySelector('.jitsi-follow-btn');
  // Initialize follow state
  if (state.vas3kFollowEnabled === undefined) {
    state.vas3kFollowEnabled = true;
  }
  updateFollowButtonState(followBtn);
  followBtn.addEventListener('click', () => {
    toggleVas3kFollow();
  });

  const scrollUpBtn = panel.querySelector('.jitsi-scroll-up-btn');
  scrollUpBtn.addEventListener('click', () => {
    const content = panel.querySelector('.jitsi-transcript-content');
    if (content) content.scrollTop = 0;
  });

  const scrollDownBtn = panel.querySelector('.jitsi-scroll-down-btn');
  scrollDownBtn.addEventListener('click', () => {
    const content = panel.querySelector('.jitsi-transcript-content');
    if (content) content.scrollTop = content.scrollHeight - content.clientHeight;
  });

  closeBtn.addEventListener('click', () => {
    panel.classList.add('hidden');
    toggleBtn.style.display = 'flex';
    panelVisible = false;
  });

  toggleBtn.addEventListener('click', () => {
    panel.classList.remove('hidden');
    toggleBtn.style.display = 'none';
    panelVisible = true;
  });

  // Store reference
  state.transcriptContentElement = panel.querySelector('.jitsi-transcript-content');

  return true;
}

/**
 * Set up drag functionality for the panel
 */
function setupDrag(panel) {
  const header = panel.querySelector('.jitsi-transcript-header');

  header.addEventListener('mousedown', (e) => {
    if (e.target.tagName === 'BUTTON') return;

    isDragging = true;
    const rect = panel.getBoundingClientRect();
    dragOffset.x = e.clientX - rect.left;
    dragOffset.y = e.clientY - rect.top;

    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
  });

  function onMouseMove(e) {
    if (!isDragging) return;

    const x = e.clientX - dragOffset.x;
    const y = e.clientY - dragOffset.y;

    // Keep panel within viewport
    const maxX = window.innerWidth - panel.offsetWidth;
    const maxY = window.innerHeight - panel.offsetHeight;

    panel.style.left = Math.max(0, Math.min(x, maxX)) + 'px';
    panel.style.top = Math.max(0, Math.min(y, maxY)) + 'px';
    panel.style.right = 'auto';
    panel.style.bottom = 'auto';
  }

  function onMouseUp() {
    isDragging = false;
    document.removeEventListener('mousemove', onMouseMove);
    document.removeEventListener('mouseup', onMouseUp);
  }
}

/**
 * Update WebSocket connection indicator
 */
export function updateVas3kWsIndicator(connected) {
  const indicator = document.querySelector('.jitsi-ws-indicator');
  if (indicator) {
    indicator.classList.toggle('connected', connected);
  }
}

/**
 * Update speaker status display
 */
export function updateVas3kSpeakerStatus(speaker, micsOn) {
  const dot = document.querySelector('.jitsi-speaker-dot');
  const nameEl = document.querySelector('.jitsi-speaker-name');

  if (!dot || !nameEl) return;

  if (speaker && !speaker.micMuted) {
    dot.classList.add('speaking');
    nameEl.textContent = speaker.name;
  } else {
    dot.classList.remove('speaking');
    if (!micsOn) {
      nameEl.textContent = 'Все микрофоны выключены';
    } else {
      nameEl.textContent = 'Никто не говорит';
    }
  }
}

/**
 * Show prejoin state message
 */
export function showVas3kPrejoinState() {
  const nameEl = document.querySelector('.jitsi-speaker-name');
  if (nameEl) {
    nameEl.textContent = 'Нажмите "Присоединиться" для начала';
  }
}

/**
 * Show waiting for conference message
 */
export function showVas3kWaitingState() {
  const nameEl = document.querySelector('.jitsi-speaker-name');
  if (nameEl) {
    nameEl.textContent = 'Ожидание подключения...';
  }
}

/**
 * Add transcript line to the panel
 */
export function addVas3kTranscriptLine(line) {
  const content = document.querySelector('.jitsi-transcript-content');
  if (!content) return;

  // Remove empty state message
  const emptyMsg = content.querySelector('.jitsi-transcript-empty');
  if (emptyMsg) {
    emptyMsg.remove();
  }

  const lineEl = document.createElement('div');
  lineEl.className = 'jitsi-transcript-line';
  if (line.id) {
    lineEl.dataset.lineId = line.id;
  }

  const uncertainSuffix = getUncertainSuffix(line);

  lineEl.innerHTML = `
    <div class="jitsi-transcript-line-header">
      <span class="jitsi-transcript-speaker">${escapeHtml(line.speaker || 'Unknown')}</span>
      <span class="jitsi-transcript-time">${line.time || ''}</span>
    </div>
    <div class="jitsi-transcript-text ${line.uncertain ? 'jitsi-transcript-uncertain' : ''}">
      ${escapeHtml(line.text || '')}${uncertainSuffix}
    </div>
  `;

  content.appendChild(lineEl);

  // Auto-scroll to bottom only if follow is enabled (default true)
  if (state.vas3kFollowEnabled !== false) {
    content.scrollTop = content.scrollHeight;
  }
}

/**
 * Create AI Summary modal
 */
export function createVas3kSummaryModal() {
  // Remove existing modal if any
  const existing = document.querySelector('.jitsi-summary-modal');
  if (existing) existing.remove();

  const modal = document.createElement('div');
  modal.className = 'jitsi-summary-modal';
  modal.innerHTML = `
    <div class="jitsi-summary-header">
      <h3>AI Summary</h3>
      <button class="jitsi-summary-close">×</button>
    </div>
    <div class="jitsi-summary-types">
      <button class="jitsi-summary-btn" data-type="catchup">Catch-up (1 мин)</button>
      <button class="jitsi-summary-btn" data-type="brief">Кратко (2 мин)</button>
      <button class="jitsi-summary-btn" data-type="full">Полный</button>
    </div>
    <div class="jitsi-summary-content">
      <p style="color: rgba(255,255,255,0.5)">Выберите тип summary выше</p>
    </div>
    <div class="jitsi-summary-question-wrapper">
      <input type="text" class="jitsi-summary-question-input" placeholder="Задайте вопрос по транскрипту...">
    </div>
    <div class="jitsi-summary-footer">
      <select class="jitsi-model-selector">
        <option value="codex">Codex (GPT-5.2)</option>
        <option value="codex:gpt-4.1">Codex (GPT-4.1)</option>
        <option value="claude:opus">Claude Opus</option>
        <option value="claude:sonnet">Claude Sonnet</option>
        <option value="gemini-2.5-flash">Gemini Flash</option>
        <option value="gemini-2.5-pro">Gemini Pro</option>
      </select>
      <button class="jitsi-summary-copy-btn">Copy</button>
    </div>
  `;

  document.body.appendChild(modal);

  // Close button handler
  const closeBtn = modal.querySelector('.jitsi-summary-close');
  closeBtn.addEventListener('click', () => {
    modal.remove();
  });

  // Summary type buttons
  const typeButtons = modal.querySelectorAll('.jitsi-summary-btn');
  typeButtons.forEach(btn => {
    btn.addEventListener('click', () => {
      // Remove active from all
      typeButtons.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');

      const type = btn.dataset.type;
      const questionWrapper = modal.querySelector('.jitsi-summary-question-wrapper');

      // Show question input only for "full" type
      if (type === 'full') {
        questionWrapper.classList.add('visible');
      } else {
        questionWrapper.classList.remove('visible');
      }

      // TODO: Request summary via WebSocket
      requestVas3kSummary(type);
    });
  });

  // Copy button handler
  const copyBtn = modal.querySelector('.jitsi-summary-copy-btn');
  copyBtn.addEventListener('click', () => {
    const content = modal.querySelector('.jitsi-summary-content');
    navigator.clipboard.writeText(content.textContent || '');
    copyBtn.textContent = '✓';
    setTimeout(() => {
      copyBtn.textContent = 'Copy';
    }, 2000);
  });

  // Question input Enter handler
  const questionInput = modal.querySelector('.jitsi-summary-question-input');
  questionInput.addEventListener('keypress', (e) => {
    if (e.key === 'Enter') {
      askVas3kFollowUp();
    }
  });

  // Model selector handler
  const modelSelector = modal.querySelector('.jitsi-model-selector');
  const currentModel = localStorage.getItem('ktalk-ai-model') || 'codex';
  modelSelector.value = currentModel;
  modelSelector.addEventListener('change', () => {
    localStorage.setItem('ktalk-ai-model', modelSelector.value);
  });

  // Setup drag for modal
  setupModalDrag(modal);

  return modal;
}

/**
 * Setup drag functionality for modal
 */
function setupModalDrag(modal) {
  const header = modal.querySelector('.jitsi-summary-header');
  let modalDragging = false;
  let modalDragOffset = { x: 0, y: 0 };

  header.addEventListener('mousedown', (e) => {
    if (e.target.tagName === 'BUTTON') return;

    modalDragging = true;
    const rect = modal.getBoundingClientRect();
    modalDragOffset.x = e.clientX - rect.left;
    modalDragOffset.y = e.clientY - rect.top;

    // Remove transform to use absolute positioning
    modal.style.transform = 'none';
    modal.style.left = rect.left + 'px';
    modal.style.top = rect.top + 'px';

    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
  });

  function onMouseMove(e) {
    if (!modalDragging) return;

    const x = e.clientX - modalDragOffset.x;
    const y = e.clientY - modalDragOffset.y;

    // Keep modal within viewport
    const maxX = window.innerWidth - modal.offsetWidth;
    const maxY = window.innerHeight - modal.offsetHeight;

    modal.style.left = Math.max(0, Math.min(x, maxX)) + 'px';
    modal.style.top = Math.max(0, Math.min(y, maxY)) + 'px';
  }

  function onMouseUp() {
    modalDragging = false;
    document.removeEventListener('mousemove', onMouseMove);
    document.removeEventListener('mouseup', onMouseUp);
  }
}

/**
 * Request summary from backend
 */
function requestVas3kSummary(type) {
  if (state.transcriptLines.length === 0) {
    const content = document.querySelector('.jitsi-summary-content');
    if (content) {
      content.innerHTML = '<p style="color: #ff6b6b">Нет транскрипта для анализа</p>';
    }
    return;
  }

  state.currentSummaryType = type;
  let linesToSend;

  if (type === 'catchup') {
    const oneMinuteAgo = Date.now() - 60000;
    const recentLines = state.transcriptLines.filter(line => line.createdAt && line.createdAt >= oneMinuteAgo);
    linesToSend = recentLines.length > 0 ? recentLines : state.transcriptLines.slice(-10);
  } else if (type === 'brief') {
    const twoMinutesAgo = Date.now() - 120000;
    const recentLines = state.transcriptLines.filter(line => line.createdAt && line.createdAt >= twoMinutesAgo);
    linesToSend = recentLines.length > 0 ? recentLines : state.transcriptLines.slice(-20);
  } else {
    linesToSend = state.transcriptLines;
  }

  const transcriptText = linesToSend.map(line => {
    const speaker = line.speaker + getUncertainSuffix(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  const content = document.querySelector('.jitsi-summary-content');
  if (content) {
    content.innerHTML = '<p style="color: rgba(255,255,255,0.5)">⏳ Генерация summary...</p>';
  }

  sendWsMessage({
    type: 'summarize_request',
    summary_type: type,
    transcript: transcriptText,
    lines_count: linesToSend.length,
    model: localStorage.getItem('ktalk-ai-model') || 'codex',
    meeting_title: document.title || ''
  });
}

/**
 * Ask follow-up question
 */
function askVas3kFollowUp() {
  const input = document.querySelector('.jitsi-summary-question-input');
  const question = input?.value?.trim();
  if (!question) return;

  const transcriptText = state.transcriptLines.map(line => {
    const speaker = line.speaker + getUncertainSuffix(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  const content = document.querySelector('.jitsi-summary-content');
  if (content) {
    content.innerHTML = '<p style="color: rgba(255,255,255,0.5)">⏳ Отвечаю на вопрос...</p>';
  }

  sendWsMessage({
    type: 'summarize_request',
    summary_type: 'question',
    transcript: transcriptText,
    question: question,
    previous_summary: state.currentSummaryText || '',
    model: localStorage.getItem('ktalk-ai-model') || 'codex',
    meeting_title: document.title || ''
  });

  input.value = '';
}

/**
 * Handle summary response from backend
 */
export function handleVas3kSummaryResponse(data) {
  const content = document.querySelector('.jitsi-summary-content');
  if (!content) return;

  if (data.error) {
    content.innerHTML = `<p style="color: #ff6b6b">Ошибка: ${escapeHtml(data.error)}</p>`;
    return;
  }

  state.currentSummaryText = data.summary || '';
  content.innerHTML = `<div style="white-space: pre-wrap">${escapeHtml(data.summary || 'Нет данных')}</div>`;
}

/**
 * Update follow button visual state
 */
function updateFollowButtonState(btn) {
  if (!btn) btn = document.querySelector('.jitsi-follow-btn');
  if (!btn) return;

  if (state.vas3kFollowEnabled) {
    btn.classList.add('active');
  } else {
    btn.classList.remove('active');
  }
}

/**
 * Toggle follow/auto-scroll mode
 */
export function toggleVas3kFollow() {
  state.vas3kFollowEnabled = !state.vas3kFollowEnabled;
  updateFollowButtonState();
}

/**
 * Highlight a specific transcript line by ID
 */
export function highlightVas3kLine(lineId) {
  const lines = document.querySelectorAll('.jitsi-transcript-line');
  lines.forEach((line, index) => {
    const dataId = line.dataset.lineId;
    if (dataId == lineId || index + 1 == lineId) {
      line.classList.add('highlighted');
      line.scrollIntoView({ behavior: 'smooth', block: 'center' });

      // Remove highlight after 2 seconds
      setTimeout(() => {
        line.classList.remove('highlighted');
      }, 2000);
    }
  });
}

/**
 * Copy all transcript to clipboard
 */
export async function copyVas3kTranscript() {
  if (state.transcriptLines.length === 0) return;

  const text = state.transcriptLines.map(line => {
    const speaker = line.speaker || 'Unknown';
    const time = line.time || '';
    const content = line.text || '';
    return `[${time}] ${speaker}: ${content}`;
  }).join('\n');

  await navigator.clipboard.writeText(text);

  // Show visual feedback
  const copyBtn = document.querySelector('.jitsi-copy-btn');
  if (copyBtn) {
    copyBtn.classList.add('copied');
    copyBtn.textContent = '✓';
    setTimeout(() => {
      copyBtn.classList.remove('copied');
      copyBtn.textContent = 'Copy';
    }, 2000);
  }
}

/**
 * Clear transcript content
 */
export function clearVas3kTranscript() {
  const content = document.querySelector('.jitsi-transcript-content');
  if (!content) return;

  content.innerHTML = `
    <div class="jitsi-transcript-empty">
      Транскрипт появится здесь, когда начнётся речь
    </div>
  `;
}

/**
 * Initialize the vas3k.cloud floating panel UI
 */
export function initVas3kUI() {
  injectVas3kStyles();
  return createFloatingPanel();
}
