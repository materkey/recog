import { state } from './state.js';
import { sendWsMessage } from './ws_api.js';
import { getPlatformAdapter, getPlatform } from './platforms/index.js';

let titleSentForUrl = null;
// Per-join generation token: bumped on every `sendMeetingJoin()` so a pending
// title-retry chain from a previous join (same OR different url) invalidates
// itself instead of leaking its near-exhausted `attempt` count into the fresh
// join. The `scheduledUrl` guard alone cannot catch a leave+rejoin of the same
// room (e.g. after `meeting_join_rejected`).
let titleRetryGen = 0;

// The adapter's authoritative title (matrix → room name) can resolve after the
// initial 2 s window: the Element Call iframe runs its own matrix-js-sdk client
// that may still be syncing `getRoom(roomId).name` when we first look. Retry a
// bounded number of times (2 s apart) before locking in the generic
// `document.title` ("Element Call"), so a cold client does not permanently
// mislabel the meeting.
const TITLE_MAX_RETRIES = 5;

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

function sendDelayedTitle(attempt = 0, scheduledUrl = state.currentMeetingUrl, gen = titleRetryGen) {
  // Отправляем title через 2 сек после входа, когда страница уже загрузилась
  setTimeout(() => {
    // A newer join has superseded this retry chain. The `scheduledUrl` guard
    // below only distinguishes DIFFERENT meetings; a leave+rejoin of the SAME
    // url (e.g. after `meeting_join_rejected`) would otherwise let this stale
    // chain's near-exhausted `attempt` count fall back to `document.title` and
    // lock the room before the fresh join's room name has synced. The per-join
    // generation token invalidates the old chain regardless of url.
    if (gen !== titleRetryGen) return;

    const url = state.currentMeetingUrl;
    if (!url || titleSentForUrl === url) return;

    // Abandon this retry chain if the meeting changed since it was scheduled.
    // Retries read `state.currentMeetingUrl` fresh, so a stale chain from
    // meeting A (carrying A's near-exhausted `attempt` count) could otherwise
    // fire during meeting B, skip B's own retries, and prematurely lock B to
    // the generic `document.title` before B's room name has synced.
    if (scheduledUrl !== url) return;

    // Prefer the adapter's authoritative title (matrix → room name) over the
    // page title: inside the EC iframe `document.title` is Element Call's own
    // page title, not the room. Same optional-chaining pattern as getMeetingUrl.
    const adapter = getPlatformAdapter();
    const hasTitleHook = typeof adapter?.getMeetingTitle === 'function';
    const adapterTitle = hasTitleHook ? adapter.getMeetingTitle() : '';

    // Adapter has a title hook but the room name has not resolved yet — retry
    // instead of locking in the generic `document.title` for the whole meeting.
    if (hasTitleHook && !adapterTitle && attempt < TITLE_MAX_RETRIES) {
      sendDelayedTitle(attempt + 1, scheduledUrl, gen);
      return;
    }

    const title = adapterTitle || document.title;
    if (title) {
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
    // Supersede any pending title-retry chain from a prior join before starting
    // a fresh one (see `titleRetryGen`).
    titleRetryGen++;
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
