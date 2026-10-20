import { state } from './state.js';

let messageSeq = 0;


export function sendWsMessage(data) {
  if (state.wsConnection && state.wsConnection.readyState === WebSocket.OPEN) {
    state.wsConnection.send(JSON.stringify(data));
  }
}

export function sendWsSpeakerUpdate(speaker) {
  sendWsMessage({
    type: 'speaker_update',
    speaker: speaker,
    timestamp: Date.now(),
    seq: ++messageSeq
  });
}

export function sendWsParticipants(participants, localMicMuted = null) {
  const message = {
    type: 'participants_update',
    participants: participants
  };
  if (typeof localMicMuted === 'boolean') message.localMicMuted = localMicMuted;
  sendWsMessage(message);
}

export function sendWsConnectionStatus(connected) {
  sendWsMessage({
    type: 'connection_status',
    connected: connected
  });
}

export function sendWsPcmChunk(pid, speakerName, isLocal, seq, startMs, endMs, pcmBase64) {
  // Tag every chunk with the tab's current meeting_id so the bridge can
  // drop sibling-tab PCM when two KTalk tabs are open at once. Without
  // this, audio from an older tab keeps flowing into whichever meeting
  // joined the bridge most recently and contaminates its transcript.jsonl.
  const meetingId = state.currentMeetingId || null;
  sendWsMessage({
    type: 'pcm_chunk',
    pid: pid,
    speaker_name: speakerName,
    is_local: !!isLocal,
    seq: seq,
    chunk_start_ms: startMs,
    chunk_end_ms: endMs,
    sample_rate: 16000,
    pcm_base64: pcmBase64,
    meeting_id: meetingId,
  });
}

export function sendWsPcmTransportStats(stats) {
  sendWsMessage({
    type: 'pcm_transport_stats',
    stats: stats || {},
    timestamp: Date.now(),
  });
}

function generateCopilotRequestId() {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return 'copilot_' + crypto.randomUUID();
  }
  return 'copilot_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 10);
}

export function sendCopilotChatMessage({ text, meetingId, meetingTitle, transcriptWindow }) {
  const requestId = generateCopilotRequestId();
  sendWsMessage({
    type: 'copilot_chat_message',
    request_id: requestId,
    text: text,
    meeting_id: meetingId || null,
    meeting_title: meetingTitle || null,
    transcript_window: transcriptWindow || [],
    created_at: Date.now()
  });
  return requestId;
}
