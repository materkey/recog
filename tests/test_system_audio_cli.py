"""Выбор системного звука из CLI и автовыбор микрофона на обеих ОС.

Три контракта, которые ломаются молча:
- приоритет `--system-audio` над скрытыми алиасами прежнего CLI: коллега
  ставит новый флаг, а сервисный юнит/старый скрипт всё ещё передаёт
  `--screencapture`, и звук уходил бы не туда;
- `find_default_mic()` на Linux: PortAudio-имя `pulse` — единственное
  устройство, следующее за дефолтным источником сервера, а MacBook-эвристика
  там не находит ничего;
- отдельный маркер потери системного захвата: PCM-путь при этом здоров,
  поэтому watchdog-сообщение «PCM → mixed» было бы враньём, а refine должен
  остаться включённым.
"""
import sys
import threading
import time
import types
from collections import deque
from pathlib import Path
from unittest.mock import Mock

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog import cli as mp4_notes
from recog.live import (
    LiveTranscriber,
    NO_SYSTEM_AUDIO,
    SingleDeviceCapture,
    normalize_system_audio,
    parse_system_audio_device,
)


# =============================================================================
# Парсер: --system-audio и скрытые алиасы
# =============================================================================


def _spec(*argv) -> str:
    args = mp4_notes.build_parser().parse_args(["live", *argv])
    return mp4_notes.system_audio_spec(args)


def test_no_flags_means_no_system_audio():
    assert _spec() == NO_SYSTEM_AUDIO


def test_system_audio_backends_pass_through():
    assert _spec("--system-audio", "auto") == "auto"
    assert _spec("--system-audio", "pulse") == "pulse"
    assert _spec("--system-audio", "screencapture") == "screencapture"
    assert _spec("--system-audio", "device:3") == "device:3"


def test_legacy_screencapture_flag_maps_to_the_backend():
    assert _spec("--screencapture") == "screencapture"


def test_legacy_system_device_maps_to_a_device_spec():
    assert _spec("--system-device", "5") == "device:5"
    assert _spec("--system-device", "auto") == "device:auto"


def test_system_audio_wins_over_both_legacy_aliases():
    assert _spec("--system-audio", "pulse", "--screencapture",
                 "--system-device", "5") == "pulse"


def test_screencapture_wins_over_system_device():
    assert _spec("--screencapture", "--system-device", "5") == "screencapture"


def test_legacy_aliases_are_hidden_from_help(capsys):
    with pytest.raises(SystemExit):
        mp4_notes.build_parser().parse_args(["live", "--help"])

    live_help = capsys.readouterr().out
    assert "--system-audio" in live_help
    assert "--system-device" not in live_help
    assert "--screencapture" not in live_help


def test_mic_device_none_is_kept_as_a_sentinel():
    args = mp4_notes.build_parser().parse_args(["live", "--mic-device", "none"])
    assert mp4_notes.parse_device(args.mic_device) == "none"


def test_numeric_mic_device_becomes_an_int():
    args = mp4_notes.build_parser().parse_args(["live", "--mic-device", "2"])
    assert mp4_notes.parse_device(args.mic_device) == 2


# =============================================================================
# normalize_system_audio / parse_system_audio_device
# =============================================================================


def test_normalize_prefers_the_explicit_spec():
    assert normalize_system_audio("pulse", system_device=5, use_screencapture=True) == "pulse"


def test_normalize_falls_back_to_the_legacy_flags():
    assert normalize_system_audio(None, None, True) == "screencapture"
    assert normalize_system_audio(None, 5, False) == "device:5"
    assert normalize_system_audio(None, None, False) == NO_SYSTEM_AUDIO


@pytest.mark.parametrize("spec,expected", [
    ("device:5", 5),
    ("device:auto", "auto"),
    ("device:Loopback", "Loopback"),
    ("pulse", None),
    ("screencapture", None),
    ("none", None),
])
def test_device_is_parsed_out_of_the_spec(spec, expected):
    assert parse_system_audio_device(spec) == expected


# =============================================================================
# find_default_mic
# =============================================================================


def _devices(*names):
    """Список устройств в форме sounddevice.query_devices()."""
    return [{"name": name, "max_input_channels": channels} for name, channels in names]


class _FakeSounddevice:
    def __init__(self, devices, default_input=None):
        self._devices = devices
        self._default_input = default_input

    def query_devices(self, device=None, kind=None):
        if kind == "input":
            if self._default_input is None:
                raise ValueError("no default input device")
            return {"index": self._default_input}
        if device is None:
            return self._devices
        return self._devices[device]


@pytest.fixture
def fake_sd(monkeypatch):
    def install(devices, default_input=None):
        module = _FakeSounddevice(devices, default_input)
        monkeypatch.setitem(sys.modules, "sounddevice", module)
        return module
    return install


def test_macos_still_picks_the_builtin_macbook_microphone(fake_sd):
    fake_sd(_devices(("BlackHole 2ch", 2), ("MacBook Pro Microphone", 1)))

    assert SingleDeviceCapture.find_default_mic(platform="darwin") == 1


def test_macos_falls_back_to_a_built_in_device(fake_sd):
    fake_sd(_devices(("External USB", 1), ("Built-in Input", 1)))

    assert SingleDeviceCapture.find_default_mic(platform="darwin") == 1


def test_linux_prefers_the_pulse_proxy_device(fake_sd):
    fake_sd(_devices(("HDA Intel PCH: ALC257 Analog", 2), ("pulse", 32), ("default", 32)))

    assert SingleDeviceCapture.find_default_mic(platform="linux") == 1


def test_linux_accepts_pipewire_when_there_is_no_pulse_device(fake_sd):
    fake_sd(_devices(("HDA Intel PCH", 2), ("pipewire", 64)))

    assert SingleDeviceCapture.find_default_mic(platform="linux") == 1


def test_output_only_pulse_device_is_not_treated_as_a_microphone(fake_sd):
    fake_sd(_devices(("pulse", 0), ("USB mic", 1)), default_input=1)

    assert SingleDeviceCapture.find_default_mic(platform="linux") == 1


def test_falls_back_to_the_portaudio_default_input(fake_sd):
    fake_sd(_devices(("HDA Intel PCH", 2)), default_input=0)

    assert SingleDeviceCapture.find_default_mic(platform="linux") == 0


def test_returns_none_when_there_is_no_input_device_at_all(fake_sd):
    fake_sd(_devices(("Speakers", 0)))

    assert SingleDeviceCapture.find_default_mic(platform="linux") is None


# =============================================================================
# --list-devices на Linux
# =============================================================================


def test_list_devices_on_linux_also_prints_pulse_sources(fake_sd, capsys):
    fake_sd(_devices(("pulse", 32)))
    run = Mock(return_value=types.SimpleNamespace(
        returncode=0,
        stdout="0\talsa_output.pci-0000_00_1f.3.analog-stereo.monitor\tRUNNING\n",
    ))

    SingleDeviceCapture.list_devices(platform="linux", run=run)

    assert run.call_args.args[0] == ["pactl", "list", "short", "sources"]
    out = capsys.readouterr().out
    assert "analog-stereo.monitor" in out


def test_list_devices_on_macos_does_not_call_pactl(fake_sd):
    fake_sd(_devices(("MacBook Pro Microphone", 1)))
    run = Mock()

    SingleDeviceCapture.list_devices(platform="darwin", run=run)

    run.assert_not_called()


def test_missing_pactl_does_not_break_the_listing(fake_sd, capsys):
    fake_sd(_devices(("pulse", 32)))
    run = Mock(side_effect=FileNotFoundError("pactl"))

    SingleDeviceCapture.list_devices(platform="linux", run=run)

    assert "pactl" in capsys.readouterr().err


# =============================================================================
# LiveTranscriber: спека → захват
# =============================================================================


def test_device_spec_creates_a_portaudio_system_capture():
    transcriber = LiveTranscriber(mic_device="none", system_audio="device:4")

    assert transcriber.mic_capture is None
    assert transcriber.system_capture.device == 4
    assert transcriber.dual_mode is False


def test_legacy_system_device_still_reaches_the_same_capture():
    transcriber = LiveTranscriber(mic_device=1, system_device=4)

    assert transcriber.system_audio == "device:4"
    assert transcriber.system_capture.device == 4
    assert transcriber.dual_mode is True


def _pretend_linux(monkeypatch):
    """`resolve_system_audio_backend` берёт платформу дефолтом на импорте."""
    monkeypatch.setattr("recog.system_audio.resolve_system_audio_backend",
                        lambda spec, **kwargs: spec)


def test_pulse_backend_builds_a_parec_capture(monkeypatch):
    _pretend_linux(monkeypatch)

    transcriber = LiveTranscriber(mic_device="none", system_audio="pulse")

    assert transcriber.system_capture.argv[0] == "parec"
    assert transcriber.system_device is None
    assert transcriber.system_audio_label() == "PulseAudio/PipeWire (parec)"


def test_disabling_both_sources_is_rejected():
    with pytest.raises(ValueError, match="нечего захватывать"):
        LiveTranscriber(mic_device="none", system_audio="none")


def test_meta_reports_screencapture_only_for_screencapture(monkeypatch):
    _pretend_linux(monkeypatch)
    transcriber = LiveTranscriber(mic_device="none", system_audio="pulse")

    storage = Mock()
    transcriber._meeting_storage = None
    with pytest.MonkeyPatch.context() as mp:
        mp.setattr("recog.meeting_storage.MeetingStorage", Mock(return_value=storage))
        storage.start_session.return_value = "meeting-1"
        LiveTranscriber._setup_meeting_storage(transcriber)

    kwargs = storage.start_session.call_args.kwargs
    assert kwargs["screencapture"] is False
    assert kwargs["system_audio_backend"] == "pulse"


# =============================================================================
# Маркер потери системного захвата
# =============================================================================


def _transcriber_with_dead_capture(dead: bool, meeting: str = "meeting-A"):
    transcriber = Mock(spec=LiveTranscriber)
    transcriber.ktalk_only = False
    transcriber.system_capture = Mock(dead=dead)
    transcriber._system_capture_lost = False
    transcriber._system_capture_lost_meeting = None
    transcriber._state_lock = threading.RLock()
    transcriber._meeting_generation = 7
    transcriber._meeting_storage = Mock()
    transcriber._meeting_storage.current_meeting_path = Path("/meetings") / meeting
    transcriber._refine_worker = Mock()
    transcriber.transcript_lines = []
    transcriber._current_meeting_key = types.MethodType(
        LiveTranscriber._current_meeting_key, transcriber)
    transcriber._emit_capture_loss_marker = types.MethodType(
        LiveTranscriber._emit_capture_loss_marker, transcriber)
    return transcriber


def _switch_meeting(transcriber, meeting: str):
    """Смена встречи так, как её видит watchdog: новый current_meeting_path."""
    transcriber._meeting_storage.current_meeting_path = Path("/meetings") / meeting


def test_exhausted_capture_restarts_write_one_capture_loss_marker():
    transcriber = _transcriber_with_dead_capture(dead=True)

    LiveTranscriber._check_system_capture_alive(transcriber)
    LiveTranscriber._check_system_capture_alive(transcriber)

    assert transcriber._meeting_storage.append_segment.call_count == 1
    kwargs = transcriber._meeting_storage.append_segment.call_args.kwargs
    assert kwargs["uncertain_reason"] == "system_audio_capture_lost"
    assert "mixed" not in kwargs["text"]


def test_recovered_capture_writes_one_recovery_marker():
    """given: захват потерян, маркер потери записан
    when: редкая попытка подняла захват, данные пошли
    then: один маркер восстановления — без него транскрипт после ночи сна
          так и заканчивался «потерян», хотя звук давно вернулся"""
    transcriber = _transcriber_with_dead_capture(dead=True)

    LiveTranscriber._check_system_capture_alive(transcriber)
    transcriber.system_capture.dead = False
    LiveTranscriber._check_system_capture_alive(transcriber)
    LiveTranscriber._check_system_capture_alive(transcriber)

    calls = transcriber._meeting_storage.append_segment.call_args_list
    assert [c.kwargs["uncertain_reason"] for c in calls] == [
        "system_audio_capture_lost",
        "system_audio_capture_recovered",
    ]
    assert transcriber._system_capture_lost is False


def test_capture_lost_again_after_recovery_writes_a_new_loss_marker():
    transcriber = _transcriber_with_dead_capture(dead=True)

    LiveTranscriber._check_system_capture_alive(transcriber)
    transcriber.system_capture.dead = False
    LiveTranscriber._check_system_capture_alive(transcriber)
    transcriber.system_capture.dead = True
    LiveTranscriber._check_system_capture_alive(transcriber)

    calls = transcriber._meeting_storage.append_segment.call_args_list
    assert [c.kwargs["uncertain_reason"] for c in calls] == [
        "system_audio_capture_lost",
        "system_audio_capture_recovered",
        "system_audio_capture_lost",
    ]


def test_capture_loss_does_not_pause_refine_or_claim_a_pcm_transition():
    transcriber = _transcriber_with_dead_capture(dead=True)

    LiveTranscriber._check_system_capture_alive(transcriber)

    transcriber._refine_worker.pause.assert_not_called()
    transcriber._refine_worker.resume.assert_not_called()
    transcriber._emit_pcm_transition_marker.assert_not_called()


def test_healthy_capture_writes_nothing():
    transcriber = _transcriber_with_dead_capture(dead=False)

    LiveTranscriber._check_system_capture_alive(transcriber)

    transcriber._meeting_storage.append_segment.assert_not_called()
    assert transcriber._system_capture_lost is False


def test_reconnect_into_the_same_meeting_does_not_repeat_the_marker():
    """
    given: захват сдался во встрече A, маркер записан
    when:  расширение переподключается в ту же встречу — bridge зовёт
           pre-switch callback, но current_meeting_path не меняется
    then:  второго маркера в той же встрече нет. Пока состояние сбрасывалось
           по факту `_invalidate_pending_chunks`, каждый reconnect добавлял
           в транскрипт ещё одну копию.
    """
    transcriber = _transcriber_with_dead_capture(dead=True, meeting="meeting-A")
    transcriber._recent_system_segments = deque()
    transcriber._mic_state = None
    transcriber._sys_state = None
    transcriber._pcm_prev_words = {}
    transcriber._pcm_pid_state = {}
    transcriber._pcm_live_segments = {}
    transcriber._pcm_segment_texts = {}
    transcriber._last_pcm_degraded = False
    transcriber._per_speaker_ingest = None
    transcriber.mic_capture = None

    LiveTranscriber._check_system_capture_alive(transcriber)
    # Reconnect: bridge зовёт pre-switch callback, но on_meeting_join
    # оставляет current_meeting_path прежним (ktalk_bridge.py).
    LiveTranscriber._invalidate_pending_chunks(transcriber)
    LiveTranscriber._check_system_capture_alive(transcriber)

    assert transcriber._meeting_storage.append_segment.call_count == 1


def test_bridge_switch_to_a_new_meeting_writes_its_own_marker():
    """
    given: захват сдался во встрече A, маркер записан
    when:  bridge переводит live на встречу B
    then:  встреча B получает собственный маркер — иначе у неё пустой
           audio_system.wav при meta с system_audio: true и ни следа в
           транскрипте
    """
    transcriber = _transcriber_with_dead_capture(dead=True, meeting="meeting-A")

    LiveTranscriber._check_system_capture_alive(transcriber)
    _switch_meeting(transcriber, "meeting-B")
    LiveTranscriber._check_system_capture_alive(transcriber)

    assert transcriber._meeting_storage.append_segment.call_count == 2


def test_silence_split_meeting_gets_its_own_marker():
    """
    given: захват сдался во встрече A, маркер записан
    when:  orphan-сплит по тишине заводит встречу B — этот путь идёт через
           `_on_meeting_change` мимо `_invalidate_pending_chunks`
    then:  встреча B тоже получает маркер. Пока состояние сбрасывалось по
           факту инвалидации, сплит-встречи молча оставались без него.
    """
    transcriber = _transcriber_with_dead_capture(dead=True, meeting="meeting-A")

    LiveTranscriber._check_system_capture_alive(transcriber)
    _switch_meeting(transcriber, "meeting-A-2")  # split_on_silence_timeout
    LiveTranscriber._check_system_capture_alive(transcriber)

    assert transcriber._meeting_storage.append_segment.call_count == 2


def test_marker_is_dropped_when_the_meeting_changed_under_it():
    transcriber = _transcriber_with_dead_capture(dead=True)

    def bump_and_emit(marker_gen):
        transcriber._meeting_generation = marker_gen + 1
        return LiveTranscriber._emit_capture_loss_marker(transcriber, marker_gen)

    transcriber._emit_capture_loss_marker = bump_and_emit

    LiveTranscriber._check_system_capture_alive(transcriber)

    transcriber._meeting_storage.append_segment.assert_not_called()


def test_the_lost_flag_is_set_under_the_same_lock_as_the_generation_read():
    """given: смена встречи (`_invalidate_pending_chunks`) держит _state_lock
              и сбрасывает флаг потери захвата
    when: watchdog в это время обнаруживает мёртвый захват
    then: флаг ещё не взведён — взвод вне лока попадал в окно «взвели →
          встреча сменилась и сбросила флаг → читаем уже новое поколение», и
          новая встреча получала два одинаковых маркера подряд
    """
    transcriber = _transcriber_with_dead_capture(dead=True)
    started = threading.Event()

    def watchdog():
        started.set()
        LiveTranscriber._check_system_capture_alive(transcriber)

    worker = threading.Thread(target=watchdog)
    with transcriber._state_lock:
        worker.start()
        started.wait(timeout=2.0)
        time.sleep(0.1)
        assert transcriber._system_capture_lost is False
        transcriber._meeting_storage.append_segment.assert_not_called()

    worker.join(timeout=2.0)
    assert transcriber._system_capture_lost is True
    assert transcriber._meeting_storage.append_segment.call_count == 1


def test_capture_without_a_dead_flag_is_never_reported():
    """SingleDeviceCapture не имеет `dead` — getattr не должен падать."""
    transcriber = _transcriber_with_dead_capture(dead=False)
    transcriber.system_capture = object()

    LiveTranscriber._check_system_capture_alive(transcriber)

    transcriber._meeting_storage.append_segment.assert_not_called()


# =============================================================================
# live_pipeline: склейка нормализации спеки и автоопределения устройств
# =============================================================================


@pytest.fixture
def captured_transcriber(monkeypatch):
    """Перехватывает конструктор LiveTranscriber, отдаёт его kwargs."""
    from recog import live as live_mod

    seen = {}

    class _FakeTranscriber:
        def __init__(self, **kwargs):
            seen.update(kwargs)

        def run(self, force_exit=False):
            seen["ran"] = True

    monkeypatch.setattr(live_mod, "LiveTranscriber", _FakeTranscriber)
    return seen


def test_live_pipeline_resolves_auto_mic_to_a_device_index(fake_sd, captured_transcriber):
    """`--mic-device auto` — единственный путь, где индекс микрофона вообще
    появляется: LiveTranscriber строку "auto" уже не понимает."""
    from recog.live import live_pipeline

    fake_sd(_devices(("pulse", 32)))

    live_pipeline(mic_device="auto", system_audio=NO_SYSTEM_AUDIO)

    assert captured_transcriber["mic_device"] == 0
    assert captured_transcriber["ran"] is True


def test_live_pipeline_expands_device_auto_to_the_blackhole_index(
    fake_sd, captured_transcriber
):
    from recog.live import live_pipeline

    fake_sd(_devices(("MacBook Pro Microphone", 1), ("BlackHole 2ch", 2)))

    live_pipeline(mic_device="none", system_audio="device:auto")

    assert captured_transcriber["system_audio"] == "device:1"


def test_live_pipeline_downgrades_device_auto_to_none_without_blackhole(
    fake_sd, captured_transcriber, capsys
):
    """Без BlackHole `device:auto` некуда разворачивать: захват молча писал бы
    вход по умолчанию, то есть второй микрофон."""
    from recog.live import live_pipeline

    fake_sd(_devices(("MacBook Pro Microphone", 1)))

    live_pipeline(mic_device="none", system_audio="device:auto")

    assert captured_transcriber["system_audio"] == NO_SYSTEM_AUDIO
    assert "BlackHole" in capsys.readouterr().err


def test_live_pipeline_normalizes_the_legacy_kwargs(captured_transcriber):
    """Прямые вызовы (не через CLI) всё ещё передают старые аргументы."""
    from recog.live import live_pipeline

    live_pipeline(mic_device="none", system_device=5)

    assert captured_transcriber["system_audio"] == "device:5"


def test_live_pipeline_list_devices_builds_no_transcriber(
    fake_sd, captured_transcriber, monkeypatch
):
    from recog.live import live_pipeline

    fake_sd(_devices(("pulse", 32)))
    monkeypatch.setattr(SingleDeviceCapture, "print_pulse_sources",
                        staticmethod(lambda **kw: None))

    live_pipeline(list_devices=True)

    assert captured_transcriber == {}


# =============================================================================
# Перезапуск системного захвата, отдающего одни нули
# =============================================================================


def _transcriber_with_silent_capture(silent_for: float, conference: bool):
    transcriber = Mock(spec=LiveTranscriber)
    transcriber.ktalk_only = False
    transcriber._next_silence_check_at = 0.0
    transcriber.system_capture = Mock()
    transcriber.system_capture.digital_silence_s.return_value = silent_for
    transcriber._ktalk_bridge = Mock()
    transcriber._ktalk_bridge.is_conference_active.return_value = conference
    transcriber.is_recording_active = types.MethodType(
        LiveTranscriber.is_recording_active, transcriber
    )
    return transcriber


def test_minute_of_zeros_during_a_conference_restarts_the_capture():
    transcriber = _transcriber_with_silent_capture(silent_for=61, conference=True)

    LiveTranscriber._restart_silent_system_capture(transcriber)

    transcriber.system_capture.request_restart.assert_called_once()


def test_zeros_outside_a_conference_leave_the_capture_alone():
    """Между встречами системный звук молчит часами — это норма."""
    transcriber = _transcriber_with_silent_capture(silent_for=3600, conference=False)

    LiveTranscriber._restart_silent_system_capture(transcriber)

    transcriber.system_capture.request_restart.assert_not_called()


def test_short_silence_during_a_conference_leaves_the_capture_alone():
    transcriber = _transcriber_with_silent_capture(silent_for=30, conference=True)

    LiveTranscriber._restart_silent_system_capture(transcriber)

    transcriber.system_capture.request_restart.assert_not_called()
    transcriber._ktalk_bridge.is_conference_active.assert_not_called()


def test_silence_check_is_throttled_between_loop_iterations():
    """Проверка конференции читает meta.json — не на каждой итерации в 100 мс."""
    transcriber = _transcriber_with_silent_capture(silent_for=3600, conference=False)

    LiveTranscriber._restart_silent_system_capture(transcriber)
    LiveTranscriber._restart_silent_system_capture(transcriber)

    assert transcriber._ktalk_bridge.is_conference_active.call_count == 1


def test_capture_without_a_silence_clock_is_left_alone():
    """sounddevice-захват (`device:<id>`) не умеет ни считать тишину, ни перезапускаться."""
    transcriber = _transcriber_with_silent_capture(silent_for=0, conference=True)
    transcriber.system_capture = Mock(spec=SingleDeviceCapture)

    LiveTranscriber._restart_silent_system_capture(transcriber)

    transcriber._ktalk_bridge.is_conference_active.assert_not_called()
