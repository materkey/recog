"""`[web] runtime` and `[copilot] runtime`: the services' Python entry points hand over
to their Rust binaries (recog/rust_runtime.py) when the setting asks for it and the
binary is built."""

import pytest

from recog import rust_runtime
from recog.install_config import INSTALL_CONFIG


def _binary(tmp_path, name="recog-web"):
    binary = tmp_path / name
    binary.write_text("#!/bin/sh\n")
    binary.chmod(0o755)
    return binary


@pytest.fixture
def execs(monkeypatch):
    """os.execv recorded instead of run; the call ends the test's "process"."""
    calls = []

    def fake_execv(path, argv):
        calls.append((path, argv))
        raise SystemExit(0)

    monkeypatch.setattr(rust_runtime.os, "execv", fake_execv)
    return calls


def test_python_runtime_choosing_the_program_keeps_python_silently(tmp_path, capsys):
    """given: a runtime left at "python" and a built binary
    when: the program is chosen
    then: none, and nothing is logged"""
    program = rust_runtime.rust_program(
        "[web] runtime", "python", _binary(tmp_path), "[web]", "the web server"
    )
    assert program is None
    assert capsys.readouterr().err == ""


def test_rust_runtime_and_a_built_binary_choosing_the_program_gives_the_binary(tmp_path):
    binary = _binary(tmp_path)
    assert rust_runtime.rust_program("[web] runtime", "rust", binary, "[web]", "x") == binary


def test_rust_runtime_without_the_build_choosing_the_program_falls_back_in_one_line(
    tmp_path, capsys
):
    """given: runtime "rust" but no binary (cargo was missing when setup ran)
    when: the program is chosen
    then: Python, with one line naming the binary and setup.sh"""
    missing = tmp_path / "recog-web"
    program = rust_runtime.rust_program("[web] runtime", "rust", missing, "[web]", "the web server")
    assert program is None
    err = capsys.readouterr().err
    assert err.count("\n") == 1
    assert err.startswith("[web] ") and str(missing) in err and "setup.sh" in err
    assert "the web server runs in Python" in err


def test_rust_runtime_with_a_non_executable_file_choosing_the_program_falls_back(tmp_path):
    binary = _binary(tmp_path)
    binary.chmod(0o644)
    assert rust_runtime.rust_program("[web] runtime", "rust", binary, "[web]", "x") is None


def test_unknown_runtime_choosing_the_program_stops_naming_the_key(tmp_path):
    with pytest.raises(
        ValueError, match=r'\[web\] runtime must be "python" or "rust", not \'rsut\''
    ):
        rust_runtime.rust_program("[web] runtime", "rsut", _binary(tmp_path), "[web]", "x")


def test_rust_web_runtime_starting_the_server_execs_recog_web_on_the_same_address(
    tmp_path, monkeypatch, execs, capsys
):
    """given: [web] runtime = "rust" and a built recog-web
    when: `python -m recog.web.server` hands over on 127.0.0.1:9080
    then: the process becomes recog-web on that host and port, with this checkout"""
    binary = _binary(tmp_path)
    monkeypatch.setattr(rust_runtime, "RUST_WEB", binary)
    monkeypatch.setattr(INSTALL_CONFIG.web, "runtime", "rust")

    with pytest.raises(SystemExit):
        rust_runtime.hand_over_web("127.0.0.1", 9080, [])

    path, argv = execs[0]
    assert path == binary
    assert argv == [
        str(binary),
        "--host",
        "127.0.0.1",
        "--port",
        "9080",
        "--repo",
        str(rust_runtime.REPO_ROOT),
    ]
    assert '[web] runtime = "rust": running' in capsys.readouterr().err


def test_rust_web_runtime_with_arguments_starting_the_server_passes_them_last(
    tmp_path, monkeypatch, execs
):
    """given: [web] runtime = "rust" and `python -m recog.web.server --port 19080`
    when: the server hands over
    then: the extra arguments follow the defaults, so recog-web's later --port wins"""
    binary = _binary(tmp_path)
    monkeypatch.setattr(rust_runtime, "RUST_WEB", binary)
    monkeypatch.setattr(INSTALL_CONFIG.web, "runtime", "rust")

    with pytest.raises(SystemExit):
        rust_runtime.hand_over_web("127.0.0.1", 9080, ["--port", "19080"])

    assert execs[0][1][-2:] == ["--port", "19080"]


def test_python_web_runtime_starting_the_server_returns_without_exec(monkeypatch, execs):
    """given: the default [web] runtime
    when: the server would hand over
    then: no exec; the caller goes on to uvicorn"""
    monkeypatch.setattr(INSTALL_CONFIG.web, "runtime", "python")

    rust_runtime.hand_over_web("127.0.0.1", 9080, [])

    assert execs == []


def test_rust_web_runtime_without_the_build_starting_the_server_stays_in_python(
    tmp_path, monkeypatch, execs, capsys
):
    monkeypatch.setattr(rust_runtime, "RUST_WEB", tmp_path / "recog-web")
    monkeypatch.setattr(INSTALL_CONFIG.web, "runtime", "rust")

    rust_runtime.hand_over_web("127.0.0.1", 9080, [])

    assert execs == []
    assert "the web server runs in Python" in capsys.readouterr().err


COPILOT_SERVICES = [
    pytest.param("RUST_COPILOT_SUPERVISOR", "recog-copilot-supervisor", id="supervisor"),
    pytest.param("RUST_COPILOT_CHANNEL", "recog-copilot-channel", id="channel"),
]


@pytest.mark.parametrize("attr, name", COPILOT_SERVICES)
def test_rust_copilot_runtime_starting_a_copilot_process_execs_its_binary_with_its_arguments(
    tmp_path, monkeypatch, execs, capsys, attr, name
):
    """given: [copilot] runtime = "rust" and the built binary
    when: the supervisor or the channel starts with Python's own arguments
    then: the process becomes the binary with the checkout and those arguments"""
    binary = _binary(tmp_path, name)
    monkeypatch.setattr(INSTALL_CONFIG.copilot, "runtime", "rust")

    with pytest.raises(SystemExit):
        rust_runtime.hand_over_copilot(
            binary, "[copilot-x]", "the process", ["--bridge-url", "ws://127.0.0.1:1"]
        )

    path, argv = execs[0]
    assert path == binary
    assert argv == [
        str(binary),
        "--repo-root",
        str(rust_runtime.REPO_ROOT),
        "--bridge-url",
        "ws://127.0.0.1:1",
    ]
    assert '[copilot] runtime = "rust": running' in capsys.readouterr().err
    assert getattr(rust_runtime, attr) == rust_runtime.RELEASE_DIR / name


def test_python_copilot_runtime_starting_a_copilot_process_returns_without_exec(
    tmp_path, monkeypatch, execs
):
    monkeypatch.setattr(INSTALL_CONFIG.copilot, "runtime", "python")

    rust_runtime.hand_over_copilot(_binary(tmp_path), "[copilot-x]", "the process", [])

    assert execs == []


def test_rust_copilot_runtime_without_the_build_starting_the_channel_stays_in_python(
    tmp_path, monkeypatch, execs, capsys
):
    """given: [copilot] runtime = "rust" but no binary
    when: the channel starts
    then: no exec, and one line under the channel's prefix says it runs in Python"""
    monkeypatch.setattr(INSTALL_CONFIG.copilot, "runtime", "rust")

    rust_runtime.hand_over_copilot(
        tmp_path / "recog-copilot-channel", "[copilot-channel]", "the channel", []
    )

    assert execs == []
    err = capsys.readouterr().err
    assert err.count("\n") == 1
    assert err.startswith("[copilot-channel] ") and "the channel runs in Python" in err


def test_unknown_copilot_runtime_starting_a_copilot_process_stops_naming_the_key(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(INSTALL_CONFIG.copilot, "runtime", "Rust")

    with pytest.raises(ValueError, match=r"\[copilot\] runtime must be"):
        rust_runtime.hand_over_copilot(_binary(tmp_path), "[copilot-x]", "x", [])
