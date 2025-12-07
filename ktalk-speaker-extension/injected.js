// KTalk Speaker Detector - Injected Script
// Integrates transcript panel into KTalk side menu
// WebSocket integration with recog for live transcription

(function() {
  'use strict';

  let panelElement = null;
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

  function connectWebSocket() {
    if (wsConnection && wsConnection.readyState === WebSocket.OPEN) {
      return;
    }

    try {
      wsConnection = new WebSocket(WS_URL);

      wsConnection.onopen = () => {
        console.log('[KTalk Speaker Detector] WebSocket connected to recog');
        wsConnected = true;
        updateWsIndicator(true);

        // Send current state including active speaker
        if (window.$conf) {
          sendWsParticipants(window.$conf);
          const activeSpeaker = getActiveSpeaker(window.$conf);
          sendWsSpeakerUpdate(activeSpeaker);
          console.log('[KTalk Speaker Detector] Sent initial speaker:', activeSpeaker?.name || 'no one');
        }
      };

      wsConnection.onclose = () => {
        console.log('[KTalk Speaker Detector] WebSocket disconnected');
        wsConnected = false;
        updateWsIndicator(false);

        if (!wsReconnectTimer) {
          wsReconnectTimer = setTimeout(() => {
            wsReconnectTimer = null;
            connectWebSocket();
          }, 5000);
        }
      };

      wsConnection.onerror = (err) => {
        console.log('[KTalk Speaker Detector] WebSocket error (recog server may not be running)');
        wsConnected = false;
      };

      wsConnection.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          if (data.type === 'transcript') {
            handleTranscript(data);
          }
        } catch (e) {
          console.log('[KTalk Speaker Detector] Error parsing message:', e);
        }
      };

    } catch (e) {
      console.log('[KTalk Speaker Detector] WebSocket connection failed:', e);
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

  function updateWsIndicator(connected) {
    const indicator = document.querySelector('.ktalk-transcript-ws-indicator');
    if (indicator) {
      indicator.classList.toggle('connected', connected);
      indicator.title = connected ? 'recog connected' : 'recog disconnected';
    }
  }

  function handleTranscript(data) {
    const { speaker, text, timestamp, is_continuation, is_local, uncertain } = data;

    if (is_continuation && transcriptLines.length > 0) {
      transcriptLines[transcriptLines.length - 1].text += ' ' + text;
    } else {
      transcriptLines.push({
        speaker,
        text,
        timestamp,
        is_local: is_local || false,
        uncertain: uncertain || false,
        time: new Date().toLocaleTimeString('ru-RU', {
          hour: '2-digit', minute: '2-digit', second: '2-digit'
        })
      });

      if (transcriptLines.length > MAX_TRANSCRIPT_LINES) {
        transcriptLines.shift();
      }
    }

    updateTranscriptUI();
  }

  function updateTranscriptUI() {
    const container = document.querySelector('.ktalk-transcript-messages');
    if (!container) return;

    container.innerHTML = transcriptLines.map(line => {
      const classes = [
        'ktalk-msg',
        line.is_local ? 'local' : 'remote',
        line.uncertain ? 'uncertain' : ''
      ].filter(Boolean).join(' ');

      const speakerDisplay = line.uncertain ? `${line.speaker} (?)` : line.speaker;

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

  setTimeout(connectWebSocket, 1000);

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
        console.log('[KTalk Speaker Detector] Conference object found!');
        callback(window.$conf);
        return;
      }
      if (attempts < maxAttempts) {
        setTimeout(check, 500);
      } else {
        console.log('[KTalk Speaker Detector] Conference object not found');
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
      if (firstName && lastName) {
        return `${firstName} ${lastName}`;
      }
      if (firstName) return firstName;
      if (lastName) return lastName;
    }

    if (participant?.displayName?.includes(' ')) {
      return participant.displayName;
    }
    if (participant?.name?.includes(' ')) {
      return participant.name;
    }

    if (info) {
      const name = info.name || info.firstName || '';
      const surname = info.surname || info.lastName || '';

      if (info.displayName && info.displayName.includes(' ')) {
        return info.displayName;
      }

      if (name && surname) {
        return `${name} ${surname}`;
      }

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

  function isMicMuted(conf, participantId) {
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const jitsiParticipant = jitsiParticipants.find(p => p._id === participantId);
    if (jitsiParticipant) {
      const tracks = jitsiParticipant.getTracks?.() || jitsiParticipant._tracks || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack) {
        return audioTrack.isMuted?.() === true;
      }
    }

    if (conf.localParticipant?.id === participantId) {
      const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
      const localAudio = localTracks.find(t => t.getType?.() === 'audio');
      if (localAudio) {
        return localAudio.isMuted?.() === true;
      }
      return true;
    }

    return true;
  }

  function anyMicOn(conf) {
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    for (const jp of jitsiParticipants) {
      const tracks = jp.getTracks?.() || jp._tracks || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack && !audioTrack.isMuted?.()) {
        return true;
      }
    }

    const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
    const localAudio = localTracks.find(t => t.getType?.() === 'audio');
    if (localAudio && !localAudio.isMuted?.()) {
      return true;
    }

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

  // Find or create the slide menu container
  function findSlideMenuContainer() {
    return document.querySelector('.conference-slide-menu-container') ||
           document.querySelector('[class*="slide-menu"]') ||
           document.querySelector('[class*="SlideMenu"]');
  }

  // Add transcript tab button to toolbar
  function addTranscriptTabButton() {
    // Check if button already exists
    if (document.querySelector('.ktalk-transcript-btn')) {
      return document.querySelector('.ktalk-transcript-btn');
    }

    // Try various toolbar selectors
    const toolbarSelectors = [
      '.conference-control-buttons',
      '.toolbar',
      '[class*="control-buttons"]',
      '[class*="toolbar"]',
      '.bottom-toolbar',
      '.conference-toolbar'
    ];

    let toolbar = null;
    for (const sel of toolbarSelectors) {
      toolbar = document.querySelector(sel);
      if (toolbar) {
        console.log('[KTalk Speaker Detector] Found toolbar with selector:', sel);
        break;
      }
    }

    const btn = document.createElement('button');
    btn.className = 'ktalk-transcript-btn';
    btn.innerHTML = '📝';
    btn.title = 'Transcript (click to open)';
    btn.onclick = toggleTranscriptPanel;

    if (toolbar) {
      toolbar.appendChild(btn);
      console.log('[KTalk Speaker Detector] Button added to toolbar');
    } else {
      // Fallback: create floating button in corner
      console.log('[KTalk Speaker Detector] No toolbar found, creating floating button');
      btn.classList.add('floating');
      document.body.appendChild(btn);
    }

    return btn;
  }

  // Create the transcript panel
  function createTranscriptPanel() {
    if (panelElement) return panelElement;

    const container = findSlideMenuContainer();

    panelElement = document.createElement('div');
    panelElement.className = 'ktalk-transcript-panel';
    panelElement.innerHTML = `
      <div class="ktalk-transcript-panel-header">
        <div class="ktalk-transcript-panel-title">
          <span>📝 Transcript</span>
          <span class="ktalk-transcript-ws-indicator" title="recog disconnected"></span>
        </div>
        <button class="ktalk-transcript-panel-close">×</button>
      </div>
      <div class="ktalk-transcript-speaker-status">
        <div class="ktalk-speaker-indicator"></div>
        <span class="ktalk-speaker-name">No one speaking</span>
      </div>
      <div class="ktalk-transcript-messages"></div>
    `;

    // Add styles
    const styles = document.createElement('style');
    styles.textContent = `
      .ktalk-transcript-btn {
        width: 40px;
        height: 40px;
        border-radius: 50%;
        border: none;
        background: rgba(255,255,255,0.1);
        color: white;
        font-size: 18px;
        cursor: pointer;
        margin: 0 4px;
        transition: all 0.2s;
      }
      .ktalk-transcript-btn:hover {
        background: rgba(255,255,255,0.2);
      }
      .ktalk-transcript-btn.active {
        background: #4ade80;
      }
      .ktalk-transcript-btn.floating {
        position: fixed;
        bottom: 20px;
        right: 20px;
        z-index: 9999;
        width: 56px;
        height: 56px;
        font-size: 24px;
        box-shadow: 0 4px 12px rgba(0,0,0,0.3);
        background: #3b82f6;
      }
      .ktalk-transcript-btn.floating:hover {
        background: #2563eb;
        transform: scale(1.05);
      }
      .ktalk-transcript-btn.floating.active {
        background: #4ade80;
      }

      .ktalk-transcript-panel {
        position: fixed;
        right: 0;
        top: 0;
        width: 380px;
        height: 100vh;
        background: #1a1a2e;
        color: white;
        display: none;
        flex-direction: column;
        z-index: 1000;
        box-shadow: -4px 0 20px rgba(0,0,0,0.5);
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
      }
      .ktalk-transcript-panel.open {
        display: flex;
      }

      .ktalk-transcript-panel-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 16px 20px;
        background: rgba(255,255,255,0.05);
        border-bottom: 1px solid rgba(255,255,255,0.1);
      }

      .ktalk-transcript-panel-title {
        display: flex;
        align-items: center;
        gap: 10px;
        font-size: 16px;
        font-weight: 600;
      }

      .ktalk-transcript-ws-indicator {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: #ef4444;
        transition: background 0.3s;
      }
      .ktalk-transcript-ws-indicator.connected {
        background: #22c55e;
      }

      .ktalk-transcript-panel-close {
        background: none;
        border: none;
        color: white;
        font-size: 24px;
        cursor: pointer;
        opacity: 0.6;
        padding: 0;
        line-height: 1;
      }
      .ktalk-transcript-panel-close:hover {
        opacity: 1;
      }

      .ktalk-transcript-speaker-status {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 12px 20px;
        background: rgba(255,255,255,0.03);
        border-bottom: 1px solid rgba(255,255,255,0.1);
      }

      .ktalk-speaker-indicator {
        width: 12px;
        height: 12px;
        border-radius: 50%;
        background: #6b7280;
        transition: all 0.3s;
      }
      .ktalk-speaker-indicator.speaking {
        background: #4ade80;
        box-shadow: 0 0 10px #4ade80;
        animation: pulse 1.5s infinite;
      }

      @keyframes pulse {
        0%, 100% { box-shadow: 0 0 10px #4ade80; }
        50% { box-shadow: 0 0 20px #4ade80; }
      }

      .ktalk-speaker-name {
        font-size: 14px;
        font-weight: 500;
      }

      .ktalk-transcript-messages {
        flex: 1;
        overflow-y: auto;
        padding: 12px;
      }

      .ktalk-msg {
        padding: 10px 12px;
        margin-bottom: 8px;
        border-radius: 8px;
        background: rgba(255,255,255,0.05);
      }

      .ktalk-msg-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 4px;
      }

      .ktalk-msg-speaker {
        font-weight: 600;
        font-size: 13px;
      }

      .ktalk-msg-time {
        font-size: 11px;
        color: #888;
      }

      .ktalk-msg-text {
        font-size: 14px;
        line-height: 1.4;
        color: #e5e5e5;
      }

      /* Local messages (me/mic) - blue */
      .ktalk-msg.local {
        background: rgba(96, 165, 250, 0.15);
        border-left: 3px solid #60a5fa;
      }
      .ktalk-msg.local .ktalk-msg-speaker {
        color: #60a5fa;
      }

      /* Remote messages (others) - green */
      .ktalk-msg.remote {
        background: rgba(74, 222, 128, 0.1);
      }
      .ktalk-msg.remote .ktalk-msg-speaker {
        color: #4ade80;
      }

      /* Uncertain messages - yellow */
      .ktalk-msg.uncertain {
        background: rgba(250, 204, 21, 0.1);
        border-left: 3px dashed #facc15;
      }
      .ktalk-msg.uncertain .ktalk-msg-speaker {
        color: #facc15;
      }

      .ktalk-transcript-messages:empty::before {
        content: 'Waiting for transcript...\\A\\AStart recog with:\\Apython mp4_notes.py live --screencapture --ktalk';
        white-space: pre-wrap;
        color: #666;
        font-style: italic;
        display: block;
        text-align: center;
        padding: 40px 20px;
      }
    `;

    document.head.appendChild(styles);
    document.body.appendChild(panelElement);

    // Close button handler
    panelElement.querySelector('.ktalk-transcript-panel-close').onclick = () => {
      toggleTranscriptPanel();
    };

    updateWsIndicator(wsConnected);
    return panelElement;
  }

  function toggleTranscriptPanel() {
    if (!panelElement) {
      createTranscriptPanel();
    }

    const isOpen = panelElement.classList.toggle('open');
    const btn = document.querySelector('.ktalk-transcript-btn');
    if (btn) {
      btn.classList.toggle('active', isOpen);
    }
  }

  function updateSpeakerStatus(conf) {
    const activeSpeaker = getActiveSpeaker(conf);
    const hasMicsOn = anyMicOn(conf);

    const indicator = document.querySelector('.ktalk-speaker-indicator');
    const nameEl = document.querySelector('.ktalk-speaker-name');

    if (!indicator || !nameEl) return;

    if (activeSpeaker) {
      indicator.className = 'ktalk-speaker-indicator speaking';
      nameEl.textContent = activeSpeaker.name;

      if (currentSpeaker !== activeSpeaker.id) {
        if (currentSpeaker && speakingStartTime) {
          const prevName = getParticipantName(conf, currentSpeaker);
          addToHistory(prevName, currentSpeaker, Date.now() - speakingStartTime);
        }
        currentSpeaker = activeSpeaker.id;
        speakingStartTime = Date.now();
      }
    } else {
      indicator.className = 'ktalk-speaker-indicator';

      if (!hasMicsOn) {
        nameEl.textContent = 'All mics off';
      } else {
        const dominantId = conf._dominantSpeakerId$?.value;
        const dominantName = getParticipantName(conf, dominantId);
        nameEl.textContent = dominantName || 'No one speaking';
      }

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

  function subscribeToDominantSpeaker(conf) {
    // Create UI elements
    createTranscriptPanel();
    addTranscriptTabButton();

    sendConnectionStatus(true);
    sendWsParticipants(conf);

    console.log('[KTalk Speaker Detector] Initial update');
    updateSpeakerStatus(conf);

    // Subscribe to all _activeSpeaker$ observables
    conf._participantsMap?.forEach((p, key) => {
      if (p._activeSpeaker$?.subscribe) {
        p._activeSpeaker$.subscribe((isActive) => {
          updateSpeakerStatus(conf);
        });
      }
    });

    conf.jitsiConf.on('conference.dominantSpeaker', () => {
      updateSpeakerStatus(conf);
    });

    conf.jitsiConf.on('conference.trackAdded', (track) => {
      if (track.getType?.() === 'audio') {
        updateSpeakerStatus(conf);
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
      }
    });

    conf.jitsiConf.on('conference.trackMuteChanged', (track) => {
      if (track.getType?.() === 'audio') {
        updateSpeakerStatus(conf);
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

    console.log('[KTalk Speaker Detector] Subscribed to all events');
  }

  console.log('[KTalk Speaker Detector] Script loaded, waiting for conference...');

  // Periodically ensure button exists (KTalk rebuilds UI on join)
  setInterval(() => {
    if (!document.querySelector('.ktalk-transcript-btn')) {
      console.log('[KTalk Speaker Detector] Re-creating button...');
      addTranscriptTabButton();
    }
    if (!document.querySelector('.ktalk-transcript-panel')) {
      createTranscriptPanel();
    }
  }, 2000);

  waitForConference(subscribeToDominantSpeaker);
})();
