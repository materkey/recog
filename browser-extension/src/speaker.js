import { state, constants } from './state.js';
import { sendUpdate } from './utils.js';
import { sendWsSpeakerUpdate, sendWsParticipants } from './ws_api.js';
import { getPlatformAdapter, getPlatform, isJitsiPlatform } from './platforms/index.js';
import { updateVas3kSpeakerStatus, showVas3kWaitingState } from './ui-vas3k.js';
import { isSpeech, getFlatness } from './audio-analyzer.js';
import { getJoinRejectionStatus } from './meeting.js';

export function getActiveSpeaker(conf) {
  const adapter = getPlatformAdapter();
  if (adapter) {
    return adapter.getActiveSpeaker();
  }

  // Legacy fallback for KTalk if adapter not ready
  let activeSpeaker = null;
  conf?._participantsMap?.forEach((p, key) => {
    if (p._activeSpeaker$?.value === true) {
      activeSpeaker = {
        id: p._id,
        name: legacyDisplayName(p, key),
        isLocal: p.local
      };
    }
  });
  return activeSpeaker;
}

// "First Last" (or "First") of a KTalk user/participant info object, '' without a first name.
function infoFullName(info) {
  const firstName = info?.firstName || '';
  const lastName = info?.lastName || '';
  if (firstName && lastName) return `${firstName} ${lastName}`;
  return firstName;
}

// Legacy helper for fallback
function _getFullNameLegacy(participant, info, fallback) {
  return infoFullName(participant?._userInfo$?.value) || infoFullName(info) || fallback || 'Unknown';
}

// A KTalk participant's name: its info observables, then its own name, then `fallback`.
function legacyDisplayName(p, fallback) {
  const info = p._participantInfo$?.value || p._userInfo$?.value;
  return _getFullNameLegacy(p, info, p.name || p.displayName || fallback);
}

export function isMicMuted(conf, participantId) {
  const adapter = getPlatformAdapter();
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

export function anyMicOn(conf) {
  const adapter = getPlatformAdapter();
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

  if (!foundAnyAudioTrack && !state.conferenceReady) {
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

export function getParticipantsList(conf) {
  const adapter = getPlatformAdapter();
  if (adapter) {
    const participants = adapter.getParticipants();
    state.lastParticipantsMapSize = participants.length;
    return participants;
  }

  return legacyParticipantsList(conf);
}

function legacyParticipantsList(conf) {
  state.lastParticipantsMapSize = conf?._participantsMap?.size || 0;

  // Only return participants if we're actually in a meeting
  const isInMeeting = conf?._participantsMap && conf._participantsMap.size > 0;
  if (!isInMeeting) {
    return [];
  }

  const participants = [];
  const byId = new Map();
  const add = (entry) => {
    participants.push(entry);
    byId.set(entry.id, entry);
  };

  if (conf.localParticipant) {
    add(legacyLocalEntry(conf, conf.localParticipant));
  }

  conf._participantsMap?.forEach((p, key) => {
    if (p._id !== conf.localParticipant?.id) {
      add(legacyRemoteEntry(conf, p, key));
    }
  });

  // Fallback: add participants from Jitsi if map is empty or missing remotes
  const jitsiParticipants = conf?.jitsiConf?.getParticipants?.() || [];
  state.lastJitsiParticipantsCount = jitsiParticipants.length;
  const jitsiMapValues = jitsiParticipantValues(conf?.jitsiConf);
  state.lastJitsiParticipantsMapCount = jitsiMapValues.length;

  for (const jp of [...jitsiParticipants, ...jitsiMapValues]) {
    const pid = jp?._id || jp?.getId?.() || jp?.id;
    if (!pid || byId.has(pid)) continue;
    add(jitsiEntry(conf, jp, pid));
  }

  return participants;
}

function legacyLocalEntry(conf, lp) {
  return {
    id: lp.id,
    name: legacyDisplayName(lp, 'You'),
    isLocal: true,
    isSpeaking: conf._participantsMap?.get(lp.id)?._activeSpeaker$?.value || false,
    micMuted: isMicMuted(conf, lp.id)
  };
}

function legacyRemoteEntry(conf, p, key) {
  return {
    id: p._id,
    name: legacyDisplayName(p, key),
    isLocal: false,
    isSpeaking: p._activeSpeaker$?.value || false,
    micMuted: isMicMuted(conf, p._id)
  };
}

// Jitsi's own participant registry: a Map, or a plain object.
function jitsiParticipantValues(jitsiConf) {
  const registry = jitsiConf?.participants || jitsiConf?._participants;
  if (!registry) return [];
  return typeof registry.values === 'function'
    ? Array.from(registry.values())
    : Object.values(registry);
}

function jitsiEntry(conf, jp, pid) {
  return {
    id: pid,
    name: jp.getDisplayName?.() || jp._displayName || jp.displayName || jp._name || pid,
    isLocal: false,
    isSpeaking: conf?._participantsMap?.get(pid)?._activeSpeaker$?.value || false,
    micMuted: isMicMuted(conf, pid)
  };
}

export function sendParticipantsUpdate(conf) {
  // Optional adapter hook: the user's own microphone when the platform knows it
  // apart from the participant list (Zoom: the toolbar's audio button).
  const localMicMuted = getPlatformAdapter()?.getLocalMicMuted?.();
  sendWsParticipants(getParticipantsList(conf), localMicMuted);
}

// Get participant name using adapter
function getParticipantName(conf, participantId) {
  const adapter = getPlatformAdapter();
  if (adapter) {
    return adapter.getParticipantName(participantId);
  }

  return legacyParticipantName(conf, participantId);
}

function legacyParticipantName(conf, participantId) {
  if (!participantId) return 'Unknown';

  if (conf?.localParticipant && conf.localParticipant.id === participantId) {
    return legacyDisplayName(conf.localParticipant, 'You');
  }

  const participant = conf?._participantsMap?.get(participantId);
  if (participant) {
    return legacyDisplayName(participant, participantId);
  }

  return participantId;
}

export function updateSpeakerStatus(conf) {
  const activeSpeaker = getActiveSpeaker(conf);
  const hasMicsOn = anyMicOn(conf);

  // Update platform-specific UI
  if (isJitsiPlatform()) {
    updateVas3kSpeakerStatus(activeSpeaker, hasMicsOn);
    // A refused join and selector drift (Meet) outrank the speaker text:
    // otherwise every indicator change would overwrite them within ~100 ms.
    const diagnostic = getJoinRejectionStatus() || getPlatformAdapter()?.getDiagnostic?.();
    if (diagnostic) showVas3kWaitingState(diagnostic);
  }

  // KTalk UI elements
  const dot = document.querySelector('.ktalk-speaker-dot');
  const nameEl = document.querySelector('.ktalk-speaker-name');

  if (!activeSpeaker && !hasMicsOn && state.conferenceReady) {
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    console.log('[KTalk Debug] All mics off detected:', {
      conferenceReady: state.conferenceReady,
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
    if (state.currentSpeaker !== activeSpeaker.id) {
      if (state.currentSpeaker && state.speakingStartTime) {
        const prevName = getParticipantName(conf, state.currentSpeaker);
        addToHistory(prevName, state.currentSpeaker, Date.now() - state.speakingStartTime);
      }
      state.currentSpeaker = activeSpeaker.id;
      state.speakingStartTime = Date.now();
    }
  } else {
    if (state.currentSpeaker && state.speakingStartTime) {
      const prevName = getParticipantName(conf, state.currentSpeaker);
      addToHistory(prevName, state.currentSpeaker, Date.now() - state.speakingStartTime);
      state.currentSpeaker = null;
      state.speakingStartTime = null;
    }
  }

  const participants = getParticipantsList(conf);
  state.lastActiveSpeaker = activeSpeaker;
  state.lastParticipants = participants;

  sendUpdate({
    currentSpeaker: activeSpeaker,
    speakerHistory: state.speakerHistory,
    participants: participants
  });

  sendFilteredSpeakerUpdate(activeSpeaker);
}

export function sendFilteredSpeakerUpdate(speaker) {
  if (speaker) {
    // Jitsi keeps _activeSpeaker$ pointing at the last dominant speaker even after
    // they mute.  If the reported speaker's mic is muted, suppress the update so
    // the bridge doesn't attribute audio to a silent participant.
    const adapter = getPlatformAdapter();
    const conf = adapter?.getConference?.() || window.$conf;
    if (conf && isMicMuted(conf, speaker.id)) {
      sendWsSpeakerUpdate(null);
      return;
    }
  }
  if (constants.AUDIO_FILTER_ENABLED && speaker) {
    const result = isSpeech(speaker.id);
    state.audioFilterLastFlatness = getFlatness(speaker.id);
    state.audioFilterLastReason = result.reason;
    if (!result.speech) {
      state.audioFilterSuppressCount++;
      sendWsSpeakerUpdate(null);
      return;
    }
  }
  sendWsSpeakerUpdate(speaker);
}

function addToHistory(name, speakerId, durationMs) {
  if (!name || durationMs < constants.SPEAKING_THRESHOLD_MS) return;

  state.speakerHistory.unshift({
    name, id: speakerId,
    time: new Date().toLocaleTimeString('ru-RU', {
      hour: '2-digit', minute: '2-digit', second: '2-digit'
    }),
    durationMs
  });

  if (state.speakerHistory.length > constants.MAX_HISTORY) state.speakerHistory.pop();
}
