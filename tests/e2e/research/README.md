# KTalk per-speaker stream research probes

One-off scripts that validate whether KTalk's Jitsi-based SFU exposes each
remote participant's audio as a **separate MediaStreamTrack** (the theory
behind a per-speaker transcription pipeline), as opposed to a single mixed
track.

These are **research artefacts**, not regression tests. They run against a
live room and a live account.

## What's here

| File | Purpose |
|---|---|
| `launch_guests.py` | Playwright driver — launches N Chromium instances with fake audio (loops a WAV file into the fake mic) and joins them into a room. Purely fixture-driven, no credentials or account data. |
| `host_probe.js` | Browser-side JS snippet — paste into DevTools on a meeting tab to collect RMS + spectral flatness per remote participant. Returns summary JSON. No audio bytes are exfiltrated. |

## Prerequisites

1. KTalk extension built: `cd browser-extension && npm run build`
2. Playwright installed in the project venv:
   `./venv-gigaam/bin/pip install playwright && ./venv-gigaam/bin/playwright install chromium`
3. An account on any KTalk workspace where you can create a public room.

## Quick start

```bash
# 1. In your normal browser: log in to your KTalk workspace, click
#    "Start quick call", enable Public room, copy the URL.
#    Keep that tab open — you are the host.

# 2. Launch 2 fake-audio guests (overlap / spectral-signature scenario):
./venv-gigaam/bin/python tests/e2e/research/launch_guests.py \
    --guest ProbeAlice=speech_reference.wav \
    --guest ProbeBob=noise_pink.wav \
    https://<your-subdomain>.ktalk.ru/<room-id>

# 3. In your host tab, open DevTools console, paste the content of
#    host_probe.js, and press Enter. After ~15s it returns per-participant
#    summary stats.
```

## Verified scenarios (2026-04-22)

All three ran on a 3-participant SFU room (host + two fake-audio guests).

| Scenario | Guests | What we learned |
|---|---|---|
| **Baseline isolation** | Alice=speech_reference.wav, Bob=silence-timed | Bob's stream carried 0 signal while Alice's stream was highly active. No cross-talk → streams are per-speaker, not mixed. |
| **1-on-1 P2P fallback check** | Alice only | With just host + 1 guest, `conf.jitsiConf.isP2PActive()` still returns `false`. **KTalk keeps SFU even for 1-on-1 rooms.** |
| **Spectral overlap** | Alice=speech_reference.wav, Bob=noise_pink.wav, both playing simultaneously | Alice flat_median=0.013 (tonal, speech), Bob flat_median=0.375 (noise). ~29× separation — if streams were mixed, both would show intermediate flatness. |

## What's not covered

- Packet-loss / jitter resilience (needs network emulation — out of scope).
- Behaviour in very large rooms (>5 guests) — `audio-analyzer.js:99` selectors
  are expected to generalise but not tested here.
- CPU load with N parallel AudioWorklet downsamplers — belongs in the ASR
  pipeline benchmark, not here.

## Anonymisation note

These scripts contain no account logins, room URLs, or user
names. All runtime data is passed via CLI. Credentials live in `.env`
(read by other e2e tests through `recog.config.load_dotenv`) but these
research scripts deliberately do not touch them.
