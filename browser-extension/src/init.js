import { state } from './state.js';
import { injectStyles, addAiTab, createTranscriptContent, createCopilotContent, watchTabChanges, initPlatformUI } from './ui.js';
import { connectWebSocket } from './ws.js';
import { updateSpeakerStatus, sendParticipantsUpdate, getParticipantsList } from './speaker.js';
import { sendMeetingLeave } from './meeting.js';
import { sendWsConnectionStatus } from './ws_api.js';
import { ensureDebugPanel, updateDebugPanel } from './debug/panel.js';
import { getPlatformAdapter, getPlatform, getPlatformDisplayName, isJitsiPlatform } from './platforms/index.js';
import { showVas3kPrejoinState, showVas3kWaitingState } from './ui-vas3k.js';

const AI_CONTENT_RECOVERY = [
  { selector: '.ktalk-transcript-content', stateKey: 'transcriptContentElement', create: createTranscriptContent },
  { selector: '.ktalk-copilot-content', stateKey: 'copilotContentElement', create: createCopilotContent }
];

function sendConnectionStatus(connected) {
  window.dispatchEvent(new CustomEvent('ktalk-connection-status', { detail: { connected } }));
  sendWsConnectionStatus(connected);
  if (!connected) {
    state.conferenceReady = false;
    state.activeConfRef = null;
    state.activeJitsiConf = null;
  }
}

function waitForConference(callback, maxAttempts = 100) {
  const platform = getPlatform();
  const adapter = getPlatformAdapter();
  const platformName = getPlatformDisplayName();

  console.log(`[${platformName} Transcript] Waiting for conference...`);

  if (adapter) {
    // For Jitsi platforms: Check if in prejoin and update UI accordingly
    if (isJitsiPlatform() && adapter.isInPrejoin?.()) {
      console.log(`[${platformName} Transcript] User is in prejoin screen`);
      showVas3kPrejoinState();
    }

    adapter.waitForConference(maxAttempts)
      .then((conf) => {
        console.log(`[${platformName} Transcript] Conference found`);
        callback(conf);
      })
      .catch(() => {
        console.log(`[${platformName} Transcript] Conference not found after ${maxAttempts} attempts`);
        // For Jitsi platforms: keep retrying until conference is found
        if (isJitsiPlatform()) {
          if (adapter.isInPrejoin?.()) {
            console.log(`[${platformName} Transcript] Still in prejoin, retrying...`);
            showVas3kPrejoinState();
          } else {
            console.log(`[${platformName} Transcript] Not in prejoin, retrying...`);
            showVas3kWaitingState();
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
  const adapter = getPlatformAdapter();
  const platform = getPlatform();

  if (adapter) {
    // Use adapter to check conference availability
    if (!adapter.isConferenceAvailable()) {
      if (state.activeConfRef || state.activeJitsiConf) {
        state.conferenceReady = false;
        state.activeConfRef = null;
        state.activeJitsiConf = null;
      }
      // Update UI based on prejoin state
      if (isJitsiPlatform()) {
        if (adapter.isInPrejoin?.()) {
          showVas3kPrejoinState();
        } else {
          showVas3kWaitingState();
        }
      }
      return;
    }

    const conf = adapter.getConference();
    const jitsiConf = adapter.getJitsiConference();

    if (conf !== state.activeConfRef || jitsiConf !== state.activeJitsiConf) {
      // Dispose old subscriptions and analysers before reattach
      adapter.dispose();
      state.activeConfRef = conf;
      state.activeJitsiConf = jitsiConf;
      console.log(`[${getPlatformDisplayName()} Transcript] Conference reattached`);
      subscribeToDominantSpeaker(conf);
    }
  } else {
    // Legacy fallback for KTalk
    if (!window.$conf || !window.$conf.jitsiConf || !window.$conf._dominantSpeakerId$) {
      if (state.activeConfRef || state.activeJitsiConf) {
        state.conferenceReady = false;
        state.activeConfRef = null;
        state.activeJitsiConf = null;
      }
      return;
    }

    if (window.$conf !== state.activeConfRef || window.$conf.jitsiConf !== state.activeJitsiConf) {
      state.activeConfRef = window.$conf;
      state.activeJitsiConf = window.$conf.jitsiConf;
      console.log('[KTalk Transcript] Conference reattached');
      subscribeToDominantSpeaker(window.$conf);
    }
  }
}

function ktalkUiMissing() {
  // No tabset means the chat panel is closed - there is nowhere to inject yet.
  if (!document.querySelector('.tl-tabset__content')) return false;
  return !document.querySelector('.ktalk-ai-tab')
    || AI_CONTENT_RECOVERY.some(({ selector }) => !document.querySelector(selector));
}

function recoverKtalkUi() {
  if (!document.querySelector('.ktalk-ai-tab')) {
    setupUI();
  }
  for (const { selector, stateKey, create } of AI_CONTENT_RECOVERY) {
    if (!document.querySelector(selector)) {
      state[stateKey] = null;
      state.originalChatContent = null;
      create();
    }
  }
}

function setupUI() {
  const adapter = getPlatformAdapter();

  injectStyles();

  // Use platform-specific UI initialization
  if (adapter && !adapter.supportsTabInjection()) {
    // For platforms without native tab support (vas3k.cloud), use floating panel
    const uiReady = initPlatformUI();
    ensureDebugPanel();
    return uiReady;
  }

  // KTalk: Use tab injection
  const aiTabAdded = addAiTab();
  if (aiTabAdded) {
    createTranscriptContent();
    createCopilotContent();
    watchTabChanges();
  }

  ensureDebugPanel();
  return aiTabAdded;
}

function subscribeToDominantSpeaker(conf) {
  const adapter = getPlatformAdapter();
  const platformName = getPlatformDisplayName();

  sendConnectionStatus(true);
  sendParticipantsUpdate(conf);
  startParticipantsRefresh(conf, 'conference_ready');

  state.conferenceReady = false;
  if (state.conferenceReadyTimeout) clearTimeout(state.conferenceReadyTimeout);

  const checkAndMarkReady = () => {
    if (adapter) {
      if (adapter.isConferenceReady()) {
        state.conferenceReady = true;
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
        state.conferenceReady = true;
        console.log('[KTalk Transcript] Conference ready - tracks loaded');
      }
    }
  };

  checkAndMarkReady();
  state.conferenceReadyTimeout = setTimeout(() => {
    state.conferenceReady = true;
    console.log(`[${platformName} Transcript] Conference ready - timeout`);
    updateSpeakerStatus(conf);
  }, 5000);

  console.log(`[${platformName} Transcript] Subscribing to speaker events`);
  updateSpeakerStatus(conf);

  // Use adapter for event subscription
  if (adapter) {
    adapter.subscribeToSpeakerChanges(() => {
      checkAndMarkReady();
      updateSpeakerStatus(conf);
      sendParticipantsUpdate(conf);
    });

    adapter.onParticipantJoined?.((id) => {
      updateSpeakerStatus(conf);
      sendParticipantsUpdate(conf);
      startParticipantsRefresh(conf, 'user_joined');
    });
  } else {
    // Legacy KTalk event subscription
    conf?._participantsMap?.forEach((p, key) => {
      if (p._activeSpeaker$?.subscribe) {
        p._activeSpeaker$.subscribe(() => updateSpeakerStatus(conf));
      }
    });

    if (conf?._dominantSpeakerId$?.subscribe) {
      conf._dominantSpeakerId$.subscribe(() => {
        console.log('[KTalk Transcript] Dominant speaker changed:', conf._dominantSpeakerId$?.value);
        updateSpeakerStatus(conf);
      });
    }

    conf?.jitsiConf?.on('conference.dominantSpeaker', (id) => {
      console.log('[KTalk Transcript] Jitsi dominant speaker event:', id);
      updateSpeakerStatus(conf);
    });

    conf?.jitsiConf?.on('conference.trackAdded', (track) => {
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

    conf?.jitsiConf?.on('conference.trackRemoved', (track) => {
      if (track.getType?.() === 'audio') {
        updateSpeakerStatus(conf);
        sendParticipantsUpdate(conf);
      }
    });

    conf?.jitsiConf?.on('conference.trackMuteChanged', (track) => {
      if (track.getType?.() === 'audio') {
        updateSpeakerStatus(conf);
        sendParticipantsUpdate(conf);
      }
    });

    conf?.jitsiConf?.on('conference.userJoined', (id) => {
      setTimeout(() => {
        const p = conf._participantsMap?.get(id);
        if (p?._activeSpeaker$?.subscribe) {
          p._activeSpeaker$.subscribe(() => updateSpeakerStatus(conf));
        }
        updateSpeakerStatus(conf);
        sendParticipantsUpdate(conf);
        startParticipantsRefresh(conf, 'user_joined');
      }, 1000);
    });

    conf?.jitsiConf?.on('conference.userLeft', () => {
      setTimeout(() => {
        updateSpeakerStatus(conf);
        sendParticipantsUpdate(conf);
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

  if (state.participantsRefreshTimer) {
    clearInterval(state.participantsRefreshTimer);
    state.participantsRefreshTimer = null;
  }

  const tick = () => {
    updateSpeakerStatus(conf);
    sendParticipantsUpdate(conf);
    const count = getParticipantsList(conf).length;
    if (count >= 2 || Date.now() >= deadline) {
      clearInterval(state.participantsRefreshTimer);
      state.participantsRefreshTimer = null;
    }
  };

  console.log(`[${getPlatformDisplayName()} Transcript] Participants refresh start:`, reason);
  tick();
  state.participantsRefreshTimer = setInterval(tick, intervalMs);
}

export function init() {
  const platformName = getPlatformDisplayName();
  const adapter = getPlatformAdapter();

  console.log(`[${platformName} Transcript] Script loaded on platform: ${getPlatform()}`);

  setTimeout(connectWebSocket, 1000);

  // Panel presence alone can't gate setupUI(): an early bridge frame
  // (copilot_status primer ~1 s after WS connect) side-creates the floating
  // panel via ensureCopilotContent → createFloatingPanel before this 2 s
  // interval ever fires, which used to skip setupUI()/initPlatformUI()
  // forever (unregistered Copilot activate handler, no debug panel).
  let floatingUiInitialized = false;

  setInterval(() => {
    if (adapter && !adapter.supportsTabInjection()) {
      // For vas3k.cloud: check floating panel exists
      if (!floatingUiInitialized || !document.querySelector('.jitsi-transcript-panel')) {
        setupUI();
        floatingUiInitialized = true;
      }
    } else {
      // For KTalk: check tab exists
      recoverKtalkUi();
    }
    tryAttachConference();
    updateDebugPanel();
  }, 2000);

  // KTalk renders the tabset only while the chat panel is open, so the 2 s
  // interval alone shows the Recog tab with a visible lag after the panel
  // opens (and after every KTalk re-render). React to DOM changes instead;
  // the interval stays as a fallback.
  if (!adapter || adapter.supportsTabInjection()) {
    // Synchronous on purpose: rAF never fires and setTimeout is clamped to
    // ~1 s in hidden/unfocused tabs, while observer callbacks are microtasks
    // and run everywhere. ktalkUiMissing() keeps the common case to a single
    // querySelector, and our own insertions can't loop the observer - the
    // re-entered callback sees the UI present and returns.
    const uiObserver = new MutationObserver(() => {
      if (ktalkUiMissing()) recoverKtalkUi();
    });
    uiObserver.observe(document.body, { childList: true, subtree: true });
  }

  waitForConference(subscribeToDominantSpeaker);

  window.addEventListener('beforeunload', () => {
    if (state.currentMeetingUrl) {
      sendMeetingLeave('page_closed');
    }
    const beforeUnloadAdapter = getPlatformAdapter();
    beforeUnloadAdapter?.dispose();
  });
}
