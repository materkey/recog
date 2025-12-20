/******/ (() => { // webpackBootstrap
/******/ 	"use strict";
/******/ 	var __webpack_modules__ = ({

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
    ['transcriptLines', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length],
    ['speakerHistory', _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.length]
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
    transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length,
    speakerHistory: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakerHistory.length
  };

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.debugPanelElement.innerHTML = `<div class="ktalk-debug-title">KTalk Debug</div>${rowsHtml}<div class="ktalk-debug-json">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.escapeHtml)(JSON.stringify(payload, null, 2))}</div>`;
}

function formatValue(value) {
  if (value === null || value === undefined) return '—';
  if (typeof value === 'boolean') return value ? 'true' : 'false';
  return String(value);
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








function sendConnectionStatus(connected) {
  window.dispatchEvent(new CustomEvent('ktalk-connection-status', { detail: { connected } }));
  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_5__.sendWsConnectionStatus)(connected);
}

function waitForConference(callback, maxAttempts = 100) {
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

function setupUI() {
  (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.injectStyles)();

  const tabAdded = (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.addTranscriptTab)();
  if (tabAdded) {
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createTranscriptContent)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.watchTabChanges)();
    (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicator)(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected);
  }

  (0,_debug_panel_js__WEBPACK_IMPORTED_MODULE_6__.ensureDebugPanel)();
  return tabAdded;
}

function subscribeToDominantSpeaker(conf) {
  sendConnectionStatus(true);
  (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = false;
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReadyTimeout) clearTimeout(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReadyTimeout);

  const checkAndMarkReady = () => {
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    const hasAnyTrack = jitsiParticipants.some(jp => {
      const tracks = jp.getTracks?.() || jp._tracks || [];
      return tracks.some(t => (t.getType?.() || t.type) === 'audio');
    }) || localTracks.some(t => t.getType?.() === 'audio');

    if (hasAnyTrack) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = true;
      console.log('[KTalk Transcript] Conference ready - tracks loaded');
    }
  };

  checkAndMarkReady();
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReadyTimeout = setTimeout(() => {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady = true;
    console.log('[KTalk Transcript] Conference ready - timeout');
    (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
  }, 5000);

  console.log('[KTalk Transcript] Subscribing to speaker events');
  (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);

  conf._participantsMap?.forEach((p, key) => {
    if (p._activeSpeaker$?.subscribe) {
      p._activeSpeaker$.subscribe(() => (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf));
    }
  });

  if (conf._dominantSpeakerId$?.subscribe) {
    conf._dominantSpeakerId$.subscribe(() => {
      console.log('[KTalk Transcript] Dominant speaker changed:', conf._dominantSpeakerId$?.value);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
    });
  }

  conf.jitsiConf.on('conference.dominantSpeaker', (id) => {
    console.log('[KTalk Transcript] Jitsi dominant speaker event:', id);
    (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
  });

  conf.jitsiConf.on('conference.trackAdded', (track) => {
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

  conf.jitsiConf.on('conference.trackRemoved', (track) => {
    if (track.getType?.() === 'audio') {
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
    }
  });

  conf.jitsiConf.on('conference.trackMuteChanged', (track) => {
    if (track.getType?.() === 'audio') {
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
    }
  });

  conf.jitsiConf.on('conference.userJoined', (id) => {
    setTimeout(() => {
      const p = conf._participantsMap?.get(id);
      if (p?._activeSpeaker$?.subscribe) {
        p._activeSpeaker$.subscribe(() => (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf));
      }
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
    }, 1000);
  });

  conf.jitsiConf.on('conference.userLeft', () => {
    setTimeout(() => {
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.updateSpeakerStatus)(conf);
      (0,_speaker_js__WEBPACK_IMPORTED_MODULE_3__.sendParticipantsUpdate)(conf);
    }, 500);
  });

  console.log('[KTalk Transcript] Ready');
}

function init() {
  console.log('[KTalk Transcript] Script loaded');

  setTimeout(_ws_js__WEBPACK_IMPORTED_MODULE_2__.connectWebSocket, 1000);

  setInterval(() => {
    if (!document.querySelector('.ktalk-transcript-tab')) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptTabAdded = false;
      setupUI();
    }
    if (!document.querySelector('.ktalk-transcript-content')) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement = null;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent = null;
      (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.createTranscriptContent)();
    }
    (0,_debug_panel_js__WEBPACK_IMPORTED_MODULE_6__.updateDebugPanel)();
  }, 2000);

  waitForConference(subscribeToDominantSpeaker);

  window.addEventListener('beforeunload', () => {
    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl) {
      (0,_meeting_js__WEBPACK_IMPORTED_MODULE_4__.sendMeetingLeave)('page_closed');
    }
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



function sendMeetingJoin() {
  const url = window.location.href;
  if (url && url.includes('ktalk.ru')) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentMeetingUrl = url;
    (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_1__.sendWsMessage)({
      type: 'meeting_join',
      url: url,
      title: document.title || ''
    });
    console.log('[KTalk Transcript] Sent meeting_join:', url);
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
  }
}

function isInMeeting() {
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
/* harmony export */   sendParticipantsUpdate: () => (/* binding */ sendParticipantsUpdate),
/* harmony export */   updateSpeakerStatus: () => (/* binding */ updateSpeakerStatus)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _utils_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./utils.js */ "./src/utils.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");




function getActiveSpeaker(conf) {
  let activeSpeaker = null;

  conf._participantsMap?.forEach((p, key) => {
    if (p._activeSpeaker$?.value === true) {
      const info = p._participantInfo$?.value || p._userInfo$?.value;
      activeSpeaker = {
        id: p._id,
        name: (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getFullName)(p, info, p.name || p.displayName || key),
        isLocal: p.local
      };
    }
  });

  return activeSpeaker;
}

function isMicMuted(conf, participantId) {
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

  if (conf.localParticipant?.id === participantId) {
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    const localAudio = localTracks.find(t => t.getType?.() === 'audio');
    if (localAudio) {
      const muted = localAudio.isMuted?.();
      if (muted !== undefined) return muted === true;
    }
  }

  const participant = conf._participantsMap?.get(participantId);
  if (participant) {
    if (participant._audioMuted$?.value !== undefined) {
      return participant._audioMuted$.value === true;
    }
    if (participant._audioMuted !== undefined) {
      return participant._audioMuted === true;
    }
  }

  const speaking = conf._participantsMap?.get(participantId)?._activeSpeaker$?.value;
  if (speaking === true) {
    return false;
  }

  return true;
}

function anyMicOn(conf) {
  const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
  let foundAnyAudioTrack = false;

  for (const jp of jitsiParticipants) {
    const tracks = jp.getTracks?.() || jp._tracks || [];
    const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
    if (audioTrack) {
      foundAnyAudioTrack = true;
      if (audioTrack.isMuted?.() === false) return true;
    }
  }

  const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
  const localAudio = localTracks.find(t => t.getType?.() === 'audio');
  if (localAudio) {
    foundAnyAudioTrack = true;
    if (localAudio.isMuted?.() === false) return true;
  }

  conf._participantsMap?.forEach((p) => {
    if (p._audioMuted$?.value === false || p._audioMuted === false) {
      foundAnyAudioTrack = true;
    }
  });

  if (!foundAnyAudioTrack && !_state_js__WEBPACK_IMPORTED_MODULE_0__.state.conferenceReady) {
    return true;
  }

  let hasAnyParticipantWithUnmutedMic = false;
  conf._participantsMap?.forEach((p) => {
    if (p._audioMuted$?.value === false || p._audioMuted === false) {
      hasAnyParticipantWithUnmutedMic = true;
    }
  });
  if (hasAnyParticipantWithUnmutedMic) return true;

  return false;
}

function getParticipantsList(conf) {
  const participants = [];

  if (conf.localParticipant) {
    const lp = conf.localParticipant;
    const localInfo = lp._participantInfo$?.value || lp._userInfo$?.value;
    participants.push({
      id: lp.id,
      name: (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getFullName)(lp, localInfo, lp.name || lp.displayName || 'You'),
      isLocal: true,
      isSpeaking: conf._participantsMap?.get(lp.id)?._activeSpeaker$?.value || false,
      micMuted: isMicMuted(conf, lp.id)
    });
  }

  conf._participantsMap?.forEach((p, key) => {
    if (p._id !== conf.localParticipant?.id) {
      const info = p._participantInfo$?.value || p._userInfo$?.value;
      participants.push({
        id: p._id,
        name: (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getFullName)(p, info, p.name || p.displayName || key),
        isLocal: false,
        isSpeaking: p._activeSpeaker$?.value || false,
        micMuted: isMicMuted(conf, p._id)
      });
    }
  });

  return participants;
}

function sendParticipantsUpdate(conf) {
  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsParticipants)(getParticipantsList(conf));
}

function updateSpeakerStatus(conf) {
  const activeSpeaker = getActiveSpeaker(conf);
  const hasMicsOn = anyMicOn(conf);

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
        const prevName = (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getParticipantName)(conf, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker);
        addToHistory(prevName, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker, Date.now() - _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime);
      }
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker = activeSpeaker.id;
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime = Date.now();
    }
  } else {
    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.speakingStartTime) {
      const prevName = (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getParticipantName)(conf, _state_js__WEBPACK_IMPORTED_MODULE_0__.state.currentSpeaker);
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

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsSpeakerUpdate)(activeSpeaker);
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
  WS_URL: 'ws://localhost:8765'
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

  debugPanelElement: null,
  debugVisible: false,
  lastParticipants: [],
  lastActiveSpeaker: null
};


/***/ },

/***/ "./src/ui.js"
/*!*******************!*\
  !*** ./src/ui.js ***!
  \*******************/
(__unused_webpack___webpack_module__, __webpack_exports__, __webpack_require__) {

__webpack_require__.r(__webpack_exports__);
/* harmony export */ __webpack_require__.d(__webpack_exports__, {
/* harmony export */   addTranscriptTab: () => (/* binding */ addTranscriptTab),
/* harmony export */   createTranscriptContent: () => (/* binding */ createTranscriptContent),
/* harmony export */   handleNewMeeting: () => (/* binding */ handleNewMeeting),
/* harmony export */   handleSummaryResponse: () => (/* binding */ handleSummaryResponse),
/* harmony export */   handleTranscript: () => (/* binding */ handleTranscript),
/* harmony export */   handleTranscriptHistory: () => (/* binding */ handleTranscriptHistory),
/* harmony export */   injectStyles: () => (/* binding */ injectStyles),
/* harmony export */   updateWsIndicator: () => (/* binding */ updateWsIndicator),
/* harmony export */   watchTabChanges: () => (/* binding */ watchTabChanges)
/* harmony export */ });
/* harmony import */ var _state_js__WEBPACK_IMPORTED_MODULE_0__ = __webpack_require__(/*! ./state.js */ "./src/state.js");
/* harmony import */ var _utils_js__WEBPACK_IMPORTED_MODULE_1__ = __webpack_require__(/*! ./utils.js */ "./src/utils.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");




let selectedModel = localStorage.getItem('ktalk-ai-model') || 'gemini-2.5-flash';
let meetingTitle = document.title || '';

function injectStyles() {
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
        justify-content: space-between;
        padding: 8px 16px;
        background: rgb(56, 56, 56);
        border-bottom: 1px solid rgba(255,255,255,0.1);
      }
      .ktalk-transcript-toolbar-left {
        display: flex;
        gap: 8px;
      }
      .ktalk-transcript-scroll-btn {
        padding: 4px 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s;
      }
      .ktalk-transcript-scroll-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-copy-btn {
        padding: 4px 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s;
      }
      .ktalk-transcript-copy-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-copy-btn.copied {
        background: #22c55e;
      }
      .ktalk-transcript-summary-btn {
        padding: 4px 8px;
        background: rgba(255,255,255,0.1);
        border: none;
        border-radius: 4px;
        color: rgba(255,255,255,0.8);
        font-size: 11px;
        cursor: pointer;
        transition: background 0.15s;
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
          <span class="ws-dot ${_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected ? 'connected' : ''}"></span>
        </span>
        <span class="underline" style="background-color: rgb(0, 138, 255);"></span>
      </button>
    `;

  tabsContainer.appendChild(tab);

  tab.querySelector('button').addEventListener('click', () => {
    activateTranscriptTab();
  });

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptTabAdded = true;
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
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.innerHTML = `
      <div class="ktalk-transcript-speaker-bar">
        <div class="ktalk-speaker-dot"></div>
        <span class="ktalk-speaker-name">No one speaking</span>
      </div>
      <div class="ktalk-transcript-toolbar">
        <div class="ktalk-transcript-toolbar-left">
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-top" title="Scroll to top">↑ Top</button>
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-bottom" title="Scroll to bottom">↓ Bottom</button>
        </div>
        <div class="ktalk-transcript-toolbar-left">
          <button class="ktalk-transcript-summary-btn" data-type="catchup" title="Catch up - включиться в дискуссию">🎯</button>
          <button class="ktalk-transcript-summary-btn" data-type="brief" title="Краткое за 2 мин">📋</button>
          <button class="ktalk-transcript-summary-btn" data-type="full" title="Вся встреча + вопрос">💬</button>
          <button class="ktalk-transcript-copy-btn" title="Copy all transcript">Copy</button>
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
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.addEventListener('click', () => requestSummary(btn.dataset.type));
  });
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-scroll-top').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = 0;
  });
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.querySelector('.ktalk-scroll-bottom').addEventListener('click', () => {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (container) container.scrollTop = container.scrollHeight;
  });

  createSummaryModal();

  contentArea.appendChild(_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement);

  console.log('[KTalk Transcript] Content element created');
  return _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement;
}

function activateTranscriptTab() {
  const transcriptTabBtn = document.querySelector('.ktalk-transcript-tab');
  if (!transcriptTabBtn) return;

  const tab = transcriptTabBtn.closest('.tl-tab');
  if (tab) {
    tab.classList.add('_active');
    transcriptTabBtn.classList.add('_active');
  }

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent.style.display = 'none';
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.classList.add('active');
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.isTranscriptActive = true;
}

function deactivateTranscriptTab() {
  const transcriptTabBtn = document.querySelector('.ktalk-transcript-tab');
  if (!transcriptTabBtn) return;

  const tab = transcriptTabBtn.closest('.tl-tab');
  if (tab) {
    tab.classList.remove('_active');
    transcriptTabBtn.classList.remove('_active');
  }

  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.originalChatContent.style.display = '';
  }
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptContentElement.classList.remove('active');
  }

  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.isTranscriptActive = false;
}

function watchTabChanges() {
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

function updateWsIndicator(connected) {
  const dot = document.querySelector('.ktalk-transcript-tab-wrapper .ws-dot');
  if (dot) {
    dot.classList.toggle('connected', connected);
  }
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

  if (is_continuation && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length > 0) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines[_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length - 1].text += ' ' + text;
  } else {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.push({
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

    if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length > _state_js__WEBPACK_IMPORTED_MODULE_0__.constants.MAX_TRANSCRIPT_LINES) {
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.shift();
    }
  }

  updateTranscriptUI();

  window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
    detail: { transcriptLines: _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.slice() }
  }));
}

function copyAllTranscript() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.length === 0) return;

  const text = _state_js__WEBPACK_IMPORTED_MODULE_0__.state.transcriptLines.map(line => {
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getUncertainSuffix)(line);
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

    const speakerDisplay = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getUncertainSuffix)(line);

    return `<div class="${classes}">
        <div class="ktalk-msg-header">
          <span class="ktalk-msg-speaker">${speakerDisplay}</span>
          <span class="ktalk-msg-time">${line.time}</span>
        </div>
        <div class="ktalk-msg-text">${line.text}</div>
      </div>`;
  }).join('');

  container.scrollTop = container.scrollHeight;
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
    body.innerHTML = `<div class="ktalk-summary-text">${(0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.renderMarkdown)(text)}</div>`;
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
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getUncertainSuffix)(line);
    return `[${line.time}] ${speaker}: ${line.text}`;
  }).join('\n');

  showSummaryModal(summaryType);
  _state_js__WEBPACK_IMPORTED_MODULE_0__.state.summaryRequestPending = true;

  document.querySelectorAll('.ktalk-transcript-summary-btn').forEach(btn => {
    btn.classList.add('loading');
    btn.disabled = true;
  });

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsMessage)({
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
    const speaker = line.speaker + (0,_utils_js__WEBPACK_IMPORTED_MODULE_1__.getUncertainSuffix)(line);
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

  (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_2__.sendWsMessage)({
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
/* harmony import */ var _speaker_js__WEBPACK_IMPORTED_MODULE_2__ = __webpack_require__(/*! ./speaker.js */ "./src/speaker.js");
/* harmony import */ var _ws_api_js__WEBPACK_IMPORTED_MODULE_3__ = __webpack_require__(/*! ./ws_api.js */ "./src/ws_api.js");
/* harmony import */ var _meeting_js__WEBPACK_IMPORTED_MODULE_4__ = __webpack_require__(/*! ./meeting.js */ "./src/meeting.js");






function connectWebSocket() {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.readyState === WebSocket.OPEN) {
    return;
  }

  try {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection = new WebSocket(_state_js__WEBPACK_IMPORTED_MODULE_0__.constants.WS_URL);

    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.onopen = () => {
      console.log('[KTalk Transcript] WebSocket connected');
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected = true;
      (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicator)(true);

      if (window.$conf) {
        (0,_speaker_js__WEBPACK_IMPORTED_MODULE_2__.sendParticipantsUpdate)(window.$conf);
        const activeSpeaker = (0,_speaker_js__WEBPACK_IMPORTED_MODULE_2__.getActiveSpeaker)(window.$conf);
        (0,_ws_api_js__WEBPACK_IMPORTED_MODULE_3__.sendWsSpeakerUpdate)(activeSpeaker);
        (0,_meeting_js__WEBPACK_IMPORTED_MODULE_4__.sendMeetingJoin)();
      }

      (0,_meeting_js__WEBPACK_IMPORTED_MODULE_4__.startMeetingCheck)();
    };

    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.onclose = () => {
      console.log('[KTalk Transcript] WebSocket disconnected');
      _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnected = false;
      (0,_ui_js__WEBPACK_IMPORTED_MODULE_1__.updateWsIndicator)(false);

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


function sendWsMessage(data) {
  if (_state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection && _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.readyState === WebSocket.OPEN) {
    _state_js__WEBPACK_IMPORTED_MODULE_0__.state.wsConnection.send(JSON.stringify(data));
  }
}

function sendWsSpeakerUpdate(speaker) {
  sendWsMessage({
    type: 'speaker_update',
    speaker: speaker,
    timestamp: Date.now()
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