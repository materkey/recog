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

// Optional hook invoked when the user clicks the Copilot sub-tab. Registered
// by ui.js (which owns the copilot subsystem) so a tab click can lazily build
// the copilot content and focus its input. ui-vas3k must not import ui.js
// (ui.js already imports ui-vas3k — the reverse would be a cycle), so the
// dependency is inverted through this setter.
let copilotActivateHandler = null;

export function setVas3kCopilotActivateHandler(fn) {
  copilotActivateHandler = fn;
}

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
      gap: 4px;
      flex-wrap: wrap;
      justify-content: flex-end;
    }

    .jitsi-transcript-header button {
      background: transparent;
      border: none;
      color: rgba(255, 255, 255, 0.7);
      cursor: pointer;
      padding: 4px 6px;
      border-radius: 4px;
      font-size: 11px;
      transition: background 0.15s, color 0.15s;
      white-space: nowrap;
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

    /* Sub-tab bar: Transcript | Copilot */
    .jitsi-panel-tabs {
      display: flex;
      gap: 4px;
      padding: 6px 10px;
      background: rgba(45, 45, 45, 0.9);
      border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    }

    .jitsi-panel-tab {
      display: flex;
      align-items: center;
      gap: 5px;
      background: transparent;
      border: none;
      color: rgba(255, 255, 255, 0.6);
      cursor: pointer;
      padding: 5px 10px;
      border-radius: 6px;
      font-size: 12px;
      font-weight: 600;
      transition: background 0.15s, color 0.15s;
    }

    .jitsi-panel-tab:hover {
      background: rgba(255, 255, 255, 0.08);
      color: rgba(255, 255, 255, 0.9);
    }

    .jitsi-panel-tab.active {
      background: rgba(100, 181, 246, 0.25);
      color: #64B5F6;
    }

    /* Unread marker on the Copilot sub-tab. Self-contained so it renders in
       the tab bar even before the copilot content (and ui.js styles) mount. */
    .jitsi-panel-tab .ktalk-ai-unread {
      width: 6px;
      height: 6px;
      border-radius: 50%;
      background: #4ade80;
      display: inline-block;
    }

    .jitsi-panel-tab .ktalk-ai-unread[hidden] {
      display: none;
    }

    /* Copilot chat host inside the floating panel. The reused
       .ktalk-copilot-content element mounts here; its own styles come from
       ui.js injectStyles() (called by createCopilotContent on this platform). */
    .jitsi-copilot-host {
      flex: 1;
      min-height: 0;
      max-height: 340px;
      display: none;
      flex-direction: column;
      overflow: hidden;
    }

    .jitsi-copilot-host .ktalk-copilot-content.active {
      flex: 1;
      min-height: 0;
    }

    /* Highlighted transcript line */
    .jitsi-transcript-line.highlighted {
      background: rgba(255, 235, 59, 0.2);
      border-left: 3px solid #FFEB3B;
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

    /* Tier-2 refine pulse: brief fade to hint that text was rewritten. */
    .jitsi-transcript-text.refine-fade {
      transition: opacity 100ms;
      opacity: 0.3;
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
        <button class="jitsi-follow-btn" title="Автоскролл">Follow</button>
        <button class="jitsi-scroll-up-btn" title="В начало">⬆</button>
        <button class="jitsi-scroll-down-btn" title="В конец">⬇</button>
        <button class="jitsi-collapse-btn" title="Свернуть">−</button>
        <button class="jitsi-close-btn" title="Скрыть">×</button>
      </div>
    </div>
    <div class="jitsi-panel-tabs">
      <button type="button" class="jitsi-panel-tab active" data-view="transcript">Транскрипт</button>
      <button type="button" class="jitsi-panel-tab" data-view="copilot">Copilot<span class="ktalk-ai-unread" hidden></span></button>
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
    <div class="jitsi-copilot-host"></div>
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

  // Sub-tab bar: Transcript | Copilot. Clicking a tab switches the panel's
  // active view; the Copilot content is built lazily on first activation via
  // the handler ui.js registers (it owns the copilot subsystem).
  panel.querySelectorAll('.jitsi-panel-tab').forEach((tab) => {
    tab.addEventListener('click', () => setVas3kView(tab.dataset.view));
  });

  // Store reference
  state.transcriptContentElement = panel.querySelector('.jitsi-transcript-content');

  return true;
}

/**
 * Ensure the floating panel exists and return its copilot host container.
 * ui.js's createCopilotContent() mounts the shared `.ktalk-copilot-content`
 * DOM into this host on floating-panel platforms (vas3k / matrix).
 */
export function ensureVas3kCopilotHost() {
  let host = document.querySelector('.jitsi-transcript-panel .jitsi-copilot-host');
  if (!host) {
    createFloatingPanel();
    host = document.querySelector('.jitsi-transcript-panel .jitsi-copilot-host');
  }
  return host;
}

/**
 * Switch the floating panel between the Transcript and Copilot views.
 * Panel-level DOM (tab highlight, content visibility) is handled here;
 * copilot-subsystem state (lazy content build, active class, unread reset,
 * input focus) is delegated to the handler ui.js registers, so ui-vas3k
 * stays free of a ui.js import (which would form a cycle).
 */
export function setVas3kView(view) {
  const panel = document.querySelector('.jitsi-transcript-panel');
  if (!panel) return;

  const isCopilot = view === 'copilot';
  state.vas3kActiveView = view;

  panel.querySelectorAll('.jitsi-panel-tab').forEach((btn) => {
    btn.classList.toggle('active', btn.dataset.view === view);
  });

  const transcript = panel.querySelector('.jitsi-transcript-content');
  const host = panel.querySelector('.jitsi-copilot-host');
  const speakerStatus = panel.querySelector('.jitsi-speaker-status');
  if (transcript) transcript.style.display = isCopilot ? 'none' : '';
  if (host) host.style.display = isCopilot ? 'flex' : 'none';
  if (speakerStatus) speakerStatus.style.display = isCopilot ? 'none' : '';

  if (copilotActivateHandler) copilotActivateHandler(view);
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
  if (line.segment_id && line.segment_id > 0) {
    lineEl.dataset.segmentId = line.segment_id;
  }
  if (line.timestamp) {
    lineEl.dataset.timestamp = line.timestamp;
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
