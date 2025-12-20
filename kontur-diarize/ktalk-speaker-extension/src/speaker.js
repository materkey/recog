import { state, constants } from './state.js';
import { sendUpdate, getFullName, getParticipantName } from './utils.js';
import { sendWsSpeakerUpdate, sendWsParticipants } from './ws_api.js';

export function getActiveSpeaker(conf) {
  let activeSpeaker = null;

  conf._participantsMap?.forEach((p, key) => {
    if (p._activeSpeaker$?.value === true) {
      const info = p._participantInfo$?.value || p._userInfo$?.value;
      activeSpeaker = {
        id: p._id,
        name: getFullName(p, info, p.name || p.displayName || key),
        isLocal: p.local
      };
    }
  });

  return activeSpeaker;
}

export function isMicMuted(conf, participantId) {
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

export function anyMicOn(conf) {
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

  if (!foundAnyAudioTrack && !state.conferenceReady) {
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

export function getParticipantsList(conf) {
  const participants = [];

  if (conf.localParticipant) {
    const lp = conf.localParticipant;
    const localInfo = lp._participantInfo$?.value || lp._userInfo$?.value;
    participants.push({
      id: lp.id,
      name: getFullName(lp, localInfo, lp.name || lp.displayName || 'You'),
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
        name: getFullName(p, info, p.name || p.displayName || key),
        isLocal: false,
        isSpeaking: p._activeSpeaker$?.value || false,
        micMuted: isMicMuted(conf, p._id)
      });
    }
  });

  return participants;
}

export function sendParticipantsUpdate(conf) {
  sendWsParticipants(getParticipantsList(conf));
}

export function updateSpeakerStatus(conf) {
  const activeSpeaker = getActiveSpeaker(conf);
  const hasMicsOn = anyMicOn(conf);

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

  sendWsSpeakerUpdate(activeSpeaker);
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
