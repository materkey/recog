# KTalk Speaker Noise Detection — Runtime Research

Date: 2026-02-22 (updated with multi-participant data)

## Problem

When a participant rustles papers, types, or makes non-speech noise near their mic,
Jitsi's dominant speaker detection marks them as the active speaker. The transcription
pipeline then attributes ASR output to them — producing garbage lines like
`**Rustler** [HH:MM:SS]: Да.` from noise.

The entire pipeline has zero speech/noise filtering:
Jitsi dominant speaker (energy-only) → Chrome extension → bridge → LiveTranscriber → ASR

## Bug Reproduction

### Root cause

The extension's `getActiveSpeaker()` (`speaker.js:7-26`) iterates `_participantsMap`
with `forEach` and returns the **last** participant with `_activeSpeaker$.value === true`.

Critical finding: `_activeSpeaker$` is `true` for **multiple participants simultaneously**
(observed in 58/60 samples = 97% of the time). This means the "active speaker"
returned by the extension is determined by **Map iteration order**, not by who
is actually speaking.

### Reproduced scenario

Three participants: Host (authenticated), Speaker (speech WAV, peak 0.15),
Rustler (loud pink noise WAV, peak 0.90). Noise 6x louder than speech.

```
t=23.0s  extension sends: Speaker   spkFlat=0.001  rstFlat=0.416  ← correct
t=23.5s  extension sends: Rustler   spkFlat=0.022  rstFlat=0.101  ← BUG!
t=24.0s  extension sends: Rustler   spkFlat=0.003  rstFlat=0.264  ← BUG!
t=24.5s  extension sends: Rustler   spkFlat=0.005  rstFlat=0.551  ← BUG!
t=25.0s  extension sends: Rustler   spkFlat=0.005  rstFlat=0.443  ← BUG!
t=25.5s  extension sends: Speaker   ...                            ← correct
```

Over 30 seconds: extension sent Rustler as speaker **5 times** (8.3%).
Of those, 4 had `flatness >= 0.15` (confirmed noise). **Flatness check would
have blocked all 4** (100% fix rate).

### Why it happens

1. Speech WAV hits a natural pause (silence gap between sentences)
2. Noise continues at constant level
3. Jitsi sets `_activeSpeaker$ = true` for **both** Speaker and Rustler
4. `getActiveSpeaker().forEach` returns Rustler (last in Map iteration)
5. Extension sends `speaker_update` with Rustler's name
6. Bridge forwards to LiveTranscriber → ASR output attributed to "Rustler"

## Test Setup (multi-participant)

Three participants in a KTalk conference:
- **Host**: authenticated user (local participant)
- **Speaker**: guest with `speech_reference.wav` (real Russian speech, 20s from GigaAM)
- **Rustler**: guest with `noise_pink.wav` (synthesized pink noise, 30s continuous)

Guests launched via Playwright with `--use-file-for-fake-audio-capture`.
APIs probed via JavaScript injection in the host's Chrome session.
All data collected on **remote** participant audio tracks (not local mic).

## Detection Methods — Ranked by Effectiveness

### 1. Spectral Flatness via AnalyserNode ⭐ BEST

Per-participant AnalyserNode on `p._audioTrack$.value.jitsiTrack.stream`.

| Metric | Speech | Pink Noise | Separation |
|---|---|---|---|
| **Flatness** (median) | **0.015** | **0.326** | **22x** |
| Flatness (avg) | 0.082 | 0.346 | 4x |
| Flatness (range) | 0.001–0.025 | 0.175–0.502 | Non-overlapping |

**Distribution (60 samples over 30s):**

| Flatness range | Speech | Noise |
|---|---|---|
| < 0.05 | **45** (75%) | 0 |
| 0.05–0.15 | 5 (8%) | 0 |
| 0.15–0.25 | 3 (5%) | **8** (13%) |
| > 0.25 | 7 (12%)* | **52** (87%) |

*Speech samples >0.25 occur during silence/transition frames.

**Threshold: `flatness < 0.15` → speech. Catches 83% of speech, 0% false positives.**

Improved flatness computation (handles silence correctly):

```js
function computeFlatness(analyser) {
  const data = new Float32Array(analyser.frequencyBinCount);
  analyser.getFloatFrequencyData(data);
  const power = [];
  for (let i = 1; i < 256; i++) { // bins 1-255 = ~23Hz to ~6kHz
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
```

### 2. Spectral Centroid — frequency "center of mass"

| Metric | Speech | Pink Noise | Separation |
|---|---|---|---|
| **Centroid** (avg) | **347 Hz** | **1104 Hz** | **3.2x** |
| Centroid (median) | 346 Hz | 1104 Hz | 3.2x |
| Centroid (range) | 120–972 Hz | 643–1591 Hz | Mostly non-overlapping |

Speech energy concentrates in fundamental + harmonics (100–500 Hz).
Noise energy spreads across the full spectrum.

**Threshold: `centroid < 700 Hz` → likely speech.**

### 3. PeerConnection Audio Levels — `pc.getAudioLevels()`

Jitsi's PeerConnection wrapper exposes per-SSRC audio levels:

```js
const pc = Array.from($conf.jitsiConf.rtc.peerConnections.values())[0];
const levels = pc.getAudioLevels(); // { ssrc: level, ... }
```

| Metric | Speech | Pink Noise | Separation |
|---|---|---|---|
| Mean level | 0.052 | 0.012 | 4.3x |
| **CV (coeff of variation)** | **1.18** | **0.19** | **6.2x** |
| Std dev | 0.061 | 0.002 | 30x |
| Range | 0–0.224 | 0.008–0.020 | Different dynamics |

**Key insight**: speech has high variability (silent gaps + loud bursts), noise is constant.
`CV > 0.5` over a sliding window → likely speech.

SSRC mapping via `jitsiParticipant.getTracks()[audioTrack].getSsrc()`.

### 4. WebRTC Stats `audioLevel` — per-receiver RTP stats

```js
const rawPc = pc.peerconnection; // native RTCPeerConnection
const stats = await rawPc.getStats();
stats.forEach(report => {
  if (report.type === 'inbound-rtp' && report.kind === 'audio') {
    // report.audioLevel — instantaneous RMS, 0.0–1.0
    // report.totalAudioEnergy — cumulative energy
    // report.totalSamplesDuration — cumulative time
  }
});
```

| Metric | Speech | Pink Noise | Separation |
|---|---|---|---|
| audioLevel (mean) | **0.202** | **0.045** | **4.5x** |
| audioLevel (std) | 0.208 | 0.006 | 35x |
| audioLevel (range) | 0.0004–0.753 | 0.036–0.058 | Very different |
| avgLevel (energy/duration) | 0.219 | 0.044 | 5x |

Also available per-packet via `RTCRtpReceiver.getSynchronizationSources()`:
```js
const receivers = rawPc.getReceivers().filter(r => r.track.kind === 'audio');
// receiver.getSynchronizationSources() → [{ source: ssrc, audioLevel: 0.0-1.0 }]
```

### 5. `_highAudioLevel$` Observable — toggle frequency

KTalk's `_highAudioLevel$` (BehaviorSubject\<boolean\>) on each remote participant.

| Metric | Speech | Pink Noise |
|---|---|---|
| Toggle events (30s) | **162** (rapidly toggling) | **1** (stays true) |
| True events | 81 (50%) | 1 (100%) |

Speech causes rapid true/false toggling (syllables + pauses).
Noise stays permanently true (constant energy above threshold).

**Detection: `toggleCount > 5 in 5s window` → speech. `toggleCount <= 1` → noise.**

### 6. Zero-Crossing Rate (ZCR) — time-domain

| Metric | Speech | Pink Noise | Separation |
|---|---|---|---|
| ZCR (avg) | **0.019** | **0.156** | **8.2x** |
| ZCR (range) | 0.006–0.044 | 0.119–0.206 | Non-overlapping |

Speech has low ZCR (periodic voiced signals). Noise has high ZCR (random crossings).

**Threshold: `ZCR > 0.08` → likely noise.**

### 7. `conference.dominantSpeaker` Event — enriched signature

```js
$conf.jitsiConf.on('conference.dominantSpeaker',
  (participantId, rankedOthers, isSilent) => { ... });
```

The 3rd argument `isSilent` maps to `jitsiConf.dominantSpeakerIsSilent`.
In our test: dominant speaker alternated between Host and Speaker only — Rustler
was **never** the dominant speaker (Jitsi's energy detection favored speech peaks).

**Note**: this worked in our test because speech peaks > noise amplitude.
In real scenarios with loud rustling, Jitsi would still misattribute.

## APIs That Did NOT Work

### `_audioLevel$` — local-only

`_audioLevel$` (BehaviorSubject\<number\>) only exists on the **local** participant.
Remote participants have it as `undefined`. Use `_highAudioLevel$` (boolean) instead,
or `pc.getAudioLevels()` / WebRTC stats for numeric values.

### `conference.audioLevelsChanged` — did not fire

Subscribed in a 3-participant conference. Event never fired.
May require specific Jitsi server configuration to enable per-participant level reporting.

### VAD Events — did not fire

`detection.vad_score_published`, `detection.vad_noise_device`, `vad-report-published`
all exist as event constants but never fired in a real conference with audio.
`createTrackVADEmitter` exists on `$conf.jitsiMeetApi` but appears to require
explicit initialization/configuration. Not usable without KTalk server changes.

## Full Observable Map (remote participants)

All RxJS BehaviorSubjects on remote participant objects:

| Observable | Type | Speech vs Noise Useful? |
|---|---|---|
| `_activeSpeaker$` | boolean | No (both can be true) |
| `_highAudioLevel$` | boolean | **Yes** (toggle frequency) |
| `_audioVolume$` | number | No (always 1, volume setting) |
| `_audioTrack$` | object | **Yes** (→ MediaStream → AnalyserNode) |
| `_audioMuted$` | boolean | No (mute state, not speech/noise) |
| `_connectionStatus$` | string | No (always "active") |
| `_statistic$` | object | No (media/bitrate/quality, no audio levels) |
| `_participantInfo$` | object | No (user metadata) |
| `_userInfo$` | object | No (user metadata) |
| `_noSoundFromMic$` | boolean | No (undefined on remotes) |
| `_handRaised$` | boolean | No |
| `_pinned$` | boolean | No |
| `_clientFeatures$` | object | No |

**Note**: `_audioLevel$` only exists on local participant (float 0-1, ~200ms updates).

## Recommended Detection Strategy

### Primary: Spectral Flatness (AnalyserNode)

Best discriminator. Non-overlapping ranges. Works on remote tracks through WebRTC.

```
if (flatness < 0.15) → SPEECH
if (flatness >= 0.15 && flatness < 0.25) → AMBIGUOUS (use secondary signals)
if (flatness >= 0.25) → NOISE
```

### Secondary: Level Variability (pcLevel CV or _highAudioLevel$ toggles)

For ambiguous flatness, check audio level dynamics:
- Speech: pcLevel CV > 0.5 over 5s window, or _highAudioLevel$ toggles > 5
- Noise: pcLevel CV < 0.3, or _highAudioLevel$ stays constant

### Tertiary: ZCR + Centroid

Additional confirmation:
- Speech: ZCR < 0.08, centroid < 700 Hz
- Noise: ZCR > 0.08, centroid > 700 Hz

### Combined classifier (all signals):

```js
function isSpeech(flatness, pcLevelCV, zcr, centroid) {
  // Strong speech: flatness alone is sufficient
  if (flatness >= 0 && flatness < 0.10) return true;
  // Strong noise: flatness alone is sufficient
  if (flatness > 0.30) return false;
  // Ambiguous: use secondary signals
  const speechVotes =
    (flatness < 0.15 ? 1 : 0) +
    (pcLevelCV > 0.5 ? 1 : 0) +
    (zcr < 0.08 ? 1 : 0) +
    (centroid < 700 ? 1 : 0);
  return speechVotes >= 2;
}
```

## Architecture for Fix

### Where to implement

The extension (`kontur-diarize/ktalk-speaker-extension/`) is the right place because:
1. It already has access to participant objects and Jitsi tracks
2. It controls what `speaker_update` messages are sent to the bridge
3. Per-participant MediaStream is available via `_audioTrack$.value.jitsiTrack.stream`
4. PeerConnection accessible via `$conf.jitsiConf.rtc.peerConnections`

### Proposed approach

For each participant, create an AnalyserNode on their audio track.
When a `speaker_update` is about to be sent, check the current speaker's flatness.
Only send the update if the speaker is producing actual speech.

```
participant._audioTrack$ → jitsiTrack.stream → AnalyserNode
  → getFloatFrequencyData() → compute flatness
  → if flatness < 0.15: sendWsSpeakerUpdate(speaker)
  → if flatness >= 0.15: suppress (noise, not speech)
```

### Alternative: Duration gate + flatness

```
on dominantSpeaker change:
  start 300ms timer
  during timer: sample flatness 3 times
  if majority < 0.15: confirm as speech, send speaker_update
  else: suppress
```

## Files involved

- `kontur-diarize/ktalk-speaker-extension/src/speaker.js` — `updateSpeakerStatus()` → `sendWsSpeakerUpdate(activeSpeaker)` (line 349, unconditional)
- `kontur-diarize/ktalk-speaker-extension/src/platforms/ktalk.js` — `subscribeToSpeakerChanges()`, Jitsi event subscriptions
- `kontur-diarize/ktalk-speaker-extension/src/ws_api.js` — WebSocket protocol to bridge
- `kontur-diarize/ktalk-speaker-extension/src/constants.js` — thresholds
- `ktalk_bridge.py` — bridge receives speaker_update, could add defense-in-depth

## Raw Data Reference

### Time-series summary (30s, 60 samples at 500ms)

**Speaker (speech_reference.wav):**
- Samples with signal: 15/40 (speech has gaps), all 60 tracked by pcLevel
- Flatness when active: avg 0.011, range 0.005–0.025
- pcLevel: avg 0.052, std 0.061, CV=1.18
- WebRTC audioLevel: avg 0.202, std 0.208, range 0.0004–0.753
- Centroid: avg 347 Hz, ZCR: avg 0.019
- _activeSpeaker$: true 59/60 samples
- _highAudioLevel$: true 28/60 samples (toggles with speech dynamics)
- dominantSpeaker: 60/60 samples (always dominant when speaking)

**Rustler (noise_pink.wav):**
- Samples with signal: 40/40 (constant noise)
- Flatness: avg 0.346, range 0.238–0.454
- pcLevel: avg 0.012, std 0.002, CV=0.19
- WebRTC audioLevel: avg 0.045, std 0.006, range 0.036–0.058
- Centroid: avg 1104 Hz, ZCR: avg 0.156
- _activeSpeaker$: true 60/60 samples (Jitsi thinks it's always speaking!)
- _highAudioLevel$: true 60/60 (stays true, 1 toggle event total)
- dominantSpeaker: 0/60 (never won dominant speaker in this test)
