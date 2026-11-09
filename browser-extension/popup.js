// KTalk Speaker Detector - Popup Script

let transcriptLines = [];

// Participant names, times and transcript text come from the conference and the
// recognizer: they are inserted as text, never as markup.
function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text == null ? '' : String(text);
  return div.innerHTML;
}

function getInitials(name) {
  if (!name) return '?';
  return name.split(' ').map(w => w[0]).join('').toUpperCase().substring(0, 2);
}

function getUncertainSuffix(line) {
  if (!line.uncertain) return "";
  if (line.uncertain_reason === "mics_off") return " (🔇?)";
  return " (?)";
}

function updateSpeakerStatus(data) {
  const statusDot = document.getElementById('statusDot');
  const statusText = document.getElementById('statusText');
  const currentSpeakerName = document.getElementById('currentSpeakerName');
  const currentSpeakerId = document.getElementById('currentSpeakerId');

  if (data.currentSpeaker) {
    statusDot.classList.add('connected');
    statusText.textContent = 'Connected to conference';
    currentSpeakerName.textContent = data.currentSpeaker.name || 'No one';
    currentSpeakerId.textContent = data.currentSpeaker.id || '';
  } else if (data.connected) {
    statusDot.classList.add('connected');
    statusText.textContent = 'Connected (no one speaking)';
    currentSpeakerName.textContent = '-';
    currentSpeakerId.textContent = '';
  } else {
    statusDot.classList.remove('connected');
    statusText.textContent = 'Not in a conference';
    currentSpeakerName.textContent = '-';
    currentSpeakerId.textContent = '';
  }
}

function updateParticipants(participants) {
  const participantsList = document.getElementById('participantsList');
  const participantCount = document.getElementById('participantCount');

  if (participants && participants.length > 0) {
    participantCount.textContent = participants.length;
    participantsList.innerHTML = participants.map(p => `
      <li>
        <div class="participant-name">
          <div class="participant-avatar">${escapeHtml(getInitials(p.name))}</div>
          <span>${escapeHtml(p.name)}</span>
        </div>
        ${p.isLocal ? '<span class="participant-local">You</span>' : ''}
      </li>
    `).join('');
  } else {
    participantCount.textContent = '0';
    participantsList.innerHTML = '<li class="empty-state">No participants</li>';
  }
}

function updateHistory(speakerHistory) {
  const historyList = document.getElementById('historyList');

  if (speakerHistory && speakerHistory.length > 0) {
    historyList.innerHTML = speakerHistory.slice(0, 10).map(h => `
      <li>
        <span>${escapeHtml(h.name)}</span>
        <span class="history-time">${escapeHtml(h.time)}</span>
      </li>
    `).join('');
  } else {
    historyList.innerHTML = '<li class="empty-state">No history yet</li>';
  }
}

function updateUI(data) {
  // Update current speaker
  updateSpeakerStatus(data);

  // Update participants
  updateParticipants(data.participants);

  // Update history
  updateHistory(data.speakerHistory);

  // Update transcript
  if (data.transcriptLines) {
    transcriptLines = data.transcriptLines;
  }
  updateTranscriptUI();
}

function updateTranscriptUI() {
  const transcriptList = document.getElementById('transcriptList');
  const transcriptCount = document.getElementById('transcriptCount');

  if (transcriptLines.length > 0) {
    transcriptCount.textContent = transcriptLines.length;
    transcriptList.innerHTML = transcriptLines.map(line => {
      const classes = [
        line.is_local ? 'local' : '',
        line.uncertain ? 'uncertain' : '',
        line.uncertain_reason === 'mics_off' ? 'mics-off' : ''
      ].filter(Boolean).join(' ');

      const speakerDisplay = line.speaker + getUncertainSuffix(line);

      return `<li class="${classes}">
        <span class="transcript-speaker">${escapeHtml(speakerDisplay)}</span>
        <span class="transcript-time">${escapeHtml(line.time)}</span>
        <div class="transcript-text">${escapeHtml(line.text)}</div>
      </li>`;
    }).join('');

    // Auto-scroll to bottom
    transcriptList.scrollTop = transcriptList.scrollHeight;
  } else {
    transcriptCount.textContent = '0';
    transcriptList.innerHTML = '<li class="empty-state">No transcript yet</li>';
  }
}

function copyAllTranscript() {
  if (transcriptLines.length === 0) return;

  const text = transcriptLines.map(line => {
    const speaker = line.speaker + getUncertainSuffix(line);
    return `**${speaker}** *[${line.time}]*: ${line.text}`;
  }).join('\n\n');

  navigator.clipboard.writeText(text).then(() => {
    const btn = document.getElementById('copyTranscriptBtn');
    const textSpan = btn.querySelector('.copy-text');
    btn.classList.add('copied');
    textSpan.textContent = 'Copied!';
    setTimeout(() => {
      btn.classList.remove('copied');
      textSpan.textContent = 'Copy all';
    }, 2000);
  });
}

// Tab switching
function setupTabs() {
  const tabBtns = document.querySelectorAll('.tab-btn');
  tabBtns.forEach(btn => {
    btn.addEventListener('click', () => {
      const tabId = btn.dataset.tab;

      // Update buttons
      tabBtns.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');

      // Update content
      document.querySelectorAll('.tab-content').forEach(c => c.classList.remove('active'));
      document.getElementById('tab-' + tabId).classList.add('active');
    });
  });
}

// Load data from storage
function loadData() {
  chrome.storage.local.get(['currentSpeaker', 'speakerHistory', 'participants', 'connected', 'transcriptLines'], (data) => {
    updateUI(data);
  });
}

// Listen for storage changes
chrome.storage.onChanged.addListener((changes, namespace) => {
  if (namespace === 'local') {
    loadData();
  }
});

// Initial setup
document.addEventListener('DOMContentLoaded', () => {
  setupTabs();
  loadData();

  // Copy button handler
  document.getElementById('copyTranscriptBtn').addEventListener('click', copyAllTranscript);
});

// Refresh every second
setInterval(loadData, 1000);
