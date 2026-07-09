import { state } from './state.js';
import { sendWsMessage } from './ws_api.js';
import { getPlatformAdapter, getPlatform } from './platforms/index.js';

let titleSentForUrl = null;

/**
 * Canonical meeting URL: the adapter's synthesized URL when it provides one
 * (matrix → `#/room/<roomId>`), otherwise the raw page URL. BOTH the sent
 * `meeting_join.url` / stored `state.currentMeetingUrl` AND
 * `startMeetingCheck()`'s change-detection must read this same source, or the
 * inside-iframe location (`…/index.html#…`) and the stored synthesized URL
 * would never match and the check would flap leave/join every 2 s tick.
 */
export function getMeetingUrl() {
  const adapter = getPlatformAdapter();
  return adapter?.getMeetingUrl?.() || window.location.href;
}

function sendDelayedTitle() {
  // Отправляем title через 2 сек после входа, когда страница уже загрузилась
  setTimeout(() => {
    // Prefer the adapter's authoritative title (matrix → room name) over the
    // page title: inside the EC iframe `document.title` is Element Call's own
    // page title, not the room. Same optional-chaining pattern as getMeetingUrl.
    const adapter = getPlatformAdapter();
    const title = adapter?.getMeetingTitle?.() || document.title;
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
  const url = getMeetingUrl();
  const platform = getPlatform();
  // Send meeting_join for both KTalk and vas3k platforms
  if (url && platform) {
    state.currentMeetingUrl = url;
    sendWsMessage({
      type: 'meeting_join',
      url: url,
      platform: platform,
      title: ''  // Title отправим позже когда страница загрузится
    });
    console.log(`[${platform} Transcript] Sent meeting_join:`, url);
    sendDelayedTitle();
  }
}

export function handleMeetingJoinRejected(data) {
  // Bridge сообщает, что активная встреча уже занята другим табом.
  // Очищаем local-кеш `currentMeetingUrl`, чтобы startMeetingCheck() через
  // ~2 с повторил sendMeetingJoin (когда owner-tab может уже закрыться),
  // и не сетим `state.currentMeetingId` — handleTranscript filter тогда
  // дропнет броадкасты соседнего таба и не загрязнит нашу UI стеной.
  const activeId = data && data.active_meeting_id ? data.active_meeting_id : 'unknown';
  console.log(
    `[KTalk Transcript] meeting_join rejected by bridge — another tab owns meeting ${activeId}. Will retry on next poll.`
  );
  state.currentMeetingUrl = null;
  titleSentForUrl = null;
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
    const url = getMeetingUrl();

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
