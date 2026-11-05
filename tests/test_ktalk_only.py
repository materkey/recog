"""Recording policy exercises real bridge/storage transitions with fake devices."""

import asyncio
import json
import threading
import time
import wave
from unittest.mock import AsyncMock, Mock

import numpy as np
import pytest

from recog import live
from recog.audio_capture import BufferedAudioCapture
from recog.cli import build_parser
from recog.ktalk_bridge import PcmChunk
from recog.live import SingleDeviceCapture
from recog.per_speaker import PerSpeakerIngest
from recog.refine import RefineTask, RefineWorker
from recog.remote_asr import ASRTemporarilyUnavailable


class Capture(BufferedAudioCapture):
    def __init__(self, device, sample_rate, name):
        super().__init__(sample_rate, name)
        self.starts = 0
        self.stops = 0

    def start(self):
        self.running = True
        self.starts += 1

    def stop(self):
        self.running = False
        self.stops += 1

    def speech(self):
        self._append_locked_for_tests(np.ones(48000, dtype=np.float32), time.time())


@pytest.fixture
def transcriber(tmp_path, monkeypatch):
    monkeypatch.setenv("RECOG_DATA_DIR", str(tmp_path))
    monkeypatch.setattr(live, "SingleDeviceCapture", Capture)
    monkeypatch.setattr(
        live, "create_mic_capture", lambda device, rate: Capture(device, rate, "mic")
    )
    monkeypatch.setattr(live, "build_asr", lambda: Mock(available=True))
    t = live.LiveTranscriber(
        mic_device=1, system_audio="device:2", ktalk_only=True, save_audio=True
    )
    t._mic_state = live.SourceState()
    t._sys_state = live.SourceState()
    t._setup_meeting_storage()
    t._setup_ktalk_bridge()
    t._ktalk_bridge._increment_generator = None
    t._transcribe_direct = Mock(return_value="Рабочая встреча")
    t._start_audio_captures()
    yield t
    t._set_conference_capture(False)
    for writer in (t._wav_writer_mic, t._wav_writer_system):
        if writer:
            writer.finalize()


async def join(t, ws=None, platform="ktalk"):
    ws = ws or AsyncMock()
    t._ktalk_bridge._connected_clients.add(ws)
    await t._ktalk_bridge._process_message(
        {
            "type": "meeting_join",
            "platform": platform,
            "url": "https://example.ktalk.ru/work",
            "title": "Work",
        },
        websocket=ws,
    )
    return ws


def assert_idle(t):
    assert not t._recording_allowed
    assert not t.mic_capture.running
    assert not t.system_capture.running
    assert not t.mic_capture.buffer
    assert not t.system_capture.buffer


def test_startup_does_not_open_devices_or_write_audio(transcriber):
    t = transcriber
    assert t.use_ktalk
    assert t._meeting_storage.list_active_meetings() == []
    assert_idle(t)
    assert t.mic_capture.starts == t.system_capture.starts == 0
    assert not list(t._meeting_storage.base_path.rglob("*.wav"))
    t._process_chunk(t.mic_capture, t._mic_state, is_mic=True)
    t._transcribe_direct.assert_not_called()


@pytest.mark.asyncio
async def test_join_records_leave_discards_buffers_and_rejoin_is_fresh(transcriber):
    t = transcriber
    ws = await join(t)
    assert t.mic_capture.running and t.system_capture.running
    assert len(t._meeting_storage.list_active_meetings()) == 1
    t.mic_capture.speech()
    t._process_chunk(t.mic_capture, t._mic_state, is_mic=True)
    meeting_path = t._meeting_storage.current_meeting_path
    assert (meeting_path / "audio_mic.wav").stat().st_size > 44
    assert "Рабочая встреча" in (meeting_path / "transcript.jsonl").read_text()
    t._sys_state.pending_chunk = (np.ones(48000), time.time(), t._meeting_generation)
    t.system_capture.speech()
    await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
    assert_idle(t)
    assert t._sys_state.pending_chunk is None
    assert t._meeting_storage.list_active_meetings() == []
    assert t._wav_writer_mic is t._wav_writer_system is None
    assert not list(t._meeting_storage.current_meeting_path.glob("*.wav"))
    await join(t, ws)
    assert t.mic_capture.running and t.system_capture.running
    assert t.mic_capture.get_chunk(3) is None
    assert t.system_capture.get_chunk(3) is None


@pytest.mark.asyncio
async def test_same_room_join_reopens_writers_and_capture(transcriber):
    t = transcriber
    ws = await join(t)
    old_path = t._meeting_storage.current_meeting_path
    t.mic_capture.speech()
    await join(t, ws)
    assert t._meeting_storage.current_meeting_path == old_path
    assert t.mic_capture.running
    assert t._wav_writer_mic is not None
    assert not t.mic_capture.buffer


@pytest.mark.asyncio
@pytest.mark.parametrize("platform", ["jitsi", "vas3k", "matrix", "gmeet", "zoom"])
async def test_other_platform_does_not_start_recording(transcriber, platform):
    t = transcriber
    ws = await join(t, platform=platform)
    assert_idle(t)
    assert json.loads(ws.send.call_args.args[0])["reason"] == "ktalk_only"
    assert t._meeting_storage._load_meta()["source"] == "orphan"


@pytest.mark.asyncio
async def test_idle_tab_cannot_stop_owner_meeting(transcriber):
    t = transcriber
    await join(t)
    await t._ktalk_bridge._process_message({"type": "meeting_leave"}, AsyncMock())
    assert t.mic_capture.running


@pytest.mark.asyncio
async def test_owner_socket_disconnect_stops_capture_with_idle_tab_connected(transcriber):
    t = transcriber
    bridge = t._ktalk_bridge
    bridge._connected_clients.add(AsyncMock())

    class Socket:
        remote_address = ("127.0.0.1", 1)

        async def send(self, message):
            pass

        def __aiter__(self):
            return self.frames()

        async def frames(self):
            yield json.dumps({"type": "meeting_join", "url": "https://example.ktalk.ru/work"})
            assert t.mic_capture.running

    await bridge._handle_client(Socket())
    assert_idle(t)
    assert bridge._connected_clients  # The unrelated idle tab is still open.


@pytest.mark.asyncio
async def test_connection_status_false_stops_capture(transcriber):
    t = transcriber
    ws = await join(t)
    await t._ktalk_bridge._process_message({"type": "connection_status", "connected": False}, ws)
    assert_idle(t)
    assert t._wav_writer_mic is None


@pytest.mark.asyncio
async def test_disconnect_stops_even_if_storage_metadata_is_unreadable(transcriber, monkeypatch):
    t = transcriber
    await join(t)
    generation = t._meeting_generation
    monkeypatch.setattr(t._meeting_storage, "_load_meta", Mock(side_effect=OSError("disk error")))
    await t._ktalk_bridge._end_meeting_on_extension_gone()
    assert_idle(t)
    assert t._meeting_generation > generation


@pytest.mark.asyncio
async def test_leave_during_asr_drops_late_transcript(transcriber):
    t = transcriber
    ws = await join(t)
    started, finish = threading.Event(), threading.Event()

    def transcribe(chunk):
        started.set()
        assert finish.wait(3)
        return "Late result"

    t._transcribe_direct = transcribe
    t.mic_capture.speech()
    worker = threading.Thread(target=t._process_chunk, args=(t.mic_capture, t._mic_state, True))
    worker.start()
    try:
        assert started.wait(3)
        await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
    finally:
        finish.set()
        worker.join(3)
    assert not worker.is_alive()
    assert_idle(t)
    assert not t.transcript_lines
    assert "Late result" not in "".join(
        p.read_text() for p in t._meeting_storage.base_path.rglob("transcript.jsonl")
    )


@pytest.mark.asyncio
async def test_partial_capture_start_failure_closes_microphone(transcriber, monkeypatch):
    t = transcriber
    monkeypatch.setattr(t.system_capture, "start", Mock(side_effect=OSError("device gone")))
    ws = await join(t)
    assert_idle(t)
    assert not t._ktalk_bridge.is_recording_active()
    assert t._meeting_storage.list_active_meetings() == []
    assert t._wav_writer_mic is t._wav_writer_system is None
    assert t._ktalk_bridge._meeting_owner_ws is None
    assert json.loads(ws.send.call_args.args[0])["reason"] == "capture_unavailable"


@pytest.mark.asyncio
async def test_join_before_main_loop_initialization_waits_for_ready(transcriber):
    t = transcriber
    t._captures_ready = False
    ws = await join(t)
    assert not t.mic_capture.running
    assert t._meeting_storage.list_active_meetings() == []
    t._start_audio_captures()
    assert not t.mic_capture.running
    await join(t, ws)
    assert t.mic_capture.running
    t._start_audio_captures()
    assert t.mic_capture.starts == 1


@pytest.mark.asyncio
async def test_no_save_audio_still_gates_capture(transcriber):
    t = transcriber
    t.save_audio = False
    ws = await join(t)
    assert t.mic_capture.running
    assert t._wav_writer_mic is None
    await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
    assert_idle(t)


def test_cli_ktalk_only_is_opt_in():
    assert build_parser().parse_args(["live", "--ktalk-only"]).ktalk_only
    assert not build_parser().parse_args(["live", "--ktalk"]).ktalk_only


@pytest.mark.asyncio
async def test_foreign_tab_pcm_does_not_reach_ingest(transcriber):
    t = transcriber
    await join(t)
    ingest = Mock()
    t._ktalk_bridge.set_pcm_chunk_callback(ingest)
    await t._ktalk_bridge._process_message({"type": "pcm_chunk"}, AsyncMock())
    ingest.assert_not_called()


@pytest.mark.asyncio
async def test_bridge_stop_closes_capture_and_invalidates_inflight_results(transcriber):
    t = transcriber
    await join(t)
    generation = t._meeting_generation
    t._ktalk_bridge.stop()
    assert_idle(t)
    assert t._meeting_generation > generation


@pytest.mark.asyncio
async def test_one_failed_stop_still_closes_the_other_device(transcriber, monkeypatch):
    t = transcriber
    ws = await join(t)

    def stop():
        t.mic_capture.running = False
        raise OSError("stop failed")

    monkeypatch.setattr(t.mic_capture, "stop", stop)
    with pytest.raises(OSError, match="stop failed"):
        await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
    assert_idle(t)
    monkeypatch.setattr(t.mic_capture, "stop", lambda: None)


@pytest.mark.asyncio
async def test_failed_join_can_retry_after_device_recovers(transcriber, monkeypatch):
    t = transcriber
    start = t.system_capture.start
    monkeypatch.setattr(t.system_capture, "start", Mock(side_effect=OSError("busy")))
    ws = await join(t)
    assert_idle(t)
    monkeypatch.setattr(t.system_capture, "start", start)
    ws.send.reset_mock()
    await join(t, ws)
    assert t._ktalk_bridge.is_recording_active()
    assert t.mic_capture.running and t.system_capture.running
    assert len(t._meeting_storage.list_active_meetings()) == 1
    assert t._ktalk_bridge._copilot_tick_state is not None
    frames = [json.loads(call.args[0]) for call in ws.send.call_args_list]
    assert any(frame["type"] == "meeting_started" for frame in frames)
    assert any(frame["type"] == "transcript_history" for frame in frames)


@pytest.fixture
def pcm_transcriber(transcriber):
    """Use the real ingest and refine queues; tick them deterministically."""
    t = transcriber
    t._per_speaker_ingest = PerSpeakerIngest(
        transcribe_callback=t._on_pcm_batch,
        generation_getter=lambda: t._meeting_generation,
        asr_available_getter=t._asr_available,
    )
    t._ktalk_bridge.set_pcm_chunk_callback(t._per_speaker_ingest.enqueue)
    t._refine_worker = RefineWorker(
        threading.RLock(),
        Mock(return_value=None),
        t._apply_refine,
        generation_getter=lambda: t._meeting_generation,
    )
    yield t
    t._refine_worker.stop()


def enqueue_speech(t):
    t._per_speaker_ingest.enqueue(
        PcmChunk(
            pid="p1",
            speaker_name="Alice",
            is_local=False,
            seq=1,
            chunk_start_ts=time.time(),
            chunk_end_ts=time.time() + 3,
            sample_rate=16000,
            pcm=np.full(48000, 3000, dtype=np.int16).tobytes(),
        )
    )


@pytest.mark.asyncio
async def test_leave_discards_pcm_retry_before_asr_recovers(pcm_transcriber):
    t = pcm_transcriber
    ws = await join(t)
    t.asr.transcribe_batch.side_effect = ASRTemporarilyUnavailable("offline")
    enqueue_speech(t)
    t._per_speaker_ingest.tick()
    assert t._per_speaker_ingest._pending_retry_batch is not None
    await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
    assert t._per_speaker_ingest._pending_retry_batch is None
    t.asr.transcribe_batch.reset_mock(side_effect=True)
    t._per_speaker_ingest.tick()
    t.asr.transcribe_batch.assert_not_called()


@pytest.mark.asyncio
@pytest.mark.parametrize("source", ["mic", "pcm"])
async def test_late_asr_failure_cannot_repopulate_retry_after_leave(pcm_transcriber, source):
    t = pcm_transcriber
    ws = await join(t)
    entered, finish = threading.Event(), threading.Event()

    def fail(_audio):
        entered.set()
        assert finish.wait(3)
        raise ASRTemporarilyUnavailable("offline")

    if source == "mic":
        t._transcribe_direct = fail
        t.mic_capture.speech()
        worker = threading.Thread(target=t._process_chunk, args=(t.mic_capture, t._mic_state, True))
    else:
        t.asr.transcribe_batch.side_effect = fail
        enqueue_speech(t)
        worker = threading.Thread(target=t._per_speaker_ingest.tick)
    worker.start()
    try:
        assert entered.wait(3)
        await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
        # Rejoin before the old failure returns: active alone isn't sufficient.
        await join(t, ws)
    finally:
        finish.set()
        worker.join(3)
    assert not worker.is_alive()
    assert t._mic_state.pending_chunk is None
    assert t._per_speaker_ingest._pending_retry_batch is None


@pytest.mark.asyncio
async def test_leave_discards_refine_queue_and_rejects_old_generation(pcm_transcriber):
    t = pcm_transcriber
    ws = await join(t)
    task = RefineTask("p1", np.ones(16000, dtype=np.int16), 0, time.time(), t._meeting_generation)
    t._refine_worker.submit(task)
    assert t._refine_worker._queue.qsize() == 1
    await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
    assert t._refine_worker._queue.empty()
    assert t._refine_worker.is_paused()
    await join(t, ws)
    t._refine_worker.submit(task)
    assert t._refine_worker._queue.empty()
    t._refine_worker._process_task(task)
    t._refine_worker._transcribe_fn.assert_not_called()


@pytest.mark.asyncio
async def test_bridge_stop_updates_persisted_recording_status(transcriber):
    t = transcriber
    await join(t)
    t._ktalk_bridge.stop()
    assert t._meeting_storage.list_active_meetings() == []
    assert t._wav_writer_mic is t._wav_writer_system is None


@pytest.mark.asyncio
async def test_late_refine_failure_cannot_requeue_after_rejoin(pcm_transcriber):
    t = pcm_transcriber
    ws = await join(t)
    task = RefineTask("p1", np.ones(16000, dtype=np.int16), 0, time.time(), t._meeting_generation)
    entered, finish = threading.Event(), threading.Event()

    def fail(_audio, **_kwargs):
        entered.set()
        assert finish.wait(3)
        raise ASRTemporarilyUnavailable("offline")

    t._refine_worker._transcribe_fn.side_effect = fail
    worker = threading.Thread(target=t._refine_worker._process_task, args=(task,))
    worker.start()
    try:
        assert entered.wait(3)
        await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
        await join(t, ws)
    finally:
        finish.set()
        worker.join(3)
    assert not worker.is_alive()
    assert t._refine_worker._queue.empty()


@pytest.mark.parametrize("failed_operation", [None, "start", "stop", "close"])
def test_device_errors_close_stream_and_allow_restart(monkeypatch, failed_operation):
    capture = SingleDeviceCapture(device=1)
    stream = Mock()
    backend = Mock()
    backend.query_devices.return_value = {"max_input_channels": 1}
    backend.InputStream.return_value = stream
    monkeypatch.setattr(capture, "sd", backend)
    if failed_operation:
        getattr(stream, failed_operation).side_effect = OSError(failed_operation)
        with pytest.raises(OSError, match=failed_operation):
            capture.start()
            capture.stop()
    else:
        capture.start()
        capture.stop()
    assert not capture.running
    stream.close.assert_called_once()
    capture.stop()
    stream.close.assert_called_once()

    backend.InputStream.return_value = Mock()
    capture.start()
    assert capture.running
    capture.stop()
    backend.InputStream.return_value.close.assert_called_once()


@pytest.mark.parametrize("ktalk_only", [False, True])
def test_run_finalizes_recording_and_session_on_shutdown(
    transcriber, monkeypatch, tmp_path, ktalk_only
):
    t = transcriber
    t.ktalk_only = ktalk_only
    t._recording_allowed = not ktalk_only
    t.output_file = tmp_path / "transcript.txt"
    monkeypatch.setattr(t, "_setup_per_speaker_ingest", Mock())
    monkeypatch.setattr(
        t, "_start_ktalk_bridge", lambda: setattr(t._ktalk_bridge, "_increment_generator", None)
    )
    recorded_files = []

    def record_once(_delay):
        if ktalk_only:
            assert not t.mic_capture.running
            asyncio.run(join(t))
        assert t.mic_capture.running and t.system_capture.running
        t.mic_capture.speech()
        recorded_files.extend([t._wav_writer_mic.path, t._wav_writer_system.path])
        t.running = False

    monkeypatch.setattr(live.time, "sleep", record_once)
    t.run()
    assert not t.mic_capture.running
    assert not t.system_capture.running
    assert t._meeting_storage.list_active_meetings() == []
    assert "Рабочая встреча" in t.output_file.read_text()
    with wave.open(str(recorded_files[0])) as recording:
        assert recording.getnframes() == 48000
    for writer in (t._wav_writer_mic, t._wav_writer_system):
        assert writer is None or writer._file is None


@pytest.mark.asyncio
async def test_debug_status_follows_capture_owner(transcriber, tmp_path):
    t = transcriber
    bridge = t._ktalk_bridge
    bridge._debug_file = tmp_path / "debug.json"
    bridge._debug_update_interval = 0
    bridge.set_pcm_stats_getter(lambda: {"queued": 0})

    def recording_status():
        bridge._write_debug_state()
        return json.loads(bridge._debug_file.read_text())["recording"]

    assert recording_status() == {"policy": "ktalk_only", "active": False}
    ws = await join(t)
    assert recording_status() == {"policy": "ktalk_only", "active": True}
    await bridge._process_message({"type": "meeting_leave"}, ws)
    assert recording_status() == {"policy": "ktalk_only", "active": False}
    assert json.loads(bridge._debug_file.read_text())["pcm"] == {"queued": 0}


@pytest.mark.asyncio
async def test_pcm_drained_before_leave_cannot_run_after_rejoin(pcm_transcriber):
    t = pcm_transcriber
    ws = await join(t)
    enqueue_speech(t)
    with t._per_speaker_ingest._lock:
        batch = t._per_speaker_ingest._drain_batch_locked()
    assert batch
    await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
    await join(t, ws)
    t._on_pcm_batch(batch)
    t.asr.transcribe_batch.assert_not_called()
    assert not t.transcript_lines


@pytest.mark.asyncio
async def test_leave_during_capture_read_cannot_write_into_rejoined_meeting(
    transcriber, monkeypatch
):
    t = transcriber
    ws = await join(t)
    t.mic_capture.speech()
    get_chunk = t.mic_capture.get_chunk
    entered, finish = threading.Event(), threading.Event()

    def delayed_chunk(duration):
        result = get_chunk(duration)
        entered.set()
        assert finish.wait(3)
        return result

    monkeypatch.setattr(t.mic_capture, "get_chunk", delayed_chunk)
    worker = threading.Thread(target=t._process_chunk, args=(t.mic_capture, t._mic_state, True))
    worker.start()
    try:
        assert entered.wait(3)
        await t._ktalk_bridge._process_message({"type": "meeting_leave"}, ws)
        await join(t, ws)
    finally:
        finish.set()
        worker.join(3)
    assert not worker.is_alive()
    t._transcribe_direct.assert_not_called()
    assert t._wav_writer_mic.get_offset_ms() == 0
    assert not t.transcript_lines
