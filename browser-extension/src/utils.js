export function sendUpdate(data) {
  window.dispatchEvent(new CustomEvent('ktalk-speaker-update', { detail: data }));
}

export function getFullName(participant, info, fallback) {
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

export function getParticipantName(conf, participantId) {
  if (!participantId) return 'Unknown';

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
