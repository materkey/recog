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
/* harmony import */ var _pcm_transport_js__WEBPACK_IMPORTED_MODULE_3__ = __webpack_require__(/*! ../pcm_transport.js */ "./src/pcm_transport.js");





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
  const pcm = (0,_pcm_transport_js__WEBPACK_IMPORTED_MODULE_3__.pcmGetDiagnostics)();

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
    ['filterReason', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.audioFilterLastReason ?? '—'],
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
    wsConnected: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected,
    conferenceReady: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady,
    currentMeetingUrl: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl,
    activeSpeaker: activeSpeaker,
    participants: participants,
    participantsMapSize: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastParticipantsMapSize,
    jitsiParticipantsCount: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsCount,
    jitsiParticipantsMapCount: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastJitsiParticipantsMapCount,
    transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length,
    speakerHistory: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.length,
    pcmTransport: pcm
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

/***/ "./src/focus_guard.js"
/*!****************************!*\
  !*** ./src/focus_guard.js ***!
  \****************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   installFocusGuard: () => (/* binding */ installFocusGuard)
/* harmony export */ });
/**
 * Focus guard.
 *
 * On macOS an activated browser window yanks the user back to Chrome's
 * Space ("When switching to an application, switch to a Space with open
 * windows for the application"). A conference page has a handful of
 * vectors that can activate the window while the user works elsewhere.
 *
 * Suppressed:
 * - window.focus() — on a meeting page its only purpose is "bring the
 *   meeting back", which is exactly the behavior being disabled.
 *
 * Logged but not suppressed (they have legitimate in-meeting uses, and
 * the log identifies the culprit if the yank persists):
 * - Element.requestFullscreen
 * - new Notification(...)
 * - alert / confirm / prompt (native dialogs activate the window)
 */
function installFocusGuard(win = window) {
  if (win.__ktalkFocusGuardInstalled) return;
  win.__ktalkFocusGuardInstalled = true;

  try {
    win.focus = function focusSuppressed() {
      console.log('[KTalk Recog] window.focus() suppressed by focus guard');
    };
  } catch (e) {
    console.log('[KTalk Recog] focus guard: window.focus override failed', e);
  }

  const elementProto = win.Element && win.Element.prototype;
  if (elementProto && typeof elementProto.requestFullscreen === 'function') {
    const origRequestFullscreen = elementProto.requestFullscreen;
    elementProto.requestFullscreen = function (...args) {
      console.log('[KTalk Recog] requestFullscreen:', this.tagName);
      return origRequestFullscreen.apply(this, args);
    };
  }

  if (typeof win.Notification === 'function') {
    win.Notification = new Proxy(win.Notification, {
      construct(target, args) {
        console.log('[KTalk Recog] Notification created:', args[0]);
        return Reflect.construct(target, args);
      }
    });
  }

  ['alert', 'confirm', 'prompt'].forEach((name) => {
    if (typeof win[name] !== 'function') return;
    const orig = win[name].bind(win);
    win[name] = function (...args) {
      console.log(`[KTalk Recog] ${name}() called by page:`, args[0]);
      return orig(...args);
    };
  });

  const videoProto = win.HTMLVideoElement && win.HTMLVideoElement.prototype;
  if (videoProto && typeof videoProto.requestPictureInPicture === 'function') {
    const origRequestPip = videoProto.requestPictureInPicture;
    videoProto.requestPictureInPicture = function (...args) {
      console.log('[KTalk Recog] requestPictureInPicture called');
      return origRequestPip.apply(this, args);
    };
  }

  // Timestamped window-activation trace: when macOS yanks the user back
  // to Chrome's Space, this window gains focus — the log pinpoints the
  // moment so it can be correlated with front-app-monitor.sh and the
  // vector logs above (see test-stand/).
  if (typeof win.addEventListener === 'function') {
    const stamp = () => {
      const d = new Date();
      return d.toTimeString().slice(0, 8) + '.' + String(d.getMilliseconds()).padStart(3, '0');
    };
    win.addEventListener('focus', () => console.log(`[KTalk Recog] window FOCUS at ${stamp()}`));
    win.addEventListener('blur', () => console.log(`[KTalk Recog] window blur at ${stamp()}`));
    const doc = win.document;
    if (doc && typeof doc.addEventListener === 'function') {
      doc.addEventListener('visibilitychange', () => {
        console.log(`[KTalk Recog] visibility=${doc.visibilityState} at ${stamp()}`);
      });
      doc.addEventListener('fullscreenchange', () => {
        const el = doc.fullscreenElement;
        console.log(`[KTalk Recog] fullscreenchange element=${el ? el.tagName : 'none'} at ${stamp()}`);
      });
    }
  }
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
  const aiTabAdded = (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.addAiTab)();
  if (aiTabAdded) {
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createTranscriptContent)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createContextContent)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createCopilotContent)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.watchTabChanges)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicator)(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected);
  }

  (0,_debug_panel_js__WEBPACK_IMPORTED_MODULE_6__.ensureDebugPanel)();
  return aiTabAdded;
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
      // For KTalk: check tab exists
      if (!document.querySelector('.ktalk-ai-tab')) {
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
      if (!document.querySelector('.ktalk-copilot-content')) {
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement = null;
        _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent = null;
        (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createCopilotContent)();
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
/* harmony export */   handleMeetingJoinRejected: () => (/* binding */ handleMeetingJoinRejected),
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

function handleMeetingJoinRejected(data) {
  // Bridge сообщает, что активная встреча уже занята другим табом.
  // Очищаем local-кеш `currentMeetingUrl`, чтобы startMeetingCheck() через
  // ~2 с повторил sendMeetingJoin (когда owner-tab может уже закрыться),
  // и не сетим `state.currentMeetingId` — handleTranscript filter тогда
  // дропнет броадкасты соседнего таба и не загрязнит нашу UI стеной.
  const activeId = data && data.active_meeting_id ? data.active_meeting_id : 'unknown';
  console.log(
    `[KTalk Transcript] meeting_join rejected by bridge — another tab owns meeting ${activeId}. Will retry on next poll.`
  );
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl = null;
  titleSentForUrl = null;
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

/***/ "./src/pcm_transport.js"
/*!******************************!*\
  !*** ./src/pcm_transport.js ***!
  \******************************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   pcmAttachAllFromConf: () => (/* binding */ pcmAttachAllFromConf),
/* harmony export */   pcmAttachParticipant: () => (/* binding */ pcmAttachParticipant),
/* harmony export */   pcmDetachParticipant: () => (/* binding */ pcmDetachParticipant),
/* harmony export */   pcmGetDiagnostics: () => (/* binding */ pcmGetDiagnostics),
/* harmony export */   pcmTransportDispose: () => (/* binding */ pcmTransportDispose),
/* harmony export */   pcmUpdateParticipantName: () => (/* binding */ pcmUpdateParticipantName)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
// Per-speaker PCM transport.
//
// Per decision D4/D9 in docs/plans/2026-04-22-per-speaker-asr-pipeline.md:
// each remote KTalk participant's audio track is routed through an
// AudioWorkletNode (see src/audio-worklet/downsampler.js) that emits
// 16 kHz Int16 PCM chunks every 3.0 s stride with 3.6 s payload. Chunks
// are base64-encoded and shipped to ktalk_bridge via sendWsPcmChunk().
//
// Local-mic participant is intentionally skipped (decision D9): the host
// tab does not expose its own mic as a remote track and unifying paths
// would add risk without benefit; the sounddevice path stays the source
// of truth for the local mic.




const BOUND_VIA = {
  WRAPPER: 'wrapper',
  RECEIVER: 'receiver',
};

const REBIND_REASON = {
  INITIAL_PROBE: 'initial_receiver_probe',
  JITSI_TRACK_UPDATE: 'jitsi_track_update',
  JITSI_UPDATE_RECEIVER_PROBE: 'jitsi_track_update_receiver_probe',
  DIAGNOSTICS_PROBE: 'diagnostics_receiver_probe',
};

function createEmptyWebrtcStats() {
  return {
    pc_count: 0,
    inbound_audio_bytes_received: 0,
    inbound_audio_bytes_delta: 0,
    inbound_audio_packets_received: 0,
    inbound_audio_packets_delta: 0,
  };
}

let audioContext = null;
let workletLoadPromise = null;
let diagnosticsTimer = null;
let lastStatsSentAtMs = 0;
let lastWebrtcStats = createEmptyWebrtcStats();

const pending = new Map(); // pid -> {speakerName, isLocal, mediaStream, jitsiTrack, jitsiConf, conf}
const active = new Map();  // pid -> binding entry

function getWorkletUrl() {
  if (typeof window !== 'undefined' && window.__PCM_WORKLET_URL__) {
    return window.__PCM_WORKLET_URL__;
  }
  // content.js stashes the URL on <html> via setAttribute because
  // inline-script injection is blocked by KTalk's CSP. Reading a DOM
  // attribute from the page world is always allowed.
  if (typeof document !== 'undefined' && document.documentElement) {
    const attr = document.documentElement.getAttribute('data-pcm-worklet-url');
    if (attr) return attr;
  }
  return null;
}

function ensureAudioContext() {
  if (!audioContext || audioContext.state === 'closed') {
    try {
      audioContext = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
      console.log(
        '[PcmTransport] AudioContext created, state=', audioContext.state,
        'baseLatency=', audioContext.baseLatency,
        'sampleRate=', audioContext.sampleRate
      );
      // Without this resume() the context stays suspended under
      // autoplay-policy (Playwright + headful Chromium in particular) —
      // worklet addModule succeeds and the graph builds but process()
      // never runs, so no pcm_chunk is ever emitted. Safe no-op when
      // ctx is already running.
      audioContext.resume().then(
        () => console.log('[PcmTransport] AudioContext resumed, state=', audioContext?.state),
        (e) => console.warn('[PcmTransport] AudioContext resume failed:', e?.message || e)
      );
    } catch (e) {
      console.error('[PcmTransport] AudioContext creation failed:', e);
      audioContext = null;
    }
  }
  return audioContext;
}

async function ensureWorkletModule(ctx) {
  if (!ctx || !ctx.audioWorklet) {
    console.warn('[PcmTransport] ensureWorkletModule: no ctx or no audioWorklet API');
    return false;
  }
  if (workletLoadPromise) {
    return workletLoadPromise;
  }
  const url = getWorkletUrl();
  if (!url) {
    console.warn('[PcmTransport] window.__PCM_WORKLET_URL__ not set; worklet cannot be loaded');
    return false;
  }
  console.log('[PcmTransport] Loading worklet module from', url);
  workletLoadPromise = ctx.audioWorklet
    .addModule(url)
    .then(() => {
      console.log('[PcmTransport] Worklet module loaded OK');
      return true;
    })
    .catch((e) => {
      console.error('[PcmTransport] Worklet module load failed:', e?.message || e);
      workletLoadPromise = null;
      return false;
    });
  return workletLoadPromise;
}

function int16ToBase64(int16) {
  const bytes = new Uint8Array(int16.buffer, int16.byteOffset, int16.byteLength);
  let binary = '';
  const chunkSize = 8192;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const slice = bytes.subarray(i, Math.min(i + chunkSize, bytes.length));
    binary += String.fromCharCode.apply(null, slice);
  }
  return btoa(binary);
}

function logMaterializeSkip(pid, reason, warn = false) {
  const fn = warn ? console.warn : console.log;
  fn('[PcmTransport] materialize skip pid=', pid, 'reason:', reason);
}

function getAudioTrackFromStream(stream) {
  if (!stream || typeof stream.getAudioTracks !== 'function') return null;
  const tracks = stream.getAudioTracks();
  return tracks && tracks.length ? tracks[0] : null;
}

function getNativeTrackFromJitsiTrack(jitsiTrack) {
  if (!jitsiTrack) return null;
  const track = jitsiTrack.getTrack?.() || jitsiTrack.track;
  return track && track.kind === 'audio' ? track : null;
}

function getStreamFromJitsiTrack(jitsiTrack) {
  if (!jitsiTrack) return null;
  const stream = jitsiTrack.stream || jitsiTrack.getStream?.() || jitsiTrack.getOriginalStream?.();
  if (stream) return stream;
  const track = getNativeTrackFromJitsiTrack(jitsiTrack);
  if (track && typeof MediaStream !== 'undefined') return new MediaStream([track]);
  return null;
}

function getTrackIdSet(infoOrEntry) {
  const ids = new Set();
  const add = (track) => {
    if (track && track.kind === 'audio' && track.id) ids.add(String(track.id));
  };
  add(getNativeTrackFromJitsiTrack(infoOrEntry?.jitsiTrack));
  add(getAudioTrackFromStream(infoOrEntry?.mediaStream));
  if (infoOrEntry?.requestedTrackId) ids.add(String(infoOrEntry.requestedTrackId));
  if (infoOrEntry?.boundTrackId) ids.add(String(infoOrEntry.boundTrackId));
  return ids;
}

function createHiddenAudioConsumer(pid, stream, trackId) {
  if (typeof document === 'undefined' || !document.createElement || !stream) {
    return { element: null, state: 'unavailable', error: '' };
  }
  try {
    const audio = document.createElement('audio');
    audio.autoplay = true;
    audio.muted = true;
    audio.playsInline = true;
    audio.controls = false;
    audio.srcObject = stream;
    audio.setAttribute('data-recog-pcm-consumer', String(pid));
    if (trackId) audio.setAttribute('data-recog-track-id', String(trackId));
    audio.style.position = 'fixed';
    audio.style.left = '-10000px';
    audio.style.top = '0';
    audio.style.width = '1px';
    audio.style.height = '1px';
    audio.style.opacity = '0';
    audio.style.pointerEvents = 'none';
    (document.body || document.documentElement).appendChild(audio);
    const consumer = { element: audio, state: 'created', error: '' };
    if (typeof audio.play === 'function') {
      audio.play().then(
        () => { consumer.state = 'playing'; },
        (e) => {
          consumer.state = 'play_failed';
          consumer.error = e?.message || String(e);
          console.warn('[PcmTransport] hidden audio consumer play() failed pid=', pid, consumer.error);
        }
      );
    }
    return consumer;
  } catch (e) {
    return { element: null, state: 'failed', error: e?.message || String(e) };
  }
}

function destroyConsumer(entry) {
  const element = entry?.consumer?.element;
  if (!element) return;
  try {
    element.pause();
    element.srcObject = null;
    element.remove();
  } catch (_) {
    /* ignore */
  }
  entry.consumer = null;
}

function refreshConsumer(pid, entry, stream, trackId) {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED) return;
  const existingStream = entry.consumer?.element?.srcObject;
  if (existingStream === stream) return;
  destroyConsumer(entry);
  entry.consumer = createHiddenAudioConsumer(pid, stream, trackId);
}

function createSourceBinding(ctx, stream, node) {
  const source = ctx.createMediaStreamSource(stream);
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 512;
  analyser.smoothingTimeConstant = 0.2;
  const analyserBuf = new Float32Array(analyser.fftSize);
  try {
    source.connect(analyser);
    analyser.connect(node);
  } catch (e) {
    try { source.disconnect(); } catch (_) { /* ignore */ }
    try { analyser.disconnect(); } catch (_) { /* ignore */ }
    throw e;
  }
  return { source, analyser, analyserBuf };
}

function disconnectSource(entry) {
  try { entry.source?.disconnect(); } catch (_) { /* already disconnected */ }
  try { entry.analyser?.disconnect(); } catch (_) { /* already disconnected */ }
}

function bindEntryToStream(pid, entry, stream, via, trackObject = null) {
  const ctx = ensureAudioContext();
  if (!ctx || !stream || !entry.node) return false;
  const track = trackObject || getAudioTrackFromStream(stream);
  if (track?.readyState === 'ended') {
    entry.lastError = 'track_ended';
    return false;
  }

  let binding;
  try {
    binding = createSourceBinding(ctx, stream, entry.node);
  } catch (e) {
    entry.lastError = e?.message || String(e);
    console.warn('[PcmTransport] source bind failed pid=', pid, entry.lastError);
    return false;
  }

  disconnectSource(entry);
  entry.source = binding.source;
  entry.analyser = binding.analyser;
  entry.analyserBuf = binding.analyserBuf;
  entry.mediaStream = stream;
  entry.boundTrackObject = track || null;
  entry.boundTrackId = track?.id || '';
  entry.boundVia = via || BOUND_VIA.WRAPPER;
  entry.trackReadyState = track?.readyState || '';
  entry.trackMuted = track?.muted === true;
  entry.lastError = '';
  refreshConsumer(pid, entry, stream, entry.boundTrackId);
  return true;
}

function sendDiagnosticsSnapshot(reason = 'snapshot') {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED) return;
  try {
    lastStatsSentAtMs = Date.now();
    (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_1__.sendWsPcmTransportStats)(pcmGetDiagnostics());
  } catch (e) {
    console.warn('[PcmTransport] diagnostics', reason, 'failed:', e?.message || e);
  }
}

async function materialize(pid, info) {
  if (active.has(pid)) return logMaterializeSkip(pid, 'already active');
  const ctx = ensureAudioContext();
  if (!ctx) return logMaterializeSkip(pid, 'no AudioContext', true);
  const loaded = await ensureWorkletModule(ctx);
  if (!loaded) return logMaterializeSkip(pid, 'worklet not loaded', true);

  // By the time addModule resolves, attach may have been cancelled.
  if (!pending.has(pid)) return logMaterializeSkip(pid, 'pending cleared during await');
  // Two concurrent materialize(pid) calls can both pass the top-of-function
  // active.has(pid) check (gate flip + pcmAttachParticipant both dispatch),
  // then both await, then both fall through to create a node/source — the
  // second active.set overwrites the first entry and leaks its nodes.
  // Re-check after the await so only one wins.
  if (active.has(pid)) return logMaterializeSkip(pid, 'raced another materialize');

  try {
    const node = new AudioWorkletNode(ctx, 'pcm-downsampler', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
      channelCount: 1,
      channelCountMode: 'explicit',
      channelInterpretation: 'discrete',
    });

    const entry = {
      source: null,
      analyser: null,
      analyserBuf: null,
      node,
      seq: 0,
      chunksLogged: 0,
      speakerName: info.speakerName,
      isLocal: info.isLocal,
      mediaStream: null,
      jitsiTrack: info.jitsiTrack || null,
      jitsiConf: info.jitsiConf || null,
      conf: info.conf || null,
      consumer: null,
      boundTrackObject: null,
      boundTrackId: '',
      requestedTrackId: '',
      boundVia: '',
      rebindCount: 0,
      lastRebindAtMs: 0,
      lastRebindReason: '',
      lastChunkAtMs: 0,
      preWorkletMaxAbs: 0,
      trackReadyState: '',
      trackMuted: false,
      lastError: '',
      createdAtMs: Date.now(),
    };
    entry.requestedTrackId = Array.from(getTrackIdSet(info))[0] || '';

    node.port.onmessage = (ev) => {
      const data = ev.data;
      if (!data) return;
      if (data.type === 'error') {
        if (data.reason === 'sample_rate_mismatch') {
          console.error(
            '[PcmTransport] Worklet reported sample_rate_mismatch for pid',
            pid,
            'actualSampleRate=',
            data.sampleRate
          );
          pcmDetachParticipant(pid);
        }
        return;
      }
      if (data.type !== 'pcm_chunk') return;
      const int16 = data.int16Pcm;
      if (!int16 || !int16.length) return;

      const nowMs = Date.now();
      const payloadMs = (data.sampleCount / 16000) * 1000;
      const chunkEndMs = nowMs;
      const chunkStartMs = nowMs - payloadMs;

      const pcmBase64 = int16ToBase64(int16);
      (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_1__.sendWsPcmChunk)(
        pid,
        entry.speakerName,
        entry.isLocal,
        entry.seq++,
        chunkStartMs,
        chunkEndMs,
        pcmBase64
      );
      entry.lastChunkAtMs = nowMs;

      if (entry.chunksLogged < 3) {
        entry.chunksLogged++;
        console.log(
          '[PcmTransport] chunk sent pid=', pid,
          'seq=', entry.seq - 1,
          'samples=', data.sampleCount,
          'payloadMs=', payloadMs.toFixed(0)
        );
      }
    };

    if (!bindEntryToStream(pid, entry, info.mediaStream, BOUND_VIA.WRAPPER)) {
      try { node.port.onmessage = null; } catch (_) { /* ignore */ }
      try { node.disconnect(); } catch (_) { /* ignore */ }
      return;
    }

    // Pump the graph. AudioWorkletNode with numberOfOutputs:0 can be
    // treated as disconnected by Chromium and never get process() ticks
    // unless something downstream drives it. Connecting to destination
    // here is a no-op signal-wise (worklet has no outputs) but keeps
    // the graph alive. See docs/plans/rippling-mixing-giraffe.md POF-5.
    try {
      node.connect(ctx.destination);
    } catch (e) {
      console.warn('[PcmTransport] node.connect(destination) failed:', e?.message || e);
    }
    active.set(pid, entry);
    ensureDiagnosticsTimer();
    console.log('[PcmTransport] Attached participant:', pid, entry.speakerName);

    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_REBIND_ENABLED) {
      setTimeout(() => {
        const current = active.get(pid);
        if (current) tryRebindToReceiverTrack(pid, current, true, REBIND_REASON.INITIAL_PROBE);
      }, 500);
    }
  } catch (e) {
    console.warn('[PcmTransport] Failed to materialize pid', pid, e);
  }
}

function teardown(pid) {
  const entry = active.get(pid);
  if (!entry) return;
  disconnectSource(entry);
  destroyConsumer(entry);
  try {
    entry.node.port.onmessage = null;
    if (audioContext && audioContext.destination) {
      try { entry.node.disconnect(audioContext.destination); } catch (_) { /* not connected */ }
    }
    entry.node.disconnect();
  } catch (_) {
    /* already disconnected */
  }
  active.delete(pid);
  console.log('[PcmTransport] Detached participant:', pid);
}

function pcmAttachParticipant(pid, speakerName, isLocal, mediaStream, options = {}) {
  if (!pid || !mediaStream) return;
  // Local mic stays on sounddevice path (D9).
  if (isLocal) return;

  const next = {
    speakerName: speakerName || pid,
    isLocal: !!isLocal,
    mediaStream,
    jitsiTrack: options?.jitsiTrack || null,
    jitsiConf: options?.jitsiConf || null,
    conf: options?.conf || null,
  };

  const existing = pending.get(pid);
  if (existing) {
    existing.speakerName = next.speakerName;
    existing.isLocal = next.isLocal;
    existing.mediaStream = next.mediaStream || existing.mediaStream;
    existing.jitsiTrack = next.jitsiTrack || existing.jitsiTrack;
    existing.jitsiConf = next.jitsiConf || existing.jitsiConf;
    existing.conf = next.conf || existing.conf;

    const entry = active.get(pid);
    if (entry) {
      entry.speakerName = existing.speakerName;
      entry.isLocal = existing.isLocal;
      entry.jitsiTrack = existing.jitsiTrack || entry.jitsiTrack;
      entry.jitsiConf = existing.jitsiConf || entry.jitsiConf;
      entry.conf = existing.conf || entry.conf;
      const ids = getTrackIdSet(existing);
      entry.requestedTrackId = Array.from(ids)[0] || entry.requestedTrackId;
      const newTrack = getAudioTrackFromStream(existing.mediaStream);
      const shouldRebindWrapper =
        existing.mediaStream !== entry.mediaStream ||
        (newTrack && entry.boundTrackId && newTrack.id !== entry.boundTrackId);
      if (shouldRebindWrapper) {
        if (bindEntryToStream(pid, entry, existing.mediaStream, BOUND_VIA.WRAPPER, newTrack)) {
          entry.rebindCount++;
          entry.lastRebindAtMs = Date.now();
          entry.lastRebindReason = REBIND_REASON.JITSI_TRACK_UPDATE;
        }
      }
      tryRebindToReceiverTrack(pid, entry, false, REBIND_REASON.JITSI_UPDATE_RECEIVER_PROBE);
    } else {
      void materialize(pid, existing);
    }
    return;
  }

  pending.set(pid, next);
  void materialize(pid, pending.get(pid));
}

// Update the cached display name for a pid whose PCM transport is already
// attached. Safe to call for unknown pids (no-op).
function pcmUpdateParticipantName(pid, newName) {
  if (!pid || !newName) return;
  const info = pending.get(pid);
  if (info && info.speakerName !== newName) {
    info.speakerName = newName;
  }
  const entry = active.get(pid);
  if (entry && entry.speakerName !== newName) {
    entry.speakerName = newName;
  }
}

function pcmDetachParticipant(pid) {
  if (!pid) return;
  pending.delete(pid);
  teardown(pid);
  if (active.size === 0 && diagnosticsTimer) {
    sendDiagnosticsSnapshot('idle snapshot');
    clearInterval(diagnosticsTimer);
    diagnosticsTimer = null;
  }
}

function pcmAttachAllFromConf(conf) {
  if (!conf) {
    console.warn('[PcmTransport] attachAll: no conf');
    return;
  }
  const localId = conf.localParticipant?.id;

  let inspected = 0;
  let skippedLocal = 0;
  let skippedNoStream = 0;
  let attached = 0;

  conf._participantsMap?.forEach((p) => {
    inspected++;
    const pid = p._id;
    if (!pid || p.local) { skippedLocal++; return; }
    const jitsiTrack = p._audioTrack$?.value?.jitsiTrack;
    const stream = getStreamFromJitsiTrack(jitsiTrack);
    if (!stream) { skippedNoStream++; return; }
    const speakerName = resolveSpeakerName(p, pid);
    pcmAttachParticipant(pid, speakerName, false, stream, {
      jitsiTrack,
      jitsiConf: conf.jitsiConf,
      conf,
    });
    attached++;
  });

  let jitsiInspected = 0;
  let jitsiSkippedNoStream = 0;
  let jitsiAttached = 0;
  const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
  for (const jp of jitsiParticipants) {
    jitsiInspected++;
    const pid = jp._id || jp.getId?.() || jp.id;
    if (!pid || pid === localId || pending.has(pid)) continue;
    const tracks = jp.getTracks?.() || [];
    const audioTrack = tracks.find((t) => t.getType?.() === 'audio');
    const stream = getStreamFromJitsiTrack(audioTrack);
    if (!stream) { jitsiSkippedNoStream++; continue; }
    const speakerName =
      jp.getDisplayName?.() || jp._displayName || jp.displayName || jp._name || pid;
    pcmAttachParticipant(pid, speakerName, false, stream, {
      jitsiTrack: audioTrack,
      jitsiConf: conf.jitsiConf,
      conf,
    });
    jitsiAttached++;
  }

  console.log(
    '[PcmTransport] attachAll summary:',
    'participantsMap inspected=', inspected,
    'skippedLocal=', skippedLocal,
    'skippedNoStream=', skippedNoStream,
    'attached=', attached,
    '| jitsiConf inspected=', jitsiInspected,
    'skippedNoStream=', jitsiSkippedNoStream,
    'attached=', jitsiAttached
  );
}

function pcmTransportDispose() {
  for (const pid of Array.from(active.keys())) {
    teardown(pid);
  }
  active.clear();
  pending.clear();
  if (diagnosticsTimer) {
    clearInterval(diagnosticsTimer);
    diagnosticsTimer = null;
  }
  if (audioContext && audioContext.state !== 'closed') {
    audioContext.close().catch(() => {});
  }
  audioContext = null;
  workletLoadPromise = null;
  lastStatsSentAtMs = 0;
  lastWebrtcStats = createEmptyWebrtcStats();
  console.log('[PcmTransport] Disposed');
}

function resolveSpeakerName(p, fallback) {
  const userInfo = p?._userInfo$?.value;
  if (userInfo) {
    const first = userInfo.firstName || '';
    const last = userInfo.lastName || '';
    if (first && last) return `${first} ${last}`;
    if (first) return first;
  }
  const info = p?._participantInfo$?.value;
  if (info) {
    const first = info.firstName || '';
    const last = info.lastName || '';
    if (first && last) return `${first} ${last}`;
    if (first) return first;
  }
  return p?.name || p?.displayName || fallback;
}

function findPeerConnections(conf, jitsiConf) {
  const roots = [
    conf,
    jitsiConf,
    typeof window !== 'undefined' ? window.$conf : null,
    typeof window !== 'undefined' ? window.APP?.conference?._room : null,
  ];
  const pcs = [];
  const seen = typeof WeakSet !== 'undefined' ? new WeakSet() : null;

  function isPcLike(obj) {
    return obj &&
      typeof obj === 'object' &&
      typeof obj.getStats === 'function' &&
      typeof obj.getReceivers === 'function' &&
      typeof obj.createOffer === 'function';
  }

  function walk(obj, depth) {
    if (!obj || typeof obj !== 'object' || depth > 4) return;
    try {
      if (seen) {
        if (seen.has(obj)) return;
        seen.add(obj);
      }
    } catch (_) {
      /* ignore */
    }
    if (isPcLike(obj)) {
      if (!pcs.includes(obj)) pcs.push(obj);
      return;
    }
    for (const key in obj) {
      if (!Object.prototype.hasOwnProperty.call(obj, key)) continue;
      try { walk(obj[key], depth + 1); } catch (_) { /* ignore */ }
    }
  }

  for (const root of roots) {
    try { walk(root, 0); } catch (_) { /* ignore */ }
  }
  return pcs;
}

function findReceiverTrackByIds(trackIds, conf, jitsiConf) {
  if (!trackIds || trackIds.size === 0) return null;
  const pcs = findPeerConnections(conf, jitsiConf);
  for (const pc of pcs) {
    let receivers = [];
    try { receivers = pc.getReceivers?.() || []; } catch (_) { receivers = []; }
    for (const receiver of receivers) {
      const track = receiver?.track;
      if (track?.kind === 'audio' && trackIds.has(String(track.id))) {
        return track;
      }
    }
  }
  return null;
}

function tryRebindToReceiverTrack(pid, entry, force = false, reason = 'receiver_probe') {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_REBIND_ENABLED || !entry) return false;
  const now = Date.now();
  const cooldownMs = _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_REBIND_COOLDOWN_MS ?? 4000;
  if (!force && entry.lastRebindAtMs && now - entry.lastRebindAtMs < cooldownMs) {
    return false;
  }

  const trackIds = getTrackIdSet(entry);
  const receiverTrack = findReceiverTrackByIds(trackIds, entry.conf, entry.jitsiConf);
  if (!receiverTrack) return false;
  if (receiverTrack.readyState === 'ended') {
    entry.lastError = 'receiver_track_ended';
    return false;
  }
  if (entry.boundTrackObject === receiverTrack && entry.boundVia === BOUND_VIA.RECEIVER) {
    return false;
  }

  const stream = new MediaStream([receiverTrack]);
  if (!bindEntryToStream(pid, entry, stream, BOUND_VIA.RECEIVER, receiverTrack)) {
    return false;
  }
  entry.rebindCount++;
  entry.lastRebindAtMs = now;
  entry.lastRebindReason = reason;
  console.log(
    '[PcmTransport] rebound pid=',
    pid,
    'track=',
    receiverTrack.id,
    'reason=',
    reason
  );
  return true;
}

function ensureDiagnosticsTimer() {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED) return;
  if (diagnosticsTimer) return;
  diagnosticsTimer = setInterval(
    () => updateDiagnostics().catch((e) => {
      console.warn('[PcmTransport] diagnostics tick failed:', e?.message || e);
    }),
    _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_STATS_INTERVAL_MS ?? 1000
  );
}

async function updateDiagnostics() {
  if (active.size === 0) return;
  if (audioContext?.state === 'suspended') {
    try { await audioContext.resume(); } catch (_) { /* ignore */ }
  }

  for (const [pid, entry] of active.entries()) {
    updateEntryEnergy(entry);
    const track = entry.boundTrackObject || getAudioTrackFromStream(entry.mediaStream);
    entry.trackReadyState = track?.readyState || entry.trackReadyState || '';
    entry.trackMuted = track?.muted === true;
    tryRebindToReceiverTrack(pid, entry, false, REBIND_REASON.DIAGNOSTICS_PROBE);
  }

  await updateWebrtcStats();

  const now = Date.now();
  const sendIntervalMs = _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_STATS_SEND_INTERVAL_MS ?? 2000;
  if (now - lastStatsSentAtMs >= sendIntervalMs) {
    lastStatsSentAtMs = now;
    (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_1__.sendWsPcmTransportStats)(pcmGetDiagnostics());
  }
}

function updateEntryEnergy(entry) {
  if (!entry?.analyser || !entry.analyserBuf) return;
  try {
    entry.analyser.getFloatTimeDomainData(entry.analyserBuf);
    let max = 0;
    for (let i = 0; i < entry.analyserBuf.length; i++) {
      const v = entry.analyserBuf[i];
      const abs = v < 0 ? -v : v;
      if (abs > max) max = abs;
    }
    entry.preWorkletMaxAbs = Math.round(max * 32767);
  } catch (_) {
    /* ignore */
  }
}

async function updateWebrtcStats() {
  // Jitsi/KTalk participants share one conf — one walk covers everyone, plus a
  // standalone fallback walk for window.$conf / window.APP roots.
  const firstEntry = active.values().next().value;
  const pcs = findPeerConnections(firstEntry?.conf, firstEntry?.jitsiConf);
  for (const pc of findPeerConnections()) {
    if (!pcs.includes(pc)) pcs.push(pc);
  }
  let bytesReceived = 0;
  let packetsReceived = 0;
  const reports = await Promise.all(
    pcs.map((pc) => (pc?.getStats ? pc.getStats(null).catch(() => null) : null))
  );
  for (const report of reports) {
    if (!report) continue;
    report.forEach((s) => {
      if (s && s.type === 'inbound-rtp' && (s.kind === 'audio' || s.mediaType === 'audio')) {
        if (typeof s.bytesReceived === 'number') bytesReceived += s.bytesReceived;
        if (typeof s.packetsReceived === 'number') packetsReceived += s.packetsReceived;
      }
    });
  }
  const prevBytes = lastWebrtcStats.inbound_audio_bytes_received || 0;
  const prevPackets = lastWebrtcStats.inbound_audio_packets_received || 0;
  lastWebrtcStats = {
    pc_count: pcs.length,
    inbound_audio_bytes_received: bytesReceived,
    inbound_audio_bytes_delta: Math.max(0, bytesReceived - prevBytes),
    inbound_audio_packets_received: packetsReceived,
    inbound_audio_packets_delta: Math.max(0, packetsReceived - prevPackets),
  };
}

function pcmGetDiagnostics() {
  const now = Date.now();
  let totalRebinds = 0;
  let silentBindings = 0;
  const perPid = {};

  for (const [pid, entry] of active.entries()) {
    totalRebinds += entry.rebindCount;
    const lastChunkAgeMs = entry.lastChunkAtMs ? now - entry.lastChunkAtMs : null;
    const isSilent = entry.preWorkletMaxAbs < 10 && (lastChunkAgeMs === null || lastChunkAgeMs > 4000);
    if (isSilent) silentBindings++;
    perPid[pid] = {
      speaker_name: entry.speakerName,
      seq: entry.seq,
      bound_via: entry.boundVia,
      bound_track_id: entry.boundTrackId,
      requested_track_id: entry.requestedTrackId,
      rebind_count: entry.rebindCount,
      last_rebind_reason: entry.lastRebindReason,
      pre_worklet_max_abs: entry.preWorkletMaxAbs,
      last_chunk_age_ms: lastChunkAgeMs,
      consumer_state: entry.consumer?.state || 'none',
      track_ready_state: entry.trackReadyState,
      track_muted: entry.trackMuted === true,
      last_error: entry.lastError,
    };
  }

  return {
    enabled: true,
    rebind_enabled: _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_REBIND_ENABLED !== false,
    diagnostics_enabled: _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED !== false,
    audio_context_state: audioContext?.state || null,
    active_count: active.size,
    pending_count: pending.size,
    pc_count: lastWebrtcStats.pc_count,
    inbound_audio_bytes_delta: lastWebrtcStats.inbound_audio_bytes_delta,
    inbound_audio_packets_delta: lastWebrtcStats.inbound_audio_packets_delta,
    total_rebinds: totalRebinds,
    silent_bindings: silentBindings,
    per_pid: perPid,
  };
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
/* harmony import */ var _pcm_transport_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ../pcm_transport.js */ "./src/pcm_transport.js");
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
    // Pids for which we've already subscribed to user/participant info
    // observables for display-name refresh (Finding 3 / speaker name freeze).
    this._pcmNameSubscribedPids = new Set();
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

    const isLocal = conf.localParticipant?.id === participantId;

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
    if (isLocal) {
      const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
      const localAudio = localTracks.find(t => t.getType?.() === 'audio');
      if (localAudio) {
        const muted = localAudio.isMuted?.();
        if (muted !== undefined) return muted === true;
      }
      // Some KTalk builds expose a top-level helper on jitsiConf instead
      // of (or in addition to) per-track isMuted. Try it before falling
      // through to RxJS / default.
      if (typeof conf.jitsiConf?.isLocalAudioMuted === 'function') {
        try {
          return conf.jitsiConf.isLocalAudioMuted() === true;
        } catch (_) { /* fall through */ }
      }
      const lp = conf.localParticipant;
      if (lp?._audioMuted$?.value !== undefined) {
        return lp._audioMuted$.value === true;
      }
      if (lp?._audioMuted !== undefined) {
        return lp._audioMuted === true;
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

    // Fallback splits by isLocal:
    //  - remote: assume muted. Speaker-attribution code uses this signal to
    //    decide who to attribute audio to; defaulting to unmuted would let
    //    a participant whose mute state is unknown grab attribution.
    //  - local: assume UNMUTED. is_local_mic_muted() on the bridge gates
    //    mic chunk ingestion in recog/live.py; if it answers True before
    //    the local audio track has bound (initial WS-open / first poll),
    //    the user's voice is silently dropped from the transcript until
    //    they manually toggle mute and back. The conservative-true default
    //    here was the reason "после mute-toggle транскрипты появились".
    //    trackAdded/trackMuteChanged still corrects this within ~1 s if
    //    the user actually IS muted.
    return !isLocal;
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
      if (!p.local) {
        this._subscribePcmNameRefresh(p);
      }
    });

    // Attach audio analysers for existing participants
    (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.attachAllFromConf)(conf);
    (0,_pcm_transport_js__WEBPACK_IMPORTED_MODULE_1__.pcmAttachAllFromConf)(conf);

    // Also subscribe to Jitsi events
    const jitsiConf = conf.jitsiConf;
    if (jitsiConf) {
      this._onJitsi(jitsiConf, 'conference.dominantSpeaker', () => callback());

      this._onJitsi(jitsiConf, 'conference.trackAdded', (track) => {
        if (track.getType?.() === 'audio') {
          callback();
          const pid = track.getParticipantId?.();
          const stream = track.stream || track.getOriginalStream?.();
          if (pid && stream) {
            (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.attachParticipant)(pid, stream);
            const isLocal = this._isLocalParticipant(pid);
            if (!isLocal) {
              const speakerName = this.getParticipantName(pid);
              (0,_pcm_transport_js__WEBPACK_IMPORTED_MODULE_1__.pcmAttachParticipant)(pid, speakerName, false, stream, {
                jitsiTrack: track,
                jitsiConf,
                conf,
              });
              // User info often populates after trackAdded fires; wire a
              // name refresher so the first chunks' fallback pid is
              // replaced once real metadata lands.
              const p = conf._participantsMap?.get(pid);
              if (p) this._subscribePcmNameRefresh(p);
            }
          }
        }
      });

      this._onJitsi(jitsiConf, 'conference.trackRemoved', (track) => {
        if (track.getType?.() === 'audio') {
          callback();
          const pid = track.getParticipantId?.();
          if (pid) {
            (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.detachParticipant)(pid);
            (0,_pcm_transport_js__WEBPACK_IMPORTED_MODULE_1__.pcmDetachParticipant)(pid);
          }
        }
      });

      this._onJitsi(jitsiConf, 'conference.trackMuteChanged', (track) => {
        if (track.getType?.() === 'audio') callback();
      });

      this._onJitsi(jitsiConf, 'conference.userJoined', (id) => {
        setTimeout(() => {
          // Re-subscribe for late joiners: their participant object is
          // populated on userJoined, but _userInfo$ / _participantInfo$
          // typically emit a frame or two later.
          const p = id && conf._participantsMap?.get(id);
          if (p && !p.local) this._subscribePcmNameRefresh(p);
          callback();
        }, 1000);
      });

      this._onJitsi(jitsiConf, 'conference.userLeft', (id) => {
        (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.detachParticipant)(id);
        (0,_pcm_transport_js__WEBPACK_IMPORTED_MODULE_1__.pcmDetachParticipant)(id);
        this._pcmNameSubscribedPids.delete(id);
        setTimeout(callback, 500);
      });
    }
  }

  /**
   * Subscribe to _userInfo$ / _participantInfo$ so the PCM transport's
   * cached display name follows metadata updates instead of freezing on
   * the first value (typically the pid fallback when userJoined lands
   * before userInfo). Idempotent per pid.
   */
  _subscribePcmNameRefresh(p) {
    const pid = p?._id;
    if (!pid) return;
    if (this._pcmNameSubscribedPids.has(pid)) return;
    const refresh = () => {
      const newName = this.getParticipantName(pid);
      if (newName) (0,_pcm_transport_js__WEBPACK_IMPORTED_MODULE_1__.pcmUpdateParticipantName)(pid, newName);
    };
    let anySubscribed = false;
    if (p._userInfo$?.subscribe) {
      this.subscriptions.push(p._userInfo$.subscribe(refresh));
      anySubscribed = true;
    }
    if (p._participantInfo$?.subscribe) {
      this.subscriptions.push(p._participantInfo$.subscribe(refresh));
      anySubscribed = true;
    }
    if (anySubscribed) {
      this._pcmNameSubscribedPids.add(pid);
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
    this._pcmNameSubscribedPids.clear();
    (0,_pcm_transport_js__WEBPACK_IMPORTED_MODULE_1__.pcmTransportDispose)();
    (0,_audio_analyzer_js__WEBPACK_IMPORTED_MODULE_0__.dispose)();
  }

  _isLocalParticipant(pid) {
    const conf = this.getConference();
    if (!conf || !pid) return false;
    if (conf.localParticipant?.id === pid) return true;
    const p = conf._participantsMap?.get(pid);
    return !!p?.local;
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
  AUDIO_FILTER_ENABLED: true,
  PCM_REBIND_ENABLED: true,
  PCM_WEBRTC_DIAGNOSTICS_ENABLED: true,
  PCM_STATS_INTERVAL_MS: 1000,
  PCM_STATS_SEND_INTERVAL_MS: 2000,
  PCM_REBIND_COOLDOWN_MS: 4000
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
  currentMeetingId: null,
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
  privacyMode: localStorage.getItem('ktalk-privacy-mode') === 'true',
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
  },

  copilotMessages: [],
  copilotPending: new Map(),
  copilotProactiveMutedUntil: 0,
  copilotChannelStatus: 'unknown',
  copilotLastProactiveAt: 0,
  copilotContentElement: null,
  isCopilotActive: false,
  copilotUnread: false,
  lastAiView: localStorage.getItem('ktalk-ai-last-view') || 'transcript'
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
  if (line.segment_id && line.segment_id > 0) {
    lineEl.dataset.segmentId = line.segment_id;
  }
  if (line.timestamp) {
    lineEl.dataset.timestamp = line.timestamp;
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
        <option value="codex">Codex (GPT-5.5)</option>
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
/* harmony export */   addAiTab: () => (/* binding */ addAiTab),
/* harmony export */   addTranscriptLinePlatform: () => (/* binding */ addTranscriptLinePlatform),
/* harmony export */   appendCopilotSystemMessage: () => (/* binding */ appendCopilotSystemMessage),
/* harmony export */   buildSpeakerAliasMap: () => (/* binding */ buildSpeakerAliasMap),
/* harmony export */   createContextContent: () => (/* binding */ createContextContent),
/* harmony export */   createCopilotContent: () => (/* binding */ createCopilotContent),
/* harmony export */   createTranscriptContent: () => (/* binding */ createTranscriptContent),
/* harmony export */   handleAnchorResponse: () => (/* binding */ handleAnchorResponse),
/* harmony export */   handleCopilotBridgeDisconnect: () => (/* binding */ handleCopilotBridgeDisconnect),
/* harmony export */   handleCopilotReply: () => (/* binding */ handleCopilotReply),
/* harmony export */   handleCopilotStatus: () => (/* binding */ handleCopilotStatus),
/* harmony export */   handleNewMeeting: () => (/* binding */ handleNewMeeting),
/* harmony export */   handleSummaryResponse: () => (/* binding */ handleSummaryResponse),
/* harmony export */   handleTranscript: () => (/* binding */ handleTranscript),
/* harmony export */   handleTranscriptHistory: () => (/* binding */ handleTranscriptHistory),
/* harmony export */   handleTranscriptRefine: () => (/* binding */ handleTranscriptRefine),
/* harmony export */   initPlatformUI: () => (/* binding */ initPlatformUI),
/* harmony export */   injectStyles: () => (/* binding */ injectStyles),
/* harmony export */   isCopilotProactiveMuted: () => (/* binding */ isCopilotProactiveMuted),
/* harmony export */   renderCopilotMessage: () => (/* binding */ renderCopilotMessage),
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
        transition: color 0.15s;
      }
      .ktalk-ai-tab.link:hover {
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-ai-tab.link._active {
        color: rgba(255, 255, 255, 0.85);
      }
      .ktalk-ai-tab .tab-content {
        display: flex;
        align-items: center;
        height: 32px;
        line-height: 16px;
        gap: 4px;
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

      /* AI panel: sub-navigation + active view (Transcript | Context | Copilot) */
      .ktalk-ai-panel {
        display: none;
        flex-direction: column;
        height: 100%;
        background: rgb(52, 52, 52);
      }
      .ktalk-ai-panel.active {
        display: flex;
      }
      .ktalk-ai-subnav {
        display: flex;
        gap: 4px;
        padding: 8px 12px;
        background: rgb(60, 60, 60);
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
      .ktalk-ai-subtab-dot {
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: #4ade80;
      }
      .ktalk-ai-subtab-dot[hidden] {
        display: none;
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
        align-items: center;
        justify-content: flex-start;
        padding: 8px 12px;
        background: rgb(56, 56, 56);
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
        left: 0;
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
        background: rgb(52, 52, 52);
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
        background: rgb(60, 60, 60);
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
      .ktalk-copilot-actions {
        margin-left: auto;
        display: flex;
        align-items: center;
        gap: 8px;
      }
      .ktalk-copilot-mute-btn {
        padding: 0 8px;
        background: rgba(255, 255, 255, 0.1);
        border: 1px solid rgba(255, 255, 255, 0.2);
        border-radius: 4px;
        color: rgba(255, 255, 255, 0.85);
        font-size: 11px;
        cursor: pointer;
        height: 22px;
        line-height: 22px;
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
        background: rgb(56, 56, 56);
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

function addAiTab() {
  if (document.querySelector('.ktalk-ai-tab')) {
    return true;
  }

  const tabsContainer = document.querySelector('.tl-tabset__content');
  if (!tabsContainer) {
    return false;
  }

  console.log('[KTalk Transcript] Adding AI tab to tabset');

  const tab = document.createElement('div');
  tab.className = 'tl-tab _switchable ktalk-ai-tab-wrapper';
  tab.innerHTML = `
      <button type="button" class="ktalk-ai-tab link">
        <span class="tab-content">
          <span class="title">Recog</span>
          <span class="ktalk-ai-unread" hidden></span>
        </span>
      </button>
    `;

  tabsContainer.appendChild(tab);

  tab.querySelector('button').addEventListener('click', () => {
    activateAiTab();
  });

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptTabAdded = true;
  return true;
}

function captureOriginalChatContent(contentArea) {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent && document.body.contains(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent)) return;
  const first = contentArea.firstElementChild;
  if (first && !first.classList.contains('ktalk-ai-panel')) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent = first;
  }
}

function activateSubView(view) {
  if (view === 'context') {
    activateContextTab();
  } else if (view === 'copilot') {
    activateCopilotTab();
  } else {
    activateTranscriptTab();
  }
}

function ensureAiPanelBody() {
  const contentArea = document.querySelector('.slide-menu-wrapper .content');
  if (!contentArea) return null;

  let panel = contentArea.querySelector('.ktalk-ai-panel');
  if (!panel) {
    panel = document.createElement('div');
    panel.className = 'ktalk-ai-panel';
    panel.innerHTML = `
        <div class="ktalk-ai-subnav">
          <button type="button" class="ktalk-ai-subtab" data-view="transcript">Transcript</button>
          <button type="button" class="ktalk-ai-subtab" data-view="context">Context</button>
          <button type="button" class="ktalk-ai-subtab" data-view="copilot">Copilot<span class="ktalk-ai-subtab-dot" hidden></span></button>
        </div>
        <div class="ktalk-ai-panel-body"></div>
      `;
    panel.querySelectorAll('.ktalk-ai-subtab').forEach((btn) => {
      btn.addEventListener('click', () => activateSubView(btn.dataset.view));
    });
    contentArea.appendChild(panel);
  }
  return panel.querySelector('.ktalk-ai-panel-body');
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

  captureOriginalChatContent(contentArea);
  const panelBody = ensureAiPanelBody();
  if (!panelBody) return null;

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement = document.createElement('div');
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.className = 'ktalk-transcript-content';
  loadBioReadSettings();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.innerHTML = `
      <div class="ktalk-transcript-speaker-bar">
        <div class="ktalk-speaker-dot"></div>
        <span class="ktalk-speaker-name">No one speaking</span>
        <div class="ktalk-speaker-bar-nav">
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-top" title="Scroll to top">↑</button>
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-bottom" title="Scroll to bottom">↓</button>
        </div>
      </div>
      <div class="ktalk-transcript-toolbar">
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
  const privacyBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-transcript-privacy-btn');
  privacyBtn.classList.toggle('active', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.privacyMode);
  privacyBtn.addEventListener('click', () => {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.privacyMode = !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.privacyMode;
    localStorage.setItem('ktalk-privacy-mode', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.privacyMode);
    privacyBtn.classList.toggle('active', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.privacyMode);
  });
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

  panelBody.appendChild(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement);

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

  captureOriginalChatContent(contentArea);
  const panelBody = ensureAiPanelBody();
  if (!panelBody) return null;

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

  panelBody.appendChild(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement);
  console.log('[KTalk Transcript] Context element created');
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement;
}

const COPILOT_PROACTIVE_MUTE_MS = 10 * 60 * 1000;
const COPILOT_TRANSCRIPT_WINDOW_SIZE = 20;

function nowDisplay() {
  const d = new Date();
  return d.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function buildCopilotTranscriptWindow() {
  if (!Array.isArray(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines)) return [];
  const tail = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice(-COPILOT_TRANSCRIPT_WINDOW_SIZE);
  return tail.map((line) => ({
    speaker: line.speaker || '',
    text: line.text || '',
    timestamp: line.time || line.timestamp || '',
    segment_id: line.segment_id || 0
  }));
}

function createCopilotContent() {
  if (document.querySelector('.ktalk-copilot-content')) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement = document.querySelector('.ktalk-copilot-content');
    return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement;
  }

  const contentArea = document.querySelector('.slide-menu-wrapper .content');
  if (!contentArea) {
    console.log('[KTalk Transcript] Content area not found for copilot');
    return null;
  }

  captureOriginalChatContent(contentArea);
  const panelBody = ensureAiPanelBody();
  if (!panelBody) return null;

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement = document.createElement('div');
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement.className = 'ktalk-copilot-content';
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement.innerHTML = `
      <div class="ktalk-copilot-header">
        <span class="ktalk-copilot-title">Copilot</span>
        <span class="ktalk-copilot-status">…</span>
        <div class="ktalk-copilot-actions">
          <button type="button" class="ktalk-copilot-mute-btn" title="Mute proactive messages for 10 minutes">Mute proactive 10m</button>
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
    `;

  const muteBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement.querySelector('.ktalk-copilot-mute-btn');
  muteBtn.addEventListener('click', () => toggleCopilotProactiveMute());
  refreshCopilotMuteButton();

  const input = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement.querySelector('.ktalk-copilot-input');
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      submitCopilotInput();
    }
  });

  const sendBtn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement.querySelector('.ktalk-copilot-send-btn');
  sendBtn.addEventListener('click', () => submitCopilotInput());

  panelBody.appendChild(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement);
  updateCopilotStatusIndicator();
  console.log('[KTalk Transcript] Copilot element created');
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement;
}

function ensureCopilotContent() {
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement || !document.body.contains(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement)) {
    createCopilotContent();
  }
}

function getCopilotMessagesEl() {
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement?.querySelector('.ktalk-copilot-messages') || null;
}

function getCopilotInputEl() {
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement?.querySelector('.ktalk-copilot-input') || null;
}

function getCopilotSendBtn() {
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement?.querySelector('.ktalk-copilot-send-btn') || null;
}

function refreshCopilotMuteButton() {
  const btn = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement?.querySelector('.ktalk-copilot-mute-btn');
  if (!btn) return;
  const muted = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotProactiveMutedUntil > Date.now();
  btn.classList.toggle('active', muted);
  btn.textContent = muted ? 'Unmute proactive' : 'Mute proactive 10m';
}

function toggleCopilotProactiveMute() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotProactiveMutedUntil > Date.now()) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotProactiveMutedUntil = 0;
  } else {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotProactiveMutedUntil = Date.now() + COPILOT_PROACTIVE_MUTE_MS;
  }
  refreshCopilotMuteButton();
}

function isCopilotProactiveMuted() {
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotProactiveMutedUntil > Date.now();
}

function updateCopilotStatusIndicator() {
  const el = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement?.querySelector('.ktalk-copilot-status');
  if (!el) return;
  const status = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotChannelStatus || 'unknown';
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

function escapeCopilotText(value) {
  return (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(value == null ? '' : String(value));
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
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotMessages.length === 0 && !messagesEl.querySelector('.ktalk-copilot-empty')) {
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

function renderCopilotMessage(msg) {
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
  if (msg.kind === 'assistant' || msg.kind === 'proactive') {
    actionsHtml = `
        <div class="copilot-msg-actions">
          <button type="button" class="copilot-msg-action-btn copilot-msg-copy-btn" title="Copy">Copy</button>
          <button type="button" class="copilot-msg-action-btn copilot-msg-dismiss-btn" title="Dismiss">Dismiss</button>
        </div>
      `;
  }

  wrapper.innerHTML = headerHtml + textHtml + actionsHtml;

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
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotMessages.push(msg);
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.isCopilotActive && (msg.kind === 'proactive' || msg.kind === 'assistant')) {
    setCopilotUnread(true);
  }
}

function findCopilotMessageElement(id) {
  const messagesEl = getCopilotMessagesEl();
  if (!messagesEl || !id) return null;
  return messagesEl.querySelector(`[data-copilot-id="${id}"]`);
}

function dismissCopilotMessage(id) {
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotMessages = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotMessages.filter((m) => m.id !== id);
  const el = findCopilotMessageElement(id);
  if (el) el.remove();
  showCopilotEmptyIfNoneLeft();
}

function updateCopilotMessageDom(id, updates) {
  const el = findCopilotMessageElement(id);
  if (!el) return;
  if (updates.text != null) {
    const textEl = el.querySelector('.copilot-msg-text');
    if (textEl) {
      textEl.textContent = updates.text;
      textEl.classList.toggle('pending', updates.status === 'working');
    }
  }
  if (updates.time != null) {
    const timeEl = el.querySelector('.copilot-msg-time');
    if (timeEl) timeEl.textContent = updates.time;
  }
  if (updates.kind) {
    el.classList.remove(
      'copilot-msg-user',
      'copilot-msg-assistant',
      'copilot-msg-proactive',
      'copilot-msg-status',
      'copilot-msg-error'
    );
    el.classList.add(`copilot-msg-${updates.kind}`);
    const roleEl = el.querySelector('.copilot-msg-role');
    if (roleEl) roleEl.textContent = getCopilotRoleLabel(updates.kind);
    // Drop the copy/dismiss row when the kind flips to one that does not
    // own actions (status/error/user). The block below re-adds the row
    // only for assistant/proactive, so without this cleanup an assistant
    // bubble downgraded to error keeps a Copy button that copies stale
    // text.
    if (updates.kind !== 'assistant' && updates.kind !== 'proactive') {
      const existingActions = el.querySelector('.copilot-msg-actions');
      if (existingActions) existingActions.remove();
    }
  }
  if (updates.kind === 'assistant' || updates.kind === 'proactive') {
    if (!el.querySelector('.copilot-msg-actions')) {
      const actions = document.createElement('div');
      actions.className = 'copilot-msg-actions';
      actions.innerHTML = `
          <button type="button" class="copilot-msg-action-btn copilot-msg-copy-btn" title="Copy">Copy</button>
          <button type="button" class="copilot-msg-action-btn copilot-msg-dismiss-btn" title="Dismiss">Dismiss</button>
        `;
      el.appendChild(actions);
      const copyBtn = actions.querySelector('.copilot-msg-copy-btn');
      copyBtn.addEventListener('click', () => {
        const msg = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotMessages.find((m) => m.id === id);
        const text = msg ? (msg.text || '') : '';
        if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
          navigator.clipboard.writeText(text).then(() => {
            copyBtn.classList.add('copied');
            copyBtn.textContent = 'Copied';
            setTimeout(() => {
              copyBtn.classList.remove('copied');
              copyBtn.textContent = 'Copy';
            }, 1500);
          }).catch(() => {});
        }
      });
      const dismissBtn = actions.querySelector('.copilot-msg-dismiss-btn');
      dismissBtn.addEventListener('click', () => dismissCopilotMessage(id));
    }
  }
}

function updateCopilotMessage(id, updates) {
  const msg = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotMessages.find((m) => m.id === id);
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
  const ws = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection;
  if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected || !ws || ws.readyState !== WebSocket.OPEN) {
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
    requestId = (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_3__.sendCopilotChatMessage)({
      text,
      meetingId: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId,
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
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.set(requestId, { userId, assistantId, createdAt: Date.now() });

  input.value = '';
  setCopilotInputBusy(true);
}

function findPendingAssistantId(requestId) {
  if (!requestId) return null;
  const pending = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.get(requestId);
  return pending ? pending.assistantId : null;
}

function handleCopilotReply(data) {
  ensureCopilotContent();
  const requestId = data && data.request_id;
  const replyText = (data && (data.text || data.message || '')) || '';
  const kind = data && data.kind === 'proactive' ? 'proactive' : 'assistant';
  const time = nowDisplay();

  if (kind === 'proactive') {
    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotProactiveMutedUntil > Date.now()) {
      console.log('[KTalk Copilot] Proactive reply muted, dropping.');
      return;
    }
    if (Date.now() - (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotLastProactiveAt || 0) < 60000) {
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
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotLastProactiveAt = Date.now();
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
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.delete(requestId);
    setCopilotInputBusy(false);
    if (!_state_js__WEBPACK_IMPORTED_MODULE_0__.state.isCopilotActive) setCopilotUnread(true);
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

function handleCopilotStatus(data) {
  ensureCopilotContent();
  const requestId = data && data.request_id;
  const status = (data && data.status) || 'unknown';

  if (status === 'channel_connected') {
    // Channel-level recovery signal — bridge emits this on the 0→1 channel
    // client transition (or as a primer when an extension late-joins a
    // bridge whose channel was already up). Without it, once the indicator
    // flipped to `channel_disconnected` it would stay stuck even after the
    // channel came back online.
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotChannelStatus = 'connected';
    updateCopilotStatusIndicator();
    return;
  }

  if (status === 'channel_disconnected') {
    // Channel-level event — surface in the connectivity indicator. Per-
    // request statuses (queued/working/done/error) below describe a single
    // chat turn and must not overwrite the indicator: otherwise the label
    // would read "done" or "working…" long after the per-request bubble
    // already settled, masking actual channel health.
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotChannelStatus = 'channel_disconnected';
    updateCopilotStatusIndicator();
    const pendingId = findPendingAssistantId(requestId);
    if (pendingId) {
      updateCopilotMessage(pendingId, {
        kind: 'error',
        text: 'Channel offline: Claude не подключён.',
        status: 'error',
        time: nowDisplay()
      });
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.delete(requestId);
      setCopilotInputBusy(false);
    } else if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.size > 0) {
      // Channel-level signal (no specific request_id match) with pending
      // requests in flight — the bridge fires this when the last channel
      // client drops mid-flight. Flip every placeholder to error and re-
      // enable input rather than appending a single bubble that says
      // nothing about the stranded turns.
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.forEach((pending) => {
        if (pending && pending.assistantId) {
          updateCopilotMessage(pending.assistantId, {
            kind: 'error',
            text: 'Channel offline: Claude не подключён.',
            status: 'error',
            time: nowDisplay()
          });
        }
      });
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.clear();
      setCopilotInputBusy(false);
    } else {
      appendCopilotMessage({
        id: nextCopilotId(),
        kind: 'error',
        text: 'Channel offline: Claude не подключён.',
        time: nowDisplay()
      });
    }
    return;
  }

  // Terminal `error` from the channel server (e.g. Claude failed) — without
  // a corresponding `copilot_reply` the input would stay disabled forever
  // and the placeholder would still claim "working". Symmetrize with
  // channel_disconnected: flip the placeholder to an error bubble, drop the
  // pending entry, and re-enable input so the user can retry.
  if (status === 'error') {
    const pendingId = findPendingAssistantId(requestId);
    const message = (data && data.message) || 'Claude не смог ответить.';
    if (pendingId) {
      updateCopilotMessage(pendingId, {
        kind: 'error',
        text: message,
        status: 'error',
        time: nowDisplay()
      });
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.delete(requestId);
      setCopilotInputBusy(false);
    } else {
      appendCopilotMessage({
        id: nextCopilotId(),
        kind: 'error',
        text: message,
        time: nowDisplay()
      });
    }
    return;
  }

  // Terminal `done` — the bridge already pops the pending bookkeeping
  // (ktalk_bridge.py:2349). If a matching `copilot_reply` arrived first,
  // `findPendingAssistantId` returns null and there is nothing to do.
  // If `done` arrived without a preceding reply (Claude called
  // `set_copilot_status('done')` standalone), the placeholder would stay
  // on "Жду ответа Claude…" and `setCopilotInputBusy(false)` would never
  // run, stranding the textarea disabled until reload.
  if (status === 'done') {
    const pendingId = findPendingAssistantId(requestId);
    if (pendingId) {
      const message = (data && data.message) || 'Запрос завершён без ответа.';
      updateCopilotMessage(pendingId, {
        kind: 'status',
        text: message,
        status: 'done',
        time: nowDisplay()
      });
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.delete(requestId);
      setCopilotInputBusy(false);
    }
    return;
  }

  const pendingId = findPendingAssistantId(requestId);
  if (pendingId) {
    updateCopilotMessage(pendingId, {
      status,
      text: status === 'working' ? 'Claude думает…' : `Status: ${status}`,
      time: nowDisplay()
    });
  }
}

function appendCopilotSystemMessage(text) {
  ensureCopilotContent();
  appendCopilotMessage({
    id: nextCopilotId(),
    kind: 'status',
    text: text || '',
    time: nowDisplay()
  });
}

function handleCopilotBridgeDisconnect() {
  // Bridge WS dropped — flip every in-flight chat placeholder to error and
  // re-enable input. Without this, `submitCopilotInput` would leave the
  // textarea disabled forever (only `copilot_reply` / `copilot_status` re-
  // enable it, and neither will arrive from a closed socket).
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.size > 0) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.forEach((pending) => {
      if (pending && pending.assistantId) {
        updateCopilotMessage(pending.assistantId, {
          kind: 'error',
          text: 'Bridge offline: соединение прервано.',
          status: 'error',
          time: nowDisplay()
        });
      }
    });
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotPending.clear();
  }
  setCopilotInputBusy(false);
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotChannelStatus = 'channel_disconnected';
  updateCopilotStatusIndicator();
}

function setActiveView(view) {
  const aiTabBtn = document.querySelector('.ktalk-ai-tab');
  const aiTab = aiTabBtn?.closest('.tl-tab');

  const isTranscript = view === 'transcript';
  const isContext = view === 'context';
  const isCopilot = view === 'copilot';
  const isAnyActive = isTranscript || isContext || isCopilot;

  if (aiTab) aiTab.classList.toggle('_active', isAnyActive);
  if (aiTabBtn) aiTabBtn.classList.toggle('_active', isAnyActive);

  const panel = document.querySelector('.ktalk-ai-panel');
  if (panel) {
    panel.classList.toggle('active', isAnyActive);
    panel.querySelectorAll('.ktalk-ai-subtab').forEach((btn) => {
      btn.classList.toggle('active', btn.dataset.view === view);
    });
  }

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent.style.display = isAnyActive ? 'none' : '';
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.classList.toggle('active', isTranscript);
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.contextContentElement.classList.toggle('active', isContext);
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotContentElement.classList.toggle('active', isCopilot);
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.isTranscriptActive = isTranscript;
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.isContextActive = isContext;
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.isCopilotActive = isCopilot;

  if (isAnyActive) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastAiView = view;
    try {
      localStorage.setItem('ktalk-ai-last-view', view);
    } catch (err) {
      void err;
    }
  }
  if (isCopilot) setCopilotUnread(false);
}

function setCopilotUnread(unread) {
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotUnread = unread;
  const dot = document.querySelector('.ktalk-ai-tab .ktalk-ai-unread');
  if (dot) dot.hidden = !unread;
  const subDot = document.querySelector('.ktalk-ai-subtab[data-view="copilot"] .ktalk-ai-subtab-dot');
  if (subDot) subDot.hidden = !unread;
}

function activateAiTab() {
  activateSubView(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.lastAiView || 'transcript');
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

function activateCopilotTab() {
  ensureCopilotContent();
  setActiveView('copilot');
  const input = getCopilotInputEl();
  if (input) input.focus();
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

    if (!clickedTab.querySelector('.ktalk-ai-tab')) {
      deactivateCustomTabs();
    }
  });
}

function updateWsIndicator(connected) {
  document.querySelectorAll('.ktalk-ai-tab-wrapper .ws-dot')
    .forEach((dot) => {
      dot.classList.toggle('connected', connected);
    });
}

function handleTranscriptHistory(data) {
  console.log('[KTalk Transcript] Received history:', data.segments?.length || 0, 'segments');

  if (data && data.meeting_id) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId = data.meeting_id;
  }

  if (!data.segments || data.segments.length === 0) {
    return;
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines = [];

  for (const seg of data.segments) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.push({
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

  // Re-render using the appropriate platform renderer
  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
    (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__.clearVas3kTranscript)();
    for (const line of _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines) {
      (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__.addVas3kTranscriptLine)(line);
    }
  } else {
    updateTranscriptUI();
  }

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice() }
  }));
}

function handleNewMeeting(data) {
  console.log('[KTalk Transcript] New meeting:', data.meeting_id);

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId = data && data.meeting_id ? data.meeting_id : null;
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines = [];
  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
    (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_5__.clearVas3kTranscript)();
  } else {
    updateTranscriptUI();
  }

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: [] }
  }));
}

function handleTranscript(data) {
  // Drop segments produced from a sibling tab's audio when two KTalk
  // tabs are open at the same time. Mirrors the cross-meeting guard in
  // `handleTranscriptRefine`. Older bridge builds that don't tag the
  // frame still pass through (missing field == no filter).
  if (data && data.meeting_id && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId
      && data.meeting_id !== _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId) {
    return;
  }

  const { speaker, text, timestamp, is_continuation, is_local, uncertain, uncertain_reason, correction, segment_id, source_id } = data;

  const sid = segment_id || 0;
  const sourceId = typeof source_id === 'string' ? source_id : '';
  const newLine = {
    speaker,
    text,
    timestamp,
    segment_id: sid,
    // Stable per-source identifier (e.g. per-pid PCM stream). Used by the
    // continuation-merge predicate below so chunks from different sources
    // are not glued into a single bubble even if `is_continuation=true`.
    source_id: sourceId,
    // Per-child segment tracking so tier-2 refine can target continuation
    // chunks merged into this line. Backend issues a fresh segment_id for
    // every 3-s PCM chunk including continuations; without this, refines
    // for any chunk after the first in a merged bubble are silently dropped.
    segment_ids: sid > 0 ? [sid] : [],
    segment_texts: sid > 0 ? { [sid]: text } : {},
    is_local: is_local || false,
    uncertain: uncertain || false,
    uncertain_reason: uncertain_reason || '',
    time: new Date().toLocaleTimeString('ru-RU', {
      hour: '2-digit', minute: '2-digit', second: '2-digit'
    }),
    createdAt: Date.now()
  };

  if (correction) {
    // Retroactive correction: find and update existing Unknown line by timestamp+text
    const idx = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.findIndex(
      line => line.speaker === 'Unknown' && line.text.startsWith(text) && line.timestamp === timestamp
    );
    if (idx !== -1) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines[idx].speaker = speaker;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines[idx].uncertain = uncertain || false;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines[idx].uncertain_reason = uncertain_reason || '';
    }
    // Re-render UI for correction (don't fall through to append which would duplicate)
    if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
      // Find the DOM element with Unknown speaker matching this text and update in place
      const content = document.querySelector('.jitsi-transcript-content');
      if (content) {
        for (const lineEl of content.querySelectorAll('.jitsi-transcript-line')) {
          const speakerEl = lineEl.querySelector('.jitsi-transcript-speaker');
          const textEl = lineEl.querySelector('.jitsi-transcript-text');
          const rawText = textEl ? textEl.textContent.replace(/ \(\?\)$/, '').replace(/ \(🔇\?\)$/, '').trim() : '';
          if (speakerEl && speakerEl.textContent === 'Unknown' &&
              textEl && rawText.startsWith(text) &&
              (!timestamp || lineEl.dataset.timestamp === timestamp)) {
            speakerEl.textContent = speaker;
            // Remove uncertainty markers from corrected line
            textEl.classList.remove('jitsi-transcript-uncertain');
            textEl.textContent = rawText;
            break;
          }
        }
      }
    } else {
      updateTranscriptUI();
    }
    window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
      detail: { transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice() }
    }));
    return;
  }

  const prev = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length > 0
    ? _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines[_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length - 1]
    : null;
  const sameSource = sourceId && prev && prev.source_id === sourceId;
  const legacySameSpeaker = prev && !sourceId && !prev.source_id && prev.speaker === speaker;
  if (is_continuation && prev && (sameSource || legacySameSpeaker)) {
    // source_id prevents continuation chunks from contaminating another
    // source's segment_ids/segment_texts; speaker fallback is only for
    // legacy frames without source_id.
    prev.text += ' ' + text;
    if (sid > 0) {
      // Backfill tracking for legacy lines pushed before this change so the
      // refine handler can find them by either primary or child segment_id.
      if (!prev.segment_ids) {
        prev.segment_ids = prev.segment_id > 0 ? [prev.segment_id] : [];
      }
      if (!prev.segment_texts) {
        prev.segment_texts = prev.segment_id > 0
          ? { [prev.segment_id]: prev.text.slice(0, -(text.length + 1)) }
          : {};
      }
      prev.segment_ids.push(sid);
      prev.segment_texts[sid] = text;
    }
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
function handleTranscriptRefine(data) {
  const updates = Array.isArray(data && data.updates) ? data.updates : [];
  if (updates.length === 0) return;

  // Segment_ids restart at 1 per meeting, so a refine that slipped past a
  // meeting switch would otherwise rewrite a DIFFERENT speaker's bubble in
  // the new meeting with stale text from the old one.
  if (data && data.meeting_id && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId
      && data.meeting_id !== _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId) {
    return;
  }

  const byId = new Map();
  for (const upd of updates) {
    if (!upd) continue;
    const sid = Number(upd.segment_id);
    if (!Number.isFinite(sid) || sid <= 0) continue;
    if (typeof upd.text !== 'string') continue;
    byId.set(sid, upd.text);
  }
  if (byId.size === 0) return;

  // Resolve each refined segment_id to its containing state line. A state
  // line may own multiple segment_ids when consecutive PCM chunks were
  // merged as continuations (handleTranscript above) — we look up via both
  // `segment_ids` (child list) and `segment_id` (legacy primary).
  const touchedLines = new Set();
  for (const [sid, newText] of byId) {
    let target = null;
    for (const line of _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines) {
      if (Array.isArray(line.segment_ids) && line.segment_ids.includes(sid)) {
        target = line;
        break;
      }
      if (line.segment_id === sid) {
        target = line;
        break;
      }
    }
    if (!target) continue;
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
    target.refined = true;
    touchedLines.add(target);
  }

  if ((0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_4__.isJitsiPlatform)()) {
    const content = document.querySelector('.jitsi-transcript-content');
    if (content) {
      // Vas3k path renders one DOM line per chunk (addVas3kTranscriptLine is
      // called for every transcript, including continuations), so each
      // refined segment_id has its own bubble to target.
      for (const [sid, newText] of byId) {
        const lineEl = content.querySelector(`.jitsi-transcript-line[data-segment-id="${sid}"]`);
        if (!lineEl) continue;
        const textEl = lineEl.querySelector('.jitsi-transcript-text');
        if (!textEl) continue;
        textEl.textContent = newText;
        textEl.classList.add('refine-fade');
        setTimeout(() => textEl.classList.remove('refine-fade'), 200);
      }
    }
  } else {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) {
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
        textEl.innerHTML = formatTranscriptText(line.text);
        textEl.classList.add('refine-fade');
        setTimeout(() => textEl.classList.remove('refine-fade'), 200);
      }
      restoreScrollAnchor(container, anchor);
    }
  }

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice() }
  }));
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
function buildSpeakerAliasMap(lines) {
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
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) return;

  const alias = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.privacyMode ? buildSpeakerAliasMap(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines) : null;

  const text = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.map(line => {
    const base = (alias && !isSystemRow(line))
      ? (alias.get(aliasKey(line)) ?? PRIVACY_ALIAS_FALLBACK)
      : line.speaker;
    const speaker = base + (0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.getUncertainSuffix)(line);
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

  const anchor = captureScrollAnchor(container);
  const shouldAutoScroll = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled && isNearBottom(container);

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
    const segmentIdAttr = line.segment_id > 0
      ? ` data-segment-id="${line.segment_id}"`
      : '';

    return `<div class="${classes}" data-ts="${(0,_utils_js__WEBPACK_IMPORTED_MODULE_2__.escapeHtml)(timeValue)}"${segmentIdAttr}>
        <div class="ktalk-msg-header">
          <span class="ktalk-msg-speaker">${speakerDisplay}</span>
          <span class="ktalk-msg-time">${line.time}</span>
        </div>
        <div class="ktalk-msg-text">${messageText}</div>
      </div>`;
  }).join('');

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
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.autoScrollEnabled) return null;
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
    { value: 'codex', label: 'Codex (GPT-5.5)' },
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

  // For KTalk, return false - tabs are handled by addAiTab
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
/* harmony import */ var _meeting_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./meeting.js */ "./src/meeting.js");
/* harmony import */ var _ui_vas3k_js__WEBPACK_IMPORTED_MODULE_3__ = __webpack_require__(/*! ./ui-vas3k.js */ "./src/ui-vas3k.js");
/* harmony import */ var _speaker_js__WEBPACK_IMPORTED_MODULE_4__ = __webpack_require__(/*! ./speaker.js */ "./src/speaker.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_5__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
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
      // Reset channel-status indicator on bridge reconnect: the prior
      // session's `channel_disconnected` (set by `onclose` /
      // `handleCopilotBridgeDisconnect`) is no longer authoritative — the
      // bridge will replay the actual channel state via a `channel_connected`
      // primer in `_handle_client` if a channel client is registered.
      // Without this reset the indicator would stay stuck on "channel
      // offline" forever even after the channel came back.
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.copilotChannelStatus = 'unknown';

      // Send participants for both KTalk and vas3k
      const adapter = (0,_platforms_index_js__WEBPACK_IMPORTED_MODULE_6__.getPlatformAdapter)();
      if (adapter) {
        const conf = adapter.getConference();
        if (conf) {
          (0,_speaker_js__WEBPACK_IMPORTED_MODULE_4__.sendParticipantsUpdate)(conf);
          const activeSpeaker = (0,_speaker_js__WEBPACK_IMPORTED_MODULE_4__.getActiveSpeaker)(conf);
          (0,_speaker_js__WEBPACK_IMPORTED_MODULE_4__.sendFilteredSpeakerUpdate)(activeSpeaker);
          (0,_meeting_js__WEBPACK_IMPORTED_MODULE_2__.sendMeetingJoin)();
        }
      } else if (window.$conf) {
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_4__.sendParticipantsUpdate)(window.$conf);
        const activeSpeaker = (0,_speaker_js__WEBPACK_IMPORTED_MODULE_4__.getActiveSpeaker)(window.$conf);
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_4__.sendFilteredSpeakerUpdate)(activeSpeaker);
        (0,_meeting_js__WEBPACK_IMPORTED_MODULE_2__.sendMeetingJoin)();
      }

      (0,_meeting_js__WEBPACK_IMPORTED_MODULE_2__.startMeetingCheck)();

      // Clock sync for accurate timestamp correlation
      (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_5__.sendWsMessage)({ type: 'clock_sync', browser_time: Date.now() });

      // Re-sync clock every 30 seconds
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.clockSyncTimer = setInterval(() => {
        (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_5__.sendWsMessage)({ type: 'clock_sync', browser_time: Date.now() });
      }, 30000);
    };

    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.onclose = () => {
      console.log('[KTalk Transcript] WebSocket disconnected');
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected = false;
      (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicatorPlatform)(false);
      // Surface the bridge drop in the Copilot tab — flip pending
      // placeholders to error and re-enable input. Otherwise an in-flight
      // chat would leave the textarea disabled until the user reloaded.
      try {
        (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleCopilotBridgeDisconnect)();
      } catch (e) {
        console.log('[KTalk Copilot] bridge-disconnect handler failed:', e);
      }

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
        } else if (data.type === 'transcript_refine') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleTranscriptRefine)(data);
        } else if (data.type === 'new_meeting') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleNewMeeting)(data);
        } else if (data.type === 'summarize_response') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleSummaryResponse)(data);
          (0,_ui_vas3k_js__WEBPACK_IMPORTED_MODULE_3__.handleVas3kSummaryResponse)(data);
        } else if (data.type === 'anchor_response') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleAnchorResponse)(data);
        } else if (data.type === 'copilot_reply') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleCopilotReply)(data);
        } else if (data.type === 'copilot_status') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.handleCopilotStatus)(data);
        } else if (data.type === 'meeting_started') {
          const title = (data && data.meeting_title) ? ` — ${data.meeting_title}` : '';
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.appendCopilotSystemMessage)(`Встреча началась${title}`);
        } else if (data.type === 'meeting_ended') {
          (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.appendCopilotSystemMessage)('Встреча закончилась');
        } else if (data.type === 'meeting_join_rejected') {
          (0,_meeting_js__WEBPACK_IMPORTED_MODULE_2__.handleMeetingJoinRejected)(data);
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
/* harmony export */   sendCopilotChatMessage: () => (/* binding */ sendCopilotChatMessage),
/* harmony export */   sendWsConnectionStatus: () => (/* binding */ sendWsConnectionStatus),
/* harmony export */   sendWsMessage: () => (/* binding */ sendWsMessage),
/* harmony export */   sendWsParticipants: () => (/* binding */ sendWsParticipants),
/* harmony export */   sendWsPcmChunk: () => (/* binding */ sendWsPcmChunk),
/* harmony export */   sendWsPcmTransportStats: () => (/* binding */ sendWsPcmTransportStats),
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

function sendWsPcmChunk(pid, speakerName, isLocal, seq, startMs, endMs, pcmBase64) {
  // Tag every chunk with the tab's current meeting_id so the bridge can
  // drop sibling-tab PCM when two KTalk tabs are open at once. Without
  // this, audio from an older tab keeps flowing into whichever meeting
  // joined the bridge most recently and contaminates its transcript.jsonl.
  const meetingId = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingId || null;
  sendWsMessage({
    type: 'pcm_chunk',
    pid: pid,
    speaker_name: speakerName,
    is_local: !!isLocal,
    seq: seq,
    chunk_start_ms: startMs,
    chunk_end_ms: endMs,
    sample_rate: 16000,
    pcm_base64: pcmBase64,
    meeting_id: meetingId,
  });
}

function sendWsPcmTransportStats(stats) {
  sendWsMessage({
    type: 'pcm_transport_stats',
    stats: stats || {},
    timestamp: Date.now(),
  });
}

function generateCopilotRequestId() {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return 'copilot_' + crypto.randomUUID();
  }
  return 'copilot_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 10);
}

function sendCopilotChatMessage({ text, meetingId, meetingTitle, transcriptWindow }) {
  const requestId = generateCopilotRequestId();
  sendWsMessage({
    type: 'copilot_chat_message',
    request_id: requestId,
    text: text,
    meeting_id: meetingId || null,
    meeting_title: meetingTitle || null,
    transcript_window: transcriptWindow || [],
    created_at: Date.now()
  });
  return requestId;
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
/* harmony import */ var _focus_guard_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./focus_guard.js */ "./src/focus_guard.js");
/* harmony import */ var _init_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./init.js */ "./src/init.js");



(0,_focus_guard_js__WEBPACK_IMPORTED_MODULE_0__.installFocusGuard)();
(0,_init_js__WEBPACK_IMPORTED_MODULE_1__.init)();

})();

/******/ })()
;