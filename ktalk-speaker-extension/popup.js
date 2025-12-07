// KTalk Speaker Detector - Popup Script

function getInitials(name) {
  if (!name) return '?';
  return name.split(' ').map(w => w[0]).join('').toUpperCase().substring(0, 2);
}

function updateUI(data) {
  const statusDot = document.getElementById('statusDot');
  const statusText = document.getElementById('statusText');
  const currentSpeakerName = document.getElementById('currentSpeakerName');
  const currentSpeakerId = document.getElementById('currentSpeakerId');
  const participantsList = document.getElementById('participantsList');
  const participantCount = document.getElementById('participantCount');
  const historyList = document.getElementById('historyList');

  // Update current speaker
  if (data.currentSpeaker) {
    statusDot.classList.add('connected');
    statusText.textContent = 'Connected to conference';
    currentSpeakerName.textContent = data.currentSpeaker.name || 'No one';
    currentSpeakerId.textContent = data.currentSpeaker.id || '';
  } else {
    statusDot.classList.remove('connected');
    statusText.textContent = 'Not in a conference';
    currentSpeakerName.textContent = '-';
    currentSpeakerId.textContent = '';
  }

  // Update participants
  if (data.participants && data.participants.length > 0) {
    participantCount.textContent = data.participants.length;
    participantsList.innerHTML = data.participants.map(p => `
      <li>
        <div class="participant-name">
          <div class="participant-avatar">${getInitials(p.name)}</div>
          <span>${p.name}</span>
        </div>
        ${p.isLocal ? '<span class="participant-local">You</span>' : ''}
      </li>
    `).join('');
  } else {
    participantCount.textContent = '0';
    participantsList.innerHTML = '<li class="empty-state">No participants</li>';
  }

  // Update history
  if (data.speakerHistory && data.speakerHistory.length > 0) {
    historyList.innerHTML = data.speakerHistory.slice(0, 10).map(h => `
      <li>
        <span>${h.name}</span>
        <span class="history-time">${h.time}</span>
      </li>
    `).join('');
  } else {
    historyList.innerHTML = '<li class="empty-state">No history yet</li>';
  }
}

// Load data from storage
function loadData() {
  chrome.storage.local.get(['currentSpeaker', 'speakerHistory', 'participants'], (data) => {
    updateUI(data);
  });
}

// Listen for storage changes
chrome.storage.onChanged.addListener((changes, namespace) => {
  if (namespace === 'local') {
    loadData();
  }
});

// Initial load
document.addEventListener('DOMContentLoaded', loadData);

// Refresh every second
setInterval(loadData, 1000);
