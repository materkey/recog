import { constants } from './state.js';

let audioCtx = null;
const analysers = new Map(); // participantId → { source, analyser, freqData, history[] }

const HISTORY_SIZE = 3;

function ensureAudioContext() {
  if (!audioCtx || audioCtx.state === 'closed') {
    audioCtx = new AudioContext();
  }
  return audioCtx;
}

function computeFlatness(analyser) {
  const data = new Float32Array(analyser.frequencyBinCount);
  analyser.getFloatFrequencyData(data);
  const power = [];
  for (let i = 1; i < 256; i++) {
    const db = data[i];
    power.push(db > -100 ? Math.pow(10, db / 10) : 0);
  }
  const totalEnergy = power.reduce((a, b) => a + b, 0);
  if (totalEnergy < 1e-10) return -1; // no signal
  const nonZero = power.filter(p => p > 1e-15);
  if (nonZero.length < 10) return -1;
  const geoMean = Math.exp(nonZero.reduce((a, b) => a + Math.log(b), 0) / nonZero.length);
  const ariMean = nonZero.reduce((a, b) => a + b, 0) / nonZero.length;
  return ariMean > 0 ? geoMean / ariMean : 0;
}

export function attachParticipant(participantId, mediaStream) {
  if (!participantId || !mediaStream) return;
  if (analysers.has(participantId)) return;

  try {
    const ctx = ensureAudioContext();
    const source = ctx.createMediaStreamSource(mediaStream);
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 512;
    analyser.smoothingTimeConstant = 0.3;
    source.connect(analyser);
    analysers.set(participantId, { source, analyser, history: [] });
    console.log('[AudioAnalyzer] Attached participant:', participantId);
  } catch (e) {
    console.warn('[AudioAnalyzer] Failed to attach participant:', participantId, e);
  }
}

export function detachParticipant(participantId) {
  const entry = analysers.get(participantId);
  if (!entry) return;
  try {
    entry.source.disconnect();
  } catch (e) { /* already disconnected */ }
  analysers.delete(participantId);
  console.log('[AudioAnalyzer] Detached participant:', participantId);
}

export function getFlatness(participantId) {
  const entry = analysers.get(participantId);
  if (!entry) return null;
  return computeFlatness(entry.analyser);
}

export function isSpeech(participantId) {
  const entry = analysers.get(participantId);
  if (!entry) return { speech: true, reason: 'no_analyser' };

  if (audioCtx?.state === 'suspended') return { speech: true, reason: 'ctx_suspended' };

  const flatness = computeFlatness(entry.analyser);

  if (flatness === -1) return { speech: true, reason: 'silence' };

  // Maintain sliding window
  entry.history.push(flatness);
  if (entry.history.length > HISTORY_SIZE) entry.history.shift();

  if (entry.history.length < 2) return { speech: true, reason: 'warmup' };

  // Median vote
  const sorted = [...entry.history].sort((a, b) => a - b);
  const median = sorted[Math.floor(sorted.length / 2)];

  if (median < constants.FLATNESS_THRESHOLD) {
    return { speech: true, reason: 'speech' };
  }
  return { speech: false, reason: 'noise' };
}

export function attachAllFromConf(conf) {
  if (!conf) return;

  // Scan KTalk _participantsMap
  conf._participantsMap?.forEach((p) => {
    const pid = p._id;
    if (!pid || p.local) return;
    const stream = p._audioTrack$?.value?.jitsiTrack?.stream;
    if (stream) attachParticipant(pid, stream);
  });

  // Scan Jitsi participants
  const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
  for (const jp of jitsiParticipants) {
    const pid = jp._id || jp.getId?.() || jp.id;
    if (!pid || analysers.has(pid)) continue;
    const tracks = jp.getTracks?.() || [];
    const audioTrack = tracks.find(t => t.getType?.() === 'audio');
    const stream = audioTrack?.stream || audioTrack?.getOriginalStream?.();
    if (stream) attachParticipant(pid, stream);
  }
}

export function dispose() {
  for (const [pid, entry] of analysers) {
    try { entry.source.disconnect(); } catch (e) { /* ok */ }
  }
  analysers.clear();
  if (audioCtx && audioCtx.state !== 'closed') {
    audioCtx.close().catch(() => {});
  }
  audioCtx = null;
  console.log('[AudioAnalyzer] Disposed');
}
