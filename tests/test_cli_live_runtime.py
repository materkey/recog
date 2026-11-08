"""`[live] runtime`: `python -m recog live` hands over to the Rust live process
(rust/recog-live's `recog`) when the setting asks for it and the binary can run."""

import sys

import pytest

from recog import cli
from recog.install_config import INSTALL_CONFIG


def _binary(tmp_path):
    binary = tmp_path / "recog"
    binary.write_text("#!/bin/sh\n")
    binary.chmod(0o755)
    return binary


def test_default_runtime_choosing_the_program_keeps_python(tmp_path, capsys):
    """given: [live] runtime left at "python" and a built binary
    when: the program is chosen
    then: none: the live process stays in Python, silently"""
    assert cli.rust_live_program("python", "auto", _binary(tmp_path)) is None
    assert capsys.readouterr().err == ""


@pytest.mark.parametrize("backend", ["auto", "wavo"])
def test_rust_runtime_and_a_built_binary_choosing_the_program_gives_the_binary(tmp_path, backend):
    """given: runtime "rust", the binary built, ASR on wavo (or auto)
    when: the program is chosen
    then: the binary"""
    binary = _binary(tmp_path)
    assert cli.rust_live_program("rust", backend, binary) == binary


def test_rust_runtime_without_the_build_choosing_the_program_falls_back_and_says_so(tmp_path, capsys):
    """given: runtime "rust" but no binary (cargo was missing when setup ran)
    when: the program is chosen
    then: Python, with a line naming the binary and setup.sh"""
    missing = tmp_path / "recog"
    assert cli.rust_live_program("rust", "auto", missing) is None
    err = capsys.readouterr().err
    assert str(missing) in err and "setup.sh" in err and "runs in Python" in err


@pytest.mark.parametrize("backend", ["local", "remote"])
def test_rust_runtime_with_another_asr_backend_choosing_the_program_keeps_python(
    tmp_path, capsys, backend
):
    """given: runtime "rust" and an ASR backend the Rust process does not have
    when: the program is chosen
    then: Python, saying why"""
    assert cli.rust_live_program("rust", backend, _binary(tmp_path)) is None
    assert f"backend = '{backend}'" in capsys.readouterr().err


def test_unknown_runtime_choosing_the_program_stops_naming_the_key(tmp_path):
    with pytest.raises(ValueError, match=r'\[live\] runtime must be "python" or "rust", not \'rsut\''):
        cli.rust_live_program("rsut", "auto", _binary(tmp_path))


def test_rust_runtime_starting_live_execs_the_binary_with_the_same_arguments(
    tmp_path, monkeypatch
):
    """given: runtime "rust" and a built binary
    when: `recog live --system-audio auto --ktalk` starts
    then: the process becomes the binary with those arguments, the interpreter named
          for the Python microphone worker"""
    binary = _binary(tmp_path)
    monkeypatch.setattr(cli, "RUST_LIVE", binary)
    monkeypatch.setattr(INSTALL_CONFIG.live, "runtime", "rust")
    monkeypatch.setattr(INSTALL_CONFIG.asr, "backend", "auto")
    calls = []

    def fake_execve(path, argv, env):
        calls.append((path, argv, env))
        raise SystemExit(0)

    monkeypatch.setattr(cli.os, "execve", fake_execve)
    monkeypatch.setattr(sys, "argv", ["recog", "live", "--system-audio", "auto", "--ktalk"])

    with pytest.raises(SystemExit):
        cli.main()

    path, argv, env = calls[0]
    assert path == binary
    assert argv == [str(binary), "live", "--system-audio", "auto", "--ktalk"]
    assert env["RECOG_LIVE_PYTHON"] == sys.executable


def test_python_runtime_starting_live_runs_the_python_pipeline(monkeypatch):
    """given: the default runtime
    when: `recog live --list-devices` starts
    then: no exec; the Python pipeline runs"""
    monkeypatch.setattr(INSTALL_CONFIG.live, "runtime", "python")
    monkeypatch.setattr(cli.os, "execve", lambda *a: pytest.fail("exec"))
    ran = []
    import recog.live

    monkeypatch.setattr(recog.live, "live_pipeline", lambda **kwargs: ran.append(kwargs))
    monkeypatch.setattr(sys, "argv", ["recog", "live", "--list-devices"])

    cli.main()

    assert ran and ran[0]["list_devices"] is True
