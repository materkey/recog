import { state, constants } from './state.js';
import { updateWsIndicator, handleTranscript, handleTranscriptHistory, handleNewMeeting, handleSummaryResponse, handleAnchorResponse } from './ui.js';
import { getActiveSpeaker, sendParticipantsUpdate } from './speaker.js';
import { sendWsSpeakerUpdate } from './ws_api.js';
import { sendMeetingJoin, startMeetingCheck } from './meeting.js';

export function connectWebSocket() {
  if (state.wsConnection && state.wsConnection.readyState === WebSocket.OPEN) {
    return;
  }

  try {
    state.wsConnection = new WebSocket(constants.WS_URL);

    state.wsConnection.onopen = () => {
      console.log('[KTalk Transcript] WebSocket connected');
      state.wsConnected = true;
      updateWsIndicator(true);

      if (window.$conf) {
        sendParticipantsUpdate(window.$conf);
        const activeSpeaker = getActiveSpeaker(window.$conf);
        sendWsSpeakerUpdate(activeSpeaker);
        sendMeetingJoin();
      }

      startMeetingCheck();
    };

    state.wsConnection.onclose = () => {
      console.log('[KTalk Transcript] WebSocket disconnected');
      state.wsConnected = false;
      updateWsIndicator(false);

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
        } else if (data.type === 'new_meeting') {
          handleNewMeeting(data);
        } else if (data.type === 'summarize_response') {
          handleSummaryResponse(data);
        } else if (data.type === 'anchor_response') {
          handleAnchorResponse(data);
        }
      } catch (e) {
        console.log('[KTalk Transcript] Parse error:', e);
      }
    };

  } catch (e) {
    console.log('[KTalk Transcript] Connection failed:', e);
  }
}
