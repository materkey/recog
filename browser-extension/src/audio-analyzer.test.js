/**
 * Tests for the spectral-flatness noise filter (audio-analyzer.js).
 *
 * @jest-environment jsdom
 */

import { jest } from '@jest/globals';

const contexts = [];

class FakeAnalyser {
  constructor() {
    this.frequencyBinCount = 256;
    this.spectrum = null;
  }

  getFloatFrequencyData(data) {
    data.fill(-Infinity);
    if (this.spectrum) this.spectrum(data);
  }
}

class FakeAudioContext {
  constructor() {
    this.state = 'running';
    this.sources = [];
    this.analysers = [];
    this.close = jest.fn(() => {
      this.state = 'closed';
      return Promise.resolve();
    });
    contexts.push(this);
  }

  createMediaStreamSource(stream) {
    if (stream.broken) throw new Error('not a media stream');
    const source = { stream, connect: jest.fn(), disconnect: jest.fn() };
    this.sources.push(source);
    return source;
  }

  createAnalyser() {
    const analyser = new FakeAnalyser();
    this.analysers.push(analyser);
    return analyser;
  }
}

// Every bin from 1 to 255 at the same level: flatness 1 (white noise).
const flat = (data) => data.fill(-20);
// Ten loud bins over a quiet floor: flatness close to 0 (voice-like).
const tonal = (data) => {
  data.fill(-90);
  for (let i = 1; i <= 10; i++) data[i] = 0;
};
// Five audible bins only: too few to judge.
const sparse = (data) => {
  for (let i = 1; i <= 5; i++) data[i] = 0;
};

let analyzer;

beforeAll(async () => {
  globalThis.AudioContext = FakeAudioContext;
  analyzer = await import('./audio-analyzer.js');
});

beforeEach(() => {
  contexts.length = 0;
  jest.spyOn(console, 'log').mockImplementation(() => {});
  jest.spyOn(console, 'warn').mockImplementation(() => {});
});

afterEach(() => {
  analyzer.dispose();
  jest.restoreAllMocks();
});

function attach(pid, spectrum) {
  analyzer.attachParticipant(pid, { id: `stream-${pid}` });
  const ctx = contexts[contexts.length - 1];
  const analyser = ctx.analysers[ctx.analysers.length - 1];
  analyser.spectrum = spectrum;
  return { ctx, analyser };
}

describe('attachParticipant', () => {
  test('a participant with a stream - attached - connects a 512-point analyser to its source', () => {
    const { ctx, analyser } = attach('p1', flat);
    expect(ctx.sources).toHaveLength(1);
    expect(ctx.sources[0].connect).toHaveBeenCalledWith(analyser);
    expect(analyser.fftSize).toBe(512);
    expect(analyser.smoothingTimeConstant).toBe(0.3);
  });

  test('the same participant twice - attached - keeps one source', () => {
    attach('p1', flat);
    analyzer.attachParticipant('p1', { id: 'another' });
    expect(contexts).toHaveLength(1);
    expect(contexts[0].sources).toHaveLength(1);
  });

  test('no participant id or no stream - attached - analyses nothing', () => {
    analyzer.attachParticipant('', { id: 's' });
    analyzer.attachParticipant('p1', null);
    expect(contexts).toHaveLength(0);
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'no_analyser' });
  });

  test('a stream the context rejects - attached - warns and analyses nothing', () => {
    analyzer.attachParticipant('p1', { broken: true });
    expect(console.warn).toHaveBeenCalled();
    expect(analyzer.getFlatness('p1')).toBeNull();
  });
});

describe('getFlatness', () => {
  test('white noise - measured - is 1', () => {
    attach('p1', flat);
    expect(analyzer.getFlatness('p1')).toBeCloseTo(1, 10);
  });

  test('a voice-like spectrum - measured - is close to 0', () => {
    attach('p1', tonal);
    const flatness = analyzer.getFlatness('p1');
    expect(flatness).toBeGreaterThan(0);
    expect(flatness).toBeLessThan(1e-6);
  });

  test('no signal or fewer than ten audible bins - measured - is -1', () => {
    attach('p1', null);
    attach('p2', sparse);
    expect(analyzer.getFlatness('p1')).toBe(-1);
    expect(analyzer.getFlatness('p2')).toBe(-1);
  });

  test('an unknown participant - measured - is null', () => {
    expect(analyzer.getFlatness('nobody')).toBeNull();
  });
});

describe('isSpeech', () => {
  test('silence - asked - lets it through as silence without filling the history', () => {
    const { analyser } = attach('p1', null);
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'silence' });
    analyser.spectrum = flat;
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'warmup' });
  });

  test('a suspended context - asked - lets it through', () => {
    const { ctx } = attach('p1', flat);
    ctx.state = 'suspended';
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'ctx_suspended' });
  });

  test('noise then voice - asked repeatedly - votes by the median of the last three', () => {
    const { analyser } = attach('p1', flat);
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'warmup' });
    expect(analyzer.isSpeech('p1')).toEqual({ speech: false, reason: 'noise' });
    analyser.spectrum = tonal;
    // History [noise, noise, voice]: the median is noise.
    expect(analyzer.isSpeech('p1')).toEqual({ speech: false, reason: 'noise' });
    // History [noise, voice, voice]: the median is voice.
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'speech' });
    // History [voice, voice, voice] after the oldest noise drops out.
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'speech' });
    analyser.spectrum = flat;
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'speech' });
    expect(analyzer.isSpeech('p1')).toEqual({ speech: false, reason: 'noise' });
  });
});

describe('detachParticipant and dispose', () => {
  test('an attached participant - detached - disconnects it and forgets it', () => {
    const { ctx } = attach('p1', flat);
    analyzer.detachParticipant('p1');
    expect(ctx.sources[0].disconnect).toHaveBeenCalled();
    expect(analyzer.isSpeech('p1')).toEqual({ speech: true, reason: 'no_analyser' });
    expect(() => analyzer.detachParticipant('p1')).not.toThrow();
  });

  test('a source that throws on disconnect - detached - is still forgotten', () => {
    const { ctx } = attach('p1', flat);
    ctx.sources[0].disconnect.mockImplementation(() => { throw new Error('gone'); });
    analyzer.detachParticipant('p1');
    expect(analyzer.getFlatness('p1')).toBeNull();
  });

  test('two participants - disposed - disconnects both, closes the context and opens a new one next time', () => {
    const { ctx } = attach('p1', flat);
    attach('p2', flat);
    ctx.sources[1].disconnect.mockImplementation(() => { throw new Error('gone'); });
    analyzer.dispose();
    expect(ctx.sources[0].disconnect).toHaveBeenCalled();
    expect(ctx.close).toHaveBeenCalledTimes(1);
    expect(analyzer.getFlatness('p1')).toBeNull();
    attach('p1', flat);
    expect(contexts).toHaveLength(2);
  });

  test('a context closed by the page - attached again - opens a new context', () => {
    const { ctx } = attach('p1', flat);
    ctx.state = 'closed';
    attach('p2', flat);
    expect(contexts).toHaveLength(2);
    analyzer.dispose();
    expect(ctx.close).not.toHaveBeenCalled();
  });
});

describe('attachAllFromConf', () => {
  test('a conference with KTalk and Jitsi participants - attached - analyses each remote microphone once', () => {
    const stream = (id) => ({ id });
    const conf = {
      _participantsMap: new Map([
        ['me', { _id: 'me', local: true, _audioTrack$: { value: { jitsiTrack: { stream: stream('me') } } } }],
        ['k1', { _id: 'k1', _audioTrack$: { value: { jitsiTrack: { stream: stream('k1') } } } }],
        ['k2', { _id: 'k2', _audioTrack$: { value: null } }],
        ['nameless', { _audioTrack$: { value: { jitsiTrack: { stream: stream('x') } } } }],
      ]),
      jitsiConf: {
        getParticipants: () => [
          { _id: 'k1', getTracks: () => [{ getType: () => 'audio', stream: stream('k1-again') }] },
          {
            getId: () => 'j1',
            getTracks: () => [
              { getType: () => 'audio', audioType: 'screenShare', stream: stream('j1-screen') },
              { getType: () => 'audio', audioType: 'microphone', getOriginalStream: () => stream('j1-mic') },
            ],
          },
          { id: 'j2', getTracks: () => [{ getType: () => 'video', stream: stream('j2-video') }] },
          { id: 'j3' },
          {},
        ],
      },
    };
    analyzer.attachAllFromConf(conf);
    const attached = contexts[0].sources
      .map(
        (s) => s.stream.id
      );
    expect(attached).toEqual(['k1', 'j1-mic']);
  });

  test('no conference - attached - does nothing', () => {
    expect(() => analyzer.attachAllFromConf(null)).not.toThrow();
    analyzer.attachAllFromConf({});
    expect(contexts).toHaveLength(0);
  });
});
