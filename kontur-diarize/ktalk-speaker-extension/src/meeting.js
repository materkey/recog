import { state } from './state.js';
import { sendWsMessage } from './ws_api.js';
import { getPlatformAdapter, getPlatform } from './platforms/index.js';

let titleSentForUrl = null;

function sendDelayedTitle() {
  // Отправляем title через 2 сек после входа, когда страница уже загрузилась
  setTimeout(() => {
    const title = document.title;
    const url = state.currentMeetingUrl;
    if (title && url && titleSentForUrl !== url) {
      titleSentForUrl = url;
      sendWsMessage({
        type: 'meeting_title_update',
        title: title
      });
      console.log('[KTalk Transcript] Sent title update:', title);
    }
  }, 2000);
}

export function sendMeetingJoin() {
  const url = window.location.href;
  const platform = getPlatform();
  // Send meeting_join for both KTalk and vas3k platforms
  if (url && platform) {
    state.currentMeetingUrl = url;
    sendWsMessage({
      type: 'meeting_join',
      url: url,
      title: ''  // Title отправим позже когда страница загрузится
    });
    console.log(`[${platform} Transcript] Sent meeting_join:`, url);
    sendDelayedTitle();
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
    titleSentForUrl = null;
  }
}

function isInMeeting() {
  const adapter = getPlatformAdapter();
  if (adapter) {
    return adapter.isConferenceAvailable();
  }
  // Legacy fallback for KTalk
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
