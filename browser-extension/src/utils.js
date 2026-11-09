export function sendUpdate(data) {
  window.dispatchEvent(new CustomEvent('ktalk-speaker-update', { detail: data }));
}

// A participant sharing a screen with sound has a second audio track next to the
// microphone. KTalk tells them apart the same way: audioType, or for a track
// without one the source name, `-a0` for the microphone and `-a1` for the screen.
export function isScreenShareTrack(jitsiTrack) {
  return jitsiTrack?.audioType === 'screenShare'
    || (jitsiTrack?.audioType == null && jitsiTrack?.getSourceName?.()?.endsWith('-a1') === true);
}

export function getUncertainSuffix(line) {
  if (!line.uncertain) return '';
  if (line.uncertain_reason === 'mics_off') return ' (🔇?)';
  return ' (?)';
}

export function renderMarkdown(text) {
  let html = escapeHtml(text);

  html = html.replace(/^## (.+)$/gm, '<h3>$1</h3>');
  html = html.replace(/^### (.+)$/gm, '<h4>$1</h4>');

  html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
  html = html.replace(/\*([^*]+)\*/g, '<em>$1</em>');

  html = html.replace(/^- (.+)$/gm, '<li>$1</li>');
  html = html.replace(/(<li>.*<\/li>\n?)+/g, '<ul>$&</ul>');

  html = html.replace(/\n/g, '<br>');

  html = html.replace(/<\/h3><br>/g, '</h3>');
  html = html.replace(/<\/h4><br>/g, '</h4>');
  html = html.replace(/<\/ul><br>/g, '</ul>');
  html = html.replace(/<br><ul>/g, '<ul>');

  return html;
}

export function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}

// Google Meet sends `require-trusted-types-for 'script'`: assigning a plain
// string to innerHTML throws there. Panel markup is built from escapeHtml()-ed
// values, so a pass-through policy is enough. The policy is created lazily,
// once; without TT (or if creation fails) the raw string is returned, which
// is what every other platform accepts.
let trustedHtmlPolicyInitialized = false;
let trustedHtmlPolicy = null;

export function toTrustedHtml(html) {
  if (!trustedHtmlPolicyInitialized) {
    trustedHtmlPolicyInitialized = true;
    try {
      trustedHtmlPolicy = globalThis.trustedTypes?.createPolicy('recog', { createHTML: (s) => s }) || null;
    } catch (err) {
      console.warn('[recog] Trusted Types policy unavailable:', err);
    }
  }
  return trustedHtmlPolicy ? trustedHtmlPolicy.createHTML(html) : html;
}
