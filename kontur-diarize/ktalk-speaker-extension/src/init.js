import { state } from './state.js';
import { injectStyles, addTranscriptTab, createTranscriptContent, watchTabChanges, updateWsIndicator } from './ui.js';
import { connectWebSocket } from './ws.js';
import { updateSpeakerStatus, sendParticipantsUpdate } from './speaker.js';
import { sendMeetingLeave } from './meeting.js';
import { sendWsConnectionStatus } from './ws_api.js';
import { ensureDebugPanel, updateDebugPanel } from './debug/panel.js';

function sendConnectionStatus(connected) {
  window.dispatchEvent(new CustomEvent('ktalk-connection-status', { detail: { connected } }));
  sendWsConnectionStatus(connected);
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
  injectStyles();

  const tabAdded = addTranscriptTab();
  if (tabAdded) {
    createTranscriptContent();
    watchTabChanges();
    updateWsIndicator(state.wsConnected);
  }

  ensureDebugPanel();
  return tabAdded;
}

function subscribeToDominantSpeaker(conf) {
  sendConnectionStatus(true);
  sendParticipantsUpdate(conf);

  state.conferenceReady = false;
  if (state.conferenceReadyTimeout) clearTimeout(state.conferenceReadyTimeout);

  const checkAndMarkReady = () => {
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    const hasAnyTrack = jitsiParticipants.some(jp => {
      const tracks = jp.getTracks?.() || jp._tracks || [];
      return tracks.some(t => (t.getType?.() || t.type) === 'audio');
    }) || localTracks.some(t => t.getType?.() === 'audio');

    if (hasAnyTrack) {
      state.conferenceReady = true;
      console.log('[KTalk Transcript] Conference ready - tracks loaded');
    }
  };

  checkAndMarkReady();
  state.conferenceReadyTimeout = setTimeout(() => {
    state.conferenceReady = true;
    console.log('[KTalk Transcript] Conference ready - timeout');
    updateSpeakerStatus(conf);
  }, 5000);

  console.log('[KTalk Transcript] Subscribing to speaker events');
  updateSpeakerStatus(conf);

  conf._participantsMap?.forEach((p, key) => {
    if (p._activeSpeaker$?.subscribe) {
      p._activeSpeaker$.subscribe(() => updateSpeakerStatus(conf));
    }
  });

  if (conf._dominantSpeakerId$?.subscribe) {
    conf._dominantSpeakerId$.subscribe(() => {
      console.log('[KTalk Transcript] Dominant speaker changed:', conf._dominantSpeakerId$?.value);
      updateSpeakerStatus(conf);
    });
  }

  conf.jitsiConf.on('conference.dominantSpeaker', (id) => {
    console.log('[KTalk Transcript] Jitsi dominant speaker event:', id);
    updateSpeakerStatus(conf);
  });

  conf.jitsiConf.on('conference.trackAdded', (track) => {
    if (track.getType?.() === 'audio') {
      checkAndMarkReady();
      updateSpeakerStatus(conf);
      sendParticipantsUpdate(conf);
      const participantId = track.getParticipantId?.();
      if (participantId) {
        const p = conf._participantsMap?.get(participantId);
        if (p?._activeSpeaker$?.subscribe) {
          p._activeSpeaker$.subscribe(() => updateSpeakerStatus(conf));
        }
      }
    }
  });

  conf.jitsiConf.on('conference.trackRemoved', (track) => {
    if (track.getType?.() === 'audio') {
      updateSpeakerStatus(conf);
      sendParticipantsUpdate(conf);
    }
  });

  conf.jitsiConf.on('conference.trackMuteChanged', (track) => {
    if (track.getType?.() === 'audio') {
      updateSpeakerStatus(conf);
      sendParticipantsUpdate(conf);
    }
  });

  conf.jitsiConf.on('conference.userJoined', (id) => {
    setTimeout(() => {
      const p = conf._participantsMap?.get(id);
      if (p?._activeSpeaker$?.subscribe) {
        p._activeSpeaker$.subscribe(() => updateSpeakerStatus(conf));
      }
      updateSpeakerStatus(conf);
      sendParticipantsUpdate(conf);
    }, 1000);
  });

  conf.jitsiConf.on('conference.userLeft', () => {
    setTimeout(() => {
      updateSpeakerStatus(conf);
      sendParticipantsUpdate(conf);
    }, 500);
  });

  console.log('[KTalk Transcript] Ready');
}

export function init() {
  console.log('[KTalk Transcript] Script loaded');

  setTimeout(connectWebSocket, 1000);

  setInterval(() => {
    if (!document.querySelector('.ktalk-transcript-tab')) {
      state.transcriptTabAdded = false;
      setupUI();
    }
    if (!document.querySelector('.ktalk-transcript-content')) {
      state.transcriptContentElement = null;
      state.originalChatContent = null;
      createTranscriptContent();
    }
    updateDebugPanel();
  }, 2000);

  waitForConference(subscribeToDominantSpeaker);

  window.addEventListener('beforeunload', () => {
    if (state.currentMeetingUrl) {
      sendMeetingLeave('page_closed');
    }
  });
}
