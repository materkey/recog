import { state } from '../state.js';
import { escapeHtml } from '../utils.js';
import { injectDebugStyles } from './styles.js';

export function ensureDebugPanel() {
  if (state.debugPanelElement) return;

  injectDebugStyles();

  const toggle = document.createElement('button');
  toggle.className = 'ktalk-debug-toggle';
  toggle.textContent = 'Debug';
  toggle.addEventListener('click', () => {
    state.debugVisible = !state.debugVisible;
    if (state.debugPanelElement) {
      state.debugPanelElement.classList.toggle('active', state.debugVisible);
    }
  });

  const panel = document.createElement('div');
  panel.className = 'ktalk-debug-panel';
  panel.innerHTML = '<div class="ktalk-debug-title">KTalk Debug</div>';

  document.body.appendChild(toggle);
  document.body.appendChild(panel);

  state.debugPanelElement = panel;
}

export function updateDebugPanel() {
  if (!state.debugPanelElement) return;

  const activeSpeaker = state.lastActiveSpeaker;
  const participants = state.lastParticipants || [];
  const speakerName = activeSpeaker?.name || '—';
  const speakerId = activeSpeaker?.id || '—';

  const rows = [
    ['wsConnected', state.wsConnected],
    ['conferenceReady', state.conferenceReady],
    ['meetingUrl', state.currentMeetingUrl || '—'],
    ['activeSpeaker', speakerName],
    ['activeSpeakerId', speakerId],
    ['participants', participants.length],
    ['transcriptLines', state.transcriptLines.length],
    ['speakerHistory', state.speakerHistory.length]
  ];

  const rowsHtml = rows.map(([key, value]) => {
    return `<div class="ktalk-debug-row"><div class="ktalk-debug-key">${key}</div><div class="ktalk-debug-value">${formatValue(value)}</div></div>`;
  }).join('');

  const payload = {
    wsConnected: state.wsConnected,
    conferenceReady: state.conferenceReady,
    currentMeetingUrl: state.currentMeetingUrl,
    activeSpeaker: activeSpeaker,
    participants: participants,
    transcriptLines: state.transcriptLines.length,
    speakerHistory: state.speakerHistory.length
  };

  state.debugPanelElement.innerHTML = `<div class="ktalk-debug-title">KTalk Debug</div>${rowsHtml}<div class="ktalk-debug-json">${escapeHtml(JSON.stringify(payload, null, 2))}</div>`;
}

function formatValue(value) {
  if (value === null || value === undefined) return '—';
  if (typeof value === 'boolean') return value ? 'true' : 'false';
  return String(value);
}
