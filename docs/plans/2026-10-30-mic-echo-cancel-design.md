# Microphone echo cancellation on macOS: design

Status: built, the stage's switch off by default until validations 1, 2, 3 and 5 pass.
Replaying the takes through `recog/echo_cancel.py`: reading 96.1% / 0 false words, lecture 22,
quiet voice over echo 82.7% / 14, others silent 95.8%, which reproduces the table below.
Measurements were made on 2026-10-30 and 2026-10-31 with throwaway
code outside the repository, on one MacBook Pro with its built-in speakers and
microphone, at one output volume.

## Why

With laptop speakers on, the microphone hears whatever the system plays. Every
phrase is transcribed twice: once from the system capture as `Unknown` (or as a
conference participant) and once from the microphone under the user's own name.
The user-visible effect is a transcript where half the lines are false claims that
the user said something. In 15 minutes of speech from the speakers, the raw
microphone produced 2914 words the user never said.

The running service also loses microphone samples, which this design fixes on its own
account, whether or not cancellation is on.

## Goals

- Sound from the speakers does not reach the transcript under the user's name.
- The user's words are not lost: no worse than the raw microphone, including when
  speaking quietly.
- With headphones, or with nothing playing, the microphone stays exactly as it is
  today.
- The microphone does not depend on anything that captures the system audio.
- Segment playback from `audio_mic.wav` matches what ASR heard.

## Non-goals

- Linux. The same chain should fit later: `pyaec` has Linux wheels, and `parec` already
  gives the reference. PipeWire's `module-echo-cancel` is installed on the Linux machine
  checked, as an alternative. Neither is measured.
- Noise suppression and automatic gain.
- Removing the duplicate when a user's own voice returns through a conference.
- The crackle described below.

## Anti-goals

- Dropping the user's words to get rid of an echo. Passing an echo through is the
  lesser failure, since it is today's behaviour.
- A failure in echo cancellation that stops microphone transcription.

## Approaches

Where the microphone runs:

1. **A small child process with `sounddevice` (chosen).** Removes the sample loss,
   keeps PortAudio's timestamps for alignment, and stays independent of `replayd`,
   Screen Recording and a display. Needs a framing on its pipe and alignment over two
   sources.
2. **The microphone in the `SCStream` of the system capture.** Measured to work and
   gives one clock, but a lost helper would take the microphone with it. The service
   log shows the helper dying and staying down for hours with no display (`Start failed
   (initial, attempt 671): No display found`), and this needs macOS 15. It would also
   need a fallback path with source boundaries and a change to the silence restart.
3. **Staying in the live process with a larger block, filling holes by timestamp.**
   Smallest change, but a block of 1600 samples still lost 6–9% under holds of 30 ms and
   more, so the audio stays damaged.

What cancels:

1. **Linear Speex with the low cut undone before it, a reference-driven residual
   suppressor after it, and raw audio while the reference is quiet (chosen).**
2. **WebRTC AEC3.** Recall 88.2% against 93.0% raw on the same take; the user heard the
   voice as muffled.
3. **macOS VoiceProcessingIO.** What it uses as a reference with aggregate devices is
   unknown, and it may duck other applications. Not measured.
4. **Dropping microphone chunks that resemble the system audio.** An anti-goal.

Reverse engineering Yandex Station's canceller was rejected: a closed aarch64 library
tuned for four microphones in one enclosure. What transfers is the principle of a shared
time base between microphone and reference.

## Measurements

### The sample loss

`sounddevice` calls back with 15 samples, about a thousand times a second, and holds the
interpreter lock to do so. In the service, audio per wall-clock second is 0.99358 over 55
minutes and 0.99212 on a recording from 2026-10-31, against 1.00000 for the system
capture. Reproduced with a process that keeps the interpreter busy, 60 s each:

| Load | Default block | Block of 1600 |
|---|---|---|
| None | 0.99992 | 1.00000 |
| One C call holding the lock 8 ms every 20 ms | 0.630 | 1.00008 |
| 30 ms every 100 ms | 0.690 | 0.924 |
| 100 ms every 300 ms | 0.726 | 0.912 |
| Two spinning Python threads | 0.080 | 0.99996 |

PortAudio reports no status for any of this, which is why the logs are empty. The gaps in
`inputBufferAdcTime` account for the whole loss (55.128 s of gaps against 55.12 s lost).
With the microphone in a child process and a parent holding the lock 30 ms every 100 ms,
audio per ADC second is 1.00000, with no gaps.

The loss in the service itself is measured directly: during a 90 s test, the service's
own `audio_mic.wav` and a child process recorded the same sound from the same device. Against
the child's lossless take, the service's file was short by 0.41% on average, unevenly: no
loss in a third of half-second steps, up to 195 samples per 8000 in others.

The samples go missing at capture, not when chunks are cut or written: inside one continuous
written chunk, with no boundary in it, 0.57% on average (median 0.39%) is missing.

Local GigaAM alone is not the cause: `SingleDeviceCapture` with it running in the same process,
every 2.5 s or without pause, delivered 0.99996 and 1.00000. The real `live_pipeline` with the
installation's remote ASR and no bridge or extension lost 0.06% without a system capture and
0.07% with one, in 125–153 small gaps that `inputBufferAdcTime` shows (0.095–0.127 s of the
0.101–0.131 s lost), again with no status. The service with its bridge loses more, so the
size follows the work in the process.

The mechanism is confirmed in the running service. Sampled with `sample` for 25 s while it
transcribed speech, its audio I/O thread was in PortAudio's callback in 336 samples and
waiting in `take_gil` in 280 of them, 83%. The most frequent Python work on top of the
stacks is garbage collection (`gc_collect_main`, `deduce_unreachable`) and JSON
(`escape_unicode`, `scanstring_unicode`). A full collection with the same imports takes about
32 ms, and `recog/meeting_storage.py` parses and writes JSON on segment writes, with
`index.json` at 2.7 MB taking 15–35 ms per load or indented dump. Each of these holds the
lock for many times the callback's 1 ms, which matches the reproductions above.

There is no single culprit to fix. With the real loop running, garbage collection was
negligible there (228 runs, 8 ms in total, none of the full kind), yet one extra thread that
only woke every 5 ms to look at stacks raised the loss from 0.06% to 0.50%, with gaps up to
6.4 ms, close to Python's 5 ms thread switch interval. A callback due every 1 ms loses audio
whenever any Python thread runs. Tuning individual operations would only shrink the loss,
while the child removes it: the child was lossless at the same moment as the service, recording the same device.
Validation 4 confirms the fix.

### Alignment

PortAudio's ADC time and ScreenCaptureKit's presentation time share one base, the host
uptime clock: both minus wall clock agree within about 10 ms. Placing both streams on that
timeline, with a child microphone and a busy parent, the echo stayed 51.6 to 54.1 ms
behind the reference for 90 s, median suppression -23.6 dB. An output switch in the middle
caused no gap in either stream.

Over one hour alongside the running service, with a soft sweep from the speakers every 3
minutes, the echo stayed 48.19–48.44 ms behind the reference by timestamps on all 20 sweeps,
with no trend. The child had 2 gaps of 16 ms in total and the system capture none. By samples
alone the microphone ran at 15999.881 per second, a drift of 0.0007%, which timestamps absorb.
Validation 4 extends this to several hours inside the service, under conference load.

Speex tolerance on the lecture, baseline -17.5 dB, where the reference already leads by
about 50 ms:

| Reference moved | Median suppression |
|---|---|
| 40 ms later | -17.4 dB |
| 50 ms later | -5.5 dB |
| 150 ms earlier | -17.1 dB |
| 200 ms earlier | -13.8 dB |
| 220 ms earlier | -3.0 dB |
| Sudden 5–100 ms shift | back within 2–4 s |
| Tail 2048, 4096, 8192 | -17.7, -17.5, -16.8 dB |

Steady unnoticed drift is far less tolerated than a shift:

| Steady loss | Median suppression |
|---|---|
| 0.005% | -12.9 dB |
| 0.01% | -9.6 dB |
| 0.02% | -6.6 dB |
| 0.05% and more | about -3 dB |
| 0.64% in blocks, audio closing up | -2.9 dB |
| 0.64% in blocks, holes zero-filled by timestamp | -16.2 dB |

Counted by samples, not timestamps, the lag moved -6 ms per hour on `SCStream` and -35 ms
per hour on `sounddevice`. Placing by timestamp is therefore required, not optional.

### Speex cuts the voice

With a silent reference Speex is not transparent. White noise loses 14 dB at 100–200 Hz
and 3.6 dB at 200–500 Hz, with no loss above 1 kHz. A real voice lost 5–11 dB overall. The
30 s quiet recording fell from 0.006–0.013 rms to 0.0016–0.0041, below `VAD_RMS_THRESHOLD`
(0.005), so **no chunk reached ASR** even though ASR reads that thin audio without error
when given it (44 words against 42 from the raw audio). An earlier reading take passed
only because it was loud.

### The chosen chain

`pre` is the low cut undone before Speex (high-pass at 60 Hz, then an inverse filter).
`thin` is Speex without it. All use the suppressor (beta 2, floor 0.2) and the real
threshold. `gate` means raw audio while the reference is quiet.

| Variant | Reading, recall / false words | Lecture 300 s, false words | Quiet voice over echo, recall / false | Quiet voice, others silent |
|---|---|---|---|---|
| Raw microphone | 93.0% / 270 | 904 | 22.6% / 501 | 95.8% |
| thin | 95.2% / 0 | 2 | 2.4% / 0 | 0% |
| thin + gate | 95.2% / 0 | 10 | 29.8% / 3 | 95.8% |
| pre | 95.6% / 1 | 24 | 81.5% / 14 | 95.8% |
| **pre + gate** | **95.6% / 1** | **24** | **81.0% / 14** | **95.8%** |
| pre + gate, reference leading 125 ms (the design's lead) | 95.6% / 4 | 20 | 83.9% / 12 | not repeated |

On the whole 15 minutes the chain leaves 56 false words of 2914, a 98% reduction, while
AEC3 reached 11 at the voice's expense. On a 90 s take through the child microphone: 222
false words raw, 6 after, all in one chunk during the first five seconds of adaptation.

Each 300 s lecture and quiet-voice row uses the same recording. The quiet voice is a real
recording mixed digitally over a real echo.

Settings are not fragile. Over 13 changes of the suppressor's constants on the reading
take, recall stayed 94.3–96.9% with 0–5 false words; both readings of the text behaved
alike. Harder settings trade words for echo along a smooth curve:

| High-pass, beta | Reading | Lecture false words | Quiet voice |
|---|---|---|---|
| 60 Hz, 2 | 95.6% | 24 | 81.0% |
| 60 Hz, 4 | 96.1% | 23 | 78.0% |
| 100 Hz, 2 | 96.1% | 24 | 78.6% |
| 150 Hz, 4 | 95.6% | 11 | 69.0% |
| 150 Hz, 8 | 95.2% | 9 | 64.3% |

An extra chunk threshold proportional to the reference removes the rest but sits close to
a cliff: on the `thin` chain at 0.25 × reference rms, 0 false words and 93.9% recall; at
0.3, 86.8%. It is not adopted, following the anti-goal.

### Output changes

Moving the output to a virtual device, with the reference still playing and no echo, is
the real version of plugging in headphones. Speex then adds noise: 0.005 rms from a
microphone at 0.0008, above the threshold, for about 2 s. Resetting does not help, because
the cut above is constant. So the stage must not run there.

| First second back on the speakers, mic 0.0217 | Output rms |
|---|---|
| Filter kept running while the output was elsewhere | 0.0097 |
| Filter reset | 0.0126 |
| **Filter paused and kept** | **0.0039** |

From nothing, suppression is -6.5 dB in the first second, -10.5 in the second, -14 by
the fourth, -16 by the sixth.

A real volume change, from 63 to 30 and back while speech played, through the whole chain:
output returns under the threshold about 4 s after the cut and 3 s after the rise, at up to
0.0094 rms meanwhile. Over the whole run 2 chunks of 23 passed the threshold, against 16 raw.
A volume change therefore costs a few words at most.

`CoreAudio` reports the default output change 37 ms and 3 ms after it is made (two
samples). Its `transport` and `mute`/volume read correctly on the speakers and on a virtual
device. The built-in speakers have the UID `BuiltInSpeakerDevice`.

The default output does not tell where a call plays: an application can route to headphones
while the default stays on the speakers. With no echo path and the stage running, a
quiet voice keeps 86.3% recall against 96.4% raw, so this needs its own check.

### Detecting the echo path

The peak of the normalised correlation between the microphone and the reference, over 3 s,
with the reference leading 0–300 ms, band 200–3500 Hz, percentiles 1 / 50 / 99:

| Case | Peak |
|---|---|
| Echo only | 0.36 / 0.55 / 0.77 |
| Echo with a quiet voice | 0.36 / 0.53 / 0.69 |
| Echo with a loud voice | 0.04 / 0.17 / 0.29 |
| No echo | 0.01 / 0.03 / 0.12 |
| No echo, voice | 0.01 / 0.05 / 0.13 |
| No echo, real output switch | 0.01 / 0.05 / 0.05 |
| Echo at 5% of its level | above 0.31 at the 5th percentile |

The rule is: present once a window exceeds 0.2, absent once the median of the last 10
windows falls below 0.13, and a window with a quiet reference counts for neither, since it says
nothing about the path. On 16 minutes of echo it never left "present"; on 300 s of no echo
it never entered it; through both readings aloud it stayed "present". It enters within
about 1 s and leaves about 5 s after the echo stops. A wrong "absent" under a loud voice
leaves an echo for a moment, which is the accepted lesser failure.

### The crackle

The user hears a crackle after Speex that AEC3 hid. It is not caused by cancellation: the
user hears it in a raw recording with nothing playing. It is not the capture path: single-
sample steps exist in the microphone signal (94 in 31 s), they coincide 88% between
`SCStream` and `sounddevice`, and bridging them does not remove what the user hears.
Subtracting them barely changes the measured high-frequency impulses (16.33% of frames to
16.28%), though the user has not listened to that version. The steps follow a person near
the laptop: 170 a minute during reading aloud, 0–7 with only speech from the speakers, and
about 1 in the digital reference, so they are not a capture fault. Whether they and the
crackle are the same thing is not established. The source remains unknown and the playback chain has not been ruled out. Repairing them changed recall from
95.2% to 94.3%, which is noise, not an improvement. It is accepted as a limit of segment
playback, not of the transcript.

### Package and cost

`pyaec` 1.0.1: 194 KB wheel with no dependencies, MIT by its repositories (`thewh1teagle/aec`,
`aec-rs`), though its metadata declares no licence. Wheels exist for macOS and Linux. Last push
April 2025. `pip install --dry-run` against `deploy/python/constraints/macos.txt` adds only
`pyaec`. The chain runs at 100× real time in one Python thread, about 1% of a core. The child
starts delivering audio about 0.2 s after launch.

## Design

**Capture.** On macOS the microphone runs in a small child process that opens the same
device through `sounddevice` and writes frames with their `inputBufferAdcTime` to a pipe. A
capture in the live process appends them to the microphone's `BufferedAudioCapture`, keeping
its contract, so `_process_chunk`, mute handling and overlap do not change. The logic in
`SingleDeviceCapture` for finding a device by name and reopening it moves with the stream.
The child's death is handled by the system capture's bounded respawn. The buffer stays, so no
source boundary is needed, and as with today's reopening the log holds it, without a marker in
the transcript. This removes the sample loss even with cancellation switched off.

**Reference.** `sck_audio_capture --frames` adds each buffer's presentation time and the output's
state to its output, read every 100 ms. Without a stage the helper is run as before. The
existing system-capture path is otherwise unchanged and the microphone never reads from it.

**Alignment.** Both streams are placed on the uptime timeline at 16 kHz, zero-filling holes.
The microphone is delayed by 75 ms so that the reference leads by about 125 ms, the middle
of the measured range of about 5 to 250 ms. The delay also covers the batches in which the
system audio arrives, up to about 50 ms apart.

**Cancellation.** A stage after the alignment runs the high-pass at 60 Hz, the inverse of
Speex's low cut, then `pyaec` with `enable_preprocess=False`, frame 160, tail 4096. Its
output goes through a suppressor on a 512-sample STFT with a 256-sample hop: the expected
residual in a bin is a coupling factor times a decaying memory of the reference power
spread over neighbouring bins, and the gain is `max(0.2, 1 - 2 * residual / output power)`.
The coupling factor is tracked causally as the 30th percentile, over 30 s, of the median
output-to-reference ratio per 2 s block. The inverse filter is measured from `pyaec` itself,
so a new version of `pyaec` means measuring it again, which the pin in the requirements
keeps deliberate.

The reference implementation is `.local/echo-spike/` (gitignored: the recordings hold the
user's voice), with `echolib.py`, `final.py` and `lowcut.py` as the chain and the takes that
produced every table above. The constants not repeated here, including the memory decay,
spread, smoothing, activity gate and the inverse filter's recipe, are there. Since the
suppressor's reference offset follows the lead, it is 1950 samples at the design's lead,
as measured in the row above. Before any live wiring, the port replays those takes and
reproduces the table.

**When the stage runs.** It applies only when all of these hold; otherwise the microphone is
raw, with a 10 ms crossfade:

- The default output is the built-in speakers, by UID, and not muted, with volume above zero.
  Anything else, including Bluetooth speakers and aggregate devices, is raw: an echo there
  stays as it is today. While the output is elsewhere, Speex is paused and keeps its filter.
- The echo path is present by the rule above. This covers a call routed elsewhere
  and devices nobody listed. Speex is paused while it is absent, like the output
  above, and it starts absent. Measured only on recordings; validation 3 checks it live.
- The reference has not been quiet (rms below 0.002) for 300 ms. Speex keeps running then.
- A reference exists at all. A lost system capture makes the microphone raw, and silently
  cannot make it worse than today.

An exception raised in the stage also makes the microphone raw, logged once, for good until a
restart.
`audio_mic.wav` holds what ASR hears.

**Silence restart.** `_restart_silent_system_capture` stays as it is, since the microphone
no longer shares the process.

**Switch.** One `[live]` key in `config.toml`, resolved through `INSTALL_CONFIG`, on by
default on macOS once validation passes (see Delivery). Off leaves the child microphone but
no stage. `--system-audio none` and
`device:<id>` have no reference, so the stage does not run. `CHANGELOG.md` gets an entry
covering the new dependency, the setup step and the changed behaviour.

## Delivery

Our call, two steps that merge separately. The child microphone goes first, needing only
validation 4: it fixes a loss that exists today and tests the inference about its cause on
its own, so a later instability can be attributed. Timestamps, alignment and the stage follow,
with the stage's switch off by default until validations 1, 2, 3 and 5 pass.

## Validation

What only a person or a real call can provide, before the stage is on by default:

1. **A second real reading over the speakers,** normal and quiet, because the constants
   were chosen on one take. Passes at recall no lower than the raw microphone's and at most
   a few false words.
2. **A real call on the speakers.** Only video playback has been measured. Passes when
   segments under the user's name hold only what the user said, apart from isolated words.
3. **Real headphones, including a switch mid-call,** with a jack and with Bluetooth. Only a
   virtual output has been measured. The microphone must be identical to the raw one.
   AirPods also change the microphone, which the existing reopening by name should handle.
4. **Several hours in the running service.** Audio per wall-clock second stays within
   1.0 ± 0.0002 for the microphone, which also confirms the inference about the sample loss.
   The echo stays inside the measured range.
5. **A moved laptop.** A real volume change has been measured; a change of the room or of
   the laptop's position has not.

## Left to the builder

- The framing on the two pipes.
- How the stage learns of the output's state: `sck_audio_capture`, or a small separate
  listener. A mute or volume listener has not been timed, only read.
- A cheaper form of the inverse filter than 1024 FIR taps, as long as the response stays
  within measurement.
- Limits for waiting on the reference, and for how late a block may arrive.
- Whether Speex is re-created after a long absence on the speakers. Nothing measured asks
  for it: it recovers in seconds either way.
