"""Desktop commands resolve local configuration without starting the ASR stack."""

import json

import pytest

from recog import cli
from recog.desktop import processes, service, storage, worker


def invoke(monkeypatch, data_dir, *arguments):
    from recog import config

    monkeypatch.setattr(config, "get_data_dir", lambda: data_dir)
    monkeypatch.setattr(cli.sys, "argv", ["recog", "desktop", *arguments])
    with pytest.raises(SystemExit) as exit_info:
        cli.main()
    return exit_info.value.code


def test_disabled_installation_when_status_dispatched_then_json_and_exit_zero(
    tmp_path, monkeypatch, capsys
):
    assert invoke(monkeypatch, tmp_path, "status", "--json") == 0
    assert json.loads(capsys.readouterr().out)["state"] == "disabled"
    assert not (tmp_path / "desktop").exists()


def test_configured_installation_when_disabled_then_preserves_snapshot(
    tmp_path, monkeypatch, capsys
):
    root = tmp_path / "desktop"
    storage.write_json(root / "config.json", {"enabled": True, "app": "/ktalk", "bundle": "digest"})
    assert invoke(monkeypatch, tmp_path, "disable") == 0
    assert storage.read_json(root / "config.json") == {
        "enabled": False,
        "app": "/ktalk",
        "bundle": "digest",
    }


def test_selected_application_when_enabled_then_snapshots_verified_bundle(tmp_path, monkeypatch):
    app = tmp_path / "KTalk.AppImage"
    monkeypatch.setattr(processes, "application_path", lambda raw: app)
    monkeypatch.setattr(
        storage,
        "prepare_bundle",
        lambda source, root: ("digest", root / "bundles/digest"),
    )
    assert invoke(monkeypatch, tmp_path, "enable", "--app", str(app)) == 0
    assert storage.read_json(tmp_path / "desktop/config.json") == {
        "enabled": True,
        "app": str(app),
        "bundle": "digest",
    }


@pytest.mark.parametrize(
    "count,state,expected",
    [
        (0, "attached", 1),
        (2, "attached", 1),
        (1, "attached", 0),
        (1, "incompatible", 1),
    ],
)
def test_manual_attachment_when_target_count_or_result_changes_then_reports_exit_status(
    tmp_path, monkeypatch, count, state, expected
):
    root = tmp_path / "desktop"
    storage.write_json(root / "config.json", {"app": "/ktalk"})
    target = processes.Target(42, 12, "/ktalk")
    monkeypatch.setattr(processes, "application_path", lambda raw: raw)
    monkeypatch.setattr(processes, "find_targets", lambda app: [target] * count)

    def run(root, selected):
        assert count == 1
        assert selected == target
        return {"state": state}

    monkeypatch.setattr(worker, "run", run)
    assert invoke(monkeypatch, tmp_path, "attach") == expected


@pytest.mark.parametrize("reason", [None, "cleanup_failed"])
def test_human_status_when_reason_present_then_renders_it(tmp_path, monkeypatch, capsys, reason):
    monkeypatch.setattr(service, "status", lambda root: {"state": "waiting", "reason": reason})
    assert invoke(monkeypatch, tmp_path, "status") == 0
    assert capsys.readouterr().out.strip() == ("waiting: cleanup_failed" if reason else "waiting")


@pytest.mark.parametrize("ktalk_only", [False, True])
def test_existing_live_command_when_dispatched_then_forwards_audio_and_meeting_options(
    monkeypatch, ktalk_only
):
    from recog import live

    calls = []
    monkeypatch.setattr(live, "live_pipeline", lambda **options: calls.append(options))
    monkeypatch.setattr(
        cli.sys,
        "argv",
        [
            "recog",
            "live",
            "--mic-device",
            "7",
            "--system-audio",
            "device:8",
            "--chunk-duration",
            "2.5",
            "--output",
            "transcript.txt",
            "--ktalk",
            "--mic-speaker-name",
            "Speaker",
            "--no-save-audio",
            "--list-devices",
            *(["--ktalk-only"] if ktalk_only else []),
        ],
    )

    cli.main()

    assert calls == [
        {
            "mic_device": 7,
            "system_audio": "device:8",
            "chunk_duration": 2.5,
            "output_file": "transcript.txt",
            "list_devices": True,
            "use_ktalk": True,
            "ktalk_only": ktalk_only,
            "mic_speaker_name": "Speaker",
            "save_audio": False,
        }
    ]
