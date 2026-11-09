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
const audioContexts = [];

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
    if (FakeAnalyser.peak) buf[3] = FakeAnalyser.peak;
  }
}
FakeAnalyser.peak = 0;

class FakeAudioContext {
  constructor() {
    this.state = 'running';
    this.sampleRate = 48000;
    this.baseLatency = 0.01;
    this.destination = {};
    this.audioWorklet = {
      addModule: jest.fn().mockResolvedValue(undefined),
    };
    audioContexts.push(this);
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
    this.connect = jest.fn(() => {
      if (FakeAudioWorkletNode.connectError) throw FakeAudioWorkletNode.connectError;
    });
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
  audioContexts.length = 0;
  FakeAnalyser.peak = 0;
  FakeAudioWorkletNode.connectError = null;
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

test('joining a participant who already shares a screen - attaching from the conference - binds the microphone', async () => {
  const screenTrack = new FakeTrack('track-screen');
  const micTrack = new FakeTrack('track-mic');
  const jitsiScreen = { ...makeJitsiTrack(screenTrack), audioType: 'screenShare' };
  const jitsiMic = { ...makeJitsiTrack(micTrack), audioType: 'microphone' };
  const conf = {
    jitsiConf: {
      getParticipants: () => [{ _id: 'pid-a', getTracks: () => [jitsiScreen, jitsiMic] }],
    },
  };

  pcmTransport.pcmAttachAllFromConf(conf);
  await waitForPid('pid-a');

  expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].bound_track_id).toBe('track-mic');
});

function attachAlice(options = {}, pid = 'pid-a', trackId = 'track-a') {
  const track = new FakeTrack(trackId);
  const stream = new FakeMediaStream([track]);
  pcmTransport.pcmAttachParticipant(pid, 'Alice', false, stream, {
    jitsiTrack: makeJitsiTrack(track, stream),
    ...options,
  });
  return { track, stream };
}

function fakePc(receivers = [], stats = () => new Map()) {
  return {
    createOffer: jest.fn(),
    getReceivers: jest.fn(() => receivers),
    getStats: jest.fn(() => Promise.resolve(stats())),
  };
}

function lastStats() {
  const calls = mockSendWsPcmTransportStats.mock.calls;
  return calls[calls.length - 1][0];
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe('pcm chunk frames', () => {
  test('a worklet chunk - received - sends a pcm frame with name, seq, payload span and base64 samples', async () => {
    attachAlice();
    await waitForPid('pid-a');
    const now = jest.spyOn(Date, 'now').mockReturnValue(1_000_000);
    try {
      workletNodes[0].port.onmessage({
        data: { type: 'pcm_chunk', int16Pcm: new Int16Array([1, -2]), sampleCount: 57600 },
      });
      workletNodes[0].port.onmessage({
        data: { type: 'pcm_chunk', int16Pcm: new Int16Array([0]), sampleCount: 57600 },
      });
    } finally {
      now.mockRestore();
    }
    expect(mockSendWsPcmChunk.mock.calls).toEqual([
      ['pid-a', 'Alice', false, 0, 1_000_000 - 3600, 1_000_000, 'AQD+/w=='],
      ['pid-a', 'Alice', false, 1, 1_000_000 - 3600, 1_000_000, 'AAA='],
    ]);
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].seq).toBe(2);
  });

  test('an empty chunk or a foreign message - received - sends nothing', async () => {
    attachAlice();
    await waitForPid('pid-a');
    const port = workletNodes[0].port;
    port.onmessage({ data: null });
    port.onmessage({ data: { type: 'stats' } });
    port.onmessage({ data: { type: 'pcm_chunk', int16Pcm: new Int16Array(0), sampleCount: 0 } });
    port.onmessage({ data: { type: 'pcm_chunk', sampleCount: 0 } });
    port.onmessage({ data: { type: 'error', reason: 'other' } });
    expect(mockSendWsPcmChunk).not.toHaveBeenCalled();
    expect(pcmTransport.pcmGetDiagnostics().active_count).toBe(1);
  });

  test('worklet reports a sample-rate mismatch - received - detaches the participant', async () => {
    attachAlice();
    await waitForPid('pid-a');
    const error = jest.spyOn(console, 'error').mockImplementation(() => {});
    try {
      workletNodes[0].port.onmessage({
        data: { type: 'error', reason: 'sample_rate_mismatch', sampleRate: 44100 },
      });
    } finally {
      error.mockRestore();
    }
    const diag = pcmTransport.pcmGetDiagnostics();
    expect(diag.active_count).toBe(0);
    expect(diag.pending_count).toBe(0);
    expect(workletNodes[0].port.onmessage).toBeNull();
    expect(document.querySelector('audio[data-recog-pcm-consumer="pid-a"]')).toBeNull();
  });

  test('a renamed participant - next chunk - carries the new name', async () => {
    attachAlice();
    await waitForPid('pid-a');
    pcmTransport.pcmUpdateParticipantName('pid-a', 'Alice Smith');
    pcmTransport.pcmUpdateParticipantName('pid-a', '');
    pcmTransport.pcmUpdateParticipantName('pid-unknown', 'Nobody');
    workletNodes[0].port.onmessage({
      data: { type: 'pcm_chunk', int16Pcm: new Int16Array([0]), sampleCount: 16000 },
    });
    expect(mockSendWsPcmChunk.mock.calls[0][1]).toBe('Alice Smith');
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-unknown']).toBeUndefined();
  });

  test('a participant renamed while the worklet loads - attached - starts with the new name', async () => {
    attachAlice();
    pcmTransport.pcmUpdateParticipantName('pid-a', 'Alice Smith');
    await waitForPid('pid-a');
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].speaker_name).toBe('Alice Smith');
  });
});

describe('attach preconditions', () => {
  test('a local participant or a missing stream - attached - creates nothing', async () => {
    const track = new FakeTrack('track-a');
    pcmTransport.pcmAttachParticipant('pid-me', 'Me', true, new FakeMediaStream([track]));
    pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, null);
    pcmTransport.pcmAttachParticipant('', 'Nobody', false, new FakeMediaStream([track]));
    await flushMaterialize();
    const diag = pcmTransport.pcmGetDiagnostics();
    expect(diag.pending_count).toBe(0);
    expect(diag.active_count).toBe(0);
    expect(workletNodes).toHaveLength(0);
  });

  test('no speaker name - attached - uses the pid as the name', async () => {
    const track = new FakeTrack('track-a');
    pcmTransport.pcmAttachParticipant('pid-a', '', false, new FakeMediaStream([track]));
    await waitForPid('pid-a');
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].speaker_name).toBe('pid-a');
  });

  test('a participant detached while the worklet loads - attached - creates no node', async () => {
    attachAlice();
    pcmTransport.pcmDetachParticipant('pid-a');
    await flushMaterialize();
    expect(workletNodes).toHaveLength(0);
    expect(pcmTransport.pcmGetDiagnostics().active_count).toBe(0);
  });

  test('two attaches while the worklet loads - materialized - create one node', async () => {
    attachAlice();
    attachAlice();
    await waitForPid('pid-a');
    expect(workletNodes).toHaveLength(1);
    expect(pcmTransport.pcmGetDiagnostics().active_count).toBe(1);
  });

  test('AudioContext cannot be created - attached - stays pending without a node', async () => {
    const original = globalThis.AudioContext;
    globalThis.AudioContext = function () { throw new Error('no audio'); };
    const error = jest.spyOn(console, 'error').mockImplementation(() => {});
    try {
      attachAlice();
      await flushMaterialize();
    } finally {
      globalThis.AudioContext = original;
      error.mockRestore();
    }
    expect(workletNodes).toHaveLength(0);
    expect(pcmTransport.pcmGetDiagnostics().pending_count).toBe(1);
    expect(pcmTransport.pcmGetDiagnostics().audio_context_state).toBeNull();
  });

  test('no worklet URL - attached - creates no node', async () => {
    document.documentElement.removeAttribute('data-pcm-worklet-url');
    attachAlice();
    await flushMaterialize();
    expect(workletNodes).toHaveLength(0);
    expect(audioContexts[0].audioWorklet.addModule).not.toHaveBeenCalled();
  });

  test('a worklet URL on window - attached - loads the module from it', async () => {
    window.__PCM_WORKLET_URL__ = '/from-window.js';
    try {
      attachAlice();
      await waitForPid('pid-a');
    } finally {
      delete window.__PCM_WORKLET_URL__;
    }
    expect(audioContexts[0].audioWorklet.addModule).toHaveBeenCalledWith('/from-window.js');
    expect(workletNodes).toHaveLength(1);
  });

  test('the worklet module fails to load - attached again - retries the load', async () => {
    const error = jest.spyOn(console, 'error').mockImplementation(() => {});
    globalThis.AudioContext = class extends FakeAudioContext {
      constructor() {
        super();
        this.audioWorklet.addModule.mockRejectedValueOnce(new Error('404'));
      }
    };
    try {
      attachAlice();
      await flushMaterialize();
    } finally {
      globalThis.AudioContext = FakeAudioContext;
      error.mockRestore();
    }
    expect(workletNodes).toHaveLength(0);
    attachAlice();
    await waitForPid('pid-a');
    expect(audioContexts[0].audioWorklet.addModule).toHaveBeenCalledTimes(2);
    expect(workletNodes).toHaveLength(1);
  });

  test('the source cannot be created - attached - disconnects the node and stays pending', async () => {
    const original = FakeAudioContext.prototype.createMediaStreamSource;
    FakeAudioContext.prototype.createMediaStreamSource = () => { throw new Error('no source'); };
    try {
      attachAlice();
      await flushMaterialize();
    } finally {
      FakeAudioContext.prototype.createMediaStreamSource = original;
    }
    expect(workletNodes).toHaveLength(1);
    expect(workletNodes[0].disconnect).toHaveBeenCalled();
    expect(workletNodes[0].port.onmessage).toBeNull();
    const diag = pcmTransport.pcmGetDiagnostics();
    expect(diag.active_count).toBe(0);
    expect(diag.pending_count).toBe(1);
  });

  test('the source cannot be connected - attached - disconnects source and analyser', async () => {
    const sources = [];
    const original = FakeAudioContext.prototype.createMediaStreamSource;
    FakeAudioContext.prototype.createMediaStreamSource = function (stream) {
      const source = original.call(this, stream);
      source.connect.mockImplementation(() => { throw new Error('connect failed'); });
      sources.push(source);
      return source;
    };
    try {
      attachAlice();
      await flushMaterialize();
    } finally {
      FakeAudioContext.prototype.createMediaStreamSource = original;
    }
    expect(sources).toHaveLength(1);
    expect(sources[0].disconnect).toHaveBeenCalled();
    expect(pcmTransport.pcmGetDiagnostics().active_count).toBe(0);
  });

  test('the node cannot reach the destination - attached - still attaches', async () => {
    FakeAudioWorkletNode.connectError = new Error('no destination');
    attachAlice();
    await waitForPid('pid-a');
    expect(pcmTransport.pcmGetDiagnostics().active_count).toBe(1);
  });
});

describe('rebinding an attached participant', () => {
  test('a new stream that cannot be bound - attached again - keeps the old binding and reports the error', async () => {
    attachAlice();
    await waitForPid('pid-a');
    const original = FakeAudioContext.prototype.createMediaStreamSource;
    FakeAudioContext.prototype.createMediaStreamSource = () => { throw new Error('boom'); };
    try {
      attachAlice({}, 'pid-a', 'track-b');
    } finally {
      FakeAudioContext.prototype.createMediaStreamSource = original;
    }
    const diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
    expect(diag.bound_track_id).toBe('track-a');
    expect(diag.rebind_count).toBe(0);
    expect(diag.last_error).toBe('boom');
  });

  test('a new stream whose track ended - attached again - reports track_ended', async () => {
    attachAlice();
    await waitForPid('pid-a');
    const ended = new FakeTrack('track-b');
    ended.readyState = 'ended';
    const stream = new FakeMediaStream([ended]);
    pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
      jitsiTrack: makeJitsiTrack(ended, stream),
    });
    const diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
    expect(diag.bound_track_id).toBe('track-a');
    expect(diag.last_error).toBe('track_ended');
  });

  test('a receiver track within the cooldown - probed - waits until the cooldown passes', async () => {
    const receivers = [];
    const conf = { pc: fakePc(receivers) };
    attachAlice({ conf });
    await waitForPid('pid-a');
    constants.PCM_REBIND_ENABLED = true;
    receivers.push({ track: new FakeTrack('track-b') });
    const start = Date.now();
    const now = jest.spyOn(Date, 'now').mockReturnValue(start);
    try {
      // The wrapper rebind to track-b starts the cooldown, so the receiver probe right after it waits.
      const { stream } = attachAlice({ conf }, 'pid-a', 'track-b');
      let diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
      expect(diag.bound_via).toBe('wrapper');
      expect(diag.rebind_count).toBe(1);
      expect(diag.last_rebind_reason).toBe('jitsi_track_update');

      now.mockReturnValue(start + 4001);
      pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
        jitsiTrack: makeJitsiTrack(stream.getAudioTracks()[0], stream),
        conf,
      });
      diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
      expect(diag.bound_via).toBe('receiver');
      expect(diag.bound_track_id).toBe('track-b');
      expect(diag.rebind_count).toBe(2);
      expect(diag.last_rebind_reason).toBe('jitsi_track_update_receiver_probe');
    } finally {
      now.mockRestore();
    }
  });

  test('an ended receiver track - probed - stays on the wrapper and reports receiver_track_ended', async () => {
    const receiverTrack = new FakeTrack('track-a');
    receiverTrack.readyState = 'ended';
    const conf = { pc: fakePc([{ track: receiverTrack }]) };
    const { stream, track } = attachAlice({ conf });
    await waitForPid('pid-a');
    constants.PCM_REBIND_ENABLED = true;
    pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
      jitsiTrack: makeJitsiTrack(track, stream),
      conf,
    });
    const diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
    expect(diag.bound_via).toBe('wrapper');
    expect(diag.rebind_count).toBe(0);
    expect(diag.last_error).toBe('receiver_track_ended');
  });

  test('rebinding enabled at attach - half a second later - binds the receiver track', async () => {
    constants.PCM_REBIND_ENABLED = true;
    const conf = { pc: fakePc([{ track: new FakeTrack('track-a') }]) };
    attachAlice({ conf });
    await waitForPid('pid-a');
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].bound_via).toBe('wrapper');
    await sleep(550);
    const diag = pcmTransport.pcmGetDiagnostics().per_pid['pid-a'];
    expect(diag.bound_via).toBe('receiver');
    expect(diag.last_rebind_reason).toBe('initial_receiver_probe');
  });

  test('a receiver on a peer connection under window roots - probed - is found, deep or cyclic', async () => {
    const deepPc = fakePc([{ track: new FakeTrack('track-a') }]);
    const cyclic = { level1: { level2: { level3: { pc: deepPc } } } };
    cyclic.level1.back = cyclic;
    window.$conf = cyclic;
    try {
      const { stream, track } = attachAlice();
      await waitForPid('pid-a');
      constants.PCM_REBIND_ENABLED = true;
      pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
        jitsiTrack: makeJitsiTrack(track, stream),
      });
    } finally {
      delete window.$conf;
    }
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].bound_via).toBe('receiver');
  });

  test('a peer connection nested deeper than four levels - probed - is not found', async () => {
    const pc = fakePc([{ track: new FakeTrack('track-a') }]);
    window.APP = { conference: { _room: { a: { b: { c: { d: { pc } } } } } } };
    try {
      const { stream, track } = attachAlice();
      await waitForPid('pid-a');
      constants.PCM_REBIND_ENABLED = true;
      pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
        jitsiTrack: makeJitsiTrack(track, stream),
      });
    } finally {
      delete window.APP;
    }
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].bound_via).toBe('wrapper');
  });

  test('a peer connection four levels under window.APP - probed - is found', async () => {
    const pc = fakePc([{ track: new FakeTrack('track-a') }]);
    window.APP = { conference: { _room: { a: { b: { c: { pc } } } } } };
    try {
      const { stream, track } = attachAlice();
      await waitForPid('pid-a');
      constants.PCM_REBIND_ENABLED = true;
      pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
        jitsiTrack: makeJitsiTrack(track, stream),
      });
    } finally {
      delete window.APP;
    }
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].bound_via).toBe('receiver');
  });

  test('a peer connection whose receivers throw - probed - is skipped', async () => {
    const pc = fakePc();
    pc.getReceivers.mockImplementation(() => { throw new Error('closed'); });
    const { stream, track } = attachAlice({ conf: { pc } });
    await waitForPid('pid-a');
    constants.PCM_REBIND_ENABLED = true;
    pcmTransport.pcmAttachParticipant('pid-a', 'Alice', false, stream, {
      jitsiTrack: makeJitsiTrack(track, stream),
      conf: { pc },
    });
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].bound_via).toBe('wrapper');
  });
});

describe('hidden audio consumer', () => {
  test('play() resolves - attached - reports the consumer playing with its track id', async () => {
    attachAlice();
    await waitForPid('pid-a');
    const audio = document.querySelector('audio[data-recog-pcm-consumer="pid-a"]');
    expect(audio.getAttribute('data-recog-track-id')).toBe('track-a');
    expect(audio.muted).toBe(true);
    expect(audio.style.left).toBe('-10000px');
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].consumer_state).toBe('playing');
  });

  test('play() rejects - attached - reports play_failed', async () => {
    HTMLMediaElement.prototype.play.mockRejectedValueOnce(new Error('NotAllowedError'));
    const warn = jest.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      attachAlice();
      await waitForPid('pid-a');
    } finally {
      warn.mockRestore();
    }
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].consumer_state).toBe('play_failed');
  });

  test('the audio element cannot be created - attached - reports failed and still attaches', async () => {
    const original = document.createElement.bind(document);
    const create = jest.spyOn(document, 'createElement').mockImplementation((tag) => {
      if (tag === 'audio') throw new Error('blocked');
      return original(tag);
    });
    try {
      attachAlice();
      await waitForPid('pid-a');
    } finally {
      create.mockRestore();
    }
    const diag = pcmTransport.pcmGetDiagnostics();
    expect(diag.active_count).toBe(1);
    expect(diag.per_pid['pid-a'].consumer_state).toBe('failed');
  });

  test('diagnostics disabled - attached - creates no consumer', async () => {
    constants.PCM_WEBRTC_DIAGNOSTICS_ENABLED = false;
    attachAlice();
    await waitForPid('pid-a');
    expect(document.querySelector('audio[data-recog-pcm-consumer="pid-a"]')).toBeNull();
    expect(pcmTransport.pcmGetDiagnostics().per_pid['pid-a'].consumer_state).toBe('none');
  });
});

describe('participants from the KTalk conference map', () => {
  test('participants with various name sources - attached from the conference - named by the first one present', async () => {
    const withTrack = (id, extra) => ({
      _id: id,
      _audioTrack$: { value: { jitsiTrack: makeJitsiTrack(new FakeTrack(`t-${id}`)) } },
      ...extra,
    });
    const conf = {
      localParticipant: { id: 'me' },
      _participantsMap: new Map([
        ['me', withTrack('me', { local: true })],
        ['p1', withTrack('p1', { _userInfo$: { value: { firstName: 'Ann', lastName: 'Lee' } } })],
        ['p2', withTrack('p2', { _userInfo$: { value: { firstName: 'Bo' } } })],
        ['p3', withTrack('p3', {
          _userInfo$: { value: {} },
          _participantInfo$: { value: { firstName: 'Cy', lastName: 'Do' } },
        })],
        ['p4', withTrack('p4', { _participantInfo$: { value: { firstName: 'Di' } } })],
        ['p5', withTrack('p5', { _participantInfo$: { value: { lastName: 'Only' } }, name: 'Eve' })],
        ['p6', withTrack('p6', { displayName: 'Fay' })],
        ['p7', withTrack('p7', {})],
        ['p8', { _id: 'p8', _audioTrack$: { value: { jitsiTrack: null } } }],
        ['x', { _audioTrack$: { value: { jitsiTrack: makeJitsiTrack(new FakeTrack('t-x')) } } }],
      ]),
    };
    pcmTransport.pcmAttachAllFromConf(conf);
    await waitForPid('p7');
    await flushMaterialize();
    const perPid = pcmTransport.pcmGetDiagnostics().per_pid;
    const names = Object.fromEntries(
      Object.entries(perPid)
        .map(
          ([pid, d]) => [pid, d.speaker_name]
        )
    );
    expect(names).toEqual({
      p1: 'Ann Lee', p2: 'Bo', p3: 'Cy Do', p4: 'Di', p5: 'Eve', p6: 'Fay', p7: 'p7',
    });
  });
});

describe('diagnostics tick', () => {
  beforeEach(() => {
    constants.PCM_STATS_INTERVAL_MS = 5;
    constants.PCM_STATS_SEND_INTERVAL_MS = 0;
  });

  test('inbound audio statistics - ticked twice - reports peer connections and byte deltas', async () => {
    let bytes = 1000;
    const stats = () => new Map([
      ['a', { type: 'inbound-rtp', kind: 'audio', bytesReceived: bytes, packetsReceived: 10 }],
      ['b', { type: 'inbound-rtp', mediaType: 'audio', bytesReceived: 500, packetsReceived: 5 }],
      ['c', { type: 'inbound-rtp', kind: 'video', bytesReceived: 9999, packetsReceived: 99 }],
      ['d', { type: 'outbound-rtp', kind: 'audio', bytesReceived: 7, packetsReceived: 7 }],
      ['e', null],
      ['f', { type: 'inbound-rtp', kind: 'audio', bytesReceived: 'n/a' }],
    ]);
    const failing = fakePc();
    failing.getStats.mockImplementation(() => Promise.reject(new Error('closed')));
    attachAlice({ conf: { pc: fakePc([], stats), other: failing } });
    await waitForPid('pid-a');
    await sleep(40);
    let sent = lastStats();
    expect(sent.pc_count).toBe(2);
    expect(sent.inbound_audio_packets_delta).toBe(0);
    const before = mockSendWsPcmTransportStats.mock.calls.length;
    bytes = 1100;
    await sleep(40);
    const deltas = mockSendWsPcmTransportStats.mock.calls.slice(before)
      .map(
        ([s]) => s.inbound_audio_bytes_delta
      );
    // The tick that sees the growth reports it once; later ticks see no change.
    expect(deltas.filter(
      (d) => d !== 0
    )).toEqual([100]);
    sent = lastStats();
    expect(sent.inbound_audio_packets_delta).toBe(0);
  });

  test('a first tick - ticked - counts all bytes received so far as the delta', async () => {
    const stats = () => new Map([
      ['a', { type: 'inbound-rtp', kind: 'audio', bytesReceived: 1000, packetsReceived: 10 }],
      ['b', { type: 'inbound-rtp', mediaType: 'audio', bytesReceived: 500, packetsReceived: 5 }],
    ]);
    attachAlice({ conf: { pc: fakePc([], stats) } });
    await waitForPid('pid-a');
    await sleep(15);
    const first = mockSendWsPcmTransportStats.mock.calls
      .map(
        ([s]) => s
      )
      .find(
        (s) => s.pc_count === 1
      );
    expect(first.inbound_audio_bytes_delta).toBe(1500);
    expect(first.inbound_audio_packets_delta).toBe(15);
  });

  test('a signal before the worklet - ticked - reports its peak as Int16', async () => {
    FakeAnalyser.peak = -0.5;
    attachAlice();
    await waitForPid('pid-a');
    await sleep(30);
    const diag = lastStats().per_pid['pid-a'];
    expect(diag.pre_worklet_max_abs).toBe(16384);
    expect(lastStats().silent_bindings).toBe(0);
  });

  test('silence and no chunk - ticked - counts the binding as silent', async () => {
    attachAlice();
    await waitForPid('pid-a');
    await sleep(30);
    expect(lastStats().per_pid['pid-a'].pre_worklet_max_abs).toBe(0);
    expect(lastStats().silent_bindings).toBe(1);
  });

  test('the bound track mutes and ends - ticked - reports its state', async () => {
    const { track } = attachAlice();
    await waitForPid('pid-a');
    track.muted = true;
    await sleep(30);
    expect(lastStats().per_pid['pid-a'].track_muted).toBe(true);
    track.readyState = 'ended';
    await sleep(30);
    expect(lastStats().per_pid['pid-a'].track_ready_state).toBe('ended');
  });

  test('a suspended AudioContext - ticked - resumes it', async () => {
    attachAlice();
    await waitForPid('pid-a');
    audioContexts[0].state = 'suspended';
    await sleep(30);
    expect(audioContexts[0].state).toBe('running');
    expect(lastStats().audio_context_state).toBe('running');
  });

  test('the send interval not yet elapsed - ticked again - sends no new stats', async () => {
    constants.PCM_STATS_SEND_INTERVAL_MS = 60_000;
    attachAlice();
    await waitForPid('pid-a');
    await sleep(30);
    const count = mockSendWsPcmTransportStats.mock.calls.length;
    expect(count).toBe(1);
    await sleep(30);
    expect(mockSendWsPcmTransportStats.mock.calls.length).toBe(count);
  });

  test('stats sending throws on detach - detached - warns instead of throwing', async () => {
    attachAlice();
    await waitForPid('pid-a');
    mockSendWsPcmTransportStats.mockImplementationOnce(() => { throw new Error('socket gone'); });
    const warn = jest.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      expect(() => pcmTransport.pcmDetachParticipant('pid-a')).not.toThrow();
      expect(warn).toHaveBeenCalledWith('[PcmTransport] diagnostics', 'idle snapshot', 'failed:', 'socket gone');
    } finally {
      warn.mockRestore();
    }
  });
});
