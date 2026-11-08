"""The e2e suite's glue to the Rust live process (tests/e2e/live_process.py),
without a conference: which binary it runs and how, how it watches and stops
it, and how it reads the transcript back from the meeting storage — the last
against meetings the real binary wrote, by replaying a synthetic golden
session (rust/recog-live/tests/parity/golden_sessions.py) when it is built.
"""

import json
import os
import signal
import subprocess
import sys
import textwrap
import time
from pathlib import Path

import pytest

from tests.e2e.live_process import (
    LiveProcess,
    find_binary,
    live_argv,
    live_env,
    pcm_ever_seen,
    read_transcript,
    transcript_line,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
GOLDEN_SESSIONS = REPO_ROOT / "rust/recog-live/tests/parity/golden_sessions.py"


def _executable(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n")
    path.chmod(0o755)
    return path


def _built_binary():
    """The real `recog`: the checkout's, else a cargo build of it (CARGO_TARGET_DIR
    or rust/target, release or debug — `cargo test -p recog-live` builds debug)."""
    found = find_binary(REPO_ROOT)
    if found is not None:
        return found
    targets = [Path(os.environ["CARGO_TARGET_DIR"])] if os.environ.get("CARGO_TARGET_DIR") else []
    targets.append(REPO_ROOT / "rust/target")
    for target in targets:
        for profile in ("release", "debug"):
            candidate = target / profile / "recog"
            if candidate.is_file() and os.access(candidate, os.X_OK):
                return candidate
    return None


def _meeting(data_dir: Path, name: str, created_at: str, rows: list[dict]) -> Path:
    path = data_dir / "meetings" / name
    path.mkdir(parents=True)
    (path / "meta.json").write_text(json.dumps({"id": name, "created_at": created_at}))
    (path / "transcript.jsonl").write_text(
        "".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows)
    )
    return path


def _row(speaker, text, ts="12:00:00", speaker_id=None, **extra):
    return {
        "id": 1,
        "ts": "",
        "ts_display": ts,
        "speaker": speaker,
        "speaker_id": speaker_id or speaker,
        "text": text,
        "is_local": False,
        "cont": False,
        "uncertain": False,
        "uncertain_reason": "",
        **extra,
    }


# ---- the binary and its command line ----


def test_installed_and_built_binary_when_finding_the_binary_then_installed_one_wins(tmp_path):
    built = _executable(tmp_path / "rust/target/release/recog")
    assert find_binary(tmp_path) == built
    installed = _executable(tmp_path / "bin/recog")
    assert find_binary(tmp_path) == installed


def test_no_executable_binary_when_finding_the_binary_then_none(tmp_path):
    (tmp_path / "bin").mkdir()
    (tmp_path / "bin/recog").write_text("not executable")
    assert find_binary(tmp_path) is None


def test_a_binary_when_building_the_command_then_live_with_ktalk_and_blackhole_by_name(tmp_path):
    assert live_argv(tmp_path / "recog") == [
        str(tmp_path / "recog"),
        "live",
        "--ktalk",
        "--system-audio",
        "device:auto",
    ]


def test_the_built_binary_when_asking_live_for_help_then_every_flag_of_the_command_exists():
    binary = _built_binary()
    if binary is None:
        pytest.skip("recog is not built")
    result = subprocess.run(
        [str(binary), "live", "--help"],
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 0, result.stderr
    for flag in live_argv(binary)[2:]:
        if flag.startswith("--"):
            assert flag in result.stdout
    assert "device:auto" in result.stdout


def test_a_caller_environment_when_building_the_live_environment_then_own_data_dir_and_caller_untouched(
    tmp_path,
):
    base = {"PATH": "/usr/bin", "RECOG_DATA_DIR": "/somewhere/else"}
    env = live_env(tmp_path / "data", base)
    assert env == {"PATH": "/usr/bin", "RECOG_DATA_DIR": str(tmp_path / "data")}
    assert base["RECOG_DATA_DIR"] == "/somewhere/else"


# ---- the bridge's debug file ----


def _debug(path: Path, timestamp: float, pcm: dict) -> Path:
    path.write_text(json.dumps({"timestamp": timestamp, "pcm": pcm}))
    return path


def test_a_fresh_debug_file_with_pcm_when_asking_for_pcm_then_seen(tmp_path):
    debug = _debug(tmp_path / "debug.json", 2000.0, {"pcm_ever_seen": True})
    assert pcm_ever_seen(debug, since=1000.0)


def test_a_debug_file_older_than_the_process_when_asking_for_pcm_then_not_seen(tmp_path):
    debug = _debug(tmp_path / "debug.json", 500.0, {"pcm_ever_seen": True})
    assert not pcm_ever_seen(debug, since=1000.0)


def test_a_debug_file_without_pcm_or_none_at_all_when_asking_for_pcm_then_not_seen(tmp_path):
    assert not pcm_ever_seen(_debug(tmp_path / "a.json", 2000.0, {"pcm_ever_seen": False}), 1000.0)
    assert not pcm_ever_seen(_debug(tmp_path / "b.json", 2000.0, {}), 1000.0)
    (tmp_path / "c.json").write_text("{not json")
    assert not pcm_ever_seen(tmp_path / "c.json", 1000.0)
    assert not pcm_ever_seen(tmp_path / "missing.json", 1000.0)


# ---- the transcript from the meeting storage ----


def test_a_speech_row_and_a_marker_row_when_rendering_then_the_lines_the_process_prints():
    assert transcript_line(_row("Alice", "привет", ts="10:00:01")) == (
        "\n**Alice** *[10:00:01]*: привет\n"
    )
    marker = _row(
        "Система",
        "[PCM silence >30s — switched to mixed fallback]",
        ts="10:00:40",
        speaker_id="__system__",
        uncertain_reason="pcm_watchdog_transition",
    )
    assert transcript_line(marker) == (
        "\n*[PCM silence >30s — switched to mixed fallback]* *[10:00:40]*\n"
    )


def test_two_meetings_and_an_empty_one_when_reading_the_transcript_then_rows_in_order_and_last_meeting_with_rows(
    tmp_path,
):
    data = tmp_path / "data"
    _meeting(data, "b-orphan", "2026-11-08T10:00:00+03:00", [_row("Me", "раз")])
    conference = _meeting(
        data,
        "a-ktalk",
        "2026-11-08T10:05:00+03:00",
        [_row("Alice", "два"), _row("Bob", "три")],
    )
    _meeting(data, "c-after-leave", "2026-11-08T10:30:00+03:00", [])
    (data / "meetings/index.json").write_text("{}")

    lines, meeting = read_transcript(data)

    assert [line.split("*")[2] for line in lines] == ["Me", "Alice", "Bob"]
    assert meeting == conference


def test_a_data_dir_without_meetings_when_reading_the_transcript_then_nothing(tmp_path):
    assert read_transcript(tmp_path) == ([], None)


# ---- the child process ----


def _stub(tmp_path: Path, body: str) -> list[str]:
    script = tmp_path / "stub.py"
    script.write_text(textwrap.dedent(body))
    return [sys.executable, "-u", str(script)]


def test_a_process_that_prints_the_ready_line_when_starting_and_interrupting_then_ready_and_clean_exit(
    tmp_path,
):
    argv = _stub(
        tmp_path,
        """
        import signal, sys, time
        stop = []
        signal.signal(signal.SIGINT, lambda *_: stop.append(1))
        print("loading the model")
        print("KTalk bridge started, waiting for Chrome extension...")
        while not stop:
            time.sleep(0.05)
        print("Meeting saved: x")
    """,
    )
    live = LiveProcess(argv, os.environ, tmp_path / "logs/live.log")

    assert live.start(ready_timeout=30) is None
    assert live.stop(timeout=30) is None
    assert "Meeting saved: x" in (tmp_path / "logs/live.log").read_text()
    assert list(live.output_tail)[-1] == "Meeting saved: x"


def test_a_process_that_exits_before_ready_when_starting_then_the_exit_code_is_the_error(tmp_path):
    argv = _stub(
        tmp_path,
        """
        import sys
        print("no device")
        sys.exit(3)
    """,
    )
    live = LiveProcess(argv, os.environ, tmp_path / "live.log")

    error = live.start(ready_timeout=30)

    assert error is not None and "code 3" in error
    assert live.stop(timeout=5) == "live process exited with code 3"


def test_a_process_that_ignores_sigint_when_stopping_then_killed_and_reported(tmp_path):
    argv = _stub(
        tmp_path,
        """
        import signal, time
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        print("KTalk bridge started")
        while True:
            time.sleep(0.05)
    """,
    )
    live = LiveProcess(argv, os.environ, tmp_path / "live.log")
    assert live.start(ready_timeout=30) is None

    started = time.time()
    error = live.stop(timeout=1)

    assert error is not None and "killed" in error
    assert time.time() - started < 15


def test_a_process_that_never_gets_ready_when_starting_then_times_out(tmp_path):
    argv = _stub(
        tmp_path,
        """
        import time
        time.sleep(30)
    """,
    )
    live = LiveProcess(argv, os.environ, tmp_path / "live.log")

    error = live.start(ready_timeout=0.5)

    assert error is not None and "not ready" in error
    live._proc.send_signal(signal.SIGKILL)
    live._proc.wait()


# ---- against the real binary ----


def _sandbox_env(sandbox: Path) -> dict:
    (sandbox / "home").mkdir(parents=True, exist_ok=True)
    return {
        "PATH": "/usr/bin:/bin",
        "HOME": str(sandbox / "home"),
        "RECOG_CONFIG_FILE": str(sandbox / "no-config.toml"),
        "RECOG_CONFERENCE_HOSTS_FILE": str(sandbox / "no-hosts.json"),
        "RECOG_REPO_DIR": str(REPO_ROOT),
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONUTF8": "1",
        "TZ": "UTC",
        "MIC_SPEAKER_NAME": "Me",
    }


def _run(argv: list[str], env: dict) -> subprocess.CompletedProcess:
    result = subprocess.run(
        argv,
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert result.returncode == 0, f"{argv[:3]} failed:\n{result.stdout}\n{result.stderr}"
    return result


def test_meetings_the_rust_binary_stored_in_a_replayed_call_when_reading_the_transcript_then_both_remote_speakers_attributed(
    tmp_path,
):
    binary = _built_binary()
    if binary is None:
        pytest.skip("recog is not built")
    env = _sandbox_env(tmp_path)
    session, filled, out = tmp_path / "session", tmp_path / "py", tmp_path / "rust"
    golden = [sys.executable, str(GOLDEN_SESSIONS), str(REPO_ROOT)]
    # A synthetic KTalk call (Alice and Bob on per-speaker PCM, the user on the
    # microphone), its ASR answers filled in by the Python replay.
    _run([*golden, "make", "ktalk_pcm", str(session)], env)
    _run([*golden, "fill", str(session), str(filled), "3"], env)

    _run([str(binary), "replay", str(session), str(out), "--tail", "3"], env)
    summary = json.loads((out / "replay.json").read_text())
    lines, meeting = read_transcript(out / "data")

    assert summary["asr_misses"] == 0
    assert meeting is not None
    assert json.loads((meeting / "meta.json").read_text())["source"] == "ktalk"
    speech = [line for line in lines if line.startswith("\n**")]
    # Markers of the "Система" speaker, such as the pause at the meeting's leave.
    markers = [line for line in lines if not line.startswith("\n**")]
    assert speech
    for line in speech:
        speaker, _, rest = line.strip()[2:].partition("** *[")
        assert speaker and rest[2] == ":" and rest[5] == ":" and rest[8:11] == "]*:"
    for line in markers:
        text, _, rest = line.strip()[1:].partition("* *[")
        assert text and rest[2] == ":" and rest[5] == ":" and rest[8:] == "]*"
    speakers = {line.strip()[2:].partition("**")[0] for line in speech}
    assert {"Alice", "Bob"} <= speakers
    # The synthetic model capitalises refine words: the stored rows are the refined ones.
    assert any("Привет Коллеги" in line for line in speech)


# ---- the suite without the recog package ----


COLLECT_WITHOUT_RECOG = """
import importlib.abc, sys, types

class NoRecog(importlib.abc.MetaPathFinder):
    def find_spec(self, name, path=None, target=None):
        if name == "recog" or name.startswith("recog."):
            raise ModuleNotFoundError(f"the e2e suite imported {name}")
        return None

sys.meta_path.insert(0, NoRecog())
# A stand-in playwright, so the orchestrator is imported whether or not the
# real one is installed here.
playwright = types.ModuleType("playwright")
sync_api = types.ModuleType("playwright.sync_api")
for name in ("Browser", "BrowserContext", "Page", "Playwright", "sync_playwright"):
    setattr(sync_api, name, object)
playwright.sync_api = sync_api
sys.modules.update({"playwright": playwright, "playwright.sync_api": sync_api})

import pytest
code = pytest.main(["--collect-only", "-q", "-m", "e2e", "-p", "no:cacheprovider",
                    "-p", "no:playwright", "tests/e2e"])
imported = sorted(m for m in sys.modules if m == "recog" or m.startswith("recog."))
print("ORCHESTRATOR", "tests.e2e.ktalk_orchestrator" in sys.modules)
print("RECOG", imported)
sys.exit(int(code))
"""


def test_a_python_without_recog_when_collecting_the_e2e_suite_then_both_tests_collected_and_recog_never_imported():
    result = subprocess.run(
        [sys.executable, "-c", COLLECT_WITHOUT_RECOG],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert "test_per_speaker_overlap_attribution" in result.stdout
    assert "test_per_speaker_degraded_mode_on_extension_dispose" in result.stdout
    assert "ORCHESTRATOR True" in result.stdout
    assert "RECOG []" in result.stdout
