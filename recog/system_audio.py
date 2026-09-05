#!/usr/bin/env python3
"""Захват системного звука: ScreenCaptureKit (macOS) и PulseAudio (Linux).

Оба бэкенда — один и тот же `SubprocessPCMCapture`: дочерний процесс пишет
сырой моно float32 PCM в stdout, reader-поток складывает его в общий буфер
`BufferedAudioCapture`.

Usage:
    from recog.system_audio import create_system_capture, resolve_system_audio_backend

    backend = resolve_system_audio_backend("auto")
    capture = create_system_capture(backend, 16000)
    capture.start()
    chunk = capture.get_chunk(duration=3.0)
    capture.stop()

Демо: `python -m recog.system_audio`.
"""

import os
import select
import shutil
import subprocess
import sys
import threading
import time
from collections import deque
from pathlib import Path
from typing import Callable, List, Optional

import numpy as np

from recog.audio_capture import BufferedAudioCapture
from recog.config import ASR_PENDING_BUFFER_CAP_S
from recog.log_utils import log_rate_limited

# The Swift helper and its source live at the repo root, not inside the package.
REPO_ROOT = Path(__file__).resolve().parent.parent

SAMPLE_RATE = 16000
BYTES_PER_SAMPLE = 4  # float32

# Backoff между попытками respawn, сек; последнее значение повторяется.
RESTART_BACKOFF_S = (1.0, 2.0, 5.0, 10.0)
MAX_RESTARTS = 5

# Имя резолвится в текущий монитор при создании потока, но за сменой
# default sink само по себе не следует — за этим следит PulseAudioCapture.
PULSE_DEFAULT_MONITOR = "@DEFAULT_MONITOR@"
PULSE_POLL_INTERVAL_S = 2.0
PULSE_CMD_TIMEOUT_S = 2.0

# Пауза, когда read() не дал байт (EOF на stdout ещё не пожатого процесса) или
# бросил: без неё reader-поток крутится вхолостую на 100% CPU.
READ_IDLE_SLEEP_S = 0.05
# Сколько ждать данные, прежде чем вернуться к вершине reader-loop. Данные
# приходят — select просыпается сразу, так что на обычном потоке это ничего
# не стоит; ограничение нужно молчащему источнику (см. `_wait_readable`).
READ_WAIT_TIMEOUT_S = 1.0

# Сколько последних строк stderr ребёнка показывать в диагностике.
STDERR_REPORT_LINES = 5


class MissingSwiftc(RuntimeError):
    """Хелпер надо пересобрать, но swiftc в PATH нет."""


def find_or_build_sck_utility(repo_root: Path = REPO_ROOT) -> str:
    """Путь до sck_audio_capture; компилирует хелпер, если его нет/он устарел."""
    utility = repo_root / "sck_audio_capture"
    source = repo_root / "native/macos/sck_audio_capture.swift"

    def compile_utility(prebuilt: bool = False):
        if not shutil.which("swiftc"):
            # Без swiftc компилировать нечем. Раньше subprocess.run кидал
            # FileNotFoundError('swiftc') — а «macOS без swiftc, но с уже
            # собранным хелпером» setup.sh считает штатным состоянием
            # (swiftc там optional, deps_sck_helper_ok пропускает гейт,
            # сервисы активируются). Бинарь перестал лежать в индексе, а
            # исходник остался — значит любой будущий pull, тронувший
            # sck_audio_capture.swift, делает его новее бинаря и раньше
            # уводил live-сервис в крэш-луп на ровном месте. Отдаём то, что
            # есть: устаревший хелпер лучше, чем никакого.
            detail = "" if prebuilt else (
                f", and no prebuilt helper to fall back on at {utility}"
            )
            raise MissingSwiftc(
                "swiftc not found — cannot build sck_audio_capture "
                f"(xcode-select --install){detail}"
            )

        print(f"[screencapture] Compiling {source}...", file=sys.stderr)
        result = subprocess.run([
            "swiftc", "-o", str(utility), str(source),
            "-framework", "ScreenCaptureKit",
            "-framework", "CoreMedia",
            "-framework", "AVFoundation"
        ], capture_output=True, text=True)

        if result.returncode == 0 and utility.exists():
            return str(utility)

        raise RuntimeError(f"Failed to compile utility: {result.stderr}")

    if utility.exists():
        # Rebuild if source is newer (repo may ship an old binary)
        if source.exists():
            try:
                stale = source.stat().st_mtime > utility.stat().st_mtime
            except OSError:
                # If stat fails for any reason, try compiling
                stale = True
            if stale:
                try:
                    return compile_utility(prebuilt=True)
                except Exception as e:
                    # Не только MissingSwiftc: swiftc может быть в PATH и при
                    # этом падать (SDK от снесённого Xcode, xcode-select на
                    # старый toolchain, недоступный фреймворк) — тогда летел
                    # RuntimeError и уводил live-сервис в тот же крэш-луп,
                    # ради которого и заводилась эта ветка. Выход swiftc при
                    # ошибке не перезаписывает, так что рабочий бинарь на
                    # месте: переиспользуем его.
                    log_rate_limited(
                        "sck-helper-stale",
                        f"[screencapture] {e}; reusing the existing helper",
                    )
        return str(utility)

    # Compile from source if binary is missing
    if source.exists():
        return compile_utility()

    raise FileNotFoundError(
        f"sck_audio_capture utility not found. "
        f"Expected at: {utility}"
    )


def parec_argv(source: str, sample_rate: int = SAMPLE_RATE) -> List[str]:
    """argv для parec: сырой моно float32 с монитора `source`.

    Без --device parec пишет дефолтный *source* (микрофон), поэтому монитор
    обязателен."""
    return [
        "parec",
        "--raw",
        "--format=float32le",
        f"--rate={sample_rate}",
        "--channels=1",
        "--latency-msec=100",
        f"--device={source}",
    ]


def resolve_pulse_monitor_source(env=os.environ) -> str:
    """Начальный источник parec: override из env либо @DEFAULT_MONITOR@."""
    override = (env.get("RECOG_PULSE_MONITOR_SOURCE") or "").strip()
    return override or PULSE_DEFAULT_MONITOR


class SubprocessPCMCapture(BufferedAudioCapture):
    """Захват PCM из stdout дочернего процесса с ограниченным respawn."""

    def __init__(self, argv: List[str], name: str,
                 sample_rate: int = SAMPLE_RATE,
                 max_buffer_s: float = ASR_PENDING_BUFFER_CAP_S):
        super().__init__(sample_rate=sample_rate, name=name,
                         max_buffer_s=max_buffer_s)
        self.argv = list(argv)
        self.process = None
        self._reader_thread = None
        self._stderr_thread = None
        self._stderr_lock = threading.Lock()
        self._stderr_tail = deque(maxlen=200)
        # Неполный кадр float32 с прошлого чтения (см. _decode_pcm).
        self._pcm_tail = b""
        # Захват сдался: попытки respawn исчерпаны. Потребитель (live.py)
        # видит это и один раз отмечает потерю системного звука.
        self._dead = False

    @property
    def dead(self) -> bool:
        return self._dead

    # -- планово запрошенная смена источника (реализует PulseAudioCapture) ---

    def _take_pending_source(self) -> Optional[str]:
        """Возвращает запрошенный источник один раз (None — смены нет)."""
        return None

    def _apply_source_switch(self, source: str) -> None:
        raise NotImplementedError

    # ----------------------------------------------------------------------

    def _sleep_while_running(self, delay: float) -> None:
        """Спит delay секунд, просыпаясь на stop()."""
        deadline = time.monotonic() + delay
        while self.running:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            time.sleep(min(0.2, remaining))

    def _stderr_suffix(self) -> str:
        """Последние строки stderr ребёнка, пригодные для дописывания в лог.

        Причина смерти (`parec: Connection failure`, отказ в Screen Recording)
        собиралась drain-потоком и никуда не попадала: единственный
        необратимый отказ — `_give_up` — печатался без единого слова о том,
        что произошло.
        """
        with self._stderr_lock:
            lines = list(self._stderr_tail)[-STDERR_REPORT_LINES:]
        tail = " | ".join(line.strip() for line in lines if line.strip())
        return f" (stderr: {tail})" if tail else ""

    def _give_up(self, reason: str) -> None:
        """Явная сдача: потребитель должен увидеть потерю захвата."""
        self._dead = True
        self.running = False
        print(f"[{self.name}] {reason}{self._stderr_suffix()}", file=sys.stderr)

    def _wait_readable(self, timeout: float) -> bool:
        """True — в stdout есть что читать, False — истёк timeout.

        `read()` на молчащем источнике блокируется без срока: sink,
        принудительно переведённый в suspend (`pactl suspend-sink`) или
        вставший на уровне драйвера, не отдаёт ни байта, а ребёнок при этом
        жив — ни respawn, ни `_give_up` не срабатывают. Запрошенная смена
        источника читается только на вершине цикла, поэтому без этого
        ожидания переключение на новый sink не доезжало никогда, и удалённая
        сторона встречи пропадала молча до перезапуска процесса.
        """
        stdout = self.process.stdout if self.process else None
        if stdout is None:
            return False
        try:
            fd = stdout.fileno()
        except Exception:
            # Поток без файлового дескриптора (тестовый двойник) — ждать
            # нечем, читаем как раньше.
            return True
        try:
            ready, _, _ = select.select([fd], [], [], timeout)
        except (ValueError, OSError):
            # stdout закрыт из stop()/respawn: решение оставляем проверке
            # живости на вершине цикла, но без паузы она крутилась бы вхолостую.
            time.sleep(READ_IDLE_SLEEP_S)
            return False
        return bool(ready)

    def _decode_pcm(self, data: bytes) -> np.ndarray:
        """float32-сэмплы из сырых байтов, с переносом неполного кадра.

        stdout открыт с `bufsize=0`, то есть это raw `FileIO`: `read(n)` —
        один syscall, который возвращает сколько есть, не обязательно кратно
        4 байтам. `np.frombuffer` на некратной длине бросал ValueError, байты
        молча терялись, и весь дальнейший поток съезжал с границы кадра:
        последующие чтения декодировались «успешно», но в мусор (вплоть до
        NaN, который VAD не отсекает — `NaN < threshold` ложно), и мусор шёл
        и в WAV, и в ASR до самого respawn. Хвост переносим в следующее
        чтение.
        """
        if self._pcm_tail:
            data = self._pcm_tail + data
            self._pcm_tail = b""
        usable = len(data) - len(data) % BYTES_PER_SAMPLE
        if usable != len(data):
            self._pcm_tail = data[usable:]
        return np.frombuffer(data[:usable], dtype=np.float32)

    def _reader_loop(self):
        """Read audio data from subprocess stdout with auto-restart."""
        bytes_per_chunk = self.sample_rate * BYTES_PER_SAMPLE // 10  # 100ms chunks
        restart_count = 0

        while self.running:
            pending_source = self._take_pending_source()
            if pending_source is not None:
                # Плановое переподключение не расходует бюджет crash-retry.
                try:
                    self._apply_source_switch(pending_source)
                    restart_count = 0
                except Exception as e:
                    log_rate_limited(
                        f"capture-switch-{self.name}",
                        f"[{self.name}] source switch to {pending_source} failed: {e}",
                    )
                continue

            # Check if process is alive
            if not self.process or self.process.poll() is not None:
                if restart_count >= MAX_RESTARTS:
                    self._give_up(
                        f"Max restarts ({MAX_RESTARTS}) reached, giving up on system audio"
                    )
                    return

                delay = RESTART_BACKOFF_S[min(restart_count, len(RESTART_BACKOFF_S) - 1)]
                restart_count += 1
                print(
                    f"[{self.name}] Process died, restarting... "
                    f"(attempt {restart_count}){self._stderr_suffix()}",
                    file=sys.stderr,
                )
                # Пауза перед КАЖДЫМ respawn, а не только перед повтором после
                # неудачного. Ребёнок, который стартует нормально и умирает
                # через полсекунды (источник исчез, сервер перезапускается),
                # проходил через ветку без единого sleep: пять попыток
                # сгорали за ~3 с, `_give_up` срабатывал, и системный звук
                # пропадал до конца жизни процесса — в сервисе это навсегда.
                self._sleep_while_running(delay)
                if not self.running:
                    # stop() пришёл во время паузы: respawn здесь оставил бы
                    # осиротевшего ребёнка, которого stop() уже не увидит.
                    return
                try:
                    self._restart_process()
                except Exception as e:
                    # Транзиентный сбой (parec умирает, пока перезапускается
                    # pipewire-pulse) не должен убивать поток: выход отсюда
                    # оставлял running=True и get_chunk() навсегда None.
                    log_rate_limited(
                        f"capture-restart-{self.name}",
                        f"[{self.name}] Restart failed: {e}",
                    )
                continue

            try:
                if not self._wait_readable(READ_WAIT_TIMEOUT_S):
                    # Источник молчит. Возврат к вершине цикла — единственное
                    # место, где применяется запрошенная смена источника.
                    continue
                data = self.process.stdout.read(bytes_per_chunk)
                if data:
                    samples = self._decode_pcm(data)
                    # Reset restart count on successful read
                    restart_count = 0
                    if samples.size:
                        captured_at = time.time() - len(samples) / self.sample_rate
                        with self._lock:
                            dropped = self._append_locked(samples, captured_at)
                        if dropped:
                            log_rate_limited(
                                f"capture-overflow-{self.name}",
                                f"[{self.name}] ASR unavailable too long — "
                                f"dropped {dropped / self.sample_rate:.1f}s of oldest audio",
                            )
                else:
                    # EOF на stdout, но poll() ещё может вернуть None, пока
                    # процесс не пожат: без паузы цикл крутится вхолостую.
                    time.sleep(READ_IDLE_SLEEP_S)
            except Exception as e:
                if self.running:
                    log_rate_limited(
                        f"capture-read-{self.name}",
                        f"[{self.name}] Reader error: {e}",
                    )
                time.sleep(READ_IDLE_SLEEP_S)
                # Don't break, let the loop try to restart

    def _absorb_dead_child_stderr(self):
        """Забирает stderr уже умершего ребёнка в тот же буфер, что и drain.

        Drain-поток здесь не годится: причина нужна прямо сейчас, а поток
        успел бы прочитать пайп уже после того, как её напечатали.
        """
        if not self.process or self.process.stderr is None:
            return
        try:
            text = self.process.stderr.read().decode(errors="ignore")
        except Exception:
            return
        lines = [line for line in text.splitlines() if line.strip()]
        if lines:
            with self._stderr_lock:
                self._stderr_tail.extend(lines)

    def _start_stderr_drain_thread(self):
        """Drain stderr to prevent the subprocess from blocking on a full stderr pipe."""
        if not self.process or self.process.stderr is None:
            return

        with self._stderr_lock:
            self._stderr_tail.clear()

        def _drain(pipe):
            try:
                for line in iter(pipe.readline, b""):
                    with self._stderr_lock:
                        self._stderr_tail.append(line.decode(errors="ignore"))
            except Exception:
                pass

        self._stderr_thread = threading.Thread(target=_drain, args=(self.process.stderr,), daemon=True)
        self._stderr_thread.start()

    def _spawn(self):
        return subprocess.Popen(
            self.argv,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )

    @staticmethod
    def _reap(proc, timeout: float = 2.0):
        """terminate → wait → kill → wait. `wait` после kill обязателен:
        без него убитый ребёнок остаётся зомби до сборки родителя."""
        try:
            proc.terminate()
            proc.wait(timeout=timeout)
        except Exception:
            try:
                proc.kill()
                proc.wait(timeout=1.0)
            except Exception:
                pass

    def _terminate_process(self):
        if not self.process:
            return
        self._reap(self.process, timeout=1.0)

    def _restart_process(self):
        """Restart the audio capture subprocess."""
        self._terminate_process()

        # Новый ребёнок — новый поток: недочитанный кадр от прежнего склеивать
        # не с чем.
        self._pcm_tail = b""
        self.process = self._spawn()

        # Wait for startup
        time.sleep(0.5)

        if self.process.poll() is not None:
            stderr = self.process.stderr.read().decode()
            raise RuntimeError(f"Audio capture failed to restart: {stderr}")

        self._start_stderr_drain_thread()
        print(f"[{self.name}] Restarted (PID: {self.process.pid})", file=sys.stderr)

    def start(self):
        """Start capturing system audio."""
        if self.running:
            return

        self._pcm_tail = b""
        self.process = self._spawn()

        self.running = True
        self._dead = False
        self.clear_buffer()

        # Start reader thread
        self._reader_thread = threading.Thread(target=self._reader_loop, daemon=True)
        self._reader_thread.start()

        # Wait a bit for startup
        time.sleep(0.5)

        # Ребёнок, умерший на ПЕРВОМ спавне, раньше улетал RuntimeError'ом
        # наружу — мимо всей машинерии backoff/_give_up (она ловит только
        # смерть после успешного старта) и мимо try/finally в
        # `LiveTranscriber.run()`: встреча оставалась незакрытой, а
        # Restart=always перезапускал весь конвейер (с полной загрузкой
        # GigaAM) каждые 5 с. А это ровно тот случай, который на Linux
        # штатно бывает гонкой: юнит стартует раньше, чем поднялся
        # pipewire-pulse, и parec умирает мгновенно. Отдаём отказ тому же
        # _reader_loop, который умеет подождать и, если не вышло, сдаться с
        # маркером — остальной конвейер (микрофон, per-speaker PCM) живёт.
        if self.process.poll() is not None:
            self._absorb_dead_child_stderr()
            print(
                f"[{self.name}] Failed to start, retrying in background"
                f"{self._stderr_suffix()}",
                file=sys.stderr,
            )
            return

        self._start_stderr_drain_thread()
        print(f"[{self.name}] Started (PID: {self.process.pid})", file=sys.stderr)

    def stop(self):
        """Stop capturing.

        Гварда `not self.running` мало: после `_give_up` захват уже не
        running, но дохлый Popen с открытыми stdout/stderr всё ещё висит на
        объекте — ранний return тёк двумя fd на каждую сдачу.
        """
        if not self.running and self.process is None and self._reader_thread is None:
            return

        self.running = False

        proc = self.process
        if proc:
            self._reap(proc)

        if self._reader_thread:
            self._reader_thread.join(timeout=1.0)
            self._reader_thread = None

        # reader мог успеть заспавнить нового ребёнка между своей проверкой
        # `running` и снимком `proc` выше. В снимок он не попал, а
        # `self.process = None` ниже потеряло бы на него последнюю ссылку:
        # осиротевший parec/sck пережил бы os._exit() в _teardown_session и
        # продолжил держать источник звука.
        late = self.process
        if late is proc:
            late = None
        if late is not None:
            self._reap(late)

        # Пайпы закрываем после join: пока reader ещё в read(), закрытие
        # обернулось бы исключением в чужом потоке.
        for dead in (proc, late):
            if dead is None:
                continue
            for pipe in (dead.stdout, dead.stderr):
                try:
                    if pipe is not None:
                        pipe.close()
                except Exception:
                    pass
        self.process = None

        print(f"[{self.name}] Stopped", file=sys.stderr)

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *args):
        self.stop()


class PulseAudioCapture(SubprocessPCMCapture):
    """parec с монитора default sink (PulseAudio/PipeWire).

    Без `RECOG_PULSE_MONITOR_SOURCE` захват сам следит за `pactl
    get-default-sink` и переподключается на новый монитор: `@DEFAULT_MONITOR@`
    резолвится один раз при создании потока и за переключением устройства не
    идёт. С override источник фиксирован, наблюдение выключено.
    """

    def __init__(self, sample_rate: int = SAMPLE_RATE,
                 source: Optional[str] = None,
                 env=os.environ,
                 run: Callable[..., object] = subprocess.run,
                 poll_interval_s: float = PULSE_POLL_INTERVAL_S,
                 max_buffer_s: float = ASR_PENDING_BUFFER_CAP_S):
        resolved = source or resolve_pulse_monitor_source(env)
        super().__init__(parec_argv(resolved, sample_rate=sample_rate),
                         name="pulse", sample_rate=sample_rate,
                         max_buffer_s=max_buffer_s)
        self._run = run
        self._poll_interval_s = poll_interval_s
        # Явный источник (env override или аргумент) отключает автопереключение.
        self._follow_default_sink = source is None and resolved == PULSE_DEFAULT_MONITOR
        self._current_source = resolved
        self._pending_source: Optional[str] = None
        self._last_default_sink: Optional[str] = None
        self._source_lock = threading.Lock()
        self._watch_thread = None

    # -- наблюдение за default sink ----------------------------------------

    def _query_default_sink(self) -> Optional[str]:
        """Имя текущего default sink; None — если pactl не ответил."""
        try:
            result = self._run(
                ["pactl", "get-default-sink"],
                capture_output=True, text=True, timeout=PULSE_CMD_TIMEOUT_S,
            )
        except Exception as e:
            log_rate_limited("pulse-default-sink", f"[pulse] pactl get-default-sink failed: {e}")
            return None

        if getattr(result, "returncode", 1) != 0:
            log_rate_limited(
                "pulse-default-sink",
                f"[pulse] pactl get-default-sink exited {result.returncode}",
            )
            return None

        sink = (getattr(result, "stdout", "") or "").strip()
        if not sink or sink.startswith("@"):
            return None
        return sink

    def _poll_default_sink_once(self) -> None:
        sink = self._query_default_sink()
        if sink is None:
            # Краткая ошибка pactl не обрывает текущий поток.
            return

        if self._last_default_sink is None:
            self._last_default_sink = sink
            if self._current_source == PULSE_DEFAULT_MONITOR:
                # Пиннинг при старте не состоялся (pactl тогда молчал): parec
                # висит на мониторе того sink'а, который был default в момент
                # создания потока, и его имя уже не узнать. Принять текущий за
                # базу нельзя — переключение, случившееся до этого опроса,
                # стало бы «исходным состоянием», расхождения бы не осталось, и
                # захват молча сидел бы на прежнем выходе. Переподключаемся на
                # конкретный монитор: лишний рестарт дешевле немого захвата не
                # того выхода, и дальше работает обычное сравнение имён.
                with self._source_lock:
                    self._pending_source = f"{sink}.monitor"
            # Иначе источник уже пиннился в start() — именно в этот sink.
            return

        if sink == self._last_default_sink:
            return

        self._last_default_sink = sink
        with self._source_lock:
            self._pending_source = f"{sink}.monitor"

    def _watch_loop(self):
        while self.running:
            self._poll_default_sink_once()
            self._sleep_while_running(self._poll_interval_s)

    # -- смена источника ----------------------------------------------------

    def _take_pending_source(self) -> Optional[str]:
        with self._source_lock:
            pending = self._pending_source
            self._pending_source = None
        if pending is None or pending == self._current_source:
            return None
        return pending

    def _apply_source_switch(self, source: str) -> None:
        self.argv = parec_argv(source, sample_rate=self.sample_rate)
        self._current_source = source
        # Граница источников: overlap через разрыв не склеиваем, старому
        # буферу новые timestamps не присваиваем, и разрыв виден потребителю
        # (иначе писатель WAV срежет первые 0.5 с нового источника как
        # «уже записанный» overlap).
        self.mark_source_boundary()
        print(f"[pulse] Default sink changed, reconnecting to {source}", file=sys.stderr)
        self._restart_process()

    def _pin_initial_source(self) -> None:
        """Резолвит `@DEFAULT_MONITOR@` в конкретный монитор до спавна parec.

        Первое наблюдение иначе принимает за исходный тот sink, который стоит
        на момент опроса, а не тот, в который резолвился `@DEFAULT_MONITOR@`
        при создании потока. Sink, переключённый в промежутке между стартом
        parec и первым опросом, становится «базой» — расхождения больше нет,
        переподключения не будет, и захват до следующего переключения молча
        остаётся на прежнем выходе. Пиннинг делает базу наблюдения и
        фактический источник захвата одним и тем же именем; последующая смена
        всё равно даёт конкретное `<sink>.monitor` (см. _apply_source_switch).

        Если pactl недоступен, остаёмся на `@DEFAULT_MONITOR@`; согласование
        берёт на себя первый успешный опрос — он переподключает захват на
        конкретный монитор, а не принимает текущий sink за базу (см.
        _poll_default_sink_once).
        """
        sink = self._query_default_sink()
        if sink is None:
            return
        source = f"{sink}.monitor"
        self._last_default_sink = sink
        self._current_source = source
        self.argv = parec_argv(source, sample_rate=self.sample_rate)

    def start(self):
        if self._follow_default_sink and not self.running:
            self._pin_initial_source()
        super().start()
        if self._follow_default_sink and self._watch_thread is None:
            self._watch_thread = threading.Thread(target=self._watch_loop, daemon=True)
            self._watch_thread.start()

    def stop(self):
        super().stop()
        if self._watch_thread:
            self._watch_thread.join(timeout=PULSE_POLL_INTERVAL_S + 1.0)
            self._watch_thread = None


def resolve_system_audio_backend(spec: Optional[str],
                                 platform: str = sys.platform,
                                 which: Callable[[str], Optional[str]] = shutil.which) -> str:
    """Разрешает `auto` в конкретный бэкенд и валидирует явный выбор."""
    if spec is None or spec == "" or spec == "none":
        return "none"

    if spec.startswith("device:"):
        return spec

    is_darwin = platform == "darwin"
    is_linux = platform.startswith("linux")

    def require_parec():
        if not which("parec"):
            raise RuntimeError(
                "parec not found — install it: "
                "`apt install pulseaudio-utils` (Debian/Ubuntu) or "
                "`pacman -S libpulse` (Arch/Manjaro)"
            )

    if spec == "auto":
        if is_darwin:
            return "screencapture"
        if is_linux:
            require_parec()
            return "pulse"
        raise RuntimeError(f"no system-audio backend for platform {platform!r}")

    if spec == "screencapture":
        if not is_darwin:
            raise RuntimeError(
                f"the screencapture backend needs macOS (ScreenCaptureKit), "
                f"got platform {platform!r} — use --system-audio pulse"
            )
        return "screencapture"

    if spec == "pulse":
        if not is_linux:
            raise RuntimeError(
                f"the pulse backend needs Linux (PulseAudio/PipeWire), "
                f"got platform {platform!r} — use --system-audio screencapture"
            )
        require_parec()
        return "pulse"

    raise ValueError(f"unknown system audio backend: {spec!r}")


def create_system_capture(backend: str, sample_rate: int = SAMPLE_RATE) -> SubprocessPCMCapture:
    """Строит capture по уже разрешённому имени бэкенда."""
    if backend == "screencapture":
        return SubprocessPCMCapture([find_or_build_sck_utility()],
                                    name="screencapture", sample_rate=sample_rate)
    if backend == "pulse":
        return PulseAudioCapture(sample_rate=sample_rate)
    raise ValueError(f"unknown system audio backend: {backend!r}")


def _demo():
    """Ручная проверка захвата: `python -m recog.system_audio`."""
    backend = resolve_system_audio_backend("auto")
    print(f"Testing system audio capture ({backend})...")
    if backend == "screencapture":
        print("Make sure Screen Recording is allowed in System Settings > Privacy & Security")
    print()

    try:
        capture = create_system_capture(backend)
        capture.start()

        print("Capturing... play some audio!")
        time.sleep(4)

        result = capture.get_chunk(3.0)
        if result is not None:
            chunk, _ = result
            print(f"Got chunk: {len(chunk)} samples, {len(chunk)/capture.sample_rate:.2f}s")
            print(f"Audio level (RMS): {np.sqrt(np.mean(chunk**2)):.6f}")
            print(f"Max amplitude: {np.abs(chunk).max():.6f}")
        else:
            print("No audio captured (buffer too small)")

        capture.stop()
        print("Done!")

    except Exception as e:
        print(f"Error: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    _demo()
