"""Smoke-test the installed disk-image runtime using only a temporary user directory."""

import io
import os
import socket
import subprocess
import time
import urllib.error
import urllib.request
import wave


def check_playback(runtime, env, home):
    meeting = home / "data/meetings/dmg-check"
    meeting.mkdir(parents=True)
    (meeting / "meta.json").write_text('{"id":"dmg-check","created_at":"2026-01-01T12:00:00Z"}')
    (meeting / "transcript.jsonl").write_text(
        '{"text":"test","is_local":true,"audio_offset_ms":100}\n'
    )
    with wave.open(str(meeting / "audio_mic.wav"), "wb") as wav:
        wav.setparams((1, 2, 8000, 0, "NONE", "not compressed"))
        wav.writeframes(b"\x00\x20" * 8000)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    # Match a launchd environment that doesn't supply Homebrew or the payload bin.
    with subprocess.Popen(
        [
            runtime / "bin/recog-web",
            "--repo",
            runtime,
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
        ],
        env=env,
    ) as web:
        try:
            url = f"http://127.0.0.1:{port}/api/meetings/dmg-check/audio/0"
            for _ in range(50):
                try:
                    with urllib.request.urlopen(url, timeout=5) as response:
                        audio = response.read()
                    break
                except urllib.error.HTTPError:
                    raise
                except urllib.error.URLError:
                    if web.poll() is not None:
                        raise RuntimeError("Recog web exited before serving the audio") from None
                    time.sleep(0.2)
            else:
                raise RuntimeError("Recog web did not start")
            with wave.open(io.BytesIO(audio)) as result:
                if result.getparams()[:3] != (1, 2, 16000) or not any(result.readframes(16000)):
                    raise ValueError(
                        "Bundled audio playback did not return non-silent 16 kHz mono audio"
                    )
        finally:
            web.terminate()
            try:
                web.wait(timeout=8)
            except subprocess.TimeoutExpired:
                web.kill()


def check_install(payload, home):
    home.mkdir()
    runtime = home / ".local/share/recog/runtime"
    tools = home / "test-path"
    tools.mkdir()
    for name in ("brew", "gh", "swiftc", "ffmpeg"):
        stub = tools / name
        stub.write_text('#!/bin/sh\nprintf "%s\\n" "$0" >> "$HOME/unexpected-tool"\nexit 79\n')
        stub.chmod(0o755)
    env = {
        "HOME": str(home),
        "PATH": f"{tools}:/usr/bin:/bin:/usr/sbin:/sbin",
        "RECOG_CONFIG_FILE": str(home / "config.toml"),
        "RECOG_DATA_DIR": str(home / "data"),
        "RECOG_BRIDGE_WS_URL": "ws://127.0.0.1:1",
        "SVC_UNIT_DIR": str(home / "units"),
        "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
    }
    subprocess.run(
        [payload / "bin/recog", "setup", "--no-services", "--no-prefetch", "--no-env"],
        env={**env, "PATH": f"{runtime}/bin:{env['PATH']}"},
        check=True,
    )
    check_playback(runtime, env, home)
    if (home / "unexpected-tool").exists():
        raise ValueError(
            "Setup or playback used an external tool instead of the disk image payload"
        )
    if os.environ.get("RECOG_DMG_TEST_SERVICES") == "1":
        check_services(payload, runtime, env)


def check_services(payload, runtime, env):
    # The service labels and web port are per login. Never exercise these over
    # someone's existing installation when the disk image is built locally.
    if os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise RuntimeError("Service acceptance needs a disposable GitHub-hosted Mac")
    if subprocess.run(
        ["launchctl", "print", f"gui/{os.getuid()}"],
        stdout=subprocess.DEVNULL,
        check=False,
    ).returncode:
        print("::notice::No graphical login on the runner; launchd activation was not exercised.")
        return
    for name in ("live", "frontend", "copilot-channel", "copilot-supervisor"):
        job = f"gui/{os.getuid()}/com.recog.{name}"
        if (
            subprocess.run(["launchctl", "print", job], capture_output=True, check=False).returncode
            == 0
        ):
            raise RuntimeError(f"Existing registered service: {job}")
    env = {**env, "PATH": f"{runtime}/bin:{env['PATH']}"}
    try:
        subprocess.run(
            [payload / "bin/recog", "setup", "--no-prefetch"],
            stdin=subprocess.DEVNULL,
            env=env,
            check=True,
        )
        for _ in range(50):
            try:
                with urllib.request.urlopen(
                    "http://127.0.0.1:9080/api/live/status", timeout=3
                ) as response:
                    if response.status == 200:
                        break
            except urllib.error.URLError:
                time.sleep(0.2)
        else:
            raise RuntimeError("Installed web service did not become ready")
        status = subprocess.check_output(
            [runtime / "bin/recog", "service", "status"], env=env, text=True
        )
        print(status)
        if status.count("Running(") != 4:
            raise RuntimeError("The installed services did not all start")
    finally:
        subprocess.run([runtime / "bin/recog", "service", "stop"], env=env, check=True)
