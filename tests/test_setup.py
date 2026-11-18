"""Tests for `setup.sh`, which installs recog without Python.

setup.sh is driven end-to-end against a throwaway copy of the repo with every
external command stubbed on PATH (cargo, the programs it builds, npm, ffmpeg,
systemctl, ...), so the assertions are about *what it decides*: what cargo
builds, what lands in bin/, whether a running service is restarted rather than
started, whether an incomplete install is allowed to touch the service manager
at all. Every Python entry point on PATH logs and fails, and no test may see it
called.

RECOG_HOST_OS / RECOG_SERVICE_OS pin the OS, so the Linux decisions are
covered from a Mac and vice versa.
"""

import os
import plistlib
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent


# ---------------------------------------------------------------------------
# Fake repo + stub commands
# ---------------------------------------------------------------------------

PLAIN_STUB = """#!/bin/sh
printf '%s\\n' "{name} $*" >> "$STUB_LOG"
exit {code}
"""

# Every program cargo builds: logs its arguments, answers the installation's
# questions the way the real `recog` does.
RUST_PROGRAM_STUB = r"""#!/bin/sh
printf '%s\n' "$(basename "$0") $*" >> "$STUB_LOG"
case "$*" in
  "check") [ -n "$STUB_CHECK" ] && { printf '%s\n' "$STUB_CHECK"; exit 1; }; exit 0 ;;
  "config install-runtime") echo "${STUB_PARSED_INSTALL_RUNTIME:-rust}" ;;
  "config data-dir") printf '%s\n' "$HOME/.recog" ;;
  "config conferences") echo '{"jitsi": [], "matrix": []}' ;;
  "launcher") printf '%s\n' "$(dirname "$0")/../Recog.app/Contents/MacOS/recog_launcher" ;;
esac
exit 0
"""

# cargo: logs, and for `build` writes each `--bin` into target/release.
CARGO_STUB = r"""#!/bin/sh
printf '%s\n' "cargo $*" >> "$STUB_LOG"
[ "$1" = "build" ] || exit 0
[ -n "$STUB_CARGO_FAIL" ] && exit 101
mkdir -p target/release
previous=""
for arg in "$@"; do
  if [ "$previous" = "--bin" ]; then
    cp "$STUB_BIN/.rust-program" "target/release/$arg"
    chmod 755 "target/release/$arg"
  fi
  previous="$arg"
done
exit 0
"""

# Logged and failing: the installation must never get here.
NO_PYTHON_STUB = """#!/bin/sh
printf '%s\\n' "python-called $(basename "$0") $*" >> "$STUB_LOG"
exit 1
"""

# Present by default; a test drops one to simulate a missing dependency.
DEFAULT_STUBS = (
    "ffmpeg",
    "parec",
    "pactl",
    "npm",
    "node",
    "tmux",
    "claude",
    "jq",
    "swiftc",
    "launchctl",
    "systemctl",
    "curl",
)

PROGRAMS = {
    "Linux": [
        "recog",
        "recog-copilot-channel",
        "recog-copilot-supervisor",
        "recog-desktop",
        "recog-web",
    ],
    "Darwin": [
        "recog",
        "recog-copilot-channel",
        "recog-copilot-supervisor",
        "recog-desktop",
        "recog-mic",
        "recog-web",
    ],
}

COPY_FILES = (
    "setup.sh",
    "recog-service.sh",
    ".env.example",
)
COPY_TREES = ("scripts", "deploy")


@pytest.fixture
def repo(tmp_path):
    """A throwaway copy of everything setup.sh reads."""
    dst = tmp_path / "recog"
    dst.mkdir()
    for rel in COPY_FILES:
        shutil.copy2(REPO_ROOT / rel, dst / rel)
    for rel in COPY_TREES:
        shutil.copytree(
            REPO_ROOT / rel,
            dst / rel,
            ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
        )
    # A prebuilt macOS helper: the darwin readiness probe wants it executable.
    helper = dst / "sck_audio_capture"
    helper.write_text("#!/bin/sh\nexit 0\n")
    helper.chmod(0o755)
    (dst / "crates/recog-live/macos").mkdir(parents=True, exist_ok=True)
    (dst / "crates/recog-live/macos/sck_audio_capture.swift").write_text("// stub\n")
    # The live job's program: the darwin readiness probe wants it executable.
    launcher = dst / "Recog.app/Contents/MacOS/recog_launcher"
    launcher.parent.mkdir(parents=True)
    launcher.write_text('#!/bin/sh\nexec "$@"\n')
    launcher.chmod(0o755)
    return dst


# Plain utilities setup.sh and the service scripts shell out to. They are
# symlinked into a scratch dir that becomes the *entire* non-stub PATH, so a
# test that drops a stub to simulate a missing dependency cannot accidentally
# find the host's real ffmpeg / swiftc / node instead.
SYS_TOOLS = (
    "bash",
    "sh",
    "sed",
    "grep",
    "awk",
    "head",
    "tail",
    "wc",
    "du",
    "mkdir",
    "cp",
    "mv",
    "rm",
    "ln",
    "cat",
    "dirname",
    "basename",
    "readlink",
    "sleep",
    "tr",
    "uname",
    "sort",
    "date",
    "chmod",
    "git",
)


@pytest.fixture
def stubs(tmp_path):
    """PATH dirs (stubs + plain utilities) plus the log the stubs append to."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    sys_dir = tmp_path / "sysbin"
    sys_dir.mkdir()
    log = tmp_path / "stub.log"

    for name in DEFAULT_STUBS:
        stub = bin_dir / name
        stub.write_text(PLAIN_STUB.format(name=name, code=0))
        stub.chmod(0o755)
    (bin_dir / ".rust-program").write_text(RUST_PROGRAM_STUB)
    (bin_dir / "cargo").write_text(CARGO_STUB)
    (bin_dir / "cargo").chmod(0o755)
    for name in ("python3.12", "python3.13", "python3", "python", "pip"):
        (bin_dir / name).write_text(NO_PYTHON_STUB)
        (bin_dir / name).chmod(0o755)
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
        capture_output=True,
        text=True,
        env=env,
        check=check,
    )
    result.calls = log.read_text().splitlines() if log.exists() else []
    assert not [c for c in result.calls if c.startswith(("python", "pip"))], result.calls
    return result


def offline_args(*extra):
    """Flags that keep a run free of network and service-manager work."""
    return ("--no-prefetch", "--no-extension", *extra)


# ---------------------------------------------------------------------------
# Syntax
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "script",
    [
        "setup.sh",
        "recog-service.sh",
        "scripts/deps.sh",
        "scripts/service/common.sh",
        "scripts/service/launchd.sh",
        "scripts/service/systemd.sh",
    ],
)
def test_shell_scripts_parse(script):
    result = subprocess.run(["bash", "-n", str(REPO_ROOT / script)], capture_output=True, text=True)
    assert result.returncode == 0, result.stderr


# ---------------------------------------------------------------------------
# Building and installing the programs
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("host_os", ["Linux", "Darwin"])
def test_setup_when_it_runs_then_cargo_builds_and_bin_holds_the_programs(repo, stubs, host_os):
    """given: no usable Python on PATH
    when: setup.sh runs end to end
    then: cargo builds the programs in release, they land in bin/, the units
          run them and every service is started
    """
    result = run_setup(repo, stubs, host_os=host_os)

    assert result.returncode == 0, result.stdout + result.stderr
    builds = [c for c in result.calls if c.startswith("cargo build")]
    assert len(builds) == 1
    assert builds[0].startswith("cargo build --release ")
    assert ("-p recog-mic --bin recog-mic" in builds[0]) is (host_os == "Darwin")
    assert sorted(p.name for p in (repo / "bin").iterdir()) == PROGRAMS[host_os]
    assert not (repo / "venv-gigaam").exists()
    if host_os == "Darwin":
        agents = repo.parent / "home/Library/LaunchAgents"
        live = plistlib.loads((agents / "com.recog.live.plist").read_bytes())
        assert live["ProgramArguments"][1:3] == [f"{repo}/bin/recog", "live"]
        assert "recog launcher" in result.calls
        assert len([c for c in result.calls if c.startswith("launchctl load")]) == 4
    else:
        units = repo.parent / "home/.config/systemd/user"
        assert f'ExecStart="{repo}/bin/recog" live' in (units / "recog-live.service").read_text()
        assert len([c for c in result.calls if "enable --now" in c]) == 4
    assert "recog prefetch" in result.calls
    assert f"Programs:  {repo}/bin" in result.stdout


def test_setup_when_it_runs_then_check_precedes_install_and_activation(repo, stubs):
    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode == 0, result.stdout + result.stderr
    calls = result.calls
    build = next(i for i, c in enumerate(calls) if c.startswith("cargo build"))
    check = calls.index("recog check")
    first_enable = next(i for i, c in enumerate(calls) if "enable --now" in c)
    assert build < check < first_enable
    assert "recog prefetch" not in calls


def test_bad_config_when_setup_runs_then_nothing_is_installed(repo, stubs):
    result = run_setup(
        repo, stubs, *offline_args(), env_extra={"STUB_CHECK": "Invalid AI settings: x"}
    )

    assert result.returncode == 1
    assert "Invalid AI settings: x" in result.stderr
    assert "Nothing was installed or restarted" in result.stderr
    assert not (repo / "bin").exists()
    assert not [c for c in result.calls if c.startswith(("systemctl", "launchctl"))]


def test_failed_build_when_setup_runs_then_it_stops(repo, stubs):
    result = run_setup(repo, stubs, *offline_args(), env_extra={"STUB_CARGO_FAIL": "1"})

    assert result.returncode == 1
    assert "cargo build failed" in result.stderr
    assert not (repo / "bin").exists()
    assert not [c for c in result.calls if c.startswith(("systemctl", "launchctl"))]


def test_no_cargo_when_setup_runs_then_it_stops_naming_rustup(repo, stubs):
    (stubs[0] / "cargo").unlink()

    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode == 1
    assert "cargo not found" in result.stderr
    assert "rustup" in result.stderr
    assert not (repo / "bin").exists()


@pytest.mark.parametrize("host_os", ["Linux", "Darwin"])
def test_fresh_install_when_dry_run_then_no_commands_or_files_change(repo, stubs, host_os):
    home = repo.parent / "home"
    home.mkdir()

    def snapshot():
        return {
            str(path.relative_to(repo.parent)): path.read_bytes() if path.is_file() else None
            for root in (repo, home)
            for path in root.rglob("*")
        }

    before = snapshot()
    result = run_setup(repo, stubs, "--dry-run", host_os=host_os)

    assert result.returncode == 0, result.stdout + result.stderr
    assert result.calls == []
    assert snapshot() == before
    assert "Would build with cargo (release)" in result.stdout
    assert f"install the programs into {repo}/bin" in result.stdout
    assert "Would download the ASR model gigaam-v3" in result.stdout
    assert "Would install " + host_os + " service units" in result.stdout
    assert "readiness are not verified" in result.stdout
    assert f"Would copy {repo}/.env.example to {repo}/.env" in result.stdout
    assert result.stdout.count("Would copy") == 1
    assert ("Would build sck_audio_capture" in result.stdout) is (host_os == "Darwin")


def test_skip_flags_when_dry_run_then_plan_reflects_them(repo, stubs):
    result = run_setup(
        repo, stubs, "--dry-run", "--no-services", "--no-env", "--no-prefetch", "--no-extension"
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert result.calls == []
    for flag in ("--no-services", "--no-env", "--no-prefetch", "--no-extension"):
        assert f"({flag})" in result.stdout
    assert "Would install Linux service units" not in result.stdout
    assert "Would copy" not in result.stdout


@pytest.mark.parametrize("value", ["python", "go"])
def test_install_runtime_other_than_rust_when_setup_runs_then_it_stops_before_anything(
    repo, stubs, value
):
    """The Python installation is gone: a config.toml still asking for it, or
    for anything else, stops setup and says what to do."""
    config = repo.parent / "home/.config/recog/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text(f'[install]\nruntime = "{value}"\n')

    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode == 1
    assert "[install] runtime" in result.stderr
    assert result.calls == []


def test_the_extension_when_built_then_npm_runs(repo, stubs):
    (repo / "browser-extension").mkdir()
    (repo / "browser-extension/package-lock.json").write_text("{}")

    result = run_setup(repo, stubs, "--no-prefetch", "--no-services")

    assert result.returncode == 0, result.stdout + result.stderr
    assert "npm ci" in result.calls and "npm run build" in result.calls


# ---------------------------------------------------------------------------
# Config files
# ---------------------------------------------------------------------------


def test_first_install_when_setup_runs_then_the_root_environment_file_is_the_only_one(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert (repo / ".env").read_text() == (repo / ".env.example").read_text()
    assert "web/.env" not in result.stdout + result.stderr
    assert not (repo / "recog").exists()
    assert not (repo.parent / "home/.recog/speakers_ecapa.json").exists()


def test_no_env_when_the_environment_file_is_absent_then_it_is_not_created(repo, stubs):
    result = run_setup(repo, stubs, *offline_args("--no-services", "--no-env"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert not (repo / ".env").exists()


def install_unit(repo, name="recog-live.service"):
    unit_dir = repo.parent / "home/.config/systemd/user"
    unit_dir.mkdir(parents=True, exist_ok=True)
    (unit_dir / name).write_text("[Service]\n")


def test_installed_services_when_the_environment_file_is_absent_then_it_is_not_created(repo, stubs):
    """An installation that runs without an environment file keeps running without it."""
    install_unit(repo)

    result = run_setup(repo, stubs, *offline_args("--no-services"))

    assert result.returncode == 0, result.stdout + result.stderr
    assert not (repo / ".env").exists()
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
# Services: install stages, readiness gates, activation
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("missing", ["claude", "tmux"])
def test_missing_copilot_cli_when_setup_runs_then_setup_warns_and_activates_services(repo, stubs, missing):
    (stubs[0] / missing).unlink()

    result = run_setup(repo, stubs, *offline_args())

    assert result.returncode == 0, result.stdout + result.stderr
    assert any(missing in line and "Copilot sessions" in line and "[absent]" in line for line in result.stdout.splitlines())
    assert len([c for c in result.calls if "enable --now" in c]) == 4


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
        "recog-copilot-channel.service",
        "recog-copilot-supervisor.service",
        "recog-frontend.service",
        "recog-live.service",
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


def test_parser_disagreeing_when_setup_checks_readiness_then_activation_is_blocked(repo, stubs):
    """given: the scripts read "rust", the parser reads another value (.env)"""
    result = run_setup(
        repo, stubs, *offline_args(), env_extra={"STUB_PARSED_INSTALL_RUNTIME": "python"}
    )

    assert result.returncode != 0
    assert (
        '[install] runtime is "python" for recog but "rust" for setup.sh'
        in result.stdout + result.stderr
    )
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
    (stubs[0] / "swiftc").unlink()

    result = run_setup(repo, stubs, *offline_args(), host_os="Darwin")

    assert result.returncode != 0
    assert "Recog.app" in result.stdout + result.stderr
    assert not [c for c in result.calls if c.startswith("launchctl load")]


@pytest.mark.parametrize("command", ["start", "restart"])
def test_recog_service_activation_runs_the_same_readiness_check(repo, stubs, command):
    """setup.sh is not the only entry point: a direct start/restart must gate
    on the same probes *before* it stops or activates anything — a restart
    that unloads a working service and then fails is the worst outcome."""
    first = run_setup(repo, stubs, *offline_args("--no-services"))
    assert first.returncode == 0, first.stdout + first.stderr
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
    subprocess.run(
        ["bash", str(repo / "recog-service.sh"), "install"],
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )

    log.write_text("")

    result = subprocess.run(
        ["bash", str(repo / "recog-service.sh"), command],
        capture_output=True,
        text=True,
        env=env,
    )

    assert result.returncode != 0
    assert "ffmpeg" in result.stdout + result.stderr
    assert not [
        c for c in log.read_text().splitlines() if c.startswith(("systemctl", "python", "pip"))
    ]


def test_a_second_run_is_an_update_not_a_reinstall(repo, stubs):
    """Re-running setup is how you update after `git pull`: the programs are
    replaced, the environment files kept."""
    first = run_setup(repo, stubs, *offline_args("--no-services"))
    assert first.returncode == 0, first.stdout + first.stderr
    (repo / ".env").write_text("MINE=1\n")
    inode = (repo / "bin/recog").stat().st_ino

    second = run_setup(repo, stubs, *offline_args("--no-services"))

    assert second.returncode == 0, second.stdout + second.stderr
    assert (repo / ".env").read_text() == "MINE=1\n"
    assert (repo / "bin/recog").stat().st_ino != inode
