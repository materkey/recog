"""Service-layer tests: template rendering and the install/start lifecycle.

`recog-service.sh render --dry-run <dir>` renders the units of one backend
without touching the service manager, and RECOG_SERVICE_OS selects that backend
independently of the host — so both the launchd and the systemd units are
covered from either OS. The lifecycle tests drive the real script against stub
`launchctl` / `systemctl` binaries on PATH.
"""
import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
SERVICE_SH = REPO_ROOT / "recog-service.sh"

SERVICES = ("live", "frontend", "copilot-channel", "copilot-supervisor")
UNIT_FILES = {
    "Darwin": [f"com.recog.{name}.plist" for name in SERVICES],
    "Linux": [f"recog-{name}.service" for name in SERVICES],
}
PLACEHOLDER_RE = re.compile(r"@[A-Z_]+@")


def run_service_sh(*args, env_extra=None, check=True):
    env = {**os.environ}
    env.pop("XDG_CONFIG_HOME", None)
    env.update(env_extra or {})
    return subprocess.run(
        ["bash", str(SERVICE_SH), *args],
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


@pytest.mark.skipif(shutil.which("plutil") is None, reason="plutil is macOS-only")
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
