// Per-speaker PCM transport.
//
// Per decision D4/D9 in docs/plans/2026-04-22-per-speaker-asr-pipeline.md:
// each remote KTalk participant's audio track is routed through an
// AudioWorkletNode (see src/audio-worklet/downsampler.js) that emits
// 16 kHz Int16 PCM chunks every 3.0 s stride with 3.6 s payload. Chunks
// are base64-encoded and shipped to ktalk_bridge via sendWsPcmChunk().
//
// Local-mic participant is intentionally skipped (decision D9): the host
// tab does not expose its own mic as a remote track and unifying paths
// would add risk without benefit; the sounddevice path stays the source
// of truth for the local mic.

import { constants } from './state.js';
import { sendWsPcmChunk, sendWsPcmTransportStats } from './ws_api.js';
import { isScreenShareTrack } from './utils.js';

const BOUND_VIA = {
  WRAPPER: 'wrapper',
  RECEIVER: 'receiver',
};

const REBIND_REASON = {
  INITIAL_PROBE: 'initial_receiver_probe',
  JITSI_TRACK_UPDATE: 'jitsi_track_update',
  JITSI_UPDATE_RECEIVER_PROBE: 'jitsi_track_update_receiver_probe',
  DIAGNOSTICS_PROBE: 'diagnostics_receiver_probe',
};

function createEmptyWebrtcStats() {
  return {
    pc_count: 0,
    inbound_audio_bytes_received: 0,
    inbound_audio_bytes_delta: 0,
    inbound_audio_packets_received: 0,
    inbound_audio_packets_delta: 0,
  };
}

let audioContext = null;
let workletLoadPromise = null;
let diagnosticsTimer = null;
let lastStatsSentAtMs = 0;
let lastWebrtcStats = createEmptyWebrtcStats();

const pending = new Map(); // pid -> {speakerName, isLocal, mediaStream, jitsiTrack, jitsiConf, conf}
const active = new Map();  // pid -> binding entry

function getWorkletUrl() {
  if (typeof window !== 'undefined' && window.__PCM_WORKLET_URL__) {
    return window.__PCM_WORKLET_URL__;
  }
  // content.js stashes the URL on <html> via setAttribute because
  // inline-script injection is blocked by KTalk's CSP. Reading a DOM
  // attribute from the page world is always allowed.
  if (typeof document !== 'undefined' && document.documentElement) {
    const attr = document.documentElement.getAttribute('data-pcm-worklet-url');
    if (attr) return attr;
  }
  return null;
}

function ensureAudioContext() {
  if (!audioContext || audioContext.state === 'closed') {
    try {
      audioContext = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
      console.log(
        '[PcmTransport] AudioContext created, state=', audioContext.state,
        'baseLatency=', audioContext.baseLatency,
        'sampleRate=', audioContext.sampleRate
      );
      // Without this resume() the context stays suspended under
      // autoplay-policy (Playwright + headful Chromium in particular) —
      // worklet addModule succeeds and the graph builds but process()
      // never runs, so no pcm_chunk is ever emitted. Safe no-op when
      // ctx is already running.
      audioContext.resume().then(
        () => console.log('[PcmTransport] AudioContext resumed, state=', audioContext?.state),
        (e) => console.warn('[PcmTransport] AudioContext resume failed:', e?.message || e)
      );
    } catch (e) {
      console.error('[PcmTransport] AudioContext creation failed:', e);
      audioContext = null;
    }
  }
  return audioContext;
}

async function ensureWorkletModule(ctx) {
  if (!ctx || !ctx.audioWorklet) {
    console.warn('[PcmTransport] ensureWorkletModule: no ctx or no audioWorklet API');
    return false;
  }
  if (workletLoadPromise) {
    return workletLoadPromise;
  }
  const url = getWorkletUrl();
  if (!url) {
    console.warn('[PcmTransport] window.__PCM_WORKLET_URL__ not set; worklet cannot be loaded');
    return false;
  }
  console.log('[PcmTransport] Loading worklet module from', url);
  workletLoadPromise = ctx.audioWorklet
    .addModule(url)
    .then(() => {
      console.log('[PcmTransport] Worklet module loaded OK');
      return true;
    })
    .catch((e) => {
      console.error('[PcmTransport] Worklet module load failed:', e?.message || e);
      workletLoadPromise = null;
      return false;
    });
  return workletLoadPromise;
}

function int16ToBase64(int16) {
  const bytes = new Uint8Array(int16.buffer, int16.byteOffset, int16.byteLength);
  let binary = '';
  const chunkSize = 8192;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const slice = bytes.subarray(i, Math.min(i + chunkSize, bytes.length));
    binary += String.fromCharCode.apply(null, slice);
  }
  return btoa(binary);
}

function logMaterializeSkip(pid, reason, warn = false) {
  const fn = warn ? console.warn : console.log;
  fn('[PcmTransport] materialize skip pid=', pid, 'reason:', reason);
}

function getAudioTrackFromStream(stream) {
  if (!stream || typeof stream.getAudioTracks !== 'function') return null;
  const tracks = stream.getAudioTracks();
  return tracks && tracks.length ? tracks[0] : null;
}

function getNativeTrackFromJitsiTrack(jitsiTrack) {
  if (!jitsiTrack) return null;
  const track = jitsiTrack.getTrack?.() || jitsiTrack.track;
  return track && track.kind === 'audio' ? track : null;
}

function getStreamFromJitsiTrack(jitsiTrack) {
  if (!jitsiTrack) return null;
  const stream = jitsiTrack.stream || jitsiTrack.getStream?.() || jitsiTrack.getOriginalStream?.();
  if (stream) return stream;
  const track = getNativeTrackFromJitsiTrack(jitsiTrack);
  if (track && typeof MediaStream !== 'undefined') return new MediaStream([track]);
  return null;
}

function getTrackIdSet(infoOrEntry) {
  const ids = new Set();
  const add = (track) => {
    if (track && track.kind === 'audio' && track.id) ids.add(String(track.id));
  };
  add(getNativeTrackFromJitsiTrack(infoOrEntry?.jitsiTrack));
  add(getAudioTrackFromStream(infoOrEntry?.mediaStream));
  if (infoOrEntry?.requestedTrackId) ids.add(String(infoOrEntry.requestedTrackId));
  if (infoOrEntry?.boundTrackId) ids.add(String(infoOrEntry.boundTrackId));
  return ids;
}

function createHiddenAudioConsumer(pid, stream, trackId) {
  if (typeof document === 'undefined' || !document.createElement || !stream) {
    return { element: null, state: 'unavailable', error: '' };
  }
  try {
    const audio = document.createElement('audio');
    audio.autoplay = true;
    audio.muted = true;
    audio.playsInline = true;
    audio.controls = false;
    audio.srcObject = stream;
    audio.setAttribute('data-recog-pcm-consumer', String(pid));
    if (trackId) audio.setAttribute('data-recog-track-id', String(trackId));
    audio.style.position = 'fixed';
    audio.style.left = '-10000px';
    audio.style.top = '0';
    audio.style.width = '1px';
    audio.style.height = '1px';
    audio.style.opacity = '0';
    audio.style.pointerEvents = 'none';
    (document.body || document.documentElement).appendChild(audio);
    const consumer = { element: audio, state: 'created', error: '' };
    if (typeof audio.play === 'function') {
      audio.play().then(
        () => { consumer.state = 'playing'; },
        (e) => {
          consumer.state = 'play_failed';
          consumer.error = e?.message || String(e);
          console.warn('[PcmTransport] hidden audio consumer play() failed pid=', pid, consumer.error);
        }
      );
    }
    return consumer;
  } catch (e) {
    return { element: null, state: 'failed', error: e?.message || String(e) };
  }
}

function destroyConsumer(entry) {
  const element = entry?.consumer?.element;
  if (!element) return;
  try {
    element.pause();
    element.srcObject = null;
    element.remove();
  } catch (_) {
    /* ignore */
  }
  entry.consumer = null;
}

function refreshConsumer(pid, entry, stream, trackId) {
  if (!constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED) return;
  const existingStream = entry.consumer?.element?.srcObject;
  if (existingStream === stream) return;
  destroyConsumer(entry);
  entry.consumer = createHiddenAudioConsumer(pid, stream, trackId);
}

function createSourceBinding(ctx, stream, node) {
  const source = ctx.createMediaStreamSource(stream);
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 512;
  analyser.smoothingTimeConstant = 0.2;
  const analyserBuf = new Float32Array(analyser.fftSize);
  try {
    source.connect(analyser);
    analyser.connect(node);
  } catch (e) {
    try { source.disconnect(); } catch (_) { /* ignore */ }
    try { analyser.disconnect(); } catch (_) { /* ignore */ }
    throw e;
  }
  return { source, analyser, analyserBuf };
}

function disconnectSource(entry) {
  try { entry.source?.disconnect(); } catch (_) { /* already disconnected */ }
  try { entry.analyser?.disconnect(); } catch (_) { /* already disconnected */ }
}

function bindEntryToStream(pid, entry, stream, via, trackObject = null) {
  const ctx = ensureAudioContext();
  if (!ctx || !stream || !entry.node) return false;
  const track = trackObject || getAudioTrackFromStream(stream);
  if (track?.readyState === 'ended') {
    entry.lastError = 'track_ended';
    return false;
  }

  let binding;
  try {
    binding = createSourceBinding(ctx, stream, entry.node);
  } catch (e) {
    entry.lastError = e?.message || String(e);
    console.warn('[PcmTransport] source bind failed pid=', pid, entry.lastError);
    return false;
  }

  disconnectSource(entry);
  entry.source = binding.source;
  entry.analyser = binding.analyser;
  entry.analyserBuf = binding.analyserBuf;
  entry.mediaStream = stream;
  entry.boundTrackObject = track || null;
  entry.boundTrackId = track?.id || '';
  entry.boundVia = via || BOUND_VIA.WRAPPER;
  entry.trackReadyState = track?.readyState || '';
  entry.trackMuted = track?.muted === true;
  entry.lastError = '';
  refreshConsumer(pid, entry, stream, entry.boundTrackId);
  return true;
}

function sendDiagnosticsSnapshot(reason = 'snapshot') {
  if (!constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED) return;
  try {
    lastStatsSentAtMs = Date.now();
    sendWsPcmTransportStats(pcmGetDiagnostics());
  } catch (e) {
    console.warn('[PcmTransport] diagnostics', reason, 'failed:', e?.message || e);
  }
}

async function materialize(pid, info) {
  if (active.has(pid)) return logMaterializeSkip(pid, 'already active');
  const ctx = ensureAudioContext();
  if (!ctx) return logMaterializeSkip(pid, 'no AudioContext', true);
  const loaded = await ensureWorkletModule(ctx);
  if (!loaded) return logMaterializeSkip(pid, 'worklet not loaded', true);

  // By the time addModule resolves, attach may have been cancelled.
  if (!pending.has(pid)) return logMaterializeSkip(pid, 'pending cleared during await');
  // Two concurrent materialize(pid) calls can both pass the top-of-function
  // active.has(pid) check (gate flip + pcmAttachParticipant both dispatch),
  // then both await, then both fall through to create a node/source — the
  // second active.set overwrites the first entry and leaks its nodes.
  // Re-check after the await so only one wins.
  if (active.has(pid)) return logMaterializeSkip(pid, 'raced another materialize');

  try {
    const node = new AudioWorkletNode(ctx, 'pcm-downsampler', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
      channelCount: 1,
      channelCountMode: 'explicit',
      channelInterpretation: 'discrete',
    });

    const entry = {
      source: null,
      analyser: null,
      analyserBuf: null,
      node,
      seq: 0,
      chunksLogged: 0,
      speakerName: info.speakerName,
      isLocal: info.isLocal,
      mediaStream: null,
      jitsiTrack: info.jitsiTrack || null,
      jitsiConf: info.jitsiConf || null,
      conf: info.conf || null,
      consumer: null,
      boundTrackObject: null,
      boundTrackId: '',
      requestedTrackId: '',
      boundVia: '',
      rebindCount: 0,
      lastRebindAtMs: 0,
      lastRebindReason: '',
      lastChunkAtMs: 0,
      preWorkletMaxAbs: 0,
      trackReadyState: '',
      trackMuted: false,
      lastError: '',
      createdAtMs: Date.now(),
    };
    entry.requestedTrackId = Array.from(getTrackIdSet(info))[0] || '';

    node.port.onmessage = (ev) => {
      const data = ev.data;
      if (!data) return;
      if (data.type === 'error') {
        if (data.reason === 'sample_rate_mismatch') {
          console.error(
            '[PcmTransport] Worklet reported sample_rate_mismatch for pid',
            pid,
            'actualSampleRate=',
            data.sampleRate
          );
          pcmDetachParticipant(pid);
        }
        return;
      }
      if (data.type !== 'pcm_chunk') return;
      const int16 = data.int16Pcm;
      if (!int16 || !int16.length) return;

      const nowMs = Date.now();
      const payloadMs = (data.sampleCount / 16000) * 1000;
      const chunkEndMs = nowMs;
      const chunkStartMs = nowMs - payloadMs;

      const pcmBase64 = int16ToBase64(int16);
      sendWsPcmChunk(
        pid,
        entry.speakerName,
        entry.isLocal,
        entry.seq++,
        chunkStartMs,
        chunkEndMs,
        pcmBase64
      );
      entry.lastChunkAtMs = nowMs;

      if (entry.chunksLogged < 3) {
        entry.chunksLogged++;
        console.log(
          '[PcmTransport] chunk sent pid=', pid,
          'seq=', entry.seq - 1,
          'samples=', data.sampleCount,
          'payloadMs=', payloadMs.toFixed(0)
        );
      }
    };

    if (!bindEntryToStream(pid, entry, info.mediaStream, BOUND_VIA.WRAPPER)) {
      try { node.port.onmessage = null; } catch (_) { /* ignore */ }
      try { node.disconnect(); } catch (_) { /* ignore */ }
      return;
    }

    // Pump the graph. AudioWorkletNode with numberOfOutputs:0 can be
    // treated as disconnected by Chromium and never get process() ticks
    // unless something downstream drives it. Connecting to destination
    // here is a no-op signal-wise (worklet has no outputs) but keeps
    // the graph alive. See docs/plans/rippling-mixing-giraffe.md POF-5.
    try {
      node.connect(ctx.destination);
    } catch (e) {
      console.warn('[PcmTransport] node.connect(destination) failed:', e?.message || e);
    }
    active.set(pid, entry);
    ensureDiagnosticsTimer();
    console.log('[PcmTransport] Attached participant:', pid, entry.speakerName);

    if (constants.PCM_REBIND_ENABLED) {
      setTimeout(() => {
        const current = active.get(pid);
        if (current) tryRebindToReceiverTrack(pid, current, true, REBIND_REASON.INITIAL_PROBE);
      }, 500);
    }
  } catch (e) {
    console.warn('[PcmTransport] Failed to materialize pid', pid, e);
  }
}

function teardown(pid) {
  const entry = active.get(pid);
  if (!entry) return;
  disconnectSource(entry);
  destroyConsumer(entry);
  try {
    entry.node.port.onmessage = null;
    if (audioContext && audioContext.destination) {
      try { entry.node.disconnect(audioContext.destination); } catch (_) { /* not connected */ }
    }
    entry.node.disconnect();
  } catch (_) {
    /* already disconnected */
  }
  active.delete(pid);
  console.log('[PcmTransport] Detached participant:', pid);
}

export function pcmAttachParticipant(pid, speakerName, isLocal, mediaStream, options = {}) {
  if (!pid || !mediaStream) return;
  // Local mic stays on sounddevice path (D9).
  if (isLocal) return;

  const next = {
    speakerName: speakerName || pid,
    isLocal: !!isLocal,
    mediaStream,
    jitsiTrack: options?.jitsiTrack || null,
    jitsiConf: options?.jitsiConf || null,
    conf: options?.conf || null,
  };

  const existing = pending.get(pid);
  if (existing) {
    existing.speakerName = next.speakerName;
    existing.isLocal = next.isLocal;
    existing.mediaStream = next.mediaStream || existing.mediaStream;
    existing.jitsiTrack = next.jitsiTrack || existing.jitsiTrack;
    existing.jitsiConf = next.jitsiConf || existing.jitsiConf;
    existing.conf = next.conf || existing.conf;

    const entry = active.get(pid);
    if (entry) {
      entry.speakerName = existing.speakerName;
      entry.isLocal = existing.isLocal;
      entry.jitsiTrack = existing.jitsiTrack || entry.jitsiTrack;
      entry.jitsiConf = existing.jitsiConf || entry.jitsiConf;
      entry.conf = existing.conf || entry.conf;
      const ids = getTrackIdSet(existing);
      entry.requestedTrackId = Array.from(ids)[0] || entry.requestedTrackId;
      const newTrack = getAudioTrackFromStream(existing.mediaStream);
      const shouldRebindWrapper =
        existing.mediaStream !== entry.mediaStream ||
        (newTrack && entry.boundTrackId && newTrack.id !== entry.boundTrackId);
      if (shouldRebindWrapper) {
        if (bindEntryToStream(pid, entry, existing.mediaStream, BOUND_VIA.WRAPPER, newTrack)) {
          entry.rebindCount++;
          entry.lastRebindAtMs = Date.now();
          entry.lastRebindReason = REBIND_REASON.JITSI_TRACK_UPDATE;
        }
      }
      tryRebindToReceiverTrack(pid, entry, false, REBIND_REASON.JITSI_UPDATE_RECEIVER_PROBE);
    } else {
      void materialize(pid, existing);
    }
    return;
  }

  pending.set(pid, next);
  void materialize(pid, pending.get(pid));
}

// Update the cached display name for a pid whose PCM transport is already
// attached. Safe to call for unknown pids (no-op).
export function pcmUpdateParticipantName(pid, newName) {
  if (!pid || !newName) return;
  const info = pending.get(pid);
  if (info && info.speakerName !== newName) {
    info.speakerName = newName;
  }
  const entry = active.get(pid);
  if (entry && entry.speakerName !== newName) {
    entry.speakerName = newName;
  }
}

export function pcmDetachParticipant(pid) {
  if (!pid) return;
  pending.delete(pid);
  teardown(pid);
  if (active.size === 0 && diagnosticsTimer) {
    sendDiagnosticsSnapshot('idle snapshot');
    clearInterval(diagnosticsTimer);
    diagnosticsTimer = null;
  }
}

export function pcmAttachAllFromConf(conf) {
  if (!conf) {
    console.warn('[PcmTransport] attachAll: no conf');
    return;
  }
  const localId = conf.localParticipant?.id;

  let inspected = 0;
  let skippedLocal = 0;
  let skippedNoStream = 0;
  let attached = 0;

  conf._participantsMap?.forEach((p) => {
    inspected++;
    const pid = p._id;
    if (!pid || p.local) { skippedLocal++; return; }
    const jitsiTrack = p._audioTrack$?.value?.jitsiTrack;
    const stream = getStreamFromJitsiTrack(jitsiTrack);
    if (!stream) { skippedNoStream++; return; }
    const speakerName = resolveSpeakerName(p, pid);
    pcmAttachParticipant(pid, speakerName, false, stream, {
      jitsiTrack,
      jitsiConf: conf.jitsiConf,
      conf,
    });
    attached++;
  });

  let jitsiInspected = 0;
  let jitsiSkippedNoStream = 0;
  let jitsiAttached = 0;
  const jitsiParticipants = conf.jitsiConf?.getParticipants?.() || [];
  for (const jp of jitsiParticipants) {
    jitsiInspected++;
    const pid = jp._id || jp.getId?.() || jp.id;
    if (!pid || pid === localId || pending.has(pid)) continue;
    const tracks = jp.getTracks?.() || [];
    const audioTrack = tracks.find((t) => t.getType?.() === 'audio' && !isScreenShareTrack(t));
    const stream = getStreamFromJitsiTrack(audioTrack);
    if (!stream) { jitsiSkippedNoStream++; continue; }
    const speakerName =
      jp.getDisplayName?.() || jp._displayName || jp.displayName || jp._name || pid;
    pcmAttachParticipant(pid, speakerName, false, stream, {
      jitsiTrack: audioTrack,
      jitsiConf: conf.jitsiConf,
      conf,
    });
    jitsiAttached++;
  }

  console.log(
    '[PcmTransport] attachAll summary:',
    'participantsMap inspected=', inspected,
    'skippedLocal=', skippedLocal,
    'skippedNoStream=', skippedNoStream,
    'attached=', attached,
    '| jitsiConf inspected=', jitsiInspected,
    'skippedNoStream=', jitsiSkippedNoStream,
    'attached=', jitsiAttached
  );
}

export function pcmTransportDispose() {
  for (const pid of Array.from(active.keys())) {
    teardown(pid);
  }
  active.clear();
  pending.clear();
  if (diagnosticsTimer) {
    clearInterval(diagnosticsTimer);
    diagnosticsTimer = null;
  }
  if (audioContext && audioContext.state !== 'closed') {
    audioContext.close().catch(() => {});
  }
  audioContext = null;
  workletLoadPromise = null;
  lastStatsSentAtMs = 0;
  lastWebrtcStats = createEmptyWebrtcStats();
  console.log('[PcmTransport] Disposed');
}

function resolveSpeakerName(p, fallback) {
  const userInfo = p?._userInfo$?.value;
  if (userInfo) {
    const first = userInfo.firstName || '';
    const last = userInfo.lastName || '';
    if (first && last) return `${first} ${last}`;
    if (first) return first;
  }
  const info = p?._participantInfo$?.value;
  if (info) {
    const first = info.firstName || '';
    const last = info.lastName || '';
    if (first && last) return `${first} ${last}`;
    if (first) return first;
  }
  return p?.name || p?.displayName || fallback;
}

function findPeerConnections(conf, jitsiConf) {
  const roots = [
    conf,
    jitsiConf,
    typeof window !== 'undefined' ? window.$conf : null,
    typeof window !== 'undefined' ? window.APP?.conference?._room : null,
  ];
  const pcs = [];
  const seen = typeof WeakSet !== 'undefined' ? new WeakSet() : null;

  function isPcLike(obj) {
    return obj &&
      typeof obj === 'object' &&
      typeof obj.getStats === 'function' &&
      typeof obj.getReceivers === 'function' &&
      typeof obj.createOffer === 'function';
  }

  function walk(obj, depth) {
    if (!obj || typeof obj !== 'object' || depth > 4) return;
    try {
      if (seen) {
        if (seen.has(obj)) return;
        seen.add(obj);
      }
    } catch (_) {
      /* ignore */
    }
    if (isPcLike(obj)) {
      if (!pcs.includes(obj)) pcs.push(obj);
      return;
    }
    for (const key in obj) {
      if (!Object.prototype.hasOwnProperty.call(obj, key)) continue;
      try { walk(obj[key], depth + 1); } catch (_) { /* ignore */ }
    }
  }

  for (const root of roots) {
    try { walk(root, 0); } catch (_) { /* ignore */ }
  }
  return pcs;
}

function findReceiverTrackByIds(trackIds, conf, jitsiConf) {
  if (!trackIds || trackIds.size === 0) return null;
  const pcs = findPeerConnections(conf, jitsiConf);
  for (const pc of pcs) {
    let receivers = [];
    try { receivers = pc.getReceivers?.() || []; } catch (_) { receivers = []; }
    for (const receiver of receivers) {
      const track = receiver?.track;
      if (track?.kind === 'audio' && trackIds.has(String(track.id))) {
        return track;
      }
    }
  }
  return null;
}

function tryRebindToReceiverTrack(pid, entry, force = false, reason = 'receiver_probe') {
  if (!constants.PCM_REBIND_ENABLED || !entry) return false;
  const now = Date.now();
  const cooldownMs = constants.PCM_REBIND_COOLDOWN_MS ?? 4000;
  if (!force && entry.lastRebindAtMs && now - entry.lastRebindAtMs < cooldownMs) {
    return false;
  }

  const trackIds = getTrackIdSet(entry);
  const receiverTrack = findReceiverTrackByIds(trackIds, entry.conf, entry.jitsiConf);
  if (!receiverTrack) return false;
  if (receiverTrack.readyState === 'ended') {
    entry.lastError = 'receiver_track_ended';
    return false;
  }
  if (entry.boundTrackObject === receiverTrack && entry.boundVia === BOUND_VIA.RECEIVER) {
    return false;
  }

  const stream = new MediaStream([receiverTrack]);
  if (!bindEntryToStream(pid, entry, stream, BOUND_VIA.RECEIVER, receiverTrack)) {
    return false;
  }
  entry.rebindCount++;
  entry.lastRebindAtMs = now;
  entry.lastRebindReason = reason;
  console.log(
    '[PcmTransport] rebound pid=',
    pid,
    'track=',
    receiverTrack.id,
    'reason=',
    reason
  );
  return true;
}

function ensureDiagnosticsTimer() {
  if (!constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED) return;
  if (diagnosticsTimer) return;
  diagnosticsTimer = setInterval(
    () => updateDiagnostics()
      .catch((e) => {
        console.warn('[PcmTransport] diagnostics tick failed:', e?.message || e);
      }),
    constants.PCM_STATS_INTERVAL_MS ?? 1000
  );
}

async function updateDiagnostics() {
  if (active.size === 0) return;
  if (audioContext?.state === 'suspended') {
    try { await audioContext.resume(); } catch (_) { /* ignore */ }
  }

  for (const [pid, entry] of active.entries()) {
    updateEntryEnergy(entry);
    const track = entry.boundTrackObject || getAudioTrackFromStream(entry.mediaStream);
    entry.trackReadyState = track?.readyState || entry.trackReadyState || '';
    entry.trackMuted = track?.muted === true;
    tryRebindToReceiverTrack(pid, entry, false, REBIND_REASON.DIAGNOSTICS_PROBE);
  }

  await updateWebrtcStats();

  const now = Date.now();
  const sendIntervalMs = constants.PCM_STATS_SEND_INTERVAL_MS ?? 2000;
  if (now - lastStatsSentAtMs >= sendIntervalMs) {
    lastStatsSentAtMs = now;
    sendWsPcmTransportStats(pcmGetDiagnostics());
  }
}

function updateEntryEnergy(entry) {
  if (!entry?.analyser || !entry.analyserBuf) return;
  try {
    entry.analyser.getFloatTimeDomainData(entry.analyserBuf);
    let max = 0;
    for (let i = 0; i < entry.analyserBuf.length; i++) {
      const v = entry.analyserBuf[i];
      const abs = v < 0 ? -v : v;
      if (abs > max) max = abs;
    }
    entry.preWorkletMaxAbs = Math.round(max * 32767);
  } catch (_) {
    /* ignore */
  }
}

async function updateWebrtcStats() {
  // Jitsi/KTalk participants share one conf — one walk covers everyone, plus a
  // standalone fallback walk for window.$conf / window.APP roots.
  const firstEntry = active.values().next().value;
  const pcs = findPeerConnections(firstEntry?.conf, firstEntry?.jitsiConf);
  for (const pc of findPeerConnections()) {
    if (!pcs.includes(pc)) pcs.push(pc);
  }
  let bytesReceived = 0;
  let packetsReceived = 0;
  const reports = await Promise.all(
    pcs.map((pc) => (pc?.getStats
      ? pc.getStats(null).catch(() => null)
      : null))
  );
  for (const report of reports) {
    if (!report) continue;
    report.forEach((s) => {
      if (s && s.type === 'inbound-rtp' && (s.kind === 'audio' || s.mediaType === 'audio')) {
        if (typeof s.bytesReceived === 'number') bytesReceived += s.bytesReceived;
        if (typeof s.packetsReceived === 'number') packetsReceived += s.packetsReceived;
      }
    });
  }
  const prevBytes = lastWebrtcStats.inbound_audio_bytes_received || 0;
  const prevPackets = lastWebrtcStats.inbound_audio_packets_received || 0;
  lastWebrtcStats = {
    pc_count: pcs.length,
    inbound_audio_bytes_received: bytesReceived,
    inbound_audio_bytes_delta: Math.max(0, bytesReceived - prevBytes),
    inbound_audio_packets_received: packetsReceived,
    inbound_audio_packets_delta: Math.max(0, packetsReceived - prevPackets),
  };
}

export function pcmGetDiagnostics() {
  const now = Date.now();
  let totalRebinds = 0;
  let silentBindings = 0;
  const perPid = {};

  for (const [pid, entry] of active.entries()) {
    totalRebinds += entry.rebindCount;
    const lastChunkAgeMs = entry.lastChunkAtMs ? now - entry.lastChunkAtMs : null;
    const isSilent = entry.preWorkletMaxAbs < 10 && (lastChunkAgeMs === null || lastChunkAgeMs > 4000);
    if (isSilent) silentBindings++;
    perPid[pid] = {
      speaker_name: entry.speakerName,
      seq: entry.seq,
      bound_via: entry.boundVia,
      bound_track_id: entry.boundTrackId,
      requested_track_id: entry.requestedTrackId,
      rebind_count: entry.rebindCount,
      last_rebind_reason: entry.lastRebindReason,
      pre_worklet_max_abs: entry.preWorkletMaxAbs,
      last_chunk_age_ms: lastChunkAgeMs,
      consumer_state: entry.consumer?.state || 'none',
      track_ready_state: entry.trackReadyState,
      track_muted: entry.trackMuted === true,
      last_error: entry.lastError,
    };
  }

  return {
    enabled: true,
    rebind_enabled: constants.PCM_REBIND_ENABLED !== false,
    diagnostics_enabled: constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED !== false,
    audio_context_state: audioContext?.state || null,
    active_count: active.size,
    pending_count: pending.size,
    pc_count: lastWebrtcStats.pc_count,
    inbound_audio_bytes_delta: lastWebrtcStats.inbound_audio_bytes_delta,
    inbound_audio_packets_delta: lastWebrtcStats.inbound_audio_packets_delta,
    total_rebinds: totalRebinds,
    silent_bindings: silentBindings,
    per_pid: perPid,
  };
}
