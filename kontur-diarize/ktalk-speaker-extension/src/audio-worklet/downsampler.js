// PCM Downsampler AudioWorkletProcessor
//
// Runs in AudioWorkletGlobalScope. Receives Float32 samples at 48 kHz,
// applies a 3-tap low-pass (coefs [0.25, 0.5, 0.25]) and decimates ×3 →
// 16 kHz Int16. Holds a ring buffer of PCM_CHUNK_PAYLOAD_S = 3.6 s
// (57600 samples @ 16 kHz) and emits a full payload every
// PCM_CHUNK_DURATION_S = 3.0 s (stride 48000 samples @ 16 kHz). The
// 600 ms overlap between consecutive chunks is required by
// `asr.py::_remove_duplicate_words` on chunk boundaries.
//
// Emits via port.postMessage({type: 'pcm_chunk', int16Pcm, sampleCount}),
// transferring the underlying buffer. Timestamps are stamped on the main
// thread (pcm_transport.js) on reception — worklet intentionally does not
// stamp, so the JS clock used matches the one clock_sync syncs with.
//
// Sample-rate contract: expects sampleRate === 48000 (enforced in ctor).
// If mismatched, emits {type: 'error', reason: 'sample_rate_mismatch'}
// once and disables itself.

const TARGET_SAMPLE_RATE = 16000;
const SOURCE_SAMPLE_RATE = 48000;
const DECIMATION = SOURCE_SAMPLE_RATE / TARGET_SAMPLE_RATE; // 3

const PAYLOAD_SAMPLES = Math.round((TARGET_SAMPLE_RATE * 3600) / 1000); // 57600
const STRIDE_SAMPLES = Math.round((TARGET_SAMPLE_RATE * 3000) / 1000);  // 48000

// Parallel 48 kHz Float32 ring — emitted when processorOptions.capture48k
// is set. Same cadence as the 16 kHz path (3.6 s payload, 3.0 s stride).
const PAYLOAD_SAMPLES_48K = Math.round((SOURCE_SAMPLE_RATE * 3600) / 1000); // 172800
const STRIDE_SAMPLES_48K = Math.round((SOURCE_SAMPLE_RATE * 3000) / 1000);  // 144000

class PcmDownsamplerProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();

    this._disabled = false;
    // Counter for the first few process() calls — the main thread uses
    // these logs to distinguish "worklet instantiated but never driven"
    // from "worklet driven but input empty". Cap so we don't spam logs.
    this._processDebugLeft = 3;

    this._capture48k = !!(options && options.processorOptions && options.processorOptions.capture48k);

    console.log(
      '[PcmDownsampler] constructed, sampleRate=', sampleRate,
      'expected=', SOURCE_SAMPLE_RATE,
      'capture48k=', this._capture48k
    );

    if (sampleRate !== SOURCE_SAMPLE_RATE) {
      console.error(
        `[PcmDownsampler] expected ${SOURCE_SAMPLE_RATE} Hz, got ${sampleRate}`
      );
      this.port.postMessage({
        type: 'error',
        reason: 'sample_rate_mismatch',
        sampleRate,
      });
      this._disabled = true;
      return;
    }

    // 3-tap filter state: x[n-2], x[n-1]
    this._x1 = 0.0;
    this._x2 = 0.0;

    // Decimation phase counter (0..DECIMATION-1). Emit one 16 kHz sample
    // when phase === 0, then advance.
    this._phase = 0;

    this._ring = new Int16Array(PAYLOAD_SAMPLES);
    this._ringWrite = 0;
    this._ringFill = 0;
    this._samplesSinceLastEmit = 0;

    if (this._capture48k) {
      this._ring48k = new Float32Array(PAYLOAD_SAMPLES_48K);
      this._ring48kWrite = 0;
      this._ring48kFill = 0;
      this._samplesSinceLastEmit48k = 0;
    }
  }

  process(inputs) {
    if (this._disabled) return true;

    const input = inputs[0];
    if (this._processDebugLeft > 0) {
      this._processDebugLeft--;
      const ch0 = input && input[0];
      console.log(
        '[PcmDownsampler] process tick, input.channels=', input ? input.length : 0,
        'ch0.len=', ch0 ? ch0.length : 0,
        'ringFill=', this._ringFill
      );
    }
    if (!input || input.length === 0) return true;
    const ch = input[0];
    if (!ch || ch.length === 0) return true;

    const n = ch.length;
    let x1 = this._x1;
    let x2 = this._x2;
    let phase = this._phase;
    let ringWrite = this._ringWrite;
    let ringFill = this._ringFill;
    let sinceEmit = this._samplesSinceLastEmit;

    const cap48k = this._capture48k;
    let r48W = cap48k ? this._ring48kWrite : 0;
    let r48F = cap48k ? this._ring48kFill : 0;
    let sinceEmit48k = cap48k ? this._samplesSinceLastEmit48k : 0;

    for (let i = 0; i < n; i++) {
      const x0 = ch[i];

      if (cap48k) {
        this._ring48k[r48W] = x0;
        r48W = (r48W + 1) % PAYLOAD_SAMPLES_48K;
        if (r48F < PAYLOAD_SAMPLES_48K) r48F++;
        sinceEmit48k++;
        if (sinceEmit48k >= STRIDE_SAMPLES_48K && r48F >= PAYLOAD_SAMPLES_48K) {
          this._emit48k(r48W);
          sinceEmit48k = 0;
        }
      }

      // Low-pass 3-tap: y = 0.25*x[n-2] + 0.5*x[n-1] + 0.25*x[n]
      const y = 0.25 * x2 + 0.5 * x1 + 0.25 * x0;

      x2 = x1;
      x1 = x0;

      if (phase === 0) {
        // Clamp to Int16 range.
        let s = y * 32767;
        if (s > 32767) s = 32767;
        else if (s < -32768) s = -32768;
        this._ring[ringWrite] = s | 0;
        ringWrite = (ringWrite + 1) % PAYLOAD_SAMPLES;
        if (ringFill < PAYLOAD_SAMPLES) ringFill++;
        sinceEmit++;

        if (sinceEmit >= STRIDE_SAMPLES && ringFill >= PAYLOAD_SAMPLES) {
          this._emit(ringWrite);
          sinceEmit = 0;
        }
      }

      phase++;
      if (phase >= DECIMATION) phase = 0;
    }

    this._x1 = x1;
    this._x2 = x2;
    this._phase = phase;
    this._ringWrite = ringWrite;
    this._ringFill = ringFill;
    this._samplesSinceLastEmit = sinceEmit;

    if (cap48k) {
      this._ring48kWrite = r48W;
      this._ring48kFill = r48F;
      this._samplesSinceLastEmit48k = sinceEmit48k;
    }

    return true;
  }

  _emit(ringWriteIdx) {
    // Oldest sample of the payload window sits at ringWriteIdx (next write
    // position); copy PAYLOAD_SAMPLES in order, wrapping around.
    const out = new Int16Array(PAYLOAD_SAMPLES);
    const first = PAYLOAD_SAMPLES - ringWriteIdx;
    out.set(this._ring.subarray(ringWriteIdx, PAYLOAD_SAMPLES), 0);
    if (ringWriteIdx > 0) {
      out.set(this._ring.subarray(0, ringWriteIdx), first);
    }

    this.port.postMessage(
      {
        type: 'pcm_chunk',
        int16Pcm: out,
        sampleCount: PAYLOAD_SAMPLES,
        targetSampleRate: TARGET_SAMPLE_RATE,
      },
      [out.buffer]
    );
  }

  _emit48k(ringWriteIdx) {
    // Raw 48 kHz Float32 — exactly what AudioWorklet gets from the
    // MediaStream before our 3-tap decimation. Same ring geometry as the
    // 16 kHz path: oldest sample at ringWriteIdx, wrap around.
    const out = new Float32Array(PAYLOAD_SAMPLES_48K);
    const first = PAYLOAD_SAMPLES_48K - ringWriteIdx;
    out.set(this._ring48k.subarray(ringWriteIdx, PAYLOAD_SAMPLES_48K), 0);
    if (ringWriteIdx > 0) {
      out.set(this._ring48k.subarray(0, ringWriteIdx), first);
    }

    this.port.postMessage(
      {
        type: 'pcm_chunk_48k',
        float32Pcm: out,
        sampleCount: PAYLOAD_SAMPLES_48K,
        sourceSampleRate: SOURCE_SAMPLE_RATE,
      },
      [out.buffer]
    );
  }
}

registerProcessor('pcm-downsampler', PcmDownsamplerProcessor);
