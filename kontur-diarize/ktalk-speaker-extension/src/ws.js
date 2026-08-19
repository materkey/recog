import { state, constants } from './state.js';
import { updateWsIndicatorPlatform, handleTranscript, handleTranscriptHistory, handleTranscriptRefine, handleNewMeeting, handleSummaryResponse, handleAnchorResponse, handleCopilotReply, handleCopilotStatus, appendCopilotSystemMessage, handleCopilotBridgeDisconnect } from './ui.js';
import { handleMeetingJoinRejected } from './meeting.js';
import { handleVas3kSummaryResponse } from './ui-vas3k.js';
import { getActiveSpeaker, sendParticipantsUpdate, sendFilteredSpeakerUpdate } from './speaker.js';
import { sendWsMessage } from './ws_api.js';
import { sendMeetingJoin, startMeetingCheck } from './meeting.js';
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

      // Send participants for both KTalk and vas3k
      const adapter = getPlatformAdapter();
      if (adapter) {
        // Gate on isConferenceAvailable(), not a merely-truthy getConference():
        // for matrix getConference() returns rtcSession even before roomId
        // resolves, so sendMeetingJoin() would fall back to the EC iframe's
        // index.html URL and store live_id="index.html" — the exact collapse
        // isConferenceAvailable() guards against. Mirrors isInMeeting().
        const conf = adapter.isConferenceAvailable() ? adapter.getConference() : null;
        if (conf) {
          sendParticipantsUpdate(conf);
          const activeSpeaker = getActiveSpeaker(conf);
          sendFilteredSpeakerUpdate(activeSpeaker);
          sendMeetingJoin();
        }
      } else if (window.$conf) {
        sendParticipantsUpdate(window.$conf);
        const activeSpeaker = getActiveSpeaker(window.$conf);
        sendFilteredSpeakerUpdate(activeSpeaker);
        sendMeetingJoin();
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
        } else if (data.type === 'transcript_refine') {
          handleTranscriptRefine(data);
        } else if (data.type === 'new_meeting') {
          handleNewMeeting(data);
        } else if (data.type === 'summarize_response') {
          handleSummaryResponse(data);
          handleVas3kSummaryResponse(data);
        } else if (data.type === 'anchor_response') {
          handleAnchorResponse(data);
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
          handleMeetingJoinRejected(data);
        }
      } catch (e) {
        console.log('[KTalk Transcript] Parse error:', e);
      }
    };

  } catch (e) {
    console.log('[KTalk Transcript] Connection failed:', e);
  }
}
