// KTalk Speaker Detector - Injected Script
// Uses _activeSpeaker$ for real speech detection
// WebSocket integration with recog for live transcription

(function() {
  'use strict';

  let overlayElement = null;
  let speakerHistory = [];
  const MAX_HISTORY = 100;
  const SPEAKING_THRESHOLD_MS = 500;

  let currentSpeaker = null;
  let speakingStartTime = null;

  // Transcript storage
  let transcriptLines = [];
  const MAX_TRANSCRIPT_LINES = 50;

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
          // Immediately send current active speaker
          const activeSpeaker = getActiveSpeaker(window.$conf);
          sendWsSpeakerUpdate(activeSpeaker);
          console.log('[KTalk Speaker Detector] Sent initial speaker:', activeSpeaker?.name || 'no one');
        }
      };

      wsConnection.onclose = () => {
        console.log('[KTalk Speaker Detector] WebSocket disconnected');
        wsConnected = false;
        updateWsIndicator(false);

        // Reconnect after 5 seconds
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
      speaker: speaker,  // {name, id, isLocal} or null
      timestamp: Date.now()  // Миллисекунды с epoch
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
    if (!overlayElement) return;
    let indicator = overlayElement.querySelector('.ktalk-ws-indicator');
    if (!indicator) {
      // Create indicator
      const header = overlayElement.querySelector('.ktalk-speaker-header');
      if (header) {
        indicator = document.createElement('span');
        indicator.className = 'ktalk-ws-indicator';
        indicator.title = 'recog connection';
        header.insertBefore(indicator, header.querySelector('.ktalk-speaker-minimize'));
      }
    }
    if (indicator) {
      indicator.classList.toggle('connected', connected);
    }
  }

  function handleTranscript(data) {
    const { speaker, text, timestamp, is_continuation, is_local, uncertain } = data;

    if (is_continuation && transcriptLines.length > 0) {
      // Append to last line
      transcriptLines[transcriptLines.length - 1].text += ' ' + text;
    } else {
      // New line
      transcriptLines.push({
        speaker,
        text,
        timestamp,
        is_local: is_local || false,  // true = микрофон (я), false = другие
        uncertain: uncertain || false,  // true = под вопросом кто говорит
        time: new Date().toLocaleTimeString('ru-RU', {
          hour: '2-digit', minute: '2-digit', second: '2-digit'
        })
      });

      // Keep max lines
      if (transcriptLines.length > MAX_TRANSCRIPT_LINES) {
        transcriptLines.shift();
      }
    }

    updateTranscriptUI();
  }

  function updateTranscriptUI() {
    if (!overlayElement) return;

    const container = overlayElement.querySelector('.ktalk-transcript-content');
    if (!container) return;

    container.innerHTML = transcriptLines.map(line => {
      const classes = [
        'ktalk-transcript-line',
        line.is_local ? 'local' : 'remote',
        line.uncertain ? 'uncertain' : ''
      ].filter(Boolean).join(' ');

      const speakerDisplay = line.uncertain ? `${line.speaker} (?)` : `${line.speaker}`;

      return `<div class="${classes}">
        <span class="ktalk-transcript-time">[${line.time}]</span>
        <span class="ktalk-transcript-speaker">${speakerDisplay}:</span>
        <span class="ktalk-transcript-text">${line.text}</span>
      </div>`;
    }).join('');

    // Auto-scroll to bottom
    container.scrollTop = container.scrollHeight;
  }

  // Connect to WebSocket on load
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
    // Сначала проверяем _userInfo$ - там firstName + lastName для авторизованных пользователей
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

    // Проверяем displayName с пробелом (может быть полное имя)
    if (participant?.displayName?.includes(' ')) {
      return participant.displayName;
    }
    if (participant?.name?.includes(' ')) {
      return participant.name;
    }

    // Проверяем info (_participantInfo$)
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

  // Get participant who is actively speaking (activeSpeaker = true)
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

  // Check if participant's mic is muted
  function isMicMuted(conf, participantId) {
    // Check via jitsi participants
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    const jitsiParticipant = jitsiParticipants.find(p => p._id === participantId);
    if (jitsiParticipant) {
      const tracks = jitsiParticipant.getTracks?.() || jitsiParticipant._tracks || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack) {
        return audioTrack.isMuted?.() === true;
      }
    }

    // Check local
    if (conf.localParticipant?.id === participantId) {
      const localTracks = conf.jitsiConf?.getLocalTracks?.() || [];
      const localAudio = localTracks.find(t => t.getType?.() === 'audio');
      if (localAudio) {
        return localAudio.isMuted?.() === true;
      }
      // No local audio track = mic off
      return true;
    }

    // No track found = mic off
    return true;
  }

  // Check if anyone has mic on (unmuted)
  function anyMicOn(conf) {
    // Check jitsi participants
    const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
    for (const jp of jitsiParticipants) {
      const tracks = jp.getTracks?.() || jp._tracks || [];
      const audioTrack = tracks.find(t => (t.getType?.() || t.type) === 'audio');
      if (audioTrack && !audioTrack.isMuted?.()) {
        return true;
      }
    }

    // Check local tracks
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

  function createOverlay() {
    if (overlayElement) return overlayElement;

    overlayElement = document.createElement('div');
    overlayElement.id = 'ktalk-speaker-overlay';
    overlayElement.innerHTML = `
      <div class="ktalk-speaker-header">
        <span class="ktalk-speaker-icon">🎤</span>
        <span class="ktalk-speaker-title">Speaker Detector</span>
        <button class="ktalk-speaker-minimize">−</button>
        <button class="ktalk-speaker-close">×</button>
      </div>
      <div class="ktalk-speaker-content">
        <div class="ktalk-speaker-status">
          <div class="ktalk-status-indicator"></div>
          <span class="ktalk-status-text">Waiting...</span>
        </div>
        <div class="ktalk-speaker-name">-</div>
        <div class="ktalk-speaker-duration"></div>

        <div class="ktalk-transcript-section">
          <div class="ktalk-transcript-header">
            <span class="ktalk-transcript-title">📝 Transcript</span>
            <button class="ktalk-transcript-toggle">▼</button>
          </div>
          <div class="ktalk-transcript-content"></div>
        </div>

        <div class="ktalk-speaker-history">
          <div class="ktalk-history-title">Recent speakers:</div>
          <ul class="ktalk-history-list"></ul>
        </div>
      </div>
    `;

    const styles = document.createElement('style');
    styles.textContent = `
      #ktalk-speaker-overlay {
        position: fixed;
        top: 20px;
        right: 20px;
        background: rgba(0, 0, 0, 0.92);
        color: white;
        padding: 0;
        border-radius: 12px;
        z-index: 999999;
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
        min-width: 280px;
        box-shadow: 0 4px 24px rgba(0,0,0,0.4);
        overflow: hidden;
        user-select: none;
      }

      #ktalk-speaker-overlay.minimized .ktalk-speaker-content {
        display: none;
      }

      #ktalk-speaker-overlay.minimized {
        min-width: auto;
      }

      .ktalk-speaker-header {
        display: flex;
        align-items: center;
        padding: 10px 15px;
        background: rgba(255,255,255,0.08);
        border-bottom: 1px solid rgba(255,255,255,0.1);
        cursor: move;
      }

      .ktalk-speaker-icon { font-size: 16px; margin-right: 8px; }

      .ktalk-speaker-title {
        flex: 1;
        font-size: 11px;
        text-transform: uppercase;
        letter-spacing: 1px;
        opacity: 0.6;
      }

      .ktalk-speaker-minimize,
      .ktalk-speaker-close {
        background: none;
        border: none;
        color: white;
        font-size: 18px;
        cursor: pointer;
        opacity: 0.6;
        padding: 0 5px;
        line-height: 1;
      }

      .ktalk-speaker-minimize:hover,
      .ktalk-speaker-close:hover { opacity: 1; }

      .ktalk-ws-indicator {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: #ef4444;
        margin-right: 8px;
        transition: background 0.3s;
      }

      .ktalk-ws-indicator.connected {
        background: #22c55e;
      }

      .ktalk-speaker-content { padding: 15px; }

      .ktalk-speaker-status {
        display: flex;
        align-items: center;
        gap: 8px;
        margin-bottom: 10px;
      }

      .ktalk-status-indicator {
        width: 10px;
        height: 10px;
        border-radius: 50%;
        background: #6b7280;
        transition: all 0.3s;
      }

      .ktalk-status-indicator.speaking {
        background: #4ade80;
        box-shadow: 0 0 10px #4ade80;
        animation: pulse-green 1.5s infinite;
      }

      .ktalk-status-indicator.silent {
        background: #6b7280;
      }

      @keyframes pulse-green {
        0%, 100% { box-shadow: 0 0 10px #4ade80; }
        50% { box-shadow: 0 0 20px #4ade80, 0 0 30px #4ade80; }
      }

      .ktalk-status-text {
        font-size: 12px;
        opacity: 0.7;
        text-transform: uppercase;
        letter-spacing: 0.5px;
      }

      .ktalk-speaker-name {
        font-size: 24px;
        font-weight: 600;
        text-align: center;
        padding: 15px;
        background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
        border-radius: 8px;
        margin-bottom: 8px;
        transition: background 0.3s;
      }

      .ktalk-speaker-name.silent {
        background: linear-gradient(135deg, #4b5563 0%, #374151 100%);
      }

      .ktalk-speaker-duration {
        text-align: center;
        font-size: 12px;
        opacity: 0.5;
        margin-bottom: 15px;
        min-height: 18px;
      }

      .ktalk-speaker-history {
        border-top: 1px solid rgba(255,255,255,0.1);
        padding-top: 12px;
        max-height: 180px;
        overflow-y: auto;
      }

      .ktalk-history-title {
        font-size: 10px;
        text-transform: uppercase;
        letter-spacing: 1px;
        opacity: 0.4;
        margin-bottom: 10px;
      }

      .ktalk-history-list {
        list-style: none;
        padding: 0;
        margin: 0;
      }

      .ktalk-history-list li {
        padding: 6px 8px;
        font-size: 13px;
        background: rgba(255,255,255,0.05);
        border-radius: 4px;
        margin-bottom: 4px;
        display: flex;
        justify-content: space-between;
        align-items: center;
      }

      .ktalk-history-name { font-weight: 500; }

      .ktalk-history-meta {
        display: flex;
        gap: 10px;
        font-size: 11px;
        opacity: 0.5;
      }

      /* Transcript section */
      .ktalk-transcript-section {
        border-top: 1px solid rgba(255,255,255,0.1);
        margin-top: 12px;
        padding-top: 12px;
      }

      .ktalk-transcript-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 8px;
      }

      .ktalk-transcript-title {
        font-size: 10px;
        text-transform: uppercase;
        letter-spacing: 1px;
        opacity: 0.6;
      }

      .ktalk-transcript-toggle {
        background: none;
        border: none;
        color: white;
        opacity: 0.5;
        cursor: pointer;
        font-size: 10px;
        padding: 2px 6px;
      }

      .ktalk-transcript-toggle:hover { opacity: 1; }

      .ktalk-transcript-content {
        max-height: 200px;
        overflow-y: auto;
        font-size: 12px;
        line-height: 1.5;
        background: rgba(0,0,0,0.3);
        border-radius: 6px;
        padding: 8px;
      }

      .ktalk-transcript-section.collapsed .ktalk-transcript-content {
        display: none;
      }

      .ktalk-transcript-section.collapsed .ktalk-transcript-toggle {
        transform: rotate(-90deg);
      }

      .ktalk-transcript-line {
        margin-bottom: 6px;
        padding-bottom: 6px;
        border-bottom: 1px solid rgba(255,255,255,0.05);
      }

      .ktalk-transcript-line:last-child {
        margin-bottom: 0;
        padding-bottom: 0;
        border-bottom: none;
      }

      .ktalk-transcript-time {
        color: #888;
        font-size: 10px;
        margin-right: 6px;
      }

      .ktalk-transcript-speaker {
        font-weight: 600;
        margin-right: 6px;
      }

      /* Локальные реплики (я/микрофон) - синий */
      .ktalk-transcript-line.local .ktalk-transcript-speaker {
        color: #60a5fa;
      }
      .ktalk-transcript-line.local {
        background: rgba(96, 165, 250, 0.1);
        border-left: 2px solid #60a5fa;
        padding-left: 6px;
        margin-left: -6px;
      }

      /* Удалённые реплики (другие/системный звук) - зелёный */
      .ktalk-transcript-line.remote .ktalk-transcript-speaker {
        color: #4ade80;
      }
      .ktalk-transcript-line.remote {
        background: rgba(74, 222, 128, 0.05);
      }

      /* Неопределённые реплики (под вопросом кто говорит) - жёлтый */
      .ktalk-transcript-line.uncertain {
        background: rgba(250, 204, 21, 0.1);
        border-left: 2px dashed #facc15;
        padding-left: 6px;
        margin-left: -6px;
      }
      .ktalk-transcript-line.uncertain .ktalk-transcript-speaker {
        color: #facc15;
      }

      .ktalk-transcript-text {
        color: #e5e5e5;
      }

      .ktalk-transcript-content:empty::before {
        content: 'Waiting for transcript...';
        color: #666;
        font-style: italic;
      }
    `;

    document.head.appendChild(styles);
    document.body.appendChild(overlayElement);

    overlayElement.querySelector('.ktalk-speaker-close').onclick = () => {
      overlayElement.style.display = 'none';
    };

    overlayElement.querySelector('.ktalk-speaker-minimize').onclick = () => {
      overlayElement.classList.toggle('minimized');
    };

    // Toggle transcript section
    const transcriptToggle = overlayElement.querySelector('.ktalk-transcript-toggle');
    if (transcriptToggle) {
      transcriptToggle.onclick = () => {
        const section = overlayElement.querySelector('.ktalk-transcript-section');
        section.classList.toggle('collapsed');
      };
    }

    makeDraggable(overlayElement);
    return overlayElement;
  }

  function makeDraggable(el) {
    const header = el.querySelector('.ktalk-speaker-header');
    let dragging = false, startX, startY, initX, initY;

    header.onmousedown = (e) => {
      if (e.target.tagName === 'BUTTON') return;
      dragging = true;
      startX = e.clientX;
      startY = e.clientY;
      const rect = el.getBoundingClientRect();
      initX = rect.left;
      initY = rect.top;
      e.preventDefault();
    };

    document.onmousemove = (e) => {
      if (!dragging) return;
      el.style.left = (initX + e.clientX - startX) + 'px';
      el.style.top = (initY + e.clientY - startY) + 'px';
      el.style.right = 'auto';
    };

    document.onmouseup = () => { dragging = false; };
  }

  function formatDuration(ms) {
    if (ms < 1000) return '<1s';
    const s = Math.floor(ms / 1000);
    if (s < 60) return `${s}s`;
    return `${Math.floor(s / 60)}m ${s % 60}s`;
  }

  function addToHistory(name, speakerId, durationMs) {
    if (!name || durationMs < SPEAKING_THRESHOLD_MS) return;

    const timeStr = new Date().toLocaleTimeString('ru-RU', {
      hour: '2-digit', minute: '2-digit', second: '2-digit'
    });

    speakerHistory.unshift({
      name, id: speakerId, time: timeStr,
      duration: formatDuration(durationMs), durationMs
    });

    if (speakerHistory.length > MAX_HISTORY) speakerHistory.pop();
    updateHistoryUI();
  }

  function updateHistoryUI() {
    if (!overlayElement) return;
    const list = overlayElement.querySelector('.ktalk-history-list');
    list.innerHTML = speakerHistory.slice(0, 15).map(h =>
      `<li>
        <span class="ktalk-history-name">${h.name}</span>
        <span class="ktalk-history-meta">
          <span>${h.duration}</span>
          <span>${h.time}</span>
        </span>
      </li>`
    ).join('');
  }

  function updateOverlay(conf) {
    if (!overlayElement) return;

    const nameEl = overlayElement.querySelector('.ktalk-speaker-name');
    const indicator = overlayElement.querySelector('.ktalk-status-indicator');
    const statusText = overlayElement.querySelector('.ktalk-status-text');
    const durationEl = overlayElement.querySelector('.ktalk-speaker-duration');

    // Get who is actively speaking
    const activeSpeaker = getActiveSpeaker(conf);
    const hasMicsOn = anyMicOn(conf);

    if (activeSpeaker) {
      // Someone is speaking!
      nameEl.textContent = activeSpeaker.name;
      nameEl.classList.remove('silent');
      indicator.className = 'ktalk-status-indicator speaking';
      statusText.textContent = 'Speaking';

      if (currentSpeaker !== activeSpeaker.id) {
        // New speaker - save previous
        if (currentSpeaker && speakingStartTime) {
          const prevName = getParticipantName(conf, currentSpeaker);
          addToHistory(prevName, currentSpeaker, Date.now() - speakingStartTime);
        }
        currentSpeaker = activeSpeaker.id;
        speakingStartTime = Date.now();
      }

      const elapsed = Date.now() - speakingStartTime;
      durationEl.textContent = `Speaking for ${formatDuration(elapsed)}`;
    } else {
      // No one speaking
      indicator.className = 'ktalk-status-indicator silent';
      nameEl.classList.add('silent');

      if (!hasMicsOn) {
        statusText.textContent = 'All mics off';
        nameEl.textContent = 'No one';
      } else {
        statusText.textContent = 'Silence';
        // Show last dominant speaker
        const dominantId = conf._dominantSpeakerId$?.value;
        const dominantName = getParticipantName(conf, dominantId);
        nameEl.textContent = dominantName || 'No one';
      }

      durationEl.textContent = '';

      // Save previous speaker to history
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

    // Send to recog via WebSocket
    sendWsSpeakerUpdate(activeSpeaker);
  }

  function subscribeToDominantSpeaker(conf) {
    createOverlay();
    sendConnectionStatus(true);

    // Update WS indicator and send initial state
    updateWsIndicator(wsConnected);
    sendWsParticipants(conf);

    console.log('[KTalk Speaker Detector] Initial update');
    updateOverlay(conf);

    // Subscribe to all _activeSpeaker$ observables
    conf._participantsMap?.forEach((p, key) => {
      if (p._activeSpeaker$?.subscribe) {
        p._activeSpeaker$.subscribe((isActive) => {
          const info = p._participantInfo$?.value || {};
          console.log('[KTalk Speaker Detector] activeSpeaker changed:', info.name || key, isActive);
          updateOverlay(conf);
        });
      }
    });

    // Subscribe to dominant speaker events
    conf.jitsiConf.on('conference.dominantSpeaker', () => {
      updateOverlay(conf);
    });

    // Track events for mic on/off
    conf.jitsiConf.on('conference.trackAdded', (track) => {
      if (track.getType?.() === 'audio') {
        console.log('[KTalk Speaker Detector] Audio track added');
        updateOverlay(conf);

        // Subscribe to new participant's activeSpeaker
        const participantId = track.getParticipantId?.();
        if (participantId) {
          const p = conf._participantsMap?.get(participantId);
          if (p?._activeSpeaker$?.subscribe) {
            p._activeSpeaker$.subscribe(() => updateOverlay(conf));
          }
        }
      }
    });

    conf.jitsiConf.on('conference.trackRemoved', (track) => {
      if (track.getType?.() === 'audio') {
        console.log('[KTalk Speaker Detector] Audio track removed');
        updateOverlay(conf);
      }
    });

    conf.jitsiConf.on('conference.trackMuteChanged', (track) => {
      if (track.getType?.() === 'audio') {
        console.log('[KTalk Speaker Detector] Audio mute changed:', track.isMuted?.());
        updateOverlay(conf);
      }
    });

    // User joined - subscribe to their activeSpeaker
    conf.jitsiConf.on('conference.userJoined', (id) => {
      setTimeout(() => {
        const p = conf._participantsMap?.get(id);
        if (p?._activeSpeaker$?.subscribe) {
          p._activeSpeaker$.subscribe(() => updateOverlay(conf));
        }
        updateOverlay(conf);
        sendWsParticipants(conf);  // Update participants list
      }, 1000);
    });

    // User left - update participants
    conf.jitsiConf.on('conference.userLeft', () => {
      setTimeout(() => {
        updateOverlay(conf);
        sendWsParticipants(conf);
      }, 500);
    });

    // Update duration every second
    setInterval(() => {
      if (currentSpeaker && speakingStartTime && overlayElement) {
        const durationEl = overlayElement.querySelector('.ktalk-speaker-duration');
        durationEl.textContent = `Speaking for ${formatDuration(Date.now() - speakingStartTime)}`;
      }
    }, 1000);

    console.log('[KTalk Speaker Detector] Subscribed to all events');
  }

  console.log('[KTalk Speaker Detector] Script loaded, waiting for conference...');
  waitForConference(subscribeToDominantSpeaker);
})();
