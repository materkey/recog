/**
 * vas3k.cloud (Standard Jitsi Meet) Platform Adapter
 *
 * Uses standard Jitsi Meet API:
 * - window.APP - main application object
 * - APP.conference._room - JitsiConference object
 * - APP.store - Redux store with participants state
 * - room.on('conference.dominantSpeaker', callback) - speaker events
 */

export class Vas3kAdapter {
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
