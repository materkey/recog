"""Тесты recog/system_audio.py.

Модуль отвечает за системный звук на обеих ОС: ScreenCaptureKit-хелпер на
macOS и `parec` на Linux, оба поверх одного `SubprocessPCMCapture`. Тесты
держат три контракта, которые нельзя проверить на живой платформе:
- пути до Swift-хелпера считаются от корня репо, а не от каталога пакета
  (`find_or_build_sck_utility` зовётся из `__init__`, ошибка роняет live);
- транзиентная смерть ребёнка не убивает reader-поток навсегда;
- смена default sink переподключает parec ровно один раз и не расходует
  бюджет crash-retry.
"""
import io
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from recog import system_audio
from recog.system_audio import (
    MAX_RESTARTS,
    PULSE_DEFAULT_MONITOR,
    REPO_ROOT,
    RESTART_BACKOFF_S,
    PulseAudioCapture,
    SubprocessPCMCapture,
    create_system_capture,
    find_or_build_sck_utility,
    parec_argv,
    resolve_pulse_monitor_source,
    resolve_system_audio_backend,
)

SR = 16000


# =============================================================================
# Swift-хелпер: пути от корня репо
# =============================================================================


def test_repo_root_points_at_the_repository_not_the_package():
    package_dir = Path(system_audio.__file__).resolve().parent
    assert REPO_ROOT == package_dir.parent
    assert (REPO_ROOT / "mp4_notes.py").exists()
    assert not (package_dir / "sck_audio_capture.swift").exists()


def test_existing_binary_in_repo_root_is_found(tmp_path):
    binary = tmp_path / "sck_audio_capture"
    binary.write_bytes(b"")

    assert find_or_build_sck_utility(tmp_path) == str(binary)


def test_missing_binary_and_source_raises(tmp_path):
    with pytest.raises(FileNotFoundError, match="sck_audio_capture"):
        find_or_build_sck_utility(tmp_path)


def test_stale_binary_is_recompiled_from_repo_root_source(tmp_path, monkeypatch):
    binary = tmp_path / "sck_audio_capture"
    source = tmp_path / "sck_audio_capture.swift"
    binary.write_bytes(b"")
    source.write_text("// newer than the binary")
    import os
    os.utime(binary, (0, 0))

    argv = {}

    def fake_run(cmd, **kwargs):
        argv["cmd"] = cmd
        binary.write_bytes(b"rebuilt")

        class Result:
            returncode = 0
            stderr = ""

        return Result()

    monkeypatch.setattr(system_audio.subprocess, "run", fake_run)

    assert find_or_build_sck_utility(tmp_path) == str(binary)
    assert argv["cmd"][:4] == ["swiftc", "-o", str(binary), str(source)]


# =============================================================================
# parec: argv и выбор источника
# =============================================================================


def test_parec_argv_asks_for_raw_mono_float32():
    argv = parec_argv("alsa_output.pci.analog-stereo.monitor")

    assert argv[0] == "parec"
    assert "--raw" in argv
    assert "--format=float32le" in argv
    assert f"--rate={SR}" in argv
    assert "--channels=1" in argv
    assert "--latency-msec=100" in argv
    # Без --device parec пишет дефолтный source (микрофон), а не монитор.
    assert "--device=alsa_output.pci.analog-stereo.monitor" in argv


def test_parec_argv_follows_capture_sample_rate():
    assert "--rate=48000" in parec_argv("x.monitor", sample_rate=48000)


def test_monitor_source_defaults_to_default_monitor():
    assert resolve_pulse_monitor_source(env={}) == PULSE_DEFAULT_MONITOR


def test_env_override_pins_the_monitor_source():
    env = {"RECOG_PULSE_MONITOR_SOURCE": "  bluez.monitor  "}

    assert resolve_pulse_monitor_source(env=env) == "bluez.monitor"


def test_blank_env_override_falls_back_to_default_monitor():
    assert resolve_pulse_monitor_source(env={"RECOG_PULSE_MONITOR_SOURCE": "   "}) == PULSE_DEFAULT_MONITOR


# =============================================================================
# reader-loop: разбор wire-протокола и restart-политика
# =============================================================================


def _make_capture(argv=("fake",), name="test-capture") -> SubprocessPCMCapture:
    cap = SubprocessPCMCapture.__new__(SubprocessPCMCapture)
    SubprocessPCMCapture.__init__(cap, list(argv), name, sample_rate=SR,
                                  max_buffer_s=10.0)
    return cap


class _StopWhenDrained(io.BytesIO):
    """stdout фейкового процесса: исчерпался — глушим reader-поток."""

    def __init__(self, payload: bytes, cap):
        super().__init__(payload)
        self._cap = cap

    def read(self, size=-1):
        data = super().read(size)
        if not data:
            self._cap.running = False
        return data


class _FakeProcess:
    def __init__(self, stdout):
        self.stdout = stdout
        self.stderr = None
        self.pid = 4242

    def poll(self):
        return None

    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0

    def kill(self):
        pass


class _OneBlockThenDead:
    """Процесс, отдающий один блок PCM и умирающий (poll() -> 0)."""

    def __init__(self, payload: bytes):
        self.stdout = io.BytesIO(payload)
        self.stderr = None
        self.pid = 4243
        self._alive = True

    def poll(self):
        if self.stdout.tell() >= len(self.stdout.getvalue()):
            self._alive = False
        return None if self._alive else 0

    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0

    def kill(self):
        pass


def test_reader_loop_parses_float32_blocks_from_stdout():
    """given: процесс, отдающий 0.2 с float32 PCM
    when: reader-loop прочитал stdout до конца
    then: буфер содержит те же сэмплы"""
    cap = _make_capture()
    samples = (np.arange(SR // 5, dtype=np.float32) / SR).astype(np.float32)
    cap.process = _FakeProcess(_StopWhenDrained(samples.tobytes(), cap))
    cap.running = True

    cap._reader_loop()

    assert cap.buffer, "reader-loop не положил ничего в буфер"
    np.testing.assert_allclose(np.concatenate(cap.buffer), samples)
    assert not cap._dead


def test_transient_child_death_is_retried_with_backoff_then_gives_up():
    """given: ребёнок умирает сразу после каждого respawn
    when: reader-loop крутится
    then: попытки исчерпываются с backoff, затем _dead/running=False —
          а не выход из потока на первой же ошибке"""
    cap = _make_capture(name="pulse")
    cap.running = True
    cap.process = None

    attempts = []
    delays = []

    def failing_restart():
        attempts.append(1)
        raise RuntimeError("Audio capture failed to restart: parec: connection refused")

    cap._restart_process = failing_restart
    cap._sleep_while_running = lambda delay: delays.append(delay)

    cap._reader_loop()

    assert len(attempts) == MAX_RESTARTS
    assert delays == [
        RESTART_BACKOFF_S[min(i, len(RESTART_BACKOFF_S) - 1)] for i in range(MAX_RESTARTS)
    ]
    assert cap._dead is True
    assert cap.dead is True
    assert cap.running is False


def test_successful_read_resets_the_restart_budget():
    """given: рестарт удался, чанк прочитан, ребёнок умер снова
    when: reader-loop крутится до отказа
    then: backoff отсчитывается заново с первого шага — здоровый отрезок
          захвата не приближает give-up"""
    cap = _make_capture()
    cap.running = True
    cap.process = None

    delays = []
    cap._sleep_while_running = lambda delay: delays.append(delay)

    samples = np.ones(SR // 10, dtype=np.float32)
    states = {"restarts": 0}

    def restart():
        states["restarts"] += 1
        if states["restarts"] == 2:
            cap.process = _OneBlockThenDead(samples.tobytes())
            return
        raise RuntimeError("dead again")

    cap._restart_process = restart

    cap._reader_loop()

    first = RESTART_BACKOFF_S[0]
    # [1] — провал до успешного рестарта; далее полный бюджет с нуля.
    assert delays == [first] + [
        RESTART_BACKOFF_S[min(i, len(RESTART_BACKOFF_S) - 1)] for i in range(MAX_RESTARTS)
    ]
    assert np.concatenate(cap.buffer).size == samples.size
    assert cap._dead is True


# =============================================================================
# PulseAudio: наблюдение за default sink
# =============================================================================


def _pactl_result(stdout: str, returncode: int = 0):
    class Result:
        pass

    result = Result()
    result.stdout = stdout
    result.returncode = returncode
    return result


def _make_pulse(sinks, env=None, poll_interval_s=0.01):
    """PulseAudioCapture с инжектированным `run` вместо pactl."""
    calls = []

    def fake_run(argv, **kwargs):
        calls.append((argv, kwargs))
        value = sinks.pop(0) if sinks else None
        if isinstance(value, Exception):
            raise value
        if isinstance(value, tuple):
            return _pactl_result(*value)
        return _pactl_result(value if value is not None else "")

    cap = PulseAudioCapture(sample_rate=SR, env=env if env is not None else {},
                            run=fake_run, poll_interval_s=poll_interval_s)
    return cap, calls


def test_pulse_capture_starts_on_the_default_monitor():
    cap, _ = _make_pulse([])

    assert cap.argv == parec_argv(PULSE_DEFAULT_MONITOR, sample_rate=SR)
    assert cap._follow_default_sink is True


def test_env_override_disables_default_sink_following():
    cap, calls = _make_pulse([], env={"RECOG_PULSE_MONITOR_SOURCE": "bluez.monitor"})

    assert cap.argv == parec_argv("bluez.monitor", sample_rate=SR)
    assert cap._follow_default_sink is False
    assert calls == []


def test_default_sink_query_uses_the_injected_runner():
    cap, calls = _make_pulse(["alsa_output.a"])

    assert cap._query_default_sink() == "alsa_output.a"
    argv, kwargs = calls[0]
    assert argv == ["pactl", "get-default-sink"]
    assert kwargs["timeout"] > 0


def test_default_sink_change_reconnects_once():
    """given: pactl отдаёт sinkA, sinkA, sinkB, sinkB
    when: наблюдение опрашивает default sink 4 раза
    then: ровно одно переподключение на монитор sinkB"""
    cap, _ = _make_pulse(["sinkA", "sinkA", "sinkB", "sinkB"])
    restarts = []
    cap._restart_process = lambda: restarts.append(cap.argv)

    cap._poll_default_sink_once()  # первое наблюдение = то, во что резолвился @DEFAULT_MONITOR@
    assert cap._take_pending_source() is None

    cap._poll_default_sink_once()
    assert cap._take_pending_source() is None

    cap._poll_default_sink_once()
    pending = cap._take_pending_source()
    assert pending == "sinkB.monitor"
    cap._apply_source_switch(pending)

    cap._poll_default_sink_once()
    assert cap._take_pending_source() is None

    assert restarts == [parec_argv("sinkB.monitor", sample_rate=SR)]
    assert cap.argv == parec_argv("sinkB.monitor", sample_rate=SR)


def test_source_switch_drops_the_buffer_so_overlap_is_not_glued():
    """given: в буфере аудио со старого источника
    when: захват переключился на новый монитор
    then: буфер пуст — overlap через разрыв не склеивается и старым
          сэмплам не присваиваются новые timestamps"""
    cap, _ = _make_pulse([])
    cap._restart_process = lambda: None
    cap._append_locked_for_tests(np.ones(SR, dtype=np.float32), 1000.0)
    assert cap.buffer

    cap._apply_source_switch("sinkB.monitor")

    assert cap.buffer == []
    assert cap._buffer_start_ts is None
    assert cap._buffered_samples == 0


def test_planned_switch_does_not_spend_the_crash_retry_budget():
    """given: захват уже потратил рестарты на падения ребёнка
    when: приходит плановая смена источника
    then: бюджет сброшен — переподключение не приближает give-up"""
    cap, _ = _make_pulse([])
    cap.running = True
    cap.process = None

    delays = []
    cap._sleep_while_running = lambda delay: delays.append(delay)
    switched = []

    def restart():
        raise RuntimeError("still dead")

    cap._restart_process = restart

    pending = {"value": "sinkB.monitor"}

    def take_pending():
        value = pending["value"]
        pending["value"] = None
        return value

    def apply_switch(source):
        switched.append(source)

    cap._take_pending_source = take_pending
    cap._apply_source_switch = apply_switch

    cap._reader_loop()

    assert switched == ["sinkB.monitor"]
    # После планового переключения бюджет полный: MAX_RESTARTS падений.
    assert len(delays) == MAX_RESTARTS
    assert cap._dead is True


@pytest.mark.parametrize("failure", [
    subprocess.TimeoutExpired(cmd="pactl", timeout=2.0),
    ("", 1),
    ("", 0),
])
def test_pactl_failure_keeps_the_current_stream(failure):
    """given: pactl упал/протаймаутил/вернул пусто
    when: наблюдение опросило default sink
    then: источник не меняется и переподключения нет"""
    cap, _ = _make_pulse(["sinkA", failure])
    cap._restart_process = lambda: pytest.fail("переподключение на ошибке pactl")

    cap._poll_default_sink_once()
    cap._poll_default_sink_once()

    assert cap._take_pending_source() is None
    assert cap.argv == parec_argv(PULSE_DEFAULT_MONITOR, sample_rate=SR)
    assert cap._last_default_sink == "sinkA"


def test_stop_ends_the_default_sink_watch(monkeypatch):
    """given: работающий pulse-захват с наблюдением за default sink
    when: capture остановлен
    then: поток наблюдения завершился вместе с ним"""
    cap, _ = _make_pulse(["sinkA"] * 100, poll_interval_s=0.01)
    cap._spawn = lambda: _FakeProcess(None)
    cap._reader_loop = lambda: None
    monkeypatch.setattr(system_audio.time, "sleep", lambda s: None)

    cap.start()
    watcher = cap._watch_thread
    assert watcher is not None and watcher.is_alive()

    cap.stop()

    assert cap._watch_thread is None
    assert not watcher.is_alive()


def test_override_start_does_not_watch_the_default_sink(monkeypatch):
    cap, calls = _make_pulse([], env={"RECOG_PULSE_MONITOR_SOURCE": "bluez.monitor"})
    cap._spawn = lambda: _FakeProcess(None)
    cap._reader_loop = lambda: None
    monkeypatch.setattr(system_audio.time, "sleep", lambda s: None)

    cap.start()
    try:
        assert cap._watch_thread is None
        assert calls == []
    finally:
        cap.stop()


# =============================================================================
# Выбор бэкенда
# =============================================================================


def test_auto_on_macos_is_screencapture():
    assert resolve_system_audio_backend("auto", platform="darwin", which=lambda n: None) == "screencapture"


def test_auto_on_linux_with_parec_is_pulse():
    assert resolve_system_audio_backend("auto", platform="linux", which=lambda n: "/usr/bin/parec") == "pulse"


def test_auto_on_linux_without_parec_explains_how_to_install():
    with pytest.raises(RuntimeError, match="pulseaudio-utils"):
        resolve_system_audio_backend("auto", platform="linux", which=lambda n: None)


def test_auto_on_unknown_platform_raises():
    with pytest.raises(RuntimeError, match="win32"):
        resolve_system_audio_backend("auto", platform="win32", which=lambda n: "/x")


def test_screencapture_outside_macos_raises():
    with pytest.raises(RuntimeError, match="macOS"):
        resolve_system_audio_backend("screencapture", platform="linux", which=lambda n: "/x")


def test_pulse_outside_linux_raises():
    with pytest.raises(RuntimeError, match="Linux"):
        resolve_system_audio_backend("pulse", platform="darwin", which=lambda n: "/x")


def test_pulse_without_parec_raises():
    with pytest.raises(RuntimeError, match="parec"):
        resolve_system_audio_backend("pulse", platform="linux", which=lambda n: None)


@pytest.mark.parametrize("spec,expected", [
    (None, "none"),
    ("", "none"),
    ("none", "none"),
    ("device:3", "device:3"),
    ("device:auto", "device:auto"),
])
def test_passthrough_specs(spec, expected):
    assert resolve_system_audio_backend(spec, platform="linux", which=lambda n: None) == expected


def test_unknown_backend_is_a_value_error():
    with pytest.raises(ValueError, match="bogus"):
        resolve_system_audio_backend("bogus", platform="darwin", which=lambda n: "/x")


def test_create_screencapture_uses_the_swift_helper(monkeypatch):
    monkeypatch.setattr(system_audio, "find_or_build_sck_utility", lambda *a, **kw: "/tmp/sck")

    cap = create_system_capture("screencapture", SR)

    assert isinstance(cap, SubprocessPCMCapture)
    assert cap.argv == ["/tmp/sck"]
    assert cap.name == "screencapture"


def test_create_pulse_capture(monkeypatch):
    monkeypatch.delenv("RECOG_PULSE_MONITOR_SOURCE", raising=False)

    cap = create_system_capture("pulse", SR)

    assert isinstance(cap, PulseAudioCapture)
    assert cap.argv == parec_argv(PULSE_DEFAULT_MONITOR, sample_rate=SR)
    assert cap.name == "pulse"


def test_create_unknown_backend_raises():
    with pytest.raises(ValueError, match="none"):
        create_system_capture("none", SR)
