"""Service-layer tests: template rendering and the install/start lifecycle.

`recog-service.sh render --dry-run <dir>` renders the units of one backend
without touching the service manager, and RECOG_SERVICE_OS selects that backend
independently of the host — so both the launchd and the systemd units are
covered from either OS. The lifecycle tests drive the real script against stub
`launchctl` / `systemctl` binaries on PATH.

The scripts run from a copy of the checkout whose bin/ holds stub programs:
rendering asks `bin/recog config data-dir`, which the stub answers as the real
binary does (RECOG_DATA_DIR, else ~/.recog).
"""

import atexit
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

SOURCE_ROOT = Path(__file__).resolve().parent.parent
PROGRAM_STUB = """#!/bin/sh
if [ -n "$STUB_LOG" ]; then printf '%s\\n' "$(basename "$0") $*" >> "$STUB_LOG"; fi
if [ "$*" = "config data-dir" ]; then printf '%s\\n' "${RECOG_DATA_DIR:-$HOME/.recog}"; fi
exit 0
"""
RUST_PROGRAMS = (
    "recog",
    "recog-web",
    "recog-copilot-supervisor",
    "recog-copilot-channel",
    "recog-desktop",
    "recog-mic",
)


def make_checkout(repo):
    """A copy of what the service scripts read, with stub programs in bin/."""
    repo.mkdir(parents=True)
    shutil.copy2(SOURCE_ROOT / "recog-service.sh", repo / "recog-service.sh")
    for tree in ("scripts", "deploy"):
        shutil.copytree(SOURCE_ROOT / tree, repo / tree)
    (repo / "bin").mkdir()
    for name in RUST_PROGRAMS:
        (repo / "bin" / name).write_text(PROGRAM_STUB)
        (repo / "bin" / name).chmod(0o755)
    return repo


_CHECKOUT_DIR = tempfile.mkdtemp(prefix="recog-units-")
atexit.register(shutil.rmtree, _CHECKOUT_DIR, True)
REPO_ROOT = make_checkout(Path(_CHECKOUT_DIR).resolve() / "recog")
SERVICE_SH = REPO_ROOT / "recog-service.sh"

SERVICES = ("live", "frontend", "copilot-channel", "copilot-supervisor")
UNIT_FILES = {
    "Darwin": [f"com.recog.{name}.plist" for name in SERVICES],
    "Linux": [f"recog-{name}.service" for name in SERVICES],
}
PLACEHOLDER_RE = re.compile(r"@[A-Z_]+@")


def run_service_sh(*args, env_extra=None, check=True, script=SERVICE_SH):
    env = {**os.environ}
    env.pop("XDG_CONFIG_HOME", None)
    # The developer's own config.toml must not decide anything here.
    env["RECOG_CONFIG_FILE"] = "/nonexistent/recog/config.toml"
    env.pop("RECOG_INSTALL_RUNTIME", None)
    env.update(env_extra or {})
    return subprocess.run(
        ["bash", str(script), *args],
        capture_output=True,
        text=True,
        check=check,
        env=env,
    )


def render(target_os, out_dir):
    run_service_sh(
        "render",
        "--dry-run",
        str(out_dir),
        env_extra={"RECOG_SERVICE_OS": target_os},
    )
    return {path.name: path for path in sorted(Path(out_dir).iterdir())}


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_both_renderers_when_given_the_same_installation_then_unit_bytes_match(tmp_path, target_os):
    target = Path(os.environ.get("CARGO_TARGET_DIR") or "target")
    built = SOURCE_ROOT / target / "debug/recog"
    if not built.is_file():
        pytest.skip("needs cargo build -p recog-live")
    repo = make_checkout(tmp_path / "Recog & tools")
    (repo / "crates/recog-live").mkdir(parents=True)
    (repo / "crates/recog-live/Cargo.toml").touch()
    program = repo / "bin/recog"
    shutil.copy2(built, program)
    env = {
        "HOME": str(tmp_path), "PATH": os.environ["PATH"],
        "RECOG_SERVICE_OS": target_os, "RECOG_DATA_DIR": str(tmp_path / "Meeting & data"),
        "RECOG_CONFIG_FILE": str(tmp_path / "absent.toml"),
    }
    shell, rust = tmp_path / "shell", tmp_path / "rust"
    subprocess.run([repo / "recog-service.sh", "render", "--dry-run", shell], env=env, check=True)
    subprocess.run([program, "service", "render", rust], env=env, check=True, cwd=tmp_path)
    assert sorted(p.name for p in rust.iterdir()) == sorted(UNIT_FILES[target_os])
    for name in UNIT_FILES[target_os]:
        assert (rust / name).read_bytes() == (shell / name).read_bytes()


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_custom_cli_directories_when_rendering_then_service_path_can_run_copilot(
    target_os, tmp_path
):
    cli_dirs = [tmp_path / "npm prefix & tools/bin", tmp_path / "tmux/bin"]
    for name, directory in zip(("claude", "tmux"), cli_dirs):
        directory.mkdir(parents=True)
        cli = directory / name
        cli.write_text(f"#!/bin/sh\necho {name}-ready\n")
        cli.chmod(0o755)
    out = tmp_path / "units"
    run_service_sh(
        "render",
        "--dry-run",
        str(out),
        env_extra={
            "RECOG_SERVICE_OS": target_os,
            "PATH": os.pathsep.join(map(str, cli_dirs)) + os.pathsep + os.environ["PATH"],
        },
    )
    if target_os == "Darwin":
        unit = plistlib.loads((out / "com.recog.copilot-supervisor.plist").read_bytes())
        service_path = unit["EnvironmentVariables"]["PATH"]
    else:
        text = (out / "recog-copilot-supervisor.service").read_text()
        service_path = re.search(r'^Environment="PATH=(.*)"$', text, re.M).group(1)
    for name in ("claude", "tmux"):
        result = subprocess.run(
            [name], env={"PATH": service_path}, capture_output=True, text=True, check=True
        )
        assert result.stdout.strip() == f"{name}-ready"


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_custom_data_root_when_rendering_then_env_and_logs_share_it(target_os, tmp_path):
    """Сервис не должен читать один архив, пока CLI и логи используют другой."""
    data = tmp_path / "My data & archive"
    out = tmp_path / "out"
    run_service_sh("render", "--dry-run", str(out), env_extra={
        "RECOG_SERVICE_OS": target_os, "RECOG_DATA_DIR": str(data),
    })
    for path in out.iterdir():
        if target_os == "Darwin":
            unit = plistlib.loads(path.read_bytes())
            assert unit["EnvironmentVariables"]["RECOG_DATA_DIR"] == str(data)
            assert Path(unit["StandardOutPath"]).parent == data / "logs"
            assert Path(unit["StandardErrorPath"]).parent == data / "logs"
        else:
            content = path.read_text()
            assert f'Environment="RECOG_DATA_DIR={data}"' in content
            assert f"StandardOutput=append:{data}/logs/" in content
            assert f"StandardError=append:{data}/logs/" in content


def stub_bin(tmp_path, names=("launchctl", "systemctl", "tmux")):
    """PATH dir of logging stubs. Every invocation is appended to stub.log as
    `<name> <args>`; tmux exits 1 so `has-session` reads as "session gone"."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    log = tmp_path / "stub.log"
    for name in names:
        stub = bin_dir / name
        exit_code = 1 if name == "tmux" else 0
        stub.write_text(
            "#!/bin/sh\n"
            f'printf "%s\\n" "{name} $*" >> "$STUB_LOG"\n'
            f"exit {exit_code}\n"
        )
        stub.chmod(0o755)
    return bin_dir, log


def stub_env(tmp_path):
    bin_dir, log = stub_bin(tmp_path)
    home = tmp_path / "home"
    home.mkdir()
    env = {
        "HOME": str(home),
        "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
        "STUB_LOG": str(log),
        # These tests drive the service verbs, not the host: the readiness gate
        # start/restart run (scripts/deps.sh) is covered in tests/test_setup.py
        # against controlled stubs, and would otherwise make the outcome depend
        # on whether the developer's box has ffmpeg/parec and a filled-in .env.
        "RECOG_SKIP_READINESS": "1",
    }
    return env, log, home


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_render_produces_every_unit_with_no_placeholders_left(target_os, tmp_path):
    """given: the templates of one backend
    when: they are rendered
    then: all four units appear and no @TOKEN@ survives — an unsubstituted
          placeholder would reach the service manager as a literal path
    """
    units = render(target_os, tmp_path / "out")

    assert sorted(units) == sorted(UNIT_FILES[target_os])
    for name, path in units.items():
        assert not PLACEHOLDER_RE.search(path.read_text()), name


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_rendered_units_point_at_this_repo(target_os, tmp_path):
    units = render(target_os, tmp_path / "out")

    for path in units.values():
        assert str(REPO_ROOT) in path.read_text()


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_live_unit_selects_the_system_audio_backend_automatically(target_os, tmp_path):
    """The live unit must not hardcode a macOS-only capture flag: `auto` is
    what makes the same unit work on ScreenCaptureKit and PulseAudio."""
    units = render(target_os, tmp_path / "out")
    live = units[UNIT_FILES[target_os][0]].read_text()

    assert "--system-audio" in live
    assert "auto" in live
    assert "--ktalk" in live
    assert "--screencapture" not in live


def test_systemd_start_limit_lives_in_the_unit_section(tmp_path):
    """given: the systemd units
    when: StartLimitIntervalSec is placed
    then: it is in [Unit] — systemd ignores the key in [Service] with an
          "Unknown key" warning that `systemd-analyze verify` still exits 0 on
    """
    units = render("Linux", tmp_path / "out")

    for path in units.values():
        # split on the section header line, not the token: the [Unit] comment
        # mentions "[Service]" inline
        unit_section, service_section = path.read_text().split("\n[Service]\n")
        assert "StartLimitIntervalSec=0" in unit_section
        assert "StartLimitIntervalSec" not in service_section


def test_systemd_restart_policy_matches_launchd_keepalive(tmp_path):
    """frontend exits cleanly on purpose (launchd KeepAlive.SuccessfulExit
    false); the daemons must come back from any exit."""
    units = render("Linux", tmp_path / "out")

    assert "Restart=on-failure" in units["recog-frontend.service"].read_text()
    for name in ("recog-live", "recog-copilot-channel", "recog-copilot-supervisor"):
        text = units[f"{name}.service"].read_text()
        assert "Restart=always" in text
        assert "RestartSec=5" in text


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_render_survives_a_home_with_a_space_and_an_xml_character(target_os, tmp_path):
    """given: $HOME containing a space and `&`
    when: the units are rendered
    then: the plist is still well-formed XML and the systemd values stay a
          single token — an unescaped path used to produce a unit the service
          manager refuses while install reported success
    """
    home = tmp_path / "My Home & Co"
    home.mkdir()
    out = tmp_path / "out"
    run_service_sh(
        "render", "--dry-run", str(out),
        env_extra={"RECOG_SERVICE_OS": target_os, "HOME": str(home)},
    )
    units = {path.name: path for path in sorted(out.iterdir())}

    if target_os == "Darwin":
        import xml.etree.ElementTree as ET

        for name, path in units.items():
            ET.parse(path)  # raises ParseError on a stray & or <
            assert "&amp;" in path.read_text(), name
    else:
        for name, path in units.items():
            text = path.read_text()
            assert f'Environment="HOME={home}"' in text, name
            exec_start = next(
                line for line in text.splitlines() if line.startswith("ExecStart=")
            )
            assert exec_start.startswith('ExecStart="'), name
            path_line = next(
                line for line in text.splitlines() if line.startswith('Environment="PATH=')
            )
            assert path_line.endswith('"'), name


def test_render_rejects_a_template_with_an_unknown_placeholder(tmp_path):
    """given: a template carrying a placeholder the renderer does not know
    when: it is rendered
    then: rendering fails and leaves no output file behind
    """
    src = tmp_path / "broken.in"
    src.write_text("ExecStart=@PYTHON@ @NOT_A_REAL_TOKEN@\n")
    dst = tmp_path / "broken.out"

    result = subprocess.run(
        [
            "bash",
            "-c",
            f'SVC_REPO_DIR="{REPO_ROOT}" source "{REPO_ROOT}/scripts/service/common.sh" '
            f'&& svc_render_template "{src}" "{dst}"',
        ],
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert "@NOT_A_REAL_TOKEN@" in result.stderr
    assert not dst.exists()


def test_macos_live_unit_starts_the_binary_through_the_recog_application(tmp_path):
    """macOS attributes Screen Recording and the microphone to the program the
    job starts: the permissions belong to "Recog", whatever it runs."""
    units = render("Darwin", tmp_path / "out")
    live = plistlib.loads(units["com.recog.live.plist"].read_bytes())

    program = live["ProgramArguments"]
    assert program[0] == f"{REPO_ROOT}/Recog.app/Contents/MacOS/recog_launcher"
    assert program[1:] == [f"{REPO_ROOT}/bin/recog", "live", "--system-audio", "auto", "--ktalk"]


@pytest.mark.skipif(
    sys.platform != "darwin" or shutil.which("plutil") is None,
    reason="needs macOS plutil (Swift on Linux ships a plutil that cannot read files)",
)
def test_rendered_plists_are_valid_property_lists(tmp_path):
    units = render("Darwin", tmp_path / "out")

    for path in units.values():
        result = subprocess.run(
            ["plutil", "-lint", str(path)], capture_output=True, text=True
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert "OK" in result.stdout


@pytest.mark.skipif(
    shutil.which("systemd-analyze") is None, reason="systemd-analyze not installed"
)
def test_rendered_systemd_units_have_no_unknown_keys(tmp_path):
    """`systemd-analyze verify` exits 0 even when it reports an unknown key,
    so the assertion is on the output, not the return code."""
    units = render("Linux", tmp_path / "out")

    for path in units.values():
        result = subprocess.run(
            ["systemd-analyze", "--user", "verify", str(path)],
            capture_output=True,
            text=True,
        )
        combined = result.stdout + result.stderr
        assert "Unknown key" not in combined, combined


# ---------------------------------------------------------------------------
# Lifecycle: install stages the units, start activates them
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "target_os,unit_dir",
    [("Darwin", "Library/LaunchAgents"), ("Linux", ".config/systemd/user")],
)
def test_install_stages_units_without_activating_them(target_os, unit_dir, tmp_path):
    """given: a machine with no services installed
    when: `install` runs
    then: every unit file lands in the manager's directory but nothing is
          loaded / enabled / started — activation waits for the readiness check
    """
    env, log, home = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = target_os

    run_service_sh("install", env_extra=env)

    for name in UNIT_FILES[target_os]:
        assert (home / unit_dir / name).is_file()

    calls = log.read_text().splitlines() if log.exists() else []
    assert not [c for c in calls if c.startswith("launchctl load")]
    assert not [c for c in calls if "enable --now" in c]
    assert not [c for c in calls if re.search(r"\bstart\b", c)]
    if target_os == "Linux":
        # a reload is a pure cache refresh, not an activation
        assert "systemctl --user daemon-reload" in calls


def test_launchd_start_loads_every_installed_agent(tmp_path):
    env, log, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Darwin"

    run_service_sh("install", env_extra=env)
    log.write_text("")
    run_service_sh("start", env_extra=env)

    calls = log.read_text()
    for name in SERVICES:
        assert f"load {env['HOME']}/Library/LaunchAgents/com.recog.{name}.plist" in calls


def test_launchd_start_reloads_a_loaded_but_stopped_agent(tmp_path):
    """`launchctl list` печатает и загруженный, но не запущенный job (PID `-`)
    — так выглядит frontend после чистого выхода при KeepAlive. Ранний выход
    по «уже загружен» делал `start` бессильным поднять его, а setup.sh после
    git pull рапортовал «Started» на плист, который launchd не перечитывал."""
    env, log, home = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Darwin"

    run_service_sh("install", env_extra=env)

    launchctl = Path(env["PATH"].split(os.pathsep)[0]) / "launchctl"
    launchctl.write_text(
        "#!/bin/sh\n"
        'printf "%s\\n" "launchctl $*" >> "$STUB_LOG"\n'
        'if [ "$1" = "list" ]; then\n'
        "  for n in " + " ".join(SERVICES) + "; do\n"
        '    printf -- "-\\t0\\tcom.recog.$n\\n"\n'
        "  done\n"
        "fi\n"
        "exit 0\n"
    )
    launchctl.chmod(0o755)

    log.write_text("")
    run_service_sh("start", env_extra=env)

    calls = log.read_text().splitlines()
    for name in SERVICES:
        plist = f"{home}/Library/LaunchAgents/com.recog.{name}.plist"
        assert f"launchctl unload {plist}" in calls
        assert calls.index(f"launchctl unload {plist}") < calls.index(
            f"launchctl load {plist}"
        )


def test_systemd_start_enables_and_starts_every_installed_unit(tmp_path):
    env, log, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Linux"

    run_service_sh("install", env_extra=env)
    log.write_text("")
    run_service_sh("start", env_extra=env)

    calls = log.read_text()
    for name in SERVICES:
        assert f"systemctl --user enable --now recog-{name}.service" in calls


def test_systemd_restart_restarts_instead_of_reloading(tmp_path):
    """A daemon-reload does not restart a running unit and `start` on an
    active one is a no-op — only `restart` re-reads code and environment."""
    env, log, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Linux"

    run_service_sh("install", env_extra=env)
    log.write_text("")
    run_service_sh("restart", env_extra=env)

    calls = log.read_text()
    for name in SERVICES:
        assert f"systemctl --user restart recog-{name}.service" in calls
    assert "enable --now" not in calls


def test_launchd_restart_unloads_before_loading(tmp_path):
    """`launchctl start` reuses the cached plist, so PATH / ProgramArguments
    edits only take effect through unload + load."""
    env, log, home = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Darwin"

    run_service_sh("install", env_extra=env)
    log.write_text("")
    run_service_sh("restart-live", env_extra=env)

    calls = log.read_text().splitlines()
    plist = f"{home}/Library/LaunchAgents/com.recog.live.plist"
    assert calls.index(f"launchctl unload {plist}") < calls.index(
        f"launchctl load {plist}"
    )


def test_start_without_install_fails_loudly(tmp_path):
    env, _, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Linux"

    result = run_service_sh("start", env_extra=env, check=False)

    assert result.returncode != 0
    assert "install" in result.stdout


def test_restart_keeps_the_copilot_session_alive(tmp_path):
    """A mid-meeting restart must not reap the copilot session: the supervisor
    re-adopts it on reconnect. Only stop/uninstall clean it up."""
    env, log, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Linux"

    run_service_sh("install", env_extra=env)
    log.write_text("")
    run_service_sh("restart", env_extra=env)

    calls = log.read_text()
    assert "kill-session" not in calls
    assert "recog-copilot-session.service" not in calls


def test_stop_reaps_the_linux_transient_copilot_unit(tmp_path):
    """given: Linux, where the copilot tmux session runs in its own transient
             unit outside the supervisor's cgroup
    when: `stop` runs
    then: that unit is stopped too — stopping the supervisor alone would leave
          the Copilot running with nothing left to kill it
    """
    env, log, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Linux"

    run_service_sh("install", env_extra=env)
    log.write_text("")
    run_service_sh("stop", env_extra=env)

    assert "systemctl --user stop recog-copilot-session.service" in log.read_text()


def test_linux_status_hints_at_lingering(tmp_path):
    """`systemd --user` units die at logout unless lingering is enabled — the
    single most common "it stopped by itself" report on Linux."""
    env, _, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Linux"

    result = run_service_sh("status", env_extra=env)

    assert "enable-linger" in result.stdout


def test_macos_status_has_no_linger_hint(tmp_path):
    env, _, _ = stub_env(tmp_path)
    env["RECOG_SERVICE_OS"] = "Darwin"

    result = run_service_sh("status", env_extra=env)

    assert "enable-linger" not in result.stdout


def test_render_requires_the_dry_run_flag_and_a_directory(tmp_path):
    for args in (["render"], ["render", str(tmp_path / "out")], ["render", "--dry-run"]):
        result = run_service_sh(*args, check=False)
        assert result.returncode != 0, args


def test_unsupported_os_is_rejected():
    result = run_service_sh(
        "status", env_extra={"RECOG_SERVICE_OS": "Plan9"}, check=False
    )

    assert result.returncode != 0
    assert "Unsupported OS" in result.stderr


# ---------------------------------------------------------------------------
# The commands of the units
# ---------------------------------------------------------------------------


@pytest.fixture
def rust_checkout(tmp_path):
    """A copy of what rendering reads, with stub programs in bin/; its path has a
    space and `&`, which each unit format must keep in one argument. `recog config
    data-dir` answers as the real binary does; a python on PATH would log."""
    repo = tmp_path / "check & out"
    repo.mkdir()
    shutil.copy2(SOURCE_ROOT / "recog-service.sh", repo / "recog-service.sh")
    for tree in ("scripts", "deploy"):
        shutil.copytree(SOURCE_ROOT / tree, repo / tree)
    bin_dir = repo / "bin"
    bin_dir.mkdir()
    for name in RUST_PROGRAMS:
        (bin_dir / name).write_text(
            "#!/bin/sh\n"
            'printf "%s\\n" "$(basename "$0") $*" >> "$STUB_LOG"\n'
            'if [ "$*" = "config data-dir" ]; then printf "%s\\n" "$TEST_DATA_DIR"; fi\n'
        )
        (bin_dir / name).chmod(0o755)
    stubs = tmp_path / "stubs"
    stubs.mkdir()
    for name in ("python", "python3", "python3.12"):
        (stubs / name).write_text('#!/bin/sh\nprintf "%s\\n" "python $*" >> "$STUB_LOG"\nexit 1\n')
        (stubs / name).chmod(0o755)
    log = tmp_path / "stub.log"
    env = {
        "RECOG_INSTALL_RUNTIME": "rust",
        "TEST_DATA_DIR": str(tmp_path / "data"),
        "STUB_LOG": str(log),
        "PATH": f"{stubs}{os.pathsep}{os.environ['PATH']}",
    }
    return repo, env, log


def render_rust(repo, env, target_os, out):
    run_service_sh(
        "render",
        "--dry-run",
        str(out),
        env_extra={**env, "RECOG_SERVICE_OS": target_os},
        script=repo / "recog-service.sh",
    )
    return {path.name: path for path in sorted(Path(out).iterdir())}


def test_installation_without_python_when_rendering_launchd_then_units_run_the_binaries(
    rust_checkout, tmp_path
):
    """given: [install] runtime = "rust" and the programs in bin/
    when: the launchd agents are rendered
    then: each runs its binary with the checkout named, the live one through
          Recog.app so the permissions stay with "Recog", and nothing asks Python
    """
    repo, env, log = rust_checkout

    units = render_rust(repo, env, "Darwin", tmp_path / "out")

    def program(name):
        return plistlib.loads(units[f"com.recog.{name}.plist"].read_bytes())["ProgramArguments"]

    bin_dir = f"{repo}/bin"
    assert program("live") == [
        f"{repo}/Recog.app/Contents/MacOS/recog_launcher",
        f"{bin_dir}/recog",
        "live",
        "--system-audio",
        "auto",
        "--ktalk",
    ]
    assert program("frontend") == [
        f"{bin_dir}/recog-web",
        "--host",
        "127.0.0.1",
        "--port",
        "9080",
        "--repo",
        str(repo),
    ]
    assert program("copilot-channel") == [
        f"{bin_dir}/recog-copilot-channel",
        "--repo-root",
        str(repo),
    ]
    assert program("copilot-supervisor") == [
        f"{bin_dir}/recog-copilot-supervisor",
        "--repo-root",
        str(repo),
    ]
    data = plistlib.loads(units["com.recog.live.plist"].read_bytes())["EnvironmentVariables"]
    assert data["RECOG_DATA_DIR"] == env["TEST_DATA_DIR"]
    calls = log.read_text().splitlines()
    assert calls and not [c for c in calls if c.startswith("python")], calls


def test_installation_without_python_when_rendering_systemd_then_exec_start_runs_the_binaries(
    rust_checkout, tmp_path
):
    repo, env, log = rust_checkout

    units = render_rust(repo, env, "Linux", tmp_path / "out")

    def exec_start(name):
        text = units[f"recog-{name}.service"].read_text()
        return next(line for line in text.splitlines() if line.startswith("ExecStart="))

    assert exec_start("live") == f'ExecStart="{repo}/bin/recog" live --system-audio auto --ktalk'
    assert exec_start("frontend") == (
        f'ExecStart="{repo}/bin/recog-web" --host 127.0.0.1 --port 9080 --repo "{repo}"'
    )
    assert exec_start("copilot-channel") == (
        f'ExecStart="{repo}/bin/recog-copilot-channel" --repo-root "{repo}"'
    )
    assert exec_start("copilot-supervisor") == (
        f'ExecStart="{repo}/bin/recog-copilot-supervisor" --repo-root "{repo}"'
    )
    for path in units.values():
        assert not PLACEHOLDER_RE.search(path.read_text()), path.name
    assert not [c for c in log.read_text().splitlines() if c.startswith("python")]


@pytest.mark.parametrize("target_os", ["Darwin", "Linux"])
def test_installation_without_python_and_no_binary_when_rendering_then_it_fails_naming_setup(
    rust_checkout, target_os, tmp_path
):
    repo, env, _ = rust_checkout
    (repo / "bin/recog").unlink()

    result = run_service_sh(
        "render",
        "--dry-run",
        str(tmp_path / "out"),
        env_extra={**env, "RECOG_SERVICE_OS": target_os},
        script=repo / "recog-service.sh",
        check=False,
    )

    assert result.returncode != 0
    assert "bin/recog is not installed" in result.stderr
    assert "./setup.sh" in result.stderr


def test_install_runtime_in_config_file_when_rendering_then_it_decides(rust_checkout, tmp_path):
    """The scripts read [install] runtime from config.toml themselves."""
    repo, env, _ = rust_checkout
    config = tmp_path / "config.toml"
    config.write_text(
        '[asr]\nbackend = "wavo"\n\n[ install ]  # without Python\nruntime = "rust"  # binaries\n'
    )
    env = {**env, "RECOG_INSTALL_RUNTIME": "", "RECOG_CONFIG_FILE": str(config)}

    units = render_rust(repo, env, "Linux", tmp_path / "out")

    assert f'ExecStart="{repo}/bin/recog" live' in units["recog-live.service"].read_text()
