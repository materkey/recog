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

export function sendWsParticipants(participants) {
  sendWsMessage({
    type: 'participants_update',
    participants: participants
  });
}

export function sendWsConnectionStatus(connected) {
  sendWsMessage({
    type: 'connection_status',
    connected: connected
  });
}
