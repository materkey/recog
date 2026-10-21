"""Recog.app: the application the macOS live service runs under."""

import plistlib
from pathlib import Path

import pytest

from recog import macos_bundle, macos_launcher
from recog.macos_launcher import find_or_build_launcher


def _repo(tmp_path):
    path = tmp_path / macos_launcher.LAUNCHER_SOURCE
    path.parent.mkdir(parents=True)
    path.write_text("// source")
    return tmp_path


@pytest.fixture
def toolchain(monkeypatch):
    """swiftc and codesign that write what the real ones would; returns the calls."""
    calls = []

    def fake_run(command, **kwargs):
        calls.append(command)
        if command[0] == "swiftc":
            Path(command[command.index("-o") + 1]).write_text("binary")
        if command[0] == "codesign":
            signature = Path(command[-1]) / "Contents/_CodeSignature"
            signature.mkdir(exist_ok=True)
            (signature / "CodeResources").write_text("seal")
        return type("Result", (), {"returncode": 0, "stderr": ""})()

    monkeypatch.setattr(macos_bundle.shutil, "which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr(macos_bundle.subprocess, "run", fake_run)
    monkeypatch.delenv("RECOG_CODESIGN_IDENTITY", raising=False)
    return calls


def test_no_launcher_when_located_then_a_signed_background_bundle_named_recog_is_built(
    tmp_path, toolchain
):
    repo = _repo(tmp_path)

    app = find_or_build_launcher(repo)

    assert app == repo / "Recog.app"
    assert (app / "Contents/MacOS/recog_launcher").exists()
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    assert info["CFBundleIdentifier"] == "com.recog.live"
    assert info["CFBundleName"] == "Recog"
    assert info["CFBundleExecutable"] == "recog_launcher"
    assert info["LSBackgroundOnly"] is True
    # macOS does not show the microphone dialog for an application without it.
    assert info["NSMicrophoneUsageDescription"]
    sign = toolchain[1]
    assert sign[sign.index("--identifier") + 1] == "com.recog.live"
    assert (repo / "Recog.source-sha256").exists()


def test_built_launcher_when_located_again_then_it_is_not_rebuilt(tmp_path, toolchain):
    repo = _repo(tmp_path)
    find_or_build_launcher(repo)

    find_or_build_launcher(repo)

    assert [command[0] for command in toolchain] == ["swiftc", "codesign"]


def test_launcher_entry_point_prints_the_executable_path(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(macos_launcher, "find_or_build_launcher", lambda: tmp_path / "Recog.app")

    assert macos_launcher.main() == 0

    executable = tmp_path / "Recog.app/Contents/MacOS/recog_launcher"
    assert capsys.readouterr().out.strip() == str(executable)


def test_launcher_entry_point_reports_a_failed_build(monkeypatch, capsys):
    def fail():
        raise RuntimeError("swiftc not found — cannot build Recog.app (xcode-select --install)")

    monkeypatch.setattr(macos_launcher, "find_or_build_launcher", fail)

    assert macos_launcher.main() == 1

    assert "xcode-select --install" in capsys.readouterr().err
