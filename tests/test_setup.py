"""Tests for `setup.sh` and `scripts/prefetch_gigaam.py`.

setup.sh is driven end-to-end against a throwaway copy of the repo with every
external command stubbed on PATH (python/pip/npm/ffmpeg/systemctl/...), so the
assertions are about *what it decides* — which index torch comes from, which
GigaAM ref is installed, whether a running service is restarted rather than
started, whether an incomplete install is allowed to touch the service manager
at all — without installing anything.

RECOG_HOST_OS / RECOG_SERVICE_OS pin the OS, so the Linux decisions are
covered from a Mac and vice versa.
"""
import email.message
import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


@pytest.mark.parametrize("payload,ready", [
    (None, True),
    ('{"version":1,"revision":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","models":{},"tasks":{}}', True),
    ("broken JSON", False),
    ('{"version":1,"revision":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","models":{"api_key":"secret"},"tasks":{}}', False),
])
def test_ai_settings_when_checking_readiness_then_invalid_settings_block_activation(tmp_path, payload, ready):
    root = Path(__file__).resolve().parent.parent
    settings = tmp_path / "ai-settings.json"
    if payload is not None:
        settings.write_text(payload)
    result = subprocess.run(
        ["bash", "-c", 'source "$1"; deps_asr_config_check', "--", str(root / "scripts/deps.sh")],
        env={**os.environ, "DEPS_PYTHON": sys.executable, "RECOG_DATA_DIR": str(tmp_path),
             "RECOG_ASR_BACKEND": "local"}, capture_output=True, text=True,
    )
    assert (result.returncode == 0) is ready, result.stdout + result.stderr
    if not ready:
        assert "Invalid AI settings" in result.stdout
    if payload is None:
        assert not settings.exists()
    else:
        assert settings.read_text() == payload
    assert not (tmp_path / "ai-settings.lock").exists()

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import prefetch_gigaam  # noqa: E402


# ---------------------------------------------------------------------------
# Fake repo + stub commands
# ---------------------------------------------------------------------------

# `python` covers both the bootstrap interpreter and the venv one setup.sh
# creates from it: it answers the version probe, materialises a venv on
# `-m venv`, and lets the library probes be steered per test.
PYTHON_STUB = r"""#!/bin/sh
printf '%s\n' "python $*" >> "$STUB_LOG"
case "$*" in
  *"print(get_data_dir())"*) exec "$STUB_REAL_PYTHON" "$@" ;;
  *sys.version_info*)   echo "${STUB_PY_VERSION:-3.12}"; exit 0 ;;
  *"-m venv"*)
      for arg in "$@"; do target="$arg"; done
      mkdir -p "$target/bin"
      cp "$STUB_BIN/python3.12" "$target/bin/python"
      cp "$STUB_BIN/pip" "$target/bin/pip"
      exit 0 ;;
  *recog.config*)       exit "${STUB_ASR_RC:-0}" ;;
  *sounddevice*)        exit "${STUB_PORTAUDIO_RC:-0}" ;;
  *prefetch_gigaam*)    exit "${STUB_PREFETCH_RC:-0}" ;;
  *"print(INSTALL_CONFIG.asr.backend)"*) echo "${STUB_ASR_BACKEND:-local}"; exit 0 ;;
esac
exit 0
"""

PLAIN_STUB = """#!/bin/sh
printf '%s\\n' "{name} $*" >> "$STUB_LOG"
exit {code}
"""

# Present by default; a test drops one to simulate a missing dependency.
DEFAULT_STUBS = (
    "ffmpeg", "parec", "pactl", "pip", "npm", "node", "tmux", "claude",
    "jq", "swiftc", "launchctl", "systemctl", "curl",
)
# Builds the echo canceller on macOS (rust/speexdsp-aec-py).
DEFAULT_STUBS += ("cargo",)

COPY_FILES = (
    "setup.sh",
    "recog-service.sh",
    ".env.example",
)
COPY_TREES = ("scripts", "deploy", "recog")


@pytest.fixture
def repo(tmp_path):
    """A throwaway copy of everything setup.sh reads."""
    dst = tmp_path / "recog"
    dst.mkdir()
    for rel in COPY_FILES:
        shutil.copy2(REPO_ROOT / rel, dst / rel)
    for rel in COPY_TREES:
        shutil.copytree(
            REPO_ROOT / rel, dst / rel,
            ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
        )
    # A prebuilt macOS helper: the darwin readiness probe wants it executable.
    helper = dst / "sck_audio_capture"
    helper.write_text("#!/bin/sh\nexit 0\n")
    helper.chmod(0o755)
    (dst / "recog/native/macos").mkdir(parents=True, exist_ok=True)
    (dst / "recog/native/macos/sck_audio_capture.swift").write_text("// stub\n")
    # The live job's program: the darwin readiness probe wants it executable.
    launcher = dst / "Recog.app/Contents/MacOS/recog_launcher"
    launcher.parent.mkdir(parents=True)
    launcher.write_text('#!/bin/sh\nexec "$@"\n')
    launcher.chmod(0o755)
    # The echo canceller's Python package: setup.sh hands its directory to pip.
    (dst / "rust/speexdsp-aec-py").mkdir(parents=True)
    (dst / "rust/speexdsp-aec-py/pyproject.toml").write_text('[project]\nname = "speexdsp-aec"\n')
    # The ASR backend "wavo"'s Python package, built only when that backend is chosen.
    (dst / "rust/wavo-asr-py").mkdir(parents=True)
    (dst / "rust/wavo-asr-py/pyproject.toml").write_text('[project]\nname = "wavo-asr"\n')
    return dst


# Plain utilities setup.sh and the service scripts shell out to. They are
# symlinked into a scratch dir that becomes the *entire* non-stub PATH, so a
# test that drops a stub to simulate a missing dependency cannot accidentally
# find the host's real ffmpeg / swiftc / node instead.
SYS_TOOLS = (
    "bash", "sh", "sed", "grep", "awk", "head", "tail", "wc", "du", "mkdir",
    "cp", "mv", "rm", "ln", "cat", "dirname", "basename", "readlink", "sleep",
    "tr", "uname", "sort", "date", "chmod", "git",
)


@pytest.fixture
def stubs(tmp_path):
    """PATH dirs (stubs + plain utilities) plus the log the stubs append to."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    sys_dir = tmp_path / "sysbin"
    sys_dir.mkdir()
    log = tmp_path / "stub.log"

    (bin_dir / "python3.12").write_text(PYTHON_STUB)
    (bin_dir / "python3.12").chmod(0o755)
    for name in DEFAULT_STUBS:
        stub = bin_dir / name
        stub.write_text(PLAIN_STUB.format(name=name, code=0))
        stub.chmod(0o755)
    for name in SYS_TOOLS:
        real = shutil.which(name)
        if real:
            (sys_dir / name).symlink_to(real)
    return bin_dir, log, sys_dir


def run_setup(repo, stubs, *args, host_os="Linux", env_extra=None, check=False):
    bin_dir, log, sys_dir = stubs
    env = {
        "HOME": str(repo.parent / "home"),
        "PATH": f"{bin_dir}{os.pathsep}{sys_dir}",
        "STUB_LOG": str(log),
        "STUB_BIN": str(bin_dir),
        "STUB_REAL_PYTHON": sys.executable,
        "RECOG_HOST_OS": host_os,
        # Pinned with the OS: setup.sh rejects macOS on anything but arm64,
        # and the Darwin decisions are exercised from a Linux CI runner.
        "RECOG_HOST_ARCH": "arm64" if host_os == "Darwin" else "x86_64",
        "RECOG_SERVICE_OS": host_os,
        "TERM": "dumb",
    }
    Path(env["HOME"]).mkdir(exist_ok=True)
    env.update(env_extra or {})
    result = subprocess.run(
        ["bash", str(repo / "setup.sh"), *args],
        capture_output=True, text=True, env=env, check=check,
    )
    result.calls = log.read_text().splitlines() if log.exists() else []
    return result


def offline_args(*extra):
    """Flags that keep a run free of network and service-manager work."""
    return ("--no-prefetch", "--no-extension", *extra)


def pip_calls(result):
    return [c for c in result.calls if c.startswith("pip ")]


# ---------------------------------------------------------------------------
# Syntax
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "script",
    ["setup.sh", "recog-service.sh", "scripts/deps.sh",
     "scripts/service/common.sh", "scripts/service/launchd.sh",
     "scripts/service/systemd.sh"],
)
def test_shell_scripts_parse(script):
    result = subprocess.run(
        ["bash", "-n", str(REPO_ROOT / script)], capture_output=True, text=True
    )
    assert result.returncode == 0, result.stderr


# ---------------------------------------------------------------------------
# Python and venv
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("host_os", ["Linux", "Darwin"])
@pytest.mark.parametrize("existing", [False, True])
def test_fresh_or_existing_install_when_dry_run_then_no_commands_or_files_change(repo, stubs, host_os, existing):
    if existing:
        venv_bin = repo / "venv-gigaam/bin"
        venv_bin.mkdir(parents=True)
        shutil.copy2(stubs[0] / "python3.12", venv_bin / "python")
    home = repo.parent / "home"
    home.mkdir()

    def snapshot():
        return {
            str(path.relative_to(repo.parent)): path.read_bytes() if path.is_file() else None
            for root in (repo, home) for path in root.rglob("*")
        }

    before = snapshot()
    result = run_setup(repo, stubs, "--dry-run", host_os=host_os)

    assert result.returncode == 0, result.stdout + result.stderr
    assert result.calls == []
    assert snapshot() == before
    assert ("Would reuse venv" if existing else "Would create venv") in result.stdout
    assert "Would install GigaAM" in result.stdout
    assert "Would install " + host_os + " service units" in result.stdout
    assert "readiness are not verified" in result.stdout
    assert ("Would build sck_audio_capture" in result.stdout) is (host_os == "Darwin")


def test_skip_flags_and_overrides_when_dry_run_then_plan_reflects_them(repo, stubs):
    result = run_setup(
        repo, stubs, "--dry-run", "--no-services", "--no-env", "--no-prefetch", "--no-extension",
        env_extra={"RECOG_TORCH_INDEX_URL": "https://example.test/torch", "RECOG_GIGAAM_REF": "test-pin"},
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert result.calls == []
    for flag in ("--no-services", "--no-env", "--no-prefetch", "--no-extension"):
        assert f"({flag})" in result.stdout
    assert "https://example.test/torch" in result.stdout
    assert "@test-pin" in result.stdout
    assert "Would install Linux service units" not in result.stdout
    assert "Would copy" not in result.stdout


def test_creates_the_venv_and_installs_the_pinned_gigaam(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert any("-m venv" in call for call in result.calls)
    assert any(
        "gigaam @ git+https://github.com/salute-developers/GigaAM.git@6e4b027" in call
        for call in pip_calls(result)
    ), pip_calls(result)
    assert any("deploy/python/requirements.txt" in call for call in pip_calls(result))


def test_reuses_an_existing_venv_symlink(repo, stubs, tmp_path):
    """The reference Mac has venv-gigaam as a symlink into the GigaAM
    checkout; recreating it would swap the interpreter and every version."""
    real_venv = tmp_path / "elsewhere-venv"
    (real_venv / "bin").mkdir(parents=True)
    shutil.copy2(stubs[0] / "python3.12", real_venv / "bin/python")
    shutil.copy2(stubs[0] / "pip", real_venv / "bin/pip")
    (repo / "venv-gigaam").symlink_to(real_venv)

    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert (repo / "venv-gigaam").is_symlink()
    assert not any("-m venv" in call for call in result.calls)
    assert "Reusing venv" in result.stdout


def test_python_older_than_312_is_refused(repo, stubs):
    result = run_setup(
        repo, stubs, *offline_args("--no-services"),
        env_extra={"STUB_PY_VERSION": "3.11"},
    )

    assert result.returncode != 0
    assert "3.12" in result.stderr
    assert not pip_calls(result)


# ---------------------------------------------------------------------------
# torch index / constraints / GigaAM source
# ---------------------------------------------------------------------------


def test_linux_without_nvidia_uses_the_cpu_torch_index(repo, stubs):
    """Default PyPI torch on Linux drags in a CUDA runtime a GPU-less box
    never uses."""
    assert not (stubs[0] / "nvidia-smi").exists(), "nvidia-smi must not be stubbed here"

    result = run_setup(repo, stubs, *offline_args("--no-services"))

    torch_call = next(c for c in pip_calls(result) if " torch " in f" {c} ")
    assert "--index-url https://download.pytorch.org/whl/cpu" in torch_call


def test_linux_with_nvidia_keeps_the_default_cuda_torch_wheels(repo, stubs):
    """given: a Linux box where nvidia-smi is on PATH
    when: setup installs torch
    then: no --index-url is forced — the CPU-only override would leave
          RECOG_ASR_DEVICE=cuda failing at startup on a GPU machine
    """
    stub = stubs[0] / "nvidia-smi"
    stub.write_text(PLAIN_STUB.format(name="nvidia-smi", code=0))
    stub.chmod(0o755)

    result = run_setup(repo, stubs, *offline_args("--no-services"))

    torch_call = next(c for c in pip_calls(result) if " torch " in f" {c} ")
    assert "--index-url" not in torch_call


def test_torch_index_url_env_overrides_the_default(repo, stubs):
    result = run_setup(
        repo, stubs, *offline_args("--no-services"),
        env_extra={"RECOG_TORCH_INDEX_URL": "https://example.test/cu129"},
    )

    torch_call = next(c for c in pip_calls(result) if " torch " in f" {c} ")
    assert "--index-url https://example.test/cu129" in torch_call


def test_empty_torch_index_url_falls_back_to_plain_pypi(repo, stubs):
    result = run_setup(
        repo, stubs, *offline_args("--no-services"),
        env_extra={"RECOG_TORCH_INDEX_URL": ""},
    )

    torch_call = next(c for c in pip_calls(result) if " torch " in f" {c} ")
    assert "--index-url" not in torch_call


def test_macos_pip_stages_all_apply_the_constraints_file(repo, stubs):
    """Porting to Linux must not bump the working Mac's ML stack — every pip
    stage on darwin runs under deploy/python/constraints/macos.txt."""
    result = run_setup(repo, stubs, *offline_args("--no-services"), host_os="Darwin")

    installs = [c for c in pip_calls(result) if c.startswith("pip install")]
    assert installs
    for call in installs:
        assert "-c " in call and "deploy/python/constraints/macos.txt" in call, call
    assert any("deploy/python/requirements-macos.txt" in call for call in installs)


def _echo_module_installs(result):
    return [
        c for c in pip_calls(result) if c.startswith("pip install") and "rust/speexdsp-aec-py" in c
    ]


def test_macos_with_cargo_setup_builds_the_echo_canceller_under_the_constraints(repo, stubs):
    """given: macOS with cargo on PATH
    when: setup runs
    then: pip builds the Rust echo canceller, under the macOS constraints like every stage"""
    result = run_setup(repo, stubs, *offline_args("--no-services"), host_os="Darwin")

    assert result.returncode == 0, result.stdout + result.stderr
    [call] = _echo_module_installs(result)
    assert "deploy/python/constraints/macos.txt" in call


def test_macos_without_cargo_setup_goes_on_and_says_echo_cancellation_is_unavailable(repo, stubs):
    """given: macOS without cargo (echo cancellation is off by default)
    when: setup runs
    then: it does not try the build, names rustup, and still finishes"""
    (stubs[0] / "cargo").unlink()

    result = run_setup(repo, stubs, *offline_args("--no-services"), host_os="Darwin")

    assert result.returncode == 0, result.stdout + result.stderr
    assert _echo_module_installs(result) == []
    assert "echo cancellation" in result.stdout and "rustup" in result.stdout


def test_macos_failed_echo_build_setup_goes_on_and_says_so(repo, stubs):
    """given: macOS where the build of the echo canceller fails
    when: setup runs
    then: it reports the failure and finishes the rest"""
    pip = stubs[0] / "pip"
    pip.write_text(
        '#!/bin/sh\nprintf \'%s\\n\' "pip $*" >> "$STUB_LOG"\n'
        'case "$*" in *speexdsp-aec-py*) exit 1 ;; esac\nexit 0\n'
    )

    result = run_setup(repo, stubs, *offline_args("--no-services"), host_os="Darwin")

    assert result.returncode == 0, result.stdout + result.stderr
    assert "echo canceller" in result.stdout and "not built" in result.stdout


def test_linux_setup_does_not_build_the_echo_canceller(repo, stubs):
    """given: Linux, where echo cancellation does not run
    when: setup runs
    then: no build"""
    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert _echo_module_installs(result) == []


def _wavo_module_installs(result):
    return [c for c in pip_calls(result) if c.startswith("pip install") and "rust/wavo-asr-py" in c]


@pytest.mark.parametrize("host_os", ["Linux", "Darwin"])
def test_backend_wavo_with_cargo_setup_builds_the_wavo_module(repo, stubs, host_os):
    """given: [asr] backend = "wavo" and cargo on PATH
    when: setup runs
    then: pip builds rust/wavo-asr-py, on either system"""
    result = run_setup(
        repo,
        stubs,
        *offline_args("--no-services"),
        host_os=host_os,
        env_extra={"STUB_ASR_BACKEND": "wavo"},
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert len(_wavo_module_installs(result)) == 1


def test_backend_local_setup_does_not_build_the_wavo_module(repo, stubs):
    """given: the default backend
    when: setup runs
    then: no wavo build, whose release build takes minutes"""
    result = run_setup(repo, stubs, *offline_args("--no-services"), host_os="Darwin")

    assert _wavo_module_installs(result) == []


def test_backend_wavo_without_cargo_setup_goes_on_and_says_what_to_install(repo, stubs):
    """given: backend wavo and no cargo
    when: setup runs
    then: no build attempt, rustup named, and the rest still runs (readiness then reports it)"""
    (stubs[0] / "cargo").unlink()

    result = run_setup(
        repo, stubs, *offline_args("--no-services"), env_extra={"STUB_ASR_BACKEND": "wavo"}
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert _wavo_module_installs(result) == []
    assert "wavo" in result.stdout and "rustup" in result.stdout


def test_macos_dry_run_mentions_the_echo_canceller(repo, stubs):
    result = run_setup(repo, stubs, "--dry-run", host_os="Darwin")

    assert "rust/speexdsp-aec-py" in result.stdout


def test_linux_does_not_apply_the_macos_constraints(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services"))

    for call in pip_calls(result):
        assert "deploy/python/constraints/macos.txt" not in call
    assert not any("deploy/python/requirements-macos.txt" in call for call in pip_calls(result))


def test_editable_gigaam_needs_an_explicit_path(repo, stubs, tmp_path):
    checkout = tmp_path / "GigaAM"
    checkout.mkdir()
    (checkout / "pyproject.toml").write_text('[project]\nname = "gigaam"\n')

    result = run_setup(
        repo, stubs, *offline_args("--no-services"),
        env_extra={"RECOG_GIGAAM_EDITABLE": str(checkout)},
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert any(f"install -e {checkout}" in call for call in pip_calls(result))
    assert not any("git+" in call for call in pip_calls(result))


def test_editable_gigaam_rejects_a_directory_without_pyproject(repo, stubs, tmp_path):
    empty = tmp_path / "not-gigaam"
    empty.mkdir()

    result = run_setup(
        repo, stubs, *offline_args("--no-services"),
        env_extra={"RECOG_GIGAAM_EDITABLE": str(empty)},
    )

    assert result.returncode != 0
    assert "pyproject.toml" in result.stderr


def test_gigaam_ref_is_overridable(repo, stubs):
    result = run_setup(
        repo, stubs, *offline_args("--no-services"),
        env_extra={"RECOG_GIGAAM_REF": "deadbee", "RECOG_GIGAAM_URL": "https://git.test/g.git"},
    )

    assert any(
        "gigaam @ git+https://git.test/g.git@deadbee" in call
        for call in pip_calls(result)
    )


# ---------------------------------------------------------------------------
# Config files
# ---------------------------------------------------------------------------


def test_creates_config_files_from_the_examples(repo, stubs):
    run_setup(repo, stubs, *offline_args("--no-services"))

    assert (repo / ".env").read_text() == (repo / ".env.example").read_text()
    assert (repo / "recog/web/.env").is_file()
    assert not (repo.parent / "home/.recog/speakers_ecapa.json").exists()


def test_no_env_when_config_files_are_absent_then_neither_is_created(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services", "--no-env"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert not (repo / ".env").exists()
    assert not (repo / "recog/web/.env").exists()


def install_unit(repo, name="recog-live.service"):
    unit_dir = repo.parent / "home/.config/systemd/user"
    unit_dir.mkdir(parents=True, exist_ok=True)
    (unit_dir / name).write_text("[Service]\n")


def test_installed_services_when_config_files_are_absent_then_neither_is_created(repo, stubs):
    """An installation that runs without environment files keeps running without them."""
    install_unit(repo)

    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert not (repo / ".env").exists()
    assert not (repo / "recog/web/.env").exists()
    assert "services are already installed" in result.stdout


def test_installed_services_when_dry_run_then_plan_keeps_config_files_absent(repo, stubs):
    install_unit(repo)

    result = run_setup(repo, stubs, "--dry-run")

    assert result.returncode == 0, result.stdout + result.stderr
    assert "Would copy" not in result.stdout
    assert "services are already installed" in result.stdout


def test_existing_config_files_are_never_overwritten(repo, stubs):
    (repo / ".env").write_text("RECOG_ASR_BACKEND=remote\nMINE=1\n")

    run_setup(repo, stubs, *offline_args("--no-services"))

    assert (repo / ".env").read_text() == "RECOG_ASR_BACKEND=remote\nMINE=1\n"


def test_migrated_database_when_setup_runs_then_preserved_outside_checkout(repo, stubs):
    """Обновление после миграции не должно заново создать пустую базу в git-дереве."""
    data = repo.parent / "home/.recog"
    data.mkdir(parents=True)
    database = data / "speakers_ecapa.json"
    database.write_bytes(b'{"Someone": {"embeddings": []}}')
    before = (database.read_bytes(), database.stat().st_ino)

    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert (database.read_bytes(), database.stat().st_ino) == before
    assert not (repo / "speakers_ecapa.json").exists()


# ---------------------------------------------------------------------------
# ASR configuration gate
# ---------------------------------------------------------------------------


def test_remote_asr_without_a_url_stops_before_pip(repo, stubs):
    """The check has to precede the package and unit changes: on an existing
    machine this is the migration that must be done by hand first."""
    result = run_setup(
        repo, stubs, *offline_args(), env_extra={"STUB_ASR_RC": "1"},
    )

    assert result.returncode != 0
    assert not pip_calls(result)
    assert not [c for c in result.calls if c.startswith(("systemctl", "launchctl"))]


# ---------------------------------------------------------------------------
# Services: install stages, readiness gates, activation
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("missing", ["claude", "tmux"])
def test_missing_copilot_cli_when_setup_runs_then_activation_is_blocked(repo, stubs, missing):
    (stubs[0] / missing).unlink()

    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode != 0
    assert f"{missing} not found (Copilot)" in result.stderr
    assert "RECOG_SKIP_COPILOT_CHECK=1 ./setup.sh" in result.stderr
    assert not [c for c in result.calls if "enable --now" in c or " restart " in c]


@pytest.mark.parametrize("missing_audio", [False, True])
def test_missing_copilot_when_checks_skipped_then_audio_still_gates_activation(
    repo, stubs, missing_audio
):
    for name in ("claude", "tmux"):
        (stubs[0] / name).unlink()
    if missing_audio:
        (stubs[0] / "ffmpeg").unlink()

    result = run_setup(repo, stubs, *offline_args(), env_extra={"RECOG_SKIP_COPILOT_CHECK": "1"})

    assert "not found (Copilot)" not in result.stderr
    enables = [c for c in result.calls if "enable --now" in c]
    if missing_audio:
        assert result.returncode != 0
        assert "ffmpeg not found" in result.stderr
        assert not enables
    else:
        assert result.returncode == 0, result.stdout + result.stderr
        assert len(enables) == 4


def test_no_services_flag_leaves_the_service_manager_alone(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0
    assert not [c for c in result.calls if c.startswith(("systemctl", "launchctl"))]
    assert "--no-services" in result.stdout


def test_install_precedes_activation_and_starts_every_unit(repo, stubs):
    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode == 0, result.stdout + result.stderr
    unit_dir = repo.parent / "home/.config/systemd/user"
    assert sorted(p.name for p in unit_dir.iterdir()) == [
        "recog-copilot-channel.service", "recog-copilot-supervisor.service",
        "recog-frontend.service", "recog-live.service",
    ]
    enables = [c for c in result.calls if "enable --now" in c]
    assert len(enables) == 4
    first_enable = next(i for i, c in enumerate(result.calls) if "enable --now" in c)
    daemon_reload = next(i for i, c in enumerate(result.calls) if "daemon-reload" in c)
    assert daemon_reload < first_enable


def test_a_running_service_is_restarted_not_started(repo, stubs):
    """An update must re-read the unit and the code of what was already
    running; `enable --now` on an active unit is a no-op."""
    systemctl = stubs[0] / "systemctl"
    systemctl.write_text(
        "#!/bin/sh\n"
        'printf "%s\\n" "systemctl $*" >> "$STUB_LOG"\n'
        'case "$*" in *"MainPID"*"recog-live"*) echo 4242 ;; esac\n'
        "exit 0\n"
    )
    systemctl.chmod(0o755)

    result = run_setup(repo, stubs, *offline_args())

    assert "systemctl --user restart recog-live.service" in result.calls
    assert not any("enable --now recog-live.service" in c for c in result.calls)
    assert any("enable --now recog-frontend.service" in c for c in result.calls)


def test_a_failed_activation_is_not_reported_as_success(repo, stubs):
    """given: systemctl, который не может активировать юниты (типовой Linux
              без user-D-Bus или без linger)
    when: setup доходит до активации
    then: сводка не рапортует «services are running» и код возврата ненулевой
          — иначе коллега и любой скриптовый вызов считают установку удачной,
          хотя не поднялся ни один юнит
    """
    systemctl = stubs[0] / "systemctl"
    systemctl.write_text(
        "#!/bin/sh\n"
        'printf "%s\\n" "systemctl $*" >> "$STUB_LOG"\n'
        'case "$*" in *"enable --now"*) exit 1 ;; esac\n'
        "exit 0\n"
    )
    systemctl.chmod(0o755)

    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode != 0
    assert "services are running" not in result.stdout
    assert "failed to activate" in result.stdout.lower()


def test_a_missing_required_dependency_blocks_activation(repo, stubs):
    """given: a host without ffmpeg
    when: setup runs with services enabled
    then: units are staged but nothing is activated, the exit code is
          non-zero, and a previously running instance is left alone
    """
    (stubs[0] / "ffmpeg").unlink()

    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode != 0
    assert "ffmpeg" in result.stdout + result.stderr
    assert (repo.parent / "home/.config/systemd/user/recog-live.service").is_file()
    assert not [c for c in result.calls if "enable --now" in c]
    assert not [c for c in result.calls if "restart" in c]
    assert not [c for c in result.calls if "stop" in c]


def test_linux_readiness_covers_portaudio(repo, stubs):
    """PortAudio is a shared library, not a binary: the only honest probe is
    importing sounddevice, which is why it happens after the pip stages."""
    result = run_setup(
        repo, stubs, *offline_args(), env_extra={"STUB_PORTAUDIO_RC": "1"},
    )

    assert result.returncode != 0
    assert "PortAudio" in result.stdout + result.stderr
    assert not [c for c in result.calls if "enable --now" in c]


def test_macos_readiness_requires_the_capture_helper(repo, stubs):
    (repo / "sck_audio_capture").unlink()
    (stubs[0] / "swiftc").unlink()

    result = run_setup(repo, stubs, *offline_args(), host_os="Darwin")

    assert result.returncode != 0
    assert "sck_audio_capture" in result.stdout + result.stderr
    assert not [c for c in result.calls if c.startswith("launchctl load")]


def test_macos_readiness_requires_the_launcher_application(repo, stubs):
    """The live job starts Recog.app: without it launchd has nothing to run."""
    shutil.rmtree(repo / "Recog.app")

    result = run_setup(repo, stubs, *offline_args(), host_os="Darwin")

    assert result.returncode != 0
    assert "Recog.app" in result.stdout + result.stderr
    assert not [c for c in result.calls if c.startswith("launchctl load")]


@pytest.mark.parametrize("command", ["start", "restart"])
def test_recog_service_activation_runs_the_same_readiness_check(repo, stubs, command):
    """setup.sh is not the only entry point: a direct start/restart must gate
    on the same probes *before* it stops or activates anything — a restart
    that unloads a working service and then fails is the worst outcome."""
    (stubs[0] / "ffmpeg").unlink()
    bin_dir, log, sys_dir = stubs
    env = {
        "HOME": str(repo.parent / "home"),
        "PATH": f"{bin_dir}{os.pathsep}{sys_dir}",
        "STUB_LOG": str(log),
        "STUB_BIN": str(bin_dir),
        "RECOG_HOST_OS": "Linux",
        "RECOG_SERVICE_OS": "Linux",
        "DEPS_PYTHON": sys.executable,
    }
    Path(env["HOME"]).mkdir(exist_ok=True)
    subprocess.run(
        ["bash", str(repo / "recog-service.sh"), "install"],
        capture_output=True, text=True, env=env, check=True,
    )

    log.write_text("")

    result = subprocess.run(
        ["bash", str(repo / "recog-service.sh"), command],
        capture_output=True, text=True, env=env,
    )

    assert result.returncode != 0
    assert "ffmpeg" in result.stdout + result.stderr
    assert log.read_text() == ""


def test_a_second_run_is_an_update_not_a_reinstall(repo, stubs):
    """Re-running setup is how you update after `git pull`."""
    first = run_setup(repo, stubs, *offline_args("--no-services"))
    assert first.returncode == 0, first.stdout + first.stderr
    (repo / ".env").write_text("MINE=1\n")
    stubs[1].write_text("")

    second = run_setup(repo, stubs, *offline_args("--no-services"))

    assert second.returncode == 0, second.stdout + second.stderr
    assert not any("-m venv" in call for call in second.calls)
    assert (repo / ".env").read_text() == "MINE=1\n"


def test_intel_mac_is_rejected_before_anything_is_installed(repo, stubs):
    """given: macOS на x86_64
    when: ./setup.sh
    then: отказ до venv — deploy/python/constraints/macos.txt закрепляет torch 2.8.0, а
          macOS-колёс под x86_64 upstream не публикует с 2.2.2, и установка
          всё равно упёрлась бы в pip"""
    result = run_setup(repo, stubs, *offline_args("--no-services"),
                       host_os="Darwin", env_extra={"RECOG_HOST_ARCH": "x86_64"})

    assert result.returncode != 0
    assert "x86_64" in result.stderr
    assert not any("-m venv" in call for call in result.calls)


# ---------------------------------------------------------------------------
# scripts/prefetch_gigaam.py
# ---------------------------------------------------------------------------


GIGAAM_INIT_SAMPLE = '''
import os
_CACHE_DIR = os.path.expanduser("~/.cache/gigaam")
_URL_DIR = "https://cdn.example.test/GigaAM"
_MODEL_HASHES = {
    "v3_e2e_rnnt": "2730de7545ac43ad256485a462b0a27a",
    "v2_rnnt": "547460139acfebd842323f59ed54ab54",
}
'''


def md5(data: bytes) -> str:
    return hashlib.md5(data).hexdigest()


def test_parses_url_and_hashes_from_the_pinned_gigaam_source():
    """The constants are read with ast rather than imported: importing gigaam
    pulls in torch, which is the one thing prefetch must not do."""
    url_dir, hashes = prefetch_gigaam.parse_gigaam_constants(GIGAAM_INIT_SAMPLE)

    assert url_dir == "https://cdn.example.test/GigaAM"
    assert hashes["v3_e2e_rnnt"] == "2730de7545ac43ad256485a462b0a27a"


def test_parsing_fails_loudly_when_the_constants_moved():
    with pytest.raises(prefetch_gigaam.PrefetchError):
        prefetch_gigaam.parse_gigaam_constants("_SOMETHING_ELSE = 1\n")


def test_file_md5_matches_hashlib(tmp_path):
    path = tmp_path / "blob.bin"
    payload = os.urandom(3 * 1024)
    path.write_bytes(payload)

    assert prefetch_gigaam.file_md5(path, block_size=512) == md5(payload)


def test_a_valid_cached_file_is_not_downloaded_again(tmp_path):
    cached = tmp_path / "v3_e2e_rnnt.ckpt"
    cached.write_bytes(b"weights")

    state = prefetch_gigaam.ensure_file("http://unreachable.invalid/x", cached, md5(b"weights"))

    assert state == "cached"
    assert cached.read_bytes() == b"weights"


def test_a_corrupt_cached_file_is_replaced(tmp_path):
    source = tmp_path / "remote.ckpt"
    source.write_bytes(b"good weights")
    dst = tmp_path / "cache" / "model.ckpt"
    dst.parent.mkdir()
    dst.write_bytes(b"truncated")

    state = prefetch_gigaam.ensure_file(source.as_uri(), dst, md5(b"good weights"))

    assert state == "downloaded"
    assert dst.read_bytes() == b"good weights"


def test_a_checksum_mismatch_leaves_the_cache_untouched(tmp_path):
    """A live pipeline must never see a bad or partial checkpoint: the
    download is published only after it verifies."""
    source = tmp_path / "remote.ckpt"
    source.write_bytes(b"corrupted download")
    dst = tmp_path / "cache" / "model.ckpt"
    dst.parent.mkdir()
    dst.write_bytes(b"previous good weights")

    with pytest.raises(prefetch_gigaam.PrefetchError):
        prefetch_gigaam.ensure_file(source.as_uri(), dst, md5(b"something else"))

    assert dst.read_bytes() == b"previous good weights"
    assert list(dst.parent.iterdir()) == [dst]


class _FakeResponse:
    """Ответ, отдающий `body` блоками и объявляющий `declared` байт.

    Обрыв тела на середине http.client возвращает как b"" — исключения нет
    ("it might break compatibility"), поэтому по одному циклу чтения полная
    загрузка и оборванная неразличимы.
    """

    def __init__(self, body: bytes, declared=None, encoding=None):
        self._body = body
        self._pos = 0
        self.headers = email.message.Message()
        if declared is not None:
            self.headers["Content-Length"] = str(declared)
        if encoding is not None:
            self.headers["Content-Encoding"] = encoding

    def read(self, size):
        block = self._body[self._pos:self._pos + size]
        self._pos += len(block)
        return block

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


def _serve(monkeypatch, response):
    monkeypatch.setattr(
        prefetch_gigaam.urllib.request, "urlopen",
        lambda url, timeout=None: response,
    )


def test_a_truncated_download_is_not_published(tmp_path, monkeypatch):
    """given: сервер объявил 12 байт и оборвал тело на третьем
    when: докачка токенизатора (контрольной суммы у него нет)
    then: PrefetchError, каталог кеша пуст — неполный файл не публикуется и
          не принимается следующими запусками по факту существования"""
    _serve(monkeypatch, _FakeResponse(b"abc", declared=12))
    dst = tmp_path / "cache" / "v3_e2e_rnnt_tokenizer.model"

    with pytest.raises(prefetch_gigaam.PrefetchError):
        prefetch_gigaam.ensure_file("http://cdn.test/tok", dst, None)

    assert not dst.exists()
    assert list(dst.parent.iterdir()) == []


def test_a_complete_download_is_published(tmp_path, monkeypatch):
    _serve(monkeypatch, _FakeResponse(b"tokenizer", declared=9))
    dst = tmp_path / "cache" / "v3_e2e_rnnt_tokenizer.model"

    assert prefetch_gigaam.ensure_file("http://cdn.test/tok", dst, None) == "downloaded"
    assert dst.read_bytes() == b"tokenizer"


def test_a_compressed_body_is_rejected_instead_of_being_published(tmp_path, monkeypatch):
    """given: сервер проигнорировал `Accept-Encoding: identity` и ответил gzip
    when: докачка токенизатора
    then: PrefetchError, ничего не опубликовано. urllib тело не распаковывает,
          так что в кеш легли бы сжатые байты — а токенизатор без контрольной
          суммы принимается следующими запусками по факту существования"""
    _serve(monkeypatch, _FakeResponse(b"\x1f\x8b compressed", encoding="gzip"))
    dst = tmp_path / "cache" / "tok.model"

    with pytest.raises(prefetch_gigaam.PrefetchError):
        prefetch_gigaam.ensure_file("http://cdn.test/tok", dst, None)

    assert not dst.exists()
    assert list(dst.parent.iterdir()) == []


def test_content_length_is_checked_for_every_declared_encoding(tmp_path, monkeypatch):
    """given: тело оборвано, сервер объявил identity-кодирование явно
    when: докачка токенизатора
    then: обрыв пойман — явный `identity` не отключает проверку длины"""
    _serve(monkeypatch, _FakeResponse(b"abc", declared=12, encoding="identity"))
    dst = tmp_path / "cache" / "tok.model"

    with pytest.raises(prefetch_gigaam.PrefetchError):
        prefetch_gigaam.ensure_file("http://cdn.test/tok", dst, None)

    assert not dst.exists()


def test_prefetch_fetches_the_checkpoint_and_the_tokenizer(tmp_path):
    remote = tmp_path / "remote"
    remote.mkdir()
    (remote / "v3_e2e_rnnt.ckpt").write_bytes(b"ckpt")
    (remote / "v3_e2e_rnnt_tokenizer.model").write_bytes(b"tok")
    cache = tmp_path / "cache"

    results = prefetch_gigaam.prefetch(
        "v3_e2e_rnnt", cache, remote.as_uri(), {"v3_e2e_rnnt": md5(b"ckpt")}
    )

    assert results == {
        "v3_e2e_rnnt.ckpt": "downloaded",
        "v3_e2e_rnnt_tokenizer.model": "downloaded",
    }
    assert (cache / "v3_e2e_rnnt.ckpt").read_bytes() == b"ckpt"


def test_prefetch_skips_the_tokenizer_for_models_without_one(tmp_path):
    remote = tmp_path / "remote"
    remote.mkdir()
    (remote / "v2_rnnt.ckpt").write_bytes(b"ckpt")

    results = prefetch_gigaam.prefetch(
        "v2_rnnt", tmp_path / "cache", remote.as_uri(), {"v2_rnnt": md5(b"ckpt")}
    )

    assert list(results) == ["v2_rnnt.ckpt"]


def test_prefetch_rejects_a_model_the_pin_does_not_publish(tmp_path):
    with pytest.raises(prefetch_gigaam.PrefetchError):
        prefetch_gigaam.prefetch("v9_imaginary", tmp_path, "file:///nowhere", {"v2_rnnt": "x"})


def test_prefetch_main_reports_a_failure_without_raising(tmp_path, capsys):
    rc = prefetch_gigaam.main(
        ["--model", "v9_imaginary", "--cache-dir", str(tmp_path),
         "--url-dir", "file:///nowhere"]
    )

    assert rc == 1
    assert "v9_imaginary" in capsys.readouterr().err
