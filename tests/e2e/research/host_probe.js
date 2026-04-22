// Paste this into DevTools console on a KTalk meeting tab where you are
// a participant (host or guest). It attaches an AnalyserNode to every
// remote participant's MediaStream, records RMS amplitude and spectral
// flatness for DURATION_MS at INTERVAL_MS cadence, and returns summary
// stats as JSON.
//
// Nothing leaves the browser — audio bytes are not exfiltrated, only
// numeric aggregates. Safe to paste on a real meeting.
//
// Interpretation (expected):
//   - per-speaker streams: each remote has a distinct rms/flatness profile
//     that matches their audio source (speech → low flatness ~0.01; noise →
//     high flatness ~0.3-0.5).
//   - mixed streams: all remotes show similar, intermediate profiles.

(async (DURATION_MS = 15000, INTERVAL_MS = 250) => {
  const conf = window.$conf;
  if (!conf) return { error: 'no $conf — are you inside a meeting?' };

  const seen = new Set();
  const remotes = [];
  conf._participantsMap?.forEach((p) => {
    if (seen.has(p._id) || p.local) return;
    seen.add(p._id);
    const stream = p._audioTrack$?.value?.jitsiTrack?.stream;
    if (stream && stream.active && stream.getAudioTracks().length) {
      remotes.push({ name: p.name || p.displayName || p._id, stream });
    }
  });
  if (!remotes.length) return { error: 'no remote streams; is anyone else in the room?' };

  const ctx = new AudioContext();
  const probes = remotes.map((r) => {
    const src = ctx.createMediaStreamSource(r.stream);
    const an = ctx.createAnalyser();
    an.fftSize = 1024;
    src.connect(an);
    return {
      name: r.name,
      an,
      tbuf: new Float32Array(an.fftSize),
      fbuf: new Float32Array(an.frequencyBinCount),
      rms: [],
      flat: [],
    };
  });

  const t0 = performance.now();
  while (performance.now() - t0 < DURATION_MS) {
    for (const p of probes) {
      p.an.getFloatTimeDomainData(p.tbuf);
      let sum = 0;
      for (let i = 0; i < p.tbuf.length; i++) sum += p.tbuf[i] * p.tbuf[i];
      p.rms.push(+Math.sqrt(sum / p.tbuf.length).toFixed(5));

      p.an.getFloatFrequencyData(p.fbuf);
      let gSum = 0;
      let aSum = 0;
      let n = 0;
      for (let i = 1; i < Math.min(256, p.fbuf.length); i++) {
        const db = p.fbuf[i];
        if (db > -100) {
          const pw = Math.pow(10, db / 10);
          if (pw > 1e-15) {
            gSum += Math.log(pw);
            aSum += pw;
            n++;
          }
        }
      }
      p.flat.push(n > 10 && aSum > 0 ? +(Math.exp(gSum / n) / (aSum / n)).toFixed(4) : null);
    }
    await new Promise((r) => setTimeout(r, INTERVAL_MS));
  }
  try { await ctx.close(); } catch (_) {}

  return probes.map((p) => {
    const rms = p.rms;
    const flat = p.flat.filter((v) => v !== null).sort((a, b) => a - b);
    return {
      name: p.name,
      samples: rms.length,
      rms_mean: +(rms.reduce((a, b) => a + b, 0) / rms.length).toFixed(5),
      rms_max: Math.max(...rms),
      rms_active_frac: +(rms.filter((v) => v > 0.005).length / rms.length).toFixed(2),
      flat_p25: flat[Math.floor(flat.length * 0.25)] ?? null,
      flat_median: flat[Math.floor(flat.length / 2)] ?? null,
      flat_p75: flat[Math.floor(flat.length * 0.75)] ?? null,
    };
  });
})();
