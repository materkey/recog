import { state, constants } from './state.js';
import { updateWsIndicatorPlatform, handleTranscript, handleTranscriptHistory, handleTranscriptRefine, handleNewMeeting, handleCopilotReply, handleCopilotStatus, appendCopilotSystemMessage, handleCopilotBridgeDisconnect } from './ui.js';
import { handleMeetingJoinRejected } from './meeting.js';
import { getActiveSpeaker, sendParticipantsUpdate, sendFilteredSpeakerUpdate } from './speaker.js';
import { sendWsMessage } from './ws_api.js';
import { isInMeeting, sendMeetingJoin, startMeetingCheck } from './meeting.js';
import { getPlatformAdapter } from './platforms/index.js';

/**
 * `meeting_ended` is broadcast to every extension client, and the bridge now
 * ends a meeting as soon as its owner tab drops — so a tab merely loaded on a
 * conference host (the content script connects to the bridge with or without a
 * call) receives it too. Only the tab actually in that meeting should narrate
 * it, so this match is strict: an idle tab has `currentMeetingId === null` and
 * stays quiet. The transcript guards in `ui.js` deliberately do the opposite
 * and pass unknown ids through — dropping a transcript row costs data,
 * dropping a system line costs nothing.
 */
function isOurMeeting(data) {
  return Boolean(state.currentMeetingId) && data.meeting_id === state.currentMeetingId;
}

/**
 * `transcript_history` answers this tab's accepted `meeting_join` (or its own
 * transcript request). The join made the bridge clear participant state, and
 * a participants frame sent before it by the periodic refresh is gone: report
 * the participants again, own microphone included, which an adapter that
 * reports only on change (Zoom) would not resend. A tab whose join was refused
 * has no `currentMeetingUrl` and stays quiet.
 */
function resendParticipantsAfterJoin() {
  if (!state.currentMeetingUrl || !isInMeeting()) return;
  const adapter = getPlatformAdapter();
  const conf = adapter ? adapter.getConference() : window.$conf;
  if (conf) sendParticipantsUpdate(conf);
}

export function connectWebSocket() {
  if (state.wsConnection && state.wsConnection.readyState === WebSocket.OPEN) {
    return;
  }

  try {
    state.wsConnection = new WebSocket(constants.WS_URL);

    state.wsConnection.onopen = () => {
      console.log('[KTalk Transcript] WebSocket connected');
      state.wsConnected = true;
      updateWsIndicatorPlatform(true);
      // Reset channel-status indicator on bridge reconnect: the prior
      // session's `channel_disconnected` (set by `onclose` /
      // `handleCopilotBridgeDisconnect`) is no longer authoritative — the
      // bridge will replay the actual channel state via a `channel_connected`
      // primer in `_handle_client` if a channel client is registered.
      // Without this reset the indicator would stay stuck on "channel
      // offline" forever even after the channel came back.
      state.copilotChannelStatus = 'unknown';

      // Send participants for both KTalk and vas3k. The join goes first: the
      // bridge clears participant and speaker state on meeting_join, and an
      // adapter that reports only on change (Zoom) would not resend them.
      const adapter = getPlatformAdapter();
      if (adapter) {
        // Use the same membership predicate as the periodic join/leave check.
        const conf = isInMeeting() ? adapter.getConference() : null;
        if (conf) {
          sendMeetingJoin();
          sendParticipantsUpdate(conf);
          const activeSpeaker = getActiveSpeaker(conf);
          sendFilteredSpeakerUpdate(activeSpeaker);
        }
      } else if (isInMeeting()) {
        sendMeetingJoin();
        sendParticipantsUpdate(window.$conf);
        const activeSpeaker = getActiveSpeaker(window.$conf);
        sendFilteredSpeakerUpdate(activeSpeaker);
      }

      startMeetingCheck();

      // Clock sync for accurate timestamp correlation
      sendWsMessage({ type: 'clock_sync', browser_time: Date.now() });

      // Re-sync clock every 30 seconds
      state.clockSyncTimer = setInterval(() => {
        sendWsMessage({ type: 'clock_sync', browser_time: Date.now() });
      }, 30000);
    };

    state.wsConnection.onclose = () => {
      console.log('[KTalk Transcript] WebSocket disconnected');
      state.wsConnected = false;
      updateWsIndicatorPlatform(false);
      // Surface the bridge drop in the Copilot tab — flip pending
      // placeholders to error and re-enable input. Otherwise an in-flight
      // chat would leave the textarea disabled until the user reloaded.
      try {
        handleCopilotBridgeDisconnect();
      } catch (e) {
        console.log('[KTalk Copilot] bridge-disconnect handler failed:', e);
      }

      if (state.clockSyncTimer) {
        clearInterval(state.clockSyncTimer);
        state.clockSyncTimer = null;
      }

      if (!state.wsReconnectTimer) {
        state.wsReconnectTimer = setTimeout(() => {
          state.wsReconnectTimer = null;
          connectWebSocket();
        }, 5000);
      }
    };

    state.wsConnection.onerror = () => {
      state.wsConnected = false;
    };

    state.wsConnection.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data);
        if (data.type === 'transcript') {
          handleTranscript(data);
        } else if (data.type === 'transcript_history') {
          handleTranscriptHistory(data);
          resendParticipantsAfterJoin();
        } else if (data.type === 'transcript_refine') {
          handleTranscriptRefine(data);
        } else if (data.type === 'new_meeting') {
          handleNewMeeting(data);
        } else if (data.type === 'copilot_reply') {
          // Platform dispatch is centralized in the handler: ensureCopilotContent
          // → createCopilotContent() mounts the copilot chat into the floating
          // panel on floating-panel platforms (matrix / vas3k) and the KTalk tab
          // otherwise, so copilot frames reach the right surface on every platform.
          handleCopilotReply(data);
        } else if (data.type === 'copilot_status') {
          handleCopilotStatus(data);
        } else if (data.type === 'meeting_started') {
          // No isOurMeeting() guard here: the bridge emits this BEFORE the
          // joiner's `transcript_history` (ktalk_bridge.py:2651 vs :2698), so
          // `state.currentMeetingId` is still the previous meeting's id.
          const title = (data && data.meeting_title) ? ` — ${data.meeting_title}` : '';
          appendCopilotSystemMessage(`Встреча началась${title}`);
        } else if (data.type === 'meeting_ended') {
          if (isOurMeeting(data)) {
            appendCopilotSystemMessage('Встреча закончилась');
          }
        } else if (data.type === 'meeting_join_rejected') {
          const notice = handleMeetingJoinRejected(data);
          if (notice) appendCopilotSystemMessage(notice);
        }
      } catch (e) {
        console.log('[KTalk Transcript] Parse error:', e);
      }
    };

  } catch (e) {
    console.log('[KTalk Transcript] Connection failed:', e);
  }
}
