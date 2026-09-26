import { state } from '../state.js';
import { escapeHtml, toTrustedHtml } from '../utils.js';
import { injectDebugStyles } from './styles.js';
import { pcmGetDiagnostics } from '../pcm_transport.js';

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
  panel.innerHTML = toTrustedHtml('<div class="ktalk-debug-title">KTalk Debug</div>');

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
  const pcm = pcmGetDiagnostics();

  const rows = [
    ['wsConnected', state.wsConnected],
    ['conferenceReady', state.conferenceReady],
    ['meetingUrl', state.currentMeetingUrl || '—'],
    ['activeSpeaker', speakerName],
    ['activeSpeakerId', speakerId],
    ['participants', participants.length],
    ['participantsMap', state.lastParticipantsMapSize],
    ['jitsiParticipants', state.lastJitsiParticipantsCount],
    ['jitsiParticipantsMap', state.lastJitsiParticipantsMapCount],
    ['transcriptLines', state.transcriptLines.length],
    ['speakerHistory', state.speakerHistory.length],
    ['noiseFilter', state.audioFilterSuppressCount + ' suppressed'],
    ['flatness', state.audioFilterLastFlatness?.toFixed(3) ?? '—'],
    ['filterReason', state.audioFilterLastReason ?? '—'],
    ['pcmActive', pcm.active_count],
    ['pcmSilent', pcm.silent_bindings],
    ['pcmRebinds', pcm.total_rebinds],
    ['pcmPcCount', pcm.pc_count],
    ['pcmInboundDelta', pcm.inbound_audio_bytes_delta]
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
    participantsMapSize: state.lastParticipantsMapSize,
    jitsiParticipantsCount: state.lastJitsiParticipantsCount,
    jitsiParticipantsMapCount: state.lastJitsiParticipantsMapCount,
    transcriptLines: state.transcriptLines.length,
    speakerHistory: state.speakerHistory.length,
    pcmTransport: pcm
  };

  state.debugPanelElement.innerHTML = toTrustedHtml(`<div class="ktalk-debug-title">KTalk Debug</div>${rowsHtml}<div class="ktalk-debug-json">${escapeHtml(JSON.stringify(payload, null, 2))}</div><div class="ktalk-debug-actions"><button class="ktalk-debug-copy-btn">Copy</button></div>`);
  const copyBtn = state.debugPanelElement.querySelector('.ktalk-debug-copy-btn');
  if (copyBtn) {
    copyBtn.addEventListener('click', () => copyDebug(payload, copyBtn));
  }
}

function formatValue(value) {
  if (value === null || value === undefined) return '—';
  if (typeof value === 'boolean') return value ? 'true' : 'false';
  return String(value);
}

function copyDebug(payload, btn) {
  const text = JSON.stringify(payload, null, 2);
  navigator.clipboard.writeText(text).then(() => {
    btn.classList.add('copied');
    btn.textContent = 'Copied';
    setTimeout(() => {
      btn.classList.remove('copied');
      btn.textContent = 'Copy';
    }, 1200);
  });
}
