/**
 * KTalk Platform Adapter
 *
 * KTalk uses a custom wrapper around Jitsi with RxJS observables:
 * - window.$conf - main conference object
 * - $conf._participantsMap - Map of participants with RxJS observables
 * - $conf._dominantSpeakerId$ - RxJS observable for dominant speaker
 * - $conf.jitsiConf - underlying Jitsi conference
 */

import { attachParticipant, detachParticipant, attachAllFromConf, dispose as audioAnalyzerDispose } from '../audio-analyzer.js';

export class KTalkAdapter {
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
    attachAllFromConf(conf);

    // Also subscribe to Jitsi events
    const jitsiConf = conf.jitsiConf;
    if (jitsiConf) {
      this._onJitsi(jitsiConf, 'conference.dominantSpeaker', () => callback());

      this._onJitsi(jitsiConf, 'conference.trackAdded', (track) => {
        if (track.getType?.() === 'audio') {
          callback();
          const pid = track.getParticipantId?.();
          const stream = track.stream || track.getOriginalStream?.();
          if (pid && stream) attachParticipant(pid, stream);
        }
      });

      this._onJitsi(jitsiConf, 'conference.trackRemoved', (track) => {
        if (track.getType?.() === 'audio') {
          callback();
          const pid = track.getParticipantId?.();
          if (pid) detachParticipant(pid);
        }
      });

      this._onJitsi(jitsiConf, 'conference.trackMuteChanged', (track) => {
        if (track.getType?.() === 'audio') callback();
      });

      this._onJitsi(jitsiConf, 'conference.userJoined', () => {
        setTimeout(callback, 1000);
      });

      this._onJitsi(jitsiConf, 'conference.userLeft', (id) => {
        detachParticipant(id);
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
    audioAnalyzerDispose();
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
