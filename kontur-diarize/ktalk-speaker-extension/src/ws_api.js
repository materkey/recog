import { state } from './state.js';

let messageSeq = 0;

// Server-authoritative PCM transport gate. Bridge sends
// {type: 'pcm_transport_enabled', enabled: bool} on each WS connect.
// Extension must not send pcm_chunk frames unless this flag is true.
// See docs/plans/2026-04-22-per-speaker-asr-pipeline.md decision D2.
let pcmTransportEnabled = false;
const pcmTransportListeners = new Set();

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

export function sendWsPcmChunk(pid, speakerName, isLocal, seq, startMs, endMs, pcmBase64) {
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
  });
}

export function isPcmTransportEnabled() {
  return pcmTransportEnabled;
}

export function subscribePcmTransport(listener) {
  pcmTransportListeners.add(listener);
  return () => {
    pcmTransportListeners.delete(listener);
  };
}

export function handlePcmTransportEnabledMessage(data) {
  const next = !!data?.enabled;
  if (next === pcmTransportEnabled) return;
  pcmTransportEnabled = next;
  console.log('[PcmTransport] Server gate:', pcmTransportEnabled ? 'ENABLED' : 'DISABLED');
  for (const listener of pcmTransportListeners) {
    try {
      listener(pcmTransportEnabled);
    } catch (e) {
      console.warn('[PcmTransport] Listener error:', e);
    }
  }
}

// Reset gate to default-off when WS disconnects so we don't keep the
// stale positive flag when the server is gone. The next connect will
// re-assert the server's value.
export function resetPcmTransportOnDisconnect() {
  if (pcmTransportEnabled) {
    pcmTransportEnabled = false;
    console.log('[PcmTransport] Server gate: DISABLED (ws disconnect)');
    for (const listener of pcmTransportListeners) {
      try {
        listener(false);
      } catch (e) {
        console.warn('[PcmTransport] Listener error:', e);
      }
    }
  }
}
