// KTalk Speaker Detector - Content Script
// Injects into page context to access $conf

(function() {
  'use strict';

  // Inject script into page context
  function injectScript() {
    const script = document.createElement('script');
    script.src = chrome.runtime.getURL('injected.js');
    script.onload = function() {
      this.remove();
    };
    (document.head || document.documentElement).appendChild(script);
  }

  // Listen for messages from injected script
  window.addEventListener('ktalk-speaker-update', (event) => {
    const data = event.detail;

    // Save to chrome storage for popup
    chrome.storage.local.set({
      currentSpeaker: data.currentSpeaker,
      speakerHistory: data.speakerHistory,
      participants: data.participants,
      connected: true
    });
  });

  // Listen for connection status
  window.addEventListener('ktalk-connection-status', (event) => {
    chrome.storage.local.set({
      connected: event.detail.connected
    });
  });

  // Listen for transcript updates
  window.addEventListener('ktalk-transcript-update', (event) => {
    chrome.storage.local.set({
      transcriptLines: event.detail.transcriptLines
    });
  });

  console.log('[KTalk Speaker Detector] Content script loaded, injecting into page...');
  injectScript();
})();
