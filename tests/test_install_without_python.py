"""The probes of an installation without Python (`[install] runtime = "rust"`).

`scripts/deps.sh` reads the runtime itself (the calling shell, else a plain line
under `[install]` in config.toml) and, in that mode, asks `bin/recog` what it asked
Python: the configuration check, the data directory, the config path. Every test
puts a logging `python` first on PATH and asserts it is never called.
"""

import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
BUILT_RECOG = REPO_ROOT / "rust/target/debug/recog"
PROGRAMS = (
    "recog",
    "recog-web",
    "recog-copilot-supervisor",
    "recog-copilot-channel",
    "recog-desktop",
    "recog-mic",
)

# `recog` as the probes see it: logs its arguments, answers from the environment.
RECOG_STUB = r"""#!/bin/sh
printf '%s\n' "$(basename "$0") $*" >> "$STUB_LOG"
case "$*" in
  "check") [ -n "$STUB_CHECK" ] && { printf '%s\n' "$STUB_CHECK"; exit 1; }; exit 0 ;;
  "config install-runtime") printf '%s\n' "${STUB_PARSED_RUNTIME:-rust}" ;;
  "config data-dir") printf '%s\n' "$STUB_DATA_DIR" ;;
  "config config-path") printf '%s\n' "$STUB_CONFIG_PATH" ;;
esac
exit 0
"""

LOGGING_PYTHON = """#!/bin/sh
printf '%s\\n' "python $*" >> "$STUB_LOG"
exit 1
"""


@pytest.fixture
def checkout(tmp_path):
    """scripts/ of this repository, bin/ with stub programs, a logging python."""
    repo = tmp_path / "recog"
    repo.mkdir()
    shutil.copytree(REPO_ROOT / "scripts", repo / "scripts")
    (repo / "recog-service.sh").write_text("#!/bin/bash\n")
    (repo / "bin").mkdir()
    for name in PROGRAMS:
        program = repo / "bin" / name
        program.write_text(RECOG_STUB)
        program.chmod(0o755)
    stubs = tmp_path / "stubs"
    stubs.mkdir()
    for name in ("python", "python3", "python3.12"):
        (stubs / name).write_text(LOGGING_PYTHON)
        (stubs / name).chmod(0o755)
    home = tmp_path / "home"
    home.mkdir()
    return repo, stubs, home, tmp_path / "stub.log"


def deps(checkout, snippet, env_extra=None, runtime="rust"):
    """Runs `snippet` with scripts/deps.sh sourced; the stdout and the log."""
    repo, stubs, home, log = checkout
    env = {
        "PATH": f"{stubs}{os.pathsep}/usr/bin:/bin",
        "HOME": str(home),
        "STUB_LOG": str(log),
        "RECOG_INSTALL_RUNTIME": runtime,
        "RECOG_CONFIG_FILE": str(home / "absent.toml"),
        **(env_extra or {}),
    }
    result = subprocess.run(
        ["bash", "-c", f'source "$1"; {snippet}', "--", str(repo / "scripts/deps.sh")],
        env=env,
        capture_output=True,
        text=True,
    )
    result.calls = log.read_text().splitlines() if log.exists() else []
    return result


def python_calls(result):
    return [call for call in result.calls if call.startswith("python")]


@pytest.mark.parametrize(
    "text,expected",
    [
        (None, "python"),
        ("", "python"),
        ('[install]\nruntime = "rust"\n', "rust"),
        ("[install]\nruntime = 'rust'\n", "rust"),
        ('# comment\n[ install ]   # the mode\n  runtime   =   "rust"   # no Python\n', "rust"),
        ('[live]\nruntime = "rust"\n[web]\nruntime = "rust"\n', "python"),
        ('[install]\n# runtime = "rust"\n', "python"),
        ('[install]\nruntime = "rust"\n[live]\nruntime = "python"\n', "rust"),
        ('[install]\nruntime = "go"\n', "go"),
    ],
)
def test_config_file_when_reading_the_install_runtime_then_only_install_runtime_counts(
    checkout, text, expected
):
    _, _, home, _ = checkout
    config = home / "config.toml"
    if text is not None:
        config.write_text(text)

    result = deps(checkout, "deps_install_runtime", {"RECOG_CONFIG_FILE": str(config)}, runtime="")

    assert result.stdout == f"{expected}\n"
    assert not python_calls(result)


def test_calling_shell_and_config_file_when_reading_the_install_runtime_then_the_shell_wins(
    checkout,
):
    _, _, home, _ = checkout
    config = home / ".config/recog/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text('[install]\nruntime = "rust"\n')

    default_location = deps(checkout, "deps_install_runtime", {"RECOG_CONFIG_FILE": ""}, runtime="")
    overridden = deps(
        checkout, "deps_install_runtime", {"RECOG_CONFIG_FILE": ""}, runtime=" python "
    )

    assert default_location.stdout == "rust\n"
    assert overridden.stdout == "python\n"


def test_parser_disagreeing_with_the_scripts_when_checking_then_one_problem_names_both(checkout):
    """given: config.toml says nothing, but .env says rust to the parser"""
    result = deps(checkout, "deps_install_runtime_problem", {"STUB_PARSED_RUNTIME": "python"})

    assert '[install] runtime is "python" for recog but "rust" for setup.sh' in result.stdout
    assert "never from .env" in result.stdout


def test_invalid_or_agreeing_install_runtime_when_checking_then_a_problem_or_nothing(checkout):
    invalid = deps(checkout, "deps_install_runtime_problem", runtime="go")
    agreeing = deps(checkout, "deps_install_runtime_problem")

    assert invalid.stdout.startswith('[install] runtime must be "python" or "rust", not "go"')
    assert agreeing.stdout == ""
    assert not python_calls(invalid) and not python_calls(agreeing)


def test_installation_without_python_when_checking_the_config_then_recog_check_answers(checkout):
    ok = deps(checkout, "deps_asr_config_check")
    broken = deps(checkout, "deps_asr_config_check", {"STUB_CHECK": "Invalid AI settings: x"})

    assert ok.returncode == 0 and ok.stdout == ""
    assert broken.returncode == 1 and broken.stdout == "Invalid AI settings: x\n"
    assert "recog check" in ok.calls
    assert not python_calls(ok) and not python_calls(broken)


@pytest.mark.parametrize("host_os", ["Linux", "Darwin"])
def test_installation_without_python_when_checking_readiness_then_no_python_and_no_portaudio(
    checkout, host_os
):
    repo, stubs, _, _ = checkout
    for tool in ("ffmpeg", "tmux", "claude", "parec", "pactl"):
        (stubs / tool).write_text("#!/bin/sh\nexit 0\n")
        (stubs / tool).chmod(0o755)
    shutil.copy2(stubs / "tmux", repo / "sck_audio_capture")
    launcher = repo / "Recog.app/Contents/MacOS/recog_launcher"
    launcher.parent.mkdir(parents=True)
    shutil.copy2(stubs / "tmux", launcher)

    result = deps(checkout, "deps_readiness_problems", {"RECOG_HOST_OS": host_os})

    assert result.stdout == ""
    assert not python_calls(result)


@pytest.mark.parametrize("host_os,mic", [("Linux", False), ("Darwin", True)])
def test_missing_programs_when_checking_readiness_then_each_is_named(checkout, host_os, mic):
    repo, _, _, _ = checkout
    for name in ("recog-web", "recog-mic"):
        (repo / "bin" / name).unlink()

    result = deps(checkout, "deps_readiness_problems", {"RECOG_HOST_OS": host_os})

    assert f"{repo}/bin/recog-web is not installed" in result.stdout
    assert (f"{repo}/bin/recog-mic is not installed" in result.stdout) is mic
    assert "PortAudio" not in result.stdout


def test_installation_without_python_when_resolving_paths_then_recog_config_answers(checkout):
    repo, stubs, home, log = checkout
    env = {
        "PATH": f"{stubs}{os.pathsep}/usr/bin:/bin",
        "HOME": str(home),
        "STUB_LOG": str(log),
        "RECOG_INSTALL_RUNTIME": "rust",
        "STUB_DATA_DIR": "/data root",
        "STUB_CONFIG_PATH": "/c.toml",
    }

    data = subprocess.run(
        ["bash", str(repo / "scripts/data-dir.sh")], env=env, capture_output=True, text=True
    )
    config = subprocess.run(
        ["bash", str(repo / "scripts/config-path.sh")], env=env, capture_output=True, text=True
    )
    (repo / "bin/recog").unlink()
    missing = subprocess.run(
        ["bash", str(repo / "scripts/data-dir.sh")], env=env, capture_output=True, text=True
    )

    assert (data.returncode, data.stdout) == (0, "/data root\n")
    assert (config.returncode, config.stdout) == (0, "/c.toml\n")
    assert missing.returncode != 0
    assert "bin/recog is not installed" in missing.stderr
    assert not [line for line in log.read_text().splitlines() if line.startswith("python")]


@pytest.mark.skipif(
    not BUILT_RECOG.exists(), reason="needs cargo build -p recog-live (scripts/check.sh)"
)
@pytest.mark.parametrize(
    "payload,ready",
    [
        (None, True),
        (
            '{"version":1,"revision":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","models":{},"tasks":{}}',
            True,
        ),
        ("broken JSON", False),
        (
            '{"version":1,"revision":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","models":{"api_key":"secret"},"tasks":{}}',
            False,
        ),
    ],
)
def test_real_recog_check_when_ai_settings_vary_then_it_blocks_as_the_python_probe_did(
    checkout, tmp_path, payload, ready
):
    """The cases of test_ai_settings_when_checking_readiness_then_invalid_settings_block_activation
    (tests/test_setup.py) against the built binary installed into bin/."""
    repo, _, _, _ = checkout
    program = repo / "bin/recog"
    program.unlink()
    try:
        os.link(BUILT_RECOG, program)
    except OSError:
        shutil.copy2(BUILT_RECOG, program)
    data = tmp_path / "data"
    data.mkdir()
    settings = data / "ai-settings.json"
    if payload is not None:
        settings.write_text(payload)

    result = deps(checkout, "deps_asr_config_check", {"RECOG_DATA_DIR": str(data)})

    assert (result.returncode == 0) is ready, result.stdout + result.stderr
    if not ready:
        assert "Invalid AI settings" in result.stdout
    if payload is not None:
        assert settings.read_text() == payload
    assert not (data / "ai-settings.lock").exists()
    assert not python_calls(result)
