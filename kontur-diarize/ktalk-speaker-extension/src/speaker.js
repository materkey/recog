import { state, constants } from './state.js';
import { sendUpdate } from './utils.js';
import { sendWsSpeakerUpdate, sendWsParticipants } from './ws_api.js';
import { getPlatformAdapter, getPlatform, isJitsiPlatform } from './platforms/index.js';
import { updateVas3kSpeakerStatus } from './ui-vas3k.js';
import { isSpeech, getFlatness } from './audio-analyzer.js';

export function getActiveSpeaker(conf) {
  const adapter = getPlatformAdapter();
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

  // Legacy fallback
  const participants = [];
  const byId = new Map();
  state.lastParticipantsMapSize = conf?._participantsMap?.size || 0;

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
  state.lastJitsiParticipantsCount = jitsiParticipants.length;
  const jitsiParticipantsMap = conf?.jitsiConf?.participants || conf?.jitsiConf?._participants;
  const jitsiMapValues = jitsiParticipantsMap
    ? (typeof jitsiParticipantsMap.values === 'function'
      ? Array.from(jitsiParticipantsMap.values())
      : Object.values(jitsiParticipantsMap))
    : [];
  state.lastJitsiParticipantsMapCount = jitsiMapValues.length;

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

export function sendParticipantsUpdate(conf) {
  sendWsParticipants(getParticipantsList(conf));
}

// Get participant name using adapter
function getParticipantName(conf, participantId) {
  const adapter = getPlatformAdapter();
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

export function updateSpeakerStatus(conf) {
  const activeSpeaker = getActiveSpeaker(conf);
  const hasMicsOn = anyMicOn(conf);

  // Update platform-specific UI
  if (isJitsiPlatform()) {
    updateVas3kSpeakerStatus(activeSpeaker, hasMicsOn);
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
