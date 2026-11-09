/**
 * Tests for the PCM downsampler AudioWorkletProcessor.
 *
 * The processor runs in an AudioWorkletGlobalScope; the test provides that
 * scope's globals (AudioWorkletProcessor, registerProcessor, sampleRate) and
 * checks the frames the processor posts to the main thread.
 */

import { jest } from '@jest/globals';

const SOURCE_RATE = 48000;
const PAYLOAD = 57600;
const STRIDE = 48000;
const RENDER_QUANTUM = 128;

let Processor;

beforeAll(async () => {
  globalThis.AudioWorkletProcessor = class {
    constructor() {
      this.port = { postMessage: jest.fn() };
    }
  };
  globalThis.registerProcessor = (name, cls) => {
    if (name === 'pcm-downsampler') Processor = cls;
  };
  globalThis.sampleRate = SOURCE_RATE;
  await import('./downsampler.js');
});

beforeEach(() => {
  globalThis.sampleRate = SOURCE_RATE;
  jest.spyOn(console, 'log').mockImplementation(() => {});
  jest.spyOn(console, 'error').mockImplementation(() => {});
});

afterEach(() => {
  jest.restoreAllMocks();
});

// Feed `samples` (a function of the absolute source index) in render quanta.
function feed(processor, total, sample) {
  let offset = 0;
  while (offset < total) {
    const n = Math.min(RENDER_QUANTUM, total - offset);
    const ch = new Float32Array(n);
    for (let i = 0; i < n; i++) ch[i] = sample(offset + i);
    expect(processor.process([[ch]])).toBe(true);
    offset += n;
  }
}

function chunks(processor) {
  return processor.port.postMessage.mock.calls
    .filter(
      ([msg]) => msg.type === 'pcm_chunk'
    );
}

describe('PcmDownsamplerProcessor', () => {
  test('48 kHz context - constructed - posts no error', () => {
    const p = new Processor();
    expect(p.port.postMessage).not.toHaveBeenCalled();
  });

  test('44.1 kHz context - constructed - posts sample_rate_mismatch once and ignores audio', () => {
    globalThis.sampleRate = 44100;
    const p = new Processor();
    expect(p.port.postMessage).toHaveBeenCalledTimes(1);
    expect(p.port.postMessage).toHaveBeenCalledWith({
      type: 'error',
      reason: 'sample_rate_mismatch',
      sampleRate: 44100,
    });
    feed(p, PAYLOAD * 3, () => 0.5);
    expect(p.port.postMessage).toHaveBeenCalledTimes(1);
  });

  test('constant 0.5 for 3.6 s - processed - emits one 3.6 s Int16 chunk with the filtered values', () => {
    const p = new Processor();
    feed(p, PAYLOAD * 3, () => 0.5);
    const sent = chunks(p);
    expect(sent).toHaveLength(1);
    const [msg, transfer] = sent[0];
    expect(msg.sampleCount).toBe(PAYLOAD);
    expect(msg.targetSampleRate).toBe(16000);
    expect(msg.int16Pcm).toBeInstanceOf(Int16Array);
    expect(msg.int16Pcm.length).toBe(PAYLOAD);
    expect(transfer).toEqual([msg.int16Pcm.buffer]);
    // First output sample: 0.25 * 0.5 (filter state starts at zero) * 32767, truncated.
    expect(msg.int16Pcm[0]).toBe(4095);
    // From the second one on the filter is settled: 0.5 * 32767 = 16383.5 → 16383.
    expect(msg.int16Pcm[1]).toBe(16383);
    expect(msg.int16Pcm[PAYLOAD - 1]).toBe(16383);
  });

  test('one sample short of 3.6 s - processed - emits nothing', () => {
    const p = new Processor();
    feed(p, PAYLOAD * 3 - 3, () => 0.5);
    expect(chunks(p)).toHaveLength(0);
  });

  test('a ramp for 6.6 s - processed - second chunk starts 3 s after the first and overlaps it by 0.6 s', () => {
    const p = new Processor();
    // Output sample k comes from source index 3k; a step every 3 source
    // samples makes the filtered value of output k depend on k only.
    const level = (i) => (Math.floor(i / 3) % 1024) / 1024 - 0.5;
    feed(p, (PAYLOAD + STRIDE) * 3, level);
    const sent = chunks(p);
    expect(sent).toHaveLength(2);
    const first = sent[0][0].int16Pcm;
    const second = sent[1][0].int16Pcm;
    expect(Array.from(second.subarray(0, PAYLOAD - STRIDE)))
      .toEqual(Array.from(first.subarray(STRIDE)));
    // Output k (k ≥ 1) filters x[3k-2] = x[3k-1] = level(k-1) and x[3k] = level(k).
    const expected = (k) => Math.trunc((0.75 * level(3 * (k - 1)) + 0.25 * level(3 * k)) * 32767);
    expect(first[10]).toBe(expected(10));
    expect(second[PAYLOAD - 1]).toBe(expected(PAYLOAD + STRIDE - 1));
  });

  test('a signal beyond full scale - processed - clamps to the Int16 range', () => {
    const p = new Processor();
    feed(p, PAYLOAD * 3, (i) => (Math.floor(i / 3) % 2 === 0 ? 4 : -4));
    const pcm = chunks(p)[0][0].int16Pcm;
    // Output 2 filters -4, -4, 4 → -2; output 3 filters 4, 4, -4 → 2.
    expect(pcm[2]).toBe(-32768);
    expect(pcm[3]).toBe(32767);
  });

  test('missing or empty input - processed - keeps the node alive without state change', () => {
    const p = new Processor();
    expect(p.process([])).toBe(true);
    expect(p.process([[]])).toBe(true);
    expect(p.process([[new Float32Array(0)]])).toBe(true);
    feed(p, PAYLOAD * 3, () => 0.5);
    expect(chunks(p)).toHaveLength(1);
  });

  test('first process ticks - processed - logs three ticks only', () => {
    const p = new Processor();
    console.log.mockClear();
    p.process([]);
    p.process([[]]);
    feed(p, RENDER_QUANTUM * 3, () => 0);
    const ticks = console.log.mock.calls
      .filter(
        (args) => args[0] === '[PcmDownsampler] process tick, input.channels='
      );
    expect(ticks).toHaveLength(3);
    expect(ticks[0]).toEqual([
      '[PcmDownsampler] process tick, input.channels=', 0, 'ch0.len=', 0, 'ringFill=', 0,
    ]);
    expect(ticks[1]).toEqual([
      '[PcmDownsampler] process tick, input.channels=', 0, 'ch0.len=', 0, 'ringFill=', 0,
    ]);
    expect(ticks[2]).toEqual([
      '[PcmDownsampler] process tick, input.channels=', 1, 'ch0.len=', RENDER_QUANTUM, 'ringFill=', 0,
    ]);
  });
});
