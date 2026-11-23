"""The signing Mac is polled until its Terminal window reports both exit codes."""

import importlib
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).absolute().parents[1] / "scripts"))
release = importlib.import_module("sign_release")


def test_signing_window_when_it_reports_after_a_while_then_its_answer_is_returned():
    answers = iter(["", "", "0 0", "never asked"])
    assert release.wait_for(lambda: next(answers), attempts=5, pause=0) == "0 0"


def test_signing_window_when_it_never_reports_then_waiting_ends_empty():
    calls = []
    assert release.wait_for(lambda: calls.append(1) or "", attempts=3, pause=0) == ""
    assert len(calls) == 3


def test_signing_window_when_both_signings_succeed_then_nothing_is_reported(monkeypatch):
    monkeypatch.setattr(
        release, "remote_output", lambda host, command: pytest.fail("no logs needed")
    )
    release.require_clean("signing-mac", "/tmp/x", "0 0")


@pytest.mark.parametrize(("done", "shown"), [("1 0", "1 0"), ("", "timed out")])
def test_signing_window_when_a_signing_fails_or_never_ends_then_its_logs_are_shown(
    monkeypatch, done, shown
):
    monkeypatch.setattr(release, "remote_output", lambda host, command: "Keychain: item not found")
    with pytest.raises(SystemExit, match=f"({shown})[\\s\\S]*Keychain: item not found"):
        release.require_clean("signing-mac", "/tmp/x", done)
