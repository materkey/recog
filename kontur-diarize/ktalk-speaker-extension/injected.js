/******/ (() => { // webpackBootstrap
/******/ 	"use strict";
/******/ 	var __webpack_modules__ = ({

/***/ "./src/audio-analyzer.js"
/*!*******************************!*\
  !*** ./src/audio-analyzer.js ***!
  \*******************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   attachAllFromConf: () => (/* binding */ attachAllFromConf),
/* harmony export */   attachParticipant: () => (/* binding */ attachParticipant),
/* harmony export */   detachParticipant: () => (/* binding */ detachParticipant),
/* harmony export */   dispose: () => (/* binding */ dispose),
/* harmony export */   getFlatness: () => (/* binding */ getFlatness),
/* harmony export */   isSpeech: () => (/* binding */ isSpeech)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");


let audioCtx = null;
const analysers = new Map(); // participantId → { source, analyser, freqData, history[] }

const HISTORY_SIZE = 3;

function ensureAudioContext() {
  if (!audioCtx || audioCtx.state === 'closed') {
    audioCtx = new AudioContext();
  }
  return audioCtx;
}

function computeFlatness(analyser) {
  const data = new Float32Array(analyser.frequencyBinCount);
  analyser.getFloatFrequencyData(data);
  const power = [];
  for (let i = 1; i < 256; i++) {
    const db = data[i];
    power.push(db > -100 ? Math.pow(10, db / 10) : 0);
  }
  const totalEnergy = power.reduce((a, b) => a + b, 0);
  if (totalEnergy < 1e-10) return -1; // no signal
  const nonZero = power.filter(p => p > 1e-15);
  if (nonZero.length < 10) return -1;
  const geoMean = Math.exp(nonZero.reduce((a, b) => a + Math.log(b), 0) / nonZero.length);
  const ariMean = nonZero.reduce((a, b) => a + b, 0) / nonZero.length;
  return ariMean > 0 ? geoMean / ariMean : 0;
}

function attachParticipant(participantId, mediaStream) {
  if (!participantId || !mediaStream) return;
  if (analysers.has(participantId)) return;

  try {
    const ctx = ensureAudioContext();
    const source = ctx.createMediaStreamSource(mediaStream);
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 512;
    analyser.smoothingTimeConstant = 0.3;
    source.connect(analyser);
    analysers.set(participantId, { source, analyser, history: [] });
    console.log('[AudioAnalyzer] Attached participant:', participantId);
  } catch (e) {
    console.warn('[AudioAnalyzer] Failed to attach participant:', participantId, e);
  }
}

function detachParticipant(participantId) {
  const entry = analysers.get(participantId);
  if (!entry) return;
  try {
    entry.source.disconnect();
  } catch (e) { /* already disconnected */ }
  analysers.delete(participantId);
  console.log('[AudioAnalyzer] Detached participant:', participantId);
}

function getFlatness(participantId) {
  const entry = analysers.get(participantId);
  if (!entry) return null;
  return computeFlatness(entry.analyser);
}

function isSpeech(participantId) {
  const entry = analysers.get(participantId);
  if (!entry) return { speech: true, reason: 'no_analyser' };

  if (audioCtx?.state === 'suspended') return { speech: true, reason: 'ctx_suspended' };

  const flatness = computeFlatness(entry.analyser);

  if (flatness === -1) return { speech: true, reason: 'silence' };

  // Maintain sliding window
  entry.history.push(flatness);
  if (entry.history.length > HISTORY_SIZE) entry.history.shift();

  if (entry.history.length < 2) return { speech: true, reason: 'warmup' };

  // Median vote
  const sorted = [...entry.history].sort((a, b) => a - b);
  const median = sorted[Math.floor(sorted.length / 2)];

  if (median < _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.FLATNESS_THRESHOLD) {
    return { speech: true, reason: 'speech' };
  }
  return { speech: false, reason: 'noise' };
}

function attachAllFromConf(conf) {
  if (!conf) return;

  // Scan KTalk _participantsMap
  conf._participantsMap?.forEach((p) => {
    const pid = p._id;
    if (!pid || p.local) return;
    const stream = p._audioTrack$?.value?.jitsiTrack?.stream;
    if (stream) attachParticipant(pid, stream);
  });

  // Scan Jitsi participants
  const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
  for (const jp of jitsiParticipants) {
    const pid = jp._id || jp.getId?.() || jp.id;
    if (!pid || analysers.has(pid)) continue;
    const tracks = jp.getTracks?.() || [];
    const audioTrack = tracks.find(t => t.getType?.() === 'audio');
    const stream = audioTrack?.stream || audioTrack?.getOriginalStream?.();
    if (stream) attachParticipant(pid, stream);
  }
}

function dispose() {
  for (const [pid, entry] of analysers) {
    try { entry.source.disconnect(); } catch (e) { /* ok */ }
  }
  analysers.clear();
  if (audioCtx && audioCtx.state !== 'closed') {
    audioCtx.close().catch(() => {});
  }
  audioCtx = null;
  console.log('[AudioAnalyzer] Disposed');
}


/***/ },

/***/ "./src/bioread.js"
/*!************************!*\
  !*** ./src/bioread.js ***!
  \************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   BIOREAD_LIMITS: () => (/* binding */ BIOREAD_LIMITS),
/* harmony export */   BIOREAD_SETTINGS_KEY: () => (/* binding */ BIOREAD_SETTINGS_KEY),
/* harmony export */   formatBioReadText: () => (/* binding */ formatBioReadText),
/* harmony export */   normalizeBioReadSettings: () => (/* binding */ normalizeBioReadSettings)
/* harmony export */ });
/* harmony import */ var _utils_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./utils.js */ "./src/utils.js");


const BIOREAD_SETTINGS_KEY = 'ktalk-bioread-settings';

const BIOREAD_LIMITS = {
  fixation: { min: 10, max: 60, step: 5, default: 40 },
  tailOpacity: { min: 35, max: 90, step: 5, default: 60 },
  minWordLength: 3,
  maxHeadRatio: 0.6,
  ignoreNumbers: true
};

function normalizeBioReadSettings(input = {}) {
  const fixation = clampNumber(
    parseInt(input.fixation, 10),
    BIOREAD_LIMITS.fixation.default,
    BIOREAD_LIMITS.fixation.min,
    BIOREAD_LIMITS.fixation.max
  );
  const tailOpacity = clampNumber(
    parseInt(input.tailOpacity, 10),
    BIOREAD_LIMITS.tailOpacity.default,
    BIOREAD_LIMITS.tailOpacity.min,
    BIOREAD_LIMITS.tailOpacity.max
  );

  return {
    enabled: Boolean(input.enabled),
    fixation,
    tailOpacity,
    minWordLength: BIOREAD_LIMITS.minWordLength,
    maxHeadRatio: BIOREAD_LIMITS.maxHeadRatio,
    ignoreNumbers: input.ignoreNumbers !== undefined ? Boolean(input.ignoreNumbers) : BIOREAD_LIMITS.ignoreNumbers
  };
}

function formatBioReadText(text, settings) {
  const safeText = typeof text === 'string' ? text : String(text ?? '');
  if (!settings || !settings.enabled) {
    return safeText;
  }

  const tokens = tokenize(safeText);
  return tokens.map(token => formatToken(token, settings)).join('');
}

function tokenize(text) {
  if (!text) return [''];
  return text.match(/[\p{L}\p{M}\p{N}]+|[^\p{L}\p{M}\p{N}]+/gu) || [text];
}

function formatToken(token, settings) {
  if (!token) return '';

  if (isWordToken(token, settings)) {
    if (settings.ignoreNumbers && isNumericToken(token)) {
      return (0,_utils_js__WEBPACK_IMPORTED_MODULE_0__.escapeHtml)(token);
    }

    if (token.length < settings.minWordLength) {
      return (0,_utils_js__WEBPACK_IMPORTED_MODULE_0__.escapeHtml)(token);
    }

    const headLen = getHeadLength(token.length, settings.fixation, settings.maxHeadRatio);
    if (headLen <= 0 || headLen >= token.length) {
      return (0,_utils_js__WEBPACK_IMPORTED_MODULE_0__.escapeHtml)(token);
    }

    const head = token.slice(0, headLen);
    const tail = token.slice(headLen);
    return `<span class="bio-word"><span class="bio-head">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_0__.escapeHtml)(head)}</span><span class="bio-tail">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_0__.escapeHtml)(tail)}</span></span>`;
  }

  return (0,_utils_js__WEBPACK_IMPORTED_MODULE_0__.escapeHtml)(token);
}

function isWordToken(token, settings) {
  if (/[\p{L}\p{M}]/u.test(token)) return true;
  if (!settings.ignoreNumbers && /[\p{N}]/u.test(token)) return true;
  return false;
}

function isNumericToken(token) {
  return /^[\p{N}]+$/u.test(token);
}

function getHeadLength(wordLength, fixationPercent, maxHeadRatio) {
  const ratio = fixationPercent / 100;
  const raw = Math.ceil(wordLength * ratio);
  const max = Math.max(1, Math.floor(wordLength * maxHeadRatio));
  return clamp(raw, 1, max);
}

function clampNumber(value, fallback, min, max) {
  if (!Number.isFinite(value)) return fallback;
  return clamp(value, min, max);
}

function clamp(value, min, max) {
  return Math.min(Math.max(value, min), max);
}


/***/ },

/***/ "./src/debug/panel.js"
/*!****************************!*\
  !*** ./src/debug/panel.js ***!
  \****************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   ensureDebugPanel: () => (/* binding */ ensureDebugPanel),
/* harmony export */   updateDebugPanel: () => (/* binding */ updateDebugPanel)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ../state.js */ "./src/state.js");
/* harmony import */ var _utils_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ../utils.js */ "./src/utils.js");
/* harmony import */ var _styles_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./styles.js */ "./src/debug/styles.js");




function ensureDebugPanel() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement) return;

  (0,_styles_js__WEBPACK_IMPORTED_MODULE_2__.injectDebugStyles)();

  const toggle = document.createElement('button');
  toggle.className = 'ktalk-debug-toggle';
  toggle.textContent = 'Debug';
  toggle.addEventListener('click', () => {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugVisible = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugVisible;
    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement.classList.toggle('active', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugVisible);
    }
  });

  const panel = document.createElement('div');
  panel.className = 'ktalk-debug-panel';
  panel.innerHTML = '<div class="ktalk-debug-title">KTalk Debug</div>';

  document.body.appendChild(toggle);
  document.body.appendChild(panel);

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement = panel;
}

function updateDebugPanel() {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement) return;

  const activeSpeaker = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastActiveSpeaker;
  const participants = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipants || [];
  const speakerName = activeSpeaker?.name || '—';
  const speakerId = activeSpeaker?.id || '—';

  const rows = [
    ['wsConnected', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected],
    ['conferenceReady', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady],
    ['meetingUrl', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl || '—'],
    ['activeSpeaker', speakerName],
    ['activeSpeakerId', speakerId],
    ['participants', participants.length],
    ['participantsMap', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipantsMapSize],
    ['jitsiParticipants', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsCount],
    ['jitsiParticipantsMap', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsMapCount],
    ['transcriptLines', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length],
    ['speakerHistory', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.length],
    ['noiseFilter', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.audioFilterSuppressCount + ' suppressed'],
    ['flatness', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.audioFilterLastFlatness?.toFixed(3) ?? '—'],
    ['filterReason', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.audioFilterLastReason ?? '—']
  ];

  const rowsHtml = rows.map(([key, value]) => {
    return `<div class="ktalk-debug-row"><div class="ktalk-debug-key">${key}</div><div class="ktalk-debug-value">${formatValue(value)}</div></div>`;
  }).join('');

  const payload = {
    wsConnected: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected,
    conferenceReady: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady,
    currentMeetingUrl: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl,
    activeSpeaker: activeSpeaker,
    participants: participants,
    participantsMapSize: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipantsMapSize,
    jitsiParticipantsCount: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsCount,
    jitsiParticipantsMapCount: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsMapCount,
    transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length,
    speakerHistory: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.length
  };

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement.innerHTML = `<div class="ktalk-debug-title">KTalk Debug</div>${rowsHtml}<div class="ktalk-debug-json">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.escapeHtml)(JSON.stringify(payload, null, 2))}</div><div class="ktalk-debug-actions"><button class="ktalk-debug-copy-btn">Copy</button></div>`;
  const copyBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement.querySelector('.ktalk-debug-copy-btn');
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


/***/ },

/***/ "./src/debug/styles.js"
/*!*****************************!*\
  !*** ./src/debug/styles.js ***!
  \*****************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   injectDebugStyles: () => (/* binding */ injectDebugStyles)
/* harmony export */ });
function injectDebugStyles() {
  if (document.querySelector('#ktalk-debug-styles')) return;

  const styles = document.createElement('style');
  styles.id = 'ktalk-debug-styles';
  styles.textContent = `
      /* Debug panel */
      .ktalk-debug-toggle {
        position: fixed;
        right: 12px;
        bottom: 12px;
        z-index: 9999;
        padding: 6px 10px;
        background: rgba(0, 0, 0, 0.6);
        border: 1px solid rgba(255,255,255,0.2);
        border-radius: 6px;
        color: #fff;
        font-size: 11px;
        cursor: pointer;
      }
      .ktalk-debug-panel {
        position: fixed;
        right: 12px;
        bottom: 48px;
        width: 360px;
        max-height: 50vh;
        overflow: auto;
        z-index: 9999;
        background: rgba(17, 17, 17, 0.92);
        border: 1px solid rgba(255,255,255,0.2);
        border-radius: 8px;
        color: #e5e7eb;
        font-size: 11px;
        padding: 10px;
        display: none;
        font-family: "Lab Grotesque", Arial, sans-serif;
      }
      .ktalk-debug-panel.active {
        display: block;
      }
      .ktalk-debug-title {
        font-weight: 600;
        margin-bottom: 8px;
        color: #fff;
      }
      .ktalk-debug-row {
        display: flex;
        justify-content: space-between;
        gap: 8px;
        margin-bottom: 4px;
      }
      .ktalk-debug-key {
        color: rgba(255,255,255,0.6);
      }
      .ktalk-debug-value {
        color: #fff;
        text-align: right;
        max-width: 220px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .ktalk-debug-json {
        margin-top: 8px;
        padding: 6px;
        background: rgba(255,255,255,0.05);
        border-radius: 6px;
        white-space: pre-wrap;
        word-break: break-word;
      }
      .ktalk-debug-actions {
        display: flex;
        justify-content: flex-end;
        gap: 8px;
        margin-top: 8px;
      }
      .ktalk-debug-copy-btn {
        padding: 4px 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
      }
      .ktalk-debug-copy-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-debug-copy-btn.copied {
        background: #22c55e;
        color: #fff;
      }
    `;

  document.head.appendChild(styles);
}


/***/ },

/***/ "./src/init.js"
/*!*********************!*\
  !*** ./src/init.js ***!
  \*********************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   init: () => (/* binding */ init)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _ui_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./ui.js */ "./src/ui.js");
/* harmony import */ var _ws_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./ws.js */ "./src/ws.js");
/* harmony import */ var _speaker_js__WEBPACK_IMPORTED_MODULE_3__ = __webpack_require__(/*! ./speaker.js */ "./src/speaker.js");
/* harmony import */ var _meeting_js__WEBPACK_IMPORTED_MODULE_4__ = __webpack_require__(/*! ./meeting.js */ "./src/meeting.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_5__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
/* harmony import */ var _debug_panel_js__WEBPACK_IMPORTED_MODULE_6__ = __webpack_require__(/*! ./debug/panel.js */ "./src/debug/panel.js");
/* harmony import */ var _platforms_index_js__WEBPACK_IMPORTED_MODULE_7__ = __webpack_require__(/*! ./platforms/index.js */ "./src/platforms/index.js");
/* harmony import */ var _ui_vas3k_js__WEBPACK_IMPORTED_MODULE_8__ = __webpack_require__(/*! ./ui-vas3k.js */ "./src/ui-vas3k.js");










function sendConnectionStatus(connected) {
  window.dispatchEvent(new CustomEvent('ktalk-connection-status', { detail: { connected } }));
  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_5__.sendWsConnectionStatus)(connected);
  if (!connected) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = false;
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef = null;
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf = null;
  }
}

function waitForConference(callback, maxAttempts = 100) {
  const platform = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatform)();
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformAdapter)();
  const platformName = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformDisplayName)();

  console.log(`[${platformName} Transcript] Waiting for conference...`);

  if (adapter) {
    // For Jitsi platforms: Check if in prejoin and update UI accordingly
    if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.isJitsiPlatform)() && adapter.isInPrejoin?.()) {
      console.log(`[${platformName} Transcript] User is in prejoin screen`);
      (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_8__.showVas3kPrejoinState)();
    }

    adapter.waitForConference(maxAttempts)
      .then((conf) => {
        console.log(`[${platformName} Transcript] Conference found`);
        callback(conf);
      })
      .catch(() => {
        console.log(`[${platformName} Transcript] Conference not found after ${maxAttempts} attempts`);
        // For Jitsi platforms: keep retrying until conference is found
        if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.isJitsiPlatform)()) {
          if (adapter.isInPrejoin?.()) {
            console.log(`[${platformName} Transcript] Still in prejoin, retrying...`);
            (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_8__.showVas3kPrejoinState)();
          } else {
            console.log(`[${platformName} Transcript] Not in prejoin, retrying...`);
            (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_8__.showVas3kWaitingState)();
          }
          // Retry after a delay
          setTimeout(() => waitForConference(callback, maxAttempts), 2000);
        } else {
          sendConnectionStatus(false);
        }
      });
  } else {
    // Legacy fallback for KTalk
    let attempts = 0;
    const check = () => {
      attempts++;
      if (window.$conf && window.$conf._dominantSpeakerId$ && window.$conf.jitsiConf) {
        console.log('[KTalk Transcript] Conference found');
        callback(window.$conf);
        return;
      }
      if (attempts < maxAttempts) {
        setTimeout(check, 500);
      } else {
        console.log('[KTalk Transcript] Conference not found');
        sendConnectionStatus(false);
      }
    };
    check();
  }
}

function tryAttachConference() {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformAdapter)();
  const platform = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatform)();

  if (adapter) {
    // Use adapter to check conference availability
    if (!adapter.isConferenceAvailable()) {
      if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef || _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = false;
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef = null;
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf = null;
      }
      // Update UI based on prejoin state
      if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.isJitsiPlatform)()) {
        if (adapter.isInPrejoin?.()) {
          (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_8__.showVas3kPrejoinState)();
        } else {
          (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_8__.showVas3kWaitingState)();
        }
      }
      return;
    }

    const conf = adapter.getConference();
    const jitsiConf = adapter.getJitsiConference();

    if (conf !== _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef || jitsiConf !== _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf) {
      // Dispose old subscriptions and analysers before reattach
      adapter.dispose();
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef = conf;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf = jitsiConf;
      console.log(`[${(0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformDisplayName)()} Transcript] Conference reattached`);
      subscribeToDominantSpeaker(conf);
    }
  } else {
    // Legacy fallback for KTalk
    if (!window.$conf || !window.$conf.jitsiConf || !window.$conf._dominantSpeakerId$) {
      if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef || _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = false;
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef = null;
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf = null;
      }
      return;
    }

    if (window.$conf !== _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef || window.$conf.jitsiConf !== _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeConfRef = window.$conf;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.activeJitsiConf = window.$conf.jitsiConf;
      console.log('[KTalk Transcript] Conference reattached');
      subscribeToDominantSpeaker(window.$conf);
    }
  }
}

function setupUI() {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformAdapter)();

  (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.injectStyles)();

  // Use platform-specific UI initialization
  if (adapter && !adapter.supportsTabInjection()) {
    // For platforms without native tab support (vas3k.cloud), use floating panel
    const uiReady = (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.initPlatformUI)();
    (0,_debug_panel_js__WEBPACK_IMPORTED_MODULE_6__.ensureDebugPanel)();
    return uiReady;
  }

  // KTalk: Use tab injection
  const transcriptTabAdded = (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.addTranscriptTab)();
  const contextTabAdded = (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.addContextTab)();
  if (transcriptTabAdded || contextTabAdded) {
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createTranscriptContent)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createContextContent)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.watchTabChanges)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicator)(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected);
  }

  (0,_debug_panel_js__WEBPACK_IMPORTED_MODULE_6__.ensureDebugPanel)();
  return transcriptTabAdded || contextTabAdded;
}

function subscribeToDominantSpeaker(conf) {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformAdapter)();
  const platformName = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformDisplayName)();

  sendConnectionStatus(true);
  (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
  startParticipantsRefresh(conf, 'conference_ready');

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = false;
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReadyTimeout) clearTimeout(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReadyTimeout);

  const checkAndMarkReady = () => {
    if (adapter) {
      if (adapter.isConferenceReady()) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = true;
        console.log(`[${platformName} Transcript] Conference ready - tracks loaded`);
      }
    } else {
      // Legacy KTalk check
      const jitsiParticipants = conf?.jitsiConf?.getParticipants?.() || [];
      const localTracks = conf?.jitsiConf?.getLocalTracks?.() || [];
      const hasAnyTrack = jitsiParticipants.some(jp => {
        const tracks = jp.getTracks?.() || jp._tracks || [];
        return tracks.some(t => (t.getType?.() || t.type) === 'audio');
      }) || localTracks.some(t => t.getType?.() === 'audio');

      if (hasAnyTrack) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = true;
        console.log('[KTalk Transcript] Conference ready - tracks loaded');
      }
    }
  };

  checkAndMarkReady();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReadyTimeout = setTimeout(() => {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = true;
    console.log(`[${platformName} Transcript] Conference ready - timeout`);
    (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
  }, 5000);

  console.log(`[${platformName} Transcript] Subscribing to speaker events`);
  (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);

  // Use adapter for event subscription
  if (adapter) {
    adapter.subscribeToSpeakerChanges(() => {
      checkAndMarkReady();
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
    });

    adapter.onParticipantJoined?.((id) => {
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
      startParticipantsRefresh(conf, 'user_joined');
    });
  } else {
    // Legacy KTalk event subscription
    conf?._participantsMap?.forEach((p, key) => {
      if (p._activeSpeaker$?.subscribe) {
        p._activeSpeaker$.subscribe(() => (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf));
      }
    });

    if (conf?._dominantSpeakerId$?.subscribe) {
      conf._dominantSpeakerId$.subscribe(() => {
        console.log('[KTalk Transcript] Dominant speaker changed:', conf._dominantSpeakerId$?.value);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
      });
    }

    conf?.jitsiConf?.on('conference.dominantSpeaker', (id) => {
      console.log('[KTalk Transcript] Jitsi dominant speaker event:', id);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
    });

    conf?.jitsiConf?.on('conference.trackAdded', (track) => {
      if (track.getType?.() === 'audio') {
        checkAndMarkReady();
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
        const participantId = track.getParticipantId?.();
        if (participantId) {
          const p = conf._participantsMap?.get(participantId);
          if (p?._activeSpeaker$?.subscribe) {
            p._activeSpeaker$.subscribe(() => (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf));
          }
        }
      }
    });

    conf?.jitsiConf?.on('conference.trackRemoved', (track) => {
      if (track.getType?.() === 'audio') {
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
      }
    });

    conf?.jitsiConf?.on('conference.trackMuteChanged', (track) => {
      if (track.getType?.() === 'audio') {
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
      }
    });

    conf?.jitsiConf?.on('conference.userJoined', (id) => {
      setTimeout(() => {
        const p = conf._participantsMap?.get(id);
        if (p?._activeSpeaker$?.subscribe) {
          p._activeSpeaker$.subscribe(() => (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf));
        }
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
        startParticipantsRefresh(conf, 'user_joined');
      }, 1000);
    });

    conf?.jitsiConf?.on('conference.userLeft', () => {
      setTimeout(() => {
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
        startParticipantsRefresh(conf, 'user_left');
      }, 500);
    });
  }

  console.log(`[${platformName} Transcript] Ready`);
}

function startParticipantsRefresh(conf, reason) {
  const intervalMs = 2000;
  const durationMs = 30000;
  const deadline = Date.now() + durationMs;

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.participantsRefreshTimer) {
    clearInterval(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.participantsRefreshTimer);
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.participantsRefreshTimer = null;
  }

  const tick = () => {
    (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
    (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
    const count = (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.getParticipantsList)(conf).length;
    if (count >= 2 || Date.now() >= deadline) {
      clearInterval(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.participantsRefreshTimer);
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.participantsRefreshTimer = null;
    }
  };

  console.log(`[${(0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformDisplayName)()} Transcript] Participants refresh start:`, reason);
  tick();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.participantsRefreshTimer = setInterval(tick, intervalMs);
}

function init() {
  const platformName = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformDisplayName)();
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformAdapter)();

  console.log(`[${platformName} Transcript] Script loaded on platform: ${(0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatform)()}`);

  setTimeout(_ws_js__WEBPACK_IMPORTED_MODULE_2__.connectWebSocket, 1000);

  setInterval(() => {
    if (adapter && !adapter.supportsTabInjection()) {
      // For vas3k.cloud: check floating panel exists
      if (!document.querySelector('.jitsi-transcript-panel')) {
        setupUI();
      }
    } else {
      // For KTalk: check tabs exist
      if (!document.querySelector('.ktalk-transcript-tab') || !document.querySelector('.ktalk-context-tab')) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptTabAdded = false;
        setupUI();
      }
      if (!document.querySelector('.ktalk-transcript-content')) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement = null;
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent = null;
        (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createTranscriptContent)();
      }
      if (!document.querySelector('.ktalk-context-content')) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement = null;
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent = null;
        (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createContextContent)();
      }
    }
    tryAttachConference();
    (0,_debug_panel_js__WEBPACK_IMPORTED_MODULE_6__.updateDebugPanel)();
  }, 2000);

  waitForConference(subscribeToDominantSpeaker);

  window.addEventListener('beforeunload', () => {
    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl) {
      (0,_meeting_js__WEBPACK_IMPORTED_MODULE_4__.sendMeetingLeave)('page_closed');
    }
    const beforeUnloadAdapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_7__.getPlatformAdapter)();
    beforeUnloadAdapter?.dispose();
  });
}


/***/ },

/***/ "./src/meeting.js"
/*!************************!*\
  !*** ./src/meeting.js ***!
  \************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   sendMeetingJoin: () => (/* binding */ sendMeetingJoin),
/* harmony export */   sendMeetingLeave: () => (/* binding */ sendMeetingLeave),
/* harmony export */   startMeetingCheck: () => (/* binding */ startMeetingCheck)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
/* harmony import */ var _platforms_index_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./platforms/index.js */ "./src/platforms/index.js");




let titleSentForUrl = null;

function sendDelayedTitle() {
  // Отправляем title через 2 сек после входа, когда страница уже загрузилась
  setTimeout(() => {
    const title = document.title;
    const url = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl;
    if (title && url && titleSentForUrl !== url) {
      titleSentForUrl = url;
      (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_1__.sendWsMessage)({
        type: 'meeting_title_update',
        title: title
      });
      console.log('[KTalk Transcript] Sent title update:', title);
    }
  }, 2000);
}

function sendMeetingJoin() {
  const url = window.location.href;
  const platform = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_2__.getPlatform)();
  // Send meeting_join for both KTalk and vas3k platforms
  if (url && platform) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl = url;
    (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_1__.sendWsMessage)({
      type: 'meeting_join',
      url: url,
      platform: platform,
      title: ''  // Title отправим позже когда страница загрузится
    });
    console.log(`[${platform} Transcript] Sent meeting_join:`, url);
    sendDelayedTitle();
  }
}

function sendMeetingLeave(reason = 'user_left') {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl) {
    (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_1__.sendWsMessage)({
      type: 'meeting_leave',
      reason: reason
    });
    console.log('[KTalk Transcript] Sent meeting_leave:', reason);
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl = null;
    titleSentForUrl = null;
  }
}

function isInMeeting() {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_2__.getPlatformAdapter)();
  if (adapter) {
    return adapter.isConferenceAvailable();
  }
  // Legacy fallback for KTalk
  if (!window.$conf) return false;
  const participantsMap = window.$conf._participantsMap;
  return participantsMap && participantsMap.size > 0;
}

function startMeetingCheck() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.meetingCheckInterval) {
    clearInterval(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.meetingCheckInterval);
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.meetingCheckInterval = setInterval(() => {
    const inMeeting = isInMeeting();
    const url = window.location.href;

    if (inMeeting && url !== _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl) {
      if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl) {
        sendMeetingLeave('changed_meeting');
      }
      sendMeetingJoin();
    } else if (!inMeeting && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl) {
      sendMeetingLeave('user_left');
    }
  }, 2000);
}


/***/ },

/***/ "./src/platforms/index.js"
/*!********************************!*\
  !*** ./src/platforms/index.js ***!
  \********************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   detectPlatform: () => (/* binding */ detectPlatform),
/* harmony export */   getPlatform: () => (/* binding */ getPlatform),
/* harmony export */   getPlatformAdapter: () => (/* binding */ getPlatformAdapter),
/* harmony export */   getPlatformDisplayName: () => (/* binding */ getPlatformDisplayName),
/* harmony export */   isJitsiPlatform: () => (/* binding */ isJitsiPlatform),
/* harmony export */   resetAdapter: () => (/* binding */ resetAdapter)
/* harmony export */ });
/* harmony import */ var _ktalk_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./ktalk.js */ "./src/platforms/ktalk.js");
/* harmony import */ var _vas3k_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./vas3k.js */ "./src/platforms/vas3k.js");
/**
 * Platform detection and adapter factory
 *
 * Supports:
 * - KTalk (ktalk.ru) - uses custom $conf wrapper with RxJS observables
 * - vas3k.cloud - uses standard Jitsi Meet APP object
 * - vkkovalev.work - uses standard Jitsi Meet APP object
 */




let currentPlatform = null;
let currentAdapter = null;

/**
 * Detect which platform we're on based on hostname
 */
function detectPlatform() {
  const hostname = window.location.hostname;

  if (hostname.includes('ktalk.ru')) {
    return 'ktalk';
  }
  if (hostname.includes('vas3k.cloud')) {
    return 'vas3k';
  }
  if (hostname.includes('vkkovalev.work')) {
    return 'vkkovalev';
  }

  return null;
}

/**
 * Get the current platform name
 */
function getPlatform() {
  if (!currentPlatform) {
    currentPlatform = detectPlatform();
  }
  return currentPlatform;
}

/**
 * Get the platform adapter instance
 * Creates adapter on first call, returns cached instance after
 */
function getPlatformAdapter() {
  if (!currentAdapter) {
    const platform = getPlatform();

    switch (platform) {
      case 'ktalk':
        currentAdapter = new _ktalk_js__WEBPACK_IMPORTED_MODULE_0__.KTalkAdapter();
        break;
      case 'vas3k':
      case 'vkkovalev':
        currentAdapter = new _vas3k_js__WEBPACK_IMPORTED_MODULE_1__.Vas3kAdapter();
        break;
      default:
        console.warn('[Platform] Unknown platform:', platform);
        return null;
    }

    console.log('[Platform] Using adapter:', platform);
  }

  return currentAdapter;
}

/**
 * Reset the adapter (for testing or page navigation)
 */
function resetAdapter() {
  currentPlatform = null;
  currentAdapter = null;
}

/**
 * Get platform display name for UI
 */
function getPlatformDisplayName() {
  const platform = getPlatform();
  switch (platform) {
    case 'ktalk':
      return 'KTalk';
    case 'vas3k':
      return 'Вастрик Мит';
    case 'vkkovalev':
      return 'VK Meet';
    default:
      return 'Unknown';
  }
}

/**
 * Check if platform uses standard Jitsi Meet (not KTalk)
 */
function isJitsiPlatform() {
  const platform = getPlatform();
  return platform === 'vas3k' || platform === 'vkkovalev';
}


/***/ },

/***/ "./src/platforms/ktalk.js"
/*!********************************!*\
  !*** ./src/platforms/ktalk.js ***!
  \********************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   KTalkAdapter: () => (/* binding */ KTalkAdapter)
/* harmony export */ });
/* harmony import */ var _audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ../audio-analyzer.js */ "./src/audio-analyzer.js");
/**
 * KTalk Platform Adapter
 *
 * KTalk uses a custom wrapper around Jitsi with RxJS observables:
 * - window.$conf - main conference object
 * - $conf._participantsMap - Map of participants with RxJS observables
 * - $conf._dominantSpeakerId$ - RxJS observable for dominant speaker
 * - $conf.jitsiConf - underlying Jitsi conference
 */



class KTalkAdapter {
  constructor() {
    this.name = 'ktalk';
    this.conf = null;
    this.subscriptions = [];
    this._jitsiHandlers = [];
  }

  /**
   * Wait for conference to be available
   */
  waitForConference(maxAttempts = 100) {
    return new Promise((resolve, reject) => {
      let attempts = 0;

      const check = () => {
        attempts++;
        if (window.$conf && window.$conf._dominantSpeakerId$ && window.$conf.jitsiConf) {
          console.log('[KTalk Adapter] Conference found');
          this.conf = window.$conf;
          resolve(this.conf);
          return;
        }
        if (attempts < maxAttempts) {
          setTimeout(check, 500);
        } else {
          console.log('[KTalk Adapter] Conference not found');
          reject(new Error('Conference not found'));
        }
      };

      check();
    });
  }

  /**
   * Check if conference is currently available
   */
  isConferenceAvailable() {
    return !!(window.$conf && window.$conf.jitsiConf && window.$conf._dominantSpeakerId$);
  }

  /**
   * Get the conference object
   */
  getConference() {
    if (this.isConferenceAvailable()) {
      this.conf = window.$conf;
    }
    return this.conf;
  }

  /**
   * Get the underlying Jitsi conference
   */
  getJitsiConference() {
    return this.conf?.jitsiConf;
  }

  /**
   * Get active speaker
   */
  getActiveSpeaker() {
    const conf = this.getConference();
    if (!conf) return null;

    let activeSpeaker = null;

    conf._participantsMap?.forEach((p, key) => {
      if (p._activeSpeaker$?.value === true) {
        const info = p._participantInfo$?.value || p._userInfo$?.value;
        activeSpeaker = {
          id: p._id,
          name: this._getFullName(p, info, p.name || p.displayName || key),
          isLocal: p.local
        };
      }
    });

    return activeSpeaker;
  }

  /**
   * Check if participant's mic is muted
   */
  isMicMuted(participantId) {
    const conf = this.getConference();
    if (!conf) return true;

    // Check Jitsi participant tracks
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const jitsiParticipant = jitsiParticipants.find(p => p._id === participantId);
    if (jitsiParticipant) {
      const tracks = jitsiParticipant.getTracks?.() || jitsiParticipant._tracks || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack) {
        const muted = audioTrack.isMuted?.();
        if (muted !== undefined) return muted === true;
      }
    }

    // Check local participant
    if (conf.localParticipant?.id === participantId) {
      const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
      const localAudio = localTracks.find(t => t.getType?.() === 'audio');
      if (localAudio) {
        const muted = localAudio.isMuted?.();
        if (muted !== undefined) return muted === true;
      }
    }

    // Check RxJS observables
    const participant = conf._participantsMap?.get(participantId);
    if (participant) {
      if (participant._audioMuted$?.value !== undefined) {
        return participant._audioMuted$.value === true;
      }
      if (participant._audioMuted !== undefined) {
        return participant._audioMuted === true;
      }
    }

    // If actively speaking, probably not muted
    const speaking = conf._participantsMap?.get(participantId)?._activeSpeaker$?.value;
    if (speaking === true) {
      return false;
    }

    return true;
  }

  /**
   * Check if any mic is on
   */
  anyMicOn() {
    const conf = this.getConference();
    if (!conf) return false;

    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];

    for (const jp of jitsiParticipants) {
      const tracks = jp.getTracks?.() || jp._tracks || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack && audioTrack.isMuted?.() === false) return true;
    }

    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    const localAudio = localTracks.find(t => t.getType?.() === 'audio');
    if (localAudio && localAudio.isMuted?.() === false) return true;

    let hasUnmuted = false;
    conf._participantsMap?.forEach((p) => {
      if (p._audioMuted$?.value === false || p._audioMuted === false) {
        hasUnmuted = true;
      }
    });

    return hasUnmuted;
  }

  /**
   * Get list of participants
   */
  getParticipants() {
    const conf = this.getConference();
    if (!conf) return [];

    const participants = [];
    const byId = new Map();

    // Check if we're in a meeting
    if (!conf._participantsMap || conf._participantsMap.size === 0) {
      return participants;
    }

    // Add local participant
    if (conf.localParticipant) {
      const lp = conf.localParticipant;
      const localInfo = lp._participantInfo$?.value || lp._userInfo$?.value;
      const entry = {
        id: lp.id,
        name: this._getFullName(lp, localInfo, lp.name || lp.displayName || 'You'),
        isLocal: true,
        isSpeaking: conf._participantsMap?.get(lp.id)?._activeSpeaker$?.value || false,
        micMuted: this.isMicMuted(lp.id)
      };
      participants.push(entry);
      byId.set(lp.id, entry);
    }

    // Add remote participants from _participantsMap
    conf._participantsMap?.forEach((p, key) => {
      if (p._id !== conf.localParticipant?.id) {
        const info = p._participantInfo$?.value || p._userInfo$?.value;
        const entry = {
          id: p._id,
          name: this._getFullName(p, info, p.name || p.displayName || key),
          isLocal: false,
          isSpeaking: p._activeSpeaker$?.value || false,
          micMuted: this.isMicMuted(p._id)
        };
        participants.push(entry);
        byId.set(p._id, entry);
      }
    });

    // Fallback: add from Jitsi if missing
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    for (const jp of jitsiParticipants) {
      const pid = jp._id || jp.getId?.() || jp.id;
      if (!pid || byId.has(pid)) continue;
      const name = jp.getDisplayName?.() || jp._displayName || jp.displayName || jp._name || pid;
      const entry = {
        id: pid,
        name,
        isLocal: false,
        isSpeaking: conf._participantsMap?.get(pid)?._activeSpeaker$?.value || false,
        micMuted: this.isMicMuted(pid)
      };
      participants.push(entry);
      byId.set(pid, entry);
    }

    return participants;
  }

  /**
   * Get participant name by ID
   */
  getParticipantName(participantId) {
    const conf = this.getConference();
    if (!conf || !participantId) return 'Unknown';

    if (conf.localParticipant && conf.localParticipant.id === participantId) {
      const p = conf.localParticipant;
      const info = p._participantInfo$?.value || p._userInfo$?.value;
      return this._getFullName(p, info, p.name || p.displayName || 'You');
    }

    if (conf._participantsMap) {
      const participant = conf._participantsMap.get(participantId);
      if (participant) {
        const info = participant._participantInfo$?.value || participant._userInfo$?.value;
        return this._getFullName(participant, info, participant.name || participant.displayName || participantId);
      }
    }

    return participantId;
  }

  /**
   * Subscribe to speaker changes
   */
  subscribeToSpeakerChanges(callback) {
    const conf = this.getConference();
    if (!conf) return;

    // Subscribe to RxJS dominant speaker observable
    if (conf._dominantSpeakerId$?.subscribe) {
      const sub = conf._dominantSpeakerId$.subscribe(() => {
        console.log('[KTalk Adapter] Dominant speaker changed:', conf._dominantSpeakerId$?.value);
        callback();
      });
      this.subscriptions.push(sub);
    }

    // Subscribe to individual participant speaking state
    conf._participantsMap?.forEach((p, key) => {
      if (p._activeSpeaker$?.subscribe) {
        const sub = p._activeSpeaker$.subscribe(() => callback());
        this.subscriptions.push(sub);
      }
    });

    // Attach audio analysers for existing participants
    (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.attachAllFromConf)(conf);

    // Also subscribe to Jitsi events
    const jitsiConf = conf.jitsiConf;
    if (jitsiConf) {
      this._onJitsi(jitsiConf, 'conference.dominantSpeaker', () => callback());

      this._onJitsi(jitsiConf, 'conference.trackAdded', (track) => {
        if (track.getType?.() === 'audio') {
          callback();
          const pid = track.getParticipantId?.();
          const stream = track.stream || track.getOriginalStream?.();
          if (pid && stream) (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.attachParticipant)(pid, stream);
        }
      });

      this._onJitsi(jitsiConf, 'conference.trackRemoved', (track) => {
        if (track.getType?.() === 'audio') {
          callback();
          const pid = track.getParticipantId?.();
          if (pid) (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.detachParticipant)(pid);
        }
      });

      this._onJitsi(jitsiConf, 'conference.trackMuteChanged', (track) => {
        if (track.getType?.() === 'audio') callback();
      });

      this._onJitsi(jitsiConf, 'conference.userJoined', () => {
        setTimeout(callback, 1000);
      });

      this._onJitsi(jitsiConf, 'conference.userLeft', (id) => {
        (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.detachParticipant)(id);
        setTimeout(callback, 500);
      });
    }
  }

  /**
   * Subscribe to new participant joining
   */
  onParticipantJoined(callback) {
    const conf = this.getConference();
    if (!conf?.jitsiConf) return;

    conf.jitsiConf.on('conference.userJoined', (id) => {
      setTimeout(() => {
        const p = conf._participantsMap?.get(id);
        if (p?._activeSpeaker$?.subscribe) {
          const sub = p._activeSpeaker$.subscribe(() => {
            // Notify speaker changes for this participant
          });
          this.subscriptions.push(sub);
        }
        callback(id);
      }, 1000);
    });
  }

  /**
   * Check if conference is ready (has audio tracks)
   */
  isConferenceReady() {
    const conf = this.getConference();
    if (!conf) return false;

    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];

    const hasAnyTrack = jitsiParticipants.some(jp => {
      const tracks = jp.getTracks?.() || jp._tracks || [];
      return tracks.some(t => (t.getType?.() || t.type) === 'audio');
    }) || localTracks.some(t => t.getType?.() === 'audio');

    return hasAnyTrack;
  }

  /**
   * Get UI tab container selector (KTalk uses custom tab system)
   */
  getTabContainerSelector() {
    return '.tl-tabset__content';
  }

  /**
   * Get content area selector
   */
  getContentAreaSelector() {
    return '.slide-menu-wrapper .content';
  }

  /**
   * Check if platform supports tab injection
   */
  supportsTabInjection() {
    return true;
  }

  /**
   * Cleanup subscriptions
   */
  dispose() {
    this.subscriptions.forEach(sub => {
      if (sub.unsubscribe) sub.unsubscribe();
    });
    this.subscriptions = [];
    this._jitsiHandlers.forEach(({ jitsiConf, event, handler }) => {
      jitsiConf.off?.(event, handler);
    });
    this._jitsiHandlers = [];
    (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.dispose)();
  }

  _onJitsi(jitsiConf, event, handler) {
    jitsiConf.on(event, handler);
    this._jitsiHandlers.push({ jitsiConf, event, handler });
  }

  // Private helper methods

  _getFullName(participant, info, fallback) {
    const userInfo = participant?._userInfo$?.value;
    if (userInfo) {
      const firstName = userInfo.firstName || '';
      const lastName = userInfo.lastName || '';
      if (firstName && lastName) return `${firstName} ${lastName}`;
      if (firstName) return firstName;
    }

    if (info) {
      const firstName = info.firstName || '';
      const lastName = info.lastName || '';
      if (firstName && lastName) return `${firstName} ${lastName}`;
      if (firstName) return firstName;
    }

    return fallback || 'Unknown';
  }
}


/***/ },

/***/ "./src/platforms/vas3k.js"
/*!********************************!*\
  !*** ./src/platforms/vas3k.js ***!
  \********************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   Vas3kAdapter: () => (/* binding */ Vas3kAdapter)
/* harmony export */ });
/**
 * vas3k.cloud (Standard Jitsi Meet) Platform Adapter
 *
 * Uses standard Jitsi Meet API:
 * - window.APP - main application object
 * - APP.conference._room - JitsiConference object
 * - APP.store - Redux store with participants state
 * - room.on('conference.dominantSpeaker', callback) - speaker events
 */

class Vas3kAdapter {
  constructor() {
    this.name = 'vas3k';
    this.room = null;
    this.eventHandlers = [];
    this.storeUnsubscribe = null;
    this.lastDominantSpeaker = null;
  }

  /**
   * Check if user is in prejoin screen
   */
  isInPrejoin() {
    const state = window.APP?.store?.getState?.();
    return state?.['features/prejoin']?.showPrejoin === true;
  }

  /**
   * Wait for conference to be available
   * Uses Redux store to detect when conference is joined
   */
  waitForConference(maxAttempts = 200) {
    return new Promise((resolve, reject) => {
      let attempts = 0;

      const check = () => {
        attempts++;
        const room = this._getRoom();

        if (room) {
          console.log('[Vas3k Adapter] Conference found via Redux store');
          this.room = room;
          resolve(room);
          return;
        }

        // Log state for debugging
        if (attempts % 20 === 0) {
          const inPrejoin = this.isInPrejoin();
          console.log(`[Vas3k Adapter] Waiting for conference... attempt ${attempts}, inPrejoin: ${inPrejoin}`);
        }

        if (attempts < maxAttempts) {
          setTimeout(check, 500);
        } else {
          console.log('[Vas3k Adapter] Conference not found after max attempts');
          reject(new Error('Conference not found'));
        }
      };

      check();
    });
  }

  /**
   * Check if conference is currently available
   */
  isConferenceAvailable() {
    return !!this._getRoom();
  }

  /**
   * Get the conference object (returns room for compatibility)
   */
  getConference() {
    if (this.isConferenceAvailable()) {
      this.room = this._getRoom();
    }
    return this.room;
  }

  /**
   * Get the underlying Jitsi conference
   */
  getJitsiConference() {
    return this._getRoom();
  }

  /**
   * Get active speaker
   */
  getActiveSpeaker() {
    const state = window.APP?.store?.getState?.();
    if (!state) return null;

    const participants = state['features/base/participants'];
    if (!participants) return null;

    // Check local participant
    const local = participants.local;
    if (local?.dominantSpeaker) {
      return {
        id: local.id,
        name: local.name || 'You',
        isLocal: true
      };
    }

    // Check remote participants
    const remote = participants.remote;
    if (remote && remote.forEach) {
      let activeSpeaker = null;
      remote.forEach((p, id) => {
        if (p.dominantSpeaker) {
          activeSpeaker = {
            id: p.id || id,
            name: p.name || id,
            isLocal: false
          };
        }
      });
      if (activeSpeaker) return activeSpeaker;
    }

    // Fallback: check large-video participant
    const largeVideoParticipantId = state['features/large-video']?.participantId;
    if (largeVideoParticipantId) {
      const participant = this._getParticipantById(largeVideoParticipantId);
      if (participant) {
        return {
          id: largeVideoParticipantId,
          name: participant.name || largeVideoParticipantId,
          isLocal: participant.id === local?.id
        };
      }
    }

    return null;
  }

  /**
   * Check if participant's mic is muted
   */
  isMicMuted(participantId) {
    const room = this._getRoom();
    if (!room) return true;

    // Check local participant
    const state = window.APP?.store?.getState?.();
    const local = state?.['features/base/participants']?.local;

    if (local?.id === participantId) {
      const localTracks = room.getLocalTracks?.() || [];
      const audioTrack = localTracks.find(t => t.getType?.() === 'audio');
      if (audioTrack) {
        return audioTrack.isMuted?.() === true;
      }
      // Check Redux state
      return state?.['features/base/media']?.audio?.muted !== false;
    }

    // Check remote participants
    const jitsiParticipants = room.getParticipants?.() || [];
    const participant = jitsiParticipants.find(p =>
      (p.getId?.() || p._id) === participantId
    );

    if (participant) {
      const tracks = participant.getTracks?.() || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack) {
        return audioTrack.isMuted?.() === true;
      }
    }

    return true;
  }

  /**
   * Check if any mic is on
   */
  anyMicOn() {
    const room = this._getRoom();
    if (!room) return false;

    // Check local tracks
    const localTracks = room.getLocalTracks?.() || [];
    const localAudio = localTracks.find(t => t.getType?.() === 'audio');
    if (localAudio && localAudio.isMuted?.() === false) {
      return true;
    }

    // Check remote participants
    const jitsiParticipants = room.getParticipants?.() || [];
    for (const jp of jitsiParticipants) {
      const tracks = jp.getTracks?.() || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack && audioTrack.isMuted?.() === false) {
        return true;
      }
    }

    return false;
  }

  /**
   * Get list of participants
   */
  getParticipants() {
    const state = window.APP?.store?.getState?.();
    if (!state) return [];

    const participants = [];
    const participantsState = state['features/base/participants'];
    if (!participantsState) return participants;

    // Add local participant
    const local = participantsState.local;
    if (local) {
      participants.push({
        id: local.id,
        name: local.name || 'You',
        isLocal: true,
        isSpeaking: local.dominantSpeaker || false,
        micMuted: this.isMicMuted(local.id)
      });
    }

    // Add remote participants
    const remote = participantsState.remote;
    if (remote && remote.forEach) {
      remote.forEach((p, id) => {
        participants.push({
          id: p.id || id,
          name: p.name || id,
          isLocal: false,
          isSpeaking: p.dominantSpeaker || false,
          micMuted: this.isMicMuted(p.id || id)
        });
      });
    }

    return participants;
  }

  /**
   * Get participant name by ID
   */
  getParticipantName(participantId) {
    if (!participantId) return 'Unknown';

    const participant = this._getParticipantById(participantId);
    return participant?.name || participantId;
  }

  /**
   * Subscribe to speaker changes
   */
  subscribeToSpeakerChanges(callback) {
    const room = this._getRoom();

    if (room) {
      // Subscribe to Jitsi conference events
      const dominantSpeakerHandler = (id) => {
        console.log('[Vas3k Adapter] Dominant speaker changed:', id);
        callback();
      };

      const trackHandler = (track) => {
        if (track.getType?.() === 'audio') {
          callback();
        }
      };

      const userHandler = () => {
        setTimeout(callback, 500);
      };

      room.on('conference.dominantSpeaker', dominantSpeakerHandler);
      room.on('conference.trackAdded', trackHandler);
      room.on('conference.trackRemoved', trackHandler);
      room.on('conference.trackMuteChanged', trackHandler);
      room.on('conference.userJoined', userHandler);
      room.on('conference.userLeft', userHandler);

      this.eventHandlers.push(
        { event: 'conference.dominantSpeaker', handler: dominantSpeakerHandler },
        { event: 'conference.trackAdded', handler: trackHandler },
        { event: 'conference.trackRemoved', handler: trackHandler },
        { event: 'conference.trackMuteChanged', handler: trackHandler },
        { event: 'conference.userJoined', handler: userHandler },
        { event: 'conference.userLeft', handler: userHandler }
      );
    }

    // Also subscribe to Redux store for dominant speaker changes
    if (window.APP?.store?.subscribe) {
      this.storeUnsubscribe = window.APP.store.subscribe(() => {
        const state = window.APP.store.getState();
        const participantsState = state?.['features/base/participants'];
        let currentDominantSpeaker = null;

        if (participantsState?.local?.dominantSpeaker) {
          currentDominantSpeaker = participantsState.local.id;
        } else if (participantsState?.remote) {
          participantsState.remote.forEach((p, id) => {
            if (p.dominantSpeaker) {
              currentDominantSpeaker = p.id || id;
            }
          });
        }

        if (currentDominantSpeaker !== this.lastDominantSpeaker) {
          this.lastDominantSpeaker = currentDominantSpeaker;
          callback();
        }
      });
    }
  }

  /**
   * Subscribe to new participant joining
   */
  onParticipantJoined(callback) {
    const room = this._getRoom();
    if (!room) return;

    room.on('conference.userJoined', (id) => {
      setTimeout(() => callback(id), 1000);
    });
  }

  /**
   * Check if conference is ready (has audio tracks)
   */
  isConferenceReady() {
    const room = this._getRoom();
    if (!room) return false;

    const localTracks = room.getLocalTracks?.() || [];
    if (localTracks.some(t => t.getType?.() === 'audio')) {
      return true;
    }

    const jitsiParticipants = room.getParticipants?.() || [];
    return jitsiParticipants.some(jp => {
      const tracks = jp.getTracks?.() || [];
      return tracks.some(t => (t.getType?.() || t.type) === 'audio');
    });
  }

  /**
   * Get UI tab container selector (standard Jitsi doesn't have KTalk tabs)
   */
  getTabContainerSelector() {
    return null; // No tab container in standard Jitsi
  }

  /**
   * Get content area selector
   */
  getContentAreaSelector() {
    return null; // No content area in standard Jitsi
  }

  /**
   * Check if platform supports tab injection
   */
  supportsTabInjection() {
    return false; // Use floating panel instead
  }

  /**
   * Cleanup subscriptions
   */
  dispose() {
    const room = this._getRoom();
    if (room) {
      this.eventHandlers.forEach(({ event, handler }) => {
        room.off?.(event, handler);
      });
    }
    this.eventHandlers = [];

    if (this.storeUnsubscribe) {
      this.storeUnsubscribe();
      this.storeUnsubscribe = null;
    }
  }

  // Private helper methods

  _getRoom() {
    // Primary: Get from Redux store (more reliable)
    const state = window.APP?.store?.getState?.();
    const roomFromRedux = state?.['features/base/conference']?.conference;
    if (roomFromRedux) {
      return roomFromRedux;
    }
    // Fallback: Try APP.conference._room (older Jitsi versions)
    return window.APP?.conference?._room;
  }

  _getParticipantById(participantId) {
    const state = window.APP?.store?.getState?.();
    if (!state) return null;

    const participantsState = state['features/base/participants'];
    if (!participantsState) return null;

    if (participantsState.local?.id === participantId) {
      return participantsState.local;
    }

    if (participantsState.remote?.get) {
      return participantsState.remote.get(participantId);
    }

    return null;
  }
}


/***/ },

/***/ "./src/speaker.js"
/*!************************!*\
  !*** ./src/speaker.js ***!
  \************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   anyMicOn: () => (/* binding */ anyMicOn),
/* harmony export */   getActiveSpeaker: () => (/* binding */ getActiveSpeaker),
/* harmony export */   getParticipantsList: () => (/* binding */ getParticipantsList),
/* harmony export */   isMicMuted: () => (/* binding */ isMicMuted),
/* harmony export */   sendFilteredSpeakerUpdate: () => (/* binding */ sendFilteredSpeakerUpdate),
/* harmony export */   sendParticipantsUpdate: () => (/* binding */ sendParticipantsUpdate),
/* harmony export */   updateSpeakerStatus: () => (/* binding */ updateSpeakerStatus)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _utils_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./utils.js */ "./src/utils.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
/* harmony import */ var _platforms_index_js__WEBPACK_IMPORTED_MODULE_3__ = __webpack_require__(/*! ./platforms/index.js */ "./src/platforms/index.js");
/* harmony import */ var _ui_vas3k_js__WEBPACK_IMPORTED_MODULE_4__ = __webpack_require__(/*! ./ui-vas3k.js */ "./src/ui-vas3k.js");
/* harmony import */ var _audio_analyzer_js__WEBPACK_IMPORTED_MODULE_5__ = __webpack_require__(/*! ./audio-analyzer.js */ "./src/audio-analyzer.js");







function getActiveSpeaker(conf) {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_3__.getPlatformAdapter)();
  if (adapter) {
    return adapter.getActiveSpeaker();
  }

  // Legacy fallback for KTalk if adapter not ready
  let activeSpeaker = null;
  conf?._participantsMap?.forEach((p, key) => {
    if (p._activeSpeaker$?.value === true) {
      const info = p._participantInfo$?.value || p._userInfo$?.value;
      activeSpeaker = {
        id: p._id,
        name: _getFullNameLegacy(p, info, p.name || p.displayName || key),
        isLocal: p.local
      };
    }
  });
  return activeSpeaker;
}

// Legacy helper for fallback
function _getFullNameLegacy(participant, info, fallback) {
  const userInfo = participant?._userInfo$?.value;
  if (userInfo) {
    const firstName = userInfo.firstName || '';
    const lastName = userInfo.lastName || '';
    if (firstName && lastName) return `${firstName} ${lastName}`;
    if (firstName) return firstName;
  }
  if (info) {
    const firstName = info.firstName || '';
    const lastName = info.lastName || '';
    if (firstName && lastName) return `${firstName} ${lastName}`;
    if (firstName) return firstName;
  }
  return fallback || 'Unknown';
}

function isMicMuted(conf, participantId) {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_3__.getPlatformAdapter)();
  if (adapter) {
    return adapter.isMicMuted(participantId);
  }

  // Legacy fallback
  const jitsiParticipants = conf?.jitsiConf?.getParticipants?.() || [];
  const jitsiParticipant = jitsiParticipants.find(p => p._id === participantId);
  if (jitsiParticipant) {
    const tracks = jitsiParticipant.getTracks?.() || jitsiParticipant._tracks || [];
    const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
    if (audioTrack) {
      const muted = audioTrack.isMuted?.();
      if (muted !== undefined) return muted === true;
    }
  }

  if (conf?.localParticipant?.id === participantId) {
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    const localAudio = localTracks.find(t => t.getType?.() === 'audio');
    if (localAudio) {
      const muted = localAudio.isMuted?.();
      if (muted !== undefined) return muted === true;
    }
  }

  const participant = conf?._participantsMap?.get(participantId);
  if (participant) {
    if (participant._audioMuted$?.value !== undefined) {
      return participant._audioMuted$.value === true;
    }
    if (participant._audioMuted !== undefined) {
      return participant._audioMuted === true;
    }
  }

  const speaking = conf?._participantsMap?.get(participantId)?._activeSpeaker$?.value;
  if (speaking === true) {
    return false;
  }

  return true;
}

function anyMicOn(conf) {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_3__.getPlatformAdapter)();
  if (adapter) {
    return adapter.anyMicOn();
  }

  // Legacy fallback
  const jitsiParticipants = conf?.jitsiConf?.getParticipants?.() || [];
  let foundAnyAudioTrack = false;

  for (const jp of jitsiParticipants) {
    const tracks = jp.getTracks?.() || jp._tracks || [];
    const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
    if (audioTrack) {
      foundAnyAudioTrack = true;
      if (audioTrack.isMuted?.() === false) return true;
    }
  }

  const localTracks = conf?.jitsiConf?.getLocalTracks?.() || [];
  const localAudio = localTracks.find(t => t.getType?.() === 'audio');
  if (localAudio) {
    foundAnyAudioTrack = true;
    if (localAudio.isMuted?.() === false) return true;
  }

  conf?._participantsMap?.forEach((p) => {
    if (p._audioMuted$?.value === false || p._audioMuted === false) {
      foundAnyAudioTrack = true;
    }
  });

  if (!foundAnyAudioTrack && !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady) {
    return true;
  }

  let hasAnyParticipantWithUnmutedMic = false;
  conf?._participantsMap?.forEach((p) => {
    if (p._audioMuted$?.value === false || p._audioMuted === false) {
      hasAnyParticipantWithUnmutedMic = true;
    }
  });
  if (hasAnyParticipantWithUnmutedMic) return true;

  return false;
}

function getParticipantsList(conf) {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_3__.getPlatformAdapter)();
  if (adapter) {
    const participants = adapter.getParticipants();
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipantsMapSize = participants.length;
    return participants;
  }

  // Legacy fallback
  const participants = [];
  const byId = new Map();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipantsMapSize = conf?._participantsMap?.size || 0;

  // Only return participants if we're actually in a meeting
  const isInMeeting = conf?._participantsMap && conf._participantsMap.size > 0;
  if (!isInMeeting) {
    return participants;
  }

  if (conf.localParticipant) {
    const lp = conf.localParticipant;
    const localInfo = lp._participantInfo$?.value || lp._userInfo$?.value;
    const entry = {
      id: lp.id,
      name: _getFullNameLegacy(lp, localInfo, lp.name || lp.displayName || 'You'),
      isLocal: true,
      isSpeaking: conf._participantsMap?.get(lp.id)?._activeSpeaker$?.value || false,
      micMuted: isMicMuted(conf, lp.id)
    };
    participants.push(entry);
    byId.set(lp.id, entry);
  }

  conf._participantsMap?.forEach((p, key) => {
    if (p._id !== conf.localParticipant?.id) {
      const info = p._participantInfo$?.value || p._userInfo$?.value;
      const entry = {
        id: p._id,
        name: _getFullNameLegacy(p, info, p.name || p.displayName || key),
        isLocal: false,
        isSpeaking: p._activeSpeaker$?.value || false,
        micMuted: isMicMuted(conf, p._id)
      };
      participants.push(entry);
      byId.set(p._id, entry);
    }
  });

  // Fallback: add participants from Jitsi if map is empty or missing remotes
  const jitsiParticipants = conf?.jitsiConf?.getParticipants?.() || [];
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsCount = jitsiParticipants.length;
  const jitsiParticipantsMap = conf?.jitsiConf?.participants || conf?.jitsiConf?._participants;
  const jitsiMapValues = jitsiParticipantsMap
    ? (typeof jitsiParticipantsMap.values === 'function'
      ? Array.from(jitsiParticipantsMap.values())
      : Object.values(jitsiParticipantsMap))
    : [];
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsMapCount = jitsiMapValues.length;

  for (const jp of jitsiParticipants) {
    const pid = jp._id || jp.getId?.() || jp.id;
    if (!pid || byId.has(pid)) continue;
    const name = jp.getDisplayName?.() || jp._displayName || jp.displayName || jp._name || pid;
    const entry = {
      id: pid,
      name,
      isLocal: false,
      isSpeaking: conf?._participantsMap?.get(pid)?._activeSpeaker$?.value || false,
      micMuted: isMicMuted(conf, pid)
    };
    participants.push(entry);
    byId.set(pid, entry);
  }

  for (const jp of jitsiMapValues) {
    const pid = jp?._id || jp?.getId?.() || jp?.id;
    if (!pid || byId.has(pid)) continue;
    const name = jp?.getDisplayName?.() || jp?._displayName || jp?.displayName || jp?._name || pid;
    const entry = {
      id: pid,
      name,
      isLocal: false,
      isSpeaking: conf?._participantsMap?.get(pid)?._activeSpeaker$?.value || false,
      micMuted: isMicMuted(conf, pid)
    };
    participants.push(entry);
    byId.set(pid, entry);
  }

  return participants;
}

function sendParticipantsUpdate(conf) {
  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsParticipants)(getParticipantsList(conf));
}

// Get participant name using adapter
function getParticipantName(conf, participantId) {
  const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_3__.getPlatformAdapter)();
  if (adapter) {
    return adapter.getParticipantName(participantId);
  }

  // Legacy fallback
  if (!participantId) return 'Unknown';

  if (conf?.localParticipant && conf.localParticipant.id === participantId) {
    const p = conf.localParticipant;
    const info = p._participantInfo$?.value || p._userInfo$?.value;
    return _getFullNameLegacy(p, info, p.name || p.displayName || 'You');
  }

  if (conf?._participantsMap) {
    const participant = conf._participantsMap.get(participantId);
    if (participant) {
      const info = participant._participantInfo$?.value || participant._userInfo$?.value;
      return _getFullNameLegacy(participant, info, participant.name || participant.displayName || participantId);
    }
  }

  return participantId;
}

function updateSpeakerStatus(conf) {
  const activeSpeaker = getActiveSpeaker(conf);
  const hasMicsOn = anyMicOn(conf);

  // Update platform-specific UI
  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_3__.isJitsiPlatform)()) {
    (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_4__.updateVas3kSpeakerStatus)(activeSpeaker, hasMicsOn);
  }

  // KTalk UI elements
  const dot = document.querySelector('.ktalk-speaker-dot');
  const nameEl = document.querySelector('.ktalk-speaker-name');

  if (!activeSpeaker && !hasMicsOn && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady) {
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    console.log('[KTalk Debug] All mics off detected:', {
      conferenceReady: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady,
      jitsiParticipantsCount: jitsiParticipants.length,
      localTracksCount: localTracks.length,
      participantsMapSize: conf._participantsMap?.size,
      jitsiConfExists: !!conf.jitsiConf,
      participants: jitsiParticipants.map(p => ({
        id: p._id,
        tracks: (p.getTracks?.() || p._tracks || []).map(t => ({
          type: t.getType?.() || t.type,
          muted: t.isMuted?.()
        }))
      })),
      localTracks: localTracks.map(t => ({
        type: t.getType?.(),
        muted: t.isMuted?.()
      }))
    });
  }

  if (dot && nameEl) {
    if (activeSpeaker) {
      const speakerMuted = isMicMuted(conf, activeSpeaker.id);
      if (!speakerMuted) {
        dot.classList.add('speaking');
        nameEl.textContent = activeSpeaker.name;
      } else {
        dot.classList.remove('speaking');
        if (!hasMicsOn) {
          nameEl.textContent = 'All mics off';
        } else {
          nameEl.textContent = 'No one speaking';
        }
      }
    } else {
      dot.classList.remove('speaking');
      if (!hasMicsOn) {
        nameEl.textContent = 'All mics off';
      } else {
        nameEl.textContent = 'No one speaking';
      }
    }
  }

  if (activeSpeaker) {
    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker !== activeSpeaker.id) {
      if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime) {
        const prevName = getParticipantName(conf, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker);
        addToHistory(prevName, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker, Date.now() - _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime);
      }
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker = activeSpeaker.id;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime = Date.now();
    }
  } else {
    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime) {
      const prevName = getParticipantName(conf, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker);
      addToHistory(prevName, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker, Date.now() - _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime);
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker = null;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime = null;
    }
  }

  const participants = getParticipantsList(conf);
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastActiveSpeaker = activeSpeaker;
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipants = participants;

  (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.sendUpdate)({
    currentSpeaker: activeSpeaker,
    speakerHistory: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory,
    participants: participants
  });

  sendFilteredSpeakerUpdate(activeSpeaker);
}

function sendFilteredSpeakerUpdate(speaker) {
  if (speaker) {
    // Jitsi keeps _activeSpeaker$ pointing at the last dominant speaker even after
    // they mute.  If the reported speaker's mic is muted, suppress the update so
    // the bridge doesn't attribute audio to a silent participant.
    const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_3__.getPlatformAdapter)();
    const conf = adapter?.getConference?.() || window.$conf;
    if (conf && isMicMuted(conf, speaker.id)) {
      (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsSpeakerUpdate)(null);
      return;
    }
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.AUDIO_FILTER_ENABLED && speaker) {
    const result = (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_5__.isSpeech)(speaker.id);
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.audioFilterLastFlatness = (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_5__.getFlatness)(speaker.id);
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.audioFilterLastReason = result.reason;
    if (!result.speech) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.audioFilterSuppressCount++;
      (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsSpeakerUpdate)(null);
      return;
    }
  }
  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsSpeakerUpdate)(speaker);
}

function addToHistory(name, speakerId, durationMs) {
  if (!name || durationMs < _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.SPEAKING_THRESHOLD_MS) return;

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.unshift({
    name, id: speakerId,
    time: new Date().toLocaleTimeString('ru-RU', {
      hour: '2-digit', minute: '2-digit', second: '2-digit'
    }),
    durationMs
  });

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.length > _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.MAX_HISTORY) _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.pop();
}


/***/ },

/***/ "./src/state.js"
/*!**********************!*\
  !*** ./src/state.js ***!
  \**********************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   constants: () => (/* binding */ constants),
/* harmony export */   state: () => (/* binding */ state)
/* harmony export */ });
const constants = {
  MAX_HISTORY: 100,
  SPEAKING_THRESHOLD_MS: 500,
  MAX_TRANSCRIPT_LINES: 100,
  WS_URL: 'ws://localhost:8766',
  FLATNESS_THRESHOLD: 0.15,
  AUDIO_FILTER_ENABLED: true
};

const state = {
  transcriptTabAdded: false,
  transcriptContentElement: null,
  originalChatContent: null,
  isTranscriptActive: false,

  speakerHistory: [],
  currentSpeaker: null,
  speakingStartTime: null,

  transcriptLines: [],

  wsConnection: null,
  wsReconnectTimer: null,
  wsConnected: false,

  conferenceReady: false,
  conferenceReadyTimeout: null,

  currentMeetingUrl: null,
  meetingCheckInterval: null,

  summaryModalElement: null,
  currentSummaryText: '',
  summaryRequestPending: false,
  currentSummaryType: 'catchup',
  contextContentElement: null,
  isContextActive: false,
  anchorRequestPending: false,
  currentAnchorData: null,
  anchorLastUpdated: 0,

  debugPanelElement: null,
  debugVisible: false,
  lastParticipants: [],
  lastActiveSpeaker: null,
  lastParticipantsMapSize: 0,
  lastJitsiParticipantsCount: 0,
  lastJitsiParticipantsMapCount: 0,
  autoScrollEnabled: true,
  participantsRefreshTimer: null,
  clockSyncTimer: null,
  activeConfRef: null,
  activeJitsiConf: null,
  audioFilterSuppressCount: 0,
  audioFilterLastFlatness: null,
  audioFilterLastReason: null,

  bioRead: {
    enabled: false,
    fixation: 40,
    tailOpacity: 60,
    minWordLength: 3,
    maxHeadRatio: 0.6,
    ignoreNumbers: true,
    panelOpen: false
  }
};


/***/ },

/***/ "./src/ui-vas3k.js"
/*!*************************!*\
  !*** ./src/ui-vas3k.js ***!
  \*************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   addVas3kTranscriptLine: () => (/* binding */ addVas3kTranscriptLine),
/* harmony export */   clearVas3kTranscript: () => (/* binding */ clearVas3kTranscript),
/* harmony export */   copyVas3kTranscript: () => (/* binding */ copyVas3kTranscript),
/* harmony export */   createFloatingPanel: () => (/* binding */ createFloatingPanel),
/* harmony export */   createVas3kSummaryModal: () => (/* binding */ createVas3kSummaryModal),
/* harmony export */   handleVas3kSummaryResponse: () => (/* binding */ handleVas3kSummaryResponse),
/* harmony export */   highlightVas3kLine: () => (/* binding */ highlightVas3kLine),
/* harmony export */   initVas3kUI: () => (/* binding */ initVas3kUI),
/* harmony export */   injectVas3kStyles: () => (/* binding */ injectVas3kStyles),
/* harmony export */   showVas3kPrejoinState: () => (/* binding */ showVas3kPrejoinState),
/* harmony export */   showVas3kWaitingState: () => (/* binding */ showVas3kWaitingState),
/* harmony export */   toggleVas3kFollow: () => (/* binding */ toggleVas3kFollow),
/* harmony export */   updateVas3kSpeakerStatus: () => (/* binding */ updateVas3kSpeakerStatus),
/* harmony export */   updateVas3kWsIndicator: () => (/* binding */ updateVas3kWsIndicator)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _utils_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./utils.js */ "./src/utils.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
/**
 * Floating Panel UI for vas3k.cloud (Standard Jitsi Meet)
 *
 * Creates a draggable floating panel for transcript display
 * since vas3k.cloud doesn't have KTalk's tab system.
 */





let panelVisible = true;
let isDragging = false;
let dragOffset = { x: 0, y: 0 };

/**
 * Inject styles for the floating panel
 */
function injectVas3kStyles() {
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
function createFloatingPanel() {
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
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.vas3kFollowEnabled === undefined) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.vas3kFollowEnabled = true;
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
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement = panel.querySelector('.jitsi-transcript-content');

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
function updateVas3kWsIndicator(connected) {
  const indicator = document.querySelector('.jitsi-ws-indicator');
  if (indicator) {
    indicator.classList.toggle('connected', connected);
  }
}

/**
 * Update speaker status display
 */
function updateVas3kSpeakerStatus(speaker, micsOn) {
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
function showVas3kPrejoinState() {
  const nameEl = document.querySelector('.jitsi-speaker-name');
  if (nameEl) {
    nameEl.textContent = 'Нажмите "Присоединиться" для начала';
  }
}

/**
 * Show waiting for conference message
 */
function showVas3kWaitingState() {
  const nameEl = document.querySelector('.jitsi-speaker-name');
  if (nameEl) {
    nameEl.textContent = 'Ожидание подключения...';
  }
}

/**
 * Add transcript line to the panel
 */
function addVas3kTranscriptLine(line) {
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

  const uncertainSuffix = (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getUncertainSuffix)(line);

  lineEl.innerHTML = `
    <div class="jitsi-transcript-line-header">
      <span class="jitsi-transcript-speaker">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.escapeHtml)(line.speaker || 'Unknown')}</span>
      <span class="jitsi-transcript-time">${line.time || ''}</span>
    </div>
    <div class="jitsi-transcript-text ${line.uncertain ? 'jitsi-transcript-uncertain' : ''}">
      ${(0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.escapeHtml)(line.text || '')}${uncertainSuffix}
    </div>
  `;

  content.appendChild(lineEl);

  // Auto-scroll to bottom only if follow is enabled (default true)
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.vas3kFollowEnabled !== false) {
    content.scrollTop = content.scrollHeight;
  }
}

/**
 * Create AI Summary modal
 */
function createVas3kSummaryModal() {
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
        <option value="codex">Codex (GPT-5.4)</option>
        <option value="codex:gpt-4.1">Codex (GPT-4.1)</option>
        <option value="codex:gpt-5.3-codex-spark">Codex Spark</option>
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
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) {
    const content = document.querySelector('.jitsi-summary-content');
    if (content) {
      content.innerHTML = '<p style="color: #ff6b6b">Нет транскрипта для анализа</p>';
    }
    return;
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryType = type;
  let linesToSend;

  if (type === 'catchup') {
    const oneMinuteAgo = Date.now() - 60000;
    const recentLines = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.filter(line => line.createdAt && line.createdAt >= oneMinuteAgo);
    linesToSend = recentLines.length > 0 ? recentLines : _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice(-10);
  } else if (type === 'brief') {
    const twoMinutesAgo = Date.now() - 120000;
    const recentLines = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.filter(line => line.createdAt && line.createdAt >= twoMinutesAgo);
    linesToSend = recentLines.length > 0 ? recentLines : _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice(-20);
  } else {
    linesToSend = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines;
  }

  const transcriptText = linesToSend.map(line => {
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getUncertainSuffix)(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  const content = document.querySelector('.jitsi-summary-content');
  if (content) {
    content.innerHTML = '<p style="color: rgba(255,255,255,0.5)">⏳ Генерация summary...</p>';
  }

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsMessage)({
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

  const transcriptText = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.map(line => {
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getUncertainSuffix)(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  const content = document.querySelector('.jitsi-summary-content');
  if (content) {
    content.innerHTML = '<p style="color: rgba(255,255,255,0.5)">⏳ Отвечаю на вопрос...</p>';
  }

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsMessage)({
    type: 'summarize_request',
    summary_type: 'question',
    transcript: transcriptText,
    question: question,
    previous_summary: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryText || '',
    model: localStorage.getItem('ktalk-ai-model') || 'codex',
    meeting_title: document.title || ''
  });

  input.value = '';
}

/**
 * Handle summary response from backend
 */
function handleVas3kSummaryResponse(data) {
  const content = document.querySelector('.jitsi-summary-content');
  if (!content) return;

  if (data.error) {
    content.innerHTML = `<p style="color: #ff6b6b">Ошибка: ${(0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.escapeHtml)(data.error)}</p>`;
    return;
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryText = data.summary || '';
  content.innerHTML = `<div style="white-space: pre-wrap">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.escapeHtml)(data.summary || 'Нет данных')}</div>`;
}

/**
 * Update follow button visual state
 */
function updateFollowButtonState(btn) {
  if (!btn) btn = document.querySelector('.jitsi-follow-btn');
  if (!btn) return;

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.vas3kFollowEnabled) {
    btn.classList.add('active');
  } else {
    btn.classList.remove('active');
  }
}

/**
 * Toggle follow/auto-scroll mode
 */
function toggleVas3kFollow() {
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.vas3kFollowEnabled = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.vas3kFollowEnabled;
  updateFollowButtonState();
}

/**
 * Highlight a specific transcript line by ID
 */
function highlightVas3kLine(lineId) {
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
async function copyVas3kTranscript() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) return;

  const text = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.map(line => {
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
function clearVas3kTranscript() {
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
function initVas3kUI() {
  injectVas3kStyles();
  return createFloatingPanel();
}


/***/ },

/***/ "./src/ui.js"
/*!*******************!*\
  !*** ./src/ui.js ***!
  \*******************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   addContextTab: () => (/* binding */ addContextTab),
/* harmony export */   addTranscriptLinePlatform: () => (/* binding */ addTranscriptLinePlatform),
/* harmony export */   addTranscriptTab: () => (/* binding */ addTranscriptTab),
/* harmony export */   createContextContent: () => (/* binding */ createContextContent),
/* harmony export */   createTranscriptContent: () => (/* binding */ createTranscriptContent),
/* harmony export */   handleAnchorResponse: () => (/* binding */ handleAnchorResponse),
/* harmony export */   handleNewMeeting: () => (/* binding */ handleNewMeeting),
/* harmony export */   handleSummaryResponse: () => (/* binding */ handleSummaryResponse),
/* harmony export */   handleTranscript: () => (/* binding */ handleTranscript),
/* harmony export */   handleTranscriptHistory: () => (/* binding */ handleTranscriptHistory),
/* harmony export */   initPlatformUI: () => (/* binding */ initPlatformUI),
/* harmony export */   injectStyles: () => (/* binding */ injectStyles),
/* harmony export */   updateWsIndicator: () => (/* binding */ updateWsIndicator),
/* harmony export */   updateWsIndicatorPlatform: () => (/* binding */ updateWsIndicatorPlatform),
/* harmony export */   watchTabChanges: () => (/* binding */ watchTabChanges)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _bioread_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./bioread.js */ "./src/bioread.js");
/* harmony import */ var _utils_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./utils.js */ "./src/utils.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_3__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
/* harmony import */ var _platforms_index_js__WEBPACK_IMPORTED_MODULE_4__ = __webpack_require__(/*! ./platforms/index.js */ "./src/platforms/index.js");
/* harmony import */ var _ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__ = __webpack_require__(/*! ./ui-vas3k.js */ "./src/ui-vas3k.js");







let selectedModel = localStorage.getItem('ktalk-ai-model') || 'codex';
let anchorModel = localStorage.getItem('ktalk-anchor-model') || 'codex';
let meetingTitle = document.title || '';
let bioReadHotkeyBound = false;
let bioReadPanelListenerBound = false;
let summaryMenuOpen = false;
let summaryMenuListenerBound = false;
let anchorHotkeyBound = false;

function injectStyles() {
  if (document.querySelector('#ktalk-transcript-styles')) return;

  const styles = document.createElement('style');
  styles.id = 'ktalk-transcript-styles';
  styles.textContent = `
      /* Transcript tab - exact match of KTalk's PRIVATE button */
      .ktalk-transcript-tab-wrapper.tl-tab,
      .ktalk-context-tab-wrapper.tl-tab {
        display: inline-block !important;
        height: 32px;
        padding: 0;
        margin: 0;
      }
      .ktalk-transcript-tab-wrapper.tl-tab {
        margin-right: 8px;
      }

      .ktalk-transcript-tab.link,
      .ktalk-context-tab.link {
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
      .ktalk-transcript-tab.link:hover,
      .ktalk-context-tab.link:hover {
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-transcript-tab.link._active,
      .ktalk-context-tab.link._active {
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-transcript-tab .tab-content,
      .ktalk-context-tab .tab-content {
        display: flex;
        align-items: center;
        height: 32px;
        line-height: 16px;
        gap: 4px;
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
      .ktalk-context-content {
        display: none;
        flex-direction: column;
        height: 100%;
        background: rgb(52, 52, 52);
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-context-content.active {
        display: flex;
      }
      .ktalk-context-header {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 10px 16px;
        background: rgb(60, 60, 60);
        border-bottom: 1px solid rgba(255,255,255,0.1);
      }
      .ktalk-context-title {
        font-size: 13px;
        font-weight: 600;
      }
      .ktalk-context-actions {
        margin-left: auto;
        display: flex;
        align-items: center;
        gap: 8px;
      }
      .ktalk-context-refresh-btn {
        padding: 0 10px;
        background: rgba(255,255,255,0.12);
        border: 1px solid rgba(0, 138, 255, 0.6);
        border-radius: 4px;
        color: rgba(255,255,255,0.9);
        font-size: 11px;
        cursor: pointer;
        height: 22px;
        line-height: 22px;
      }
      .ktalk-context-refresh-btn:hover {
        background: rgba(0, 138, 255, 0.2);
        border-color: rgba(0, 138, 255, 0.9);
      }
      .ktalk-context-refresh-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }
      .ktalk-context-model-select {
        background: rgba(255,255,255,0.1);
        border: 1px solid rgba(255,255,255,0.2);
        color: rgba(255,255,255,0.9);
        font-size: 12px;
        border-radius: 4px;
        padding: 2px 6px;
      }
      .ktalk-context-body {
        padding: 16px;
        overflow-y: auto;
        display: flex;
        flex-direction: column;
        gap: 12px;
        flex: 1;
      }
      .ktalk-context-body.loading {
        align-items: center;
        justify-content: center;
        color: rgba(255,255,255,0.7);
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
      .ktalk-anchor-section-title {
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 0.08em;
        color: rgba(255,255,255,0.5);
        margin-bottom: 4px;
      }
      .ktalk-anchor-topic {
        display: flex;
        gap: 8px;
        align-items: center;
        padding: 8px 10px;
        background: rgba(255,255,255,0.06);
        border-radius: 6px;
      }
      .ktalk-anchor-topic-text {
        font-size: 14px;
        font-weight: 600;
        color: rgba(255,255,255,0.92);
      }
      .ktalk-anchor-tone {
        font-size: 10px;
        padding: 2px 6px;
        border-radius: 999px;
        border: 1px solid transparent;
        text-transform: uppercase;
        letter-spacing: 0.06em;
      }
      .ktalk-anchor-tone.neutral {
        color: rgba(255,255,255,0.65);
        border-color: rgba(255,255,255,0.2);
      }
      .ktalk-anchor-tone.calm {
        color: #4ade80;
        border-color: rgba(74, 222, 128, 0.5);
      }
      .ktalk-anchor-tone.heated {
        color: #f87171;
        border-color: rgba(248, 113, 113, 0.6);
      }
      .ktalk-anchor-bullets {
        list-style: none;
        display: flex;
        flex-direction: column;
        gap: 8px;
        padding: 0;
        margin: 0;
      }
      .ktalk-anchor-bullet {
        display: flex;
        gap: 8px;
        align-items: flex-start;
      }
      .ktalk-anchor-ts-btn {
        background: rgba(255,255,255,0.08);
        border: 1px solid rgba(255,255,255,0.2);
        color: rgba(255,255,255,0.7);
        font-size: 11px;
        border-radius: 999px;
        padding: 2px 8px;
        cursor: pointer;
        font-family: monospace;
      }
      .ktalk-anchor-ts-btn:hover {
        border-color: rgba(0, 138, 255, 0.8);
        color: rgba(255,255,255,0.9);
      }
      .ktalk-anchor-ts-btn.empty {
        cursor: default;
        opacity: 0.5;
      }
      .ktalk-anchor-bullet-text {
        font-size: 13px;
        color: rgba(255,255,255,0.85);
        line-height: 1.4;
      }
      .ktalk-anchor-mention {
        font-size: 13px;
        padding: 8px 10px;
        border-radius: 6px;
        background: rgba(255,255,255,0.05);
      }
      .ktalk-anchor-mention.ok {
        color: #4ade80;
      }
      .ktalk-anchor-mention.alert {
        color: #fca5a5;
      }
      .ktalk-anchor-note {
        font-size: 11px;
        color: rgba(255,255,255,0.5);
        margin-top: 2px;
      }
      .ktalk-msg.anchor-highlight {
        box-shadow: 0 0 0 2px rgba(0, 138, 255, 0.6);
        border-radius: 6px;
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

function addTranscriptTab() {
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
        </span>
      </button>
    `;

  tabsContainer.appendChild(tab);

  tab.querySelector('button').addEventListener('click', () => {
    activateTranscriptTab();
  });

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptTabAdded = true;
  return true;
}

function addContextTab() {
  if (document.querySelector('.ktalk-context-tab')) {
    return true;
  }

  const tabsContainer = document.querySelector('.tl-tabset__content');
  if (!tabsContainer) {
    return false;
  }

  console.log('[KTalk Transcript] Adding context tab to tabset');

  const tab = document.createElement('div');
  tab.className = 'tl-tab _switchable ktalk-context-tab-wrapper';
  tab.innerHTML = `
      <button type="button" class="ktalk-context-tab link">
        <span class="tab-content">
          <span class="title">Context</span>
        </span>
      </button>
    `;

  tabsContainer.appendChild(tab);

  tab.querySelector('button').addEventListener('click', () => {
    activateContextTab();
  });

  return true;
}

function createTranscriptContent() {
  if (document.querySelector('.ktalk-transcript-content')) {
    return document.querySelector('.ktalk-transcript-content');
  }

  const contentArea = document.querySelector('.slide-menu-wrapper .content');
  if (!contentArea) {
    console.log('[KTalk Transcript] Content area not found');
    return null;
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent = contentArea.firstElementChild;

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement = document.createElement('div');
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.className = 'ktalk-transcript-content';
  loadBioReadSettings();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.innerHTML = `
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
              min="${_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_LIMITS.fixation.min}"
              max="${_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_LIMITS.fixation.max}"
              step="${_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_LIMITS.fixation.step}"
              value="${_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.fixation}"
            />
            <span class="ktalk-bioread-value" data-bioread-value="fixation"></span>
          </label>
          <label class="ktalk-bioread-control">
            <span>Contrast</span>
            <input
              type="range"
              class="ktalk-bioread-contrast"
              min="${_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_LIMITS.tailOpacity.min}"
              max="${_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_LIMITS.tailOpacity.max}"
              step="${_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_LIMITS.tailOpacity.step}"
              value="${_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.tailOpacity}"
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

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-transcript-copy-btn').addEventListener('click', copyAllTranscript);
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-transcript-follow-btn').addEventListener('click', toggleAutoFollow);
  updateFollowButton();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      requestSummary(btn.dataset.type);
      closeSummaryMenu();
    });
  });
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-scroll-top').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = 0;
  });
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-scroll-bottom').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = container.scrollHeight;
  });

  setupSummaryMenu(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement);
  setupAnchorControls();
  setupBioReadControls(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement);
  createSummaryModal();

  contentArea.appendChild(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement);

  console.log('[KTalk Transcript] Content element created');
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement;
}

function createContextContent() {
  if (document.querySelector('.ktalk-context-content')) {
    return document.querySelector('.ktalk-context-content');
  }

  const contentArea = document.querySelector('.slide-menu-wrapper .content');
  if (!contentArea) {
    console.log('[KTalk Transcript] Content area not found for context');
    return null;
  }

  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent = contentArea.firstElementChild;
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement = document.createElement('div');
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement.className = 'ktalk-context-content';
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement.innerHTML = `
      <div class="ktalk-context-header">
        <span class="ktalk-context-title">Context</span>
        <div class="ktalk-context-actions">
          <select class="ktalk-context-model-select">
            ${getAnchorModelOptions()}
          </select>
          <button class="ktalk-context-refresh-btn" title="Refresh (Alt+J)">Refresh</button>
        </div>
      </div>
      <div class="ktalk-context-body loading">
        <div class="ktalk-summary-spinner"></div>
        <span>Собираю контекст...</span>
      </div>
    `;

  const modelSelect = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement.querySelector('.ktalk-context-model-select');
  modelSelect.value = anchorModel;
  if (modelSelect.value !== anchorModel) {
    anchorModel = 'codex';
    modelSelect.value = anchorModel;
    localStorage.setItem('ktalk-anchor-model', anchorModel);
  }
  modelSelect.addEventListener('change', (e) => {
    anchorModel = e.target.value;
    localStorage.setItem('ktalk-anchor-model', anchorModel);
  });

  const refreshBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement.querySelector('.ktalk-context-refresh-btn');
  refreshBtn.addEventListener('click', () => {
    requestAnchor();
  });

  contentArea.appendChild(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement);
  console.log('[KTalk Transcript] Context element created');
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement;
}

function setActiveView(view) {
  const transcriptTabBtn = document.querySelector('.ktalk-transcript-tab');
  const contextTabBtn = document.querySelector('.ktalk-context-tab');

  const transcriptTab = transcriptTabBtn?.closest('.tl-tab');
  const contextTab = contextTabBtn?.closest('.tl-tab');

  const isTranscript = view === 'transcript';
  const isContext = view === 'context';

  if (transcriptTab) transcriptTab.classList.toggle('_active', isTranscript);
  if (contextTab) contextTab.classList.toggle('_active', isContext);

  if (transcriptTabBtn) transcriptTabBtn.classList.toggle('_active', isTranscript);
  if (contextTabBtn) contextTabBtn.classList.toggle('_active', isContext);

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent.style.display = isTranscript || isContext ? 'none' : '';
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.classList.toggle('active', isTranscript);
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement.classList.toggle('active', isContext);
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.isTranscriptActive = isTranscript;
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.isContextActive = isContext;
}

function activateTranscriptTab() {
  setActiveView('transcript');
}

function activateContextTab() {
  ensureContextContent();
  setActiveView('context');
  const now = Date.now();
  const staleMs = 90000;
  const isStale = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentAnchorData || (now - _state_js__WEBPACK_IMPORTED_MODULE_0__.state.anchorLastUpdated) > staleMs;
  if (isStale && !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.anchorRequestPending) {
    requestAnchor();
  }
}

function deactivateCustomTabs() {
  setActiveView(null);
}

function watchTabChanges() {
  const tabsContainer = document.querySelector('.tl-tabset__content');
  if (!tabsContainer) return;

  tabsContainer.addEventListener('click', (e) => {
    const clickedTab = e.target.closest('.tl-tab');
    if (!clickedTab) return;

    const isTranscriptTab = clickedTab.querySelector('.ktalk-transcript-tab');
    const isContextTab = clickedTab.querySelector('.ktalk-context-tab');
    if (!isTranscriptTab && !isContextTab) {
      deactivateCustomTabs();
    }
  });
}

function updateWsIndicator(connected) {
  document.querySelectorAll('.ktalk-transcript-tab-wrapper .ws-dot, .ktalk-context-tab-wrapper .ws-dot')
    .forEach((dot) => {
      dot.classList.toggle('connected', connected);
    });
}

function handleTranscriptHistory(data) {
  console.log('[KTalk Transcript] Received history:', data.segments?.length || 0, 'segments');

  if (!data.segments || data.segments.length === 0) {
    return;
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines = [];

  for (const seg of data.segments) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.push({
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

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length > _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.MAX_TRANSCRIPT_LINES) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice(-_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.MAX_TRANSCRIPT_LINES);
  }

  updateTranscriptUI();

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice() }
  }));
}

function handleNewMeeting(data) {
  console.log('[KTalk Transcript] New meeting:', data.meeting_id);

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines = [];
  updateTranscriptUI();

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: [] }
  }));
}

function handleTranscript(data) {
  const { speaker, text, timestamp, is_continuation, is_local, uncertain, uncertain_reason } = data;

  const newLine = {
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
  };

  if (is_continuation && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length > 0) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines[_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length - 1].text += ' ' + text;
  } else {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.push(newLine);

    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length > _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.MAX_TRANSCRIPT_LINES) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.shift();
    }
  }

  // Update platform-specific UI
  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
    (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__.addVas3kTranscriptLine)(newLine);
  } else {
    updateTranscriptUI();
  }

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice() }
  }));
}

function copyAllTranscript() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) return;

  const text = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.map(line => {
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.getUncertainSuffix)(line);
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

  const shouldAutoScroll = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled && isNearBottom(container);

  updateFollowButton();

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) {
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

  container.innerHTML = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.map(line => {
    const classes = [
      'ktalk-msg',
      line.is_local ? 'local' : 'remote',
      line.uncertain ? 'uncertain' : '',
      line.uncertain_reason === 'mics_off' ? 'mics-off' : ''
    ].filter(Boolean).join(' ');

    const speakerDisplay = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.getUncertainSuffix)(line);

    const messageText = formatTranscriptText(line.text);
    const timeValue = line.time || '';

    return `<div class="${classes}" data-ts="${(0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(timeValue)}">
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
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled;
  updateFollowButton();
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled) {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = container.scrollHeight;
  }
}

function updateFollowButton() {
  const btn = document.querySelector('.ktalk-transcript-follow-btn');
  if (!btn) return;
  btn.classList.toggle('active', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled);
  btn.textContent = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled ? 'Follow' : 'Paused';
}

function updateSummaryMenu() {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement) return;
  const toggle = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-summary-menu-toggle');
  const panel = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-summary-menu-panel');
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
      const activePanel = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement?.querySelector('.ktalk-summary-menu-panel');
      const activeToggle = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement?.querySelector('.ktalk-summary-menu-toggle');
      if (activePanel && activePanel.contains(event.target)) return;
      if (activeToggle && activeToggle.contains(event.target)) return;
      closeSummaryMenu();
    });
  }

  updateSummaryMenu();
}

function getAnchorModelOptions() {
  return [
    { value: 'codex', label: 'Codex (GPT-5.4)' },
    { value: 'codex:gpt-4.1', label: 'Codex (GPT-4.1)' },
    { value: 'codex:gpt-5.3-codex-spark', label: 'Codex Spark' },
    { value: 'claude:opus', label: 'Claude Opus' },
    { value: 'claude:sonnet', label: 'Claude Sonnet' },
    { value: 'gemini-3-pro-preview', label: 'Gemini 3.0 Pro' },
    { value: 'gemini-3-flash-preview', label: 'Gemini 3.0 Flash' },
    { value: 'gemini-2.5-pro', label: 'Gemini 2.5 Pro' },
    { value: 'gemini-2.5-flash', label: 'Gemini 2.5 Flash' }
  ].map(option => `<option value="${option.value}">${option.label}</option>`).join('');
}

function ensureContextContent() {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement) {
    createContextContent();
  }
}

function setContextLoading() {
  ensureContextContent();
  const body = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement?.querySelector('.ktalk-context-body');
  if (!body) return;
  body.className = 'ktalk-context-body loading';
  body.innerHTML = `
      <div class="ktalk-summary-spinner"></div>
      <span>Собираю контекст...</span>
    `;
  const refreshBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement?.querySelector('.ktalk-context-refresh-btn');
  if (refreshBtn) refreshBtn.disabled = true;
}

function updateAnchorContent(payload, approximate = false) {
  ensureContextContent();

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentAnchorData = payload;
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.anchorLastUpdated = Date.now();
  const data = payload?.data || payload || {};
  const tone = ['neutral', 'calm', 'heated'].includes(data.topic_tone) ? data.topic_tone : 'neutral';

  const topic = (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(data.current_topic || 'No recent context.');
  const bullets = Array.isArray(data.summary_bullets) ? data.summary_bullets : [];
  const mention = data.user_mentioned === true;
  const mentionText = mention
    ? (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(data.mention_context || 'Упоминание без деталей.')
    : 'Ваше имя не упоминалось.';
  const mentionTs = data.mention_ts_display ? (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(data.mention_ts_display) : '';

  const bulletsHtml = bullets.length > 0
    ? bullets.map((bullet) => {
      const text = (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(bullet.text || '');
      const ts = (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(bullet.ts_display || '');
      const tsButton = ts
        ? `<button class="ktalk-anchor-ts-btn" data-ts="${ts}">${ts}</button>`
        : '<span class="ktalk-anchor-ts-btn empty">--:--:--</span>';
      return `<li class="ktalk-anchor-bullet">${tsButton}<span class="ktalk-anchor-bullet-text">${text}</span></li>`;
    }).join('')
    : '<li class="ktalk-anchor-bullet"><span class="ktalk-anchor-bullet-text">Нет свежих пунктов.</span></li>';

  const mentionHtml = mention
    ? `<div class="ktalk-anchor-mention alert">Упомянули вас ${mentionTs ? `<button class="ktalk-anchor-ts-btn" data-ts="${mentionTs}">${mentionTs}</button>` : ''}<span>${mentionText}</span></div>`
    : `<div class="ktalk-anchor-mention ok">${mentionText}</div>`;

  const noteHtml = approximate ? '<div class="ktalk-anchor-note">Ответ может быть приблизительным.</div>' : '';

  const body = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement?.querySelector('.ktalk-context-body');
  if (!body) return;
  body.className = 'ktalk-context-body';
  body.innerHTML = `
      <div class="ktalk-anchor-section">
        <div class="ktalk-anchor-section-title">Right now</div>
        <div class="ktalk-anchor-topic">
          <span class="ktalk-anchor-tone ${tone}">${tone}</span>
          <span class="ktalk-anchor-topic-text">${topic}</span>
        </div>
      </div>
      <div class="ktalk-anchor-section">
        <div class="ktalk-anchor-section-title">Last minutes</div>
        <ul class="ktalk-anchor-bullets">${bulletsHtml}</ul>
      </div>
      <div class="ktalk-anchor-section">
        <div class="ktalk-anchor-section-title">Mentions</div>
        ${mentionHtml}
        ${noteHtml}
      </div>
    `;

  body.querySelectorAll('.ktalk-anchor-ts-btn').forEach((btn) => {
    if (btn.classList.contains('empty')) return;
    btn.addEventListener('click', () => {
      const ts = btn.getAttribute('data-ts');
      if (ts) scrollToAnchorTimestamp(ts);
    });
  });
}

function updateAnchorError(message) {
  ensureContextContent();
  const body = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement?.querySelector('.ktalk-context-body');
  if (!body) return;
  body.className = 'ktalk-context-body';
  body.innerHTML = `<div class="ktalk-summary-error">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(message || 'Не удалось получить контекст.')}</div>`;
}

function getLocalUserName() {
  const participants = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipants || [];
  const local = participants.find(p => p.isLocal && p.name);
  if (local?.name) return local.name;

  for (let idx = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length - 1; idx >= 0; idx -= 1) {
    const line = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines[idx];
    if (line && line.is_local && line.speaker) return line.speaker;
  }

  return '';
}

function requestAnchor() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.anchorRequestPending) return;
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) {
    updateAnchorError('Нет транскрипта для анализа');
    return;
  }

  meetingTitle = document.title || meetingTitle;

  const windowMs = 4 * 60 * 1000;
  const cutoff = Date.now() - windowMs;
  const recentLines = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.filter(line => line.createdAt && line.createdAt >= cutoff);
  const linesToSend = recentLines.length > 0 ? recentLines : _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice(-25);

  if (linesToSend.length === 0) {
    updateAnchorError('Нет данных для анализа');
    return;
  }

  const transcriptText = linesToSend.map(line => {
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.getUncertainSuffix)(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  setContextLoading();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.anchorRequestPending = true;

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_3__.sendWsMessage)({
    type: 'anchor_request',
    transcript: transcriptText,
    lines_count: linesToSend.length,
    window_seconds: Math.round(windowMs / 1000),
    model: anchorModel,
    user_name: getLocalUserName() || 'User',
    meeting_title: meetingTitle
  });
}

function scrollToAnchorTimestamp(ts) {
  const container = document.querySelector('.ktalk-transcript-messages');
  if (!container) return;

  const escaped = window.CSS && CSS.escape ? CSS.escape(ts) : ts.replace(/"/g, '\\"');
  const target = container.querySelector(`.ktalk-msg[data-ts="${escaped}"]`);
  if (!target) return;

  target.scrollIntoView({ behavior: 'smooth', block: 'center' });
  target.classList.add('anchor-highlight');
  setTimeout(() => target.classList.remove('anchor-highlight'), 1600);
}

function setupAnchorControls() {
  if (anchorHotkeyBound) return;
  anchorHotkeyBound = true;

  document.addEventListener('keydown', (event) => {
    if (!event.altKey || event.code !== 'KeyJ') return;
    if (isEditableTarget(event.target)) return;
    event.preventDefault();
    activateContextTab();
    requestAnchor();
  });
}

function handleAnchorResponse(data) {
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.anchorRequestPending = false;

  const refreshBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement?.querySelector('.ktalk-context-refresh-btn');
  if (refreshBtn) refreshBtn.disabled = false;

  if (data.error) {
    updateAnchorError(data.error);
    return;
  }

  updateAnchorContent(data.data || data, data.approximate);
}

function loadBioReadSettings() {
  const fallback = (0,_bioread_js__WEBPACK_IMPORTED_MODULE_1__.normalizeBioReadSettings)(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead);
  const saved = localStorage.getItem(_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_SETTINGS_KEY);
  if (!saved) {
    Object.assign(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead, fallback);
    return;
  }

  try {
    const parsed = JSON.parse(saved);
    Object.assign(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead, (0,_bioread_js__WEBPACK_IMPORTED_MODULE_1__.normalizeBioReadSettings)({ ...fallback, ...parsed }));
  } catch (e) {
    console.log('[KTalk Transcript] BioRead settings load failed:', e);
    Object.assign(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead, fallback);
  }
}

function saveBioReadSettings() {
  const payload = {
    enabled: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.enabled,
    fixation: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.fixation,
    tailOpacity: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.tailOpacity,
    ignoreNumbers: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.ignoreNumbers
  };
  localStorage.setItem(_bioread_js__WEBPACK_IMPORTED_MODULE_1__.BIOREAD_SETTINGS_KEY, JSON.stringify(payload));
}

function applyBioReadSettings({ persist = true, rerender = true } = {}) {
  Object.assign(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead, (0,_bioread_js__WEBPACK_IMPORTED_MODULE_1__.normalizeBioReadSettings)(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead));

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.style.setProperty(
      '--bioread-tail-opacity',
      (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.tailOpacity / 100).toString()
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
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement) return;

  const toggleBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-bioread-toggle');
  const settingsBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-bioread-settings-btn');
  const panel = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-bioread-panel');
  const fixationSlider = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-bioread-fixation');
  const contrastSlider = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-bioread-contrast');
  const fixationValue = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('[data-bioread-value="fixation"]');
  const contrastValue = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('[data-bioread-value="contrast"]');

  if (toggleBtn) toggleBtn.classList.toggle('active', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.enabled);
  if (settingsBtn) settingsBtn.classList.toggle('active', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.panelOpen);
  if (panel) panel.classList.toggle('active', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.panelOpen);

  if (fixationSlider) fixationSlider.value = String(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.fixation);
  if (contrastSlider) contrastSlider.value = String(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.tailOpacity);
  if (fixationValue) fixationValue.textContent = `${_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.fixation}%`;
  if (contrastValue) contrastValue.textContent = `${_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.tailOpacity}%`;
}

function setupBioReadControls(container) {
  const toggleBtn = container.querySelector('.ktalk-bioread-toggle');
  const settingsBtn = container.querySelector('.ktalk-bioread-settings-btn');
  const panel = container.querySelector('.ktalk-bioread-panel');
  const fixationSlider = container.querySelector('.ktalk-bioread-fixation');
  const contrastSlider = container.querySelector('.ktalk-bioread-contrast');

  if (toggleBtn) {
    toggleBtn.addEventListener('click', () => {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.enabled = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.enabled;
      applyBioReadSettings();
    });
  }

  if (settingsBtn) {
    settingsBtn.addEventListener('click', (event) => {
      event.stopPropagation();
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.panelOpen = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.panelOpen;
      updateBioReadControls();
    });
  }

  if (panel) {
    panel.addEventListener('click', (event) => event.stopPropagation());
  }

  if (fixationSlider) {
    fixationSlider.addEventListener('input', () => {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.fixation = parseInt(fixationSlider.value, 10);
      applyBioReadSettings();
    });
  }

  if (contrastSlider) {
    contrastSlider.addEventListener('input', () => {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.tailOpacity = parseInt(contrastSlider.value, 10);
      applyBioReadSettings();
    });
  }

  if (!bioReadHotkeyBound) {
    bioReadHotkeyBound = true;
    document.addEventListener('keydown', (event) => {
      if (!event.altKey || event.code !== 'KeyB') return;
      if (isEditableTarget(event.target)) return;
      event.preventDefault();
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.enabled = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.enabled;
      applyBioReadSettings();
    });
  }

  if (!bioReadPanelListenerBound) {
    bioReadPanelListenerBound = true;
    document.addEventListener('click', (event) => {
      if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.panelOpen) return;
      const activePanel = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement?.querySelector('.ktalk-bioread-panel');
      const activeSettingsBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement?.querySelector('.ktalk-bioread-settings-btn');
      if (activePanel && activePanel.contains(event.target)) return;
      if (activeSettingsBtn && activeSettingsBtn.contains(event.target)) return;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.panelOpen = false;
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
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead.enabled) {
    return safeText;
  }
  return (0,_bioread_js__WEBPACK_IMPORTED_MODULE_1__.formatBioReadText)(safeText, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.bioRead);
}


function createSummaryModal() {
  if (document.querySelector('.ktalk-summary-modal')) return;

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement = document.createElement('div');
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.className = 'ktalk-summary-modal';
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.innerHTML = `
      <div class="ktalk-summary-modal-content">
        <div class="ktalk-summary-modal-header">
          <span class="ktalk-summary-modal-title">✨ Catch Up</span>
          <select class="ktalk-model-select">
            ${getAnchorModelOptions()}
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

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-modal-close').addEventListener('click', closeSummaryModal);
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-copy-btn').addEventListener('click', copySummary);

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-question-submit').addEventListener('click', askFollowUpQuestion);
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-question-field').addEventListener('keypress', (e) => {
    if (e.key === 'Enter') askFollowUpQuestion();
  });

  const modelSelect = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-model-select');
  modelSelect.value = selectedModel;
  modelSelect.addEventListener('change', (e) => {
    selectedModel = e.target.value;
    localStorage.setItem('ktalk-ai-model', selectedModel);
  });

  const header = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-modal-header');
  let isDragging = false;
  let dragOffsetX = 0;
  let dragOffsetY = 0;

  header.addEventListener('mousedown', (e) => {
    if (e.target.classList.contains('ktalk-summary-modal-close')) return;
    isDragging = true;
    const rect = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.getBoundingClientRect();
    dragOffsetX = e.clientX - rect.left;
    dragOffsetY = e.clientY - rect.top;
    header.style.cursor = 'grabbing';
  });

  document.addEventListener('mousemove', (e) => {
    if (!isDragging) return;
    e.preventDefault();
    const x = e.clientX - dragOffsetX;
    const y = e.clientY - dragOffsetY;
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.style.left = x + 'px';
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.style.top = y + 'px';
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.style.right = 'auto';
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.style.bottom = 'auto';
  });

  document.addEventListener('mouseup', () => {
    if (isDragging) {
      isDragging = false;
      header.style.cursor = 'move';
    }
  });

  document.body.appendChild(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement);
}

function showSummaryModal(summaryType = 'catchup') {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement) {
    createSummaryModal();
  }

  const titles = {
    catchup: '🎯 Catch Up',
    brief: '📋 Краткое (2 мин)',
    full: '💬 Вся встреча'
  };
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-modal-title').textContent = titles[summaryType] || titles.catchup;

  const questionInput = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-question-input');
  questionInput.style.display = summaryType === 'full' ? 'flex' : 'none';

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.classList.add('active');
  const body = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-modal-body');
  body.className = 'ktalk-summary-modal-body loading';
  body.innerHTML = `
      <div class="ktalk-summary-spinner"></div>
      <span>Анализирую транскрипт...</span>
    `;
}

function closeSummaryModal() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.classList.remove('active');
  }
}

function updateSummaryContent(text, isError = false) {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement) return;

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryText = text;
  const body = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-modal-body');
  body.className = 'ktalk-summary-modal-body';

  if (isError) {
    body.innerHTML = `<div class="ktalk-summary-error">${text}</div>`;
  } else {
    body.innerHTML = `<div class="ktalk-summary-text">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.renderMarkdown)(text)}</div>`;
  }
}

function copySummary() {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryText) return;

  navigator.clipboard.writeText(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryText).then(() => {
    const btn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-copy-btn');
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
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryRequestPending) return;
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) {
    alert('Нет транскрипта для анализа');
    return;
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryType = summaryType;
  let linesToSend;

  if (summaryType === 'catchup') {
    const oneMinuteAgo = Date.now() - 60000;
    const recentLines = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.filter(line => line.createdAt && line.createdAt >= oneMinuteAgo);
    linesToSend = recentLines.length > 0 ? recentLines : _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice(-10);
  } else if (summaryType === 'brief') {
    const twoMinutesAgo = Date.now() - 120000;
    const recentLines = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.filter(line => line.createdAt && line.createdAt >= twoMinutesAgo);
    linesToSend = recentLines.length > 0 ? recentLines : _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice(-20);
  } else {
    linesToSend = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines;
  }

  if (linesToSend.length === 0) {
    alert('Нет данных для анализа');
    return;
  }

  const transcriptText = linesToSend.map(line => {
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.getUncertainSuffix)(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  showSummaryModal(summaryType);
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryRequestPending = true;

  document.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.classList.add('loading');
    btn.disabled = true;
  });

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_3__.sendWsMessage)({
    type: 'summarize_request',
    summary_type: summaryType,
    transcript: transcriptText,
    lines_count: linesToSend.length,
    model: selectedModel,
    meeting_title: meetingTitle
  });
}

function askFollowUpQuestion() {
  const input = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-question-field');
  const question = input.value.trim();
  if (!question) return;

  const transcriptText = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.map(line => {
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.getUncertainSuffix)(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  const body = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-summary-modal-body');
  body.innerHTML = `
      <div class="ktalk-summary-spinner"></div>
      <span>Отвечаю на вопрос...</span>
    `;
  body.classList.add('loading');

  const submitBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement.querySelector('.ktalk-question-submit');
  submitBtn.disabled = true;

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_3__.sendWsMessage)({
    type: 'summarize_request',
    summary_type: 'question',
    transcript: transcriptText,
    question: question,
    previous_summary: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSummaryText,
    model: selectedModel,
    meeting_title: meetingTitle
  });

  input.value = '';
}

function handleSummaryResponse(data) {
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryRequestPending = false;

  document.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.classList.remove('loading');
    btn.disabled = false;
  });

  const submitBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryModalElement?.querySelector('.ktalk-question-submit');
  if (submitBtn) submitBtn.disabled = false;

  if (data.error) {
    updateSummaryContent('Ошибка: ' + data.error, true);
  } else if (data.summary) {
    updateSummaryContent(data.summary);
  }
}

/**
 * Initialize platform-specific UI
 * For vas3k.cloud: creates floating panel
 * For KTalk: uses existing tab injection (handled separately)
 */
function initPlatformUI() {
  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
    return (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__.initVas3kUI)();
  }

  // For KTalk, return false - tabs are handled by addTranscriptTab/addContextTab
  return false;
}

/**
 * Platform-aware WebSocket indicator update
 */
function updateWsIndicatorPlatform(connected) {
  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
    (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__.updateVas3kWsIndicator)(connected);
  } else {
    updateWsIndicator(connected);
  }
}

/**
 * Platform-aware transcript line addition
 */
function addTranscriptLinePlatform(line) {
  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
    (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__.addVas3kTranscriptLine)(line);
  } else {
    addTranscriptLine(line);
  }
}


/***/ },

/***/ "./src/utils.js"
/*!**********************!*\
  !*** ./src/utils.js ***!
  \**********************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   escapeHtml: () => (/* binding */ escapeHtml),
/* harmony export */   getFullName: () => (/* binding */ getFullName),
/* harmony export */   getParticipantName: () => (/* binding */ getParticipantName),
/* harmony export */   getUncertainSuffix: () => (/* binding */ getUncertainSuffix),
/* harmony export */   renderMarkdown: () => (/* binding */ renderMarkdown),
/* harmony export */   sendUpdate: () => (/* binding */ sendUpdate)
/* harmony export */ });
function sendUpdate(data) {
  window.dispatchEvent(new CustomEvent('ktalk-speaker-update', { detail: data }));
}

function getFullName(participant, info, fallback) {
  const userInfo = participant?._userInfo$?.value;
  if (userInfo) {
    const firstName = userInfo.firstName || '';
    const lastName = userInfo.lastName || '';
    if (firstName && lastName) return `${firstName} ${lastName}`;
    if (firstName) return firstName;
  }

  if (info) {
    const firstName = info.firstName || '';
    const lastName = info.lastName || '';
    if (firstName && lastName) return `${firstName} ${lastName}`;
    if (firstName) return firstName;
  }

  return fallback || 'Unknown';
}

function getParticipantName(conf, participantId) {
  if (!participantId) return 'Unknown';

  if (conf.localParticipant && conf.localParticipant.id === participantId) {
    const p = conf.localParticipant;
    const info = p._participantInfo$?.value || p._userInfo$?.value;
    return getFullName(p, info, p.name || p.displayName || 'You');
  }

  if (conf._participantsMap) {
    const participant = conf._participantsMap.get(participantId);
    if (participant) {
      const info = participant._participantInfo$?.value || participant._userInfo$?.value;
      return getFullName(participant, info, participant.name || participant.displayName || participantId);
    }
  }

  return participantId;
}

function getUncertainSuffix(line) {
  if (!line.uncertain) return '';
  if (line.uncertain_reason === 'mics_off') return ' (🔇?)';
  return ' (?)';
}

function renderMarkdown(text) {
  let html = escapeHtml(text);

  html = html.replace(/^## (.+)$/gm, '<h3>$1</h3>');
  html = html.replace(/^### (.+)$/gm, '<h4>$1</h4>');

  html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
  html = html.replace(/\*([^*]+)\*/g, '<em>$1</em>');

  html = html.replace(/^- (.+)$/gm, '<li>$1</li>');
  html = html.replace(/(<li>.*<\/li>\n?)+/g, '<ul>$&</ul>');

  html = html.replace(/\n/g, '<br>');

  html = html.replace(/<\/h3><br>/g, '</h3>');
  html = html.replace(/<\/h4><br>/g, '</h4>');
  html = html.replace(/<\/ul><br>/g, '</ul>');
  html = html.replace(/<br><ul>/g, '<ul>');

  return html;
}

function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}


/***/ },

/***/ "./src/ws.js"
/*!*******************!*\
  !*** ./src/ws.js ***!
  \*******************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   connectWebSocket: () => (/* binding */ connectWebSocket)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _ui_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./ui.js */ "./src/ui.js");
/* harmony import */ var _ui_vas3k_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./ui-vas3k.js */ "./src/ui-vas3k.js");
/* harmony import */ var _speaker_js__WEBPACK_IMPORTED_MODULE_3__ = __webpack_require__(/*! ./speaker.js */ "./src/speaker.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_4__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
/* harmony import */ var _meeting_js__WEBPACK_IMPORTED_MODULE_5__ = __webpack_require__(/*! ./meeting.js */ "./src/meeting.js");
/* harmony import */ var _platforms_index_js__WEBPACK_IMPORTED_MODULE_6__ = __webpack_require__(/*! ./platforms/index.js */ "./src/platforms/index.js");








function connectWebSocket() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.readyState === WebSocket.OPEN) {
    return;
  }

  try {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection = new WebSocket(_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.WS_URL);

    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.onopen = () => {
      console.log('[KTalk Transcript] WebSocket connected');
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected = true;
      (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicatorPlatform)(true);

      // Send participants for both KTalk and vas3k
      const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_6__.getPlatformAdapter)();
      if (adapter) {
        const conf = adapter.getConference();
        if (conf) {
          (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
          const activeSpeaker = (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.getActiveSpeaker)(conf);
          (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendFilteredSpeakerUpdate)(activeSpeaker);
          (0,_meeting_js__WEBPACK_IMPORTED_MODULE_5__.sendMeetingJoin)();
        }
      } else if (window.$conf) {
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(window.$conf);
        const activeSpeaker = (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.getActiveSpeaker)(window.$conf);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendFilteredSpeakerUpdate)(activeSpeaker);
        (0,_meeting_js__WEBPACK_IMPORTED_MODULE_5__.sendMeetingJoin)();
      }

      (0,_meeting_js__WEBPACK_IMPORTED_MODULE_5__.startMeetingCheck)();

      // Clock sync for accurate timestamp correlation
      (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_4__.sendWsMessage)({ type: 'clock_sync', browser_time: Date.now() });

      // Re-sync clock every 30 seconds
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.clockSyncTimer = setInterval(() => {
        (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_4__.sendWsMessage)({ type: 'clock_sync', browser_time: Date.now() });
      }, 30000);
    };

    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.onclose = () => {
      console.log('[KTalk Transcript] WebSocket disconnected');
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected = false;
      (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicatorPlatform)(false);

      if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.clockSyncTimer) {
        clearInterval(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.clockSyncTimer);
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.clockSyncTimer = null;
      }

      if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsReconnectTimer) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsReconnectTimer = setTimeout(() => {
          _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsReconnectTimer = null;
          connectWebSocket();
        }, 5000);
      }
    };

    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.onerror = () => {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected = false;
    };

    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data);
        if (data.type === 'transcript') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleTranscript)(data);
        } else if (data.type === 'transcript_history') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleTranscriptHistory)(data);
        } else if (data.type === 'new_meeting') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleNewMeeting)(data);
        } else if (data.type === 'summarize_response') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleSummaryResponse)(data);
          (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_2__.handleVas3kSummaryResponse)(data);
        } else if (data.type === 'anchor_response') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleAnchorResponse)(data);
        }
      } catch (e) {
        console.log('[KTalk Transcript] Parse error:', e);
      }
    };

  } catch (e) {
    console.log('[KTalk Transcript] Connection failed:', e);
  }
}


/***/ },

/***/ "./src/ws_api.js"
/*!***********************!*\
  !*** ./src/ws_api.js ***!
  \***********************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   sendWsConnectionStatus: () => (/* binding */ sendWsConnectionStatus),
/* harmony export */   sendWsMessage: () => (/* binding */ sendWsMessage),
/* harmony export */   sendWsParticipants: () => (/* binding */ sendWsParticipants),
/* harmony export */   sendWsSpeakerUpdate: () => (/* binding */ sendWsSpeakerUpdate)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");


let messageSeq = 0;

function sendWsMessage(data) {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.readyState === WebSocket.OPEN) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.send(JSON.stringify(data));
  }
}

function sendWsSpeakerUpdate(speaker) {
  sendWsMessage({
    type: 'speaker_update',
    speaker: speaker,
    timestamp: Date.now(),
    seq: ++messageSeq
  });
}

function sendWsParticipants(participants) {
  sendWsMessage({
    type: 'participants_update',
    participants: participants
  });
}

function sendWsConnectionStatus(connected) {
  sendWsMessage({
    type: 'connection_status',
    connected: connected
  });
}


/***/ }

/******/ 	});
/************************************************************************/
/******/ 	// The module cache
/******/ 	var __webpack_module_cache__ = {};
/******/ 	
/******/ 	// The require function
/******/ 	function __webpack_require__(moduleId) {
/******/ 		// Check if module is in cache
/******/ 		var cachedModule = __webpack_module_cache__[moduleId];
/******/ 		if (cachedModule !== undefined) {
/******/ 			return cachedModule.exports;
/******/ 		}
/******/ 		// Check if module exists (development only)
/******/ 		if (__webpack_modules__[moduleId] === undefined) {
/******/ 			var e = new Error("Cannot find module '" + moduleId + "'");
/******/ 			e.code = 'MODULE_NOT_FOUND';
/******/ 			throw e;
/******/ 		}
/******/ 		// Create a new module (and put it into the cache)
/******/ 		var module = __webpack_module_cache__[moduleId] = {
/******/ 			// no module.id needed
/******/ 			// no module.loaded needed
/******/ 			exports: {}
/******/ 		};
/******/ 	
/******/ 		// Execute the module function
/******/ 		__webpack_modules__[moduleId](module, module.exports, __webpack_require__);
/******/ 	
/******/ 		// Return the exports of the module
/******/ 		return module.exports;
/******/ 	}
/******/ 	
/************************************************************************/
/******/ 	/* webpack/runtime/define property getters */
/******/ 	(() => {
/******/ 		// define getter functions for harmony exports
/******/ 		__webpack_require__.d = (exports, definition) => {
/******/ 			for(var key in definition) {
/******/ 				if(__webpack_require__.o(definition, key) && !__webpack_require__.o(exports, key)) {
/******/ 					Object.defineProperty(exports, key, { enumerable: true, get: definition[key] });
/******/ 				}
/******/ 			}
/******/ 		};
/******/ 	})();
/******/ 	
/******/ 	/* webpack/runtime/hasOwnProperty shorthand */
/******/ 	(() => {
/******/ 		__webpack_require__.o = (obj, prop) => (Object.prototype.hasOwnProperty.call(obj, prop))
/******/ 	})();
/******/ 	
/******/ 	/* webpack/runtime/make namespace object */
/******/ 	(() => {
/******/ 		// define __esModule on exports
/******/ 		__webpack_require__.r = (exports) => {
/******/ 			if(typeof Symbol !== 'undefined' && Symbol.toStringTag) {
/******/ 				Object.defineProperty(exports, Symbol.toStringTag, { value: 'Module' });
/******/ 			}
/******/ 			Object.defineProperty(exports, '__esModule', { value: true });
/******/ 		};
/******/ 	})();
/******/ 	
/************************************************************************/
var __webpack_exports__ = {};
// This entry needs to be wrapped in an IIFE because it needs to be isolated against other modules in the chunk.
(() => {
/*!**********************!*\
  !*** ./src/index.js ***!
  \**********************/
__webpack_require__.r(__webpack_exports__);
/* harmony import */ var _init_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./init.js */ "./src/init.js");


(0,_init_js__WEBPACK_IMPORTED_MODULE_0__.init)();

})();

/******/ })()
;