"""The microphone in a child process (`recog/mic_worker.py`, `ChildMicCapture`).

`sounddevice` calls back about a thousand times a second and holds the
interpreter lock to do so, so in the live process any busy Python thread loses
samples, with no status from PortAudio. The tests hold what moving it must keep:
- frames reach the shared buffer as 1-D mono float32 at their capture time, also
  when a read cuts a frame in two;
- the child reports every way a stream stops capturing by exiting, and finds the
  device by name, since each start reads a fresh device list;
- only macOS moves; other systems and a capture of another device stay as they are.
"""

import struct
import subprocess
import sys
import textwrap
import threading
import time
import types
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.install_config import INSTALL_CONFIG
from recog.live import SingleDeviceCapture, create_echo_stage, create_mic_capture
from recog.mic_capture import (
    ONLY_ZEROS_REPORT_S,
    REFERENCE_HEADER,
    ChildMicCapture,
    FramedSystemCapture,
)
from recog.mic_worker import (
    BLOCKSIZE,
    EXIT_DEVICE_NOT_FOUND,
    EXIT_STREAM_LOST,
    FRAME_HEADER,
    capture_loop,
    encode_frame,
    find_input_device,
)
from recog.system_audio import SubprocessPCMCapture, create_system_capture

SR = 16000
MIC = "Микрофон MacBook Pro"


def _frame(samples, adc=100.0, wall=1000.0):
    return encode_frame(adc, wall, np.asarray(samples, dtype=np.float32))


def _capture(argv=("unused",)) -> ChildMicCapture:
    return ChildMicCapture(device_name=MIC, sample_rate=SR, argv=list(argv))


def test_frames_become_mono_float32_at_the_time_the_child_stamped():
    """given: two frames in one read, the first stamped at wall clock 1000.0
    when: the reader decodes them
    then: the samples are joined in order, 1-D float32, stamped with the first"""
    capture = _capture()

    batches = capture._decode_read(_frame([0.1, 0.2], wall=1000.0) + _frame([0.3], wall=1000.5))

    assert len(batches) == 1
    samples, captured_at = batches[0]
    assert captured_at == 1000.0
    assert samples.dtype == np.float32 and samples.ndim == 1
    assert samples.tolist() == pytest.approx([0.1, 0.2, 0.3])


@pytest.mark.parametrize(
    "cut", [1, FRAME_HEADER.size - 1, FRAME_HEADER.size, FRAME_HEADER.size + 5]
)
def test_read_cutting_a_frame_loses_nothing(cut):
    """given: a pipe read that ends anywhere inside a frame
    when: the next read brings the rest
    then: nothing is emitted for the partial frame and the samples come whole"""
    capture = _capture()
    data = _frame([0.1, 0.2, 0.3])

    assert capture._decode_read(data[:cut]) == []
    batches = capture._decode_read(data[cut:])

    assert batches[0][0].tolist() == pytest.approx([0.1, 0.2, 0.3])


def test_hole_in_adc_time_is_reported(capsys):
    """given: two frames whose ADC times leave 50 ms unaccounted for
    when: they are decoded
    then: the hole is logged, because PortAudio reports no status for a lost block;
    frames that follow each other stay quiet"""
    capture = _capture()
    block = [0.0] * 160  # 10 ms

    capture._decode_read(_frame(block, adc=10.0) + _frame(block, adc=10.01))
    assert "hole" not in capsys.readouterr().err

    capture._decode_read(_frame(block, adc=10.07))
    assert "50 ms" in capsys.readouterr().err


def test_only_zeros_are_reported_once_and_rearm_on_signal(capsys, monkeypatch):
    """given: macOS answers a refused microphone and a closed lid with exact zeros,
    which an open one never gives, and the transcript would just stay empty
    when: zeros last past the limit, signal returns, zeros last again
    then: each stretch is logged once"""
    capture = _capture()
    monkeypatch.setattr(capture, "digital_silence_s", lambda: ONLY_ZEROS_REPORT_S + 1)

    capture._decode_read(_frame([0.0]))
    capture._decode_read(_frame([0.0]))
    err = capsys.readouterr().err
    assert err.count("only exact zeros") == 1
    assert "lid" in err and "permission" in err

    monkeypatch.setattr(capture, "digital_silence_s", lambda: 0.0)
    capture._decode_read(_frame([0.1]))
    monkeypatch.setattr(capture, "digital_silence_s", lambda: ONLY_ZEROS_REPORT_S + 1)
    capture._decode_read(_frame([0.0]))
    assert capsys.readouterr().err.count("only exact zeros") == 1


def test_new_child_does_not_continue_the_half_frame_of_the_old_one():
    """given: a child that died in the middle of a frame
    when: the respawned child's first frame arrives
    then: it decodes on its own"""
    capture = _capture()
    assert capture._decode_read(_frame([0.1, 0.2])[:-3]) == []

    capture._on_new_child()

    assert capture._decode_read(_frame([0.5], adc=500.0))[0][0].tolist() == pytest.approx([0.5])


def test_new_child_is_not_a_hole(capsys):
    """given: a respawned child, whose first frame comes seconds after the old one's last
    when: it is decoded
    then: no hole is claimed: the respawn has been logged already"""
    capture = _capture()
    capture._decode_read(_frame([0.0], adc=1.0))

    capture._on_new_child()
    capture._decode_read(_frame([0.0], adc=9.0))

    assert "hole" not in capsys.readouterr().err


_FAKE_CHILD = textwrap.dedent(
    """
    import struct, sys, time
    out = sys.stdout.buffer
    for i in range(50):
        out.write(struct.pack("<ddI", 1.0 + i, time.time(), 160) + struct.pack("<160f", *([0.25] * 160)))
    out.flush()
    time.sleep(30)
    """
)


def test_real_pipe_from_a_real_process_fills_the_buffer():
    """given: a child process writing frames to a real pipe
    when: the capture runs
    then: `get_chunk` returns its audio in the shape ASR and the WAV writer expect
    and stop() leaves no process behind"""
    capture = _capture([sys.executable, "-c", _FAKE_CHILD])
    capture.start()
    try:
        deadline = time.monotonic() + 5
        got = None
        while got is None and time.monotonic() < deadline:
            got = capture.get_chunk(0.25)
            time.sleep(0.01)
        assert got is not None
        chunk, start_ts = got
        assert chunk.shape == (SR // 4,) and chunk.dtype == np.float32
        assert chunk == pytest.approx(0.25)
        assert abs(start_ts - time.time()) < 5
        process = capture.process
    finally:
        capture.stop()
    assert process.poll() is not None


def test_dead_child_is_respawned_and_audio_continues(monkeypatch):
    """given: a child that delivers a little audio and exits, as on a lost stream
    when: the capture runs
    then: a new child is started and its audio arrives too"""
    monkeypatch.setattr("recog.system_audio.RESTART_BACKOFF_S", (0.05,))
    script = textwrap.dedent(
        """
        import struct, sys, time
        sys.stdout.buffer.write(struct.pack("<ddI", 1.0, time.time(), 800) + struct.pack("<800f", *([0.5] * 800)))
        sys.stdout.buffer.flush()
        time.sleep(1)
        """
    )
    capture = _capture([sys.executable, "-c", script])
    capture.start()
    try:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and capture._buffered_samples < 1600:
            time.sleep(0.02)
        assert capture._buffered_samples >= 1600
    finally:
        capture.stop()


# --- the worker side ------------------------------------------------------


def _devices(*names):
    return [{"name": name, "max_input_channels": channels} for name, channels in names]


def test_device_is_found_by_name_among_inputs_only():
    """given: an output with the same name before the input
    when: looking up the name
    then: the input's index is returned, and an absent name gives None"""
    devices = _devices(("Same", 0), ("Other", 1), ("Same", 2))
    assert find_input_device(devices, "Same") == 2
    assert find_input_device(devices, "Gone") is None


class _FakeStream:
    def __init__(
        self, backend, device, channels, samplerate, blocksize, dtype, callback, finished_callback
    ):
        backend.stream = self
        self.device = device
        self.blocksize = blocksize
        self.channels = channels
        self.callback = callback
        self.finished_callback = finished_callback
        self.active = True
        self.dtype = dtype

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.active = False

    def feed(self, block, adc=5.0):
        indata = np.asarray(block, dtype=np.float32)
        info = types.SimpleNamespace(inputBufferAdcTime=adc)
        self.callback(indata, len(indata), info, None)


class _FakeSd:
    def __init__(self, devices):
        self.devices = devices
        self.stream = None

    def query_devices(self, device=None, kind=None):
        if device is not None:
            return self.devices[device]
        if kind == "input":
            return {"index": 1, **self.devices[1]}
        return self.devices

    def InputStream(self, **kwargs):
        return _FakeStream(self, **kwargs)


class _Pipe:
    """Parent's end: collects what the worker wrote; closing it breaks the pipe."""

    def __init__(self):
        self.data = b""
        self.broken = False

    def write(self, data):
        if self.broken:
            raise BrokenPipeError
        self.data += data

    def flush(self):
        pass


def _run_worker(sd, pipe, name=MIC, stall_s=10.0):
    result = {}

    def run():
        result["code"] = capture_loop(sd, name, SR, pipe, stall_s=stall_s)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    deadline = time.monotonic() + 5
    while sd.stream is None and thread.is_alive() and time.monotonic() < deadline:
        time.sleep(0.001)
    return thread, result


def _wait(thread, result):
    thread.join(5)
    assert not thread.is_alive()
    return result["code"]


def _wait_for(predicate):
    deadline = time.monotonic() + 5
    while not predicate() and time.monotonic() < deadline:
        time.sleep(0.005)
    assert predicate()


def _parse(data):
    frames = []
    while data:
        adc, wall, count = FRAME_HEADER.unpack_from(data)
        data = data[FRAME_HEADER.size :]
        frames.append((adc, wall, np.frombuffer(data[: count * 4], dtype=np.float32)))
        data = data[count * 4 :]
    return frames


def test_worker_writes_stereo_as_mono_with_the_adc_time():
    """given: a two-channel device, as `SingleDeviceCapture` opened it
    when: a block arrives
    then: the frame holds the channels' mean and the block's ADC time"""
    sd, pipe = _FakeSd(_devices(("Out", 0), (MIC, 2))), _Pipe()
    thread, result = _run_worker(sd, pipe)
    assert sd.stream.device == 1 and sd.stream.channels == 2

    sd.stream.feed([[0.2, 0.4], [0.0, 1.0]], adc=7.5)
    _wait_for(lambda: pipe.data)
    pipe.broken = True  # the parent went away: the only way out besides a failure
    sd.stream.feed([[0.0, 0.0]])
    assert _wait(thread, result) == 0

    adc, _, samples = _parse(pipe.data)[0]
    assert adc == 7.5
    assert samples.tolist() == pytest.approx([0.3, 0.5])


def test_worker_opens_the_stream_with_a_block_of_10_ms():
    """given: a microphone
    when: the worker opens it
    then: it asks for a fixed 10 ms block, not PortAudio's ~1 ms IO cycle, whose
    callbacks lost audio whenever they waited for the interpreter lock"""
    sd, pipe = _FakeSd(_devices(("Out", 0), (MIC, 1))), _Pipe()
    thread, result = _run_worker(sd, pipe)

    assert sd.stream.blocksize == BLOCKSIZE == SR // 100
    pipe.broken = True
    sd.stream.feed([[0.0]])
    assert _wait(thread, result) == 0


def test_worker_exits_when_portaudio_stops_the_stream():
    """given: PortAudio stopped the stream on its own (render error -10863)
    when: the finished callback runs
    then: the worker exits and the parent's respawn opens a new one"""
    sd, pipe = _FakeSd(_devices(("Out", 0), (MIC, 1))), _Pipe()
    thread, result = _run_worker(sd, pipe)

    sd.stream.active = False
    sd.stream.finished_callback()

    assert _wait(thread, result) == EXIT_STREAM_LOST


def test_worker_exits_when_callbacks_stop():
    """given: an active stream that stopped calling back
    when: the stall limit passes
    then: the worker exits"""
    sd, pipe = _FakeSd(_devices(("Out", 0), (MIC, 1))), _Pipe()
    thread, result = _run_worker(sd, pipe, stall_s=0.05)

    assert _wait(thread, result) == EXIT_STREAM_LOST


def test_worker_refuses_an_absent_device():
    """given: the named microphone is not in the device list
    when: the worker starts
    then: it exits with its own code, opening nothing"""
    sd, pipe = _FakeSd(_devices(("Out", 0))), _Pipe()
    thread, result = _run_worker(sd, pipe)

    assert _wait(thread, result) == EXIT_DEVICE_NOT_FOUND
    assert sd.stream is None


def test_worker_without_a_name_opens_the_default_input():
    """given: no device name
    when: the worker starts
    then: the default input is opened"""
    sd, pipe = _FakeSd(_devices(("Out", 0), (MIC, 1))), _Pipe()
    thread, result = _run_worker(sd, pipe, name=None)

    assert sd.stream.device == 1
    pipe.broken = True
    sd.stream.feed([[0.0]])
    assert _wait(thread, result) == 0


def test_worker_process_imports_neither_recog_nor_the_ml_stack():
    """given: the worker's file run the way the capture runs it
    when: Python starts it, with sounddevice absent from this check's path
    then: importing it needs nothing heavier than the standard library and numpy"""
    worker = Path(__file__).resolve().parent.parent / "recog" / "mic_worker.py"
    code = (
        f"import runpy, sys; runpy.run_path({str(worker)!r}, run_name='check');"
        "bad = {'torch', 'recog', 'sounddevice'} & set(sys.modules); sys.exit(1 if bad else 0)"
    )
    assert subprocess.run([sys.executable, "-I", "-c", code]).returncode == 0


# --- which capture a microphone gets ----------------------------------------


def _fake_portaudio(monkeypatch):
    module = types.ModuleType("sounddevice")
    module.query_devices = lambda device=None, kind=None: {"name": MIC, "max_input_channels": 1}
    monkeypatch.setitem(sys.modules, "sounddevice", module)


def test_macos_microphone_runs_in_a_child_named_by_device(monkeypatch):
    """given: macOS and a PortAudio index
    when: the microphone capture is created
    then: nothing starts yet, and the child is told the device's name"""
    _fake_portaudio(monkeypatch)
    capture = create_mic_capture(3, SR, platform="darwin")
    assert isinstance(capture, ChildMicCapture)
    assert capture.device_name == MIC
    assert MIC in capture.argv
    assert capture.process is None
    assert "-I" in capture.argv


def test_macos_default_input_has_no_name_to_pin(monkeypatch):
    """given: macOS and no device, meaning the default input
    when: the capture is created
    then: the child gets no name and picks the default on every start"""
    capture = create_mic_capture(None, SR, platform="darwin")
    assert capture.device_name is None
    assert "--device" not in capture.argv


def _rust_worker(monkeypatch, tmp_path, built=True):
    binary = tmp_path / "recog-mic"
    if built:
        binary.write_text("#!/bin/sh\nexit 0\n")
        binary.chmod(0o755)
    monkeypatch.setattr("recog.mic_capture.RUST_MIC_WORKER", binary)
    return binary


def test_default_setting_runs_the_python_worker(monkeypatch, tmp_path):
    """given: [live] mic_worker left at its default, and a built Rust worker
    when: the microphone capture is created
    then: the Python worker runs: the default does not change with a build"""
    _rust_worker(monkeypatch, tmp_path)
    capture = create_mic_capture(None, SR, platform="darwin")
    assert capture.argv[1] == "-I"
    assert capture.argv[2].endswith("mic_worker.py")


def test_rust_setting_runs_the_binary_with_the_same_arguments(monkeypatch, tmp_path):
    """given: [live] mic_worker = "rust" and the binary built
    when: the microphone capture is created for a named device
    then: the binary is the child, given the rate and the name like the Python worker"""
    _fake_portaudio(monkeypatch)
    binary = _rust_worker(monkeypatch, tmp_path)
    monkeypatch.setattr(INSTALL_CONFIG.live, "mic_worker", "rust")

    capture = create_mic_capture(3, SR, platform="darwin")

    assert isinstance(capture, ChildMicCapture)
    assert capture.argv == [str(binary), "--sample-rate", str(SR), "--device", MIC]


def test_rust_setting_without_a_build_falls_back_to_the_python_worker_and_says_so(
    monkeypatch, tmp_path, capsys
):
    """given: [live] mic_worker = "rust" but no cargo when setup ran
    when: the microphone capture is created
    then: the Python worker runs, so the microphone still works, and the log says why"""
    _rust_worker(monkeypatch, tmp_path, built=False)
    monkeypatch.setattr(INSTALL_CONFIG.live, "mic_worker", "rust")

    capture = create_mic_capture(None, SR, platform="darwin")

    assert capture.argv[2].endswith("mic_worker.py")
    assert "not built" in capsys.readouterr().err


def test_unknown_worker_setting_stops_startup_with_its_name(monkeypatch):
    """given: a typo in [live] mic_worker
    when: the microphone capture is created
    then: startup stops naming the setting instead of quietly picking one"""
    monkeypatch.setattr(INSTALL_CONFIG.live, "mic_worker", "rsut")
    with pytest.raises(ValueError, match=r"\[live\] mic_worker"):
        create_mic_capture(None, SR, platform="darwin")


def test_linux_microphone_stays_in_process(monkeypatch):
    """given: Linux
    when: the microphone capture is created
    then: it is the capture it has always been"""
    _fake_portaudio(monkeypatch)
    assert isinstance(create_mic_capture(3, SR, platform="linux"), SingleDeviceCapture)


def test_header_is_what_the_fake_child_above_writes():
    """given: the fake children above hardcode the header
    when: compared with the real one
    then: they do not test another format"""
    assert FRAME_HEADER.format.lstrip("<") == "ddI"
    assert struct.calcsize("<ddI") == FRAME_HEADER.size


# --- echo cancellation ----------------------------------------------------------


class _FakeStage:
    """Holds nothing back and doubles the microphone, to tell it from raw."""

    def __init__(self, fail_on=None):
        self.references = []
        self.mic_times = []
        self.fail_on = fail_on

    def add_reference(self, samples, time_s, echo_possible):
        if self.fail_on == "reference":
            raise RuntimeError("stage failed")
        self.references.append((samples.tolist(), time_s, echo_possible))

    def add_mic(self, samples, time_s):
        if self.fail_on == "mic":
            raise RuntimeError("stage failed")
        self.mic_times.append(time_s)
        return samples * 2

    def flush_raw(self):
        return np.array([0.9], dtype=np.float32)


def _reference_frame(samples, pts=50.0, wall=2000.0, echo=True):
    samples = np.asarray(samples, dtype=np.float32)
    return REFERENCE_HEADER.pack(pts, wall, len(samples), echo) + samples.tobytes()


def test_microphone_goes_through_the_stage_by_its_adc_time():
    """given: a capture with an echo stage
    when: frames are decoded
    then: the stage gets each with its ADC time, and its output is what reaches the buffer"""
    stage = _FakeStage()
    capture = ChildMicCapture(MIC, SR, argv=["unused"], echo_stage=stage)

    batches = capture._decode_read(_frame([0.1], adc=7.0) + _frame([0.2], adc=7.5))

    assert stage.mic_times == [7.0, 7.5]
    assert batches[0][0].tolist() == pytest.approx([0.2, 0.4])


def test_stage_turning_on_and_off_is_logged(capsys):
    """given: the stage is the kind of thing a user asks about ("is it working now?")
    when: it starts and stops cancelling
    then: each change is logged once"""
    stage = _FakeStage()
    stage.active = False
    capture = ChildMicCapture(MIC, SR, argv=["unused"], echo_stage=stage)

    capture._decode_read(_frame([0.1]))
    assert "cancelling" not in capsys.readouterr().err
    stage.active = True
    capture._decode_read(_frame([0.1]) + _frame([0.1]))
    assert capsys.readouterr().err.count("Echo cancelling") == 1
    stage.active = False
    capture._decode_read(_frame([0.1]))
    assert capsys.readouterr().err.count("raw") == 1


def test_failing_stage_leaves_the_microphone_raw(capsys):
    """given: the stage raises
    when: the microphone keeps coming
    then: what it held is flushed, audio continues raw and the failure is logged:
    echo cancellation must never stop microphone transcription"""
    capture = ChildMicCapture(MIC, SR, argv=["unused"], echo_stage=_FakeStage(fail_on="mic"))

    first = capture._decode_read(_frame([0.1]))
    second = capture._decode_read(_frame([0.2]))

    assert first[0][0].tolist() == pytest.approx([0.9, 0.1])
    assert second[0][0].tolist() == pytest.approx([0.2])
    assert "stage failed" in capsys.readouterr().err


def test_reference_reaches_the_stage_and_the_system_buffer():
    """given: framed system audio
    when: decoded, also when a read cuts a frame
    then: the stage gets it with its time and output state, the system path the same audio"""
    stage = _FakeStage()
    capture = FramedSystemCapture(["unused"], stage, sample_rate=SR)
    data = _reference_frame([0.1, 0.2], pts=50.0, wall=2000.0, echo=False)

    assert capture._decode_read(data[:7]) == []
    batches = capture._decode_read(data[7:])

    assert stage.references[0][1:] == (50.0, False)
    assert stage.references[0][0] == pytest.approx([0.1, 0.2])
    assert batches[0][0].tolist() == pytest.approx([0.1, 0.2])
    assert batches[0][1] == 2000.0


def test_failing_stage_does_not_touch_the_system_audio(capsys):
    """given: the stage raises on the reference
    when: system audio keeps coming
    then: the system path is untouched and the stage is not called again"""
    stage = _FakeStage(fail_on="reference")
    capture = FramedSystemCapture(["unused"], stage, sample_rate=SR)

    assert capture._decode_read(_reference_frame([0.1]))[0][0].tolist() == pytest.approx([0.1])
    assert capture._decode_read(_reference_frame([0.2]))[0][0].tolist() == pytest.approx([0.2])
    assert capsys.readouterr().err.count("stage failed") == 1


def test_helper_is_only_asked_for_frames_with_a_stage(monkeypatch):
    """given: the switch off, there is no stage
    when: the system capture is created
    then: it is the capture it has always been"""
    monkeypatch.setattr("recog.system_audio.find_or_build_sck_utility", lambda: "/helper")
    plain = create_system_capture("screencapture", SR)
    assert type(plain) is SubprocessPCMCapture and plain.argv == ["/helper"]

    framed = create_system_capture("screencapture", SR, echo_stage=_FakeStage())
    assert isinstance(framed, FramedSystemCapture) and framed.argv == ["/helper", "--frames"]


def test_echo_switch_off_builds_no_stage(monkeypatch):
    """given: the switch off, the default
    when: the stage is asked for
    then: there is none, and the canceller is not even imported"""
    monkeypatch.setattr(INSTALL_CONFIG.live, "echo_cancel", False)
    monkeypatch.setitem(sys.modules, "recog.echo_cancel", None)
    assert create_echo_stage("screencapture", SR) is None


def test_echo_stage_needs_a_reference_to_exist(monkeypatch):
    """given: the switch on
    when: the system audio has no time stamps to offer
    then: there is no stage"""
    monkeypatch.setattr(INSTALL_CONFIG.live, "echo_cancel", True)
    assert create_echo_stage("none", SR) is None
    assert create_echo_stage("pulse", SR) is None


def test_echo_stage_that_cannot_be_built_leaves_the_microphone_raw(monkeypatch, capsys):
    """given: the switch on and the canceller missing
    when: the stage is asked for
    then: there is none, the log says so, and nothing stops"""
    monkeypatch.setattr(INSTALL_CONFIG.live, "echo_cancel", True)
    monkeypatch.setitem(sys.modules, "recog.echo_cancel", None)
    assert create_echo_stage("screencapture", SR) is None
    assert "Echo cancellation is off" in capsys.readouterr().err
