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
import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

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

COPY_FILES = (
    "setup.sh",
    "recog-service.sh",
    "requirements.txt",
    "requirements-macos.txt",
    ".env.example",
    "speakers_ecapa.json.example",
)
COPY_TREES = ("scripts", "deploy", "constraints", "recog")


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
    (dst / "frontend").mkdir()
    shutil.copy2(REPO_ROOT / "frontend/requirements.txt", dst / "frontend/requirements.txt")
    shutil.copy2(REPO_ROOT / "frontend/.env.example", dst / "frontend/.env.example")
    # A prebuilt macOS helper: the darwin readiness probe wants it executable.
    helper = dst / "sck_audio_capture"
    helper.write_text("#!/bin/sh\nexit 0\n")
    helper.chmod(0o755)
    (dst / "sck_audio_capture.swift").write_text("// stub\n")
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
        "RECOG_HOST_OS": host_os,
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


def test_creates_the_venv_and_installs_the_pinned_gigaam(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert any("-m venv" in call for call in result.calls)
    assert any(
        "gigaam @ git+https://github.com/salute-developers/GigaAM.git@6e4b027" in call
        for call in pip_calls(result)
    ), pip_calls(result)
    assert any("requirements.txt" in call for call in pip_calls(result))


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
    (stubs[0] / "nvidia-smi").unlink(missing_ok=True)

    result = run_setup(repo, stubs, *offline_args("--no-services"))

    torch_call = next(c for c in pip_calls(result) if " torch " in f" {c} ")
    assert "--index-url https://download.pytorch.org/whl/cpu" in torch_call


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
    stage on darwin runs under constraints/macos.txt."""
    result = run_setup(repo, stubs, *offline_args("--no-services"), host_os="Darwin")

    installs = [c for c in pip_calls(result) if c.startswith("pip install")]
    assert installs
    for call in installs:
        assert "-c " in call and "constraints/macos.txt" in call, call
    assert any("requirements-macos.txt" in call for call in installs)


def test_linux_does_not_apply_the_macos_constraints(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services"))

    for call in pip_calls(result):
        assert "constraints/macos.txt" not in call
    assert not any("requirements-macos.txt" in call for call in pip_calls(result))


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
    assert (repo / "frontend/.env").is_file()
    assert (repo / "speakers_ecapa.json").read_text() == \
        (repo / "speakers_ecapa.json.example").read_text()


def test_existing_config_files_are_never_overwritten(repo, stubs):
    (repo / ".env").write_text("RECOG_ASR_BACKEND=remote\nMINE=1\n")

    run_setup(repo, stubs, *offline_args("--no-services"))

    assert (repo / ".env").read_text() == "RECOG_ASR_BACKEND=remote\nMINE=1\n"


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
