import { state } from './state.js';
import { sendWsMessage } from './ws_api.js';

export function sendMeetingJoin() {
  const url = window.location.href;
  if (url && url.includes('ktalk.ru')) {
    state.currentMeetingUrl = url;
    sendWsMessage({
      type: 'meeting_join',
      url: url,
      title: document.title || ''
    });
    console.log('[KTalk Transcript] Sent meeting_join:', url);
  }
}

export function sendMeetingLeave(reason = 'user_left') {
  if (state.currentMeetingUrl) {
    sendWsMessage({
      type: 'meeting_leave',
      reason: reason
    });
    console.log('[KTalk Transcript] Sent meeting_leave:', reason);
    state.currentMeetingUrl = null;
  }
}

function isInMeeting() {
  if (!window.$conf) return false;
  const participantsMap = window.$conf._participantsMap;
  return participantsMap && participantsMap.size > 0;
}

export function startMeetingCheck() {
  if (state.meetingCheckInterval) {
    clearInterval(state.meetingCheckInterval);
  }

  state.meetingCheckInterval = setInterval(() => {
    const inMeeting = isInMeeting();
    const url = window.location.href;

    if (inMeeting && url !== state.currentMeetingUrl) {
      if (state.currentMeetingUrl) {
        sendMeetingLeave('changed_meeting');
      }
      sendMeetingJoin();
    } else if (!inMeeting && state.currentMeetingUrl) {
      sendMeetingLeave('user_left');
    }
  }, 2000);
}
