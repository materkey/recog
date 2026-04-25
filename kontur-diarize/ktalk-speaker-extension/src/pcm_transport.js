// Per-speaker PCM transport.
//
// Per decision D4/D9 in docs/plans/2026-04-22-per-speaker-asr-pipeline.md:
// each remote KTalk participant's audio track is routed through an
// AudioWorkletNode (see src/audio-worklet/downsampler.js) that emits
// 16 kHz Int16 PCM chunks every 3.0 s stride with 3.6 s payload. Chunks
// are base64-encoded and shipped to ktalk_bridge via sendWsPcmChunk().
//
// Gating is server-authoritative (decision D2): the bridge broadcasts
// `pcm_transport_enabled` on each WS connect; we subscribe to state
// transitions via ws_api.js and materialize/teardown worklet nodes when
// the gate flips. Attaching participants before the gate is on is fine —
// we keep their metadata pending and materialize as soon as the gate
// opens.
//
// Local-mic participant is intentionally skipped (decision D9): the host
// tab does not expose its own mic as a remote track and unifying paths
// would add risk without benefit; the sounddevice path stays the source
// of truth for the local mic.

import {
  isPcmTransportEnabled,
  isPcmCapture48kEnabled,
  subscribePcmTransport,
  sendWsPcmChunk,
  sendWsPcmChunk48k,
} from './ws_api.js';

let audioContext = null;
let workletLoadPromise = null;

const pending = new Map(); // pid → {speakerName, isLocal, mediaStream}
const active = new Map();  // pid → {source, node, seq}

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
  return bytesToBase64(new Uint8Array(int16.buffer, int16.byteOffset, int16.byteLength));
}

function float32ToBase64(f32) {
  return bytesToBase64(new Uint8Array(f32.buffer, f32.byteOffset, f32.byteLength));
}

function bytesToBase64(bytes) {
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
    const source = ctx.createMediaStreamSource(info.mediaStream);
    const capture48k = isPcmCapture48kEnabled();
    const node = new AudioWorkletNode(ctx, 'pcm-downsampler', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
      channelCount: 1,
      channelCountMode: 'explicit',
      channelInterpretation: 'discrete',
      processorOptions: { capture48k },
    });

    const entry = { source, node, seq: 0, seq48k: 0, chunksLogged: 0, chunks48kLogged: 0 };

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
      if (data.type === 'pcm_chunk_48k') {
        if (!isPcmCapture48kEnabled()) return;
        const f32 = data.float32Pcm;
        if (!f32 || !f32.length) return;
        const nowMs = Date.now();
        const payloadMs = (data.sampleCount / 48000) * 1000;
        const chunkEndMs = nowMs;
        const chunkStartMs = nowMs - payloadMs;
        sendWsPcmChunk48k(
          pid,
          info.speakerName,
          entry.seq48k++,
          chunkStartMs,
          chunkEndMs,
          float32ToBase64(f32)
        );
        if (entry.chunks48kLogged < 2) {
          entry.chunks48kLogged++;
          console.log(
            '[PcmTransport] 48k chunk sent pid=', pid,
            'seq=', entry.seq48k - 1,
            'samples=', data.sampleCount
          );
        }
        return;
      }
      if (data.type !== 'pcm_chunk') return;
      if (!isPcmTransportEnabled()) return;
      const int16 = data.int16Pcm;
      if (!int16 || !int16.length) return;

      const nowMs = Date.now();
      const payloadMs = (data.sampleCount / 16000) * 1000;
      const chunkEndMs = nowMs;
      const chunkStartMs = nowMs - payloadMs;

      const pcmBase64 = int16ToBase64(int16);
      sendWsPcmChunk(
        pid,
        info.speakerName,
        info.isLocal,
        entry.seq++,
        chunkStartMs,
        chunkEndMs,
        pcmBase64
      );

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

    source.connect(node);
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
    console.log('[PcmTransport] Attached participant:', pid, info.speakerName);
  } catch (e) {
    console.warn('[PcmTransport] Failed to materialize pid', pid, e);
  }
}

function teardown(pid) {
  const entry = active.get(pid);
  if (!entry) return;
  try {
    entry.source.disconnect();
  } catch (_) {
    /* already disconnected */
  }
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

export function pcmAttachParticipant(pid, speakerName, isLocal, mediaStream) {
  if (!pid || !mediaStream) return;
  // Local mic stays on sounddevice path (D9).
  if (isLocal) return;
  if (pending.has(pid)) return;
  pending.set(pid, { speakerName: speakerName || pid, isLocal: !!isLocal, mediaStream });
  if (isPcmTransportEnabled()) {
    void materialize(pid, pending.get(pid));
  }
}

// Update the cached display name for a pid whose PCM transport is already
// attached.  The onmessage closure in `materialize` reads `info.speakerName`
// on every chunk, and `info` is the same object stored in `pending`, so
// mutating the field here propagates to future chunks without re-attaching.
// Safe to call for unknown pids (no-op).
export function pcmUpdateParticipantName(pid, newName) {
  if (!pid || !newName) return;
  const entry = pending.get(pid);
  if (!entry) return;
  if (entry.speakerName === newName) return;
  entry.speakerName = newName;
}

export function pcmDetachParticipant(pid) {
  if (!pid) return;
  pending.delete(pid);
  teardown(pid);
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
    const stream = p._audioTrack$?.value?.jitsiTrack?.stream;
    if (!stream) { skippedNoStream++; return; }
    const speakerName = resolveSpeakerName(p, pid);
    pcmAttachParticipant(pid, speakerName, false, stream);
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
    const audioTrack = tracks.find((t) => t.getType?.() === 'audio');
    const stream = audioTrack?.stream || audioTrack?.getOriginalStream?.();
    if (!stream) { jitsiSkippedNoStream++; continue; }
    const speakerName =
      jp.getDisplayName?.() || jp._displayName || jp.displayName || jp._name || pid;
    pcmAttachParticipant(pid, speakerName, false, stream);
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
  if (audioContext && audioContext.state !== 'closed') {
    audioContext.close().catch(() => {});
  }
  audioContext = null;
  workletLoadPromise = null;
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

// Handle server gate transitions. When the gate flips to true we
// materialize any participants accumulated in `pending` while the gate
// was off. When it flips to false we tear down active worklet nodes but
// keep metadata so we can re-materialize on a later re-enable.
subscribePcmTransport((enabled) => {
  if (enabled) {
    for (const [pid, info] of pending) {
      if (!active.has(pid)) {
        void materialize(pid, info);
      }
    }
  } else {
    for (const pid of Array.from(active.keys())) {
      teardown(pid);
    }
  }
});
