// KTalk Speaker Detector - Injected Script
// Integrates transcript tab into KTalk side menu (next to General/Private)
// WebSocket integration with recog for live transcription

(function() {
  'use strict';

  let transcriptTabAdded = false;
  let transcriptContentElement = null;
  let originalChatContent = null;
  let isTranscriptActive = false;

  let speakerHistory = [];
  const MAX_HISTORY = 100;
  const SPEAKING_THRESHOLD_MS = 500;

  let currentSpeaker = null;
  let speakingStartTime = null;

  // Transcript storage
  let transcriptLines = [];
  const MAX_TRANSCRIPT_LINES = 100;

  // WebSocket connection to recog ktalk_bridge
  const WS_URL = 'ws://localhost:8765';
  let wsConnection = null;
  let wsReconnectTimer = null;
  let wsConnected = false;

  // Track if conference is fully initialized (tracks loaded)
  let conferenceReady = false;
  let conferenceReadyTimeout = null;

  // Meeting state tracking
  let currentMeetingUrl = null;
  let meetingCheckInterval = null;

  // Inject styles once
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
        gap: 10px;
        padding: 10px 16px;
        background: rgba(0, 0, 0, 0.15);
        border-bottom: 1px solid rgba(255,255,255,0.06);
        flex-shrink: 0;
      }

      .ktalk-speaker-dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: rgba(255,255,255,0.3);
        transition: all 0.3s;
      }
      .ktalk-speaker-dot.speaking {
        background: #4ade80;
        box-shadow: 0 0 6px #4ade80;
        animation: ktalk-pulse 1.5s infinite;
      }

      @keyframes ktalk-pulse {
        0%, 100% { box-shadow: 0 0 6px #4ade80; }
        50% { box-shadow: 0 0 12px #4ade80; }
      }

      .ktalk-speaker-name {
        font-size: 13px;
        font-weight: 500;
        color: rgba(255,255,255,0.8);
      }

      .ktalk-transcript-toolbar {
        display: flex;
        justify-content: space-between;
        padding: 8px 12px;
        border-bottom: 1px solid rgba(255,255,255,0.06);
        flex-shrink: 0;
        background: rgba(0, 0, 0, 0.15);
      }
      .ktalk-transcript-toolbar-left {
        display: flex;
        gap: 6px;
      }
      .ktalk-transcript-scroll-btn {
        display: flex;
        align-items: center;
        gap: 4px;
        padding: 4px 8px;
        border: none;
        border-radius: 4px;
        background: rgba(255,255,255,0.08);
        color: rgba(255,255,255,0.6);
        font-size: 11px;
        cursor: pointer;
        transition: all 0.15s;
      }
      .ktalk-transcript-scroll-btn:hover {
        background: rgba(255,255,255,0.12);
        color: rgba(255,255,255,0.8);
      }
      .ktalk-transcript-copy-btn {
        display: flex;
        align-items: center;
        gap: 4px;
        padding: 4px 10px;
        border: none;
        border-radius: 4px;
        background: rgba(255,255,255,0.08);
        color: rgba(255,255,255,0.6);
        font-size: 12px;
        cursor: pointer;
        transition: all 0.15s;
      }
      .ktalk-transcript-copy-btn:hover {
        background: rgba(255,255,255,0.12);
        color: rgba(255,255,255,0.8);
      }
      .ktalk-transcript-copy-btn.copied {
        background: rgba(34, 197, 94, 0.15);
        color: #4ade80;
      }

      .ktalk-transcript-messages {
        flex: 1;
        overflow-y: auto;
        padding: 12px;
        user-select: text;
        -webkit-user-select: text;
        background: rgba(0, 0, 0, 0.15);
      }

      .ktalk-msg {
        padding: 10px 12px;
        margin-bottom: 8px;
        border-radius: 6px;
        background: rgba(255,255,255,0.04);
      }

      .ktalk-msg-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 4px;
      }

      .ktalk-msg-speaker {
        font-weight: 600;
        font-size: 12px;
        color: rgba(255,255,255,0.8);
      }

      .ktalk-msg-time {
        font-size: 11px;
        color: rgba(255,255,255,0.4);
      }

      .ktalk-msg-text {
        font-size: 13px;
        line-height: 1.4;
        color: rgba(255,255,255,0.8);
      }

      /* Local messages (me/mic) - blue accent */
      .ktalk-msg.local {
        background: rgba(0, 138, 255, 0.1);
        border-left: 3px solid rgb(0, 138, 255);
      }
      .ktalk-msg.local .ktalk-msg-speaker {
        color: rgb(100, 180, 255);
      }

      /* Remote messages (others) - subtle */
      .ktalk-msg.remote {
        background: rgba(255, 255, 255, 0.04);
      }
      .ktalk-msg.remote .ktalk-msg-speaker {
        color: rgba(255,255,255,0.8);
      }

      /* Uncertain messages - yellow (local speaker echo) */
      .ktalk-msg.uncertain {
        background: rgba(250, 204, 21, 0.06);
        border-left: 3px dashed rgba(250, 204, 21, 0.6);
      }
      .ktalk-msg.uncertain .ktalk-msg-speaker {
        color: #facc15;
      }

      /* Uncertain messages - orange (all mics off) */
      .ktalk-msg.uncertain.mics-off {
        background: rgba(251, 146, 60, 0.06);
        border-left: 3px dashed rgba(251, 146, 60, 0.6);
      }
      .ktalk-msg.uncertain.mics-off .ktalk-msg-speaker {
        color: #fb923c;
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

  function connectWebSocket() {
    if (wsConnection && wsConnection.readyState === WebSocket.OPEN) {
      return;
    }

    try {
      wsConnection = new WebSocket(WS_URL);

      wsConnection.onopen = () => {
        console.log('[KTalk Transcript] WebSocket connected');
        wsConnected = true;
        updateWsIndicator(true);

        if (window.$conf) {
          sendWsParticipants(window.$conf);
          const activeSpeaker = getActiveSpeaker(window.$conf);
          sendWsSpeakerUpdate(activeSpeaker);

          // Send meeting_join with current URL
          sendMeetingJoin();
        }

        // Start meeting status check
        startMeetingCheck();
      };

      wsConnection.onclose = () => {
        console.log('[KTalk Transcript] WebSocket disconnected');
        wsConnected = false;
        updateWsIndicator(false);

        if (!wsReconnectTimer) {
          wsReconnectTimer = setTimeout(() => {
            wsReconnectTimer = null;
            connectWebSocket();
          }, 5000);
        }
      };

      wsConnection.onerror = () => {
        wsConnected = false;
      };

      wsConnection.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          if (data.type === 'transcript') {
            handleTranscript(data);
          } else if (data.type === 'transcript_history') {
            handleTranscriptHistory(data);
          } else if (data.type === 'new_meeting') {
            handleNewMeeting(data);
          }
        } catch (e) {
          console.log('[KTalk Transcript] Parse error:', e);
        }
      };

    } catch (e) {
      console.log('[KTalk Transcript] Connection failed:', e);
    }
  }

  function sendWsMessage(data) {
    if (wsConnection && wsConnection.readyState === WebSocket.OPEN) {
      wsConnection.send(JSON.stringify(data));
    }
  }

  function sendWsSpeakerUpdate(speaker) {
    sendWsMessage({
      type: 'speaker_update',
      speaker: speaker,
      timestamp: Date.now()
    });
  }

  function sendWsParticipants(conf) {
    const participants = getParticipantsList(conf);
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

  // Meeting tracking functions
  function sendMeetingJoin() {
    const url = window.location.href;
    if (url && url.includes('ktalk.ru')) {
      currentMeetingUrl = url;
      sendWsMessage({
        type: 'meeting_join',
        url: url,
        title: document.title || ''
      });
      console.log('[KTalk Transcript] Sent meeting_join:', url);
    }
  }

  function sendMeetingLeave(reason = 'user_left') {
    if (currentMeetingUrl) {
      sendWsMessage({
        type: 'meeting_leave',
        reason: reason
      });
      console.log('[KTalk Transcript] Sent meeting_leave:', reason);
      currentMeetingUrl = null;
    }
  }

  function isInMeeting() {
    // Check if we're in an active meeting by checking $conf participants
    if (!window.$conf) return false;
    const participantsMap = window.$conf._participantsMap;
    return participantsMap && participantsMap.size > 0;
  }

  function startMeetingCheck() {
    if (meetingCheckInterval) {
      clearInterval(meetingCheckInterval);
    }

    meetingCheckInterval = setInterval(() => {
      const inMeeting = isInMeeting();
      const url = window.location.href;

      if (inMeeting && url !== currentMeetingUrl) {
        // Joined a (new) meeting
        if (currentMeetingUrl) {
          // Was in another meeting, send leave first
          sendMeetingLeave('changed_meeting');
        }
        sendMeetingJoin();
      } else if (!inMeeting && currentMeetingUrl) {
        // Left the meeting
        sendMeetingLeave('user_left');
      }
    }, 2000);
  }

  function handleTranscriptHistory(data) {
    // Received transcript history from server (after reconnect or meeting_join)
    console.log('[KTalk Transcript] Received history:', data.segments?.length || 0, 'segments');

    if (!data.segments || data.segments.length === 0) {
      return;
    }

    // Clear current transcript and load history
    transcriptLines = [];

    for (const seg of data.segments) {
      transcriptLines.push({
        speaker: seg.speaker,
        text: seg.text,
        timestamp: seg.ts_display || seg.timestamp,
        is_local: seg.is_local || false,
        uncertain: seg.uncertain || false,
        uncertain_reason: seg.uncertain_reason || "",
        time: seg.ts_display || new Date(seg.ts).toLocaleTimeString('ru-RU', {
          hour: '2-digit', minute: '2-digit', second: '2-digit'
        }),
        createdAt: seg.ts ? new Date(seg.ts).getTime() : Date.now()
      });
    }

    // Keep only last MAX_TRANSCRIPT_LINES
    if (transcriptLines.length > MAX_TRANSCRIPT_LINES) {
      transcriptLines = transcriptLines.slice(-MAX_TRANSCRIPT_LINES);
    }

    updateTranscriptUI();

    // Notify popup
    window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
      detail: { transcriptLines: transcriptLines.slice() }
    }));
  }

  function handleNewMeeting(data) {
    // Server created a new meeting (after meeting_leave)
    console.log('[KTalk Transcript] New meeting:', data.meeting_id);

    // Clear transcript for new meeting
    transcriptLines = [];
    updateTranscriptUI();

    // Notify popup
    window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
      detail: { transcriptLines: [] }
    }));
  }

  function updateWsIndicator(connected) {
    const dot = document.querySelector('.ktalk-transcript-tab-wrapper .ws-dot');
    if (dot) {
      dot.classList.toggle('connected', connected);
    }
  }

  function handleTranscript(data) {
    const { speaker, text, timestamp, is_continuation, is_local, uncertain, uncertain_reason } = data;

    if (is_continuation && transcriptLines.length > 0) {
      transcriptLines[transcriptLines.length - 1].text += ' ' + text;
    } else {
      transcriptLines.push({
        speaker,
        text,
        timestamp,
        is_local: is_local || false,
        uncertain: uncertain || false,
        uncertain_reason: uncertain_reason || "",  // "local" or "mics_off"
        time: new Date().toLocaleTimeString('ru-RU', {
          hour: '2-digit', minute: '2-digit', second: '2-digit'
        }),
        createdAt: Date.now()
      });

      if (transcriptLines.length > MAX_TRANSCRIPT_LINES) {
        transcriptLines.shift();
      }
    }

    updateTranscriptUI();

    // Send transcript to content script for popup
    window.dispatchEvent(new CustomEvent('ktalk-transcript-update', {
      detail: { transcriptLines: transcriptLines.slice() }
    }));
  }

  function getUncertainSuffix(line) {
    if (!line.uncertain) return "";
    if (line.uncertain_reason === "mics_off") return " (🔇?)";
    return " (?)";  // "local" or unknown
  }

  function copyAllTranscript() {
    if (transcriptLines.length === 0) return;

    const text = transcriptLines.map(line => {
      const speaker = line.speaker + getUncertainSuffix(line);
      return `**${speaker}** *[${line.time}]*: ${line.text}`;
    }).join('\n\n');

    navigator.clipboard.writeText(text).then(() => {
      const btn = document.querySelector('.ktalk-transcript-copy-btn');
      const textSpan = btn?.querySelector('.ktalk-copy-text');
      if (btn && textSpan) {
        btn.classList.add('copied');
        textSpan.textContent = 'Copied!';
        setTimeout(() => {
          btn.classList.remove('copied');
          textSpan.textContent = 'Copy all';
        }, 2000);
      }
    }).catch(err => {
      console.log('[KTalk Transcript] Copy failed:', err);
    });
  }

  function updateTranscriptUI() {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (!container) return;

    if (transcriptLines.length === 0) {
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

    container.innerHTML = transcriptLines.map(line => {
      const classes = [
        'ktalk-msg',
        line.is_local ? 'local' : 'remote',
        line.uncertain ? 'uncertain' : '',
        line.uncertain_reason === 'mics_off' ? 'mics-off' : ''
      ].filter(Boolean).join(' ');

      const speakerDisplay = line.speaker + getUncertainSuffix(line);

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

  function sendUpdate(data) {
    window.dispatchEvent(new CustomEvent('ktalk-speaker-update', { detail: data }));
  }

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

  function getFullName(participant, info, fallback) {
    const userInfo = participant?._userInfo$?.value;
    if (userInfo) {
      const firstName = userInfo.firstName || '';
      const lastName = userInfo.lastName || '';
      if (firstName && lastName) return `${firstName} ${lastName}`;
      if (firstName) return firstName;
      if (lastName) return lastName;
    }

    if (participant?.displayName?.includes(' ')) return participant.displayName;
    if (participant?.name?.includes(' ')) return participant.name;

    if (info) {
      const name = info.name || info.firstName || '';
      const surname = info.surname || info.lastName || '';
      if (info.displayName && info.displayName.includes(' ')) return info.displayName;
      if (name && surname) return `${name} ${surname}`;
      if (info.displayName) return info.displayName;
      if (name) return name;
    }

    return fallback;
  }

  function getParticipantName(conf, participantId) {
    if (!participantId) return null;
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

  function getActiveSpeaker(conf) {
    let activeSpeaker = null;

    // Method 1: Check _activeSpeaker$ observable on each participant
    // This is the most reliable - it means someone is CURRENTLY speaking
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

    // NOTE: We intentionally do NOT use _dominantSpeakerId$ or jitsiConf dominant speaker
    // as a fallback because those represent the LAST speaker, not the CURRENT speaker.
    // If no one has _activeSpeaker$ = true, then no one is speaking right now.

    return activeSpeaker;
  }

  function isMicMuted(conf, participantId) {
    // First try jitsi participant tracks
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

    // Fallback: check _participantsMap for direct mute state
    const participant = conf._participantsMap?.get(participantId);
    if (participant) {
      // Try _audioMuted$ observable
      if (participant._audioMuted$?.value !== undefined) {
        return participant._audioMuted$.value === true;
      }
      // Try direct _audioMuted property
      if (participant._audioMuted !== undefined) {
        return participant._audioMuted === true;
      }
    }

    // If we can't determine, check if participant is speaking
    const speaking = conf._participantsMap?.get(participantId)?._activeSpeaker$?.value;
    if (speaking === true) {
      // If participant is actively speaking, they can't have muted mic
      return false;
    }

    return true;
  }

  function anyMicOn(conf) {
    // Check remote participants via jitsi tracks
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

    // Check local participant
    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    const localAudio = localTracks.find(t => t.getType?.() === 'audio');
    if (localAudio) {
      foundAnyAudioTrack = true;
      if (localAudio.isMuted?.() === false) return true;
    }

    // Fallback: check _participantsMap for any unmuted participant
    // This catches cases where jitsiConf tracks are not yet loaded
    conf._participantsMap?.forEach((p) => {
      // Some participants may have _audioMuted property directly
      if (p._audioMuted$?.value === false || p._audioMuted === false) {
        foundAnyAudioTrack = true;
      }
    });

    // If we haven't found ANY audio tracks yet, conference is still loading
    // In this case, don't report "all mics off" - return true to avoid false alarm
    if (!foundAnyAudioTrack && !conferenceReady) {
      return true; // Assume mics are on until we can properly detect
    }

    // Check _participantsMap for unmuted state
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

  // Add Transcript tab to KTalk's tab bar (next to General/Private)
  function addTranscriptTab() {
    if (document.querySelector('.ktalk-transcript-tab')) {
      return true; // Already added
    }

    // Find the tabs container
    const tabsContainer = document.querySelector('.tl-tabset__content');
    if (!tabsContainer) {
      return false;
    }

    console.log('[KTalk Transcript] Adding tab to tabset');

    // Create transcript tab - match KTalk's tl-tab structure
    const tab = document.createElement('div');
    tab.className = 'tl-tab _switchable ktalk-transcript-tab-wrapper';
    tab.innerHTML = `
      <button type="button" class="ktalk-transcript-tab link">
        <span class="tab-content">
          <span class="title">Transcript</span>
          <span class="ws-dot ${wsConnected ? 'connected' : ''}"></span>
        </span>
        <span class="underline" style="background-color: rgb(0, 138, 255);"></span>
      </button>
    `;

    tabsContainer.appendChild(tab);

    // Add click handler
    tab.querySelector('button').addEventListener('click', () => {
      activateTranscriptTab();
    });

    transcriptTabAdded = true;
    return true;
  }

  // Create transcript content area
  function createTranscriptContent() {
    if (document.querySelector('.ktalk-transcript-content')) {
      return document.querySelector('.ktalk-transcript-content');
    }

    // Find the content area (where chat messages are)
    const contentArea = document.querySelector('.slide-menu-wrapper .content');
    if (!contentArea) {
      console.log('[KTalk Transcript] Content area not found');
      return null;
    }

    // Store reference to original chat content
    originalChatContent = contentArea.firstElementChild;

    // Create transcript content element
    transcriptContentElement = document.createElement('div');
    transcriptContentElement.className = 'ktalk-transcript-content';
    transcriptContentElement.innerHTML = `
      <div class="ktalk-transcript-speaker-bar">
        <div class="ktalk-speaker-dot"></div>
        <span class="ktalk-speaker-name">No one speaking</span>
      </div>
      <div class="ktalk-transcript-toolbar">
        <div class="ktalk-transcript-toolbar-left">
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-top" title="Scroll to top">↑ Top</button>
          <button class="ktalk-transcript-scroll-btn ktalk-scroll-bottom" title="Scroll to bottom">↓ Bottom</button>
        </div>
        <button class="ktalk-transcript-copy-btn" title="Copy all transcript">
          <span>📋</span>
          <span class="ktalk-copy-text">Copy all</span>
        </button>
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

    // Add button handlers
    transcriptContentElement.querySelector('.ktalk-transcript-copy-btn').addEventListener('click', copyAllTranscript);
    transcriptContentElement.querySelector('.ktalk-scroll-top').addEventListener('click', () => {
      const container = document.querySelector('.ktalk-transcript-messages');
      if (container) container.scrollTop = 0;
    });
    transcriptContentElement.querySelector('.ktalk-scroll-bottom').addEventListener('click', () => {
      const container = document.querySelector('.ktalk-transcript-messages');
      if (container) container.scrollTop = container.scrollHeight;
    });

    contentArea.appendChild(transcriptContentElement);
    console.log('[KTalk Transcript] Content element created');

    return transcriptContentElement;
  }

  // Activate transcript tab (deactivate others)
  function activateTranscriptTab() {
    const transcriptTabBtn = document.querySelector('.ktalk-transcript-tab');
    if (!transcriptTabBtn) return;

    // Deactivate other tabs
    document.querySelectorAll('.tl-tabset__content .tl-tab').forEach(tab => {
      if (tab.classList.contains('ktalk-transcript-tab-wrapper')) return;
      const btn = tab.querySelector('button.link, button._active, .link.tab-content');
      if (btn) {
        btn.classList.remove('_active');
        const underline = btn.querySelector('.underline') || tab.querySelector('.underline');
        if (underline) underline.style.opacity = '0';
      }
    });

    // Activate transcript tab
    transcriptTabBtn.classList.add('_active');

    // Hide original content, show transcript
    if (originalChatContent) {
      originalChatContent.style.display = 'none';
    }
    if (transcriptContentElement) {
      transcriptContentElement.classList.add('active');
    }

    // Update selector title
    const selector = document.querySelector('.tl-tabset__selector .title');
    if (selector) {
      selector.textContent = 'Transcript';
    }

    isTranscriptActive = true;
    updateTranscriptUI();
  }

  // Deactivate transcript tab (when user clicks General/Private)
  function deactivateTranscriptTab() {
    const transcriptTab = document.querySelector('.ktalk-transcript-tab');
    if (transcriptTab) {
      transcriptTab.classList.remove('_active');
    }

    // Show original content, hide transcript
    if (originalChatContent) {
      originalChatContent.style.display = '';
    }
    if (transcriptContentElement) {
      transcriptContentElement.classList.remove('active');
    }

    isTranscriptActive = false;
  }

  // Watch for tab changes (user clicking General/Private)
  function watchTabChanges() {
    const tabsContainer = document.querySelector('.tl-tabset__content');
    if (!tabsContainer) return;

    tabsContainer.addEventListener('click', (e) => {
      const clickedTab = e.target.closest('.tl-tab');
      if (!clickedTab) return;

      const isTranscriptTab = clickedTab.querySelector('.ktalk-transcript-tab');
      if (!isTranscriptTab) {
        // User clicked a different tab
        deactivateTranscriptTab();
      }
    });
  }

  function updateSpeakerStatus(conf) {
    const activeSpeaker = getActiveSpeaker(conf);
    const hasMicsOn = anyMicOn(conf);

    const dot = document.querySelector('.ktalk-speaker-dot');
    const nameEl = document.querySelector('.ktalk-speaker-name');

    // Debug logging when "All mics off" would be shown (only after conference is ready)
    if (!activeSpeaker && !hasMicsOn && conferenceReady) {
      const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
      const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
      console.log('[KTalk Debug] All mics off detected:', {
        conferenceReady,
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
        // Only show as speaking if they actually have an unmuted mic
        const speakerMuted = isMicMuted(conf, activeSpeaker.id);
        if (!speakerMuted) {
          dot.classList.add('speaking');
          nameEl.textContent = activeSpeaker.name;
        } else {
          // Speaker has muted mic - treat as no one speaking
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

    // Track speaker changes
    if (activeSpeaker) {
      if (currentSpeaker !== activeSpeaker.id) {
        if (currentSpeaker && speakingStartTime) {
          const prevName = getParticipantName(conf, currentSpeaker);
          addToHistory(prevName, currentSpeaker, Date.now() - speakingStartTime);
        }
        currentSpeaker = activeSpeaker.id;
        speakingStartTime = Date.now();
      }
    } else {
      if (currentSpeaker && speakingStartTime) {
        const prevName = getParticipantName(conf, currentSpeaker);
        addToHistory(prevName, currentSpeaker, Date.now() - speakingStartTime);
        currentSpeaker = null;
        speakingStartTime = null;
      }
    }

    sendUpdate({
      currentSpeaker: activeSpeaker,
      speakerHistory,
      participants: getParticipantsList(conf)
    });

    sendWsSpeakerUpdate(activeSpeaker);
  }

  function addToHistory(name, speakerId, durationMs) {
    if (!name || durationMs < SPEAKING_THRESHOLD_MS) return;

    speakerHistory.unshift({
      name, id: speakerId,
      time: new Date().toLocaleTimeString('ru-RU', {
        hour: '2-digit', minute: '2-digit', second: '2-digit'
      }),
      durationMs
    });

    if (speakerHistory.length > MAX_HISTORY) speakerHistory.pop();
  }

  function setupUI() {
    injectStyles();

    // Try to add tab and content
    const tabAdded = addTranscriptTab();
    if (tabAdded) {
      createTranscriptContent();
      watchTabChanges();
      updateWsIndicator(wsConnected);
    }

    return tabAdded;
  }

  function subscribeToDominantSpeaker(conf) {
    sendConnectionStatus(true);
    sendWsParticipants(conf);

    // Reset conference ready state on new conference
    conferenceReady = false;
    if (conferenceReadyTimeout) clearTimeout(conferenceReadyTimeout);

    // Mark conference as ready after a delay to allow tracks to load
    // Or immediately if we already have tracks
    const checkAndMarkReady = () => {
      const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
      const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
      const hasAnyTrack = jitsiParticipants.some(jp => {
        const tracks = jp.getTracks?.() || jp._tracks || [];
        return tracks.some(t => (t.getType?.() || t.type) === 'audio');
      }) || localTracks.some(t => t.getType?.() === 'audio');

      if (hasAnyTrack) {
        conferenceReady = true;
        console.log('[KTalk Transcript] Conference ready - tracks loaded');
      }
    };

    // Check immediately and also after delay
    checkAndMarkReady();
    conferenceReadyTimeout = setTimeout(() => {
      conferenceReady = true; // Force ready after 5 seconds
      console.log('[KTalk Transcript] Conference ready - timeout');
      updateSpeakerStatus(conf);
    }, 5000);

    console.log('[KTalk Transcript] Subscribing to speaker events');
    updateSpeakerStatus(conf);

    // Subscribe to all _activeSpeaker$ observables
    conf._participantsMap?.forEach((p, key) => {
      if (p._activeSpeaker$?.subscribe) {
        p._activeSpeaker$.subscribe(() => updateSpeakerStatus(conf));
      }
    });

    // Subscribe to _dominantSpeakerId$ observable
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
        // Track added - mark conference as ready
        checkAndMarkReady();
        updateSpeakerStatus(conf);
        sendWsParticipants(conf);  // Update mic status
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
        sendWsParticipants(conf);  // Update mic status
      }
    });

    conf.jitsiConf.on('conference.trackMuteChanged', (track) => {
      if (track.getType?.() === 'audio') {
        updateSpeakerStatus(conf);
        sendWsParticipants(conf);  // Update mic muted status
      }
    });

    conf.jitsiConf.on('conference.userJoined', (id) => {
      setTimeout(() => {
        const p = conf._participantsMap?.get(id);
        if (p?._activeSpeaker$?.subscribe) {
          p._activeSpeaker$.subscribe(() => updateSpeakerStatus(conf));
        }
        updateSpeakerStatus(conf);
        sendWsParticipants(conf);
      }, 1000);
    });

    conf.jitsiConf.on('conference.userLeft', () => {
      setTimeout(() => {
        updateSpeakerStatus(conf);
        sendWsParticipants(conf);
      }, 500);
    });

    console.log('[KTalk Transcript] Ready');
  }

  // Main initialization
  console.log('[KTalk Transcript] Script loaded');

  setTimeout(connectWebSocket, 1000);

  // Periodically try to set up UI (KTalk rebuilds on join)
  setInterval(() => {
    if (!document.querySelector('.ktalk-transcript-tab')) {
      transcriptTabAdded = false;
      setupUI();
    }
    // Also check if content element was removed
    if (!document.querySelector('.ktalk-transcript-content')) {
      transcriptContentElement = null;
      originalChatContent = null;
      createTranscriptContent();
    }
  }, 2000);

  waitForConference(subscribeToDominantSpeaker);

  // Send meeting_leave when page is closed
  window.addEventListener('beforeunload', () => {
    if (currentMeetingUrl) {
      sendMeetingLeave('page_closed');
    }
  });
})();
