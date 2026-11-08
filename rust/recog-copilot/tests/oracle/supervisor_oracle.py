"""The Python side of rust/recog-copilot/tests/differential.rs.

Reads a JSON list of cases on stdin, runs each through recog/copilot_supervisor.py (and
the AI settings store, the bridge, the Python builtins the port mirrors), writes the
JSON list of outputs on stdout. Usage: python supervisor_oracle.py REPO_ROOT < cases.

Everything personal is pointed at a temporary directory before recog is imported, as
tests/conftest.py does, so the user's settings and data never reach the comparison.
"""

import asyncio
import json
import os
import shlex
import sys
import tempfile
import unicodedata
from pathlib import Path

REPO = Path(sys.argv[1]).resolve()
SCRATCH = Path(tempfile.mkdtemp(prefix="recog-copilot-oracle-"))
os.environ["RECOG_CONFIG_FILE"] = "/nonexistent/recog/config.toml"
os.environ["RECOG_CONFERENCE_HOSTS_FILE"] = "/nonexistent/recog/conference-hosts.json"
os.environ["RECOG_DATA_DIR"] = str(SCRATCH / "data")
for legacy in ("CLAUDE_MODEL", "CODEX_MODEL", "CODEX_REASONING_EFFORT"):
    os.environ.pop(legacy, None)
sys.path.insert(0, str(REPO))

import recog.copilot_supervisor as cs  # noqa: E402
from recog.ai_settings import AiSettingsStore  # noqa: E402
from recog.install_config import INSTALL_CONFIG, CopilotSettings  # noqa: E402

cs._linux_session_manager = lambda: False
for legacy in ("CLAUDE_MODEL", "CODEX_MODEL", "CODEX_REASONING_EFFORT"):
    # The checkout's .env, loaded at import, must not leak into the settings cases.
    os.environ.pop(legacy, None)

_store_count = 0


def fresh_store(body=None):
    global _store_count
    _store_count += 1
    directory = SCRATCH / f"store-{_store_count}"
    directory.mkdir(parents=True)
    if body is not None:
        (directory / "ai-settings.json").write_text(body, encoding="utf-8")
    return AiSettingsStore(directory)


def command(command):
    if isinstance(command, cs.Spawn):
        return ["spawn", command.meeting_id]
    return ["kill"]


def lifecycle_of(start):
    machine = cs.SessionLifecycle()
    machine.state = cs.State(start["state"])
    machine.meeting_id = start["meeting_id"]
    return machine


def snapshot(machine):
    return {"state": machine.state.value, "meeting_id": machine.meeting_id}


def run_lifecycle(case):
    machine = lifecycle_of(case["start"])
    out = []
    for name, arg in case["events"]:
        method = getattr(machine, name)
        result = (
            method() if name in ("timer_expired", "session_died", "session_ready") else method(arg)
        )
        out.append({"commands": [command(c) for c in result], **snapshot(machine)})
    return out


def run_reconcile(case):
    sup = cs.Supervisor(
        runner=None, ai_settings_store=fresh_store(), grace_s=1000.0, autospawn=True
    )
    sup.lifecycle = lifecycle_of(case["start"])
    result = sup._reconcile(bool(case["active"]), case["meeting_id"], case["session_alive"])
    return {"commands": [command(c) for c in result], **snapshot(sup.lifecycle)}


class ScriptedRunner:
    def __init__(self):
        self.is_alive = True
        self.screen = ""
        self.keys = []

    async def alive(self, session_name=None):
        return self.is_alive

    async def capture(self):
        return self.screen

    async def send_keys(self, *keys):
        self.keys.append(list(keys))


def run_watchdog(case):
    machine = lifecycle_of(case["start"])
    runner = ScriptedRunner()
    statuses, died = [], []
    flags = {"in_flight": False, "now": 0.0}

    async def send_status(status):
        statuses.append(status.value)

    async def on_died():
        died.append(True)

    watchdog = cs.Watchdog(
        runner=runner,
        lifecycle=machine,
        send_status=send_status,
        on_session_died=on_died,
        clock=lambda: flags["now"],
        spawn_in_flight=lambda: flags["in_flight"],
    )

    async def main():
        out = []
        for step in case["steps"]:
            runner.is_alive = step["alive"]
            runner.screen = step["screen"]
            flags["in_flight"] = step["in_flight"]
            flags["now"] = step["now"]
            if step.get("idle"):
                machine.state, machine.meeting_id = cs.State.IDLE, None
            before_s, before_k = len(statuses), len(runner.keys)
            await watchdog.tick()
            out.append(
                {
                    "statuses": statuses[before_s:],
                    "keys": runner.keys[before_k:],
                    "died": len(died),
                    "state": machine.state.value,
                }
            )
        return out

    return asyncio.run(main())


def run_settings(case):
    for variable, key in (
        ("CLAUDE_MODEL", "claude_model"),
        ("CODEX_MODEL", "codex_model"),
        ("CODEX_REASONING_EFFORT", "codex_effort"),
    ):
        if case.get(key) is None:
            os.environ.pop(variable, None)
        else:
            os.environ[variable] = case[key]
    try:
        store = fresh_store(case["body"])
        if case.get("directory"):
            (store.path).mkdir()
        task = store.resolve_task("copilot")
        return {
            "enabled": task["enabled"],
            "model": task["models"]["claude"],
            "effort": task["claude_effort"],
        }
    except (OSError, ValueError):
        return {"error": True}
    finally:
        for variable in ("CLAUDE_MODEL", "CODEX_MODEL", "CODEX_REASONING_EFFORT"):
            os.environ.pop(variable, None)


def run_claude_command(case):
    # The URL comes from the installation config, as in production (`TmuxRunner()`).
    INSTALL_CONFIG.copilot.mcp_url = case["mcp_url"]
    runner = cs.TmuxRunner(
        run=None, use_systemd_run=False, ai_settings_store=fresh_store(case["body"])
    )
    try:
        return {"command": runner.build_claude_command()}
    except (OSError, ValueError, RuntimeError):
        return {"error": True}


class LogRunner:
    def __init__(self, calls):
        self.calls = calls

    async def spawn(self, meeting_id):
        self.calls.append(["spawn", meeting_id])

    async def kill(self):
        self.calls.append(["kill"])


def run_frames(case):
    calls = []
    alive = case["session_alive"]
    sup = cs.Supervisor(
        runner=LogRunner(calls),
        ai_settings_store=fresh_store(),
        grace_s=1000.0,
        autospawn=case["autospawn"],
        session_alive_probe=lambda: alive,
    )

    async def main():
        for item in case["frames"]:
            if item == "__died__":
                await sup.on_session_died()
            elif item == "__ready__":
                sup.lifecycle.session_ready()
            else:
                await sup.handle_frame(item)
        return {
            "calls": calls,
            **snapshot(sup.lifecycle),
            "grace_pending": sup._grace_task is not None,
            "respawn_attempts": sup._respawn_attempts,
            "respawn_pending": sup._respawn_task is not None and not sup._respawn_task.done(),
            "hello": sup._live_meeting_id(),
        }

    return asyncio.run(main())


def run_bridge_hello(case):
    from recog.ktalk_bridge import KTalkBridge

    class RecordingWebSocket:
        def __init__(self):
            self.sent = []

        async def send(self, message):
            self.sent.append(message)

    bridge = KTalkBridge(ai_settings_store=fresh_store())
    stub = RecordingWebSocket()
    asyncio.run(bridge._process_message(json.loads(case["raw"]), websocket=stub))
    states = [f for f in (json.loads(m) for m in stub.sent) if f.get("type") == "meeting_state"]
    return {
        "manual_active": bridge._manual_copilot_active,
        "meeting_states": states,
        "python_bytes": json.dumps(json.loads(case["raw"])),
    }


def run_values(case):
    values = case["values"]
    return {
        "truthy": [bool(v) for v in values],
        "str": [str(v) for v in values],
        "repr": [repr(v) for v in values],
        "json": [json.dumps(v) for v in values],
    }


def run_text(case):
    texts = case["texts"]
    return {
        "splitlines": [t.splitlines() for t in texts],
        "rstrip": [t.rstrip() for t in texts],
        "strip": [t.strip() for t in texts],
        "lower": [t.lower() for t in texts],
        "shlex": shlex.join(texts),
        "classify": [cs.classify_screen(t).value for t in texts],
        "trust_keys": [list(cs.trust_dialog_keys(t)) for t in texts],
    }


def ranges(predicate):
    # Surrogates are no `char` in Rust: both sides count them as misses.
    out, start = [], None
    for code in range(0x110000):
        hit = not 0xD800 <= code <= 0xDFFF and predicate(chr(code))
        if hit and start is None:
            start = code
        elif not hit and start is not None:
            out.append([start, code - 1])
            start = None
    if start is not None:
        out.append([start, 0x10FFFF])
    return out


def run_unicode(case):
    return {
        "isspace": ranges(str.isspace),
        "other_category": ranges(lambda c: unicodedata.category(c).startswith("C")),
        "word": ranges(lambda c: c.isalnum() or c == "_"),
        "printable": ranges(str.isprintable),
    }


def run_constants(case):
    import tests.test_copilot_supervisor as fixtures
    from recog.ai_defaults import DEFAULT_MODELS
    from recog.ktalk_bridge import MANUAL_MEETING_ID as BRIDGE_MANUAL

    defaults = CopilotSettings()
    return {
        "manual": cs.MANUAL_MEETING_ID,
        "bridge_manual": BRIDGE_MANUAL,
        "socket": cs.TMUX_SOCKET,
        "session": cs.TMUX_SESSION,
        "session_manual": cs.TMUX_SESSION_MANUAL,
        "unit": cs.SYSTEMD_SESSION_UNIT,
        "session_env_keys": list(cs.SESSION_ENV_KEYS),
        "unit_stop_timeout_s": cs.SYSTEMD_UNIT_STOP_TIMEOUT_S,
        "unit_poll_s": cs.SYSTEMD_UNIT_POLL_S,
        "kill_retries": cs.TmuxRunner.KILL_RETRIES,
        "kill_retry_delay_s": cs.TmuxRunner.KILL_RETRY_DELAY_S,
        "respawn_backoff_s": list(cs.Supervisor.RESPAWN_BACKOFF_S),
        "initial_backoff_s": cs.Supervisor.INITIAL_BACKOFF_S,
        "max_backoff_s": cs.Supervisor.MAX_BACKOFF_S,
        "log_throttle_s": cs.Supervisor.LOG_THROTTLE_S,
        "poll_s": cs.Watchdog.POLL_INTERVAL_S,
        "heartbeat_s": cs.Watchdog.HEARTBEAT_S,
        "spawn_deadline_s": cs.Watchdog.SPAWN_DEADLINE_S,
        "dismiss_retry_s": cs.Watchdog.DISMISS_RETRY_S,
        "blocked_markers": list(cs.BLOCKED_MARKERS),
        "auto_dismiss_markers": list(cs.AUTO_DISMISS_MARKERS),
        "bridge_url": cs.DEFAULT_BRIDGE_URL,
        "log_prefix": cs.LOG_PREFIX,
        "default_autospawn": defaults.autospawn,
        "default_grace_s": defaults.grace_s,
        "default_mcp_url": defaults.mcp_url,
        "default_claude_model": DEFAULT_MODELS["claude"],
        "default_codex_model": DEFAULT_MODELS["codex"],
        "screens": {
            name: getattr(fixtures, name)
            for name in (
                "DEV_CHANNELS_SCREEN",
                "MODEL_DIALOG_SCREEN",
                "REPL_PROMPT_SCREEN",
                "SPINNER_SCREEN",
                "STARTUP_SCREEN",
                "TRUST_DIALOG_SCREEN",
                "TRUST_DIALOG_NUMBERED_SCREEN",
            )
        },
    }


HANDLERS = {
    "lifecycle": run_lifecycle,
    "reconcile": run_reconcile,
    "watchdog": run_watchdog,
    "settings": run_settings,
    "claude_command": run_claude_command,
    "frames": run_frames,
    "bridge_hello": run_bridge_hello,
    "values": run_values,
    "text": run_text,
    "unicode": run_unicode,
    "constants": run_constants,
}

if __name__ == "__main__":
    cases = json.load(sys.stdin)
    # Anything recog prints goes to stderr: stdout carries only the answer.
    answer, sys.stdout = sys.stdout, sys.stderr
    json.dump([HANDLERS[case["kind"]](case) for case in cases], answer)
