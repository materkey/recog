/**
 * Tests for browser-side per-speaker PCM transport hardening.
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';

const mockSendWsPcmChunk = jest.fn();
const mockSendWsPcmTransportStats = jest.fn();

jest.unstable_mockModule('./ws_api.js', () => ({
  sendWsPcmChunk: mockSendWsPcmChunk,
  sendWsPcmTransportStats: mockSendWsPcmTransportStats,
}));

const workletNodes = [];

class FakeMediaStream {
  constructor(tracks = []) {
    this._tracks = tracks;
  }

  getAudioTracks() {
    return this._tracks.filter((track) => track.kind === 'audio');
  }
}

class FakeTrack {
  constructor(id) {
    this.id = id;
    this.kind = 'audio';
    this.readyState = 'live';
    this.muted = false;
  }
}

class FakeSource {
  constructor(stream) {
    this.stream = stream;
    this.connect = jest.fn();
    this.disconnect = jest.fn();
  }
}

class FakeAnalyser {
  constructor() {
    this.fftSize = 512;
    this.smoothingTimeConstant = 0;
    this.connect = jest.fn();
    this.disconnect = jest.fn();
  }

  getFloatTimeDomainData(buf) {
    buf.fill(0);
  }
}

class FakeAudioContext {
  constructor() {
    this.state = 'running';
    this.sampleRate = 48000;
    this.baseLatency = 0.01;
    this.destination = {};
    this.audioWorklet = {
      addModule: jest.fn().mockResolvedValue(undefined),
    };
  }

  createMediaStreamSource(stream) {
    return new FakeSource(stream);
  }

  createAnalyser() {
    return new FakeAnalyser();
  }

  resume() {
    this.state = 'running';
    return Promise.resolve();
  }

  close() {
    this.state = 'closed';
    return Promise.resolve();
  }
}

class FakeAudioWorkletNode {
  constructor() {
    this.port = { onmessage: null };
    this.connect = jest.fn();
    this.disconnect = jest.fn();
    workletNodes.push(this);
  }
}

function makeJitsiTrack(track, stream = new FakeMediaStream([track])) {
  return {
    track,
    stream,
    getTrack: () => track,
    getType: () => 'audio',
    getOriginalStream: () => stream,
  };
}

async function flushMaterialize() {
  for (let i = 0; i < 5; i++) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function waitForPid(pid) {
  await flushMaterialize();
  for (let i = 0; i < 10; i++) {
    if (pcmTransport.pcmGetDiagnostics().per_pid[pid]) return;
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

let pcmTransport;
let constants;

beforeAll(async () => {
  globalThis.MediaStream = FakeMediaStream;
  globalThis.AudioContext = FakeAudioContext;
  globalThis.AudioWorkletNode = FakeAudioWorkletNode;
  Object.defineProperty(HTMLMediaElement.prototype, 'play', {
    configurable: true,
    value: jest.fn().mockResolvedValue(undefined),
  });
  Object.defineProperty(HTMLMediaElement.prototype, 'pause', {
    configurable: true,
    value: jest.fn(),
  });
  Object.defineProperty(HTMLMediaElement.prototype, 'srcObject', {
    configurable: true,
    get() {
      return this._srcObject || null;
    },
    set(value) {
      this._srcObject = value;
    },
  });

  constants = (await import('./state.js')).constants;
  pcmTransport = await import('./pcm_transport.js');
});

beforeEach(() => {
  document.body.innerHTML = '';
  document.documentElement.setAttribute('data-pcm-worklet-url', '/downsampler.js');
  mockSendWsPcmChunk.mockClear();
  mockSendWsPcmTransportStats.mockClear();
  workletNodes.length = 0;
  constants.PCM_REBIND_ENABLED = false;
  constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED = true;
  constants.PCM_STATS_INTERVAL_MS = 60_000;
  constants.PCM_STATS_SEND_INTERVAL_MS = 60_000;
});

afterEach(() => {
  pcmTransport.pcmTransportDispose();
});

test('creates and removes hidden audio consumer for remote stream', async () => {
  const track = new FakeTrack('track-a');
  const stream = new FakeMediaStream([track]);

  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
    jitsiTrack: makeJitsiTrack(track, stream),
  });
  await waitForPid('pid-a');

  const consumer = document.querySelector('audio[data-recog-pcm-consumer="pid-a"]');
  expect(consumer).not.toBeNull();
  expect(consumer.srcObject).toBe(stream);

  pcmTransport.pcmDetachParticipant('pid-a');
  expect(document.querySelector('audio[data-recog-pcm-consumer="pid-a"]')).toBeNull();
});

test('repeated attach for same pid does not create duplicate consumer or worklet', async () => {
  const track = new FakeTrack('track-a');
  const stream = new FakeMediaStream([track]);
  const jitsiTrack = makeJitsiTrack(track, stream);

  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, { jitsiTrack });
  await waitForPid('pid-a');
  pcmTransport.pcmAttachParticipant('pid-a', 'Alice A.', false, stream, { jitsiTrack });
  await waitForPid('pid-a');

  expect(document.querySelectorAll('audio[data-recog-pcm-consumer="pid-a"]')).toHaveLength(1);
  expect(workletNodes).toHaveLength(1);
  expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].speaker_name).toBe('Alice A.');
});

test('retries materialize after initial bind sees ended track', async () => {
  const endedTrack = new FakeTrack('track-ended');
  endedTrack.readyState = 'ended';
  const staleStream = new FakeMediaStream([endedTrack]);

  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, staleStream, {
    jitsiTrack: makeJitsiTrack(endedTrack, staleStream),
  });
  await flushMaterialize();

  expect(pcmTransport.pcmGetDiagnostics().active_count).toBe(0);
  expect(pcmTransport.pcmGetDiagnostics().pending_count).toBe(1);

  const liveTrack = new FakeTrack('track-live');
  const liveStream = new FakeMediaStream([liveTrack]);
  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, liveStream, {
    jitsiTrack: makeJitsiTrack(liveTrack, liveStream),
  });
  await waitForPid('pid-a');

  const diag = pcmTransport.pcmGetDiagnostics();
  expect(diag.active_count).toBe(1);
  expect(diag.per_pid['pid-a'].bound_track_id).toBe('track-live');
});

test('sends zero diagnostics snapshot when final participant detaches', async () => {
  const track = new FakeTrack('track-a');
  const stream = new FakeMediaStream([track]);

  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
    jitsiTrack: makeJitsiTrack(track, stream),
  });
  await waitForPid('pid-a');
  mockSendWsPcmTransportStats.mockClear();

  pcmTransport.pcmDetachParticipant('pid-a');

  expect(mockSendWsPcmTransportStats).toHaveBeenCalledTimes(1);
  expect(mockSendWsPcmTransportStats).toHaveBeenCalledWith(expect.objectContaining({
    active_count: 0,
    pending_count: 0,
    per_pid: {},
  }));
});

test('track update preserves pcm sequence while replacing binding', async () => {
  const trackA = new FakeTrack('track-a');
  const streamA = new FakeMediaStream([trackA]);
  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, streamA, {
    jitsiTrack: makeJitsiTrack(trackA, streamA),
  });
  await waitForPid('pid-a');

  workletNodes[0].port.onmessage({
    data: { type: 'pcm_chunk', int16Pcm: new Int16Array([1, 2, 3]), sampleCount: 3 },
  });

  const trackB = new FakeTrack('track-b');
  const streamB = new FakeMediaStream([trackB]);
  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, streamB, {
    jitsiTrack: makeJitsiTrack(trackB, streamB),
  });

  const diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
  expect(diag.seq).toBe(1);
  expect(diag.bound_track_id).toBe('track-b');
  expect(diag.rebind_count).toBe(1);
  expect(mockSendWsPcmChunk).toHaveBeenCalledTimes(1);
});

test('exact receiver-track match rebinds to receiver object', async () => {
  const wrapperTrack = new FakeTrack('track-a');
  const receiverTrack = new FakeTrack('track-a');
  const stream = new FakeMediaStream([wrapperTrack]);
  const pc = {
    createOffer: jest.fn(),
    getSenders: jest.fn(() => []),
    getReceivers: jest.fn(() => [{ track: receiverTrack }]),
    getStats: jest.fn(),
  };
  const conf = { pc };

  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
    jitsiTrack: makeJitsiTrack(wrapperTrack, stream),
    conf,
  });
  await waitForPid('pid-a');

  constants.PCM_REBIND_ENABLED = true;
  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
    jitsiTrack: makeJitsiTrack(wrapperTrack, stream),
    conf,
  });

  const diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
  expect(diag.bound_via).toBe('receiver');
  expect(diag.bound_track_id).toBe('track-a');
  expect(diag.rebind_count).toBe(1);
});

test('receiver rebind is skipped when no exact track match exists', async () => {
  const wrapperTrack = new FakeTrack('track-a');
  const receiverTrack = new FakeTrack('track-b');
  const stream = new FakeMediaStream([wrapperTrack]);
  const pc = {
    createOffer: jest.fn(),
    getSenders: jest.fn(() => []),
    getReceivers: jest.fn(() => [{ track: receiverTrack }]),
    getStats: jest.fn(),
  };
  const conf = { pc };

  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
    jitsiTrack: makeJitsiTrack(wrapperTrack, stream),
    conf,
  });
  await waitForPid('pid-a');

  constants.PCM_REBIND_ENABLED = true;
  pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
    jitsiTrack: makeJitsiTrack(wrapperTrack, stream),
    conf,
  });

  const diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
  expect(diag.bound_via).toBe('wrapper');
  expect(diag.bound_track_id).toBe('track-a');
  expect(diag.rebind_count).toBe(0);
});
