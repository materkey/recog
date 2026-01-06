/**
 * Floating Panel UI for vas3k.cloud (Standard Jitsi Meet)
 *
 * Creates a draggable floating panel for transcript display
 * since vas3k.cloud doesn't have KTalk's tab system.
 */

import { state } from './state.js';
import { getUncertainSuffix, escapeHtml } from './utils.js';

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

  // Auto-scroll to bottom
  content.scrollTop = content.scrollHeight;
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
