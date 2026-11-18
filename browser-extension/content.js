// KTalk Speaker Detector - Content Script
// Injects into page context to access $conf

(function() {
  'use strict';

  // Inject script into page context
  function injectScript() {
    // Publish the PCM worklet URL so injected.js (page world, no
    // chrome.runtime access) can load the AudioWorklet module. Inline
    // `<script textContent=...>` is blocked by KTalk's CSP, so we stash
    // the URL on a DOM attribute instead — DOM attribute writes are not
    // gated by script-src. See docs/plans/rippling-mixing-giraffe.md.
    const workletUrl = chrome.runtime.getURL('src/audio-worklet/downsampler.js');
    document.documentElement.setAttribute('data-pcm-worklet-url', workletUrl);

    const script = document.createElement('script');
    script.src = chrome.runtime.getURL('injected.js');
    // Unpacked release builds carry per-installation hosts in the manifest.
    // Give this page's adapter the matching platform, without exposing the host list.
    const origin = window.location.origin;
    for (const entry of chrome.runtime.getManifest().content_scripts) {
      if (entry.matches.includes(origin + '/widgets/element-call/*')) script.dataset.recogPlatform = 'matrix';
      else if (entry.matches.includes(origin + '/*')) script.dataset.recogPlatform = 'jitsi';
    }
    script.onload = function() {
      this.remove();
    };
    (document.head || document.documentElement).appendChild(script);
  }

  // Listen for messages from injected script
  window.addEventListener('ktalk-speaker-update', (event) => {
    const data = event.detail;

    // Save to chrome storage for popup
    try {
      chrome.storage.local.set({
        currentSpeaker: data.currentSpeaker,
        speakerHistory: data.speakerHistory,
        participants: data.participants,
        connected: true
      });
    } catch (e) {
      // Extension context can be invalidated on reload
      console.debug('[KTalk Speaker Detector] Storage write skipped:', e);
    }
  });

  // Listen for connection status
  window.addEventListener('ktalk-connection-status', (event) => {
    try {
      chrome.storage.local.set({
        connected: event.detail.connected
      });
    } catch (e) {
      console.debug('[KTalk Speaker Detector] Storage write skipped:', e);
    }
  });

  // Listen for transcript updates
  window.addEventListener('ktalk-transcript-update', (event) => {
    try {
      chrome.storage.local.set({
        transcriptLines: event.detail.transcriptLines
      });
    } catch (e) {
      console.debug('[KTalk Speaker Detector] Storage write skipped:', e);
    }
  });

  console.log('[KTalk Speaker Detector] Content script loaded, injecting into page...');
  injectScript();
})();
