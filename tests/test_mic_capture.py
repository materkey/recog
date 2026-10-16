"""Reopening a sounddevice stream that PortAudio stopped (`SingleDeviceCapture`).

On 2026-10-16 macOS applied a configuration change to the built-in microphone
in the middle of a day-long recording; PortAudio hit render error -10863,
stopped the stream for good (finished callback, `active` False, no further
callbacks) and the next meeting recorded no local speech, without a log line.
The tests hold the contracts that cannot be checked on a live Mac:
- a stream that finished without our stop() is reopened at once, and so is
  one that is inactive or silent without that callback; our own stop is not
  taken for a failure;
- the reopened stream finds the device again by name: PortAudio caches the
  device list and its indices shift when headphones connect;
- a failing reopen is retried with a backoff and never raises into the loop;
- re-reading the list (Pa_Terminate) would close every other open stream, so
  it is skipped while another sounddevice capture is open.
"""

import sys
import types
import weakref
from pathlib import Path
from unittest.mock import Mock

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog import live
from recog.live import CALLBACK_STALL_S, LiveTranscriber, SingleDeviceCapture
from recog.system_audio import RESTART_BACKOFF_S

SR = 16000
MIC = "Микрофон MacBook Pro"


def _devices(*names):
    """Device list in the shape of sounddevice.query_devices()."""
    return [{"name": name, "max_input_channels": channels} for name, channels in names]


class _FakeStream:
    def __init__(self, backend, device, channels, samplerate, callback, finished_callback=None):
        self.backend = backend
        self.device = device
        self.callback = callback
        self.finished_callback = finished_callback
        self.active = False
        self.closed = False
        backend.streams.append(self)

    def start(self):
        self.active = True

    def stop(self):
        # PortAudio calls the finished callback for a stop we asked for too.
        self.active = False
        if self.finished_callback:
            self.finished_callback()

    def close(self):
        self.active = False
        self.closed = True

    def feed(self, block):
        self.callback(block.reshape(-1, 1), len(block), None, None)

    def fail(self):
        """What PortAudio does on a render error: stop and call back once."""
        self.active = False
        self.finished_callback()


class _FakePortAudio(types.ModuleType):
    """sounddevice whose device list is cached until _terminate/_initialize."""

    def __init__(self, devices):
        super().__init__("sounddevice")
        self.hardware = devices
        self._cached = list(devices)
        self._initialized = 1
        self.streams = []
        self.rescans = 0

    def query_devices(self, device=None, kind=None):
        if device is None:
            return self._cached
        return self._cached[device]

    def InputStream(self, **kwargs):
        return _FakeStream(self, **kwargs)

    def _terminate(self):
        # Pa_Terminate closes every open stream of the process.
        for stream in self.streams:
            stream.close()
        self._initialized -= 1

    def _initialize(self):
        self._cached = list(self.hardware)
        self._initialized += 1
        self.rescans += 1


@pytest.fixture(autouse=True)
def _no_captures_from_other_tests(monkeypatch):
    monkeypatch.setattr(SingleDeviceCapture, "_open_captures", weakref.WeakSet())


@pytest.fixture
def portaudio(monkeypatch):
    backend = _FakePortAudio(
        _devices(
            ("BlackHole 2ch", 2),
            ("MacBook Pro Speakers", 0),
            ("Studio Display", 0),
            (MIC, 1),
        )
    )
    monkeypatch.setitem(sys.modules, "sounddevice", backend)
    return backend


def _started_mic(portaudio) -> SingleDeviceCapture:
    capture = SingleDeviceCapture(device=3, sample_rate=SR, name="mic")
    capture.start()
    return capture


def _connect_headphones(portaudio):
    """Bluetooth headphones appear first and shift the microphone's index."""
    portaudio.hardware = _devices(("AirPods Pro", 1)) + portaudio.hardware


def _transcriber(capture, recording_active: bool = True):
    transcriber = Mock(spec=LiveTranscriber)
    transcriber.ktalk_only = not recording_active
    transcriber._recording_allowed = recording_active
    transcriber.mic_capture = capture
    transcriber.system_capture = None
    transcriber.is_recording_active = types.MethodType(
        LiveTranscriber.is_recording_active, transcriber
    )
    return transcriber


def _loop_iteration(transcriber):
    LiveTranscriber._reopen_stopped_device_captures(transcriber)


class _Clock:
    """Controllable time.monotonic for recog.live."""

    def __init__(self):
        self.now = 1000.0

    def __call__(self):
        return self.now


@pytest.fixture
def clock(monkeypatch):
    fake = _Clock()
    monkeypatch.setattr(live.time, "monotonic", fake)
    return fake


def test_stream_stopped_by_portaudio_is_reopened_by_name(portaudio, capsys):
    """given: PortAudio stops the mic stream on its own (finished callback
    without our stop) and headphones connected meanwhile, so the mic moved
    from index 3 to 4
    when: the live loop runs
    then: the stop is logged, a new stream is open on the same device found
    by name, and its audio reaches the buffer again"""
    capture = _started_mic(portaudio)
    old_stream = portaudio.streams[-1]
    _connect_headphones(portaudio)

    old_stream.fail()
    _loop_iteration(_transcriber(capture))

    new_stream = portaudio.streams[-1]
    assert new_stream is not old_stream and old_stream.closed
    assert new_stream.active and capture.running
    assert new_stream.device == 4
    assert portaudio.query_devices(new_stream.device)["name"] == MIC
    assert portaudio.rescans == 1
    err = capsys.readouterr().err
    assert "was stopped by the audio system" in err
    assert "Reopened" in err

    new_stream.feed(np.full(SR, 0.01, dtype=np.float32))
    chunk, _ = capture.get_chunk(1.0)
    assert chunk.shape == (SR,)


def test_our_own_stop_is_not_taken_for_a_lost_stream(portaudio):
    """given: recog stops the mic itself (meeting left in KTalk-only mode)
    when: PortAudio calls the finished callback for that stop
    then: nothing is reopened"""
    capture = _started_mic(portaudio)

    capture.stop()
    _loop_iteration(_transcriber(capture))

    assert capture.stream_failure() is None
    assert len(portaudio.streams) == 1
    assert not capture.running


def test_inactive_stream_without_finished_callback_is_reopened(portaudio):
    """given: the stream became inactive but the finished callback never came
    when: the live loop runs
    then: the stream is reopened"""
    capture = _started_mic(portaudio)
    portaudio.streams[0].active = False

    _loop_iteration(_transcriber(capture))

    assert len(portaudio.streams) == 2
    assert portaudio.streams[-1].active


def test_stream_without_callbacks_is_reopened_after_the_stall_limit(portaudio, clock):
    """given: the stream stays active but stops calling back
    when: the live loop runs before and after CALLBACK_STALL_S
    then: only the second run reopens it"""
    capture = _started_mic(portaudio)
    portaudio.streams[0].feed(np.zeros(SR // 100, dtype=np.float32))
    transcriber = _transcriber(capture)

    clock.now += CALLBACK_STALL_S - 1
    _loop_iteration(transcriber)
    assert len(portaudio.streams) == 1

    clock.now += 2
    _loop_iteration(transcriber)
    assert len(portaudio.streams) == 2
    assert portaudio.streams[-1].active


def test_failed_reopen_is_retried_with_a_backoff(portaudio, clock):
    """given: the stream is lost while the mic is missing from the device list
    when: the live loop keeps running
    then: attempts never raise, are spaced by RESTART_BACKOFF_S, and the mic
    is reopened once it is back"""
    capture = _started_mic(portaudio)
    present = portaudio.hardware
    portaudio.hardware = _devices(("BlackHole 2ch", 2))
    portaudio.streams[0].fail()
    transcriber = _transcriber(capture)

    _loop_iteration(transcriber)
    assert not capture.running
    assert portaudio.rescans == 1

    _loop_iteration(transcriber)  # immediately again: backoff holds it
    assert portaudio.rescans == 1

    clock.now += RESTART_BACKOFF_S[0]
    _loop_iteration(transcriber)
    assert portaudio.rescans == 2

    portaudio.hardware = present
    clock.now += RESTART_BACKOFF_S[0]
    _loop_iteration(transcriber)  # second backoff step not over yet
    assert not capture.running

    clock.now += RESTART_BACKOFF_S[1]
    _loop_iteration(transcriber)
    assert capture.running
    assert portaudio.streams[-1].active


def test_lost_stream_is_left_alone_while_recording_is_off(portaudio):
    """given: KTalk-only mode between meetings (recording not allowed)
    when: the stream is reported lost
    then: the live loop does not reopen the device"""
    capture = _started_mic(portaudio)
    portaudio.streams[0].fail()

    _loop_iteration(_transcriber(capture, recording_active=False))

    assert len(portaudio.streams) == 1


def test_device_list_is_kept_while_another_sounddevice_stream_is_open(portaudio):
    """given: a second sounddevice capture is open (`--system-audio device:<id>`)
    when: the mic stream is lost and reopened
    then: PortAudio is not re-initialized (that would close the other stream)
    and the mic is looked up by name in the cached list"""
    system = SingleDeviceCapture(device=0, sample_rate=SR, name="system")
    system.start()
    capture = _started_mic(portaudio)
    portaudio.streams[-1].fail()

    _loop_iteration(_transcriber(capture))

    assert portaudio.rescans == 0
    assert portaudio.streams[0].active
    assert portaudio.streams[-1].device == 3 and portaudio.streams[-1].active
